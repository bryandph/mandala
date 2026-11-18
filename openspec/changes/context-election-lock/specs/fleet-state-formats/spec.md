## ADDED Requirements

### Requirement: Context election rendezvous has a stable private inode
The persistent coordination namespace SHALL be `~/.local/state/mandala/context-election/<identity-key>.lock` under the stable account home, using the existing canonical-flake identity key. It SHALL be independent of `MANDALA_FLEET_STATE`, `XDG_STATE_HOME` and `XDG_RUNTIME_DIR`. Acquisition SHALL accept an explicitly injected coordination root for isolated tests, propagated through promotion and frontend acquisition seams, but SHALL NOT automatically substitute another root on failure. Production participants for the same user/host SHALL resolve the same account-home root.

The provisioned coordination directory SHALL be owned by the current user at mode 0700; lock files SHALL be regular files owned by that user at mode 0600, including from creation. Missing or unwritable root SHALL fail closed. A missing identity file MAY be securely created in the valid root. Resolution/opening SHALL reject symlinks and foreign ownership and prevent check/open replacement races. Unsupported or unqualified filesystem locking SHALL fail explicitly; supported operation SHALL require host-local POSIX record-lock semantics.

The lock file SHALL be a permanent rendezvous inode, not serialized leader metadata. Implementations SHALL NOT unlink, replace, rename over or recreate an existing lock inode during release, recovery, token rotation or discovery cleanup. Lock contents, age and recorded PID SHALL NOT confer ownership or liveness; kernel lock ownership is authoritative for exclusion, and TCP remains authoritative for follower failure detection. Discovery's atomic write-then-rename protocol SHALL NOT be used on lock inodes.

#### Scenario: client environment roots differ
- **WHEN** clients use the same canonical flake but different `MANDALA_FLEET_STATE`, `XDG_STATE_HOME` or `XDG_RUNTIME_DIR` values
- **THEN** their production coordination path remains the same stable account-home path and they cannot obtain independent leadership locks

#### Scenario: runtime directory is absent
- **WHEN** `XDG_RUNTIME_DIR` is unset, including on macOS
- **THEN** the coordination namespace remains unchanged and no runtime/tmp fallback is selected

#### Scenario: insecure path or foreign owner
- **WHEN** the coordination directory or lock file is a symlink, has foreign ownership or invalid private permissions, or is replaced during validation/opening
- **THEN** acquisition rejects the path without following, repairing or replacing it and without leader execution

#### Scenario: root is not provisioned
- **WHEN** the coordination root is missing or unwritable
- **THEN** acquisition reports an explicit coordination-unavailable error without choosing another directory or becoming leader

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
