//! The hostable explorer and dashboard (ADR-0009).
//!
//! One lineage page, three ways to ship it:
//! - [`standalone_page`]: a single self-contained HTML file with the graph embedded
//!   (`ods lineage view`); works offline, from disk or an email attachment;
//! - [`export_site`]: a static site (`index.html` + `graph.json`) for any static host
//!   (S3, GitHub Pages, an internal web server);
//! - [`serve`]: a small HTTP server (`ods serve`) with a read-only JSON API (search,
//!   node details, impact) and live reload when the dbt artifacts change.
//!
//! Served, it sits inside the ODS Dashboard: Home at the root (the project's runs,
//! reuse and what needs attention, see [`dashboard`]) and the explorer at `lineage`.
//!
//! This crate only presents. It never reads dbt artifacts or the state store, or wires
//! providers: the caller (a binary) supplies a [`Loader`] that produces a fresh
//! [`Snapshot`], with a [`Dashboard`] of neutral facts (ADR-0001).

pub mod dashboard;
mod fonts;
mod home;
mod page;
mod search;
mod server;

// The Lineage page and its State overlay (#312).
pub mod lineage;

pub use dashboard::Dashboard;
pub use page::{export_site, standalone_page};
pub use search::{SearchHit, search};
pub use server::{Loader, ServeOptions, Snapshot, WebError, router, serve, serve_blocking};

/// Version of the HTTP API, reported by `/api/version`.
pub const API_VERSION: u32 = 1;
