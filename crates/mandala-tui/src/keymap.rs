//! Centralized keyboard bindings for every TUI context.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::screen::ScreenState;
use crate::state::AppState;

/// The active input surface. Bindings are contextual so action keys never
/// shadow navigation in a different view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Context {
    Global,
    Explorer,
    Confirm,
    Reboot,
    Task,
    AttachedLog,
    Deploy,
    DeployKillArmed,
    Runs,
}

pub const CONTEXTS: [Context; 9] = [
    Context::Global,
    Context::Explorer,
    Context::Confirm,
    Context::Reboot,
    Context::Task,
    Context::AttachedLog,
    Context::Deploy,
    Context::DeployKillArmed,
    Context::Runs,
];

/// Pure input actions. Runtime effects remain in `app.rs`; this enum is the
/// stable seam shared by keys, footer hints, and (later) mouse hit targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Suspend,
    NextTab,
    PreviousTab,
    TabOne,
    TabTwo,
    TabThree,
    MoveUp,
    MoveDown,
    ExtendUp,
    ExtendDown,
    SkipUp,
    SkipDown,
    PageUp,
    PageDown,
    HalfPageUp,
    HalfPageDown,
    Top,
    Bottom,
    ToggleSelection,
    ClearSelection,
    Reload,
    RefreshDrift,
    Ping,
    Reboot,
    Deploy,
    OpenRuns,
    ToggleMcp,
    ToggleHalt,
    ToggleBoot,
    Confirm,
    Cancel,
    OrderOne,
    OrderTwo,
    OrderThree,
    ToggleDrain,
    Close,
    ArmTerminate,
    ConfirmTerminate,
    BuildTab,
    PlaybookTab,
    SummaryTab,
    RefreshRuns,
    Activate,
    TerminalSelectionHelp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeyPattern {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl KeyPattern {
    const fn plain(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers::NONE,
        }
    }

    const fn modified(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    fn matches(self, event: KeyEvent) -> bool {
        self.code == event.code && self.modifiers == normalized_modifiers(event)
    }
}

/// Shift is already encoded in a character's case (`R` versus `r`) and in
/// `BackTab`; normalizing it keeps real terminal events equivalent to the
/// synthetic `KeyEvent::from(KeyCode)` values used by the loop tests.
fn normalized_modifiers(event: KeyEvent) -> KeyModifiers {
    let mut modifiers = event.modifiers;
    if matches!(event.code, KeyCode::Char(_) | KeyCode::BackTab) {
        modifiers.remove(KeyModifiers::SHIFT);
    }
    modifiers
}

#[derive(Debug)]
struct Binding {
    context: Context,
    keys: &'static [KeyPattern],
    action: Action,
    hint: Option<HintSpec>,
}

const fn binding(context: Context, keys: &'static [KeyPattern], action: Action) -> Binding {
    Binding {
        context,
        keys,
        action,
        hint: None,
    }
}

const fn hinted_binding(
    context: Context,
    keys: &'static [KeyPattern],
    action: Action,
    hint: HintSpec,
) -> Binding {
    Binding {
        context,
        keys,
        action,
        hint: Some(hint),
    }
}

#[derive(Debug, Clone, Copy)]
enum HintSpec {
    Static {
        key: &'static str,
        label: &'static str,
    },
    DebugMcp,
    DeploySummary,
    DeployClose,
    DeployTerminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hint {
    pub key: &'static str,
    pub label: &'static str,
}

impl HintSpec {
    fn visible(self, state: &AppState) -> Option<Hint> {
        match self {
            Self::Static { key, label } => Some(Hint { key, label }),
            Self::DebugMcp if state.debug_mcp => Some(Hint {
                key: "m",
                label: "mcp panel",
            }),
            Self::DeploySummary
                if matches!(
                    state.screen.as_ref(),
                    Some(ScreenState::Deploy(view)) if view.summary.is_some()
                ) =>
            {
                Some(Hint {
                    key: "s",
                    label: "summary tab",
                })
            }
            Self::DeployClose => {
                let finished = matches!(
                    state.screen.as_ref(),
                    Some(ScreenState::Deploy(view)) if view.finished
                );
                Some(Hint {
                    key: "esc",
                    label: if finished {
                        "close"
                    } else {
                        "detach (run keeps going)"
                    },
                })
            }
            Self::DeployTerminate
                if matches!(
                    state.screen.as_ref(),
                    Some(ScreenState::Deploy(view)) if !view.finished
                ) =>
            {
                Some(Hint {
                    key: "ctrl-k",
                    label: "terminate…",
                })
            }
            _ => None,
        }
    }
}

const UP: &[KeyPattern] = &[
    KeyPattern::plain(KeyCode::Up),
    KeyPattern::plain(KeyCode::Char('k')),
];
const DOWN: &[KeyPattern] = &[
    KeyPattern::plain(KeyCode::Down),
    KeyPattern::plain(KeyCode::Char('j')),
];
const UP_WITH_MODIFIERS: &[KeyPattern] = &[
    KeyPattern::plain(KeyCode::Up),
    KeyPattern::modified(KeyCode::Up, KeyModifiers::SHIFT),
    KeyPattern::modified(KeyCode::Up, KeyModifiers::CONTROL),
    KeyPattern::plain(KeyCode::Char('k')),
];
const DOWN_WITH_MODIFIERS: &[KeyPattern] = &[
    KeyPattern::plain(KeyCode::Down),
    KeyPattern::modified(KeyCode::Down, KeyModifiers::SHIFT),
    KeyPattern::modified(KeyCode::Down, KeyModifiers::CONTROL),
    KeyPattern::plain(KeyCode::Char('j')),
];
const PREVIOUS_TAB: &[KeyPattern] = &[
    KeyPattern::plain(KeyCode::Left),
    KeyPattern::plain(KeyCode::Char('h')),
    KeyPattern::plain(KeyCode::BackTab),
];
const NEXT_TAB: &[KeyPattern] = &[
    KeyPattern::plain(KeyCode::Right),
    KeyPattern::plain(KeyCode::Char('l')),
    KeyPattern::plain(KeyCode::Tab),
];
const PAGE_UP: &[KeyPattern] = &[KeyPattern::plain(KeyCode::PageUp)];
const PAGE_DOWN: &[KeyPattern] = &[KeyPattern::plain(KeyCode::PageDown)];
const HALF_UP: &[KeyPattern] = &[KeyPattern::modified(
    KeyCode::Char('u'),
    KeyModifiers::CONTROL,
)];
const HALF_DOWN: &[KeyPattern] = &[KeyPattern::modified(
    KeyCode::Char('d'),
    KeyModifiers::CONTROL,
)];
const TOP: &[KeyPattern] = &[
    KeyPattern::plain(KeyCode::Home),
    KeyPattern::plain(KeyCode::Char('g')),
];
const BOTTOM: &[KeyPattern] = &[
    KeyPattern::plain(KeyCode::End),
    KeyPattern::plain(KeyCode::Char('G')),
];
const CLOSE: &[KeyPattern] = &[
    KeyPattern::plain(KeyCode::Esc),
    KeyPattern::plain(KeyCode::Char('q')),
];
const CANCEL: &[KeyPattern] = &[
    KeyPattern::plain(KeyCode::Esc),
    KeyPattern::plain(KeyCode::Char('n')),
];

const BINDINGS: &[Binding] = &[
    binding(
        Context::Global,
        &[KeyPattern::modified(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )],
        Action::Quit,
    ),
    binding(
        Context::Global,
        &[KeyPattern::modified(
            KeyCode::Char('z'),
            KeyModifiers::CONTROL,
        )],
        Action::Suspend,
    ),
    hinted_binding(
        Context::Explorer,
        &[KeyPattern::plain(KeyCode::Char('q'))],
        Action::Quit,
        HintSpec::Static {
            key: "q",
            label: "quit",
        },
    ),
    binding(Context::Explorer, PREVIOUS_TAB, Action::PreviousTab),
    hinted_binding(
        Context::Explorer,
        NEXT_TAB,
        Action::NextTab,
        HintSpec::Static {
            key: "tab",
            label: "views",
        },
    ),
    binding(
        Context::Explorer,
        &[KeyPattern::plain(KeyCode::Char('1'))],
        Action::TabOne,
    ),
    binding(
        Context::Explorer,
        &[KeyPattern::plain(KeyCode::Char('2'))],
        Action::TabTwo,
    ),
    binding(
        Context::Explorer,
        &[KeyPattern::plain(KeyCode::Char('3'))],
        Action::TabThree,
    ),
    binding(Context::Explorer, UP, Action::MoveUp),
    binding(Context::Explorer, DOWN, Action::MoveDown),
    binding(
        Context::Explorer,
        &[KeyPattern::modified(KeyCode::Up, KeyModifiers::SHIFT)],
        Action::ExtendUp,
    ),
    binding(
        Context::Explorer,
        &[KeyPattern::modified(KeyCode::Down, KeyModifiers::SHIFT)],
        Action::ExtendDown,
    ),
    binding(
        Context::Explorer,
        &[KeyPattern::modified(KeyCode::Up, KeyModifiers::CONTROL)],
        Action::SkipUp,
    ),
    binding(
        Context::Explorer,
        &[KeyPattern::modified(KeyCode::Down, KeyModifiers::CONTROL)],
        Action::SkipDown,
    ),
    binding(Context::Explorer, PAGE_UP, Action::PageUp),
    binding(Context::Explorer, PAGE_DOWN, Action::PageDown),
    binding(Context::Explorer, HALF_UP, Action::HalfPageUp),
    binding(Context::Explorer, HALF_DOWN, Action::HalfPageDown),
    binding(Context::Explorer, TOP, Action::Top),
    binding(Context::Explorer, BOTTOM, Action::Bottom),
    hinted_binding(
        Context::Explorer,
        &[KeyPattern::plain(KeyCode::Char(' '))],
        Action::ToggleSelection,
        HintSpec::Static {
            key: "space",
            label: "select",
        },
    ),
    binding(
        Context::Explorer,
        &[KeyPattern::plain(KeyCode::Esc)],
        Action::ClearSelection,
    ),
    hinted_binding(
        Context::Explorer,
        &[KeyPattern::plain(KeyCode::Char('r'))],
        Action::Reload,
        HintSpec::Static {
            key: "r",
            label: "reload",
        },
    ),
    hinted_binding(
        Context::Explorer,
        &[KeyPattern::plain(KeyCode::Char('S'))],
        Action::RefreshDrift,
        HintSpec::Static {
            key: "S",
            label: "refresh drift",
        },
    ),
    hinted_binding(
        Context::Explorer,
        &[KeyPattern::plain(KeyCode::Char('p'))],
        Action::Ping,
        HintSpec::Static {
            key: "p",
            label: "ping",
        },
    ),
    hinted_binding(
        Context::Explorer,
        &[KeyPattern::plain(KeyCode::Char('R'))],
        Action::Reboot,
        HintSpec::Static {
            key: "R",
            label: "reboot",
        },
    ),
    hinted_binding(
        Context::Explorer,
        &[KeyPattern::plain(KeyCode::Char('D'))],
        Action::Deploy,
        HintSpec::Static {
            key: "D",
            label: "deploy",
        },
    ),
    binding(
        Context::Explorer,
        &[KeyPattern::plain(KeyCode::Char('a'))],
        Action::OpenRuns,
    ),
    hinted_binding(
        Context::Explorer,
        &[KeyPattern::plain(KeyCode::Char('m'))],
        Action::ToggleMcp,
        HintSpec::DebugMcp,
    ),
    hinted_binding(
        Context::Explorer,
        &[],
        Action::TerminalSelectionHelp,
        HintSpec::Static {
            key: "shift+drag",
            label: "terminal select",
        },
    ),
    binding(
        Context::Confirm,
        &[KeyPattern::plain(KeyCode::Char('h'))],
        Action::ToggleHalt,
    ),
    binding(
        Context::Confirm,
        &[KeyPattern::plain(KeyCode::Char('b'))],
        Action::ToggleBoot,
    ),
    binding(
        Context::Confirm,
        &[KeyPattern::plain(KeyCode::Char('y'))],
        Action::Confirm,
    ),
    binding(Context::Confirm, CANCEL, Action::Cancel),
    binding(
        Context::Reboot,
        &[KeyPattern::plain(KeyCode::Char('1'))],
        Action::OrderOne,
    ),
    binding(
        Context::Reboot,
        &[KeyPattern::plain(KeyCode::Char('2'))],
        Action::OrderTwo,
    ),
    binding(
        Context::Reboot,
        &[KeyPattern::plain(KeyCode::Char('3'))],
        Action::OrderThree,
    ),
    binding(
        Context::Reboot,
        &[KeyPattern::plain(KeyCode::Char('d'))],
        Action::ToggleDrain,
    ),
    binding(
        Context::Reboot,
        &[KeyPattern::plain(KeyCode::Char('y'))],
        Action::Confirm,
    ),
    binding(Context::Reboot, CANCEL, Action::Cancel),
    binding(Context::Task, UP_WITH_MODIFIERS, Action::MoveUp),
    binding(Context::Task, DOWN_WITH_MODIFIERS, Action::MoveDown),
    binding(Context::Task, PAGE_UP, Action::PageUp),
    binding(Context::Task, PAGE_DOWN, Action::PageDown),
    binding(Context::Task, HALF_UP, Action::HalfPageUp),
    binding(Context::Task, HALF_DOWN, Action::HalfPageDown),
    binding(Context::Task, TOP, Action::Top),
    binding(Context::Task, BOTTOM, Action::Bottom),
    hinted_binding(
        Context::Task,
        CLOSE,
        Action::Close,
        HintSpec::Static {
            key: "esc",
            label: "back (terminates if running)",
        },
    ),
    binding(Context::AttachedLog, UP_WITH_MODIFIERS, Action::MoveUp),
    binding(Context::AttachedLog, DOWN_WITH_MODIFIERS, Action::MoveDown),
    binding(Context::AttachedLog, PAGE_UP, Action::PageUp),
    binding(Context::AttachedLog, PAGE_DOWN, Action::PageDown),
    binding(Context::AttachedLog, HALF_UP, Action::HalfPageUp),
    binding(Context::AttachedLog, HALF_DOWN, Action::HalfPageDown),
    binding(Context::AttachedLog, TOP, Action::Top),
    binding(Context::AttachedLog, BOTTOM, Action::Bottom),
    hinted_binding(
        Context::AttachedLog,
        CLOSE,
        Action::Close,
        HintSpec::Static {
            key: "esc",
            label: "detach (run keeps going)",
        },
    ),
    hinted_binding(
        Context::Deploy,
        &[KeyPattern::modified(
            KeyCode::Char('k'),
            KeyModifiers::CONTROL,
        )],
        Action::ArmTerminate,
        HintSpec::DeployTerminate,
    ),
    binding(Context::Deploy, UP_WITH_MODIFIERS, Action::MoveUp),
    binding(Context::Deploy, DOWN_WITH_MODIFIERS, Action::MoveDown),
    binding(Context::Deploy, PAGE_UP, Action::PageUp),
    binding(Context::Deploy, PAGE_DOWN, Action::PageDown),
    binding(Context::Deploy, HALF_UP, Action::HalfPageUp),
    binding(Context::Deploy, HALF_DOWN, Action::HalfPageDown),
    binding(Context::Deploy, TOP, Action::Top),
    binding(Context::Deploy, BOTTOM, Action::Bottom),
    binding(Context::Deploy, PREVIOUS_TAB, Action::PreviousTab),
    hinted_binding(
        Context::Deploy,
        NEXT_TAB,
        Action::NextTab,
        HintSpec::Static {
            key: "tab",
            label: "cycle tabs",
        },
    ),
    hinted_binding(
        Context::Deploy,
        &[KeyPattern::plain(KeyCode::Char('b'))],
        Action::BuildTab,
        HintSpec::Static {
            key: "b",
            label: "nom build tab",
        },
    ),
    hinted_binding(
        Context::Deploy,
        &[KeyPattern::plain(KeyCode::Char('p'))],
        Action::PlaybookTab,
        HintSpec::Static {
            key: "p",
            label: "playbook output tab",
        },
    ),
    hinted_binding(
        Context::Deploy,
        &[KeyPattern::plain(KeyCode::Char('s'))],
        Action::SummaryTab,
        HintSpec::DeploySummary,
    ),
    hinted_binding(Context::Deploy, CLOSE, Action::Close, HintSpec::DeployClose),
    hinted_binding(
        Context::DeployKillArmed,
        &[KeyPattern::plain(KeyCode::Char('y'))],
        Action::ConfirmTerminate,
        HintSpec::Static {
            key: "y",
            label: "TERMINATE the run",
        },
    ),
    hinted_binding(
        Context::DeployKillArmed,
        &[],
        Action::Cancel,
        HintSpec::Static {
            key: "any other key",
            label: "cancel",
        },
    ),
    hinted_binding(
        Context::Runs,
        UP_WITH_MODIFIERS,
        Action::MoveUp,
        HintSpec::Static {
            key: "j/k",
            label: "move",
        },
    ),
    binding(Context::Runs, DOWN_WITH_MODIFIERS, Action::MoveDown),
    binding(Context::Runs, PAGE_UP, Action::PageUp),
    binding(Context::Runs, PAGE_DOWN, Action::PageDown),
    binding(Context::Runs, HALF_UP, Action::HalfPageUp),
    binding(Context::Runs, HALF_DOWN, Action::HalfPageDown),
    binding(Context::Runs, TOP, Action::Top),
    binding(Context::Runs, BOTTOM, Action::Bottom),
    hinted_binding(
        Context::Runs,
        &[KeyPattern::plain(KeyCode::Char('r'))],
        Action::RefreshRuns,
        HintSpec::Static {
            key: "r",
            label: "refresh",
        },
    ),
    hinted_binding(
        Context::Runs,
        &[KeyPattern::plain(KeyCode::Enter)],
        Action::Activate,
        HintSpec::Static {
            key: "enter",
            label: "attach",
        },
    ),
    hinted_binding(
        Context::Runs,
        CLOSE,
        Action::Close,
        HintSpec::Static {
            key: "esc",
            label: "back",
        },
    ),
];

/// Resolve one key in one input context.
#[must_use]
pub fn resolve(context: Context, event: KeyEvent) -> Option<Action> {
    BINDINGS
        .iter()
        .find(|binding| {
            binding.context == context && binding.keys.iter().any(|key| key.matches(event))
        })
        .map(|binding| binding.action)
}

/// Footer hints come from the same contextual entries that resolve input.
#[must_use]
pub fn hints(context: Context, state: &AppState) -> Vec<Hint> {
    let mut hints: Vec<(usize, Hint)> = BINDINGS
        .iter()
        .filter(|binding| binding.context == context)
        .filter_map(|binding| {
            binding
                .hint
                .and_then(|hint| hint.visible(state))
                .map(|hint| (hint_order(context, binding.action), hint))
        })
        .collect();
    hints.sort_by_key(|(order, _)| *order);
    hints.into_iter().map(|(_, hint)| hint).collect()
}

fn hint_order(context: Context, action: Action) -> usize {
    let actions: &[Action] = match context {
        Context::Explorer => &[
            Action::NextTab,
            Action::ToggleSelection,
            Action::TerminalSelectionHelp,
            Action::Reload,
            Action::RefreshDrift,
            Action::Ping,
            Action::Reboot,
            Action::Deploy,
            Action::Quit,
            Action::ToggleMcp,
        ],
        Context::Deploy => &[
            Action::BuildTab,
            Action::PlaybookTab,
            Action::SummaryTab,
            Action::NextTab,
            Action::Close,
            Action::ArmTerminate,
        ],
        Context::DeployKillArmed => &[Action::ConfirmTerminate, Action::Cancel],
        Context::Runs => &[
            Action::Activate,
            Action::MoveUp,
            Action::RefreshRuns,
            Action::Close,
        ],
        _ => &[Action::Close],
    };
    actions
        .iter()
        .position(|candidate| *candidate == action)
        .unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    #[test]
    fn no_binding_is_shadowed_within_a_context() {
        for context in CONTEXTS {
            let mut seen = BTreeSet::new();
            for binding in BINDINGS.iter().filter(|binding| binding.context == context) {
                for pattern in binding.keys {
                    let identity = format!("{:?}+{:?}", pattern.modifiers, pattern.code);
                    assert!(
                        seen.insert(identity.clone()),
                        "shadowed binding in {context:?}: {identity}"
                    );
                }
            }
        }
    }

    #[test]
    fn arrows_and_hjkl_share_navigation_actions() {
        for context in [
            Context::Explorer,
            Context::Task,
            Context::AttachedLog,
            Context::Deploy,
            Context::Runs,
        ] {
            assert_eq!(
                resolve(context, key(KeyCode::Up)),
                resolve(context, key(KeyCode::Char('k')))
            );
            assert_eq!(
                resolve(context, key(KeyCode::Down)),
                resolve(context, key(KeyCode::Char('j')))
            );
        }
        for context in [Context::Explorer, Context::Deploy] {
            assert_eq!(
                resolve(context, key(KeyCode::Left)),
                resolve(context, key(KeyCode::Char('h')))
            );
            assert_eq!(
                resolve(context, key(KeyCode::Right)),
                resolve(context, key(KeyCode::Char('l')))
            );
        }
    }

    #[test]
    fn footer_hints_are_live_bindings_with_contextual_visibility() {
        let mut state = AppState::new();
        assert_eq!(
            hints(Context::Explorer, &state)
                .iter()
                .map(|hint| hint.key)
                .collect::<Vec<_>>(),
            ["tab", "space", "shift+drag", "r", "S", "p", "R", "D", "q"]
        );
        state.debug_mcp = true;
        assert_eq!(
            hints(Context::Explorer, &state).last().map(|hint| hint.key),
            Some("m")
        );
        assert!(
            hints(Context::Explorer, &state)
                .iter()
                .any(|hint| hint.key == "m")
        );

        let mut deploy = crate::screen::DeployViewState::new("alpha", false, false, false, true);
        state.screen = Some(ScreenState::Deploy(Box::new(deploy.clone())));
        assert_eq!(
            hints(Context::Deploy, &state)
                .iter()
                .map(|hint| hint.key)
                .collect::<Vec<_>>(),
            ["b", "p", "tab", "esc", "ctrl-k"]
        );
        deploy.sync(None, &[], true, Some(0), 1);
        state.screen = Some(ScreenState::Deploy(Box::new(deploy.clone())));
        assert_eq!(
            hints(Context::Deploy, &state)
                .iter()
                .map(|hint| hint.key)
                .collect::<Vec<_>>(),
            ["b", "p", "s", "tab", "esc"]
        );
        deploy.kill_armed = true;
        state.screen = Some(ScreenState::Deploy(Box::new(deploy)));
        assert_eq!(
            hints(Context::DeployKillArmed, &state),
            [
                Hint {
                    key: "y",
                    label: "TERMINATE the run"
                },
                Hint {
                    key: "any other key",
                    label: "cancel"
                }
            ]
        );

        for context in CONTEXTS {
            for binding in BINDINGS
                .iter()
                .filter(|binding| binding.context == context && binding.hint.is_some())
            {
                if let Some(pattern) = binding.keys.first() {
                    let event = KeyEvent::new(pattern.code, pattern.modifiers);
                    assert_eq!(resolve(context, event), Some(binding.action));
                }
            }
        }
    }
}
