//! Built-in `ansible` engine: views over the inventory projection.
//!
//! Parity port of `cli/src/mandala_fleet/engines/ansible.py`. One subcommand,
//! `inventory`, prints the projected ansible dynamic-inventory data as
//! `json.dumps(..., indent=2, sort_keys=True)` — or errors to stderr and exits
//! 1 when the aggregate carries no `ansibleInventory` projection.

use std::process::ExitCode;

use clap::{ArgMatches, Command};

use crate::cli::{Engine, to_pretty_2space};
use crate::inventory::Inventory;

/// The `ansible` engine registration.
#[must_use]
pub fn engine() -> Engine {
    let command = Command::new("ansible")
        .about("Views over the projected ansible inventory")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("inventory")
                .about("Print the projected ansible inventory (the dynamic-inventory data)"),
        );
    Engine::new(command, run)
}

/// Dispatch the `ansible` engine's subcommand.
fn run(inv: &Inventory, m: &ArgMatches) -> ExitCode {
    match m.subcommand() {
        Some(("inventory", _)) => match inv.ansible_inventory() {
            Some(projected) => {
                println!("{}", to_pretty_2space(projected));
                ExitCode::SUCCESS
            }
            None => {
                eprintln!(
                    "no ansibleInventory projection in the aggregate (import the ansible flakeModule)"
                );
                ExitCode::FAILURE
            }
        },
        // `subcommand_required` guarantees a matched arm.
        _ => ExitCode::from(2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_projection_errors() {
        let inv = Inventory::from_value(json!({
            "schemaVersion": 1, "members": {}, "groups": {},
        }))
        .unwrap();
        assert!(inv.ansible_inventory().is_none());
    }
}
