//! Display ordering for the forest.

use std::collections::{BTreeMap, BTreeSet};

use crate::{DerivationNode, DerivationStatus, ForestSnapshot};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayRow {
    pub depth: usize,
    pub node: DerivationNode,
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
