//! Stable CLI-to-TUI dispatch, owned by the crate that knows the runtime settings.

use std::process::ExitCode;

use mandala_core::TuiRequest;

use crate::deploy::{DeployConfig, run_attach_blocking, run_deploy_blocking};
use crate::explorer::{ExplorerConfig, run_explorer_blocking};

/// Launch a parsed CLI request, preserving the CLI's diagnostics and exit codes.
///
/// Pass this directly to [`mandala_core::Cli::tui_launcher`]. Downstream binaries
/// need neither an exhaustive request match nor field-by-field config plumbing
/// when Mandala adds a TUI option or mode. Low-level launch functions remain
/// available for callers that want to configure a particular screen themselves.
pub fn launch_blocking(flake: &str, request: TuiRequest) -> ExitCode {
    launch_with(
        flake,
        request,
        run_explorer_blocking,
        run_deploy_blocking,
        run_attach_blocking,
    )
}

// Keep field forwarding testable without a terminal or subprocesses.
fn launch_with(
    flake: &str,
    request: TuiRequest,
    explorer: impl FnOnce(ExplorerConfig) -> std::io::Result<()>,
    deploy: impl FnOnce(DeployConfig) -> std::io::Result<i64>,
    attach: impl FnOnce(String, String) -> std::io::Result<i64>,
) -> ExitCode {
    match request {
        TuiRequest::Explorer { debug_mcp, .. } => {
            let mut cfg = ExplorerConfig::for_flake(flake);
            cfg.debug_mcp = debug_mcp;
            match explorer(cfg) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("mandala tui: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        TuiRequest::Deploy {
            limit,
            dry_activate,
            boot,
            halt_on_build_failure,
            throttle,
            cache_push,
            ..
        } => {
            let mut cfg = DeployConfig::new(flake, limit);
            cfg.dry_activate = dry_activate;
            cfg.boot = boot;
            cfg.halt_on_build_failure = halt_on_build_failure;
            cfg.throttle = throttle;
            cfg.cache_push = cache_push;
            run_code("mandala tui deploy", deploy(cfg))
        }
        TuiRequest::Attach { run_id, .. } => {
            run_code("mandala tui attach", attach(flake.to_string(), run_id))
        }
        // mandala-core is a separate crate: a newly introduced mode is refused
        // until this dispatcher learns to launch it, not silently mislaunched.
        _ => {
            eprintln!("mandala tui: unsupported request (update mandala-tui)");
            ExitCode::FAILURE
        }
    }
}

fn run_code(label: &str, result: std::io::Result<i64>) -> ExitCode {
    match result {
        // The exit code is the run's rc (0 on operator detach); an
        // out-of-range rc collapses to 1, just as in the original CLI.
        Ok(rc) => ExitCode::from(u8::try_from(rc).unwrap_or(1)),
        Err(e) => {
            eprintln!("{label}: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_all_deploy_options_and_preserves_run_rc() {
        let mut request = TuiRequest::deploy("@web");
        if let TuiRequest::Deploy {
            dry_activate,
            boot,
            halt_on_build_failure,
            cache_push,
            throttle,
            ..
        } = &mut request
        {
            *dry_activate = true;
            *boot = true;
            *halt_on_build_failure = true;
            *cache_push = Some(false);
            *throttle = 7;
        }
        let rc = launch_with(
            "fleet",
            request,
            |_| panic!("wrong screen"),
            |cfg| {
                assert_eq!(cfg.flake, "fleet");
                assert_eq!(cfg.limit, "@web");
                assert!(cfg.dry_activate && cfg.boot && cfg.halt_on_build_failure);
                assert_eq!(cfg.cache_push, Some(false));
                assert_eq!(cfg.throttle, 7);
                assert_eq!(cfg.program, None);
                Ok(42)
            },
            |_, _| panic!("wrong screen"),
        );
        assert_eq!(rc, ExitCode::from(42));
    }

    #[test]
    fn forwards_explorer_debug_flag_and_attach_identity() {
        assert_eq!(
            launch_with(
                "fleet",
                TuiRequest::explorer(true),
                |cfg| {
                    assert_eq!(cfg.flake, "fleet");
                    assert!(cfg.debug_mcp);
                    Ok(())
                },
                |_| panic!("wrong screen"),
                |_, _| panic!("wrong screen"),
            ),
            ExitCode::SUCCESS,
        );
        assert_eq!(
            launch_with(
                "fleet",
                TuiRequest::attach("run"),
                |_| panic!("wrong screen"),
                |_| panic!("wrong screen"),
                |flake, run| {
                    assert_eq!(flake, "fleet");
                    assert_eq!(run, "run");
                    Ok(0)
                },
            ),
            ExitCode::SUCCESS,
        );
    }

    #[test]
    fn preserves_error_and_out_of_range_exit_codes() {
        assert_eq!(run_code("test", Ok(255)), ExitCode::from(255));
        assert_eq!(run_code("test", Ok(256)), ExitCode::FAILURE);
        assert_eq!(run_code("test", Ok(-1)), ExitCode::FAILURE);
        assert_eq!(
            run_code("test", Err(std::io::Error::other("stub"))),
            ExitCode::FAILURE
        );
        assert_eq!(
            launch_with(
                ".",
                TuiRequest::explorer(false),
                |_| Err(std::io::Error::other("stub")),
                |_| panic!("wrong screen"),
                |_, _| panic!("wrong screen"),
            ),
            ExitCode::FAILURE,
        );
    }
}
