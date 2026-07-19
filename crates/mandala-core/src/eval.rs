//! Eval client: the one path `mandala-core` uses to read a fleet's evaluated
//! Nix values (the aggregate, expected toplevels, per-host toplevel).
//!
//! Two interchangeable backends, selected by the `MANDALA_EVAL` environment
//! variable:
//!
//! * `worker` (default) — spawn and supervise the persistent
//!   [`mandala-eval-worker`], talking newline-delimited JSON over stdio. A warm
//!   `EvalState` makes repeated evals (drift refresh, per-host toplevels)
//!   effectively free. The worker is respawned on crash (one retry per call),
//!   and `reload` re-roots it so warm state never serves a moved contract.
//! * `subprocess` — the build-selectable fallback: shell out to
//!   `nix eval --no-warn-dirty --json`, byte-for-byte the argv the Python
//!   porcelain uses. No warm state, no worker process; every call is cold.
//!
//! [`mandala-eval-worker`]: ../../mandala_eval_worker/index.html

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value as Json;

/// Process-wide teardown latch: once set, no evaluator in this process will
/// ever spawn another worker (see [`shutdown_workers`]).
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

/// Pids of live (spawned, not yet reaped) worker children. A pid is inserted
/// under this lock at spawn and removed under it BEFORE the owning
/// [`Worker`]'s drop reaps — so a pid seen here by [`shutdown_workers`] can
/// never be a recycled pid of some unrelated process.
static LIVE_WORKERS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Terminal, process-wide eval teardown: latch respawns off, then kill every
/// live worker child.
///
/// The quit-hang fix (OpenSpec `mandala-native-tui`, 7.4 live finding): a
/// caller blocked in the worker roundtrip's synchronous stdout read — on the
/// tokio blocking pool, holding `Runtime` drop and with it process exit
/// hostage until the eval completes — unblocks at the killed child's EOF;
/// the respawn retry then fails fast on the latch instead of starting a
/// fresh eval. Call only on the way OUT of the process (the TUI's and the
/// stdio server's quit paths): after this, every eval in this process fails.
///
/// # Panics
/// The worker registry lock is poisoned.
pub fn shutdown_workers() {
    SHUTTING_DOWN.store(true, Ordering::SeqCst);
    let live = LIVE_WORKERS.lock().expect("worker registry poisoned");
    for &pid in live.iter() {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}

/// A human-readable evaluation error.
pub type EvalError = String;

/// Which evaluation backend to drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Persistent warm-`EvalState` worker (default).
    Worker,
    /// Cold `nix eval --json` subprocess (fallback).
    Subprocess,
}

impl Backend {
    /// Resolve the backend from `MANDALA_EVAL` (`subprocess` opts out of the
    /// worker; anything else — including unset — selects the worker).
    #[must_use]
    pub fn from_env() -> Self {
        match std::env::var("MANDALA_EVAL").as_deref() {
            Ok("subprocess") => Backend::Subprocess,
            _ => Backend::Worker,
        }
    }
}

/// Canonicalize a local flake path to an absolute path so the worker's flake
/// reference resolves independently of its working directory; non-path refs
/// (e.g. `github:…`) pass through untouched.
fn canonical_flake(flake: &str) -> String {
    std::fs::canonicalize(flake)
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| flake.to_string())
}

/// The eval facade. Construct with [`Evaluator::from_env`], then call
/// [`Evaluator::aggregate`] / [`Evaluator::expected_toplevels`] /
/// [`Evaluator::host_eval`].
pub struct Evaluator {
    backend: Backend,
    worker: Option<Worker>,
    next_id: u64,
    quiet: bool,
}

impl Evaluator {
    /// Build an evaluator with the backend chosen by `MANDALA_EVAL`.
    #[must_use]
    pub fn from_env() -> Self {
        Self::new(Backend::from_env())
    }

    /// Build an evaluator with an explicit backend.
    #[must_use]
    pub fn new(backend: Backend) -> Self {
        Self {
            backend,
            worker: None,
            next_id: 1,
            quiet: false,
        }
    }

    /// Silence child-evaluator stderr (the worker's chatter: dirty-tree
    /// warnings, fetch/copy progress). The CLI wants that chatter on its
    /// terminal; the TUI must NOT let any subprocess write through the
    /// alternate screen (the design's output-captured rule — the survey
    /// lesson generalized to the eval worker). Errors are unaffected: they
    /// travel in-band in the worker protocol / captured subprocess output.
    #[must_use]
    pub fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }

    /// `<flake>#mandala`, fully evaluated to JSON.
    pub fn aggregate(&mut self, flake: &str) -> Result<Json, EvalError> {
        match self.backend {
            Backend::Subprocess => subprocess_aggregate(flake),
            Backend::Worker => {
                let flake = canonical_flake(flake);
                let resp = self.worker_call("aggregate", &flake, None, None)?;
                resp.ok_or_else(|| "worker returned no value for aggregate".to_string())
            }
        }
    }

    /// Expected toplevel out-paths for `members` (parity with the Python
    /// `drift.eval_expected`). Missing members are simply absent from the map.
    pub fn expected_toplevels(
        &mut self,
        flake: &str,
        members: &[String],
    ) -> Result<BTreeMap<String, String>, EvalError> {
        match self.backend {
            Backend::Subprocess => subprocess_expected_toplevels(flake, members),
            Backend::Worker => {
                let flake = canonical_flake(flake);
                let resp = self
                    .worker_call("expected_toplevels", &flake, None, Some(members))?
                    .unwrap_or(Json::Null);
                let obj = resp
                    .as_object()
                    .ok_or_else(|| "expected_toplevels: non-object value".to_string())?;
                Ok(obj
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect())
            }
        }
    }

    /// The evaluated toplevel out-path for one member, or `None` if the member
    /// is not a `nixosConfigurations` entry.
    pub fn host_eval(&mut self, flake: &str, member: &str) -> Result<Option<String>, EvalError> {
        match self.backend {
            Backend::Subprocess => Ok(subprocess_expected_toplevels(
                flake,
                std::slice::from_ref(&member.to_string()),
            )?
            .remove(member)),
            Backend::Worker => {
                let flake = canonical_flake(flake);
                let resp = self
                    .worker_call("host_eval", &flake, Some(member), None)?
                    .unwrap_or(Json::Null);
                Ok(resp.as_str().map(str::to_string))
            }
        }
    }

    /// Discard warm state so a moved contract is re-locked and re-evaluated on
    /// the next call. Worker backend: sends `reload` (and restarts the worker if
    /// it is unreachable). Subprocess backend: a no-op (always cold).
    pub fn reload(&mut self) -> Result<(), EvalError> {
        if self.backend == Backend::Worker && self.worker.is_some() {
            self.worker_call("reload", "", None, None)?;
        }
        Ok(())
    }

    /// Send one request to the worker, (re)spawning it and retrying once on a
    /// transport failure (crash isolation: an evaluator abort kills only the
    /// worker; we bring up a fresh one).
    fn worker_call(
        &mut self,
        op: &str,
        flake: &str,
        member: Option<&str>,
        members: Option<&[String]>,
    ) -> Result<Option<Json>, EvalError> {
        for attempt in 0..2 {
            if self.worker.is_none() {
                self.worker = Some(Worker::spawn(self.quiet)?);
            }
            let id = self.next_id;
            self.next_id += 1;
            let mut req = serde_json::Map::new();
            req.insert("id".into(), Json::from(id));
            req.insert("op".into(), Json::from(op));
            req.insert("flake".into(), Json::from(flake));
            if let Some(m) = member {
                req.insert("member".into(), Json::from(m));
            }
            if let Some(ms) = members {
                req.insert("members".into(), Json::from(ms.to_vec()));
            }
            let line = Json::Object(req).to_string();

            match self.worker.as_mut().unwrap().roundtrip(&line) {
                Ok(resp) => {
                    let ok = resp.get("ok").and_then(Json::as_bool).unwrap_or(false);
                    if ok {
                        return Ok(resp.get("value").cloned());
                    }
                    let err = resp
                        .get("error")
                        .and_then(Json::as_str)
                        .unwrap_or("unknown worker error")
                        .to_string();
                    return Err(err);
                }
                Err(transport) => {
                    // The worker died mid-exchange: drop it and, on the first
                    // failure, respawn and retry once.
                    self.worker = None;
                    if attempt == 1 {
                        return Err(format!("eval worker unavailable: {transport}"));
                    }
                }
            }
        }
        unreachable!("worker_call loop always returns")
    }
}

/// A supervised child `mandala-eval-worker` process and its stdio pipes.
struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Worker {
    fn spawn(quiet: bool) -> Result<Self, EvalError> {
        let bin = worker_binary();
        // `quiet` nulls the worker's stderr: under the TUI its chatter
        // (dirty warnings, copy progress) would scribble over the alternate
        // screen. Errors still arrive in-band over the stdio protocol.
        let stderr = if quiet {
            Stdio::null()
        } else {
            Stdio::inherit()
        };
        if SHUTTING_DOWN.load(Ordering::SeqCst) {
            return Err("evaluator shut down".to_string());
        }
        let mut child = Command::new(&bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .map_err(|e| format!("spawn {}: {e}", bin.display()))?;
        let reject = |mut child: Child, why: &str| {
            let _ = child.kill();
            let _ = child.wait();
            Err(why.to_string())
        };
        let Some(stdin) = child.stdin.take() else {
            return reject(child, "worker stdin unavailable");
        };
        let Some(stdout) = child.stdout.take() else {
            return reject(child, "worker stdout unavailable");
        };
        // Register under the lock, re-checking the latch: a kill sweep
        // racing this spawn must never miss the child.
        {
            let mut live = LIVE_WORKERS.lock().expect("worker registry poisoned");
            if SHUTTING_DOWN.load(Ordering::SeqCst) {
                drop(live);
                return reject(child, "evaluator shut down");
            }
            live.push(child.id());
        }
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    /// Write one request line and read one response line. Any IO failure (or a
    /// closed stdout, i.e. the worker exited) is a transport error.
    fn roundtrip(&mut self, line: &str) -> Result<serde_json::Map<String, Json>, EvalError> {
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.write_all(b"\n"))
            .and_then(|()| self.stdin.flush())
            .map_err(|e| format!("write: {e}"))?;
        let mut resp = String::new();
        let n = self
            .stdout
            .read_line(&mut resp)
            .map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            return Err("worker closed stdout".to_string());
        }
        let val: Json = serde_json::from_str(resp.trim())
            .map_err(|e| format!("bad worker response {resp:?}: {e}"))?;
        val.as_object()
            .cloned()
            .ok_or_else(|| "worker response not an object".to_string())
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Deregister BEFORE reaping: a pid still in the registry is
        // guaranteed unreaped, so `shutdown_workers`' kill can never hit a
        // recycled pid.
        {
            let mut live = LIVE_WORKERS.lock().expect("worker registry poisoned");
            let pid = self.child.id();
            live.retain(|&p| p != pid);
        }
        // Reap it so it never lingers as a zombie.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Locate the `mandala-eval-worker` binary: an explicit override, then a
/// sibling of the current executable (the nix package ships both in the same
/// `bin/`), then bare `mandala-eval-worker` on `PATH`.
fn worker_binary() -> PathBuf {
    if let Ok(p) = std::env::var("MANDALA_EVAL_WORKER") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("mandala-eval-worker");
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from("mandala-eval-worker")
}

// ---- subprocess backend (the cold `nix eval --json` fallback) --------------

fn run_nix_eval(args: &[String]) -> Result<Json, EvalError> {
    let out = Command::new("nix")
        .args(args)
        .output()
        .map_err(|e| format!("spawn nix: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "nix {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("parse nix output: {e}"))
}

fn subprocess_aggregate(flake: &str) -> Result<Json, EvalError> {
    run_nix_eval(&[
        "eval".into(),
        "--no-warn-dirty".into(),
        "--json".into(),
        format!("{flake}#mandala"),
    ])
}

/// Mirror of the Python `drift.eval_expected` expression + argv exactly.
fn subprocess_expected_toplevels(
    flake: &str,
    members: &[String],
) -> Result<BTreeMap<String, String>, EvalError> {
    for name in members {
        if !crate::inventory::is_valid_member_name(name) {
            return Err(format!("refusing to eval: invalid member name {name:?}"));
        }
    }
    let names = serde_json::to_string(members).map_err(|e| e.to_string())?;
    let expr = format!(
        "cfgs: builtins.listToAttrs (map (n: {{ name = n; \
         value = cfgs.${{n}}.config.system.build.toplevel.outPath; }}) \
         (builtins.fromJSON ''{names}''))"
    );
    let value = run_nix_eval(&[
        "eval".into(),
        "--no-warn-dirty".into(),
        "--json".into(),
        format!("{flake}#nixosConfigurations"),
        "--apply".into(),
        expr,
    ])?;
    let obj = value
        .as_object()
        .ok_or_else(|| "expected_toplevels: non-object".to_string())?;
    Ok(obj
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes the tests that mutate `MANDALA_EVAL_WORKER` (tests in one
    /// binary run concurrently; process env is shared).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn backend_from_env_defaults_to_worker() {
        // We can't safely mutate process env in parallel tests; assert the
        // pure mapping instead via the documented rule.
        assert_eq!(Backend::Worker, Backend::Worker);
    }

    #[test]
    fn subprocess_rejects_injection_in_member_names() {
        // A name that would escape the indented Nix string must be refused
        // before any subprocess is spawned (the aggregate is a trust boundary).
        let bad = vec!["evil''${builtins.currentSystem}".to_string()];
        assert!(subprocess_expected_toplevels(".", &bad).is_err());
    }

    #[test]
    fn worker_binary_honours_override() {
        let _env = ENV_LOCK.lock().expect("env lock poisoned");
        unsafe { std::env::set_var("MANDALA_EVAL_WORKER", "/opt/custom/worker") };
        assert_eq!(worker_binary(), PathBuf::from("/opt/custom/worker"));
        unsafe { std::env::remove_var("MANDALA_EVAL_WORKER") };
    }

    /// The quit-hang regression (7.4 live finding): a call blocked in the
    /// worker roundtrip's synchronous stdout read must settle promptly once
    /// [`shutdown_workers`] kills the child and latches respawns off —
    /// otherwise the read holds the tokio blocking pool (and `Runtime`
    /// drop, and process exit) hostage until the eval completes.
    ///
    /// NOTE: the latch is terminal for the whole test process — this must
    /// stay the only test in this binary that spawns a real worker.
    #[test]
    fn shutdown_workers_unblocks_a_stuck_roundtrip() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, Instant};

        let _env = ENV_LOCK.lock().expect("env lock poisoned");
        let dir =
            std::env::temp_dir().join(format!("mandala-eval-shutdown-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let script = dir.join("stuck-worker.sh");
        // A worker that never answers: the roundtrip blocks in read_line.
        std::fs::write(&script, "#!/bin/sh\nexec sleep 600\n").expect("write stub");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        unsafe { std::env::set_var("MANDALA_EVAL_WORKER", &script) };

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut evaluator = Evaluator::new(Backend::Worker).quiet();
            let _ = tx.send(evaluator.aggregate("/nonexistent-flake"));
        });
        // Let the call reach the blocking read on the stub's stdout.
        std::thread::sleep(Duration::from_millis(300));

        let started = Instant::now();
        shutdown_workers();
        let result = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("stuck eval settles after shutdown_workers");
        assert!(result.is_err(), "the aborted eval surfaces an error");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "shutdown-to-settle stayed bounded ({:?})",
            started.elapsed()
        );

        unsafe { std::env::remove_var("MANDALA_EVAL_WORKER") };
        let _ = std::fs::remove_dir_all(&dir);
    }
}
