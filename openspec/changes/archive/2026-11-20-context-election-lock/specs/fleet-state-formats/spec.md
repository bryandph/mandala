## ADDED Requirements

### Requirement: Context election rendezvous has a stable private inode
The persistent coordination namespace SHALL be `~/.local/state/mandala/context-election/<identity-key>.lock` under the stable account home, using the existing canonical-flake identity key. It SHALL be independent of `MANDALA_FLEET_STATE`, `XDG_STATE_HOME` and `XDG_RUNTIME_DIR`. Acquisition SHALL accept an explicitly injected coordination root for isolated tests, propagated through promotion and frontend acquisition seams, but SHALL NOT automatically substitute another root on failure. Production participants for the same user/host SHALL resolve the same account-home root.

Acquisition SHALL securely create the DEFAULT coordination root when missing, including any missing parents beneath the stable account home (`.local/state/mandala`), using mkdir at mode 0700 without following symlinks. After opening, it SHALL verify that the root is a real directory owned by the current user at mode 0700 and that no path component is a symlink; existing parents SHALL be securely validated and newly created parents SHALL also be verified. An explicitly injected test root SHALL NEVER be auto-created: absence SHALL fail closed. Any creation failure, unwritable root or unsafe existing path SHALL fail closed without alternate-root fallback or repair of untrusted paths. Lock files SHALL be regular files owned by the current user at mode 0600, including from creation. A missing identity file MAY be securely created in the valid root. Resolution/opening SHALL reject symlinks and foreign ownership and prevent check/open replacement races. Unsupported or unqualified filesystem locking SHALL fail explicitly; supported operation SHALL require host-local POSIX record-lock semantics.

The lock file SHALL be a permanent rendezvous inode, not serialized leader metadata. Implementations SHALL NOT unlink, replace, rename over or recreate an existing lock inode during release, recovery, token rotation or discovery cleanup. Lock contents, age and recorded PID SHALL NOT confer ownership or liveness; kernel lock ownership is authoritative for exclusion, and TCP remains authoritative for follower failure detection. Discovery's atomic write-then-rename protocol SHALL NOT be used on lock inodes.

#### Scenario: client environment roots differ
- **WHEN** clients use the same canonical flake but different `MANDALA_FLEET_STATE`, `XDG_STATE_HOME` or `XDG_RUNTIME_DIR` values
- **THEN** their production coordination path remains the same stable account-home path and they cannot obtain independent leadership locks

#### Scenario: runtime directory is absent
- **WHEN** `XDG_RUNTIME_DIR` is unset, including on macOS
- **THEN** the coordination namespace remains unchanged and no runtime/tmp fallback is selected

#### Scenario: unsafe existing path fails
- **WHEN** an existing coordination path component or lock file is a symlink, has foreign ownership or unsafe permissions, the root is not a real mode-0700 directory, or the path is replaced during validation/opening
- **THEN** acquisition fails closed without following, repairing or replacing the unsafe path and without leader execution

#### Scenario: missing default root is created securely
- **WHEN** the default coordination root or its parents beneath the stable account home are missing
- **THEN** acquisition creates the missing directories securely at mode 0700 without following symlinks and verifies directory type, current-user ownership, root mode 0700 and path safety after opening before proceeding

#### Scenario: missing injected root fails
- **WHEN** an explicitly injected test coordination root is missing
- **THEN** acquisition fails closed without creating that root, choosing another directory or becoming leader

#### Scenario: default root creation fails
- **WHEN** secure creation of the default root or a missing parent fails, or the resulting root is unwritable
- **THEN** acquisition reports explicit coordination unavailability without alternate-root fallback or leader execution

#### Scenario: stale file survives a crash
- **WHEN** a leader dies leaving its lock file and stale discovery behind
- **THEN** the next owner locks the same inode after kernel release and recovers discovery without deleting a stale-lock marker

#### Scenario: release races another contender
- **WHEN** a leader shuts down while another contender has opened or is waiting for its rendezvous inode
- **THEN** the inode is retained so subsequent opens cannot create an independent lock domain at the same pathname

### Requirement: Coordination layout is distinct from serialized discovery compatibility
The coordination path, permissions, identity mapping and persistent-inode behavior SHALL be tested as a multi-frontend compatibility contract. Existing discovery JSON bytes and atomic publication, bearer token mint/reuse/rotation, run metadata and event-format versions SHALL remain unchanged. Lock-file contents SHALL NOT introduce a new credential, PID-based stale-lock protocol or JSON format. Tests SHALL pin root resolution, ownership/mode checks and inode retention alongside existing discovery serialization and liveness judgments. Unchanged discovery bytes SHALL NOT imply safe mixed-version election; the coordinated-upgrade precondition in `fleet-context` SHALL still apply.

#### Scenario: metadata is rewritten or a token rotates
- **WHEN** a guarded leader atomically rewrites discovery or rotates its token
- **THEN** existing discovery serialization and token cutoff semantics remain intact while the lock-file inode and lifetime lease remain unchanged

#### Scenario: format checks run after the implementation change
- **WHEN** the lifetime lock is added to acquisition and teardown
- **THEN** existing discovery golden bytes remain valid and new behavior gates reject drift in coordination root, security checks or persistent-inode handling

#### Scenario: an older reader accepts discovery
- **WHEN** an older frontend can parse discovery written by an upgraded leader
- **THEN** format compatibility is preserved but it does not establish that the older frontend participates in election locking
