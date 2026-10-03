//! A scripted [`ErrorCatalogue`].

use ods_core::failure::{ErrorCategory, Suggestion, Symptom, Text};
use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::error_catalogue::{
    CatalogueInfo, Classification, ErrorCatalogue, PatternMatch,
};
use ods_sdk::contracts::run_events::ErrorSummary;
use ods_sdk::{Provider, ProviderInfo};

use crate::KIND;

/// A catalogue of a few fake patterns, for tests of what hosts do with a
/// classification. A message recognised by `(phrase, symptom)` starts with the
/// phrase, in any case; the kind `Query Error` is a database error and `Build Error`
/// a compilation error. A failed test can be run again with `--keep-failing-rows`.
#[derive(Debug, Clone)]
pub struct FakeErrorCatalogue {
    patterns: Vec<(&'static str, Symptom)>,
}

impl Default for FakeErrorCatalogue {
    fn default() -> Self {
        Self {
            patterns: vec![
                ("no such column", Symptom::MissingColumn),
                ("no such table", Symptom::MissingRelation),
                ("no such macro", Symptom::UnknownMacro),
                ("no such node", Symptom::MissingRef),
                ("not allowed", Symptom::PermissionDenied),
                ("took too long", Symptom::QueryTimeout),
                ("rows failed the test", Symptom::TestFailed),
            ],
        }
    }
}

impl FakeErrorCatalogue {
    /// The default patterns.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Provider for FakeErrorCatalogue {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            "fake-error-catalogue",
            "0",
            CapabilitySet::from_iter([Capability::ErrorExplain]),
        )
    }
}

impl ErrorCatalogue for FakeErrorCatalogue {
    fn catalogue(&self) -> CatalogueInfo {
        CatalogueInfo::new(KIND, "1", "the fake engine")
    }

    fn classify(&self, error: &ErrorSummary) -> Classification {
        let category = match error.kind() {
            Some("Query Error") => ErrorCategory::Database,
            Some("Build Error") => ErrorCategory::Compilation,
            _ => ErrorCategory::Unknown,
        };
        let message = error.message().to_lowercase();
        let message = message
            .strip_prefix(&format!(
                "{}: ",
                error.kind().unwrap_or_default().to_lowercase()
            ))
            .unwrap_or(&message);
        match self.patterns.iter().find(|(p, _)| message.starts_with(p)) {
            Some((phrase, symptom)) => {
                let mut found = PatternMatch::new(phrase.replace(' ', "-"), *symptom);
                if *symptom == Symptom::UnknownMacro {
                    found = found.suggest(
                        Suggestion::new(Text::new().plain("Install the fake packages."))
                            .with_command("fake deps", &[]),
                    );
                }
                if *symptom == Symptom::TestFailed {
                    found = found.rerun_with(
                        Text::new().plain("See the rows that fail: test it again, keeping them:"),
                        "--keep-failing-rows",
                    );
                }
                Classification::Recognised(found)
            }
            None => Classification::NotRecognised { category },
        }
    }
}

#[cfg(test)]
mod tests {
    use ods_sdk::conformance::error_catalogue::{ErrorCatalogueHarness, Sample, run};

    use super::*;

    struct Harness(FakeErrorCatalogue);

    impl ErrorCatalogueHarness for Harness {
        fn catalogue(&self) -> &dyn ErrorCatalogue {
            &self.0
        }

        fn samples(&self) -> Vec<Sample> {
            let summary = |m: &str| ErrorSummary::from_message(m).unwrap();
            vec![
                Sample {
                    name: "column",
                    summary: summary("Query Error: no such column 'sk_live_SENTINEL' in t"),
                    expected: Some(Symptom::MissingColumn),
                    sentinel: Some("sk_live_SENTINEL"),
                },
                Sample {
                    name: "macro",
                    summary: summary("no such macro called x"),
                    expected: Some(Symptom::UnknownMacro),
                    sentinel: None,
                },
                Sample {
                    name: "other",
                    summary: summary("Query Error: the disk is on fire"),
                    expected: None,
                    sentinel: None,
                },
            ]
        }
    }

    #[test]
    fn conforms() {
        let report = run(&Harness(FakeErrorCatalogue::new()));
        assert!(report.skipped.is_empty(), "{report:?}");
        assert_eq!(report.passed.len(), 6);
    }
}
