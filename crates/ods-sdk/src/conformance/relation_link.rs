//! Conformance suite for [`RelationLinker`].

use std::sync::Arc;

use ods_core::Capability;

use super::Report;
use crate::contracts::relation_link::{NoRelationLink, RelationLinker};

/// What the suite needs from a linker under test.
pub trait RelationLinkHarness: Send + Sync {
    /// A linker with everything it needs configured.
    fn linker(&self) -> Arc<dyn RelationLinker>;

    /// A linker missing the setting links are built from, or `None` if the provider
    /// needs none, which skips the case that needs it.
    fn unconfigured(&self) -> Option<Arc<dyn RelationLinker>>;

    /// A fully qualified relation, quoted as the warehouse quotes identifiers.
    fn qualified(&self) -> String;

    /// The same relation with its first part left out.
    fn under_qualified(&self) -> String;

    /// A fully qualified relation whose names hold a space, `?`, `#`, `/` and `%`.
    fn awkward(&self) -> String;
}

/// The URL's scheme and host, and the rest (path).
fn split(url: &str) -> (&str, &str) {
    let rest = url.strip_prefix("https://").unwrap_or_else(|| {
        panic!("a link must be https: {url}");
    });
    rest.split_once('/').unwrap_or((rest, ""))
}

fn advertises_the_capability(harness: &dyn RelationLinkHarness) {
    let info = harness.linker().info();
    assert!(
        info.capabilities.contains(&Capability::RelationLink),
        "advertises_the_capability: {info:?}"
    );
}

fn links_a_qualified_relation(harness: &dyn RelationLinkHarness) {
    let case = "links_a_qualified_relation";
    let linker = harness.linker();
    let link = linker
        .link(&harness.qualified())
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    let (host, path) = split(&link.url);
    assert!(!host.is_empty(), "{case}: no host in {}", link.url);
    assert!(!host.contains('@'), "{case}: a user part in {}", link.url);
    assert!(
        !link.url.contains(['?', '#']),
        "{case}: a query or fragment in {}",
        link.url
    );
    assert!(!path.is_empty(), "{case}: no path in {}", link.url);
    assert!(!link.url.ends_with('/'), "{case}: {}", link.url);
    assert!(!link.label.trim().is_empty(), "{case}: no label");
    // Deterministic.
    assert_eq!(
        linker.link(&harness.qualified()),
        Ok(link),
        "{case}: not deterministic"
    );
}

fn never_guesses_a_missing_part(harness: &dyn RelationLinkHarness) {
    let case = "never_guesses_a_missing_part";
    let linker = harness.linker();
    let got = linker.link(&harness.under_qualified());
    assert!(
        matches!(got, Err(NoRelationLink::NotQualified { .. })),
        "{case}: {got:?}"
    );
    for empty in ["", "   "] {
        assert!(linker.link(empty).is_err(), "{case}: `{empty}` linked");
    }
}

fn encodes_every_name(harness: &dyn RelationLinkHarness) {
    let case = "encodes_every_name";
    let link = harness
        .linker()
        .link(&harness.awkward())
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    let (_, path) = split(&link.url);
    assert!(
        !path.contains([' ', '?', '#']),
        "{case}: unencoded in {}",
        link.url
    );
    // Each name stays one segment, so a `/` in a name is encoded.
    let qualified = harness
        .linker()
        .link(&harness.qualified())
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    let plain = split(&qualified.url).1.split('/').count();
    assert_eq!(path.split('/').count(), plain, "{case}: {}", link.url);
}

fn says_what_is_not_configured(harness: &dyn RelationLinkHarness) -> bool {
    let case = "says_what_is_not_configured";
    let Some(linker) = harness.unconfigured() else {
        return false;
    };
    let got = linker.link(&harness.qualified());
    assert!(
        matches!(got, Err(NoRelationLink::NotConfigured { .. })),
        "{case}: {got:?}"
    );
    true
}

/// Runs every case against `harness`.
pub fn run(harness: &dyn RelationLinkHarness) -> Report {
    let mut report = Report::default();
    advertises_the_capability(harness);
    report.passed.push("advertises_the_capability");
    links_a_qualified_relation(harness);
    report.passed.push("links_a_qualified_relation");
    never_guesses_a_missing_part(harness);
    report.passed.push("never_guesses_a_missing_part");
    encodes_every_name(harness);
    report.passed.push("encodes_every_name");
    if says_what_is_not_configured(harness) {
        report.passed.push("says_what_is_not_configured");
    } else {
        report.skipped.push((
            "says_what_is_not_configured",
            "the provider needs no setting".to_owned(),
        ));
    }
    report
}
