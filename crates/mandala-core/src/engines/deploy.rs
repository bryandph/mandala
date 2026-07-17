//! Built-in `deploy` engine: dispatch onto the `mandala.fleet` fan-out.
//!
//! Parity port of `cli/src/mandala_fleet/engines/deploy.py`. `run` shells out to
//! the fan-out playbook (eval-once batch build + per-host deploy-rs); `batch`
//! builds a `deployBatch` group artifact for cache warming; `nodes` lists the
//! deploy-rs node names off the aggregate. Dispatch + present only — deploy-rs
//! and the `mandala.fleet` collection remain the machinery.

use std::process::{Command as ProcCommand, ExitCode};

use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};

use crate::cli::Engine;
use crate::inventory::{Inventory, InventoryError};

/// The `deploy` engine registration (name + subcommand tree + handler).
#[must_use]
pub fn engine() -> Engine {
    let command = Command::new("deploy")
        .about("Fan-out deploys via deploy-rs + mandala.fleet")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("run")
                .about("Run the eval-once + fan-out deploy (mandala.fleet.deploy)")
                .arg(
                    Arg::new("limit")
                        .short('l')
                        .long("limit")
                        .required(true)
                        .help("Selector: @group (expanded to the projected members), member, or comma-list"),
                )
                .arg(
                    Arg::new("dry-activate")
                        .long("dry-activate")
                        .action(ArgAction::SetTrue)
                        .help("Build + copy but do not activate"),
                )
                .arg(
                    Arg::new("throttle")
                        .long("throttle")
                        .value_parser(value_parser!(i64))
                        .default_value("4")
                        .help("Per-host deploy parallelism"),
                )
                .arg(
                    Arg::new("events-dir")
                        .long("events-dir")
                        .help("Opt into the JSONL event channel (MANDALA_FLEET_EVENTS)"),
                ),
        )
        .subcommand(
            Command::new("batch")
                .about("Build a group's eval-once batch artifact (.#deployBatch.<group>)")
                .arg(
                    Arg::new("group")
                        .required(true)
                        .help("deployBatch group key (taxonomy spelling)"),
                ),
        )
        .subcommand(
            Command::new("nodes").about("List deploy-rs node names (from the aggregate's deploy projection)"),
        );
    Engine::new(command, run)
}

/// The argv for `deploy run` — the fan-out playbook invocation. Pure so tests
/// assert the command line without spawning ansible. Parity with the Python
/// `engines/deploy.py::run` argv (selector canonicalized via `to_limit`).
///
/// # Errors
/// Propagates [`Inventory::to_limit`] (unknown member/group, empty selector).
pub fn run_argv(
    inv: &Inventory,
    limit: &str,
    throttle: i64,
    dry_activate: bool,
) -> Result<Vec<String>, InventoryError> {
    let mut argv = vec![
        "ansible-playbook".to_string(),
        "mandala.fleet.deploy".to_string(),
        "-l".to_string(),
        inv.to_limit(limit)?,
        "-e".to_string(),
        format!("deploy_throttle={throttle}"),
    ];
    if dry_activate {
        argv.push("-e".to_string());
        argv.push("deploy_dry_activate=true".to_string());
    }
    Ok(argv)
}

/// The argv for `deploy batch` — the group's eval-once artifact build. Pure so
/// tests assert the command line without spawning nix. Parity with the Python
/// `engines/deploy.py::batch` (group validated against `all` or a known group).
///
/// # Errors
/// [`InventoryError::NoSuchGroup`] if `group` is neither `all` nor a known
/// group (the Python `InventoryError(f"no such group: {group}")`).
pub fn batch_argv(
    inv: &Inventory,
    flake: &str,
    group: &str,
) -> Result<Vec<String>, InventoryError> {
    if group != "all" && !inv.groups().contains_key(group) {
        return Err(InventoryError::NoSuchGroup(group.to_string()));
    }
    Ok(vec![
        "nix".to_string(),
        "build".to_string(),
        "--no-link".to_string(),
        "--print-out-paths".to_string(),
        format!("{flake}#deployBatch.{group}"),
    ])
}

/// Dispatch the `deploy` engine's subcommand.
fn run(inv: &Inventory, m: &ArgMatches) -> ExitCode {
    // `--flake` is a global root arg, reachable through the engine's matches.
    let flake = m.get_one::<String>("flake").map_or(".", String::as_str);
    match m.subcommand() {
        Some(("run", sm)) => {
            let limit = sm.get_one::<String>("limit").map_or("", String::as_str);
            let throttle = *sm.get_one::<i64>("throttle").unwrap_or(&4);
            let dry_activate = sm.get_flag("dry-activate");
            let argv = match run_argv(inv, limit, throttle, dry_activate) {
                Ok(a) => a,
                Err(err) => {
                    eprintln!("mandala: {err}");
                    return ExitCode::FAILURE;
                }
            };
            let mut cmd = ProcCommand::new(&argv[0]);
            cmd.args(&argv[1..]);
            if let Some(dir) = sm.get_one::<String>("events-dir") {
                cmd.env("MANDALA_FLEET_EVENTS", dir);
            }
            spawn_status(cmd)
        }
        Some(("batch", sm)) => {
            let group = sm.get_one::<String>("group").map_or("", String::as_str);
            let argv = match batch_argv(inv, flake, group) {
                Ok(a) => a,
                Err(err) => {
                    eprintln!("mandala: {err}");
                    return ExitCode::FAILURE;
                }
            };
            let mut cmd = ProcCommand::new(&argv[0]);
            cmd.args(&argv[1..]);
            spawn_status(cmd)
        }
        Some(("nodes", _)) => {
            let mut nodes = inv.deploy_nodes();
            nodes.sort();
            for name in nodes {
                println!("{name}");
            }
            ExitCode::SUCCESS
        }
        // `subcommand_required` guarantees a matched arm.
        _ => ExitCode::from(2),
    }
}

/// Spawn a child, wait, and yield its exit code (Python `raise typer.Exit(
/// subprocess.run(...).returncode)`). A spawn failure is a hard error.
fn spawn_status(mut cmd: ProcCommand) -> ExitCode {
    match cmd.status() {
        Ok(status) => ExitCode::from(exit_byte(status.code())),
        Err(err) => {
            eprintln!("mandala: failed to run {:?}: {err}", cmd.get_program());
            ExitCode::FAILURE
        }
    }
}

/// Clamp a child's exit code into the `u8` an [`ExitCode`] carries (a
/// signal-killed child reports `None` → `1`).
fn exit_byte(code: Option<i32>) -> u8 {
    match code {
        Some(0) => 0,
        Some(c) => u8::try_from(c & 0xff).unwrap_or(1).max(1),
        None => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn inv() -> Inventory {
        Inventory::from_value(json!({
            "schemaVersion": 1,
            "members": {"web": {}, "cache": {}, "router": {}},
            "groups": {"k3s": ["cache", "web"], "gateway": ["router"]},
            "projections": {"deploy": {"nodes": ["web", "cache"]}},
        }))
        .unwrap()
    }

    #[test]
    fn run_argv_matches_python() {
        // Plain selector is canonicalized (sorted) by to_limit.
        assert_eq!(
            run_argv(&inv(), "@k3s", 4, false).unwrap(),
            vec![
                "ansible-playbook",
                "mandala.fleet.deploy",
                "-l",
                "cache,web",
                "-e",
                "deploy_throttle=4",
            ]
        );
        // dry_activate appends the extra-var; throttle interpolates.
        assert_eq!(
            run_argv(&inv(), "web", 8, true).unwrap(),
            vec![
                "ansible-playbook",
                "mandala.fleet.deploy",
                "-l",
                "web",
                "-e",
                "deploy_throttle=8",
                "-e",
                "deploy_dry_activate=true",
            ]
        );
        // Unknown member is refused before any argv is built (never spawns).
        assert!(run_argv(&inv(), "ghost", 4, false).is_err());
    }

    #[test]
    fn batch_argv_validates_group() {
        assert_eq!(
            batch_argv(&inv(), ".", "k3s").unwrap(),
            vec![
                "nix",
                "build",
                "--no-link",
                "--print-out-paths",
                ".#deployBatch.k3s",
            ]
        );
        // `all` is always allowed even though it is not a named group.
        assert_eq!(
            batch_argv(&inv(), "/flake", "all").unwrap().last().unwrap(),
            "/flake#deployBatch.all"
        );
        // Unknown group errors with the Python message text.
        let err = batch_argv(&inv(), ".", "nope").unwrap_err();
        assert_eq!(err.to_string(), "no such group: nope");
    }

    #[test]
    fn exit_byte_clamps() {
        assert_eq!(exit_byte(Some(0)), 0);
        assert_eq!(exit_byte(Some(2)), 2);
        assert_eq!(exit_byte(None), 1);
        assert_eq!(exit_byte(Some(256)), 1); // 256 & 0xff == 0 → floored to 1
    }
}
