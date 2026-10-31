//! Early supervisor identity handoff for native-engine executables.
//!
//! Call [`capture_at_entry`] as the first statement of main. Plain
//! `deploy::engine()` / `register_run()` then use the captured immutable identity;
//! they never read the environment themselves. Without capture, identity is absent.
use std::sync::OnceLock;

pub(crate) const PID_ENV: &str = "MANDALA_DEPLOY_SUPERVISOR_PID";
static IDENTITY: OnceLock<Option<i32>> = OnceLock::new();

/// Validate the hint against the current parent, immediately clear the private
/// environment variable, and cache the result for registration. Later reparenting
/// cannot invalidate the early snapshot; unrelated inherited hints are rejected.
/// Death before this snapshot is intentionally not detected.
///
/// # Safety
/// Call exactly once, as the first statement of main, before starting threads or
/// invoking any other code that can access environment variables. Unix process
/// environment removal is not safe concurrently with environment readers.
///
/// ```no_run
/// fn main() {
///     // SAFETY: first statement, before any threads/environment readers.
///     unsafe { mandala_core::supervisor::capture_at_entry() };
///     // Assemble the CLI, registering deploy::engine() as usual.
/// }
/// ```
pub unsafe fn capture_at_entry() {
    let parent = nix::unistd::getppid().as_raw();
    let hint = std::env::var(PID_ENV).ok();
    // SAFETY: the executable entrypoint promises single-threaded startup.
    unsafe {
        std::env::remove_var(PID_ENV);
    }
    let pid = hint
        .and_then(|raw| raw.parse::<i32>().ok())
        .filter(|pid| *pid > 0 && *pid == parent);
    let _ = IDENTITY.set(pid);
}

pub(crate) fn captured_pid() -> Option<i32> {
    IDENTITY.get().copied().flatten()
}
