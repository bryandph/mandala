//! Per-frame mouse hit map produced by pure render functions.

use ratatui::layout::Rect;

use crate::keymap::Action;
use crate::state::Tab;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployTabTarget {
    Build,
    Playbook,
    Summary,
    Host(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollPane {
    Mcp,
    Task,
    AttachedLog,
    Build,
    Deploy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Action(Action),
    ExplorerTable(Tab),
    ExplorerRow { tab: Tab, index: usize },
    RunsTable,
    RunsRow(usize),
    DeployTab(DeployTabTarget),
    ScrollPane { pane: ScrollPane, viewport: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Hit {
    rect: Rect,
    target: Target,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HitMap {
    hits: Vec<Hit>,
}

impl HitMap {
    pub fn insert(&mut self, rect: Rect, target: Target) {
        if !rect.is_empty() {
            self.hits.push(Hit { rect, target });
        }
    }

    /// Last rendered wins, so modal targets naturally cover the explorer
    /// entries beneath them.
    #[must_use]
    pub fn hit(&self, column: u16, row: u16) -> Option<&Target> {
        self.hits
            .iter()
            .rev()
            .find(|hit| {
                column >= hit.rect.x
                    && column < hit.rect.x.saturating_add(hit.rect.width)
                    && row >= hit.rect.y
                    && row < hit.rect.y.saturating_add(hit.rect.height)
            })
            .map(|hit| &hit.target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_rendered_target_wins_and_edges_are_exclusive() {
        let mut map = HitMap::default();
        map.insert(
            Rect::new(1, 1, 4, 3),
            Target::Action(Action::ToggleSelection),
        );
        map.insert(Rect::new(2, 2, 2, 1), Target::Action(Action::Confirm));
        assert_eq!(
            map.hit(1, 1),
            Some(&Target::Action(Action::ToggleSelection))
        );
        assert_eq!(map.hit(2, 2), Some(&Target::Action(Action::Confirm)));
        assert_eq!(map.hit(5, 1), None);
        assert_eq!(map.hit(1, 4), None);
    }
}
