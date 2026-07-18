//! Manual smoke for the standalone deploy runner: from a FLEET checkout,
//!
//! ```text
//! nix develop --impure -c cargo run -p mandala-tui --example deploy -- \
//!     -l <selector> [--dry-activate] [--throttle N] [--flake <ref>]
//! ```
//!
//! The selector resolves through `Inventory::to_limit` (canonical
//! resolution) before the terminal is touched; the process exit code is the
//! run's rc, or 0 on operator cancel. b/p/s jump tabs, tab cycles, esc
//! terminates a running deploy then exits.
//!
//! An example (not a bin target) on purpose: the nix package installs bins,
//! and this must never ship in $out — the shipped entry is
//! `mandala tui deploy` (flipped to native in section 7).

use mandala_tui::deploy::{DeployConfig, run_deploy};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut cfg = DeployConfig {
        flake: ".".to_string(),
        limit: String::new(),
        dry_activate: false,
        throttle: 4,
        program: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-l" | "--limit" => cfg.limit = args.next().unwrap_or_default(),
            "--dry-activate" => cfg.dry_activate = true,
            "--throttle" => {
                cfg.throttle = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(cfg.throttle);
            }
            "--flake" => cfg.flake = args.next().unwrap_or_else(|| ".".to_string()),
            other => {
                eprintln!(
                    "unknown argument: {other} (usage: deploy -l <sel> [--dry-activate] [--throttle N] [--flake <ref>])"
                );
                std::process::exit(2);
            }
        }
    }
    if cfg.limit.is_empty() {
        eprintln!("missing -l <selector>");
        std::process::exit(2);
    }
    match run_deploy(cfg).await {
        Ok(code) => std::process::exit(i32::try_from(code).unwrap_or(1)),
        Err(e) => {
            eprintln!("deploy failed: {e}");
            std::process::exit(1);
        }
    }
}
