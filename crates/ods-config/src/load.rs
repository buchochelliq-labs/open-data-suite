//! Loading, layering and validating configuration (ADR-0005 §1, §2).
//!
//! Every layer is flattened to `key path -> value` and recorded with a sequence number
//! in precedence order. A key's latest setting is effective unless an ancestor or
//! descendant key was set later: a higher layer may replace a table with a scalar, or a
//! scalar with a table, and the replaced keys drop out. Every value keeps its full
//! history for `ods config explain`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{ConfigError, display_key};
use crate::model::{CONFIG_VERSION, Config};
use crate::secret::{SecretViolation, check_secrets, is_secret_ref_value};
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
    /// Where the project-file search started, reported when no `ods.toml` was found.
    pub search_root: Option<PathBuf>,
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
    /// is `$XDG_CONFIG_HOME/ods/config.toml` (only if that is an absolute path, as the
    /// XDG spec requires), else `$HOME/.config/ods/config.toml`, else
    /// `%APPDATA%\ods\config.toml`.
    pub fn discover(cwd: &Path, env: &[(String, String)]) -> Self {
        let var = |name: &str| {
            env.iter()
                .find(|(key, value)| key == name && !value.is_empty())
                .map(|(_, value)| PathBuf::from(value))
        };
        let user_dir = var("XDG_CONFIG_HOME")
            .filter(|dir| dir.is_absolute())
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
            search_root: Some(cwd.to_owned()),
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
    /// Position in precedence order; higher wins.
    #[serde(skip)]
    pub seq: usize,
}

/// A configuration file that was considered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileStatus {
    /// Which layer.
    pub kind: FileKind,
    /// Path (for a project file that was not found: where the upward search started).
    pub path: PathBuf,
    /// Whether it existed and was read.
    pub loaded: bool,
}

/// A setting that an effective value replaced: an earlier value of the same key, or a
/// value of an ancestor or descendant key that a later layer replaced.
#[derive(Debug, Clone, PartialEq)]
pub struct Replaced<'a> {
    /// The replaced key (equal to the effective key unless a table or scalar was replaced).
    pub key: &'a [String],
    /// The replaced setting.
    pub setting: &'a Setting,
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
    /// Every key's settings from every layer, lowest precedence first.
    pub history: BTreeMap<Vec<String>, Vec<Setting>>,
    /// Keys whose latest setting is in effect (not replaced by a related key).
    live: BTreeSet<Vec<String>>,
}

impl Loaded {
    /// The effective setting for `key`, if it is in effect.
    pub fn effective(&self, key: &[String]) -> Option<&Setting> {
        if !self.live.contains(key) {
            return None;
        }
        self.history.get(key).and_then(|settings| settings.last())
    }

    /// Every effective key and setting, sorted by key.
    pub fn effective_settings(&self) -> impl Iterator<Item = (&Vec<String>, &Setting)> {
        self.live
            .iter()
            .filter_map(|key| Some((key, self.history.get(key)?.last()?)))
    }

    /// What the effective setting at `key` replaced, most recent first.
    pub fn replaced(&self, key: &[String]) -> Vec<Replaced<'_>> {
        let Some(effective) = self.effective(key) else {
            return Vec::new();
        };
        let mut replaced: Vec<Replaced<'_>> = self
            .history
            .iter()
            .filter(|(other, _)| {
                other.as_slice() == key || (related(other, key) && !self.live.contains(*other))
            })
            .flat_map(|(other, settings)| {
                settings
                    .iter()
                    .filter(|s| s.seq < effective.seq)
                    .map(move |setting| Replaced {
                        key: other,
                        setting,
                    })
            })
            .collect();
        replaced.sort_by(|a, b| b.setting.seq.cmp(&a.setting.seq));
        replaced
    }
}

/// Whether one key is a strict ancestor of the other.
fn related(a: &[String], b: &[String]) -> bool {
    a != b && (a.starts_with(b) || b.starts_with(a))
}

/// A configuration file that was read: its tables, with `profiles` split out.
struct ParsedFile {
    kind: FileKind,
    path: PathBuf,
    base: toml::Table,
    profiles: toml::Table,
}

type History = BTreeMap<Vec<String>, Vec<Setting>>;

/// Accumulates settings in precedence order.
#[derive(Default)]
struct Layers {
    history: History,
    next_seq: usize,
}

impl Layers {
    fn record(&mut self, key: Vec<String>, value: toml::Value, source: Source) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.history
            .entry(key)
            .or_default()
            .push(Setting { value, source, seq });
    }
}

/// Loads, merges and validates configuration.
///
/// # Errors
/// Returns [`ConfigError`] for unreadable or malformed files, schema violations,
/// plaintext credentials, malformed secret references and unknown profiles. No error
/// message contains a configuration value that could be a secret.
pub fn load(inputs: &Inputs) -> Result<Loaded, ConfigError> {
    let (files, parsed) = read_files(inputs)?;
    let mut layers = Layers::default();

    // Base layers, in file order: user < project < local.
    for file in &parsed {
        for (key, value) in flatten(&file.base) {
            let source = Source::File {
                kind: file.kind,
                path: file.path.clone(),
            };
            layers.record(key, value, source);
        }
    }
    let profile = select_profile(inputs, &layers.history);
    if let Some((name, selected_by)) = &profile {
        apply_profile(&mut layers, &parsed, name, selected_by)?;
    }
    apply_env(&mut layers, &inputs.env)?;
    for flag in &inputs.flags {
        let source = Source::Flag {
            flag: flag.flag.clone(),
        };
        layers.record(flag.key.clone(), flag.value.clone(), source);
    }

    let history = layers.history;
    check_all_secrets(&history)?;
    let live = live_keys(&history);
    let config = validate(&history, &live)?;
    Ok(Loaded {
        config,
        profile,
        files,
        history,
        live,
    })
}

fn read_files(inputs: &Inputs) -> Result<(Vec<FileStatus>, Vec<ParsedFile>), ConfigError> {
    let (mut files, mut parsed) = (Vec::new(), Vec::new());
    // Report where we looked, so `explain` shows that no project file was found.
    if inputs.project_file.is_none()
        && let Some(root) = &inputs.search_root
    {
        files.push(FileStatus {
            kind: FileKind::Project,
            path: root.join(PROJECT_FILE),
            loaded: false,
        });
    }
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
    files.sort_by_key(|f| f.kind);
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

/// `default_profile` chooses the profile, so it may only come from a file's top level;
/// setting it in a profile or the environment could never take effect.
fn reject_default_profile(key: &[String], source: Source) -> Result<(), ConfigError> {
    if key.first().is_some_and(|k| k == "default_profile") {
        return Err(schema_error(
            key,
            source,
            "default_profile can only be set at the top level of a configuration file; \
             use --profile or ODS_PROFILE to choose a profile instead",
        ));
    }
    Ok(())
}

/// Applies `[profiles.<name>]` from every file, in file order.
fn apply_profile(
    layers: &mut Layers,
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
                    reject_default_profile(&key, source.clone())?;
                    layers.record(key, value, source);
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
/// depends on environment order. Each segment matches an existing key
/// case-insensitively (so `ODS__PROVIDERS__MYWH__KIND` reaches `[providers.MyWh]`);
/// otherwise it is lowercased.
fn apply_env(layers: &mut Layers, env: &[(String, String)]) -> Result<(), ConfigError> {
    let mut vars: Vec<&(String, String)> = env
        .iter()
        .filter(|(key, _)| key.starts_with(ENV_PREFIX))
        .collect();
    vars.sort();
    for (var, raw) in vars {
        let source = Source::Env { var: var.clone() };
        let segments: Vec<&str> = var[ENV_PREFIX.len()..].split("__").collect();
        if segments.iter().any(|s| s.is_empty()) {
            let key: Vec<String> = segments.iter().map(|s| s.to_ascii_lowercase()).collect();
            return Err(schema_error(&key, source, "empty key segment"));
        }
        let key = resolve_env_key(&layers.history, &segments);
        reject_default_profile(&key, source.clone())?;
        layers.record(key, parse_env_value(raw), source);
    }
    Ok(())
}

/// Spells each segment like an existing key with the same prefix, ignoring case.
fn resolve_env_key(history: &History, segments: &[&str]) -> Vec<String> {
    let mut resolved: Vec<String> = Vec::with_capacity(segments.len());
    for segment in segments {
        let depth = resolved.len();
        let existing = history
            .keys()
            .filter(|key| key.len() > depth && key[..depth] == resolved[..])
            .map(|key| &key[depth])
            .find(|name| name.eq_ignore_ascii_case(segment));
        resolved.push(
            existing
                .cloned()
                .unwrap_or_else(|| segment.to_ascii_lowercase()),
        );
    }
    resolved
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
            message: parse_error_message(&text, &err),
        })
}

/// Line, column and the parser's short message. The source snippet that `toml`'s own
/// `Display` includes is left out, because the offending line may hold a secret.
fn parse_error_message(text: &str, err: &toml::de::Error) -> String {
    match err.span() {
        Some(span) => {
            let before = &text[..span.start.min(text.len())];
            let line = before.matches('\n').count() + 1;
            let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
            format!("line {line}, column {column}: {}", err.message())
        }
        None => err.message().to_owned(),
    }
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

/// Keys whose latest setting is not replaced by a later setting of a related key.
fn live_keys(history: &History) -> BTreeSet<Vec<String>> {
    let latest: Vec<(&Vec<String>, usize)> = history
        .iter()
        .filter_map(|(key, settings)| Some((key, settings.last()?.seq)))
        .collect();
    latest
        .iter()
        .filter(|(key, seq)| {
            !latest
                .iter()
                .any(|(other, other_seq)| other_seq > seq && related(other, key))
        })
        .map(|(key, _)| (*key).clone())
        .collect()
}

/// Rebuilds a nested table from effective leaves. Live keys never conflict (no live key
/// is an ancestor of another), so every intermediate entry is a table.
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
            let toml::Value::Table(next) = entry else {
                unreachable!("live keys are never ancestors of each other")
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
        key: display_key(key),
        origin: Box::new(source),
        message: message.into(),
    }
}

/// Applies the secret rules to every value from every layer, including values a later
/// layer overrides: a plaintext credential in any file is an error (ADR-0005 §4).
fn check_all_secrets(history: &History) -> Result<(), ConfigError> {
    for (key, settings) in history {
        for setting in settings {
            match check_secrets(key, &setting.value) {
                Ok(()) => {}
                Err(SecretViolation::Plaintext { path }) => {
                    return Err(ConfigError::PlaintextSecret {
                        key: display_key(&path),
                        origin: Box::new(setting.source.clone()),
                    });
                }
                Err(SecretViolation::InvalidReference { path, reason }) => {
                    return Err(ConfigError::InvalidSecretRef {
                        key: display_key(&path),
                        origin: Box::new(setting.source.clone()),
                        reason,
                    });
                }
            }
        }
    }
    Ok(())
}

/// Checks the schema on the effective keys and builds the typed [`Config`].
fn validate(history: &History, live: &BTreeSet<Vec<String>>) -> Result<Config, ConfigError> {
    let leaves = live
        .iter()
        .filter_map(|key| Some((key, &history.get(key)?.last()?.value)));
    let table = unflatten(leaves);
    let config: Config =
        serde_path_to_error::deserialize(toml::Value::Table(table)).map_err(|err| {
            let key: Vec<String> = err
                .path()
                .iter()
                .map(|segment| match segment {
                    serde_path_to_error::Segment::Seq { index } => format!("[{index}]"),
                    serde_path_to_error::Segment::Map { key } => key.clone(),
                    serde_path_to_error::Segment::Enum { variant } => variant.clone(),
                    serde_path_to_error::Segment::Unknown => "?".to_owned(),
                })
                .collect();
            let source = source_for(history, live, &key);
            ConfigError::Schema {
                key: display_key(&key),
                origin: Box::new(source),
                message: redact_literals(&err.into_inner().to_string()),
            }
        })?;

    if let Some(version) = config.version.filter(|v| *v != CONFIG_VERSION) {
        let key = vec!["version".to_owned()];
        return Err(schema_error(
            &key,
            source_for(history, live, &key),
            format!(
                "unsupported configuration version {version}; this build reads version {CONFIG_VERSION}"
            ),
        ));
    }
    if let Some(width) = config.output.width.filter(|w| *w < 20) {
        let key = vec!["output".to_owned(), "width".to_owned()];
        return Err(schema_error(
            &key,
            source_for(history, live, &key),
            format!("width {width} is below the minimum of 20"),
        ));
    }
    Ok(config)
}

/// Replaces quoted string literals in a deserializer message with `<value>`, so a
/// schema error never echoes a configured value (`invalid type: string "…"`).
fn redact_literals(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut in_literal = false;
    let mut escaped = false;
    for c in message.chars() {
        if in_literal {
            match (escaped, c) {
                (false, '\\') => escaped = true,
                (false, '"') => in_literal = false,
                _ => escaped = false,
            }
        } else if c == '"' {
            in_literal = true;
            out.push_str("<value>");
        } else {
            out.push(c);
        }
    }
    out
}

/// The layer that set `key`: the effective setting of `key`, or of the nearest ancestor
/// or first descendant that is in effect.
fn source_for(history: &History, live: &BTreeSet<Vec<String>>, key: &[String]) -> Source {
    (0..=key.len())
        .rev()
        .find_map(|len| {
            let prefix = &key[..len];
            live.iter()
                .find(|k| k.starts_with(prefix))
                .and_then(|k| history.get(k)?.last())
                .map(|s| s.source.clone())
        })
        .unwrap_or(Source::Default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_quoted_literals_only() {
        assert_eq!(
            redact_literals(r#"invalid type: string "s3cr3t", expected a map"#),
            "invalid type: string <value>, expected a map"
        );
        assert_eq!(
            redact_literals("unknown field `fromat`, expected one of `format`"),
            "unknown field `fromat`, expected one of `format`"
        );
        assert_eq!(redact_literals(r#"a "x\"y" b"#), "a <value> b");
    }

    #[test]
    fn parse_errors_omit_the_source_line() {
        let text = "[output]\ntoken = hunter5\n";
        let err = text.parse::<toml::Table>().unwrap_err();
        let message = parse_error_message(text, &err);
        assert!(message.starts_with("line 2, column"), "{message}");
        assert!(!message.contains("hunter5"), "{message}");
    }
}
