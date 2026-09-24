//! Choosing a strategy from provider capabilities (#3, ADR-0006 §3).
//!
//! A planner lists strategies in order of preference, each with the capabilities it
//! needs, ending with a fallback that needs none (AGENTS.md rule 3: when a better
//! strategy can't be proven possible, use the conservative one). The choice records why
//! each preferred strategy was skipped, so plans stay explainable (rule 4).

use serde::Serialize;

use crate::capability::{Capability, CapabilitySet};

/// A way of doing something, and what it needs from the provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Strategy<T> {
    /// Stable identifier, e.g. `clone`.
    pub id: &'static str,
    /// Capabilities the provider must advertise.
    pub requires: CapabilitySet,
    /// What the planner does if this strategy is chosen.
    pub value: T,
}

/// A strategy that was passed over, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Skipped {
    /// The strategy's identifier.
    pub id: &'static str,
    /// Capabilities the provider lacked.
    pub missing: Vec<Capability>,
}

/// The outcome of [`choose`].
#[derive(Debug, PartialEq, Eq)]
pub struct Choice<'a, T> {
    /// The chosen strategy.
    pub chosen: &'a Strategy<T>,
    /// Preferred strategies that were skipped, in preference order.
    pub skipped: Vec<Skipped>,
}

/// Why no strategy could be chosen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChoiceError {
    /// The list was empty.
    #[error("no strategies were offered")]
    Empty,
    /// The last strategy is not an unconditional fallback.
    #[error(
        "the last strategy `{0}` requires capabilities; the list must end with a fallback that requires none"
    )]
    NoFallback(&'static str),
}

/// Picks the first strategy (in preference order) whose requirements `offered` meets.
///
/// # Errors
/// Returns [`ChoiceError`] if `strategies` is empty or does not end with a fallback that
/// requires no capabilities, so a choice is always possible.
pub fn choose<'a, T>(
    offered: &CapabilitySet,
    strategies: &'a [Strategy<T>],
) -> Result<Choice<'a, T>, ChoiceError> {
    let last = strategies.last().ok_or(ChoiceError::Empty)?;
    if !last.requires.is_empty() {
        return Err(ChoiceError::NoFallback(last.id));
    }
    let mut skipped = Vec::new();
    for strategy in strategies {
        let missing = offered.missing(&strategy.requires);
        if missing.is_empty() {
            return Ok(Choice {
                chosen: strategy,
                skipped,
            });
        }
        skipped.push(Skipped {
            id: strategy.id,
            missing: missing.into_iter().cloned().collect(),
        });
    }
    unreachable!("the fallback requires nothing, so it always matches")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    enum Reuse {
        Clone,
        Defer,
        Build,
    }

    fn strategies() -> Vec<Strategy<Reuse>> {
        vec![
            Strategy {
                id: "clone",
                requires: [Capability::ZeroCopyClone].into(),
                value: Reuse::Clone,
            },
            Strategy {
                id: "defer",
                requires: [Capability::RelationVersions].into(),
                value: Reuse::Defer,
            },
            Strategy {
                id: "build",
                requires: CapabilitySet::new(),
                value: Reuse::Build,
            },
        ]
    }

    #[test]
    fn picks_the_most_preferred_supported_strategy() {
        let all = strategies();
        let offered =
            CapabilitySet::from([Capability::ZeroCopyClone, Capability::RelationVersions]);
        let choice = choose(&offered, &all).unwrap();
        assert_eq!(choice.chosen.value, Reuse::Clone);
        assert!(choice.skipped.is_empty());
    }

    #[test]
    fn records_why_preferred_strategies_were_skipped() {
        let all = strategies();
        let offered = CapabilitySet::from([Capability::RelationVersions]);
        let choice = choose(&offered, &all).unwrap();
        assert_eq!(choice.chosen.value, Reuse::Defer);
        assert_eq!(
            choice.skipped,
            [Skipped {
                id: "clone",
                missing: vec![Capability::ZeroCopyClone]
            }]
        );
    }

    #[test]
    fn falls_back_conservatively_when_nothing_is_supported() {
        let all = strategies();
        let choice = choose(&CapabilitySet::new(), &all).unwrap();
        assert_eq!(choice.chosen.value, Reuse::Build);
        assert_eq!(choice.skipped.len(), 2);
    }

    #[test]
    fn requires_a_fallback() {
        let mut no_fallback = strategies();
        no_fallback.pop();
        assert_eq!(
            choose(&CapabilitySet::new(), &no_fallback),
            Err(ChoiceError::NoFallback("defer"))
        );
        assert_eq!(
            choose::<Reuse>(&CapabilitySet::new(), &[]),
            Err(ChoiceError::Empty)
        );
    }
}
