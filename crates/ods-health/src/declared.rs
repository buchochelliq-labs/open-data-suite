//! Checks the project declares in `[[health.checks]]` (ADR-0030 §2, tier 2): rules over
//! the project's metadata, e.g. every mart has a description and a `unique` test. They
//! read only the facts a host already has, so they run in-process like the built-ins
//! and are never *unknown*: the project says what a node has.

use std::collections::{BTreeMap, BTreeSet};

use ods_config::{DeclaredCheckConfig, HealthSeverity};

use crate::{
    BUILTINS, CheckInfo, CheckRun, CheckSource, Finding, HealthConfigError, NodeFacts, Selector,
    Severity, Status,
};

/// What a declared check can require of a node.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Requirement {
    /// A description that isn't blank.
    Description,
    /// At least one test.
    Tests,
    /// A test of this type, e.g. `unique`.
    TestType(String),
    /// At least one constraint.
    Constraints,
    /// This tag.
    Tag(String),
}

/// The forms a requirement can take, for error messages.
const VOCABULARY: &str =
    "`description`, `tests`, `test:<type>` (e.g. `test:unique`), `constraints` and `tag:<tag>`";

impl Requirement {
    fn parse(item: &str, at: &str) -> Result<Self, HealthConfigError> {
        let named = |prefix: &str| {
            item.strip_prefix(prefix)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
        };
        match item.trim() {
            "description" => Ok(Self::Description),
            "tests" => Ok(Self::Tests),
            "constraints" => Ok(Self::Constraints),
            _ => {
                if let Some(test) = named("test:") {
                    Ok(Self::TestType(test))
                } else if let Some(tag) = named("tag:") {
                    Ok(Self::Tag(tag))
                } else {
                    Err(HealthConfigError(format!(
                        "{at}.require: `{item}` isn't something a check can require; it can require {VOCABULARY}"
                    )))
                }
            }
        }
    }

    /// As written in `require`.
    fn key(&self) -> String {
        match self {
            Self::Description => "description".to_owned(),
            Self::Tests => "tests".to_owned(),
            Self::TestType(test) => format!("test:{test}"),
            Self::Constraints => "constraints".to_owned(),
            Self::Tag(tag) => format!("tag:{tag}"),
        }
    }

    fn met(&self, node: &NodeFacts) -> bool {
        match self {
            Self::Description => node.described,
            Self::Tests => node.tests > 0,
            Self::TestType(test) => node.test_types.contains(test),
            Self::Constraints => node.constraints > 0,
            Self::Tag(tag) => node.tags.iter().any(|t| t == tag),
        }
    }

    /// What a node that doesn't meet it lacks, for people.
    fn lacking(&self) -> String {
        match self {
            Self::Description => "no description".to_owned(),
            Self::Tests => "no tests".to_owned(),
            Self::TestType(test) => format!("no `{test}` test"),
            Self::Constraints => "no constraints".to_owned(),
            Self::Tag(tag) => format!("no `{tag}` tag"),
        }
    }
}

/// A check declared in `[[health.checks]]`, ready to run.
#[derive(Debug, Clone)]
pub(crate) struct Declared {
    pub(crate) id: String,
    /// `None` when it is off.
    pub(crate) severity: Option<Severity>,
    select: Option<Selector>,
    exclude: Option<Selector>,
    require: Vec<Requirement>,
}

/// The checks `[[health.checks]]` declares: rules over the project's metadata, and
/// probes (ADR-0030 §4a, §4d).
pub(crate) struct Configured {
    pub(crate) declared: Vec<Declared>,
    pub(crate) probes: Vec<crate::probe::Probe>,
}

/// One declared check, of either kind.
enum One {
    Declared(Box<Declared>),
    Probe(Box<crate::probe::Probe>),
}

impl Declared {
    /// The checks `checks` declares. Ids must be valid, and no built-in's or other
    /// declared check's.
    pub(crate) fn from_config(
        checks: &[DeclaredCheckConfig],
        probes: Option<&ods_config::HealthProbesConfig>,
    ) -> Result<Configured, HealthConfigError> {
        let mut ids = BTreeSet::new();
        let all = checks
            .iter()
            .enumerate()
            .map(|(i, config)| {
                let at = format!("health.checks[{i}]");
                if !CheckInfo::valid_id(&config.id) {
                    return Err(HealthConfigError(format!(
                        "{at}.id: `{}` isn't a valid check id: lowercase letters, digits, `_`, `-` and `.`, starting with a letter",
                        config.id
                    )));
                }
                if BUILTINS.iter().any(|b| b.id() == config.id) {
                    return Err(HealthConfigError(format!(
                        "{at}.id: `{}` is a built-in check; tune it under [health.builtin.{}] instead",
                        config.id, config.id
                    )));
                }
                if !ids.insert(config.id.as_str()) {
                    return Err(HealthConfigError(format!(
                        "{at}.id: two checks in [[health.checks]] are called `{}`",
                        config.id
                    )));
                }
                let severity = match config.severity {
                    None | Some(HealthSeverity::Warn) => Some(Severity::Warn),
                    Some(HealthSeverity::Error) => Some(Severity::Error),
                    Some(HealthSeverity::Info) => Some(Severity::Info),
                    Some(HealthSeverity::Off) => None,
                };
                let selector = |which: &str, s: Option<&ods_config::HealthSelector>| {
                    s.map(|s| Selector::new(s, &format!("{at}.{which}")))
                        .transpose()
                };
                let select = selector("select", config.select.as_ref())?;
                let exclude = selector("exclude", config.exclude.as_ref())?;
                match config.kind.as_deref() {
                    None | Some("declarative") => {}
                    Some("probe") => {
                        return crate::probe::Probe::from_config(
                            config, &at, severity, select, exclude, probes,
                        )
                            .map(|p| One::Probe(Box::new(p)));
                    }
                    Some("script") => {
                        return Err(HealthConfigError(format!(
                            "{at}.kind: `script` checks aren't supported yet (#392); `declarative` and `probe` are"
                        )));
                    }
                    Some(kind) => {
                        return Err(HealthConfigError(format!(
                            "{at}.kind: `{kind}` isn't a kind of check; the kinds are `declarative` and `probe`"
                        )));
                    }
                }
                for (field, set) in [("sql", config.sql.is_some()), ("pass", config.pass.is_some())] {
                    if set {
                        return Err(HealthConfigError(format!(
                            "{at}.{field}: only a `kind = \"probe\"` check has `{field}`"
                        )));
                    }
                }
                if config.require.is_empty() {
                    return Err(HealthConfigError(format!(
                        "{at}.require: say what every selected node must have: {VOCABULARY}"
                    )));
                }
                let require = config
                    .require
                    .iter()
                    .map(|item| Requirement::parse(item, &at))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(One::Declared(Box::new(Self {
                    id: config.id.clone(),
                    severity,
                    select,
                    exclude,
                    require,
                })))
            })
            .collect::<Result<Vec<One>, HealthConfigError>>()?;
        let mut configured = Configured {
            declared: Vec::new(),
            probes: Vec::new(),
        };
        for one in all {
            match one {
                One::Declared(d) => configured.declared.push(*d),
                One::Probe(p) => configured.probes.push(*p),
            }
        }
        Ok(configured)
    }

    /// What it checks, for people.
    pub(crate) fn about(&self) -> String {
        let keys: Vec<String> = self.require.iter().map(Requirement::key).collect();
        format!("requires {}", keys.join(", "))
    }

    /// It, as a check that ran; `None` when it is off.
    pub(crate) fn run(&self) -> Option<CheckRun> {
        Some(CheckRun {
            id: self.id.clone(),
            source: CheckSource::Declarative,
            severity: self.severity?,
            about: self.about(),
        })
    }

    fn applies(&self, node: &NodeFacts) -> bool {
        self.select.as_ref().is_none_or(|s| s.matches(node))
            && !self.exclude.as_ref().is_some_and(|s| s.matches(node))
    }

    /// Its finding about `node`; `None` when it is off.
    pub(crate) fn finding(&self, node: &NodeFacts) -> Option<Finding> {
        let severity = self.severity?;
        let required: Vec<String> = self.require.iter().map(Requirement::key).collect();
        let mut evidence = BTreeMap::from([("require".to_owned(), required.join(", "))]);
        let (status, reason) = if self.applies(node) {
            let lacking: Vec<&Requirement> = self.require.iter().filter(|r| !r.met(node)).collect();
            if lacking.is_empty() {
                (
                    Status::Pass,
                    format!("{}: has {}", self.id, required.join(", ")),
                )
            } else {
                let keys: Vec<String> = lacking.iter().map(|r| r.key()).collect();
                evidence.insert("missing".to_owned(), keys.join(", "));
                let why: Vec<String> = lacking.iter().map(|r| r.lacking()).collect();
                (Status::Fail, format!("{}: {}", self.id, why.join("; ")))
            }
        } else {
            evidence.clear();
            (Status::Skipped, "not selected for this check".to_owned())
        };
        Some(Finding {
            check: self.id.clone(),
            source: CheckSource::Declarative,
            status,
            severity,
            reason,
            evidence,
        })
    }
}
