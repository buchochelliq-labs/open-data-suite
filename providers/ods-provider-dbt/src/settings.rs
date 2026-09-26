//! What ODS does with each dbt setting read from the environment (#227).
//!
//! dbt reads about 60 `DBT_*` variables as defaults for its flags. ODS treats them as it
//! treats the flags themselves:
//! - [`EnvClass::Owned`]: ODS has its own option for the setting, reads the variable as
//!   that option's default, and passes the result to dbt as an explicit flag. The
//!   variable itself is removed from dbt's environment, so only one value is in play.
//! - [`EnvClass::Harmless`]: output, logging, parsing and caching; passed through.
//! - [`EnvClass::Overridden`]: would change what is built or recorded, but a dbt flag
//!   beats it, so ODS passes that flag (where the command accepts it) and warns.
//! - [`EnvClass::Refused`]: would change what is built or recorded, and nothing beats
//!   it: ODS refuses to run until it is unset.
//!
//! Any other `DBT_*` name isn't a dbt setting (e.g. `DBT_ENV_SECRET_*`, or a project's
//! own variable read by `env_var()`) and passes through: its effect on the code is in
//! the compiled SQL, which is fingerprinted.

use std::collections::BTreeMap;

/// What ODS does with a dbt setting from the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EnvClass {
    /// ODS reads it as the default of this option of its own.
    Owned {
        /// The ODS option, e.g. `--target`.
        option: &'static str,
    },
    /// Passed through.
    Harmless,
    /// Beaten by `flag`, which ODS passes to the dbt commands that accept it.
    Overridden {
        /// The dbt flag that beats it, e.g. `--no-defer`.
        flag: &'static str,
    },
    /// ODS refuses to run while it is set, and why.
    Refused {
        /// Why, for the error.
        why: &'static str,
    },
}

use EnvClass::{Harmless, Overridden, Owned, Refused};

const CHANGES_SELECTION: &str = "it changes which nodes dbt builds, and ODS selects exactly the nodes its plan builds; use --resource-type or --exclude-resource-type on `ods state build` instead";
const NOT_REAL: &str = "it makes dbt build something other than the real thing (empty, sampled or one time window), which ODS can't record as a build";
const BEATS_FLAGS: &str = "this old spelling beats dbt's own flags (dbt maps it over --no-defer and --no-favor-state), so ODS can't override it";
const STALE_PARSE: &str = "it makes dbt parse the project from a file of changes instead of the files themselves, so ODS could plan and fingerprint code that isn't what runs";
const REPLAYS: &str = "in replay mode dbt answers from a recording instead of the warehouse, so nothing ODS records would be real";

/// Every dbt setting ODS knows, by variable name, sorted. dbt 1.10's `params.py` names
/// are all here (a test checks the installed dbt), plus the recorder's, which dbt reads
/// outside it.
pub const DBT_ENV: [(&str, EnvClass); 61] = [
    ("DBT_ARTIFACT_STATE_PATH", Overridden { flag: "--no-defer" }),
    ("DBT_CACHE_SELECTED_ONLY", Harmless),
    ("DBT_CLEAN_PROJECT_FILES_ONLY", Harmless),
    ("DBT_DEBUG", Harmless),
    ("DBT_DEFER", Overridden { flag: "--no-defer" }),
    ("DBT_DEFER_STATE", Overridden { flag: "--no-defer" }),
    ("DBT_DEFER_TO_STATE", Refused { why: BEATS_FLAGS }),
    ("DBT_EMPTY", Overridden { flag: "--no-empty" }),
    ("DBT_ENGINE_RECORDER_MODE", Refused { why: REPLAYS }),
    ("DBT_EVENT_TIME_END", Refused { why: NOT_REAL }),
    ("DBT_EVENT_TIME_START", Refused { why: NOT_REAL }),
    (
        "DBT_EXCLUDE_RESOURCE_TYPES",
        Refused {
            why: CHANGES_SELECTION,
        },
    ),
    // Saved queries are only built when selected, and ODS selects by exact id.
    ("DBT_EXPORT_SAVED_QUERIES", Harmless),
    ("DBT_FAIL_FAST", Harmless),
    (
        "DBT_FAVOR_STATE",
        Overridden {
            flag: "--no-favor-state",
        },
    ),
    ("DBT_FAVOR_STATE_MODE", Refused { why: BEATS_FLAGS }),
    (
        "DBT_FULL_REFRESH",
        Owned {
            option: "--full-refresh",
        },
    ),
    ("DBT_HOST", Harmless),
    ("DBT_INCLUDE_SAVED_QUERY", Harmless),
    (
        "DBT_INDIRECT_SELECTION",
        Overridden {
            flag: "--indirect-selection eager",
        },
    ),
    ("DBT_INTROSPECT", Harmless),
    ("DBT_LOG_CACHE_EVENTS", Harmless),
    ("DBT_LOG_FILE_MAX_BYTES", Harmless),
    ("DBT_LOG_FORMAT", Harmless),
    ("DBT_LOG_FORMAT_FILE", Harmless),
    ("DBT_LOG_LEVEL", Harmless),
    ("DBT_LOG_LEVEL_FILE", Harmless),
    ("DBT_LOG_PATH", Harmless),
    ("DBT_MACRO_DEBUGGING", Harmless),
    ("DBT_NO_PRINT", Harmless),
    ("DBT_PARTIAL_PARSE", Harmless),
    // False makes dbt trust its saved parse instead of reading the files: changed
    // code would compile, and be fingerprinted, as it was.
    (
        "DBT_PARTIAL_PARSE_FILE_DIFF",
        Overridden {
            flag: "--partial-parse-file-diff",
        },
    ),
    ("DBT_PARTIAL_PARSE_FILE_PATH", Harmless),
    ("DBT_POPULATE_CACHE", Harmless),
    ("DBT_PP_FILE_DIFF_TEST", Refused { why: STALE_PARSE }),
    ("DBT_PRINT", Harmless),
    ("DBT_PRINTER_WIDTH", Harmless),
    (
        "DBT_PROFILE",
        Owned {
            option: "--dbt-profile",
        },
    ),
    (
        "DBT_PROFILES_DIR",
        Owned {
            option: "--profiles-dir",
        },
    ),
    (
        "DBT_PROJECT_DIR",
        Owned {
            option: "--project-dir",
        },
    ),
    ("DBT_QUIET", Harmless),
    ("DBT_RECORDER_MODE", Refused { why: REPLAYS }),
    (
        "DBT_RESOURCE_TYPES",
        Refused {
            why: CHANGES_SELECTION,
        },
    ),
    ("DBT_SAMPLE", Refused { why: NOT_REAL }),
    ("DBT_SEND_ANONYMOUS_USAGE_STATS", Harmless),
    ("DBT_SHOW_RESOURCE_REPORT", Harmless),
    ("DBT_SINGLE_THREADED", Harmless),
    // Only read for deferral, which --no-defer turns off, and `state:` selectors,
    // which ODS never passes: it selects by exact id.
    ("DBT_STATE", Overridden { flag: "--no-defer" }),
    ("DBT_STATIC_PARSER", Harmless),
    ("DBT_STORE_FAILURES", Harmless),
    ("DBT_TARGET", Owned { option: "--target" }),
    (
        "DBT_TARGET_PATH",
        Owned {
            option: "--target-dir",
        },
    ),
    ("DBT_UPLOAD_TO_ARTIFACTS_INGEST_API", Harmless),
    ("DBT_USE_COLORS", Harmless),
    ("DBT_USE_COLORS_FILE", Harmless),
    ("DBT_USE_EXPERIMENTAL_PARSER", Harmless),
    ("DBT_USE_FAST_TEST_EDGES", Harmless),
    ("DBT_VERSION_CHECK", Harmless),
    ("DBT_WARN_ERROR", Harmless),
    ("DBT_WARN_ERROR_OPTIONS", Harmless),
    (
        "DBT_WRITE_JSON",
        Overridden {
            flag: "--write-json",
        },
    ),
];

/// dbt reads these outside `params.py`, so a check against it doesn't find them.
pub const OUTSIDE_PARAMS: [&str; 3] = [
    "DBT_ENGINE_RECORDER_MODE",
    "DBT_PP_FILE_DIFF_TEST",
    "DBT_RECORDER_MODE",
];

/// What ODS does with `name`, or `None` if it isn't a dbt setting.
pub fn class(name: &str) -> Option<EnvClass> {
    DBT_ENV
        .binary_search_by(|(n, _)| (*n).cmp(name))
        .ok()
        .map(|i| DBT_ENV[i].1)
}

/// Whether `name` set to `value` changes what dbt does. For dbt's booleans, a false
/// value (empty, `0`, `false`, `no`) is the default; any other setting does something
/// with any value (`DBT_WRITE_JSON=false`, `DBT_STATE=path`, `DBT_SAMPLE=…`).
fn is_set(name: &str, value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    if matches!(
        name,
        "DBT_DEFER"
            | "DBT_DEFER_TO_STATE"
            | "DBT_EMPTY"
            | "DBT_FAVOR_STATE"
            | "DBT_FAVOR_STATE_MODE"
    ) {
        return !matches!(value.as_str(), "" | "0" | "false" | "no");
    }
    !value.is_empty()
}

/// The dbt settings in an environment, by what ODS does with them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct EnvReport {
    /// Set variables that ODS overrides, with the flag it passes.
    pub overridden: Vec<(String, &'static str)>,
    /// Set variables that stop ODS from running, with why.
    pub refused: Vec<(String, &'static str)>,
}

impl EnvReport {
    /// Sorts `env` (name to value) by class. Owned, harmless and non-dbt variables are
    /// left out: they need no action.
    pub fn of<'a>(env: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let env: BTreeMap<&str, &str> = env.into_iter().collect();
        let mut report = Self::default();
        for (name, value) in env {
            match class(name) {
                Some(Overridden { flag }) if is_set(name, value) => {
                    report.overridden.push((name.to_owned(), flag));
                }
                Some(Refused { why }) if is_set(name, value) => {
                    report.refused.push((name.to_owned(), why));
                }
                _ => {}
            }
        }
        report
    }

    /// The warning for each overridden variable.
    pub fn warnings(&self) -> Vec<String> {
        self.overridden
            .iter()
            .map(|(name, flag)| {
                format!("{name} is set: ODS passes {flag} to dbt, so it has no effect on this run")
            })
            .collect()
    }

    /// The error when anything is refused, if it is.
    pub fn refusal(&self) -> Option<String> {
        if self.refused.is_empty() {
            return None;
        }
        let each: Vec<String> = self
            .refused
            .iter()
            .map(|(name, why)| format!("{name}: {why}"))
            .collect();
        Some(format!(
            "unset {} for ODS runs; {}",
            self.refused
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            each.join("; ")
        ))
    }
}

/// The variables ODS owns: removed from dbt's environment, since ODS passes the
/// settings as flags.
pub fn owned() -> impl Iterator<Item = &'static str> {
    DBT_ENV
        .iter()
        .filter(|(_, class)| matches!(class, Owned { .. }))
        .map(|(name, _)| *name)
}

/// The flags ODS passes to `command` so no setting from the environment, or from
/// `dbt_project.yml`'s `flags:`, changes what is built or recorded.
/// - `--write-json` and `--partial-parse-file-diff` always: ODS reads the artifacts,
///   and they must describe the files as they are.
/// - `--indirect-selection eager` where tests are selected (`build`, `test`): the
///   tests of the selected nodes, as ODS plans them.
/// - `--no-defer`, `--no-favor-state` and `--no-empty` when a variable they beat is
///   set; `--no-empty` only where dbt has `--empty`.
pub fn override_args(report: &EnvReport, command: &str) -> Vec<String> {
    let mut args = vec![
        "--write-json".to_owned(),
        "--partial-parse-file-diff".to_owned(),
    ];
    if matches!(command, "build" | "test") {
        args.extend(["--indirect-selection".to_owned(), "eager".to_owned()]);
    }
    for flag in ["--no-defer", "--no-favor-state", "--no-empty"] {
        let wanted = report.overridden.iter().any(|(_, f)| *f == flag);
        let accepted =
            flag != "--no-empty" || matches!(command, "run" | "build" | "compile" | "snapshot");
        if wanted && accepted {
            args.push(flag.to_owned());
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_and_unique() {
        assert!(DBT_ENV.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn each_class_is_found() {
        assert_eq!(class("DBT_TARGET"), Some(Owned { option: "--target" }));
        assert_eq!(class("DBT_LOG_LEVEL"), Some(Harmless));
        assert_eq!(class("DBT_DEFER"), Some(Overridden { flag: "--no-defer" }));
        assert!(matches!(class("DBT_SAMPLE"), Some(Refused { .. })));
        // A project's own variable, and dbt's secret-prefixed ones.
        assert_eq!(class("DBT_SCHEMA"), None);
        assert_eq!(class("DBT_ENV_SECRET_TOKEN"), None);
    }

    #[test]
    fn only_set_variables_need_action() {
        let report = EnvReport::of([
            ("DBT_DEFER", "true"),
            ("DBT_FAVOR_STATE", "false"),
            ("DBT_EMPTY", "1"),
            ("DBT_STATE", "prod-artifacts"),
            ("DBT_SAMPLE", "3 days"),
            ("DBT_EVENT_TIME_START", ""),
            ("DBT_TARGET", "prod"),
            ("DBT_LOG_LEVEL", "debug"),
            ("DBT_SCHEMA", "x"),
        ]);
        assert_eq!(
            report.overridden,
            [
                ("DBT_DEFER".to_owned(), "--no-defer"),
                ("DBT_EMPTY".to_owned(), "--no-empty"),
                // Only deferral reads it, and --no-defer turns that off.
                ("DBT_STATE".to_owned(), "--no-defer"),
            ]
        );
        assert_eq!(report.refused.len(), 1);
        assert_eq!(report.refused[0].0, "DBT_SAMPLE");
        let refusal = report.refusal().unwrap();
        assert!(
            refusal.starts_with("unset DBT_SAMPLE for ODS runs"),
            "{refusal}"
        );
        assert_eq!(report.warnings().len(), 3);
    }

    #[test]
    fn override_flags_go_only_where_dbt_accepts_them() {
        let report = EnvReport::of([
            ("DBT_EMPTY", "true"),
            ("DBT_DEFER", "true"),
            ("DBT_INDIRECT_SELECTION", "empty"),
            ("DBT_WRITE_JSON", "false"),
            ("DBT_PARTIAL_PARSE_FILE_DIFF", "false"),
        ]);
        // False values of the last three still do harm.
        assert_eq!(report.overridden.len(), 5, "{report:?}");
        let always = ["--write-json", "--partial-parse-file-diff"];
        assert_eq!(
            override_args(&report, "run"),
            [&always[..], &["--no-defer", "--no-empty"]].concat()
        );
        assert_eq!(
            override_args(&report, "build"),
            [
                &always[..],
                &["--indirect-selection", "eager", "--no-defer", "--no-empty"]
            ]
            .concat()
        );
        assert_eq!(
            override_args(&report, "seed"),
            [&always[..], &["--no-defer"]].concat()
        );
        // With nothing set, only what is always passed.
        assert_eq!(
            override_args(&EnvReport::default(), "test"),
            [&always[..], &["--indirect-selection", "eager"]].concat()
        );
        // Several variables --no-defer beats: passed once.
        let state = EnvReport::of([("DBT_DEFER", "1"), ("DBT_STATE", "prod")]);
        assert_eq!(
            override_args(&state, "run"),
            [&always[..], &["--no-defer"]].concat()
        );
    }

    #[test]
    fn owned_variables_are_the_ones_ods_has_options_for() {
        assert_eq!(
            owned().collect::<Vec<_>>(),
            [
                "DBT_FULL_REFRESH",
                "DBT_PROFILE",
                "DBT_PROFILES_DIR",
                "DBT_PROJECT_DIR",
                "DBT_TARGET",
                "DBT_TARGET_PATH"
            ]
        );
    }
}
