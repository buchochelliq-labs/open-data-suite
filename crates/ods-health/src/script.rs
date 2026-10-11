//! Script checks (ADR-0030 §4, §4b): a command the project names, run by the host over
//! the script protocol. The engine starts no process (ADR-0031 §6): it reads the
//! definition, digests it for trust, and runs the [`HealthCheck`] the host attaches for
//! it, if the host can run scripts (`ods health check` can; the dashboard never does).
//!
//! A script runs only once trusted; untrusted, or with nothing to run it, every node it
//! selects is *unknown*, saying why, never a pass (AGENTS rule 3).

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ods_config::DeclaredCheckConfig;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::{
    CheckRun, CheckSource, Finding, HealthCheck, HealthConfigError, NodeFacts, Selector, Severity,
    Status,
};

/// How long a script may take unless its `timeout` says otherwise.
pub const SCRIPT_TIMEOUT: Duration = Duration::from_secs(60);

/// The longest `timeout` a script may have.
const LONGEST: Duration = Duration::from_secs(60 * 60);

/// What a host needs to run a script, and what a trust entry pins for it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ScriptDefinition {
    /// The check's id.
    pub id: String,
    /// Its command: its program, then its arguments.
    pub command: Vec<String>,
    /// Its configured parameters, passed on in its request.
    pub params: BTreeMap<String, serde_json::Value>,
    /// How long it may take.
    pub timeout: Duration,
    /// `sha256:` and the digest of what decides what runs and on what (§4d).
    pub digest: String,
}

/// A script check, as configured.
#[derive(Clone)]
pub(crate) struct Script {
    pub(crate) id: String,
    pub(crate) severity: Option<Severity>,
    select: Option<Selector>,
    exclude: Option<Selector>,
    command: Vec<String>,
    params: BTreeMap<String, serde_json::Value>,
    pub(crate) timeout: Duration,
    /// What the digest covers as configured, before [`Script::pin`] adds the host's.
    digested: Vec<u8>,
    digest: String,
    trusted: bool,
    /// What runs it, from the host; `None` where scripts aren't run.
    pub(crate) runner: Option<Arc<dyn HealthCheck>>,
}

impl fmt::Debug for Script {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Script")
            .field("id", &self.id)
            .field("severity", &self.severity)
            .field("command", &self.command)
            .field("timeout", &self.timeout)
            .field("digest", &self.digest)
            .field("trusted", &self.trusted)
            .field("runner", &self.runner.is_some())
            .finish_non_exhaustive()
    }
}

/// What a script's digest covers as configured, in a fixed order: what runs, and on
/// what. Its severity, timeout and parameters aren't in it: changing them runs nothing
/// new. The host adds where it runs and the files it names ([`Script::pin`]).
#[derive(Serialize)]
struct Digested<'a> {
    id: &'a str,
    kind: &'a str,
    command: &'a [String],
    select: Option<&'a ods_config::HealthSelector>,
    exclude: Option<&'a ods_config::HealthSelector>,
}

impl Script {
    pub(crate) fn from_config(
        config: &DeclaredCheckConfig,
        at: &str,
        severity: Option<Severity>,
        select: Option<Selector>,
        exclude: Option<Selector>,
    ) -> Result<Self, HealthConfigError> {
        if !config.require.is_empty() {
            return Err(HealthConfigError(format!(
                "{at}.require: a script has a `command`, not `require`"
            )));
        }
        for (field, set) in [
            ("sql", config.sql.is_some()),
            ("pass", config.pass.is_some()),
        ] {
            if set {
                return Err(HealthConfigError(format!(
                    "{at}.{field}: only a `kind = \"probe\"` check has `{field}`"
                )));
            }
        }
        let command = config.command.clone().unwrap_or_default();
        if command
            .first()
            .is_none_or(|program| program.trim().is_empty())
        {
            return Err(HealthConfigError(format!(
                "{at}.command: a script needs its program, then its arguments, e.g. command = [\"python\", \"checks/pii.py\"]; it is never run through a shell"
            )));
        }
        let timeout = match config.timeout.as_deref() {
            None => SCRIPT_TIMEOUT,
            Some(text) => {
                let timeout = humantime::parse_duration(text).map_err(|e| {
                    HealthConfigError(format!(
                        "{at}.timeout: `{text}` isn't a duration ({e}), e.g. \"30s\" or \"2m\""
                    ))
                })?;
                if timeout.is_zero() || timeout > LONGEST {
                    return Err(HealthConfigError(format!(
                        "{at}.timeout: `{text}` must be more than 0 and at most 1h"
                    )));
                }
                timeout
            }
        };
        let params = match &config.params {
            None => BTreeMap::new(),
            Some(table) => match serde_json::to_value(table) {
                Ok(serde_json::Value::Object(map)) => map.into_iter().collect(),
                _ => {
                    return Err(HealthConfigError(format!(
                        "{at}.params: a table of parameters, e.g. params = {{ min_rows = 10 }}"
                    )));
                }
            },
        };
        let digested = Digested {
            id: &config.id,
            kind: "script",
            command: &command,
            select: config.select.as_ref(),
            exclude: config.exclude.as_ref(),
        };
        let bytes =
            serde_json::to_vec(&digested).map_err(|e| HealthConfigError(format!("{at}: {e}")))?;
        Ok(Self {
            id: config.id.clone(),
            severity,
            select,
            exclude,
            command,
            params,
            timeout,
            digest: format!("sha256:{}", hex::encode(Sha256::digest(&bytes))),
            digested: bytes,
            trusted: false,
            runner: None,
        })
    }

    /// What a host runs, and a trust entry pins.
    pub(crate) fn definition(&self) -> ScriptDefinition {
        ScriptDefinition {
            id: self.id.clone(),
            command: self.command.clone(),
            params: self.params.clone(),
            timeout: self.timeout,
            digest: self.digest.clone(),
        }
    }

    /// Adds to its digest what decides what it runs and that a project can change
    /// (ADR-0030 §4d): `settings`, the host's (e.g. a configured working directory), and
    /// the content of each file its command names inside `root`, where it runs. So a
    /// commit that changes `checks/pii.py` makes `["python", "checks/pii.py"]`
    /// untrusted again. What those files load in turn isn't pinned.
    pub(crate) fn pin(&mut self, root: &Path, settings: &BTreeMap<String, String>) {
        let mut pins = settings.clone();
        if let Ok(inside) = std::fs::canonicalize(root) {
            for item in &self.command {
                let Ok(path) = std::fs::canonicalize(root.join(item)) else {
                    continue;
                };
                if !path.starts_with(&inside) || !path.is_file() {
                    continue;
                }
                // A file that can't be read is pinned as such: once readable, it is new.
                let content = std::fs::read(&path).map_or_else(
                    |_| "unreadable".to_owned(),
                    |bytes| format!("sha256:{}", hex::encode(Sha256::digest(&bytes))),
                );
                pins.insert(format!("file:{item}"), content);
            }
        }
        let mut bytes = self.digested.clone();
        if !pins.is_empty() {
            bytes.push(b'\n');
            bytes.extend(serde_json::to_vec(&pins).unwrap_or_default());
        }
        self.digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    }

    pub(crate) fn set_trusted(&mut self, trusted: bool) {
        self.trusted = trusted;
    }

    /// Whether it runs on `node`: selected (every node when it selects none), and not
    /// excluded.
    pub(crate) fn applies(&self, node: &NodeFacts) -> bool {
        self.select.as_ref().is_none_or(|s| s.matches(node))
            && !self.exclude.as_ref().is_some_and(|s| s.matches(node))
    }

    /// What it checks, for people.
    pub(crate) fn about(&self) -> String {
        format!("runs `{}`", self.command.join(" "))
    }

    /// It, as a check that ran; `None` when it is off.
    pub(crate) fn run(&self) -> Option<CheckRun> {
        Some(CheckRun {
            id: self.id.clone(),
            source: CheckSource::Script,
            severity: self.severity?,
            about: self.about(),
        })
    }

    /// Why it can't run here, when it can't: untrusted, or nothing to run it.
    pub(crate) fn held(&self) -> Option<String> {
        if !self.trusted {
            Some(format!(
                "{}: not trusted for this project; review it and run `ods health trust`",
                self.id
            ))
        } else if self.runner.is_none() {
            Some(format!(
                "{}: scripts run in `ods health check`, not here",
                self.id
            ))
        } else {
            None
        }
    }

    /// Its finding about `node` when it doesn't run: skipped when not selected, else
    /// unknown, saying `why`.
    pub(crate) fn finding(&self, severity: Severity, node: &NodeFacts, why: &str) -> Finding {
        let (status, reason) = if self.applies(node) {
            (Status::Unknown, why.to_owned())
        } else {
            (Status::Skipped, "not selected for this check".to_owned())
        };
        Finding {
            check: self.id.clone(),
            source: CheckSource::Script,
            status,
            severity,
            reason,
            evidence: BTreeMap::new(),
        }
    }
}
