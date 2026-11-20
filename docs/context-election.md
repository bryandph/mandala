# Context election qualification and operation

Until the lifetime-lock implementation ships, use one TUI or MCP context frontend per checkout per host.

The lifetime-lock guarantee requires stopping **every** context frontend for the user/host, upgrading **all** launch paths, and restarting them together. Old binaries ignore the lock: discovery compatibility, token rotation and discovery cleanup cannot fence them. Never unlink lock files as an upgrade or recovery step. Deployment requires a separate operator decision; implementing or qualifying this change does not authorize rollout.

## Isolated test environment

All acquisition tests inject a private, pre-created coordination root, retained across reacquisition. An injected root is never created by acquisition. Test fixture setup owns its creation. Production coordination uses the stable account home, not HOME/client state/runtime overrides; do not point tests at production state.

Acquisition owns secure creation of a missing default `~/.local/state/mandala/context-election` and missing parents below the account home, at mode 0700. It verifies existing and created directories without following symlinks and rejects unsafe paths rather than repairing them. Lock files are persistent mode-0600 regular inodes, never stale-lock markers. Only local filesystems with POSIX process record locking are supported; network homes are not qualified. Admission checks the opened root descriptor: macOS requires MNT_LOCAL; Linux refuses known network/FUSE filesystem magics (NFS, SMB/CIFS/SMB2, 9p including WSL drvfs, FUSE, Ceph, AFS, Coda and NCP), and assumes other filesystems are local. Unsupported kernel record-lock errors still fail explicitly.

Context suites run a bounded native-loopback refusal preflight. On WSL, unused loopback ports may time out instead of refusing (LAB-206). This is an environment error, not permission to weaken the production busy-leader rule.

Build before entering a private namespace, then copy the executable to an immutable scratch location. Provision isolated scratch HOME/state/runtime/temp directories and use only injected coordination roots:

```sh
cargo test -p mandala-context --no-run
# Select an executable from target/debug/deps; copy it, then chmod a-w.
export HOME=/tmp/owned-fixture/home
export XDG_STATE_HOME=/tmp/owned-fixture/state
export XDG_RUNTIME_DIR=/tmp/owned-fixture/runtime
export TMPDIR=/tmp/owned-fixture/tmp
unshare --user --map-root-user --net sh -c '
  ip link set lo up
  exec "$1" --test-threads=32
' sh /absolute/path/to/immutable-test-binary
```

Native macOS runs use the same refusal preflight, without a Linux namespace. Run Linux suites repeatedly under owned CPU load; record load process ownership and terminate only those owned load processes. Namespace evidence does not replace sandboxed Nix builds on either platform.

## Acquisition failures

For canonical checkouts, MCP completes protocol initialization even when acquisition fails, but logs the error and returns a structured acquisition error from every tool. It never falls back to an effectful standalone handler. A host started before discovery publication fails is fully torn down before this error-only server begins. Non-path flake references retain existing standalone behavior. TUI read-only fallback is unchanged except that coordination errors fail closed.

A fork child deliberately paused before exec can retain inherited TCP listeners even though POSIX process locks are not inherited. Current behavior is safe unavailability (LeaderBusy, no second endpoint), with recovery after the child execs or exits. A separate operator decision is required before changing the merged stronger fork guarantee or adding socket atfork handling; see the change's fork audit and qualification notes. This limitation is not hidden by helper-only lock tests.

## Preserved pre-fix witness

Signed commit `130c29b31dd8b44a60151146918d183b1153ed5d` contains the unmodified diagnostic patch (SHA256 `25881535aa1ade310c45968e2250c4a0e2cccbc1ca2458f011da1b786d46ffea`). On Linux/WSL in a fresh scratch clone and private loopback namespace, the immutable diagnostic executable failed `disappearing_foreign --ignored --nocapture`: 0 passed, 1 failed; authenticated same-flake endpoints on ports 29800 (`fast`) and 29802 (`slow`) both executed calls. This reproduces the pre-fix bug independently of retained role flags. Original supplied stress evidence also found two authenticated endpoints after 697 successful 32-contender trials (failure at trial 698); stochastic success does not disprove that observation.
