//! Coverage targets (ADR-0030 §3): `[health.coverage.<measure>]` sets the share of a
//! project that a measure must cover, e.g. 80% of models with tests. A host measures
//! the project; this judges each measure against its target. Coverage is about the
//! project as a whole, so its findings stand beside the nodes' badges, never in them.

use std::collections::BTreeMap;
use std::fmt;

use ods_config::{CoverageTargetConfig, HealthSeverity};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{HealthConfigError, Severity, Status};

/// The measures a target can be set for, with what each counts.
pub const COVERAGE_MEASURES: [(&str, &str); 4] = [
    ("tests", "models with tests"),
    ("descriptions", "models with descriptions"),
    ("constraints", "models with constraints"),
    ("source_freshness", "sources with freshness"),
];

/// A share from 0 to 1, kept to the thousandth so that findings compare exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Share(u16);

impl Share {
    /// The share, from 0 to 1.
    pub fn get(self) -> f64 {
        f64::from(self.0) / 1000.0
    }

    /// Whether `covered` of `total` reach it.
    fn reached_by(self, covered: usize, total: usize) -> bool {
        covered.saturating_mul(1000) >= usize::from(self.0).saturating_mul(total)
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked to be from 0 to 1 first, so the thousandths fit a u16"
    )]
    fn of(value: f64) -> Option<Self> {
        (value.is_finite() && (0.0..=1.0).contains(&value))
            .then(|| Self((value * 1000.0).round() as u16))
    }
}

impl fmt::Display for Share {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let permille = self.0;
        if permille.is_multiple_of(10) {
            write!(f, "{}%", permille / 10)
        } else {
            write!(f, "{}.{}%", permille / 10, permille % 10)
        }
    }
}

impl Serialize for Share {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.get())
    }
}

impl<'de> Deserialize<'de> for Share {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = f64::deserialize(deserializer)?;
        Self::of(value)
            .ok_or_else(|| serde::de::Error::custom(format!("{value} isn't between 0 and 1")))
    }
}

/// What a host measured: how many of how many a measure covers; `covered` is `None`
/// when it couldn't be measured.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Measured {
    /// The measure's key, one of [`COVERAGE_MEASURES`].
    pub key: String,
    /// How many are covered.
    pub covered: Option<usize>,
    /// Out of how many.
    pub total: usize,
}

impl Measured {
    /// `covered` of `total` for `key`.
    pub fn new(key: impl Into<String>, covered: Option<usize>, total: usize) -> Self {
        Self {
            key: key.into(),
            covered,
            total,
        }
    }
}

/// A coverage target's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CoverageFinding {
    /// The measure, e.g. `tests`.
    pub measure: String,
    /// `pass` when the target is reached, `fail` when it isn't, `unknown` when there
    /// was nothing to measure.
    pub status: Status,
    /// How severe missing the target is.
    pub severity: Severity,
    /// The share that must be covered.
    pub target: Share,
    /// How many are covered; `None` when not measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covered: Option<usize>,
    /// Out of how many.
    pub total: usize,
    /// Why, for people.
    pub reason: String,
}

/// A coverage target as configured.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    measure: &'static str,
    label: &'static str,
    share: Share,
    /// `None` when it is off.
    pub(crate) severity: Option<Severity>,
}

impl Target {
    /// The targets `config` sets, in [`COVERAGE_MEASURES`] order.
    pub(crate) fn from_config(
        config: &BTreeMap<String, CoverageTargetConfig>,
    ) -> Result<Vec<Self>, HealthConfigError> {
        if let Some(unknown) = config
            .keys()
            .find(|k| !COVERAGE_MEASURES.iter().any(|(m, _)| m == k))
        {
            let known: Vec<&str> = COVERAGE_MEASURES.iter().map(|(m, _)| *m).collect();
            return Err(HealthConfigError(format!(
                "health.coverage.{unknown}: there is no coverage measure `{unknown}`; the measures are {}",
                known.join(", ")
            )));
        }
        COVERAGE_MEASURES
            .iter()
            .filter_map(|&(measure, label)| Some((measure, label, config.get(measure)?)))
            .map(|(measure, label, own)| {
                let target = Share::of(own.target).ok_or_else(|| {
                    HealthConfigError(format!(
                        "health.coverage.{measure}.target: {} isn't a share between 0 and 1, e.g. 0.8",
                        own.target
                    ))
                })?;
                Ok(Self {
                    measure,
                    label,
                    share: target,
                    severity: match own.severity {
                        None | Some(HealthSeverity::Warn) => Some(Severity::Warn),
                        Some(HealthSeverity::Error) => Some(Severity::Error),
                        Some(HealthSeverity::Info) => Some(Severity::Info),
                        Some(HealthSeverity::Off) => None,
                    },
                })
            })
            .collect()
    }

    /// For people, e.g. `tests ≥ 80% (warn)`.
    pub(crate) fn describe(&self) -> String {
        format!(
            "{} ≥ {} ({})",
            self.measure,
            self.share,
            self.severity.map_or("off", crate::severity_word)
        )
    }

    /// Its verdict on what was measured; `None` when it is off. A measure the host
    /// didn't give, or couldn't measure, is unknown, never reached (AGENTS rule 3).
    pub(crate) fn judge(&self, measured: &[Measured]) -> Option<CoverageFinding> {
        let severity = self.severity?;
        let found = measured.iter().find(|m| m.key == self.measure);
        let total = found.map_or(0, |m| m.total);
        let covered = found.and_then(|m| m.covered).filter(|_| total > 0);
        let (status, reason) = match covered {
            None => (
                Status::Unknown,
                format!(
                    "{}: nothing to measure, so the {} target can't be judged",
                    self.label, self.share
                ),
            ),
            Some(covered) => {
                let share = covered * 100 / total;
                if self.share.reached_by(covered, total) {
                    (
                        Status::Pass,
                        format!(
                            "{covered} of {total} {} ({share}%), meeting the {} target",
                            self.label, self.share
                        ),
                    )
                } else {
                    (
                        Status::Fail,
                        format!(
                            "{covered} of {total} {} ({share}%), below the {} target",
                            self.label, self.share
                        ),
                    )
                }
            }
        };
        Some(CoverageFinding {
            measure: self.measure.to_owned(),
            status,
            severity,
            target: self.share,
            covered,
            total,
            reason,
        })
    }
}
