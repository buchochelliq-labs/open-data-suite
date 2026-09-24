//! Renderers for [`ViewNode`](super::ViewNode) trees.
//!
//! `rich` is the only module in the workspace allowed to use `rs-rich` (ADR-0003 §1).

pub mod plain;
pub mod rich;
