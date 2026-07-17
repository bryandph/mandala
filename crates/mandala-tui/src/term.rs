//! Terminal lifecycle discipline: raw-mode + alternate-screen guard,
//! panic-hook restore, suspend-to-shell.
//!
//! The rule (design "Risks"): the terminal is restored BEFORE any panic
//! message prints, and restoring twice is harmless — the guard's Drop and
//! the panic hook may both fire.

use std::io;

use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use nix::sys::signal::{Signal, raise};

/// Best-effort global restore: leave the alternate screen, drop raw mode.
/// Safe to call any number of times, from the panic hook or a guard.
pub fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
}

/// Chain a terminal-restoring panic hook in front of the default one, so
/// the panic message prints onto a sane screen instead of the alternate
/// buffer. Install once, before entering the terminal.
pub fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        original(info);
    }));
}

/// Raw-mode + alternate-screen guard. Restores on drop (idempotently — the
/// panic hook may already have restored).
#[derive(Debug)]
pub struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self { active: true })
    }

    /// Leave the managed screen (idempotent).
    pub fn exit(&mut self) {
        if self.active {
            restore();
            self.active = false;
        }
    }

    fn reenter(&mut self) -> io::Result<()> {
        if !self.active {
            enable_raw_mode()?;
            execute!(io::stdout(), EnterAlternateScreen)?;
            self.active = true;
        }
        Ok(())
    }

    /// Ctrl-Z discipline: restore the terminal, stop ourselves (the shell
    /// gets its prompt back), and re-enter when the operator resumes us.
    /// Execution continues past `raise` on SIGCONT.
    pub fn suspend_to_shell(&mut self) -> io::Result<()> {
        self.exit();
        let _ = raise(Signal::SIGTSTP);
        self.reenter()
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.exit();
    }
}
