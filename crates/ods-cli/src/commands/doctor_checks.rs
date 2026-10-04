//! The checks of `ods doctor` (#181, ADR-0023).
//!
//! Each check answers one question and returns a typed [`CheckResult`]. Checks share
//! what they learn (the manifest, `dbt --version`, the target) through memoised facts,
//! so a filtered run (`--project`, `--provider`) only does the work its checks need.
//! A check whose precondition failed is `unknown`, naming that check, never `ok`
//! (AGENTS.md rule 3). Only this composition root names providers; the result model is
//! neutral (`ods_core::diagnostic`).

use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use clap::ArgMatches;
use ods_config::{Loaded, SecretRef, display_key, is_secret_key};
use ods_core::state::{TargetIdentity, Timestamp};
use ods_core::{Capability, CheckCategory, CheckResult, Evidence};
use ods_provider_dbt::executor::{DbtExecutor, DbtOutput};
use ods_provider_dbt::version::{DbtVersion, MIN_SUPPORTED, Support};
use ods_provider_dbt::{
    ArtifactSource, Artifacts, DbtError, Manifest, ResourceType, strip_credentials,
};
use ods_sdk::Provider;
use ods_sdk::contracts::changes::{ChangeProvider, RequestedSource};
use ods_sdk::contracts::executor::RequestedNode;
use ods_sdk::contracts::relations::{RelationInspector, RelationPresence};
use ods_sdk::contracts::state_store::ProblemKind;

use super::state_doctor::{DoctorReport as StoreReport, problem_label};
use super::state_plan::{block_on, display_name};
use super::state_settings::{Setting, StateSettings};
use super::state_versions::{change_provider, has_change_provider, versions};
use crate::exit::CliError;
use crate::module::ConfigFailure;

/// Stable codes of `ods doctor`'s findings (ADR-0023 §2). Findings that mean what an
/// existing command error means reuse its code (`ODS-E0101`–`E0104`, `E0201`, `E0401`,
/// `E0405`); the letter is the finding's severity.
pub(super) mod codes {
    /// Not checked: a check it depends on failed.
    pub const BLOCKED: &str = "ODS-U0001";
    /// No dbt project (`dbt_project.yml`) in the project directory.
    pub const NO_PROJECT: &str = "ODS-E0204";
    /// The manifest names no project.
    pub const NO_PROJECT_NAME: &str = "ODS-W0205";
    /// The artifacts are older than the project's files.
    pub const STALE_ARTIFACTS: &str = "ODS-W0206";
    /// How old the artifacts are can't be told.
    pub const ARTIFACT_AGE_UNKNOWN: &str = "ODS-U0207";
    /// `ods doctor` found checks that fail; exit status 5.
    pub const DOCTOR_FAILED: &str = "ODS-E0501";
    /// dbt can't be run.
    pub const DBT_MISSING: &str = "ODS-E0502";
    /// dbt is older than ODS supports.
    pub const DBT_TOO_OLD: &str = "ODS-E0503";
    /// dbt's major version is one ODS isn't tested with.
    pub const DBT_UNTESTED: &str = "ODS-W0504";
    /// `dbt --version` printed no version ODS can read.
    pub const DBT_VERSION_UNKNOWN: &str = "ODS-U0505";
    /// The manifest's adapter isn't installed in dbt.
    pub const ADAPTER_MISSING: &str = "ODS-E0506";
    /// The manifest names no adapter.
    pub const ADAPTER_UNKNOWN: &str = "ODS-U0507";
    /// dbt lists no adapters, so whether the manifest's is installed can't be told.
    pub const ADAPTER_UNLISTED: &str = "ODS-U0508";
    /// dbt can't say which target it builds in.
    pub const TARGET_UNRENDERED: &str = "ODS-E0509";
    /// No data versions for sources: readers of sources without one always build.
    pub const NO_SOURCE_VERSIONS: &str = "ODS-W0601";
    /// Relations can't be checked before reuse.
    pub const NO_RELATION_CHECK: &str = "ODS-W0602";
    /// The live relation check failed.
    pub const RELATION_CHECK_FAILED: &str = "ODS-E0603";
    /// The live table-version probe failed.
    pub const VERSION_PROBE_FAILED: &str = "ODS-E0604";
    /// The profiles directory ODS gives dbt has no `profiles.yml`.
    pub const PROFILES_MISSING: &str = "ODS-W0510";
    /// The live relation check couldn't tell whether some relations exist.
    pub const RELATIONS_UNKNOWN: &str = "ODS-U0605";
    /// The live table-version probe ran, but some sources have no table version.
    pub const SOME_TABLE_VERSIONS: &str = "ODS-W0606";
    /// The live table-version probe ran, but no source has a table version.
    pub const NO_TABLE_VERSIONS: &str = "ODS-U0607";

    /// Every code `ods doctor` reports, reused ones included, for the docs test.
    #[cfg(test)]
    pub const ALL: [&str; 29] = [
        BLOCKED,
        "ODS-E0101",
        "ODS-E0102",
        "ODS-E0103",
        "ODS-E0104",
        "ODS-E0201",
        NO_PROJECT,
        NO_PROJECT_NAME,
        STALE_ARTIFACTS,
        ARTIFACT_AGE_UNKNOWN,
        "ODS-E0401",
        "ODS-E0405",
        DOCTOR_FAILED,
        DBT_MISSING,
        DBT_TOO_OLD,
        DBT_UNTESTED,
        DBT_VERSION_UNKNOWN,
        ADAPTER_MISSING,
        ADAPTER_UNKNOWN,
        ADAPTER_UNLISTED,
        TARGET_UNRENDERED,
        PROFILES_MISSING,
        NO_SOURCE_VERSIONS,
        NO_RELATION_CHECK,
        RELATION_CHECK_FAILED,
        VERSION_PROBE_FAILED,
        RELATIONS_UNKNOWN,
        SOME_TABLE_VERSIONS,
        NO_TABLE_VERSIONS,
    ];
}

/// Providers `--provider` accepts: those ODS wires in M1.
pub(super) const PROVIDERS: [&str; 3] = ["dbt", "databricks", "sqlite"];

/// Which checks to run and how to judge them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Options {
    /// Only the project checks (`--project`).
    pub project_only: bool,
    /// Only this provider's checks (`--provider`).
    pub provider: Option<String>,
    /// Run the live checks too (`--connect`).
    pub connect: bool,
    /// Warnings and unknowns fail the run (`--strict`).
    pub strict: bool,
}

/// A check's identity. Its outcome carries the same id and category.
#[derive(Debug, Clone, Copy)]
struct Spec {
    id: &'static str,
    category: CheckCategory,
    /// The providers the check may concern. With more than one, the project's adapter
    /// decides which ([`Checks::adapter_provider`]).
    providers: &'static [&'static str],
    /// ODS can't work without it: an unknown outcome fails the run.
    required: bool,
}

const fn spec(
    id: &'static str,
    category: CheckCategory,
    providers: &'static [&'static str],
    required: bool,
) -> Spec {
    Spec {
        id,
        category,
        providers,
        required,
    }
}

use CheckCategory as C;

const NONE: &[&str] = &[];
const DBT: &[&str] = &["dbt"];
const SQLITE: &[&str] = &["sqlite"];
/// Checks about sources' data versions: they concern the provider that reads them for
/// the project's adapter (`databricks`), else dbt's `source freshness` (`dbt`).
const BY_ADAPTER: &[&str] = &["dbt", "databricks"];

/// Every check, in display order within its category.
const SPECS: [Spec; 15] = [
    spec("config.load", C::Config, NONE, true),
    spec("config.values", C::Config, NONE, false),
    spec("config.resolution", C::Config, NONE, true),
    spec("project.dbt_project", C::Project, DBT, true),
    spec("project.manifest", C::Project, DBT, true),
    spec("project.name", C::Project, DBT, false),
    spec("project.freshness", C::Project, DBT, false),
    spec("tools.dbt", C::Tools, DBT, true),
    spec("tools.adapter", C::Tools, DBT, false),
    spec("target.identity", C::Target, DBT, true),
    spec("state_store.database", C::StateStore, SQLITE, true),
    spec(
        "capabilities.relation_existence",
        C::Capabilities,
        DBT,
        false,
    ),
    spec(
        "capabilities.relation_versions",
        C::Capabilities,
        BY_ADAPTER,
        false,
    ),
    spec("connectivity.relations", C::Connectivity, DBT, false),
    spec(
        "connectivity.table_versions",
        C::Connectivity,
        BY_ADAPTER,
        false,
    ),
];

/// A check couldn't run because the check it names, a precondition, failed.
#[derive(Debug, Clone, Copy)]
struct Blocked(&'static str);

/// Why the manifest couldn't be read.
#[derive(Debug, Clone)]
struct ManifestProblem {
    message: String,
    hint: &'static str,
}

/// What the checks learn, each at most once.
pub(super) struct Checks<'a> {
    config: &'a Loaded,
    config_failure: Option<ConfigFailure>,
    settings: Result<StateSettings, CliError>,
    connect: bool,
    manifest: OnceCell<Result<Manifest, ManifestProblem>>,
    dbt: OnceCell<Result<Option<DbtVersion>, String>>,
    target: OnceCell<Result<TargetIdentity, String>>,
}

impl<'a> Checks<'a> {
    /// Checks for a command parsed into `args`, with configuration `config` (from the
    /// flags alone when `config_failure` says it couldn't be loaded).
    pub(super) fn new(
        args: &ArgMatches,
        config: &'a Loaded,
        config_failure: Option<ConfigFailure>,
        connect: bool,
    ) -> Self {
        Self {
            config,
            config_failure,
            settings: StateSettings::resolve(args, config),
            connect,
            manifest: OnceCell::new(),
            dbt: OnceCell::new(),
            target: OnceCell::new(),
        }
    }

    /// Runs the checks `options` selects, in display order.
    pub(super) fn run(&self, options: &Options) -> Vec<CheckResult> {
        let wanted = options.provider.as_deref();
        SPECS
            .iter()
            .filter(|s| !options.project_only || s.category == C::Project)
            .filter(|s| wanted.is_none_or(|p| s.providers.contains(&p)))
            .filter_map(|s| {
                let provider = match s.providers {
                    [] => None,
                    [one] => Some(*one),
                    // The adapter decides; if it can't be read, the check is shown
                    // under the provider asked for, so its failure isn't hidden.
                    many => Some(
                        self.adapter_provider()
                            .or_else(|| wanted.and_then(|p| many.iter().copied().find(|m| *m == p)))
                            .unwrap_or(many[0]),
                    ),
                };
                if wanted.is_some_and(|p| provider != Some(p)) {
                    return None;
                }
                let result = self.check(*s).required(s.required);
                Some(match provider {
                    Some(provider) => result.provider(provider),
                    None => result,
                })
            })
            .collect()
    }

    /// The provider that reads sources' data versions for the project's adapter,
    /// from the manifest (offline); `None` if the manifest can't be read.
    fn adapter_provider(&self) -> Option<&'static str> {
        let (_, manifest) = self.manifest_ready().ok()?;
        Some(if has_change_provider(manifest.adapter_type.as_deref()) {
            "databricks"
        } else {
            "dbt"
        })
    }

    fn check(&self, s: Spec) -> CheckResult {
        let result = match s.id {
            "config.load" => Ok(self.config_load(s)),
            "config.values" => self.config_values(s),
            "config.resolution" => self.config_resolution(s),
            "project.dbt_project" => self.dbt_project(s),
            "project.manifest" => self.manifest_check(s),
            "project.name" => self.project_name(s),
            "project.freshness" => self.freshness(s),
            "tools.dbt" => self.tools_dbt(s),
            "tools.adapter" => self.tools_adapter(s),
            "target.identity" => self.target_identity(s),
            "state_store.database" => self.state_store(s),
            "capabilities.relation_existence" => self.relation_existence(s),
            "capabilities.relation_versions" => self.relation_versions(s),
            "connectivity.relations" => self.live_relations(s),
            "connectivity.table_versions" => self.live_versions(s),
            other => unreachable!(
                "`{other}` has no check: every id in SPECS has one (tests run them all)"
            ),
        };
        result.unwrap_or_else(|Blocked(dependency)| blocked(s, dependency))
    }

    // ------------------------------------------------------------------ preconditions

    /// The resolved settings, unless configuration failed: then nothing that depends on
    /// it can be trusted (a configured `project_dir` may not have been read).
    fn settings(&self) -> Result<&StateSettings, Blocked> {
        if self.config_failure.is_some() {
            return Err(Blocked("config.load"));
        }
        self.settings
            .as_ref()
            .map_err(|_| Blocked("config.resolution"))
    }

    fn executor(settings: &StateSettings) -> DbtExecutor {
        let mut executor = DbtExecutor::new(&settings.program.value, settings.target_dir())
            .output(DbtOutput::Capture);
        if let Some(dir) = &settings.project_dir {
            executor = executor.project_dir(&dir.value);
        }
        if let Some(dir) = &settings.profiles_dir {
            executor = executor.profiles_dir(&dir.value);
        }
        if let Some(target) = &settings.target {
            executor = executor.target(&target.value);
        }
        if let Some(profile) = &settings.profile {
            executor = executor.profile(&profile.value);
        }
        executor
    }

    fn manifest(&self, settings: &StateSettings) -> &Result<Manifest, ManifestProblem> {
        self.manifest.get_or_init(|| {
            Artifacts::load(&settings.target_dir())
                .map(|a| a.manifest)
                .map_err(|e| ManifestProblem {
                    hint: match &e {
                        DbtError::Io { source, .. }
                            if source.kind() == std::io::ErrorKind::NotFound =>
                        {
                            "dbt writes it when it parses the project: run `dbt parse`, or `ods state compile`"
                        }
                        DbtError::UnsupportedVersion { .. } => {
                            "ODS reads manifest v11 and v12 (dbt 1.7 and later): upgrade dbt, then run `dbt parse`"
                        }
                        _ => "write it again: run `dbt parse`, or `ods state compile`",
                    },
                    message: e.to_string(),
                })
        })
    }

    fn manifest_ready(&self) -> Result<(&StateSettings, &Manifest), Blocked> {
        let settings = self.settings()?;
        match self.manifest(settings) {
            Ok(manifest) => Ok((settings, manifest)),
            Err(_) => Err(Blocked("project.manifest")),
        }
    }

    fn dbt(&self, settings: &StateSettings) -> &Result<Option<DbtVersion>, String> {
        self.dbt.get_or_init(|| {
            block_on(Self::executor(settings).version())
                .map_err(|e| e.message)
                .and_then(|r| r.map_err(|e| e.to_string()))
        })
    }

    /// dbt, if it can be run.
    fn dbt_ready(&self, settings: &StateSettings) -> Result<Option<&DbtVersion>, Blocked> {
        match self.dbt(settings) {
            Ok(version) => Ok(version.as_ref()),
            Err(_) => Err(Blocked("tools.dbt")),
        }
    }

    // ------------------------------------------------------------------------- config

    fn config_load(&self, s: Spec) -> CheckResult {
        if let Some(failure) = &self.config_failure {
            return CheckResult::error(s.id, s.category, failure.code, &failure.message)
                .hint("fix the value named above; see docs/cli.md#configuration");
        }
        let loaded = self.config.files.iter().filter(|f| f.loaded).count();
        let message = if loaded == 0 {
            "no configuration file: built-in defaults apply".to_owned()
        } else {
            format!("{loaded} configuration file(s) loaded")
        };
        let mut result = CheckResult::ok(s.id, s.category, message);
        for file in &self.config.files {
            result = result.evidence(
                Evidence::new(
                    format!("{} file", file.kind.name()),
                    file.path.display().to_string(),
                )
                .from_source(if file.loaded { "loaded" } else { "not found" }),
            );
        }
        match &self.config.profile {
            Some((name, selected_by)) => {
                result.evidence(Evidence::new("profile", name).from_source(selected_by))
            }
            None => result.fact("profile", "none"),
        }
    }

    fn config_values(&self, s: Spec) -> Result<CheckResult, Blocked> {
        if self.config_failure.is_some() {
            return Err(Blocked("config.load"));
        }
        let mut secrets = 0;
        let mut evidence = Vec::new();
        for (key, setting) in self.config.effective_settings() {
            // Credentials are references by construction (ods-config rejects plaintext);
            // they are shown as references, never resolved (AGENTS.md rule 9).
            // Only a table is a reference: serde would also read `["x"]` as one.
            let secret = match &setting.value {
                toml::Value::Table(_) => setting.value.clone().try_into::<SecretRef>().ok(),
                _ => None,
            };
            let value = match secret {
                Some(secret) => {
                    secrets += 1;
                    secret.to_string()
                }
                None if key.last().is_some_and(|k| is_secret_key(k)) => "(not shown)".to_owned(),
                None => {
                    // A value that isn't a credential may still carry one, e.g. a
                    // connection string `postgres://user:pw@host/db`: shown without
                    // its user part, query, fragment and options, as the target is.
                    let (shown, stripped) = without_credentials(&setting.value);
                    if stripped {
                        format!("{shown} (credentials, query and options not shown)")
                    } else {
                        shown.to_string()
                    }
                }
            };
            evidence.push(
                Evidence::new(display_key(key), value).from_source(setting.source.to_string()),
            );
        }
        let message = match (evidence.len(), secrets) {
            (0, _) => "no value is set: built-in defaults apply".to_owned(),
            (n, 0) => format!("{n} value(s) set"),
            (n, k) => format!("{n} value(s) set; {k} credential(s), each a secret reference"),
        };
        let mut result = CheckResult::ok(s.id, s.category, message);
        result.evidence = evidence;
        Ok(result)
    }

    fn config_resolution(&self, s: Spec) -> Result<CheckResult, Blocked> {
        if self.config_failure.is_some() {
            return Err(Blocked("config.load"));
        }
        let settings = match &self.settings {
            Ok(settings) => settings,
            Err(e) => {
                let result = CheckResult::error(s.id, s.category, e.code, &e.message);
                return Ok(match &e.hint {
                    Some(hint) => result.hint(hint),
                    None => result,
                });
            }
        };
        Ok(resolution(s, settings))
    }

    // ------------------------------------------------------------------------ project

    fn dbt_project(&self, s: Spec) -> Result<CheckResult, Blocked> {
        let settings = self.settings()?;
        let dir = project_dir(settings);
        let file = dir.join("dbt_project.yml");
        let evidence = match &settings.project_dir {
            Some(setting) => setting_evidence("project_dir", setting),
            None => Evidence::new("project_dir", ".").from_source("default"),
        };
        Ok(if file.is_file() {
            CheckResult::ok(s.id, s.category, format!("found `{}`", file.display()))
        } else {
            CheckResult::error(
                s.id,
                s.category,
                codes::NO_PROJECT,
                format!("no dbt project in `{}`: `dbt_project.yml` isn't there", dir.display()),
            )
            .hint("run ODS in your dbt project, or name it with --project-dir, DBT_PROJECT_DIR or the dbt provider's `project_dir` setting")
        }
        .evidence(evidence))
    }

    fn manifest_check(&self, s: Spec) -> Result<CheckResult, Blocked> {
        let settings = self.settings()?;
        let target_dir = setting_evidence("target_dir", &settings.target_dir);
        Ok(match self.manifest(settings) {
            Ok(m) => {
                let format = match m.source {
                    ArtifactSource::ManifestJson => "manifest.json",
                    ArtifactSource::InfoSchema => "info_schema",
                    _ => "other",
                };
                CheckResult::ok(
                    s.id,
                    s.category,
                    format!(
                        "{format} schema v{}, written by dbt {}",
                        m.schema_version,
                        m.dbt_version.as_deref().unwrap_or("(unknown)")
                    ),
                )
                .evidence(target_dir)
                .fact("format", format)
                .fact("schema_version", m.schema_version.to_string())
                .fact("dbt_version", m.dbt_version.as_deref().unwrap_or("unknown"))
                .fact("nodes", m.nodes.len().to_string())
            }
            Err(problem) => CheckResult::error(
                s.id,
                s.category,
                crate::exit::codes::LINEAGE_ARTIFACTS,
                &problem.message,
            )
            .evidence(target_dir)
            .hint(problem.hint),
        })
    }

    fn project_name(&self, s: Spec) -> Result<CheckResult, Blocked> {
        let (_, manifest) = self.manifest_ready()?;
        Ok(match &manifest.project_name {
            Some(name) => CheckResult::ok(s.id, s.category, format!("project `{name}`"))
                .fact("project_name", name),
            None => CheckResult::warning(
                s.id,
                s.category,
                codes::NO_PROJECT_NAME,
                "the manifest names no project, so ODS can't tell it from another project's",
            )
            .hint("write it again with dbt 1.7 or later: `dbt parse`"),
        })
    }

    fn freshness(&self, s: Spec) -> Result<CheckResult, Blocked> {
        let (settings, _) = self.manifest_ready()?;
        let manifest = settings.target_dir().join("manifest.json");
        let age_unknown = |why: String| {
            CheckResult::unknown(s.id, s.category, codes::ARTIFACT_AGE_UNKNOWN, why)
                .fact("manifest", manifest.display().to_string())
                .hint("the manifest's age can't be compared with the project's files: dbt's Information Schema has no manifest.json, and some file systems keep no modification times; `dbt parse` writes it again")
        };
        let written = match std::fs::metadata(&manifest).and_then(|m| m.modified()) {
            Ok(written) => written,
            Err(e) => {
                return Ok(age_unknown(format!(
                    "can't tell when `{}` was written: {e}",
                    manifest.display()
                )));
            }
        };
        let Some(written_at) = time(written) else {
            return Ok(age_unknown(format!(
                "`{}`'s modification time can't be read as a date",
                manifest.display()
            )));
        };
        let dir = project_dir(settings);
        let newest = match newest_project_file(&dir, &settings.target_dir()) {
            Ok(newest) => newest,
            Err(why) => {
                return Ok(age_unknown(format!(
                    "can't tell whether a project file is newer than the manifest: {why}"
                )));
            }
        };
        let result = |r: CheckResult| {
            r.fact("manifest", manifest.display().to_string())
                .fact("manifest_modified", &written_at)
        };
        let Some((modified, file)) = newest else {
            return Ok(result(CheckResult::ok(
                s.id,
                s.category,
                "no project file is newer than the manifest",
            )));
        };
        let Some(modified_at) = time(modified) else {
            return Ok(age_unknown(format!(
                "`{}`'s modification time can't be read as a date",
                file.display()
            )));
        };
        let checked = if modified > written {
            CheckResult::warning(
                s.id,
                s.category,
                codes::STALE_ARTIFACTS,
                format!(
                    "the manifest is older than `{}`: plans would describe code that isn't what runs",
                    file.display()
                ),
            )
            .hint("parse the project again: `dbt parse`, or any `ods state` command that compiles (not with --no-compile)")
        } else {
            CheckResult::ok(
                s.id,
                s.category,
                "the manifest is newer than every project file",
            )
        };
        Ok(result(checked)
            .fact("newest_project_file", file.display().to_string())
            .fact("newest_project_file_modified", modified_at))
    }

    // -------------------------------------------------------------------------- tools

    fn tools_dbt(&self, s: Spec) -> Result<CheckResult, Blocked> {
        let settings = self.settings()?;
        let program = setting_evidence("program", &settings.program);
        let version = match self.dbt(settings) {
            Err(why) => {
                return Ok(CheckResult::error(
                    s.id,
                    s.category,
                    codes::DBT_MISSING,
                    format!("dbt can't be run: {why}"),
                )
                .evidence(program)
                .hint("install dbt-core with your adapter (`pip install dbt-core dbt-<adapter>`), or point ODS at it with --dbt or the dbt provider's `program` setting"));
            }
            Ok(None) => {
                return Ok(CheckResult::unknown(
                    s.id,
                    s.category,
                    codes::DBT_VERSION_UNKNOWN,
                    "`dbt --version` printed no version ODS can read",
                )
                .evidence(program)
                .hint("check that --dbt (or `program`) names dbt itself"));
            }
            Ok(Some(version)) => version,
        };
        let (min_major, min_minor) = MIN_SUPPORTED;
        let result = match version.support() {
            Support::TooOld => CheckResult::error(
                s.id,
                s.category,
                codes::DBT_TOO_OLD,
                format!(
                    "dbt {} is older than {min_major}.{min_minor}, the oldest whose manifests ODS reads",
                    version.raw
                ),
            )
            .hint(format!("upgrade dbt to {min_major}.{min_minor} or later")),
            Support::Untested => CheckResult::warning(
                s.id,
                s.category,
                codes::DBT_UNTESTED,
                format!("dbt {}: ODS is tested with dbt 1.x", version.raw),
            )
            .hint("if a command fails, try it with dbt 1.x, and report it"),
            _ => CheckResult::ok(s.id, s.category, format!("dbt {}", version.raw)),
        };
        Ok(result
            .evidence(program)
            .fact("version", &version.raw)
            .fact("adapters", plugins(version)))
    }

    fn tools_adapter(&self, s: Spec) -> Result<CheckResult, Blocked> {
        let (settings, manifest) = self.manifest_ready()?;
        let Some(adapter) = manifest.adapter_type.as_deref() else {
            return Ok(unknown_adapter(s));
        };
        let evidence = Evidence::new("adapter", adapter).from_source("manifest");
        let Some(version) = self.dbt_ready(settings)? else {
            return Ok(CheckResult::unknown(
                s.id,
                s.category,
                codes::ADAPTER_UNLISTED,
                "dbt's version can't be read, so neither can its adapters",
            )
            .evidence(evidence));
        };
        Ok(match version.has_plugin(adapter) {
            Some(true) => CheckResult::ok(
                s.id,
                s.category,
                format!("dbt has the `{adapter}` adapter the manifest was written with"),
            )
            .evidence(evidence),
            Some(false) => CheckResult::error(
                s.id,
                s.category,
                codes::ADAPTER_MISSING,
                format!("dbt has no `{adapter}` adapter, which the manifest was written with"),
            )
            .evidence(evidence)
            .fact("installed", plugins(version))
            .hint(format!(
                "install it next to dbt, e.g. `pip install dbt-{adapter}`"
            )),
            None => CheckResult::unknown(
                s.id,
                s.category,
                codes::ADAPTER_UNLISTED,
                "`dbt --version` lists no adapters, so whether this one is installed can't be told",
            )
            .evidence(evidence),
        })
    }

    // ------------------------------------------------------------------------- target

    fn target_identity(&self, s: Spec) -> Result<CheckResult, Blocked> {
        let settings = self.settings()?;
        self.dbt_ready(settings)?;
        let target = self.target.get_or_init(|| {
            block_on(Self::executor(settings).identify())
                .map_err(|e| e.message)
                .and_then(|r| r.map_err(|e| e.to_string()))
        });
        let environment = setting_evidence("environment", &settings.environment);
        Ok(match target {
            Ok(t) => {
                let mut result = CheckResult::ok(
                    s.id,
                    s.category,
                    format!("dbt builds in target `{}`", t.name),
                )
                .evidence(match &settings.target {
                    Some(setting) => setting_evidence("target", setting),
                    None => Evidence::new("target", &t.name).from_source("profile's default"),
                });
                for (key, value) in [
                    ("profile", &t.profile),
                    ("kind", &t.kind),
                    ("location", &t.location),
                    ("database", &t.database),
                ] {
                    result = result.fact(key, value.as_deref().unwrap_or("unknown"));
                }
                result.evidence(environment)
            }
            Err(why) => CheckResult::error(s.id, s.category, codes::TARGET_UNRENDERED, why)
                .evidence(environment)
                .hint("dbt must render the profile to build: check profiles.yml, --profiles-dir, --target and the variables it reads with env_var(); `dbt debug` says more"),
        })
    }

    // -------------------------------------------------------------------- state store

    fn state_store(&self, s: Spec) -> Result<CheckResult, Blocked> {
        let settings = self.settings()?;
        let db = setting_evidence("state_db", &settings.state_db);
        let report = match StoreReport::build(&settings.state_db()) {
            Ok(report) => report,
            Err(e) => {
                return Ok(CheckResult::error(s.id, s.category, e.code, e.message)
                    .evidence(db)
                    .hint("check the path and its permissions (--state-db, or `state.db` in ods.toml)"));
            }
        };
        if !report.exists {
            return Ok(CheckResult::ok(
                s.id,
                s.category,
                "no state database yet: the first run creates it",
            )
            .evidence(db));
        }
        let schema = report.schema.map(|v| {
            Evidence::new(
                "schema_version",
                format!("{} (this ODS: {})", v.version, v.latest),
            )
        });
        let mut result = if report
            .problems
            .iter()
            .any(|p| p.kind == ProblemKind::NewerSchema)
        {
            CheckResult::error(
                s.id,
                s.category,
                crate::exit::codes::STATE_STORE,
                "a newer ODS wrote this state database: this one can't read or write it",
            )
            .hint("upgrade ODS; nothing is wrong with the database")
        } else if !report.problems.is_empty() {
            CheckResult::error(
                s.id,
                s.category,
                crate::exit::codes::STATE_DAMAGED,
                format!("the state database has {} problem(s)", report.problems.len()),
            )
            .hint("`ods state doctor` lists them and how to recover; docs/cli.md#recovering-state")
        } else {
            let message = match report.schema {
                Some(v) if v.version < v.latest => format!(
                    "sound; the next command that writes migrates it from schema version {} to {}, keeping a copy first",
                    v.version, v.latest
                ),
                _ => "sound".to_owned(),
            };
            CheckResult::ok(s.id, s.category, message)
        }
        .evidence(db);
        if let Some(schema) = schema {
            result = result.evidence(schema);
        }
        for problem in &report.problems {
            result = result.fact(problem_label(problem.kind), &problem.detail);
        }
        Ok(result
            .fact("scopes", report.scopes.len().to_string())
            .fact("copies", report.copies.len().to_string()))
    }

    // ------------------------------------------------------------------- capabilities

    fn relation_existence(&self, s: Spec) -> Result<CheckResult, Blocked> {
        let settings = self.settings()?;
        let info = Self::executor(settings).info();
        let advertised = capability_list(info.capabilities.iter());
        Ok(
            if info.capabilities.contains(&Capability::RelationExistence) {
                CheckResult::ok(
                    s.id,
                    s.category,
                    "a build is reused only after checking its relation still exists",
                )
            } else {
                CheckResult::warning(
                    s.id,
                    s.category,
                    codes::NO_RELATION_CHECK,
                    "relations can't be checked before reuse: a dropped table is rebuilt only when something else changes",
                )
            }
            .fact("provider", format!("{} ({})", info.kind, info.instance))
            .fact("capabilities", advertised),
        )
    }

    fn relation_versions(&self, s: Spec) -> Result<CheckResult, Blocked> {
        let (settings, manifest) = self.manifest_ready()?;
        let Some(adapter) = manifest.adapter_type.as_deref() else {
            return Ok(unknown_adapter(s));
        };
        let executor = Self::executor(settings);
        let sources: Vec<_> = manifest
            .nodes
            .iter()
            .filter(|n| n.resource_type == ResourceType::Source)
            .collect();
        let adapter_evidence = Evidence::new("adapter", adapter).from_source("manifest");
        if let Some(provider) = change_provider(Some(adapter), &executor) {
            let info = provider.info();
            return Ok(CheckResult::ok(
                s.id,
                s.category,
                "sources' data versions are read from the warehouse's table versions",
            )
            .evidence(adapter_evidence)
            .fact("provider", format!("{} ({})", info.kind, info.instance))
            .fact("capabilities", capability_list(info.capabilities.iter()))
            .fact("sources", sources.len().to_string()));
        }
        let base = |r: CheckResult| {
            r.evidence(adapter_evidence.clone())
                .fact("missing_capability", Capability::RelationVersions.name())
        };
        if sources.is_empty() {
            return Ok(base(CheckResult::ok(
                s.id,
                s.category,
                "no table versions, and none needed: the project has no sources",
            )));
        }
        let mut without: Vec<String> = sources
            .iter()
            .filter(|n| n.config.loaded_at_field.is_none() && n.config.loaded_at_query.is_none())
            .map(|n| display_name(&n.unique_id))
            .collect();
        without.sort();
        Ok(if without.is_empty() {
            base(CheckResult::ok(
                s.id,
                s.category,
                "no table versions: sources report new data through `dbt source freshness`",
            ))
            .fact("sources", sources.len().to_string())
        } else {
            base(CheckResult::warning(
                s.id,
                s.category,
                codes::NO_SOURCE_VERSIONS,
                format!(
                    "no table versions for the `{adapter}` adapter, and {} of {} sources have no `loaded_at_field`: they count as changed on every run, so the models reading them always build",
                    without.len(),
                    sources.len()
                ),
            ))
            .fact("sources_without_loaded_at_field", without.join(", "))
            .hint("give those sources a `loaded_at_field` (or `loaded_at_query`) so `dbt source freshness` measures them; docs/cli.md#where-source-versions-come-from")
        })
    }

    // ------------------------------------------------------------------- connectivity

    fn live_relations(&self, s: Spec) -> Result<CheckResult, Blocked> {
        if !self.connect {
            return Ok(not_connected(s));
        }
        let (settings, manifest) = self.manifest_ready()?;
        self.dbt_ready(settings)?;
        let requested = relation_nodes(manifest);
        Ok(inspect_relations(s, &Self::executor(settings), &requested))
    }

    fn live_versions(&self, s: Spec) -> Result<CheckResult, Blocked> {
        if !self.connect {
            return Ok(not_connected(s));
        }
        let (settings, manifest) = self.manifest_ready()?;
        let Some(adapter) = manifest.adapter_type.as_deref() else {
            return Ok(unknown_adapter(s));
        };
        let executor = Self::executor(settings);
        let Some(provider) = change_provider(Some(adapter), &executor) else {
            return Ok(CheckResult::skipped(
                s.id,
                s.category,
                format!("the `{adapter}` adapter has no table versions to read"),
            ));
        };
        let sources: Vec<RequestedSource> = manifest
            .nodes
            .iter()
            .filter(|n| n.resource_type == ResourceType::Source)
            .map(|n| RequestedSource::new(n.unique_id.clone(), display_name(&n.unique_id)))
            .collect();
        if sources.is_empty() {
            return Ok(CheckResult::skipped(
                s.id,
                s.category,
                "the project has no sources",
            ));
        }
        self.dbt_ready(settings)?;
        Ok(probe_versions(s, &provider, &sources))
    }
}

/// The models, seeds and snapshots whose relations the live check asks about.
fn relation_nodes(manifest: &Manifest) -> Vec<RequestedNode> {
    manifest
        .nodes
        .iter()
        .filter(|n| {
            matches!(
                n.resource_type,
                ResourceType::Model | ResourceType::Seed | ResourceType::Snapshot
            ) && n.materialized.as_deref() != Some("ephemeral")
        })
        .map(|n| RequestedNode::new(n.unique_id.clone(), display_name(&n.unique_id)))
        .collect()
}

/// The live relation check, over any inspector.
fn inspect_relations<I: RelationInspector + ?Sized>(
    s: Spec,
    inspector: &I,
    requested: &[RequestedNode],
) -> CheckResult {
    if requested.is_empty() {
        return CheckResult::ok(s.id, s.category, "the project builds no relations to check");
    }
    let report = match block_on(inspector.inspect(requested)) {
        Ok(Ok(report)) => report,
        Ok(Err(e)) => return relation_failure(s, &e.to_string()),
        Err(e) => return relation_failure(s, &e.message),
    };
    let (mut present, mut missing, mut unknown) = (0, 0, 0);
    for (_, presence) in &report.nodes {
        match presence {
            RelationPresence::Present { .. } => present += 1,
            RelationPresence::Missing => missing += 1,
            _ => unknown += 1,
        }
    }
    // Anything asked about and not answered is unknown.
    unknown += requested.len().saturating_sub(report.nodes.len());
    let message = format!(
        "the relation check ran: {present} present, {missing} missing (they build on the next run), {unknown} unknown"
    );
    let result = if unknown == 0 {
        CheckResult::ok(s.id, s.category, message)
    } else {
        // It ran, but couldn't tell for some: that isn't a pass (AGENTS.md rule 3).
        CheckResult::unknown(s.id, s.category, codes::RELATIONS_UNKNOWN, message)
            .hint("the nodes it couldn't tell about are built rather than reused; `dbt debug` and the adapter's permissions say why")
    };
    result
        .fact("requested", requested.len().to_string())
        .fact("present", present.to_string())
        .fact("missing", missing.to_string())
        .fact("unknown", unknown.to_string())
}

fn relation_failure(s: Spec, why: &str) -> CheckResult {
    CheckResult::error(
        s.id,
        s.category,
        codes::RELATION_CHECK_FAILED,
        why.to_owned(),
    )
    .hint("check that the warehouse is reachable with the profile's credentials: `dbt debug`")
}

/// The live table-version probe, over any change provider.
fn probe_versions<P: ChangeProvider + ?Sized>(
    s: Spec,
    provider: &P,
    sources: &[RequestedSource],
) -> CheckResult {
    let mut warnings = Vec::new();
    let reading = match versions(provider, sources, &mut warnings) {
        Ok(reading) if warnings.is_empty() => reading,
        Ok(_) => return probe_failure(s, &warnings.join("; ")),
        Err(e) => return probe_failure(s, &e.message),
    };
    let total = sources.len();
    let names: std::collections::BTreeMap<&str, &str> = sources
        .iter()
        .map(|r| (r.id.as_str(), r.name.as_str()))
        .collect();
    let without: Vec<String> = reading
        .answers
        .iter()
        .filter_map(|(id, answer)| match answer {
            ods_state::VersionAnswer::Version(_) => None,
            ods_state::VersionAnswer::Unknown(why) => Some(format!(
                "{} ({why})",
                names.get(id.as_str()).copied().unwrap_or(id)
            )),
            _ => Some(format!(
                "{} (an answer ODS doesn't understand)",
                names.get(id.as_str()).copied().unwrap_or(id)
            )),
        })
        .collect();
    let known = total.saturating_sub(without.len());
    let message = format!("the table-version probe ran: {known} of {total} sources have a version");
    // It ran, but a source without a version gets its data version elsewhere, or none:
    // not a pass (AGENTS.md rule 3).
    let result = if without.is_empty() {
        CheckResult::ok(s.id, s.category, message)
    } else if known == 0 {
        CheckResult::unknown(s.id, s.category, codes::NO_TABLE_VERSIONS, message)
    } else {
        CheckResult::warning(s.id, s.category, codes::SOME_TABLE_VERSIONS, message)
    };
    let result = result
        .fact("sources", total.to_string())
        .fact("with_version", known.to_string());
    if without.is_empty() {
        return result;
    }
    result
        .fact("without_version", without.join(", "))
        .hint("those sources fall back to `max_loaded_at` from `dbt source freshness`, or count as changed; a view or a table that isn't Delta has no table version")
}

fn probe_failure(s: Spec, why: &str) -> CheckResult {
    CheckResult::error(s.id, s.category, codes::VERSION_PROBE_FAILED, why.to_owned())
        .hint("check the profile's credentials and that its user may read table history (`DESCRIBE HISTORY`); `dbt debug` says more")
}

/// The outcome of a check whose precondition `dependency` failed.
fn blocked(s: Spec, dependency: &str) -> CheckResult {
    CheckResult::unknown(
        s.id,
        s.category,
        codes::BLOCKED,
        format!("not checked: `{dependency}` failed"),
    )
    .fact("depends_on", dependency)
    .hint(format!("fix `{dependency}` first"))
}

fn not_connected(s: Spec) -> CheckResult {
    CheckResult::skipped(s.id, s.category, "a live check: pass --connect to run it")
}

fn unknown_adapter(s: Spec) -> CheckResult {
    CheckResult::unknown(
        s.id,
        s.category,
        codes::ADAPTER_UNKNOWN,
        "the manifest doesn't name its adapter (`metadata.adapter_type`)",
    )
    .hint("write it again with dbt 1.7 or later: `dbt parse`")
}

/// `value` with every string in it cleaned by [`strip_credentials`], and whether that
/// changed anything.
fn without_credentials(value: &toml::Value) -> (toml::Value, bool) {
    match value {
        toml::Value::String(text) => {
            let shown = strip_credentials(text);
            let stripped = shown != *text;
            (toml::Value::String(shown), stripped)
        }
        toml::Value::Array(items) => {
            let mut stripped = false;
            let items = items
                .iter()
                .map(|item| {
                    let (item, s) = without_credentials(item);
                    stripped |= s;
                    item
                })
                .collect();
            (toml::Value::Array(items), stripped)
        }
        toml::Value::Table(table) => {
            let mut stripped = false;
            let table = table
                .iter()
                .map(|(k, v)| {
                    let (v, s) = without_credentials(v);
                    stripped |= s;
                    (k.clone(), v)
                })
                .collect();
            (toml::Value::Table(table), stripped)
        }
        other => (other.clone(), false),
    }
}

/// `config.resolution` for resolved settings: where ODS finds the project, dbt and
/// the state. A profiles directory named for dbt with no `profiles.yml` in it is a
/// warning: dbt finds no profile there (only the file's presence is checked; ODS never
/// reads it, AGENTS.md rule 9).
fn resolution(s: Spec, settings: &StateSettings) -> CheckResult {
    let optional = |key: &str, setting: &Option<Setting>| match setting {
        Some(setting) => setting_evidence(key, setting),
        None => Evidence::new(key, "unset").from_source("dbt's default"),
    };
    let missing_profiles = settings
        .profiles_dir
        .as_ref()
        .filter(|dir| !Path::new(&dir.value).join("profiles.yml").is_file());
    let mut result = match missing_profiles {
        Some(dir) => CheckResult::warning(
            s.id,
            s.category,
            codes::PROFILES_MISSING,
            format!(
                "no `profiles.yml` in the profiles directory `{}` ({}): dbt finds no profile there",
                dir.value,
                dir.origin.label()
            ),
        )
        .hint("name the directory that holds profiles.yml with --profiles-dir, DBT_PROFILES_DIR or the dbt provider's `profiles_dir` setting, or leave it unset for dbt's default"),
        None => CheckResult::ok(
            s.id,
            s.category,
            "where ODS finds the project, dbt and the state",
        ),
    };
    result.evidence = vec![
        setting_evidence("program", &settings.program),
        optional("project_dir", &settings.project_dir),
        optional("profiles_dir", &settings.profiles_dir),
        optional("profile", &settings.profile),
        optional("target", &settings.target),
        setting_evidence("target_dir", &settings.target_dir),
        setting_evidence("state_db", &settings.state_db),
        setting_evidence("environment", &settings.environment),
    ];
    result
}

/// The id of the check whose findings explain a configuration failure best:
/// `config.resolution` (#323, #181).
pub(super) const RESOLUTION: &str = "config.resolution";

/// The local, side-effect-free checks that bear on a failure of dbt's profile or
/// credentials, for explanations (#323, #181): `config.load` and, for credentials,
/// `config.values` when the configuration is at hand, and `config.resolution`. None
/// runs dbt, opens a connection or writes anything.
pub(super) fn configuration_checks(
    config: Option<&Loaded>,
    settings: &StateSettings,
    credentials: bool,
) -> Vec<CheckResult> {
    let spec = |id: &str| {
        *SPECS
            .iter()
            .find(|s| s.id == id)
            .expect("every configuration check is in SPECS")
    };
    let mut results = Vec::new();
    if let Some(config) = config {
        let checks = Checks {
            config,
            config_failure: None,
            settings: Ok(settings.clone()),
            connect: false,
            manifest: OnceCell::new(),
            dbt: OnceCell::new(),
            target: OnceCell::new(),
        };
        results.push(checks.check(spec("config.load")));
        if credentials {
            results.push(checks.check(spec("config.values")));
        }
    }
    results.push(resolution(spec(RESOLUTION), settings));
    results
}

fn setting_evidence(key: &str, setting: &Setting) -> Evidence {
    Evidence::new(key, &setting.value).from_source(setting.origin.label())
}

fn project_dir(settings: &StateSettings) -> PathBuf {
    settings
        .project_dir
        .as_ref()
        .map_or_else(|| PathBuf::from("."), |d| PathBuf::from(&d.value))
}

fn plugins(version: &DbtVersion) -> String {
    if version.plugins.is_empty() {
        return "none listed".to_owned();
    }
    version
        .plugins
        .iter()
        .map(|(name, v)| format!("{name} {v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn capability_list<'c>(capabilities: impl Iterator<Item = &'c Capability>) -> String {
    let names: Vec<&str> = capabilities.map(Capability::name).collect();
    if names.is_empty() {
        "none".to_owned()
    } else {
        names.join(", ")
    }
}

/// `at` as a UTC timestamp; `None` before 1970 or beyond what a timestamp holds.
fn time(at: SystemTime) -> Option<String> {
    let seconds = at.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs();
    Some(Timestamp::from_unix(i64::try_from(seconds).ok()?).to_string())
}

/// Directories that hold no project code: dbt's own output and installed packages,
/// tools' caches and hidden directories.
const NOT_CODE: [&str; 7] = [
    "target",
    "dbt_packages",
    "dbt_modules",
    "logs",
    "node_modules",
    "venv",
    "__pycache__",
];

/// Extensions of the files dbt parses.
const CODE: [&str; 5] = ["sql", "yml", "yaml", "csv", "py"];

/// The most recently modified project file under `dir` (ties: the greatest path),
/// leaving out `target_dir` and directories that hold no project code.
///
/// # Errors
/// Any directory or file that can't be read, or has no modification time: the
/// answer would be a guess, so there is none (AGENTS.md rule 3).
fn newest_project_file(
    dir: &Path,
    target_dir: &Path,
) -> Result<Option<(SystemTime, PathBuf)>, String> {
    let target_dir = std::path::absolute(target_dir).ok();
    let walk = walkdir::WalkDir::new(dir)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 || !entry.file_type().is_dir() {
                return true;
            }
            let name = entry.file_name().to_string_lossy();
            let is_target =
                target_dir.is_some() && std::path::absolute(entry.path()).ok() == target_dir;
            !(name.starts_with('.') || NOT_CODE.contains(&name.as_ref()) || is_target)
        });
    let mut newest = None;
    for entry in walk {
        let entry = entry.map_err(|e| e.to_string())?;
        let is_code = entry.file_type().is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|x| CODE.contains(&x.to_string_lossy().as_ref()));
        if !is_code {
            continue;
        }
        let modified = entry
            .metadata()
            .map_err(|e| e.to_string())?
            .modified()
            .map_err(|e| format!("`{}`: {e}", entry.path().display()))?;
        newest = newest.max(Some((modified, entry.into_path())));
    }
    Ok(newest)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use clap::{Arg, Command};
    use ods_core::CheckStatus;
    use ods_provider_fake::{FakeChangeProvider, FakeClock, FakeExecutor};

    use super::*;

    const MANIFEST: &str = "../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10-build/manifest.json";

    fn fixture(path: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
    }

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn touch(path: &Path, seconds: u64) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(at(seconds))
            .unwrap();
    }

    /// A project directory: `dbt_project.yml`, a model and `target/manifest.json`,
    /// the manifest written after the code.
    struct Project {
        dir: tempfile::TempDir,
    }

    impl Project {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            std::fs::create_dir_all(root.join("models")).unwrap();
            std::fs::create_dir_all(root.join("target")).unwrap();
            std::fs::write(root.join("dbt_project.yml"), "name: jaffle_ods\n").unwrap();
            std::fs::write(root.join("models/orders.sql"), "select 1\n").unwrap();
            std::fs::copy(fixture(MANIFEST), root.join("target/manifest.json")).unwrap();
            touch(&root.join("dbt_project.yml"), 1_000);
            touch(&root.join("models/orders.sql"), 2_000);
            touch(&root.join("target/manifest.json"), 3_000);
            Self { dir }
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }

        /// The checks with `args` alone (no `--project-dir`).
        fn run_in(args: &[&str], options: &Options) -> Vec<CheckResult> {
            let config = empty_config();
            let mut line = vec!["doctor"];
            line.extend(args);
            Checks::new(&matches(line), &config, None, options.connect).run(options)
        }

        /// The checks, with `--project-dir` pointing here and extra `args`.
        fn run(&self, args: &[&str], options: &Options) -> Vec<CheckResult> {
            let config = empty_config();
            let dir = self.path().display().to_string();
            let mut line = vec!["doctor", "--project-dir", dir.as_str()];
            line.extend(args);
            let matches = matches(line);
            Checks::new(&matches, &config, None, options.connect).run(options)
        }
    }

    /// A dbt that can't exist: no test ever runs the host's dbt, which could read
    /// `~/.dbt/profiles.yml` or reach the network. Tests that need dbt use fakes.
    const NO_DBT: &str = "/nonexistent/ods-doctor-test/dbt";

    /// `line` parsed as `ods doctor`'s arguments, with `--dbt` [`NO_DBT`] unless it
    /// names one.
    fn matches<'a>(line: impl IntoIterator<Item = &'a str>) -> ArgMatches {
        let mut line: Vec<&str> = line.into_iter().collect();
        if !line.contains(&"--dbt") {
            line.extend(["--dbt", NO_DBT]);
        }
        command().get_matches_from(line)
    }

    fn empty_config() -> Loaded {
        ods_config::load(&ods_config::Inputs::default()).unwrap()
    }

    fn command() -> Command {
        let mut command = Command::new("doctor");
        for id in [
            "project-dir",
            "target-dir",
            "dbt",
            "profiles-dir",
            "dbt-profile",
            "target",
            "state-db",
            "environment",
        ] {
            command = command.arg(Arg::new(id).long(id));
        }
        command
    }

    fn only(id: &str, checks: &[CheckResult]) -> CheckResult {
        checks.iter().find(|c| c.id == id).unwrap().clone()
    }

    fn project_options() -> Options {
        Options {
            project_only: true,
            ..Options::default()
        }
    }

    #[test]
    fn a_fresh_project_passes_every_project_check() {
        let project = Project::new();
        let checks = project.run(&[], &project_options());
        let ids: Vec<&str> = checks.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "project.dbt_project",
                "project.manifest",
                "project.name",
                "project.freshness"
            ]
        );
        for check in &checks {
            assert_eq!(check.status, CheckStatus::Ok, "{check:?}");
            assert_eq!(check.provider.as_deref(), Some("dbt"));
        }
        assert!(only("project.dbt_project", &checks).required);
        assert!(!only("project.name", &checks).required);
        let name = only("project.name", &checks);
        assert_eq!(name.evidence[0].value, "jaffle_ods");
    }

    #[test]
    fn a_missing_project_and_manifest_are_errors_and_what_depends_on_them_unknown() {
        let project = Project::new();
        std::fs::remove_file(project.path().join("dbt_project.yml")).unwrap();
        std::fs::remove_file(project.path().join("target/manifest.json")).unwrap();
        let checks = project.run(&[], &project_options());
        let dbt_project = only("project.dbt_project", &checks);
        assert_eq!(dbt_project.status, CheckStatus::Error);
        assert_eq!(dbt_project.code.as_deref(), Some(codes::NO_PROJECT));
        let manifest = only("project.manifest", &checks);
        assert_eq!(manifest.status, CheckStatus::Error);
        assert_eq!(manifest.code.as_deref(), Some("ODS-E0201"));
        assert!(manifest.hint.as_deref().unwrap().contains("dbt parse"));
        for id in ["project.name", "project.freshness"] {
            let check = only(id, &checks);
            assert_eq!(check.status, CheckStatus::Unknown, "{check:?}");
            assert_eq!(check.code.as_deref(), Some(codes::BLOCKED));
            assert_eq!(check.evidence[0].value, "project.manifest");
        }
    }

    #[test]
    fn an_unsupported_manifest_says_which_versions_are_read() {
        let project = Project::new();
        let path = project.path().join("target/manifest.json");
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("manifest/v12.json", "manifest/v9.json");
        std::fs::write(&path, text).unwrap();
        let manifest = only("project.manifest", &project.run(&[], &project_options()));
        assert_eq!(manifest.status, CheckStatus::Error);
        assert!(manifest.message.contains("v9"), "{manifest:?}");
        assert!(manifest.hint.unwrap().contains("v11 and v12"));
    }

    #[test]
    fn code_newer_than_the_manifest_is_stale() {
        let project = Project::new();
        touch(&project.path().join("models/orders.sql"), 4_000);
        // Files dbt doesn't parse, and dbt's own directories, don't count.
        std::fs::create_dir_all(project.path().join("dbt_packages/x")).unwrap();
        std::fs::write(project.path().join("dbt_packages/x/m.sql"), "").unwrap();
        touch(&project.path().join("dbt_packages/x/m.sql"), 9_000);
        std::fs::write(project.path().join("notes.txt"), "").unwrap();
        touch(&project.path().join("notes.txt"), 9_000);
        let check = only("project.freshness", &project.run(&[], &project_options()));
        assert_eq!(check.status, CheckStatus::Warning);
        assert_eq!(check.code.as_deref(), Some(codes::STALE_ARTIFACTS));
        let facts: Vec<(&str, &str)> = check
            .evidence
            .iter()
            .map(|e| (e.key.as_str(), e.value.as_str()))
            .collect();
        assert!(
            facts.contains(&("manifest_modified", "1970-01-01T00:50:00Z")),
            "{facts:?}"
        );
        assert!(facts.contains(&("newest_project_file_modified", "1970-01-01T01:06:40Z")));
        assert!(check.message.contains("orders.sql"), "{check:?}");
    }

    #[test]
    fn a_missing_project_name_warns() {
        let project = Project::new();
        let path = project.path().join("target/manifest.json");
        let mut json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        json["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("project_name");
        std::fs::write(&path, json.to_string()).unwrap();
        let check = only("project.name", &project.run(&[], &project_options()));
        assert_eq!(check.status, CheckStatus::Warning);
        assert_eq!(check.code.as_deref(), Some(codes::NO_PROJECT_NAME));
    }

    #[test]
    fn a_config_failure_blocks_everything_that_reads_settings() {
        let project = Project::new();
        let config = empty_config();
        let dir = project.path().display().to_string();
        let matches = matches(["doctor", "--project-dir", dir.as_str()]);
        let failure = ConfigFailure::new("ODS-E0101", "ods.toml is not valid TOML: boom");
        let checks = Checks::new(&matches, &config, Some(failure), false).run(&Options::default());
        let load = only("config.load", &checks);
        assert_eq!(load.status, CheckStatus::Error);
        assert_eq!(load.code.as_deref(), Some("ODS-E0101"));
        assert!(load.required);
        for check in checks.iter().filter(|c| c.id != "config.load") {
            if check.category == C::Connectivity {
                assert_eq!(check.status, CheckStatus::Skipped, "{check:?}");
            } else {
                assert_eq!(check.status, CheckStatus::Unknown, "{check:?}");
                assert_eq!(check.evidence[0].value, "config.load");
            }
        }
    }

    #[test]
    fn config_values_show_secret_references_and_where_each_came_from() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("ods.toml");
        std::fs::write(
            &file,
            "[providers.wh]\nkind = \"x\"\nsettings = { host = \"h\", token = { secret = \"env:WH_TOKEN\" } }\n",
        )
        .unwrap();
        let config = ods_config::load(&ods_config::Inputs {
            project_file: Some(file),
            ..ods_config::Inputs::default()
        })
        .unwrap();
        let matches = matches(["doctor"]);
        let checks = Checks::new(&matches, &config, None, false).run(&Options::default());
        let values = only("config.values", &checks);
        assert_eq!(values.status, CheckStatus::Ok);
        assert!(values.message.contains("1 credential(s)"), "{values:?}");
        let token = values
            .evidence
            .iter()
            .find(|e| e.key == "providers.wh.settings.token")
            .unwrap();
        assert_eq!(token.value, "secret(env:WH_TOKEN)");
        assert!(token.source.as_deref().unwrap().starts_with("project file"));
        let load = only("config.load", &checks);
        assert!(
            load.evidence
                .iter()
                .any(|e| e.key == "project file" && e.source.as_deref() == Some("loaded"))
        );
    }

    #[test]
    fn two_dbt_providers_fail_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("ods.toml");
        std::fs::write(
            &file,
            "[providers.a]\nkind = \"dbt\"\n[providers.b]\nkind = \"dbt\"\n",
        )
        .unwrap();
        let config = ods_config::load(&ods_config::Inputs {
            project_file: Some(file),
            ..ods_config::Inputs::default()
        })
        .unwrap();
        let matches = matches(["doctor"]);
        let checks = Checks::new(&matches, &config, None, false).run(&Options::default());
        let resolution = only("config.resolution", &checks);
        assert_eq!(resolution.status, CheckStatus::Error);
        assert_eq!(resolution.code.as_deref(), Some("ODS-E0102"));
        assert_eq!(
            only("project.manifest", &checks).evidence[0].value,
            "config.resolution"
        );
    }

    #[test]
    fn resolution_says_where_each_setting_came_from() {
        let project = Project::new();
        let checks = project.run(&["--target", "prod"], &Options::default());
        let resolution = only("config.resolution", &checks);
        let find = |key: &str| {
            resolution
                .evidence
                .iter()
                .find(|e| e.key == key)
                .unwrap()
                .clone()
        };
        assert_eq!(find("target").value, "prod");
        assert_eq!(find("target").source.as_deref(), Some("flag"));
        assert_eq!(find("environment").source.as_deref(), Some("target"));
        assert_eq!(find("program").value, NO_DBT);
        assert_eq!(find("program").source.as_deref(), Some("flag"));
        assert_eq!(find("profile").value, "unset");
    }

    #[test]
    fn a_missing_dbt_is_an_error_and_the_target_unknown() {
        let project = Project::new();
        let missing = project.path().join("no-such-dbt");
        let checks = project.run(
            &["--dbt", missing.to_str().unwrap()],
            &Options {
                provider: Some("dbt".to_owned()),
                ..Options::default()
            },
        );
        let dbt = only("tools.dbt", &checks);
        assert_eq!(dbt.status, CheckStatus::Error);
        assert_eq!(dbt.code.as_deref(), Some(codes::DBT_MISSING));
        let target = only("target.identity", &checks);
        assert_eq!(target.status, CheckStatus::Unknown);
        assert!(target.required);
        assert_eq!(target.evidence[0].value, "tools.dbt");
        let adapter = only("tools.adapter", &checks);
        assert_eq!(adapter.status, CheckStatus::Unknown);
        // Only the dbt provider's checks ran.
        assert!(checks.iter().all(|c| c.provider.as_deref() == Some("dbt")));
        assert!(!checks.iter().any(|c| c.id == "state_store.database"));
    }

    #[test]
    fn the_state_store_check_is_state_doctors() {
        let project = Project::new();
        let db = project.path().join(".ods/state.db");
        let options = Options {
            provider: Some("sqlite".to_owned()),
            ..Options::default()
        };
        let db_arg = db.display().to_string();
        let checks = project.run(&["--state-db", db_arg.as_str()], &options);
        let store = only("state_store.database", &checks);
        assert_eq!(store.status, CheckStatus::Ok);
        assert!(store.message.contains("no state database yet"));

        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(
            &db,
            "not a database at all, but long enough to have a header",
        )
        .unwrap();
        let store = only(
            "state_store.database",
            &project.run(&["--state-db", db_arg.as_str()], &options),
        );
        assert_eq!(store.status, CheckStatus::Error, "{store:?}");
        assert_eq!(store.code.as_deref(), Some("ODS-E0405"));
        assert!(store.hint.unwrap().contains("ods state doctor"));
    }

    #[test]
    fn capabilities_say_what_is_missing_and_why_it_matters() {
        let project = Project::new();
        let options = Options {
            project_only: false,
            ..Options::default()
        };
        let checks = project.run(&[], &options);
        let existence = only("capabilities.relation_existence", &checks);
        assert_eq!(existence.status, CheckStatus::Ok);
        assert!(
            existence
                .evidence
                .iter()
                .any(|e| e.value.contains("relation_existence"))
        );
        // The fixture is DuckDB without sources: no table versions, none needed.
        let versions = only("capabilities.relation_versions", &checks);
        assert_eq!(versions.status, CheckStatus::Ok, "{versions:?}");
        assert!(versions.message.contains("no sources"));

        // With a source lacking `loaded_at_field`, its readers always build.
        let path = project.path().join("target/manifest.json");
        let mut json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        json["sources"]["source.jaffle_ods.raw.orders"] = serde_json::json!({
            "unique_id": "source.jaffle_ods.raw.orders", "resource_type": "source",
            "name": "orders", "source_name": "raw", "fqn": ["jaffle_ods", "raw", "orders"],
            "config": {"enabled": true}
        });
        std::fs::write(&path, json.to_string()).unwrap();
        touch(&path, 3_000);
        let versions = only(
            "capabilities.relation_versions",
            &project.run(&[], &options),
        );
        assert_eq!(versions.status, CheckStatus::Warning, "{versions:?}");
        assert_eq!(versions.code.as_deref(), Some(codes::NO_SOURCE_VERSIONS));
        assert!(versions.evidence.iter().any(|e| e.value == "raw.orders"));

        // On Databricks, table versions are read.
        json["metadata"]["adapter_type"] = "databricks".into();
        std::fs::write(&path, json.to_string()).unwrap();
        let versions = only(
            "capabilities.relation_versions",
            &project.run(&[], &options),
        );
        assert_eq!(versions.status, CheckStatus::Ok, "{versions:?}");
        assert!(
            versions
                .evidence
                .iter()
                .any(|e| e.value == "relation_versions")
        );
    }

    #[test]
    fn live_checks_are_skipped_unless_asked_for() {
        let project = Project::new();
        let checks = project.run(&[], &Options::default());
        for id in ["connectivity.relations", "connectivity.table_versions"] {
            let check = only(id, &checks);
            assert_eq!(check.status, CheckStatus::Skipped);
            assert!(check.message.contains("--connect"));
        }
    }

    const SPEC_RELATIONS: Spec = SPECS[13];
    const SPEC_VERSIONS: Spec = SPECS[14];

    #[test]
    fn the_live_relation_check_counts_what_it_found_or_fails() {
        let executor = FakeExecutor::new(FakeClock::new(), ["model.a", "model.b"]);
        executor.drop_relation("model.b");
        let requested = [
            RequestedNode::new("model.a", "a"),
            RequestedNode::new("model.b", "b"),
            RequestedNode::new("model.c", "c"),
        ];
        let check = inspect_relations(SPEC_RELATIONS, &executor, &requested[..2]);
        assert_eq!(check.status, CheckStatus::Ok, "{check:?}");
        assert!(check.message.contains("1 present, 1 missing"), "{check:?}");

        // A relation it couldn't tell about: not a pass.
        let check = inspect_relations(SPEC_RELATIONS, &executor, &requested);
        assert_eq!(check.status, CheckStatus::Unknown);
        assert_eq!(check.code.as_deref(), Some(codes::RELATIONS_UNKNOWN));
        assert!(check.message.contains("1 unknown"), "{check:?}");

        let failing = executor.failing_inspection();
        let check = inspect_relations(SPEC_RELATIONS, &failing, &requested);
        assert_eq!(check.status, CheckStatus::Error);
        assert_eq!(check.code.as_deref(), Some(codes::RELATION_CHECK_FAILED));
        assert!(check.message.contains("can't be reached"));
    }

    #[test]
    fn the_live_version_probe_counts_versions_or_fails() {
        let sources = [
            RequestedSource::new("a", "a"),
            RequestedSource::new("b", "b"),
        ];
        let all = FakeChangeProvider::new().with_source("a").with_source("b");
        let check = probe_versions(SPEC_VERSIONS, &all, &sources);
        assert_eq!(check.status, CheckStatus::Ok, "{check:?}");
        assert!(check.message.contains("2 of 2"), "{check:?}");

        // Some without a version: a warning naming them and why.
        let some = FakeChangeProvider::new()
            .with_source("a")
            .unreadable("b", "a view");
        let check = probe_versions(SPEC_VERSIONS, &some, &sources);
        assert_eq!(check.status, CheckStatus::Warning);
        assert_eq!(check.code.as_deref(), Some(codes::SOME_TABLE_VERSIONS));
        assert!(check.message.contains("1 of 2"), "{check:?}");
        assert!(
            check.evidence.iter().any(|e| e.value == "b (a view)"),
            "{check:?}"
        );

        // None with a version: the probe ran but concluded nothing.
        let none = FakeChangeProvider::new()
            .unreadable("a", "a view")
            .unreadable("b", "not Delta");
        let check = probe_versions(SPEC_VERSIONS, &none, &sources);
        assert_eq!(check.status, CheckStatus::Unknown);
        assert_eq!(check.code.as_deref(), Some(codes::NO_TABLE_VERSIONS));

        let check = probe_versions(SPEC_VERSIONS, &some.failing(), &sources);
        assert_eq!(check.status, CheckStatus::Error);
        assert_eq!(check.code.as_deref(), Some(codes::VERSION_PROBE_FAILED));
    }

    #[test]
    fn filters_pick_checks_by_category_and_provider() {
        let project = Project::new();
        let checks = project.run(
            &[],
            &Options {
                provider: Some("databricks".to_owned()),
                ..Options::default()
            },
        );
        // The fixture is DuckDB: the source-version checks concern dbt, not Databricks.
        assert!(checks.is_empty(), "{checks:?}");
        let dbt = project.run(
            &[],
            &Options {
                provider: Some("dbt".to_owned()),
                ..Options::default()
            },
        );
        let versions = only("capabilities.relation_versions", &dbt);
        assert_eq!(versions.provider.as_deref(), Some("dbt"));

        set_adapter(&project, "databricks");
        let checks = project.run(
            &[],
            &Options {
                provider: Some("databricks".to_owned()),
                ..Options::default()
            },
        );
        let ids: Vec<&str> = checks.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "capabilities.relation_versions",
                "connectivity.table_versions"
            ]
        );
        assert!(
            checks
                .iter()
                .all(|c| c.provider.as_deref() == Some("databricks"))
        );
        let both = project.run(
            &[],
            &Options {
                project_only: true,
                provider: Some("sqlite".to_owned()),
                ..Options::default()
            },
        );
        assert!(both.is_empty(), "{both:?}");
    }

    /// Sets the manifest's adapter, keeping its time.
    fn set_adapter(project: &Project, adapter: &str) {
        let path = project.path().join("target/manifest.json");
        let mut json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        json["metadata"]["adapter_type"] = adapter.into();
        std::fs::write(&path, json.to_string()).unwrap();
        touch(&path, 3_000);
    }

    #[test]
    fn a_manifest_without_an_adapter_leaves_the_live_probe_unknown() {
        let project = Project::new();
        let path = project.path().join("target/manifest.json");
        let mut json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        json["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("adapter_type");
        std::fs::write(&path, json.to_string()).unwrap();
        touch(&path, 3_000);
        let checks = project.run(
            &[],
            &Options {
                connect: true,
                ..Options::default()
            },
        );
        let versions = only("connectivity.table_versions", &checks);
        assert_eq!(versions.status, CheckStatus::Unknown, "{versions:?}");
        assert_eq!(versions.code.as_deref(), Some(codes::ADAPTER_UNKNOWN));
    }

    #[test]
    fn a_project_that_cant_be_walked_leaves_freshness_unknown() {
        let project = Project::new();
        let target = project.path().join("target").display().to_string();
        let missing = project.path().join("gone").display().to_string();
        let checks = Project::run_in(
            &[
                "--project-dir",
                missing.as_str(),
                "--target-dir",
                target.as_str(),
            ],
            &project_options(),
        );
        let freshness = only("project.freshness", &checks);
        assert_eq!(freshness.status, CheckStatus::Unknown, "{freshness:?}");
        assert_eq!(freshness.code.as_deref(), Some(codes::ARTIFACT_AGE_UNKNOWN));
        assert!(freshness.message.contains("gone"), "{freshness:?}");
        assert!(newest_project_file(Path::new("/nonexistent/ods"), Path::new("target")).is_err());
    }

    #[test]
    fn a_time_before_1970_is_no_time() {
        assert_eq!(time(at(0)).as_deref(), Some("1970-01-01T00:00:00Z"));
        if let Some(before) = SystemTime::UNIX_EPOCH.checked_sub(Duration::from_secs(1)) {
            assert_eq!(time(before), None);
        }
    }

    #[test]
    fn connection_strings_in_config_values_show_no_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("ods.toml");
        std::fs::write(
            &file,
            "[providers.wh]\nkind = \"x\"\nsettings = { url = \"postgres://u:hunter2@h/db?sslpassword=x\", hosts = [\"a:b@c\"], name = \"plain\" }\n",
        )
        .unwrap();
        let config = ods_config::load(&ods_config::Inputs {
            project_file: Some(file),
            ..ods_config::Inputs::default()
        })
        .unwrap();
        let checks = Checks::new(&matches(["doctor"]), &config, None, false).run(&Options {
            provider: None,
            ..Options::default()
        });
        let values = only("config.values", &checks);
        let value = |key: &str| {
            values
                .evidence
                .iter()
                .find(|e| e.key == key)
                .unwrap()
                .value
                .clone()
        };
        let url = value("providers.wh.settings.url");
        assert_eq!(url, "\"h/db\" (credentials, query and options not shown)");
        let hosts = value("providers.wh.settings.hosts");
        assert!(!hosts.contains("a:b"), "{hosts}");
        assert_eq!(value("providers.wh.settings.name"), "\"plain\"");
        // A password long enough that a random temp directory in `source` can't hold it.
        let all = format!("{values:?}");
        assert!(
            !all.contains("hunter2") && !all.contains("sslpassword"),
            "{all}"
        );
    }

    #[test]
    fn every_code_and_check_is_documented() {
        let docs = std::fs::read_to_string(fixture("../../docs/cli.md")).unwrap();
        let section = docs.split("## `ods doctor`").nth(1).unwrap();
        for code in codes::ALL {
            assert!(
                section.contains(&format!("| `{code}` |")),
                "{code} isn't in docs/cli.md"
            );
        }
        for s in SPECS {
            assert!(
                section.contains(&format!("| `{}` |", s.id)),
                "{} isn't in docs/cli.md",
                s.id
            );
        }
    }

    #[test]
    fn every_spec_has_a_check_and_a_unique_id() {
        let mut ids: Vec<&str> = SPECS.iter().map(|s| s.id).collect();
        for s in SPECS {
            assert!(s.id.starts_with(s.category.name()), "{}", s.id);
            assert!(s.providers.iter().all(|p| PROVIDERS.contains(p)));
        }
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), SPECS.len());
    }
}
