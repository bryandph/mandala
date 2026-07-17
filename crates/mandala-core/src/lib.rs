//! mandala-core — fleet inventory, selector algebra, drift, and the run
//! registry: the shared cores the CLI and the MCP server read.
//!
//! Phase-1 scaffold (OpenSpec change `mandala-rust-rewrite`). The real
//! inventory/drift/registry/runner types land in section 2 of the change;
//! this file carries the workspace-linking placeholder plus a compact
//! `resolve` used by the 1.2 stdio-MCP spike (and a sketch of the selector
//! algebra section 2.1 will generalize over the real aggregate).

use std::collections::BTreeMap;

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

/// The structured result of a selector expansion — the sorted member set plus
/// the canonical comma-joined `limit` string (the confirm token the gated
/// actions require). Mirrors the Python `resolve` tool's `{members, limit}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub members: Vec<String>,
    pub limit: String,
}

/// Expand a selector against the demo fleet: `all`, `@group`, bare members,
/// `!` exclusions, and `,`/`:` separators — the subset of the Python
/// `to_limit` algebra the spike needs (a bare exclusion implies `all`).
/// Unknown members/groups are an error, as in the Python core.
///
/// # Errors
/// Returns a human-readable message if a token names no member or group.
pub fn resolve(selector: &str) -> Result<Resolved, String> {
    let groups = demo_groups();
    let universe = demo_members();

    let expand = |token: &str| -> Result<Vec<&'static str>, String> {
        if token == "all" {
            Ok(universe.clone())
        } else if let Some(name) = token.strip_prefix('@') {
            groups
                .get(name)
                .cloned()
                .ok_or_else(|| format!("no such group: @{name}"))
        } else {
            universe
                .iter()
                .find(|m| **m == token)
                .map(|m| vec![*m])
                .ok_or_else(|| format!("no such member: {token}"))
        }
    };

    let tokens: Vec<&str> = selector
        .split([',', ':'])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();

    let mut include: Vec<&'static str> = Vec::new();
    let mut exclude: Vec<&'static str> = Vec::new();
    let mut saw_inclusion = false;
    for token in &tokens {
        if let Some(rest) = token.strip_prefix('!') {
            exclude.extend(expand(rest)?);
        } else {
            saw_inclusion = true;
            include.extend(expand(token)?);
        }
    }
    // A bare exclusion (`!vishnu`) implies `all`, matching the Python taxonomy.
    if !saw_inclusion && !exclude.is_empty() {
        include.extend(universe.clone());
    }

    let mut members: Vec<String> = include
        .into_iter()
        .filter(|m| !exclude.contains(m))
        .map(String::from)
        .collect();
    members.sort();
    members.dedup();

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
