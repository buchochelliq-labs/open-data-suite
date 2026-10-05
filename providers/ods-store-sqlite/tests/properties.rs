//! Property tests for the SQLite store (#192): any snapshot reads back exactly as it was
//! committed, whatever its names and values hold, and the head is always the last
//! snapshot committed.

use std::collections::BTreeMap;

use ods_core::state::{
    DataVersion, Exactness, Fingerprint, NodeState, SourceState, StateSnapshot, TargetIdentity,
    TestRecord, Timestamp,
};
use ods_sdk::contracts::state_store::{StateScope, StateStore};
use ods_store_sqlite::SqliteStateStore;
use proptest::prelude::*;

/// Any text, control characters and NUL included: names and values come from projects
/// and warehouses ODS doesn't control.
fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..10).prop_map(|c| c.into_iter().collect())
}

fn timestamp() -> impl Strategy<Value = Timestamp> {
    any::<i64>().prop_map(Timestamp::from_unix)
}

fn exactness() -> impl Strategy<Value = Exactness> {
    prop::sample::select(vec![
        Exactness::None,
        Exactness::Inferred,
        Exactness::Proxy,
        Exactness::Semantic,
        Exactness::Exact,
    ])
}

fn data_version() -> impl Strategy<Value = DataVersion> {
    (text(), exactness(), text()).prop_map(|(v, e, s)| DataVersion::new(v, e, s))
}

fn test_record() -> impl Strategy<Value = TestRecord> {
    (text(), timestamp(), prop::option::of(text())).prop_map(|(run, at, checks)| {
        let mut record = TestRecord::new(run, at, "");
        record.checks = checks;
        record
    })
}

fn node_state() -> impl Strategy<Value = NodeState> {
    (
        prop::collection::btree_map(text(), text(), 0..4),
        prop::collection::btree_map(text(), text(), 0..2),
        timestamp(),
        text(),
        prop::collection::btree_map(text(), prop::option::of(data_version()), 0..3),
        prop::collection::btree_map(text(), text(), 0..3),
        prop::option::of(test_record()),
        prop::option::of(any::<u64>()),
    )
        .prop_map(
            |(components, cosmetic, built_at, run, inputs, parents, tested, build_ms)| {
                let fingerprint = cosmetic
                    .into_iter()
                    .fold(Fingerprint::from_content(components), |f, (name, raw)| {
                        f.with_cosmetic(name, raw)
                    });
                let mut node = NodeState::new(fingerprint, built_at, run, inputs);
                node.parents = parents;
                node.tested = tested;
                node.build_ms = build_ms;
                node
            },
        )
}

fn target() -> impl Strategy<Value = TargetIdentity> {
    (
        text(),
        prop::option::of(text()),
        prop::option::of(text()),
        prop::option::of(text()),
        prop::option::of(text()),
        prop::option::of(text()),
    )
        .prop_map(|(name, profile, kind, location, digest, database)| {
            let mut target = TargetIdentity::new(name).profile(profile).kind(kind);
            target.location = location;
            target.location_digest = digest;
            target.database = database;
            target
        })
}

/// A snapshot's contents: everything but its parent, which the store's head decides.
#[derive(Debug, Clone)]
struct Contents {
    created_at: Timestamp,
    run: String,
    nodes: BTreeMap<String, NodeState>,
    target: Option<TargetIdentity>,
    sources: BTreeMap<String, SourceState>,
}

fn contents() -> impl Strategy<Value = Contents> {
    (
        timestamp(),
        text(),
        prop::collection::btree_map(text(), node_state(), 0..4),
        prop::option::of(target()),
        prop::collection::btree_map(
            text(),
            (
                prop::option::of(data_version()),
                prop::option::of(timestamp()),
                test_record(),
            )
                .prop_map(|(v, at, t)| SourceState::new(v, at, t)),
            0..3,
        ),
    )
        .prop_map(|(created_at, run, nodes, target, sources)| Contents {
            created_at,
            run,
            nodes,
            target,
            sources,
        })
}

impl Contents {
    fn snapshot(&self, parent: Option<ods_core::state::SnapshotId>) -> StateSnapshot {
        let mut snapshot =
            StateSnapshot::new(parent, self.created_at, &self.run, self.nodes.clone())
                .with_target(self.target.clone());
        snapshot.sources = self.sources.clone();
        snapshot
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn every_snapshot_reads_back_as_committed(chain in prop::collection::vec(contents(), 1..4)) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let store = SqliteStateStore::in_memory().await.unwrap();
            let scope = StateScope::new("project", "dev").unwrap();
            let mut committed = Vec::new();
            let mut head = None;
            for contents in &chain {
                let snapshot = contents.snapshot(head);
                let id = store.commit(&scope, &snapshot).await.unwrap();
                committed.push((id, snapshot));
                head = Some(id);
            }
            let latest = store.latest(&scope).await.unwrap().unwrap();
            prop_assert_eq!(Some(latest.id), head, "the head is the last commit");
            for (id, snapshot) in &committed {
                let stored = store.get(&scope, *id).await.unwrap().unwrap();
                prop_assert_eq!(&stored.snapshot, snapshot);
            }
            let history = store.history(&scope, committed.len() + 1).await.unwrap();
            prop_assert_eq!(history.len(), committed.len(), "no snapshot appears or goes");
            Ok(())
        })?;
    }
}
