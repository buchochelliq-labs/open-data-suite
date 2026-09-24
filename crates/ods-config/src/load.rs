//! Loading, layering and validating configuration (ADR-0005 §1, §2).
//!
//! Every layer is flattened to `key path -> value`, so precedence is a simple ordered
//! overwrite and every effective value keeps its full history for `ods config explain`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::ConfigError;
use crate::model::{CONFIG_VERSION, Config};
use crate::secret::{is_secret_key, is_secret_ref_value};
use crate::source::{FileKind, Source};

/// Project configuration file name.
pub const PROJECT_FILE: &str = "ods.toml";
/// Local overrides, relative to the project root.
pub const LOCAL_FILE: &str = ".ods/local.toml";
/// Prefix of environment variables that set configuration keys (`ODS__OUTPUT__FORMAT`).
pub const ENV_PREFIX: &str = "ODS__";
/// Environment variable that selects a profile.
pub const PROFILE_ENV: &str = "ODS_PROFILE";

/// A value set on the command line, fed in as the highest-precedence layer.
#[derive(Debug, Clone)]
pub struct FlagValue {
    /// Key path, e.g. `["output", "format"]`.
    pub key: Vec<String>,
    /// The value.
    pub value: toml::Value,
    /// The flag as written, e.g. `--json`.
    pub flag: String,
}

/// Everything configuration is loaded from. Built by [`Inputs::discover`] in the CLI and
/// by hand in tests, so loading never reads the process environment directly.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    /// Candidate per-user file (read if it exists).
    pub user_file: Option<PathBuf>,
    /// Project file found by searching upwards for `ods.toml`.
    pub project_file: Option<PathBuf>,
    /// Local overrides next to the project file (read if it exists).
    pub local_file: Option<PathBuf>,
    /// Environment variables (only `ODS__*` and `ODS_PROFILE` are used).
    pub env: Vec<(String, String)>,
    /// Profile chosen with `--profile`.
    pub profile_flag: Option<String>,
    /// Values from command-line flags.
    pub flags: Vec<FlagValue>,
}

impl Inputs {
    /// Finds the configuration files for a process running in `cwd` with `env`.
    ///
    /// The project file is the nearest `ods.toml` in `cwd` or an ancestor. The user file
    /// is `$XDG_CONFIG_HOME/ods/config.toml`, else `$HOME/.config/ods/config.toml`, else
    /// `%APPDATA%\ods\config.toml`.
    pub fn discover(cwd: &Path, env: &[(String, String)]) -> Self {
        let var = |name: &str| {
            env.iter()
                .find(|(key, value)| key == name && !value.is_empty())
                .map(|(_, value)| PathBuf::from(value))
        };
        let user_dir = var("XDG_CONFIG_HOME")
            .or_else(|| var("HOME").map(|home| home.join(".config")))
            .or_else(|| var("APPDATA"));
        let project_file = cwd
            .ancestors()
            .map(|dir| dir.join(PROJECT_FILE))
            .find(|candidate| candidate.is_file());
        let local_file = project_file
            .as_ref()
            .and_then(|file| file.parent())
            .map(|root| root.join(LOCAL_FILE));
        Self {
            user_file: user_dir.map(|dir| dir.join("ods").join("config.toml")),
            project_file,
            local_file,
            env: env.to_vec(),
            profile_flag: None,
            flags: Vec::new(),
        }
    }
}

/// One value set by one layer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Setting {
    /// The value, as written.
    pub value: toml::Value,
    /// Where it was set.
    pub source: Source,
}

/// A configuration file that was considered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileStatus {
    /// Which layer.
    pub kind: FileKind,
    /// Path.
    pub path: PathBuf,
    /// Whether it existed and was read.
    pub loaded: bool,
}

/// The result of loading: the validated config plus the provenance behind it.
#[derive(Debug, Clone)]
pub struct Loaded {
    /// Validated effective configuration.
    pub config: Config,
    /// The active profile, and what selected it.
    pub profile: Option<(String, String)>,
    /// Files that were considered, in precedence order.
    pub files: Vec<FileStatus>,
    /// Every key's settings, lowest precedence first; the last one is effective.
    pub history: BTreeMap<Vec<String>, Vec<Setting>>,
}

impl Loaded {
    /// The effective setting for `key`.
    pub fn effective(&self, key: &[String]) -> Option<&Setting> {
        self.history.get(key).and_then(|settings| settings.last())
    }
}

/// A configuration file that was read: its tables, with `profiles` split out.
struct ParsedFile {
    kind: FileKind,
    path: PathBuf,
    base: toml::Table,
    profiles: toml::Table,
}

type History = BTreeMap<Vec<String>, Vec<Setting>>;

/// Loads, merges and validates configuration.
///
/// # Errors
/// Returns [`ConfigError`] for unreadable or malformed files, schema violations,
/// plaintext credentials and unknown profiles.
pub fn load(inputs: &Inputs) -> Result<Loaded, ConfigError> {
    let (files, parsed) = read_files(inputs)?;
    let mut history = History::new();

    // Base layers, in file order: user < project < local.
    for file in &parsed {
        for (key, value) in flatten(&file.base) {
            let source = Source::File {
                kind: file.kind,
                path: file.path.clone(),
            };
            record(&mut history, key, value, source);
        }
    }
    let profile = select_profile(inputs, &history);
    if let Some((name, selected_by)) = &profile {
        apply_profile(&mut history, &parsed, name, selected_by)?;
    }
    apply_env(&mut history, &inputs.env)?;
    for flag in &inputs.flags {
        let source = Source::Flag {
            flag: flag.flag.clone(),
        };
        record(&mut history, flag.key.clone(), flag.value.clone(), source);
    }

    let config = validate(&history)?;
    Ok(Loaded {
        config,
        profile,
        files,
        history,
    })
}

fn read_files(inputs: &Inputs) -> Result<(Vec<FileStatus>, Vec<ParsedFile>), ConfigError> {
    let (mut files, mut parsed) = (Vec::new(), Vec::new());
    for (kind, path) in [
        (FileKind::User, &inputs.user_file),
        (FileKind::Project, &inputs.project_file),
        (FileKind::Local, &inputs.local_file),
    ] {
        let Some(path) = path else { continue };
        let Some(mut base) = read_table(path)? else {
            files.push(FileStatus {
                kind,
                path: path.clone(),
                loaded: false,
            });
            continue;
        };
        let profiles = match base.remove("profiles") {
            Some(toml::Value::Table(profiles)) => profiles,
            Some(_) => {
                return Err(schema_error(
                    &["profiles".to_owned()],
                    Source::File {
                        kind,
                        path: path.clone(),
                    },
                    "must be a table of named profiles",
                ));
            }
            None => toml::Table::new(),
        };
        files.push(FileStatus {
            kind,
            path: path.clone(),
            loaded: true,
        });
        parsed.push(ParsedFile {
            kind,
            path: path.clone(),
            base,
            profiles,
        });
    }
    Ok((files, parsed))
}

/// `--profile` beats `ODS_PROFILE` beats `default_profile`. Returns the name and what
/// selected it.
fn select_profile(inputs: &Inputs, history: &History) -> Option<(String, String)> {
    let env_profile = inputs
        .env
        .iter()
        .find(|(key, value)| key == PROFILE_ENV && !value.is_empty())
        .map(|(_, value)| value.clone());
    let default_profile = history
        .get(&vec!["default_profile".to_owned()])
        .and_then(|settings| settings.last())
        .and_then(|setting| setting.value.as_str().map(str::to_owned));
    inputs
        .profile_flag
        .clone()
        .map(|name| (name, "--profile".to_owned()))
        .or_else(|| env_profile.map(|name| (name, PROFILE_ENV.to_owned())))
        .or_else(|| default_profile.map(|name| (name, "default_profile".to_owned())))
}

/// Applies `[profiles.<name>]` from every file, in file order.
fn apply_profile(
    history: &mut History,
    parsed: &[ParsedFile],
    name: &str,
    selected_by: &str,
) -> Result<(), ConfigError> {
    let mut defined = false;
    for file in parsed {
        match file.profiles.get(name) {
            Some(toml::Value::Table(section)) => {
                defined = true;
                for (key, value) in flatten(section) {
                    let source = Source::Profile {
                        name: name.to_owned(),
                        kind: file.kind,
                        path: file.path.clone(),
                    };
                    record(history, key, value, source);
                }
            }
            Some(_) => {
                return Err(schema_error(
                    &["profiles".to_owned(), name.to_owned()],
                    Source::File {
                        kind: file.kind,
                        path: file.path.clone(),
                    },
                    "a profile must be a table",
                ));
            }
            None => {}
        }
    }
    if defined {
        return Ok(());
    }
    let mut available: Vec<String> = parsed
        .iter()
        .flat_map(|f| f.profiles.keys().cloned())
        .collect();
    available.sort();
    available.dedup();
    Err(ConfigError::UnknownProfile {
        name: name.to_owned(),
        selected_by: selected_by.to_owned(),
        available,
    })
}

/// Applies `ODS__SECTION__KEY=value` variables, in sorted order so the result never
/// depends on environment order.
fn apply_env(history: &mut History, env: &[(String, String)]) -> Result<(), ConfigError> {
    let mut vars: Vec<&(String, String)> = env
        .iter()
        .filter(|(key, _)| key.starts_with(ENV_PREFIX))
        .collect();
    vars.sort();
    for (var, raw) in vars {
        let key: Vec<String> = var[ENV_PREFIX.len()..]
            .split("__")
            .map(str::to_ascii_lowercase)
            .collect();
        if key.iter().any(String::is_empty) {
            return Err(schema_error(
                &key,
                Source::Env { var: var.clone() },
                "empty key segment",
            ));
        }
        record(
            history,
            key,
            parse_env_value(raw),
            Source::Env { var: var.clone() },
        );
    }
    Ok(())
}

fn record(history: &mut History, key: Vec<String>, value: toml::Value, source: Source) {
    history
        .entry(key)
        .or_default()
        .push(Setting { value, source });
}

/// Reads a TOML file; `Ok(None)` if it does not exist.
fn read_table(path: &Path) -> Result<Option<toml::Table>, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ConfigError::Read {
                path: path.to_owned(),
                source,
            });
        }
    };
    text.parse::<toml::Table>()
        .map(Some)
        .map_err(|err| ConfigError::Parse {
            path: path.to_owned(),
            message: err.to_string(),
        })
}

/// Flattens a table into leaf key paths. Arrays and secret references are leaves.
pub(crate) fn flatten(table: &toml::Table) -> Vec<(Vec<String>, toml::Value)> {
    fn walk(
        prefix: &mut Vec<String>,
        table: &toml::Table,
        out: &mut Vec<(Vec<String>, toml::Value)>,
    ) {
        for (key, value) in table {
            prefix.push(key.clone());
            match value {
                toml::Value::Table(inner) if !is_secret_ref_value(value) => {
                    walk(prefix, inner, out);
                }
                _ => out.push((prefix.clone(), value.clone())),
            }
            prefix.pop();
        }
    }
    let mut out = Vec::new();
    walk(&mut Vec::new(), table, &mut out);
    out
}

/// Rebuilds a nested table from leaf key paths.
fn unflatten<'a>(leaves: impl Iterator<Item = (&'a Vec<String>, &'a toml::Value)>) -> toml::Table {
    let mut root = toml::Table::new();
    for (key, value) in leaves {
        let Some((last, parents)) = key.split_last() else {
            continue;
        };
        let mut table = &mut root;
        for part in parents {
            let entry = table
                .entry(part.clone())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            // A later layer may have replaced a table with a scalar (or vice versa);
            // the schema check reports it, so the deeper key simply wins here.
            if !entry.is_table() {
                *entry = toml::Value::Table(toml::Table::new());
            }
            let toml::Value::Table(next) = entry else {
                unreachable!("just ensured a table")
            };
            table = next;
        }
        table.insert(last.clone(), value.clone());
    }
    root
}

/// Parses an environment value as a TOML value (`true`, `80`, `"x"`), else a string.
fn parse_env_value(raw: &str) -> toml::Value {
    format!("v = {raw}")
        .parse::<toml::Table>()
        .ok()
        .and_then(|mut table| table.remove("v"))
        .unwrap_or_else(|| toml::Value::String(raw.to_owned()))
}

fn schema_error(key: &[String], source: Source, message: impl Into<String>) -> ConfigError {
    ConfigError::Schema {
        key: key.join("."),
        origin: Box::new(source),
        message: message.into(),
    }
}

/// Checks credentials and the schema, then builds the typed [`Config`].
fn validate(history: &History) -> Result<Config, ConfigError> {
    let effective = || {
        history
            .iter()
            .filter_map(|(key, settings)| settings.last().map(|s| (key, s)))
    };

    for (key, setting) in effective() {
        let under_providers = key.first().is_some_and(|k| k == "providers");
        let named_like_secret = key.last().is_some_and(|k| is_secret_key(k));
        if under_providers && named_like_secret && !is_secret_ref_value(&setting.value) {
            return Err(ConfigError::PlaintextSecret {
                key: key.join("."),
                origin: Box::new(setting.source.clone()),
            });
        }
    }

    let table = unflatten(effective().map(|(key, setting)| (key, &setting.value)));
    let config: Config =
        serde_path_to_error::deserialize(toml::Value::Table(table)).map_err(|err| {
            let path = err.path().to_string();
            let key: Vec<String> = path.split('.').map(str::to_owned).collect();
            let source = source_for(history, &key);
            ConfigError::Schema {
                key: path,
                origin: Box::new(source),
                message: err.into_inner().to_string(),
            }
        })?;

    if let Some(version) = config.version.filter(|v| *v != CONFIG_VERSION) {
        let key = vec!["version".to_owned()];
        return Err(schema_error(
            &key,
            source_for(history, &key),
            format!(
                "unsupported configuration version {version}; this build reads version {CONFIG_VERSION}"
            ),
        ));
    }
    if let Some(width) = config.output.width.filter(|w| *w < 20) {
        let key = vec!["output".to_owned(), "width".to_owned()];
        return Err(schema_error(
            &key,
            source_for(history, &key),
            format!("width {width} is below the minimum of 20"),
        ));
    }
    Ok(config)
}

/// The layer that set `key`, or the nearest ancestor key that was set.
fn source_for(history: &History, key: &[String]) -> Source {
    (0..=key.len())
        .rev()
        .find_map(|len| {
            let prefix = &key[..len];
            history
                .iter()
                .filter(|(k, _)| k.starts_with(prefix))
                .find_map(|(_, settings)| settings.last())
                .map(|s| s.source.clone())
        })
        .unwrap_or(Source::Default)
}
