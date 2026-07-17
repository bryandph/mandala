//! mandala-core — fleet inventory, selector algebra, drift, and the run
//! registry: the shared cores the CLI and the MCP server read.
//!
//! Phase-1 scaffold (OpenSpec change `mandala-rust-rewrite`). The real
//! inventory/drift/registry/runner types land in section 2 of the change;
//! this file carries the workspace-linking placeholder plus a compact
//! `resolve` used by the 1.2 stdio-MCP spike (and a sketch of the selector
//! algebra section 2.1 will generalize over the real aggregate).

use std::collections::BTreeMap;

pub mod drift;
pub mod eval;
pub mod inventory;
pub mod registry;
pub mod runner;

pub use drift::{DriftEntry, DriftError, DriftStatus, Snapshot};
pub use inventory::{Aggregate, Inventory, InventoryError, Member, SUPPORTED_SCHEMA_VERSION};
pub use registry::{ObservedRun, RunInfo, RunLiveness, list_runs, new_run_dir, open_run};
pub use runner::{BuildModel, EventTailer, HostRun, HostState};

/// The mandala porcelain version, surfaced by the CLI `version` command and
/// the MCP server banner.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Placeholder greeting proving the library links into the binary.
#[must_use]
pub fn banner() -> String {
    format!("mandala-core {VERSION}")
}

/// A tiny demo fleet for the phase-1 stdio-MCP spike (section 1.2). The real
/// inventory is evaluated from `.#mandala` in section 2.1; this exists only
/// so the `resolve` tool has structured data to round-trip over stdio.
fn demo_groups() -> BTreeMap<&'static str, Vec<&'static str>> {
    BTreeMap::from([("k3s", vec!["cache", "web"]), ("gateway", vec!["router"])])
}

fn demo_members() -> Vec<&'static str> {
    let mut all: Vec<&'static str> = demo_groups().into_values().flatten().collect();
    all.sort_unstable();
    all.dedup();
    all
}

/// The demo fleet as a validated [`Inventory`] — a real aggregate value fed
/// through the same schemaVersion gate and selector algebra the fleet uses, so
/// the spike's `resolve` tool exercises the production code path (section 2.1)
/// rather than a parallel sketch.
fn demo_inventory() -> Inventory {
    let members: serde_json::Map<String, serde_json::Value> = demo_members()
        .into_iter()
        .map(|m| {
            (
                m.to_string(),
                serde_json::Value::Object(serde_json::Map::new()),
            )
        })
        .collect();
    let groups: serde_json::Map<String, serde_json::Value> = demo_groups()
        .into_iter()
        .map(|(g, names)| (g.to_string(), serde_json::Value::from(names)))
        .collect();
    let aggregate = serde_json::json!({
        "schemaVersion": SUPPORTED_SCHEMA_VERSION,
        "members": members,
        "groups": groups,
    });
    Inventory::from_value(aggregate).expect("demo aggregate is valid")
}

/// The structured result of a selector expansion — the sorted member set plus
/// the canonical comma-joined `limit` string (the confirm token the gated
/// actions require). Mirrors the Python `resolve` tool's `{members, limit}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub members: Vec<String>,
    pub limit: String,
}

/// Expand a selector against the demo fleet, delegating to the production
/// [`Inventory`] selector algebra (`all`, `@group`, bare members, `!`
/// exclusions, `,`/`:` separators; a bare exclusion implies `all`; unknown
/// atoms are errors). Kept as a thin, string-erroring wrapper for the
/// section-1.2 stdio-MCP spike; section 4 serves the real inventory instead.
///
/// # Errors
/// Returns the [`InventoryError`] message if the selector is empty, resolves to
/// nothing, or names an unknown member/group.
pub fn resolve(selector: &str) -> Result<Resolved, String> {
    let members = demo_inventory()
        .resolve(selector)
        .map_err(|e| e.to_string())?;
    let limit = members.join(",");
    Ok(Resolved { members, limit })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_carries_the_version() {
        assert!(banner().contains(VERSION));
        assert!(!VERSION.is_empty());
    }

    #[test]
    fn resolve_group_sorts_and_joins() {
        let r = resolve("@k3s").unwrap();
        assert_eq!(r.members, vec!["cache", "web"]);
        assert_eq!(r.limit, "cache,web");
    }

    #[test]
    fn resolve_all_minus_group() {
        let r = resolve("all,!@gateway").unwrap();
        assert_eq!(r.members, vec!["cache", "web"]);
    }

    #[test]
    fn resolve_bare_exclusion_implies_all() {
        let r = resolve("!router").unwrap();
        assert_eq!(r.members, vec!["cache", "web"]);
    }

    #[test]
    fn resolve_unknown_member_errors() {
        assert!(resolve("ghost").is_err());
        assert!(resolve("@nope").is_err());
    }
}
