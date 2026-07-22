//! One explicit TUI palette and named-style layer.

use mandala_core::drift::DriftStatus;
use nix_build_forest::ForestStyles;
use ratatui::style::{Color, Modifier, Style};

use mandala_core::runner::HostState;

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub foreground: Color,
    pub muted: Color,
    pub accent: Color,
    pub focus: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub info: Color,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub palette: Palette,
    pub header: Style,
    pub footer_key: Style,
    pub footer_label: Style,
    pub chrome: Style,
    pub focused_chrome: Style,
    pub modal: Style,
    pub status: Style,
    pub status_live: Style,
    pub status_error: Style,
    pub selection: Style,
}

impl Default for Theme {
    fn default() -> Self {
        let palette = Palette {
            foreground: Color::Reset,
            muted: Color::DarkGray,
            accent: Color::Cyan,
            focus: Color::LightCyan,
            success: Color::Green,
            warning: Color::Yellow,
            error: Color::Red,
            info: Color::Blue,
        };
        Self {
            palette,
            header: Style::new().fg(palette.focus).add_modifier(Modifier::BOLD),
            footer_key: Style::new().fg(palette.focus).add_modifier(Modifier::BOLD),
            footer_label: Style::new().fg(palette.muted).add_modifier(Modifier::DIM),
            chrome: Style::new().fg(palette.muted),
            focused_chrome: Style::new().fg(palette.focus).add_modifier(Modifier::BOLD),
            modal: Style::new().fg(palette.accent),
            status: Style::new().fg(palette.foreground),
            status_live: Style::new().fg(palette.accent),
            status_error: Style::new().fg(palette.error).add_modifier(Modifier::BOLD),
            selection: Style::new().fg(palette.accent).add_modifier(Modifier::BOLD),
        }
    }
}

impl Theme {
    #[must_use]
    pub fn rich_style(&self, spec: &str) -> Option<Style> {
        let mut style = Style::new();
        for token in spec.split_whitespace() {
            style = match token {
                "bold" => style.add_modifier(Modifier::BOLD),
                "dim" => style.add_modifier(Modifier::DIM),
                "green" => style.fg(self.palette.success),
                "red" => style.fg(self.palette.error),
                "yellow" => style.fg(self.palette.warning),
                "magenta" => style.fg(Color::Magenta),
                "cyan" => style.fg(self.palette.accent),
                "blue" => style.fg(self.palette.info),
                _ => return None,
            };
        }
        Some(style)
    }

    #[must_use]
    pub fn drift_status(&self, status: DriftStatus) -> Style {
        self.rich_style(status.style())
            .unwrap_or_else(|| panic!("unmapped style token in {:?}", status.style()))
    }

    #[must_use]
    pub fn host_state(&self, state: HostState) -> Style {
        let spec = match state {
            HostState::Pending => "dim",
            HostState::Evaluating | HostState::Building => "cyan",
            HostState::Copying => "blue",
            HostState::Activating | HostState::Waiting => "yellow",
            HostState::Confirmed => "green",
            HostState::RolledBack | HostState::Failed => "bold red",
        };
        self.rich_style(spec)
            .unwrap_or_else(|| panic!("unmapped host-state style {spec:?}"))
    }

    #[must_use]
    pub fn forest(&self) -> ForestStyles {
        ForestStyles {
            unknown: Style::new().fg(self.palette.muted),
            planned: Style::new().fg(self.palette.foreground),
            building: Style::new().fg(self.palette.warning),
            transfer: Style::new().fg(self.palette.accent),
            built: Style::new().fg(self.palette.success),
            failed: Style::new().fg(self.palette.error),
            summary: self.footer_label,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_style_vocabularies_are_mapped_in_one_theme() {
        let theme = Theme::default();
        for status in DriftStatus::ALL {
            assert!(theme.rich_style(status.style()).is_some());
        }
        for host in [
            HostState::Pending,
            HostState::Evaluating,
            HostState::Building,
            HostState::Copying,
            HostState::Activating,
            HostState::Waiting,
            HostState::Confirmed,
            HostState::RolledBack,
            HostState::Failed,
        ] {
            let _ = theme.host_state(host);
        }
        assert!(theme.rich_style("bold chartreuse").is_none());
    }
}
