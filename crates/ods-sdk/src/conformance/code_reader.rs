//! Conformance for immutable producing-code readers.
use crate::contracts::{
    code_reader::{CodeReadError, CodeReader},
    producing_code::CodeLocation,
};

/// Exercises an approved fixture with a known producing run and UTF-8 contents.
/// # Panics
/// Panics if identity, content or producing-run isolation is broken.
pub async fn check(reader: &dyn CodeReader, location: &CodeLocation, run: &str, text: &str) {
    crate::contracts::code_reader::require_read_only(reader, location)
        .await
        .expect("fixture authorization verified");
    let first = reader
        .read_code(location, Some(run))
        .await
        .expect("fixture is readable");
    assert_eq!(first.location(), location);
    assert_eq!(first.text(), text);
    let second = reader
        .read_code(location, Some(run))
        .await
        .expect("repeat is readable");
    assert_eq!(first.commit(), second.commit());
    assert_eq!(first.text(), second.text());
    assert_eq!(
        reader.read_code(location, None).await.unwrap_err(),
        CodeReadError::RevisionMismatch
    );
}
