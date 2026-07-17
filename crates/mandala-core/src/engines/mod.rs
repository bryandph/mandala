//! Built-in, fleet-generic effect engines (`deploy`, `ansible`).
//!
//! Parity ports of `cli/src/mandala_fleet/engines/{deploy,ansible}.py`. Both are
//! thin dispatch shells: they resolve selectors / read projections off the
//! in-process [`crate::inventory::Inventory`] and shell out to the real
//! machinery (`ansible-playbook mandala.fleet.deploy`, `nix build
//! .#deployBatch.<group>`) — no orchestration lives here. The public `mandala`
//! binary registers exactly these two via [`crate::cli::Cli::register`]; a
//! downstream operator binary (mandala-bph) adds its own on top.
//!
//! The argv builders ([`deploy::run_argv`], [`deploy::batch_argv`]) are pure and
//! public so a test can assert the exact command line without spawning ansible
//! or nix.

pub mod ansible;
pub mod deploy;
