//! mandala — the fleet porcelain binary (CLI + stdio MCP server + TUI).
//!
//! The public `mandala` binary: it assembles the `mandala-core` CLI (the
//! fleet-generic root views) and registers only the fleet-generic built-in
//! engines — `deploy` and `ansible`. A downstream operator binary (mandala-bph)
//! links `mandala-core` and registers its own engines (flux, terraform) on top,
//! sharing the same in-process inventory (see the `fleet-cli` spec).
//!
//! The `mcp` and `tui` subcommands are wired here (not in `mandala-core`)
//! because they drive `mandala-mcp` / `mandala-tui`, and `mandala-core` must
//! not depend on either — the launcher closures keep those edges out of the
//! library.

use std::process::ExitCode;

use mandala_core::engines::{ansible, deploy};
use mandala_core::{Cli, TuiRequest};
use mandala_tui::deploy::{DeployConfig, run_deploy_blocking};
use mandala_tui::explorer::{ExplorerConfig, run_explorer_blocking};

fn main() -> ExitCode {
    Cli::new()
        .mcp_launcher(|flake| match mandala_mcp::serve_stdio_blocking(flake) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("mandala mcp: {err}");
                ExitCode::FAILURE
            }
        })
        .tui_launcher(|flake, request| match request {
            TuiRequest::Explorer { debug_mcp } => {
                let mut cfg = ExplorerConfig::for_flake(flake);
                cfg.debug_mcp = debug_mcp;
                match run_explorer_blocking(cfg) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(err) => {
                        eprintln!("mandala tui: {err}");
                        ExitCode::FAILURE
                    }
                }
            }
            TuiRequest::Deploy {
                limit,
                dry_activate,
                throttle,
            } => {
                let cfg = DeployConfig {
                    flake: flake.to_string(),
                    limit,
                    dry_activate,
                    throttle,
                    program: None,
                };
                match run_deploy_blocking(cfg) {
                    // The process exit code IS the run's rc (0 on operator
                    // cancel); an out-of-range rc collapses to 1.
                    Ok(rc) => ExitCode::from(u8::try_from(rc).unwrap_or(1)),
                    Err(err) => {
                        eprintln!("mandala tui deploy: {err}");
                        ExitCode::FAILURE
                    }
                }
            }
        })
        .register(deploy::engine())
        .register(ansible::engine())
        .run()
}
