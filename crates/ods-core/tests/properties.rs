//! Property tests for `ods-core`'s pure logic (#192): the dependency order, cycle
//! reports, fingerprints, timestamps, strategy choice and redaction hold for any input,
//! not just the examples in the unit tests.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use ods_core::graph::{Cycle, layers};
use ods_core::redact;
use ods_core::state::{Fingerprint, Timestamp, TimestampMs};
use ods_core::{Capability, CapabilitySet, choose};
use proptest::prelude::*;

/// Up to 12 ids, `n00`…`n11`, so ids sort the same as their index.
fn ids(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("n{i:02}")).collect()
}

/// A graph over `n` ids with any edges, including self-loops and cycles.
fn any_graph() -> impl Strategy<Value = (usize, Vec<(usize, usize)>)> {
    (1_usize..12).prop_flat_map(|n| (Just(n), prop::collection::vec((0..n, 0..n), 0..30)))
}

/// A graph over `n` ids whose edges all go from a lower index to a higher one: acyclic.
fn acyclic_graph() -> impl Strategy<Value = (usize, Vec<(usize, usize)>)> {
    any_graph().prop_map(|(n, edges)| {
        let edges = edges
            .into_iter()
            .filter(|(a, b)| a != b)
            .map(|(a, b)| (a.min(b), a.max(b)))
            .collect();
        (n, edges)
    })
}

fn named<'a>(names: &'a [String], edges: &[(usize, usize)]) -> Vec<(&'a str, &'a str)> {
    edges
        .iter()
        .map(|(a, b)| (names[*a].as_str(), names[*b].as_str()))
        .collect()
}

/// The fewest edges from `from` back to itself, by breadth-first search.
fn shortest_cycle_through(from: usize, edges: &BTreeSet<(usize, usize)>) -> Option<usize> {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([(from, 0_usize)]);
    while let Some((node, len)) = queue.pop_front() {
        for (_, next) in edges.range((node, 0)..=(node, usize::MAX)) {
            if *next == from {
                return Some(len + 1);
            }
            if seen.insert(*next) {
                queue.push_back((*next, len + 1));
            }
        }
    }
    None
}

proptest! {
    #[test]
    fn layers_put_every_id_after_its_dependencies((n, edges) in acyclic_graph()) {
        let names = ids(n);
        let order = layers(names.iter().map(String::as_str), named(&names, &edges)).unwrap();
        let depth: BTreeMap<&str, u32> = order.iter().copied().collect();
        prop_assert_eq!(depth.len(), n, "every id once: {:?}", order);
        for (a, b) in &edges {
            prop_assert!(depth[names[*a].as_str()] < depth[names[*b].as_str()]);
        }
        for (id, d) in &order {
            // Depth is one more than the deepest dependency, or 0 with none.
            let deepest = edges
                .iter()
                .filter(|(_, b)| names[*b] == *id)
                .map(|(a, _)| depth[names[*a].as_str()] + 1)
                .max()
                .unwrap_or(0);
            prop_assert_eq!(*d, deepest, "{}", id);
        }
        let mut sorted = order.clone();
        sorted.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));
        prop_assert_eq!(order, sorted);
    }

    #[test]
    fn layers_dont_depend_on_input_order((n, mut edges) in any_graph()) {
        let names = ids(n);
        let forward = layers(names.iter().map(String::as_str), named(&names, &edges));
        edges.reverse();
        let backward = layers(names.iter().rev().map(String::as_str), named(&names, &edges));
        prop_assert_eq!(forward, backward);
    }

    #[test]
    fn a_reported_cycle_is_a_shortest_one_through_the_smallest_id_on_any((n, edges) in any_graph()) {
        let names = ids(n);
        let edge_set: BTreeSet<(usize, usize)> = edges.iter().copied().collect();
        let on_a_cycle: Vec<usize> =
            (0..n).filter(|i| shortest_cycle_through(*i, &edge_set).is_some()).collect();
        match layers(names.iter().map(String::as_str), named(&names, &edges)) {
            Ok(_) => prop_assert!(on_a_cycle.is_empty(), "a cycle goes unreported"),
            Err(Cycle(cycle)) => {
                let index: Vec<usize> =
                    cycle.iter().map(|id| names.iter().position(|n| n == id).unwrap()).collect();
                let smallest = on_a_cycle[0];
                prop_assert_eq!(index[0], smallest, "starts at the smallest id on a cycle");
                prop_assert_eq!(
                    Some(index.len()),
                    shortest_cycle_through(smallest, &edge_set),
                    "is a shortest cycle"
                );
                let distinct: BTreeSet<usize> = index.iter().copied().collect();
                prop_assert_eq!(distinct.len(), index.len(), "visits each id once");
                for (i, from) in index.iter().enumerate() {
                    let to = index[(i + 1) % index.len()];
                    prop_assert!(edge_set.contains(&(*from, to)), "{} → {} is an edge", from, to);
                }
            }
        }
    }

    #[test]
    fn a_fingerprint_is_its_components_in_any_order(
        components in prop::collection::btree_map("[a-z_]{1,8}", prop::collection::vec(any::<u8>(), 0..16), 0..6),
    ) {
        let forward = Fingerprint::from_content(components.clone());
        let backward = Fingerprint::from_content(components.into_iter().rev());
        prop_assert!(forward.diff(&backward).is_empty());
        prop_assert_eq!(forward, backward);
    }

    #[test]
    fn fingerprints_differ_exactly_when_a_component_does(
        before in prop::collection::btree_map("[a-z_]{1,6}", "[a-z]{0,4}", 0..6),
        after in prop::collection::btree_map("[a-z_]{1,6}", "[a-z]{0,4}", 0..6),
    ) {
        let a = Fingerprint::from_content(before.clone());
        let b = Fingerprint::from_content(after.clone());
        let diff = b.diff(&a);
        prop_assert_eq!(a.digest == b.digest, before == after);
        prop_assert_eq!(diff.is_empty(), before == after);
        for name in diff.all() {
            prop_assert_ne!(before.get(&name), after.get(&name));
        }
        // Reading the diff the other way round swaps added and removed.
        let back = a.diff(&b);
        prop_assert_eq!(&back.added, &diff.removed);
        prop_assert_eq!(&back.removed, &diff.added);
        prop_assert_eq!(&back.changed, &diff.changed);
    }

    #[test]
    fn a_timestamp_reads_back_as_itself(seconds in any::<i64>()) {
        let at = Timestamp::from_unix(seconds);
        prop_assert_eq!(Timestamp::parse(&at.to_string()), Ok(at));
        let json = serde_json::to_string(&at).unwrap();
        prop_assert_eq!(serde_json::from_str::<Timestamp>(&json).unwrap(), at);
    }

    #[test]
    fn a_millisecond_timestamp_reads_back_as_itself(millis in any::<i64>()) {
        let at = TimestampMs::from_unix_millis(millis);
        prop_assert_eq!(TimestampMs::parse(&at.to_string()), Ok(at));
    }

    #[test]
    fn the_first_strategy_whose_needs_are_met_is_chosen(
        offered in prop::collection::btree_set(0..Capability::WELL_KNOWN.len(), 0..6),
        needs in prop::collection::vec(prop::collection::btree_set(0..Capability::WELL_KNOWN.len(), 1..4), 0..6),
    ) {
        const IDS: [&str; 6] = ["s0", "s1", "s2", "s3", "s4", "s5"];
        let set = |i: &BTreeSet<usize>| -> CapabilitySet {
            i.iter().map(|i| Capability::WELL_KNOWN[*i].clone()).collect()
        };
        let offered = set(&offered);
        let mut strategies: Vec<ods_core::Strategy<usize>> = needs
            .iter()
            .enumerate()
            .map(|(i, need)| ods_core::Strategy::new(IDS[i], set(need), i))
            .collect();
        strategies.push(ods_core::Strategy::fallback("fallback", needs.len()));
        let choice = choose(&offered, &strategies).unwrap();
        prop_assert!(offered.satisfies(&choice.chosen.requires));
        prop_assert_eq!(choice.skipped.len(), choice.chosen.value, "skips exactly those before it");
        for skipped in &choice.skipped {
            prop_assert!(!skipped.missing.is_empty());
            prop_assert!(skipped.missing.iter().all(|c| !offered.contains(c)));
        }
    }

    #[test]
    fn a_quoted_value_never_survives_a_summary(
        before in "[a-zA-Z :,.]{0,20}",
        secret in "[a-zA-Z0-9]{12,20}",
        quote in prop::sample::select(vec!['\'', '"', '`']),
        after in "[a-zA-Z :,.]{0,20}",
    ) {
        let message = format!("{before}{quote}{secret}{quote}{after}");
        if let Some(line) = redact::summary_line(&message, 200) {
            prop_assert!(!line.contains(&secret), "{:?} kept the secret", line);
        }
        prop_assert!(!redact::literals(&message).contains(&secret));
    }

    #[test]
    fn a_quoted_value_never_survives_after_any_word(
        word in "[a-zA-Z]{1,3}",
        joiner in prop::sample::select(vec!["'", "'s ", "'s value ", "' ", ""]),
        filler in "[a-z ]{0,6}",
        lead in "[ _.,:]{0,2}",
        secret in "[A-Z0-9]{10,14}",
        tail in "[ a-z]{0,6}",
    ) {
        let message = format!("column {word}{joiner}{filler} '{lead}{secret}'{tail}");
        prop_assert!(!redact::literals(&message).contains(&secret), "{:?}", message);
        if let Some(line) = redact::summary_line(&message, 200) {
            prop_assert!(!line.contains(&secret), "{:?} => {:?}", message, line);
        }
    }

    #[test]
    fn a_summary_is_one_short_plain_line(text in any::<String>(), max in 1_usize..80) {
        if let Some(line) = redact::summary_line(&text, max) {
            prop_assert!(line.chars().count() <= max, "{:?} is longer than {}", line, max);
            prop_assert!(!line.chars().any(char::is_control), "{:?} has a control character", line);
        }
        if let Some(line) = redact::value_line(&text, max) {
            prop_assert!(line.chars().count() <= max);
            prop_assert!(!line.chars().any(char::is_control));
        }
    }
}
