//! Library half of the `ods` CLI: output settings and the presentation boundary.
//!
//! `ods-cli` is the composition root (ADR-0001) and owns presentation (ADR-0003).
//! Keeping this in a library lets commands and integration tests share it; the `ods`
//! binary in `main.rs` only parses arguments and dispatches.

pub mod output;
pub mod present;
pub mod version;

#[cfg(test)]
mod sample;
