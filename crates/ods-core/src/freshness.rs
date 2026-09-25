//! Freshness policies: when new upstream data makes a node due for a rebuild (#19, #168).
//!
//! These are the neutral form of whatever the project format configures, e.g. dbt State's
//! `state:` block. The State planner only ever sees these types (AGENTS.md rule 1).

use serde::{Deserialize, Serialize};

/// How many direct parents must have new data for a node to be due.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Quorum {
    /// Any parent with new data (the conservative default).
    Any,
    /// Only when every parent has new data.
    All,
}

/// Where a policy came from, for `explain` (AGENTS.md rule 4).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
#[non_exhaustive]
pub enum PolicyOrigin {
    /// Set on the node, e.g. `state.lag_tolerance` in the project's configuration.
    Configured {
        /// The setting(s) it came from, e.g. `state.lag_tolerance`.
        settings: Vec<String>,
    },
    /// Not set on the node; the project uses the format's own defaults because it
    /// already relies on them (e.g. other nodes configure dbt State).
    FormatDefault {
        /// Why, e.g. `project configures dbt State`.
        reason: String,
    },
    /// Not set, and nothing says the project expects anything else: rebuild whenever
    /// any parent has new data (AGENTS.md rule 3).
    ConservativeDefault,
}

/// A setting ODS understands but doesn't apply yet, and what that means for the node.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct UnappliedSetting {
    /// The setting, e.g. `state.evaluate_volatile_sql`.
    pub setting: String,
    /// Its value, as configured.
    pub value: String,
    /// Whether ignoring it could make ODS reuse something the user expects rebuilt. If
    /// so, the node is never reused until ODS supports the setting.
    pub blocks_reuse: bool,
    /// Why.
    pub reason: String,
}

impl UnappliedSetting {
    /// A setting ODS understands but doesn't apply yet.
    pub fn new(
        setting: impl Into<String>,
        value: impl Into<String>,
        blocks_reuse: bool,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            setting: setting.into(),
            value: value.into(),
            blocks_reuse,
            reason: reason.into(),
        }
    }
}

/// When a node is due because of new upstream data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct FreshnessPolicy {
    /// New upstream data only makes the node due once this many seconds have passed
    /// since its last build. `0` means rebuild on any new data.
    pub lag_tolerance_secs: u64,
    /// Which parents must have new data.
    pub require_fresh_data_from: Quorum,
    /// Where the values came from.
    pub origin: PolicyOrigin,
    /// Settings understood but not applied yet.
    pub unapplied: Vec<UnappliedSetting>,
    /// Settings not understood at all. Any makes the node ineligible for reuse.
    pub unknown: Vec<String>,
}

impl FreshnessPolicy {
    /// The conservative default: rebuild on any new data from any parent.
    pub fn conservative() -> Self {
        Self {
            lag_tolerance_secs: 0,
            require_fresh_data_from: Quorum::Any,
            origin: PolicyOrigin::ConservativeDefault,
            unapplied: Vec::new(),
            unknown: Vec::new(),
        }
    }

    /// Whether the planner may reuse the node at all under this policy: not when a
    /// setting it can't honour could change the outcome (AGENTS.md rule 3).
    pub fn allows_reuse(&self) -> bool {
        self.unknown.is_empty() && !self.unapplied.iter().any(|s| s.blocks_reuse)
    }
}

/// How to tell that a source has new data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
#[non_exhaustive]
pub enum LoadedAt {
    /// The maximum of this column or expression.
    Field(String),
    /// This query, which returns one timestamp.
    Query(String),
    /// The warehouse's own last-modified metadata.
    WarehouseMetadata,
}

/// Parses a duration such as `45m`, `4h`, `1d`, `2 hours`, `1h30m` or `90` (seconds).
///
/// # Errors
/// Returns the offending text if it isn't a duration.
pub fn parse_duration(text: &str) -> Result<u64, String> {
    let bad = || format!("`{text}` is not a duration (e.g. 45m, 4h, 1d, 2 hours)");
    let mut rest = text.trim();
    if rest.is_empty() {
        return Err(bad());
    }
    let mut total: u64 = 0;
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits == 0 {
            return Err(bad());
        }
        let count: u64 = rest[..digits].parse().map_err(|_| bad())?;
        rest = rest[digits..].trim_start();
        let unit_len = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let unit = rest[..unit_len].to_ascii_lowercase();
        rest = rest[unit_len..].trim_start();
        let seconds = if unit.is_empty() {
            1
        } else {
            period_seconds(&unit).ok_or_else(bad)?
        };
        total = count
            .checked_mul(seconds)
            .and_then(|s| total.checked_add(s))
            .ok_or_else(bad)?;
    }
    Ok(total)
}

/// Seconds in one `period`: `s`, `m`, `h`, `d`, `w` and their long forms, singular or
/// plural (`minute`, `hours`, …).
pub fn period_seconds(period: &str) -> Option<u64> {
    Some(match period.to_ascii_lowercase().as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3_600,
        "d" | "day" | "days" => 86_400,
        "w" | "week" | "weeks" => 604_800,
        _ => return None,
    })
}

/// A duration as text, largest units first: `45m`, `4h`, `1d2h`, `0s`.
pub fn format_duration(seconds: u64) -> String {
    if seconds == 0 {
        return "0s".to_owned();
    }
    let mut rest = seconds;
    let mut out = String::new();
    for (unit, size) in [
        ("w", 604_800),
        ("d", 86_400),
        ("h", 3_600),
        ("m", 60),
        ("s", 1),
    ] {
        if rest >= size {
            out.push_str(&(rest / size).to_string());
            out.push_str(unit);
            rest %= size;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_parse_in_short_long_and_compound_forms() {
        assert_eq!(parse_duration("45m"), Ok(2_700));
        assert_eq!(parse_duration("4h"), Ok(14_400));
        assert_eq!(parse_duration("1d"), Ok(86_400));
        assert_eq!(parse_duration("2 hours"), Ok(7_200));
        assert_eq!(parse_duration("1h30m"), Ok(5_400));
        assert_eq!(parse_duration("90"), Ok(90));
        assert_eq!(parse_duration("1W"), Ok(604_800));
        for bad in ["", "h", "4 fortnights", "-1h", "1.5h"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn durations_format_largest_units_first() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(2_700), "45m");
        assert_eq!(format_duration(93_600), "1d2h");
    }

    #[test]
    fn unknown_or_blocking_settings_forbid_reuse() {
        let mut policy = FreshnessPolicy::conservative();
        assert!(policy.allows_reuse());
        policy.unapplied.push(UnappliedSetting {
            setting: "state.pre_clone".into(),
            value: "never".into(),
            blocks_reuse: false,
            reason: "cloning isn't supported yet".into(),
        });
        assert!(policy.allows_reuse());
        policy.unknown.push("state.brand_new".into());
        assert!(!policy.allows_reuse());
    }
}
