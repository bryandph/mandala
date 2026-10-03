## MODIFIED Requirements

### Requirement: Deployed-generation drift judgement
The drift dashboard SHALL compare controller-side state snapshots, including
the running, booted, and installed system-profile targets (written by a
read-only fan-out survey and keyed by snapshot filename so no file can
impersonate another host), against locally evaluated toplevel out-paths. An
expected generation that is installed as the system profile but is not the
running generation SHALL be `reboot-pending`, while a different uninstalled
expectation SHALL remain drift. Existing booted-versus-current boot-critical
judgement SHALL continue identifying live-activated generations that require
reboot. The evaluated expectation SHALL be cached by the contract's git
revision: a clean-rev match reuses the cache without re-evaluating, and a
mismatch is surfaced as "contract moved". Snapshots older than a staleness
threshold, incomplete snapshots, never-surveyed members, and unreachable
members SHALL be judged as distinct statuses rather than folded into
in-sync, reboot-pending, or drift. Expected-toplevel evaluation SHALL isolate
failures per member: a failure confined to one member's configuration SHALL
be reported against that member while every other member still evaluates,
and that member SHALL be judged `eval-failed`, carrying its evaluation error,
in place of any status that needs the expectation (in-sync, drift,
reboot-pending, activated). A partial evaluation MUST NOT be written to the
expectation cache, and `drift --eval` SHALL still report every member but
exit non-zero when any member failed. A drift refresh SHALL run expected-toplevel
evaluation and the read-only state survey concurrently from one operator
gesture and SHALL also run automatically once a deploy or reboot completes.
The survey SHALL run in the background rather than as a blocking view,
reporting a running count of host snapshots written so far in the top bar.

#### Scenario: an unmoved contract costs no re-eval
- **WHEN** drift is viewed and the repo's clean git rev equals the rev the cached expectation was evaluated at
- **THEN** the cached expectation is used and no nix eval runs

#### Scenario: staged expected generation is reboot-pending
- **WHEN** the snapshot's current generation differs from expected and its system profile equals expected
- **THEN** the dashboard reports reboot-pending rather than generic drift or in-sync

#### Scenario: unstaged expected generation remains drift
- **WHEN** the current and system-profile generations both differ from the known expected generation
- **THEN** the dashboard reports drift

#### Scenario: a refresh evaluates and surveys concurrently
- **WHEN** the operator refreshes drift
- **THEN** expected evaluation and the read-only state survey run concurrently and the dashboard updates as each completes

#### Scenario: the survey runs in the background with a live count
- **WHEN** a state survey is running
- **THEN** it runs in the background instead of taking over the screen, and
  the top bar reports a running count of how many host snapshots have been
  written so far, until the survey completes and drift refreshes

#### Scenario: a completed deploy or reboot refreshes drift on its own
- **WHEN** a deploy or reboot run finishes rather than being cancelled before
  completion
- **THEN** the concurrent eval-and-survey refresh runs automatically, so the
  post-change state is judged without a separate operator gesture

#### Scenario: old data never claims in-sync
- **WHEN** a member's snapshot is older than the staleness threshold
- **THEN** its status reads stale, not in-sync, reboot-pending, or drift

#### Scenario: one broken member never hides the others
- **WHEN** expected evaluation runs and one member's configuration fails to evaluate
- **THEN** every other member is judged against its evaluated expectation and the failed member reads eval-failed with its error, never in-sync, drift, reboot-pending, or activated

#### Scenario: a partial evaluation is not cached
- **WHEN** expected evaluation completes with any member failed
- **THEN** the rev-keyed expectation cache is not written, and `drift --eval` exits non-zero after reporting every member
