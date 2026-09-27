//! Where `ods state` commands get their settings (#214): a flag, else the `DBT_*`
//! variable dbt itself would read, else ODS's configuration (`[state]` and the
//! `[providers.<name>]` instance with `kind = "dbt"`), else a default. Each value keeps
//! where it came from, for the report and the log.

use std::path::{Path, PathBuf};

use clap::ArgMatches;
use clap::parser::ValueSource;
use ods_config::{ConfigError, Loaded, Source, display_key};
use ods_provider_dbt::settings::{CONFIG_SETTINGS, KIND};

use crate::exit::{CliError, ExitStatus};

/// The state database when nothing else names one.
pub(super) const DEFAULT_STORE: &str = ".ods/state.db";

/// Where a setting came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Origin {
    /// The command line.
    Flag,
    /// A `DBT_*` variable, read as the flag's default.
    Env(&'static str),
    /// ODS's configuration.
    Config(Source),
    /// Derived from another setting, e.g. the environment from the target.
    From(&'static str),
    /// Built in.
    Default,
}

impl Origin {
    /// Short, for reports: `flag`, `DBT_TARGET`, `project config`, `profile dev`, …
    pub(super) fn label(&self) -> String {
        match self {
            Origin::Flag => "flag".to_owned(),
            Origin::Env(var) => (*var).to_owned(),
            Origin::Config(Source::File { kind, .. }) => format!("{} config", kind.name()),
            Origin::Config(Source::Profile { name, .. }) => format!("profile {name}"),
            Origin::Config(Source::Env { var }) => var.clone(),
            Origin::Config(source) => source.to_string(),
            Origin::From(setting) => (*setting).to_owned(),
            Origin::Default => "default".to_owned(),
        }
    }
}

/// A setting's value and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Setting {
    pub value: String,
    pub origin: Origin,
}

impl Setting {
    fn new(value: impl Into<String>, origin: Origin) -> Self {
        Self {
            value: value.into(),
            origin,
        }
    }
}

/// The settings of one `ods state` command.
#[derive(Debug, Clone)]
pub(super) struct StateSettings {
    /// The dbt executable.
    pub program: Setting,
    pub project_dir: Option<Setting>,
    pub profiles_dir: Option<Setting>,
    /// dbt's `--profile`.
    pub profile: Option<Setting>,
    pub target: Option<Setting>,
    /// Where dbt writes the artifacts ODS reads, resolved as dbt resolves it.
    pub target_dir: Setting,
    /// Whose state is kept.
    pub environment: Setting,
    pub state_db: Setting,
}

impl StateSettings {
    /// Resolves the settings for a command parsed into `args`. Options the command
    /// doesn't have are read from configuration alone.
    ///
    /// # Errors
    /// More than one dbt provider is configured.
    pub(super) fn resolve(args: &ArgMatches, config: &Loaded) -> Result<Self, CliError> {
        let dbt = dbt_instance(config)?;
        let configured = |key: &str| -> Option<Setting> {
            let name = dbt?;
            let path = ["providers", name, "settings", key].map(str::to_owned);
            let setting = config.effective(&path)?;
            let value = setting.value.as_str()?;
            let value = match key {
                "program" => config_path(value, &setting.source, true),
                "project_dir" | "profiles_dir" | "target_dir" => {
                    config_path(value, &setting.source, false)
                }
                _ => value.to_owned(),
            };
            Some(Setting::new(value, Origin::Config(setting.source.clone())))
        };
        let state = |key: &str| -> Option<Setting> {
            let path = ["state", key].map(str::to_owned);
            let setting = config.effective(&path)?;
            let value = setting.value.as_str()?;
            let value = if key == "db" {
                config_path(value, &setting.source, false)
            } else {
                value.to_owned()
            };
            Some(Setting::new(value, Origin::Config(setting.source.clone())))
        };
        let pick = |id: &str, env: &'static str, key: &str| {
            given(args, id, env).or_else(|| configured(key))
        };

        let project_dir = pick("project-dir", "DBT_PROJECT_DIR", "project_dir");
        let target = pick("target", "DBT_TARGET", "target");
        let target_dir = match given(args, "target-dir", "DBT_TARGET_PATH") {
            // dbt reads a relative target path against the project, not where it runs.
            Some(Setting {
                value,
                origin: origin @ Origin::Env(_),
            }) if Path::new(&value).is_relative() => Setting::new(
                project_dir.as_ref().map_or_else(
                    || value.clone(),
                    |p| Path::new(&p.value).join(&value).display().to_string(),
                ),
                origin,
            ),
            Some(given) => given,
            None => configured("target_dir").unwrap_or_else(|| match &project_dir {
                Some(p) => Setting::new(
                    Path::new(&p.value).join("target").display().to_string(),
                    Origin::From("project_dir"),
                ),
                None => Setting::new("target", Origin::Default),
            }),
        };
        let environment = given(args, "environment", "")
            .or_else(|| state("environment"))
            .or_else(|| {
                target
                    .as_ref()
                    .map(|t| Setting::new(t.value.clone(), Origin::From("target")))
            })
            .unwrap_or_else(|| Setting::new("default", Origin::Default));
        Ok(Self {
            program: pick("dbt", "", "program")
                .unwrap_or_else(|| Setting::new("dbt", Origin::Default)),
            profiles_dir: pick("profiles-dir", "DBT_PROFILES_DIR", "profiles_dir"),
            profile: pick("dbt-profile", "DBT_PROFILE", "profile"),
            state_db: given(args, "state-db", "")
                .or_else(|| state("db"))
                .unwrap_or_else(|| Setting::new(DEFAULT_STORE, Origin::Default)),
            project_dir,
            target,
            target_dir,
            environment,
        })
    }

    pub(super) fn target_dir(&self) -> PathBuf {
        PathBuf::from(&self.target_dir.value)
    }

    pub(super) fn state_db(&self) -> PathBuf {
        PathBuf::from(&self.state_db.value)
    }
}

/// The value of option `id` if it was given on the command line or, when the option
/// reads one, in dbt's variable `env`. Built-in defaults don't count: configuration
/// beats them.
fn given(args: &ArgMatches, id: &str, env: &'static str) -> Option<Setting> {
    // Not every command has every option; asking for the source of one it hasn't panics.
    let value = args.try_get_one::<String>(id).ok().flatten()?;
    let origin = match args.value_source(id)? {
        ValueSource::CommandLine => Origin::Flag,
        ValueSource::EnvVariable => Origin::Env(env),
        _ => return None,
    };
    Some(Setting::new(value.clone(), origin))
}

/// A path from configuration, read against the directory of the file that set it, so
/// `ods.toml` means the same wherever ODS runs below it. A program is a path only if it
/// names a directory; `dbt` is looked up on `PATH`.
fn config_path(value: &str, source: &Source, program: bool) -> String {
    let (Source::File { path: file, .. } | Source::Profile { path: file, .. }) = source else {
        return value.to_owned();
    };
    let path = Path::new(value);
    let bare_program = program && path.components().count() == 1;
    match file.parent() {
        Some(dir) if path.is_relative() && !bare_program => dir.join(path).display().to_string(),
        _ => value.to_owned(),
    }
}

/// The name of the one configured dbt provider instance, if any.
fn dbt_instance(config: &Loaded) -> Result<Option<&str>, CliError> {
    let mut names = config
        .config
        .providers
        .iter()
        .filter(|(_, p)| p.kind == KIND)
        .map(|(name, _)| name.as_str());
    let first = names.next();
    let others: Vec<&str> = names.collect();
    if others.is_empty() {
        return Ok(first);
    }
    Err(CliError::new(
        ExitStatus::Config,
        "ODS-E0102",
        format!(
            "more than one dbt provider is configured ({}), so `ods state` can't tell which to use",
            std::iter::once(first.unwrap_or_default())
                .chain(others)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
    .with_hint("keep one `[providers.<name>]` with `kind = \"dbt\"`"))
}

/// Checks the settings of every configured dbt provider: known keys, string values
/// (ADR-0005: providers validate their own settings).
///
/// # Errors
/// The first unknown key or non-string value, as a configuration schema error.
pub(crate) fn validate(config: &Loaded) -> Result<(), ConfigError> {
    for (key, setting) in config.effective_settings() {
        let [providers, name, settings, rest @ ..] = key.as_slice() else {
            continue;
        };
        let is_dbt = config
            .config
            .providers
            .get(name)
            .is_some_and(|p| p.kind == KIND);
        if providers != "providers" || settings != "settings" || !is_dbt {
            continue;
        }
        let known = rest.len() == 1 && CONFIG_SETTINGS.iter().any(|(k, _)| *k == rest[0]);
        let message = if !known {
            let names: Vec<&str> = CONFIG_SETTINGS.iter().map(|(k, _)| *k).collect();
            format!("unknown dbt setting; expected one of {}", names.join(", "))
        } else if !setting.value.is_str() {
            "must be a string".to_owned()
        } else {
            continue;
        };
        return Err(ConfigError::Schema {
            key: display_key(key),
            origin: Box::new(setting.source.clone()),
            message,
        });
    }
    Ok(())
}
