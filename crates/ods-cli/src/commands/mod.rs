//! Command adapters: one [`Module`](crate::module::Module) per command group (ADR-0004 §1).

mod completions;
mod config;
mod erd;
mod lineage;
mod mcp;
mod mcp_data;
mod mcp_tools;
mod planned;
mod serve;
mod state;
mod state_plan;
mod state_run;
mod version;

pub use completions::Completions;
pub use config::Config;
pub use erd::ErdCommand;
pub use lineage::Lineage;
pub use mcp::Mcp;
pub use planned::Planned;
pub use serve::Serve;
pub use state::State;
pub use version::Version;

use crate::module::Registry;

/// Modules on the roadmap that are not implemented yet, with their milestone.
const PLANNED: &[(&str, &str, &str)] = &[
    (
        "usage",
        "Inspect real downstream usage of assets and columns",
        "M3 ERD & Usage (v0.3.0)",
    ),
    (
        "ci",
        "Change-impact analysis and selective CI",
        "M4 ODS CI (v0.4.0)",
    ),
    (
        "lsp",
        "Run the ODS language server",
        "M5 LSP & VS Code (v0.5.0)",
    ),
    (
        "agent",
        "Analytics-engineering agent and skills",
        "M6 ODS Agent (v0.6.0)",
    ),
];

/// The registry used by the `ods` binary.
///
/// # Panics
/// Never in practice: built-in names are unique and not reserved, which
/// `default_registry_contains_the_roadmap_commands` checks.
pub fn default_registry() -> Registry {
    let mut registry = Registry::new();
    registry
        .register(Box::new(State))
        .expect("built-in command names are unique and not reserved");
    registry
        .register(Box::new(ErdCommand))
        .expect("built-in command names are unique and not reserved");
    for &(name, about, milestone) in PLANNED {
        registry
            .register(Box::new(Planned::new(name, about, milestone)))
            .expect("built-in command names are unique and not reserved");
    }
    registry
        .register(Box::new(Lineage))
        .expect("built-in command names are unique and not reserved");
    registry
        .register(Box::new(Serve))
        .expect("built-in command names are unique and not reserved");
    registry
        .register(Box::new(Mcp))
        .expect("built-in command names are unique and not reserved");
    registry
        .register(Box::new(Config))
        .expect("built-in command names are unique and not reserved");
    registry
        .register(Box::new(Version))
        .expect("built-in command names are unique and not reserved");
    registry
        .register(Box::new(Completions))
        .expect("built-in command names are unique and not reserved");
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_contains_the_roadmap_commands() {
        assert_eq!(
            default_registry().names(),
            [
                "state",
                "erd",
                "usage",
                "ci",
                "lsp",
                "agent",
                "lineage",
                "serve",
                "mcp",
                "config",
                "version",
                "completions"
            ]
        );
    }
}
