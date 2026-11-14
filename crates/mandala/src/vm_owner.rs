//! Opt-in owner-local helper. SSH stdio only: no listener, daemon or fleet eval.
use std::process::ExitCode;

fn main() -> ExitCode {
    if std::env::args().len() != 1 {
        eprintln!("mandala-vm-owner: no arguments accepted; use the versioned SSH stdio protocol");
        return ExitCode::FAILURE;
    }
    match mandala_core::vm::owner::run_stdio() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mandala-vm-owner: {error}");
            ExitCode::FAILURE
        }
    }
}
