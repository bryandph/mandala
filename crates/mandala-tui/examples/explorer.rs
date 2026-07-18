//! Manual smoke for the explorer tier: from `flakes/mandala` (or any fleet
//! checkout),
//!
//! ```text
//! nix develop --impure -c cargo run -p mandala-tui --example explorer -- --flake .
//! ```
//!
//! tab/1-3 switch views, space/shift+↑↓ select, ctrl+↑↓ skip, esc clears,
//! r reloads, S refreshes drift (survey + eval), ctrl-z suspends, q quits.
//! Set `MANDALA_AGGREGATE_FILE` to smoke against a fixture without a fleet.
//!
//! An example (not a bin target) on purpose: the nix package installs bins,
//! and this must never ship in $out — the shipped entry is `mandala tui`
//! (flipped to native in section 7).

use std::io;

use mandala_tui::explorer::{ExplorerConfig, run_explorer};

#[tokio::main(flavor = "current_thread")]
async fn main() -> io::Result<()> {
    let mut flake = ".".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--flake" => flake = args.next().unwrap_or_else(|| ".".to_string()),
            other => {
                eprintln!("unknown argument: {other} (usage: explorer [--flake <ref>])");
                std::process::exit(2);
            }
        }
    }
    run_explorer(ExplorerConfig::for_flake(flake)).await
}
