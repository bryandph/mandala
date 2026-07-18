//! mandala-core — fleet inventory, selector algebra, drift, and the run
//! registry: the shared cores the CLI and the MCP server read.

pub mod cli;
pub mod drift;
pub mod engines;
pub mod eval;
pub mod inventory;
pub mod registry;
pub mod runner;

pub use cli::{Cli, Engine, TuiRequest};
pub use drift::{DriftEntry, DriftError, DriftStatus, Snapshot};
pub use inventory::{Aggregate, Inventory, InventoryError, Member, SUPPORTED_SCHEMA_VERSION};
pub use registry::{ObservedRun, RunInfo, RunLiveness, list_runs, new_run_dir, open_run};
pub use runner::{
    BuildModel, COMMAND_LOG, CommandRun, DeployRun, EventTailer, HostRun, HostState, ansible_dir,
    reboot_argv,
};

/// The mandala porcelain version, surfaced by the CLI `version` command and
/// the MCP server banner.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Placeholder greeting proving the library links into the binary.
#[must_use]
pub fn banner() -> String {
    format!("mandala-core {VERSION}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_carries_the_version() {
        assert!(banner().contains(VERSION));
        assert!(!VERSION.is_empty());
    }
}
