//! Built-in, fleet-generic effect engines (`deploy`, `ansible`).
//!
//! `deploy run` owns native orchestration over the in-process
//! [`crate::inventory::Inventory`]; the other effect surfaces remain thin
//! dispatch shells around their existing commands. The public `mandala` binary
//! registers exactly these two via [`crate::cli::Cli::register`]; a downstream
//! operator binary (mandala-bph) adds its own on top.
//!
//! Native deploy preflight ([`deploy::plan_run`]) and the retained batch argv
//! builder ([`deploy::batch_argv`]) are public, pure test seams.

pub mod ansible;
pub mod deploy;
