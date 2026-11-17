//! This integration test is a separate crate: non_exhaustive is enforced here.

use std::process::ExitCode;

use mandala_core::{Cli, TuiRequest};
use mandala_tui::deploy::DeployConfig;
use mandala_tui::explorer::ExplorerConfig;

#[test]
fn external_configs_use_constructors_and_optional_overrides() {
    let mut deploy = DeployConfig::new("fleet", "@web");
    assert_eq!(deploy.flake, "fleet");
    assert_eq!(deploy.limit, "@web");
    assert!(!deploy.dry_activate);
    assert!(!deploy.boot);
    assert!(!deploy.halt_on_build_failure);
    assert_eq!(deploy.throttle, 4);
    assert_eq!(deploy.cache_push, None);
    assert_eq!(deploy.program, None);
    deploy.cache_push = Some(false);
    assert_eq!(deploy.cache_push, Some(false));

    let mut explorer = ExplorerConfig::for_flake("fleet");
    explorer.debug_mcp = true;
    assert_eq!(explorer.flake, "fleet");
    assert!(explorer.debug_mcp);
}

#[test]
fn external_requests_use_constructors_and_open_patterns() {
    let explorer = TuiRequest::explorer(true);
    assert!(matches!(
        explorer,
        TuiRequest::Explorer {
            debug_mcp: true,
            ..
        }
    ));
    let mut deploy = TuiRequest::deploy("@web");
    if let TuiRequest::Deploy { cache_push, .. } = &mut deploy {
        *cache_push = Some(false);
    }
    assert!(
        matches!(deploy, TuiRequest::Deploy { limit, cache_push: Some(false), throttle: 4, .. } if limit == "@web")
    );
    assert!(
        matches!(TuiRequest::attach("run"), TuiRequest::Attach { run_id, .. } if run_id == "run")
    );
}

#[test]
fn request_constructor_defaults_match_cli_defaults() {
    let cli = Cli::new().tui_launcher(|flake, request| {
        assert_eq!(flake, ".");
        assert_eq!(request, TuiRequest::deploy("@web"));
        ExitCode::SUCCESS
    });
    assert_eq!(
        cli.run_from(["mandala", "tui", "deploy", "-l", "@web"]),
        ExitCode::SUCCESS
    );
}

#[test]
fn external_bin_registers_the_shared_launcher_without_matching_requests() {
    // Merely building this CLI checks exactly the downstream registration API.
    // No terminal or external command is launched by this test.
    let engine = mandala_core::engines::deploy::engine();
    assert_eq!(engine.name(), "deploy");
    let cli = Cli::new()
        .mcp_launcher(|_flake| ExitCode::SUCCESS)
        .tui_launcher(mandala_tui::launch_blocking)
        .register(engine);
    assert_eq!(cli.run_from(["mandala", "version"]), ExitCode::SUCCESS);
}

#[test]
fn shared_launcher_rejects_invalid_attach_without_opening_a_terminal() {
    assert_eq!(
        mandala_tui::launch_blocking(".", TuiRequest::attach("../invalid")),
        ExitCode::FAILURE
    );
}
