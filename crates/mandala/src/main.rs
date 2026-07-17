//! mandala — the fleet porcelain binary (CLI + stdio MCP server).
//!
//! The public `mandala` binary: it assembles the `mandala-core` CLI (the
//! fleet-generic root views) and registers only the fleet-generic built-in
//! engines — `deploy` and `ansible`. A downstream operator binary (mandala-bph)
//! links `mandala-core` and registers its own engines (flux, terraform) on top,
//! sharing the same in-process inventory (see the `fleet-cli` spec).
//!
//! The `mcp` subcommand is wired here (not in `mandala-core`) because it drives
//! `mandala-mcp`, and `mandala-core` must not depend on `mandala-mcp` — the
//! launcher closure keeps that edge out of the library.

use std::process::ExitCode;

use mandala_core::Cli;
use mandala_core::engines::{ansible, deploy};

fn main() -> ExitCode {
    Cli::new()
        .mcp_launcher(|flake| match mandala_mcp::serve_stdio_blocking(flake) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("mandala mcp: {err}");
                ExitCode::FAILURE
            }
        })
        .register(deploy::engine())
        .register(ansible::engine())
        .run()
}
