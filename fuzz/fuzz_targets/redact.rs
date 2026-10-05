//! Redaction never panics, and what it returns is one short line with no control
//! characters, whatever an engine's message holds (rule 9). The input is the message
//! itself, so a seed (a recorded dbt error) reaches redaction unchanged.
#![no_main]

use libfuzzer_sys::fuzz_target;
use ods_core::redact;

fuzz_target!(|text: &str| {
    let _ = redact::literals(text);
    for max in [1, 40, 200] {
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
    }
});
