# Terminating a deploy

A native deploy owns its Nix build, copy and SSH subprocess groups. The engine
installs SIGTERM/SIGINT forwarding before registering and starting the run. Every
build/profile-resolution/recovery/log-read process, copy process, and SSH leg
(capability/preflight, boot staging/recovery, canary cleanup, activate, wait,
confirm) is spawned as a new process-group leader. Ordinary descendants inherit
that group.

Spawn and group registration use the same lock as signal forwarding: termination
cannot fall between spawning a child and remembering it. Once termination starts,
new effects fail with `Interrupted` instead of launching another copy, recovery
build or SSH connection. The engine forwards the received signal to all owned
groups. After a two-second grace period it SIGKILLs still-owned groups and exits
nonzero. Reaping and deregistration share the forwarder lock, so no signal can
race between a successful wait and removing its now-reusable PGID.

Group guards clean up when an operation is cancelled or returns early while its
leader is still unreaped. A successful wait disarms the guard: dropping a
completed operation never signals a PGID that could belong to another process. In particular, a failed waiter or failure to spawn the waiter drops the
activate-SSH future and kills that group; cancelling confirmation or boot staging
does not detach SSH. Tokio children additionally use `kill_on_drop` for direct
child reaping. Capture drains stdout/stderr concurrently with waiting and
unregisters a reaped leader immediately, even if descendants still hold pipes
open. Deliberately choosing the simpler safe policy, descendants of a normally
completed/reaped leader are not killed; as before this change, capture can wait
for them to close inherited output descriptors. Killing such descendants safely
would require a pre-reap observation (for example WNOWAIT), not a post-reap
killpg, because an empty group's identifier is no longer reserved.

The durable supervisor launches the engine in an isolated group, forwards
SIGTERM/SIGINT to it, and remains alive to reap it. `DeployRun::terminate` signals
the isolated engine/supervisor group, falling back to the engine PID when it was
launched directly rather than by a frontend. Screen dismissal still detaches;
only explicit termination sends a signal.

This is local process ownership, not remote rollback authority. Ending a local
SSH connection cannot undo a remote activation already in progress; deploy-rs's
existing canary/rollback protocol controls that outcome. SIGKILL to the engine
cannot be forwarded, and a descendant deliberately creating a new session can
escape a process group.

The supervisor passes `MANDALA_DEPLOY_SUPERVISOR_PID` only in its engine child's
command environment. The engine writes that positive process ID as
`supervisor_pid` in its initial atomic `meta.json`, before publishing its run ID.
No frontend discovery-time metadata merge is needed, and subsequent engine
metadata updates preserve the field. This is a private handoff, not a user
configuration option: ordinary direct CLI runs omit it, and malformed/out-of-range
values are ignored. A valid handoff is not checked against the current parent
PID, because an engine whose supervisor died early may already be reparented.
Callers launching a standalone engine must not inject an unrelated stale hint.

Registry observers and attached frontends treat a dead recorded supervisor with
no terminal run rc as failure, even if the engine PID remains alive. A live
supervisor also protects its registered run from retention pruning. Neither
observer writes metadata or kills processes implicitly. PID reuse retains the
registry's existing conservative liveness caveat.

`crates/mandala/tests/termination.rs` exercises real engine and supervisor binaries
with isolated shell-only Nix/SSH effects. It checks build/copy/SSH descendant
cleanup, both forwarded signals, escalation for a signal-ignoring build, and the
failed-waiter/activate race. Unit tests intercept the group-signal operation to
assert successful std/async waits never signal on drop and an unreaped guard
still does. `crates/mandala/tests/supervisor_metadata.rs` exercises real
`DeployRun::start` metadata, supervisor-death detection, active-run retention,
direct CLI omission and an already-dead handoff. Tests never signal live fleet
processes.
