//! Library half of the `ods` CLI: the command framework, output settings and the
//! presentation boundary.
//!
//! `ods-cli` is the composition root (ADR-0001), owns presentation (ADR-0003) and the
//! command framework (ADR-0004). Keeping this in a library lets commands and tests run
//! the whole CLI in-process; the `ods` binary in `main.rs` only supplies real streams.
//! [`Ods`] is that binary as a library, so a custom build can add plugins (ADR-0031).

pub mod app;
pub mod commands;
pub mod exit;
pub mod logging;
pub mod module;
mod ods;
pub mod output;
pub mod plugins;
pub mod present;
pub mod version;

pub use ods::Ods;

#[cfg(test)]
mod sample;
