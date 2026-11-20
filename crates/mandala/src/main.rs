//! mandala — the fleet porcelain binary (CLI + stdio MCP server + TUI).
//!
//! The public `mandala` binary: it assembles the `mandala-core` CLI (the
//! fleet-generic root views) and registers only the fleet-generic built-in
//! engines — `deploy` and `ansible`. A downstream operator binary
//! links `mandala-core` and registers its own engines (flux, terraform) on top,
//! sharing the same in-process inventory (see the `fleet-cli` spec).
//!
//! The `mcp` and `tui` subcommands are wired here (not in `mandala-core`)
//! because they drive `mandala-mcp` / `mandala-tui`, and `mandala-core` must
//! not depend on either — the launcher closures keep those edges out of the
//! library.

use std::process::ExitCode;

use mandala_core::Cli;
use mandala_core::engines::{ansible, deploy};

fn main() -> ExitCode {
    // SAFETY: first operation at executable entry, before any threads or other
    // environment readers. This also clears the handoff before spawning children.
    unsafe { mandala_core::supervisor::capture_at_entry() };
    Cli::new()
        .mcp_launcher_with_options(
            |flake, root| match mandala_mcp::serve_stdio_blocking_with_root(
                flake,
                root.map(std::path::Path::to_path_buf),
            ) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("mandala mcp: {err}");
                    ExitCode::FAILURE
                }
            },
        )
        .tui_launcher(mandala_tui::launch_blocking)
        .register(deploy::engine())
        .register(ansible::engine())
        .run()
}
