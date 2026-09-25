//! dbt State configuration as neutral freshness policies (#168).
//!
//! Users configure dbt State in their projects as usual:
//! - `+state:` in `dbt_project.yml`;
//! - `config: state:` in properties YAML;
//! - `{{ config(state={...}) }}` in SQL;
//! - SAO's older `freshness: build_after:`.
//!
//! dbt resolves those into each node's config in `manifest.json` and in the v2 Information
//! Schema, and this module reads them from there. Key names and meanings follow dbt's
//! public documentation (`docs/reference/resource-configs/dbt-state-configs.md` and the
//! per-key pages).

use std::collections::BTreeMap;

use ods_core::freshness::{parse_duration, period_seconds};
use ods_core::{FreshnessPolicy, LoadedAt, PolicyOrigin, Quorum, UnappliedSetting};
use serde::Serialize;
use serde_json::Value;

use crate::artifacts::{DbtConfig, Manifest, ResourceType};

/// dbt State's documented defaults, used only when the project already relies on dbt
/// State: 45 minutes, and any parent.
const DBT_STATE_LAG_TOLERANCE_SECS: u64 = 45 * 60;

/// Freshness policies for every model and snapshot, and how each source reports new
/// data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct StatePolicies {
    /// Whether any node configures dbt State (`state:`) or SAO (`freshness.build_after`).
    /// If so, nodes without their own settings get dbt State's defaults; otherwise the
    /// conservative ones.
    pub uses_dbt_state: bool,
    /// Policies of models and snapshots, by `unique_id`.
    pub nodes: BTreeMap<String, FreshnessPolicy>,
    /// How each source reports new data, by `unique_id`.
    pub sources: BTreeMap<String, LoadedAt>,
}

/// Resolves the project's dbt State configuration.
pub fn resolve(manifest: &Manifest) -> StatePolicies {
    let scheduled = |t: ResourceType| matches!(t, ResourceType::Model | ResourceType::Snapshot);
    let uses_dbt_state = manifest
        .nodes
        .iter()
        .filter(|n| scheduled(n.resource_type))
        .any(|n| n.config.state.is_some() || build_after(&n.config).is_some());
    let nodes = manifest
        .nodes
        .iter()
        .filter(|n| scheduled(n.resource_type))
        .map(|n| (n.unique_id.clone(), policy(&n.config, uses_dbt_state)))
        .collect();
    let sources = manifest
        .nodes
        .iter()
        .filter(|n| n.resource_type == ResourceType::Source)
        .map(|n| (n.unique_id.clone(), loaded_at(&n.config)))
        .collect();
    StatePolicies {
        uses_dbt_state,
        nodes,
        sources,
    }
}

fn loaded_at(config: &DbtConfig) -> LoadedAt {
    let set = |v: &Option<String>| v.clone().filter(|s| !s.trim().is_empty());
    set(&config.loaded_at_query)
        .map(LoadedAt::Query)
        .or_else(|| set(&config.loaded_at_field).map(LoadedAt::Field))
        .unwrap_or(LoadedAt::WarehouseMetadata)
}

fn build_after(config: &DbtConfig) -> Option<&serde_json::Map<String, Value>> {
    config.freshness.as_ref()?.get("build_after")?.as_object()
}

/// Settings read so far for one node.
#[derive(Default)]
struct Found {
    lag: Option<u64>,
    quorum: Option<Quorum>,
    settings: Vec<String>,
    unapplied: Vec<UnappliedSetting>,
    unknown: Vec<String>,
}

impl Found {
    fn invalid(&mut self, setting: &str, value: &Value, why: &str) {
        // A value we can't read is as unknown as a key we don't know: never reuse.
        self.unknown.push(format!("{setting}: {value} ({why})"));
    }

    fn unapplied(&mut self, setting: &str, value: &Value, blocks_reuse: bool, reason: &str) {
        self.unapplied.push(UnappliedSetting::new(
            setting,
            value.to_string(),
            blocks_reuse,
            reason,
        ));
    }
}

fn policy(config: &DbtConfig, uses_dbt_state: bool) -> FreshnessPolicy {
    let mut found = Found::default();
    if let Some(state) = &config.state {
        read_state(state, &mut found);
    }
    // SAO's `build_after` is dbt State's fallback "until build_after is deprecated":
    // it fills only what `state` leaves unset.
    if let Some(after) = build_after(config) {
        if found.lag.is_none() {
            match count_period(after) {
                Some(Ok(secs)) => {
                    found.lag = Some(secs);
                    found.settings.push("freshness.build_after".into());
                }
                Some(Err(why)) => {
                    found.invalid("freshness.build_after", &Value::Object(after.clone()), &why);
                }
                None => {}
            }
        }
        if found.quorum.is_none()
            && let Some(value) = after.get("updates_on").filter(|v| !v.is_null())
        {
            match quorum(value) {
                Some(q) => {
                    found.quorum = Some(q);
                    found
                        .settings
                        .push("freshness.build_after.updates_on".into());
                }
                None => found.invalid(
                    "freshness.build_after.updates_on",
                    value,
                    "expected any or all",
                ),
            }
        }
    }

    let origin = if !found.settings.is_empty() {
        PolicyOrigin::Configured {
            settings: found.settings,
        }
    } else if uses_dbt_state {
        PolicyOrigin::FormatDefault {
            reason: "the project configures dbt State elsewhere, so dbt State's defaults apply"
                .into(),
        }
    } else {
        PolicyOrigin::ConservativeDefault
    };
    let mut policy = FreshnessPolicy::conservative();
    policy.lag_tolerance_secs = found.lag.unwrap_or(if uses_dbt_state {
        DBT_STATE_LAG_TOLERANCE_SECS
    } else {
        0
    });
    policy.require_fresh_data_from = found.quorum.unwrap_or(Quorum::Any);
    policy.origin = origin;
    policy.unapplied = found.unapplied;
    policy.unknown = found.unknown;
    policy
}

fn read_state(state: &Value, found: &mut Found) {
    let Some(state) = state.as_object() else {
        found.invalid("state", state, "expected a mapping");
        return;
    };
    for (key, value) in state {
        if value.is_null() {
            continue;
        }
        let setting = format!("state.{key}");
        match key.as_str() {
            "lag_tolerance" => match lag_tolerance(value) {
                Ok(secs) => {
                    found.lag = Some(secs);
                    found.settings.push(setting);
                }
                Err(why) => found.invalid(&setting, value, &why),
            },
            "require_fresh_data_from" => match quorum(value) {
                Some(q) => {
                    found.quorum = Some(q);
                    found.settings.push(setting);
                }
                None => found.invalid(&setting, value, "expected any or all"),
            },
            // Ignoring this only rebuilds more often than dbt State would.
            "compare_unrendered_code" => found.unapplied(
                &setting,
                value,
                false,
                "not supported yet: rebuilds whenever the rendered SQL changes",
            ),
            // Cloning isn't supported yet, so there is nothing to pre-clone.
            "pre_clone" => found.unapplied(&setting, value, false, "cloning isn't supported yet"),
            // With `true`, dbt State treats a change in a volatile function's value as a
            // change: without that, ODS could reuse what dbt State would rebuild.
            "evaluate_volatile_sql" => {
                if value.as_bool() != Some(false) {
                    found.unapplied(
                        &setting,
                        value,
                        true,
                        "not supported yet: the node always rebuilds",
                    );
                }
            }
            // With `true`, hooks run even when the node is reused: reusing without them
            // would skip work the user asked for.
            "execute_hooks_on_any_reuse" => {
                if value.as_bool() != Some(false) {
                    found.unapplied(
                        &setting,
                        value,
                        true,
                        "not supported yet: the node always rebuilds so its hooks run",
                    );
                }
            }
            _ => found.unknown.push(setting),
        }
    }
}

/// `"4h"` (dbt 1.x, as written) or `{count: 4, period: hour}` (dbt v2).
fn lag_tolerance(value: &Value) -> Result<u64, String> {
    match value {
        Value::String(text) => parse_duration(text),
        Value::Number(n) => n.as_u64().ok_or_else(|| format!("`{n}` is not a duration")),
        Value::Object(map) => count_period(map).unwrap_or_else(|| Err("no count or period".into())),
        other => Err(format!("`{other}` is not a duration")),
    }
}

/// `{count, period}`, or `None` if neither is set.
fn count_period(map: &serde_json::Map<String, Value>) -> Option<Result<u64, String>> {
    let count = map.get("count").filter(|v| !v.is_null());
    let period = map.get("period").filter(|v| !v.is_null());
    if count.is_none() && period.is_none() {
        return None;
    }
    Some((|| {
        let count = count
            .and_then(Value::as_u64)
            .ok_or("`count` must be a whole number")?;
        let period = period
            .and_then(Value::as_str)
            .ok_or("`period` must be minute, hour, day, …")?;
        let size = period_seconds(period).ok_or_else(|| format!("unknown period `{period}`"))?;
        count.checked_mul(size).ok_or_else(|| "too long".to_owned())
    })())
}

fn quorum(value: &Value) -> Option<Quorum> {
    match value.as_str()?.to_ascii_lowercase().as_str() {
        "any" => Some(Quorum::Any),
        "all" => Some(Quorum::All),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn config(state: Value, freshness: Value) -> DbtConfig {
        DbtConfig {
            state: (!state.is_null()).then_some(state),
            freshness: (!freshness.is_null()).then_some(freshness),
            ..DbtConfig::default()
        }
    }

    #[test]
    fn both_lag_tolerance_forms_read_the_same() {
        let v1 = policy(&config(json!({"lag_tolerance": "4h"}), Value::Null), true);
        let v2 = policy(
            &config(
                json!({"lag_tolerance": {"count": 4, "period": "hour", "updates_on": null}}),
                Value::Null,
            ),
            true,
        );
        assert_eq!(v1.lag_tolerance_secs, 14_400);
        assert_eq!(v1, v2);
    }

    #[test]
    fn state_wins_over_build_after_which_fills_the_gaps() {
        let p = policy(
            &config(
                json!({"lag_tolerance": "1d"}),
                json!({"build_after": {"count": 2, "period": "hour", "updates_on": "all"}}),
            ),
            true,
        );
        assert_eq!(p.lag_tolerance_secs, 86_400);
        assert_eq!(p.require_fresh_data_from, Quorum::All);
        assert_eq!(
            p.origin,
            PolicyOrigin::Configured {
                settings: vec![
                    "state.lag_tolerance".into(),
                    "freshness.build_after.updates_on".into()
                ]
            }
        );
    }

    #[test]
    fn defaults_depend_on_whether_the_project_uses_dbt_state() {
        let none = config(Value::Null, Value::Null);
        let with_state = policy(&none, true);
        assert_eq!(with_state.lag_tolerance_secs, 45 * 60);
        assert!(matches!(
            with_state.origin,
            PolicyOrigin::FormatDefault { .. }
        ));
        assert_eq!(policy(&none, false), FreshnessPolicy::conservative());
    }

    #[test]
    fn unsupported_and_unknown_settings_are_reported_and_may_block_reuse() {
        let p = policy(
            &config(
                json!({
                    "compare_unrendered_code": true,
                    "pre_clone": "never",
                    "evaluate_volatile_sql": false,
                    "brand_new_key": 1
                }),
                Value::Null,
            ),
            true,
        );
        assert_eq!(
            p.unapplied.len(),
            2,
            "false is the default, so nothing to apply"
        );
        assert!(p.unapplied.iter().all(|u| !u.blocks_reuse));
        assert_eq!(p.unknown, ["state.brand_new_key"]);
        assert!(!p.allows_reuse());

        let hooks = policy(
            &config(json!({"execute_hooks_on_any_reuse": true}), Value::Null),
            true,
        );
        assert!(!hooks.allows_reuse());
        let bad = policy(&config(json!({"lag_tolerance": "soon"}), Value::Null), true);
        assert!(!bad.allows_reuse());
        assert_eq!(bad.lag_tolerance_secs, 45 * 60, "falls back to the default");
    }

    #[test]
    fn sources_prefer_a_query_then_a_field_then_warehouse_metadata() {
        let mut c = DbtConfig::default();
        assert_eq!(loaded_at(&c), LoadedAt::WarehouseMetadata);
        c.loaded_at_field = Some("_loaded_at".into());
        assert_eq!(loaded_at(&c), LoadedAt::Field("_loaded_at".into()));
        c.loaded_at_query = Some("select max(ts) from x".into());
        assert_eq!(
            loaded_at(&c),
            LoadedAt::Query("select max(ts) from x".into())
        );
    }
}
