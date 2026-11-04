//! Conformance checks for metadata-only producing-code sources.
use crate::contracts::producing_code::{AssetKey, CodeGap, ProducingCodeSource};

/// Checks stable output, valid locations, and separation between project namespaces.
/// The supplied asset must have at least one mapping without a producing run.
/// # Panics
/// Panics when the provider violates the contract.
pub fn check(source: &dyn ProducingCodeSource, asset: &AssetKey) {
    let first = source
        .producing_code(asset, None)
        .expect("fixture resolves");
    assert!(!first.references.is_empty(), "fixture has a reference");
    assert_eq!(
        first,
        source.producing_code(asset, None).expect("repeat resolves")
    );
    for reference in first.references {
        reference.code.validate().expect("valid location");
    }
    let other = AssetKey::new(
        format!("{}-unmapped", asset.project),
        &asset.environment,
        &asset.node,
    );
    let absent = source
        .producing_code(&other, None)
        .expect("unmapped resolves");
    assert_eq!(absent.references, Vec::new());
    assert!(absent.gaps.contains(&CodeGap::Missing));
}
