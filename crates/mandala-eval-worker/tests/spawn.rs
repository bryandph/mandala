//! Spawn regression: the BUILT worker binary must never die on a signal.
//!
//! Phase-2 live finding (mandala-native-tui task 7.4): a `set_setting` on a
//! setting the C API cannot reach raised a C++ exception through the Rust
//! frame and ABORTED the worker at startup — invisible under the TUI's nulled
//! stderr, and uncaught by every suite because nothing ever *executed* the
//! built worker. This test pins the invariant that matters for that class:
//! whatever the environment (a full store on an operator machine, or the
//! storeless nix build sandbox), a ping must produce either an in-band JSON
//! reply or a clean nonzero exit — never signal death.

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn worker_ping_never_dies_on_a_signal() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mandala-eval-worker"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn built worker");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"{\"id\":1,\"op\":\"ping\"}\n")
        .expect("write ping");
    // Dropping stdin above EOFs the worker after the one request.
    let out = child.wait_with_output().expect("wait");

    // A clean environment answers the ping; an environment where init cannot
    // complete (e.g. no usable store in a build sandbox) must fail in-band.
    // Either way the process must EXIT, not abort: on unix, signal death
    // (SIGABRT from a foreign exception) reports no exit code.
    assert!(
        out.status.code().is_some(),
        "worker died on a signal (status {:?})\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}
