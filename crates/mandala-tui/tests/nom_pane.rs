//! Nom-pane spike (task 1.3), proven headlessly.
//!
//! The pty+emulator+widget path runs against a stand-in `sh` script (always
//! available, including the nix check-phase sandbox); the real `nom` is
//! exercised by [`fixture_renders_through_real_nom`] whenever nom is on
//! PATH (it is in the devshell via ans-cli; the nix sandbox skips it).
//! The checked-in fixture is a captured `@nix` internal-json line sequence
//! from a real `nix build --log-format internal-json` of a trivial
//! derivation.

use std::time::{Duration, Instant};

use mandala_tui::nom_pane::NomPane;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};

const FIXTURE: &str = include_str!("fixtures/nix-internal-json.txt");

fn draw(pane: &NomPane, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal
        .draw(|frame| frame.render_widget(pane, frame.area()))
        .expect("render pane");
    terminal
}

fn wait_for_contents(pane: &NomPane, needle: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let contents = pane.screen_contents().unwrap_or_default();
        if contents.contains(needle) {
            return contents;
        }
        assert!(
            Instant::now() < deadline,
            "emulated screen never showed {needle:?}; last contents:\n{contents}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Load-bearing fallback (nom.py parity): an absent binary yields a dim
/// notice, no crash, and feeding/finishing stays inert.
#[test]
fn absent_command_falls_back_to_a_dim_notice() {
    let mut pane = NomPane::new();
    pane.feed(r#"@nix {"action":"msg","level":3,"msg":"early line"}"#);
    pane.spawn_cmd("mandala-definitely-not-a-binary", &[], 12, 60);
    assert!(pane.failure().is_some_and(|n| n.contains("unavailable")));
    // Feeding and finishing after failure must be no-ops, not panics.
    pane.feed("@nix {}");
    pane.finish();

    let terminal = draw(&pane, 60, 6);
    let buf = terminal.backend().buffer();
    let text = format!("{}", terminal.backend());
    assert!(text.contains("unavailable"), "notice missing:\n{text}");
    let style = buf.cell((0, 0)).expect("notice cell").style();
    assert!(
        style.add_modifier.contains(Modifier::DIM),
        "notice must be dim"
    );
}

/// The whole pipeline against a stand-in: pending-buffer before spawn,
/// live feed after, EOF-to-finish, ANSI color surviving pty → vt100 →
/// ratatui buffer.
#[test]
fn stand_in_pipeline_renders_colored_lines() {
    let script =
        r#"while IFS= read -r l; do printf '\033[31m%s\033[0m\r\n' "$l"; done; printf 'ALL DONE'"#;
    let mut pane = NomPane::new();
    // Fed BEFORE spawn: must buffer and flush (nom.py pending parity).
    pane.feed("pre-spawn-line");
    pane.spawn_cmd("sh", &["-c", script], 12, 60);
    assert!(
        pane.failure().is_none(),
        "stand-in failed: {:?}",
        pane.failure()
    );
    pane.feed("post-spawn-line");
    // EOF ends the read loop; the stand-in then prints its "summary".
    pane.finish();

    let contents = wait_for_contents(&pane, "ALL DONE", Duration::from_secs(5));
    assert!(
        contents.contains("pre-spawn-line"),
        "pending line lost:\n{contents}"
    );
    assert!(
        contents.contains("post-spawn-line"),
        "live line lost:\n{contents}"
    );
    assert!(pane.take_dirty(), "pty output must mark the pane dirty");

    let terminal = draw(&pane, 60, 12);
    let text = format!("{}", terminal.backend());
    assert!(
        text.contains("pre-spawn-line") && text.contains("ALL DONE"),
        "buffer:\n{text}"
    );
    // The SGR red survived emulation into the ratatui buffer (named or
    // indexed depending on the emulator's color model).
    let buf = terminal.backend().buffer();
    let style = buf.cell((0, 0)).expect("first cell").style();
    assert!(
        matches!(style.fg, Some(Color::Red) | Some(Color::Indexed(1))),
        "expected red fg, got {:?}",
        style.fg
    );
}

/// Resize propagates to the pty (TIOCSWINSZ, read back via TIOCGWINSZ) and
/// the emulator screen; SIGWINCH goes to the child (unobservable here
/// beyond not erroring — nom redraws on it in the manual smoke).
#[test]
fn resize_propagates_to_pty_and_emulator() {
    let mut pane = NomPane::new();
    pane.spawn_cmd("sh", &["-c", "while IFS= read -r l; do :; done"], 12, 60);
    assert!(pane.failure().is_none());
    assert_eq!(pane.pty_size(), Some((12, 60)));

    assert_eq!(pane.emulator_size(), Some((12, 60)));

    pane.resize(20, 100);
    assert_eq!(pane.pty_size(), Some((20, 100)));
    assert_eq!(pane.emulator_size(), Some((20, 100)));
    pane.finish();
    // Dropping the pane terminates the child (unmount parity) — covered by
    // Drop; nothing to assert beyond "no hang".
}

/// The captured internal-json fixture through the REAL nom, when present
/// (devshell: yes, via ans-cli; nix sandbox: skipped). The manual analog
/// was also run interactively: `nom --json < fixture` draws the green
/// checkmark dependency tree.
#[test]
fn fixture_renders_through_real_nom() {
    if std::process::Command::new("nom")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: nom not on PATH (nix sandbox) — covered by the stand-in test");
        return;
    }
    let mut pane = NomPane::new();
    pane.spawn_cmd("nom", &["--json"], 15, 100);
    assert!(
        pane.failure().is_none(),
        "nom spawn failed: {:?}",
        pane.failure()
    );
    for line in FIXTURE.lines() {
        pane.feed(line);
    }
    pane.finish();

    // nom's final summary names the derivation the fixture built.
    let contents = wait_for_contents(&pane, "mandala-nom-fixture", Duration::from_secs(10));
    assert!(!contents.trim().is_empty());

    let terminal = draw(&pane, 100, 15);
    let text = format!("{}", terminal.backend());
    assert!(text.contains("mandala-nom-fixture"), "buffer:\n{text}");
}
