//! Redaction never panics, and what it returns is one short line with no control
//! characters, whatever an engine's message holds (rule 9).
#![no_main]

use libfuzzer_sys::fuzz_target;
use ods_core::redact;

fuzz_target!(|input: (u8, &str)| {
    let (max, text) = input;
    let max = usize::from(max).max(1);
    let _ = redact::literals(text);
    for line in [
        redact::summary_line(text, max),
        redact::value_line(text, max),
    ]
    .into_iter()
    .flatten()
    {
        assert!(line.chars().count() <= max, "{line:?} is longer than {max}");
        assert!(
            !line.chars().any(char::is_control),
            "{line:?} has a control character"
        );
    }
});
