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
groups. After a two-second grace period it SIGKILLs any remaining groups and exits
nonzero. If normal error handling finishes first, its group guards still clean up
remaining children.

Group guards also clean up on normal completion and when an async operation is
dropped. In particular, a failed waiter or failure to spawn the waiter drops the
activate-SSH future and kills that group; cancelling confirmation or boot staging
does not detach SSH. Tokio children additionally use `kill_on_drop` for direct
child reaping. The synchronous build group is settled before joining its output
readers, so descendants cannot indefinitely hold those pipes open after Nix exits.

The durable supervisor launches the engine in an isolated group, forwards
SIGTERM/SIGINT to it, and remains alive to reap it. `DeployRun::terminate` signals
the isolated engine/supervisor group, falling back to the engine PID when it was
launched directly rather than by a frontend. Screen dismissal still detaches;
only explicit termination sends a signal.

This is local process ownership, not remote rollback authority. Ending a local
SSH connection cannot undo a remote activation already in progress; deploy-rs's
existing canary/rollback protocol controls that outcome. SIGKILL to the engine
cannot be forwarded, and a descendant deliberately creating a new session can
escape a process group. Run metadata/liveness after an abruptly killed supervisor
is handled separately (LAB-87).

`crates/mandala/tests/termination.rs` exercises real engine and supervisor binaries
with isolated shell-only Nix/SSH effects. It checks build/copy/SSH descendant
cleanup, both forwarded signals, escalation for a signal-ignoring build, and the
failed-waiter/activate race. It never signals live fleet processes.
