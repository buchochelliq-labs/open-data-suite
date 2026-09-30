//! Conformance suite for [`ErrorCatalogue`] (#323, ADR-0025).

use ods_core::Capability;
use ods_core::failure::{ErrorCategory, Symptom};

use super::Report;
use crate::contracts::error_catalogue::{Classification, ErrorCatalogue};
use crate::contracts::run_events::ErrorSummary;

/// An error the catalogue is tested on: the engine's full message, and what the
/// catalogue should make of it.
#[derive(Debug, Clone)]
pub struct Sample {
    /// A name for failures, e.g. the fixture it comes from.
    pub name: &'static str,
    /// The error's summary, as the provider makes it from the engine's message.
    pub summary: ErrorSummary,
    /// The symptom a pattern should recognise; `None` when no pattern should.
    pub expected: Option<Symptom>,
    /// Text in the engine's full message that must never come back (a value or a
    /// secret the message quoted), if any.
    pub sentinel: Option<&'static str>,
}

/// What the suite needs from a catalogue under test.
pub trait ErrorCatalogueHarness {
    /// The catalogue.
    fn catalogue(&self) -> &dyn ErrorCatalogue;

    /// Errors it is tested on: at least one recognised and one not, from real engine
    /// output where possible.
    fn samples(&self) -> Vec<Sample>;
}

fn advertises_the_capability(harness: &dyn ErrorCatalogueHarness) {
    let case = "advertises_the_capability";
    let catalogue = harness.catalogue();
    assert!(
        catalogue
            .info()
            .capabilities
            .contains(&Capability::ErrorExplain),
        "{case}: an error catalogue advertises error_explain"
    );
    let info = catalogue.catalogue();
    assert!(
        !info.name.is_empty() && !info.version.is_empty() && !info.engine.is_empty(),
        "{case}: {info:?}"
    );
}

fn samples_classify_as_expected(harness: &dyn ErrorCatalogueHarness) {
    let case = "samples_classify_as_expected";
    let samples = harness.samples();
    assert!(
        samples.iter().any(|s| s.expected.is_some())
            && samples.iter().any(|s| s.expected.is_none()),
        "{case}: the harness needs recognised and unrecognised samples"
    );
    for sample in samples {
        let got = harness.catalogue().classify(&sample.summary);
        match (&got, sample.expected) {
            (Classification::Recognised(m), Some(symptom)) => {
                assert_eq!(m.symptom, symptom, "{case}: {}: {got:?}", sample.name);
                assert!(
                    !m.id.is_empty(),
                    "{case}: {}: a pattern has an id",
                    sample.name
                );
            }
            (Classification::NotRecognised { .. }, None) => {}
            _ => panic!(
                "{case}: {}: expected {:?}, got {got:?}",
                sample.name, sample.expected
            ),
        }
    }
}

fn classification_is_deterministic(harness: &dyn ErrorCatalogueHarness) {
    let case = "classification_is_deterministic";
    for sample in harness.samples() {
        let catalogue = harness.catalogue();
        assert_eq!(
            catalogue.classify(&sample.summary),
            catalogue.classify(&sample.summary),
            "{case}: {}",
            sample.name
        );
    }
}

fn unknown_text_is_not_recognised(harness: &dyn ErrorCatalogueHarness) {
    let case = "unknown_text_is_not_recognised";
    let catalogue = harness.catalogue();
    for text in [
        "Something went sideways in a way nobody has seen before",
        "zzz",
        "Error: [value removed]",
    ] {
        let summary = ErrorSummary::from_message(text).unwrap_or_else(|| panic!("{case}: {text}"));
        let got = catalogue.classify(&summary);
        assert!(
            matches!(got, Classification::NotRecognised { .. }),
            "{case}: `{text}` must not be recognised: {got:?}"
        );
    }
    let bare = ErrorSummary::from_message("nothing to go on").unwrap_or_else(|| panic!("{case}"));
    assert_eq!(
        catalogue.classify(&bare).category(),
        ErrorCategory::Unknown,
        "{case}: without a kind, the category is unknown"
    );
}

fn nothing_quoted_comes_back(harness: &dyn ErrorCatalogueHarness) {
    let case = "nothing_quoted_comes_back";
    for sample in harness.samples() {
        let got = harness.catalogue().classify(&sample.summary);
        let json = serde_json::to_string(&got).unwrap_or_else(|e| panic!("{case}: {e}"));
        if let Some(sentinel) = sample.sentinel {
            assert!(
                !json.contains(sentinel) && !format!("{:?}", sample.summary).contains(sentinel),
                "{case}: {}: `{sentinel}` came back: {json}",
                sample.name
            );
        }
        assert!(
            !json.contains("[value removed]"),
            "{case}: {}: a classification quotes nothing: {json}",
            sample.name
        );
    }
}

fn token_shaped_values_never_come_back(harness: &dyn ErrorCatalogueHarness) {
    let case = "token_shaped_values_never_come_back";
    // Unquoted values survive an error summary's redaction; a classification (its
    // subject in particular) must not repeat them.
    for text in [
        "KeyError: ghp_SENTINEL123",
        "Database Error: could not connect to db://u:SENTINEL@h/db",
        "ValueError: token=SENTINEL sk_live_SENTINEL",
        "SENTINELError: boom",
    ] {
        let summary = ErrorSummary::from_message(text).unwrap_or_else(|| panic!("{case}: {text}"));
        let got = harness.catalogue().classify(&summary);
        let json = serde_json::to_string(&got).unwrap_or_else(|e| panic!("{case}: {e}"));
        assert!(
            !json.contains("SENTINEL"),
            "{case}: `{text}` came back: {json}"
        );
    }
}

/// Runs every case against the harness's catalogue. Panics on the first failure.
pub fn run(harness: &dyn ErrorCatalogueHarness) -> Report {
    let mut report = Report::default();
    advertises_the_capability(harness);
    report.passed.push("advertises_the_capability");
    samples_classify_as_expected(harness);
    report.passed.push("samples_classify_as_expected");
    classification_is_deterministic(harness);
    report.passed.push("classification_is_deterministic");
    unknown_text_is_not_recognised(harness);
    report.passed.push("unknown_text_is_not_recognised");
    nothing_quoted_comes_back(harness);
    report.passed.push("nothing_quoted_comes_back");
    token_shaped_values_never_come_back(harness);
    report.passed.push("token_shaped_values_never_come_back");
    report
}
