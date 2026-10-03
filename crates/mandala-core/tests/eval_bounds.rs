//! Worker roundtrip bounds: a worker that stops answering fails the
//! call within the configured bound and is killed, and a reply that answers a
//! different request is rejected rather than returned.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use mandala_core::eval::{Backend, Evaluator};

/// Both tests point `MANDALA_EVAL_WORKER` at their own stub; process env is
/// shared by the tests of this binary, so they run one at a time.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mandala-eval-{name}-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn write_stub(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}")).expect("write worker stub");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod worker stub");
}

fn with_worker<T>(stub: &Path, f: impl FnOnce() -> T) -> T {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    unsafe { std::env::set_var("MANDALA_EVAL_WORKER", stub) };
    let out = f();
    unsafe { std::env::remove_var("MANDALA_EVAL_WORKER") };
    out
}

#[test]
fn a_silent_worker_times_out_is_killed_and_replaced() {
    let dir = temp_dir("timeout");
    let stub = dir.join("worker.sh");
    let counter = dir.join("counter");
    let pidfile = dir.join("pid");
    // First spawn wedges (never answers); later spawns echo each request id.
    write_stub(
        &stub,
        &format!(
            r#"n=0
test -f '{counter}' && n=$(cat '{counter}')
n=$((n + 1))
echo "$n" > '{counter}'
if [ "$n" = 1 ]; then
  echo $$ > '{pidfile}'
  exec sleep 600
fi
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  printf '{{"id":%s,"ok":true,"value":"fresh"}}\n' "$id"
done
"#,
            counter = counter.display(),
            pidfile = pidfile.display(),
        ),
    );

    with_worker(&stub, || {
        let mut evaluator = Evaluator::new(Backend::Worker)
            .quiet()
            .timeout(Some(Duration::from_millis(300)));

        let started = Instant::now();
        let err = evaluator
            .aggregate(".")
            .expect_err("a wedged worker fails the call");
        assert!(err.contains("timed out"), "timeout error: {err}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the call settled within the bound ({:?})",
            started.elapsed()
        );

        // The wedged worker was killed and reaped, not left running.
        let pid: i32 = fs::read_to_string(&pidfile)
            .expect("stub recorded its pid")
            .trim()
            .parse()
            .expect("pid");
        assert!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err(),
            "timed-out worker {pid} is gone"
        );

        // No retry after a timeout: exactly one worker was spawned so far.
        assert_eq!(fs::read_to_string(&counter).unwrap().trim(), "1");

        // The next call starts a fresh worker.
        assert_eq!(evaluator.aggregate(".").expect("fresh worker"), "fresh");
    });
    fs::remove_dir_all(&dir).expect("remove temp dir");
}

#[test]
fn a_reply_for_another_request_is_rejected() {
    let dir = temp_dir("mismatch");
    let stub = dir.join("worker.sh");
    // Always answers, but never for the request it was asked.
    write_stub(
        &stub,
        r#"while IFS= read -r line; do
  printf '{"id":999999,"ok":true,"value":"stale"}\n'
done
"#,
    );

    with_worker(&stub, || {
        let mut evaluator = Evaluator::new(Backend::Worker)
            .quiet()
            .timeout(Some(Duration::from_secs(10)));
        let err = evaluator
            .aggregate(".")
            .expect_err("a mismatched reply is never returned as the value");
        assert!(
            err.contains("does not match request id"),
            "mismatch error: {err}"
        );
    });
    fs::remove_dir_all(&dir).expect("remove temp dir");
}
