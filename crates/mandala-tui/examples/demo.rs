//! Manual smoke for the harness spike: `cargo run --example demo` inside
//! the devshell. j/k move, s runs a fake job (spinner), ctrl-z suspends,
//! q quits. An example (not a bin target) on purpose: the nix package
//! installs bins, and this must never ship in $out.

use std::io;

use crossterm::event::EventStream;
use mandala_tui::app::App;
use mandala_tui::state::AppState;
use mandala_tui::term::{TerminalGuard, install_panic_hook};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

#[tokio::main(flavor = "current_thread")]
async fn main() -> io::Result<()> {
    install_panic_hook();
    let guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut app = App::new(AppState::demo());
    app.guard = Some(guard);
    let mut events = EventStream::new();
    let result = app.run(&mut terminal, &mut events).await;
    drop(app); // restores the terminal via the guard
    result
}
