//! Display ordering for the forest.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::{DerivationNode, DerivationStatus, ForestSnapshot};

pub const DEFAULT_ACTIVITY_ROW_BUDGET: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisplayRow {
    pub depth: usize,
    pub node: DerivationNode,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ElidedCounts {
    pub unknown: usize,
    pub planned: usize,
    pub built: usize,
}

impl ElidedCounts {
    #[must_use]
    pub fn total(&self) -> usize {
        self.unknown + self.planned + self.built
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ActivityProjection {
    pub rows: Vec<DisplayRow>,
    pub elided: ElidedCounts,
}

/// Failed and active work stays visible first; waiting work follows; completed
/// and unknown nodes recede. Names provide deterministic tie-breaking.
#[must_use]
pub fn display_rows(snapshot: &ForestSnapshot) -> Vec<DisplayRow> {
    let nodes: BTreeMap<&str, &DerivationNode> = snapshot
        .nodes
        .iter()
        .map(|node| (node.path.as_str(), node))
        .collect();
    let mut roots = snapshot.roots.clone();
    roots.sort_by(|a, b| compare(nodes.get(a.as_str()), nodes.get(b.as_str())));
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    for root in roots {
        visit(&root, 0, &nodes, &mut seen, &mut rows);
    }
    for node in &snapshot.nodes {
        visit(&node.path, 0, &nodes, &mut seen, &mut rows);
    }
    rows
}

/// Project the complete forest into a bounded operational view.
///
/// Active/failed nodes and their consumer-side ancestor chains are mandatory,
/// even when they exceed `row_budget`. Planned work fills the remaining
/// budget. When no work is active or planned, roots provide a compact terminal
/// view. The full graph remains available through [`display_rows`].
#[must_use]
pub fn activity_projection(snapshot: &ForestSnapshot, row_budget: usize) -> ActivityProjection {
    let full = display_rows(snapshot);
    let by_path: BTreeMap<&str, &DerivationNode> = snapshot
        .nodes
        .iter()
        .map(|node| (node.path.as_str(), node))
        .collect();
    let mut parents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for node in &snapshot.nodes {
        for input in &node.inputs {
            parents
                .entry(input.as_str())
                .or_default()
                .push(node.path.as_str());
        }
    }

    let mut keep: BTreeSet<String> = snapshot
        .nodes
        .iter()
        .filter(|node| is_mandatory(node.status))
        .map(|node| node.path.clone())
        .collect();
    let mut frontier: Vec<String> = keep.iter().cloned().collect();
    while let Some(path) = frontier.pop() {
        for parent in parents.get(path.as_str()).into_iter().flatten() {
            if keep.insert((*parent).to_string()) {
                frontier.push((*parent).to_string());
            }
        }
    }

    for row in &full {
        if keep.len() >= row_budget {
            break;
        }
        if row.node.status == DerivationStatus::Planned {
            keep.insert(row.node.path.clone());
        }
    }
    if keep.is_empty() {
        for root in &snapshot.roots {
            if keep.len() >= row_budget {
                break;
            }
            if by_path.contains_key(root.as_str()) {
                keep.insert(root.clone());
            }
        }
    }

    let rows = full
        .into_iter()
        .filter(|row| keep.contains(&row.node.path))
        .collect();
    let mut elided = ElidedCounts::default();
    for node in &snapshot.nodes {
        if keep.contains(&node.path) {
            continue;
        }
        match node.status {
            DerivationStatus::Unknown => elided.unknown += 1,
            DerivationStatus::Planned => elided.planned += 1,
            DerivationStatus::Built => elided.built += 1,
            DerivationStatus::Building
            | DerivationStatus::Downloading
            | DerivationStatus::Substituting
            | DerivationStatus::Failed => {}
        }
    }
    ActivityProjection { rows, elided }
}

fn is_mandatory(status: DerivationStatus) -> bool {
    matches!(
        status,
        DerivationStatus::Building
            | DerivationStatus::Downloading
            | DerivationStatus::Substituting
            | DerivationStatus::Failed
    )
}

fn visit(
    path: &str,
    depth: usize,
    nodes: &BTreeMap<&str, &DerivationNode>,
    seen: &mut BTreeSet<String>,
    rows: &mut Vec<DisplayRow>,
) {
    if !seen.insert(path.to_string()) {
        return;
    }
    let Some(node) = nodes.get(path).copied() else {
        return;
    };
    let mut children = node.inputs.clone();
    children.sort_by(|a, b| compare(nodes.get(a.as_str()), nodes.get(b.as_str())));
    for child in children {
        visit(&child, depth + 1, nodes, seen, rows);
    }
    rows.push(DisplayRow {
        depth,
        node: node.clone(),
    });
}

fn compare(a: Option<&&DerivationNode>, b: Option<&&DerivationNode>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => priority(a.status)
            .cmp(&priority(b.status))
            .then_with(|| a.name.cmp(&b.name)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn priority(status: DerivationStatus) -> u8 {
    match status {
        DerivationStatus::Failed => 0,
        DerivationStatus::Building => 1,
        DerivationStatus::Downloading | DerivationStatus::Substituting => 2,
        DerivationStatus::Planned => 3,
        DerivationStatus::Built => 4,
        DerivationStatus::Unknown => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ForestCounts, ForestSnapshot};

    fn node(path: &str, status: DerivationStatus, inputs: &[&str]) -> DerivationNode {
        DerivationNode {
            path: path.to_string(),
            name: path.to_string(),
            status,
            inputs: inputs.iter().map(|input| (*input).to_string()).collect(),
            outputs: BTreeMap::new(),
            host: None,
            last_activity: None,
            log_tail: Vec::new(),
            note: None,
            started_ms: None,
            finished_ms: None,
            eta_seconds: None,
        }
    }

    fn snapshot(nodes: Vec<DerivationNode>, roots: &[&str]) -> ForestSnapshot {
        ForestSnapshot {
            version: 1,
            elapsed_ms: 0,
            nodes,
            roots: roots.iter().map(|root| (*root).to_string()).collect(),
            counts: ForestCounts::default(),
            activity: ActivityProjection::default(),
            completed_downloads: 0,
            completed_substitutions: 0,
            transfers: Vec::new(),
            recent_logs: Vec::new(),
            expectations: Vec::new(),
            current_activity: Vec::new(),
            failed_derivations: Vec::new(),
            errors: Vec::new(),
            unknown_activity_types: BTreeMap::new(),
            unknown_result_types: BTreeMap::new(),
            unknown_actions: 0,
            malformed_messages: 0,
            ignored_lines: 0,
        }
    }

    #[test]
    fn active_and_failed_context_is_never_elided() {
        let nodes = vec![
            node("root", DerivationStatus::Planned, &["middle", "built-c"]),
            node("middle", DerivationStatus::Planned, &["active", "built-b"]),
            node("active", DerivationStatus::Building, &["built-a"]),
            node("failed", DerivationStatus::Failed, &[]),
            node("built-a", DerivationStatus::Built, &[]),
            node("built-b", DerivationStatus::Built, &[]),
            node("built-c", DerivationStatus::Built, &[]),
        ];
        let snapshot = snapshot(nodes, &["root", "failed"]);
        let projected = activity_projection(&snapshot, 2);
        let paths: BTreeSet<&str> = projected
            .rows
            .iter()
            .map(|row| row.node.path.as_str())
            .collect();
        assert_eq!(
            paths,
            BTreeSet::from(["active", "failed", "middle", "root"])
        );
        assert_eq!(projected.elided.built, 3);
        assert_eq!(projected, activity_projection(&snapshot, 2));
    }

    #[test]
    fn planned_work_only_fills_the_remaining_budget() {
        let nodes = (0..20)
            .map(|index| {
                node(
                    &format!("planned-{index:02}"),
                    DerivationStatus::Planned,
                    &[],
                )
            })
            .collect();
        let roots: Vec<String> = (0..20).map(|index| format!("planned-{index:02}")).collect();
        let root_refs: Vec<&str> = roots.iter().map(String::as_str).collect();
        let snapshot = snapshot(nodes, &root_refs);
        let projected = activity_projection(&snapshot, 5);
        assert_eq!(projected.rows.len(), 5);
        assert_eq!(projected.elided.planned, 15);
    }
}
