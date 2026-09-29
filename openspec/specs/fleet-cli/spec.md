# fleet-cli Specification

## Purpose
The fleet porcelain: one static `mandala` binary serving a dispatch-only CLI plus TUI tiers (read-only explorer/drift, confirm-gated action tier, deploy-runner) over the `flake.mandala` aggregate, with engines composed at compile time via the mandala-core library API and the `mandala.fleet` JSONL event protocol as the engine↔frontend contract.
## Requirements
### Requirement: Dispatch-only CLI over the inventory
The system SHALL provide a `mandala` CLI, packaged in the mandala repo as a
single static binary, that resolves `@group`/members from the versioned
aggregate `flake.mandala` output and dispatches to effect engines, containing
no orchestration logic of its own. Fleet-generic engines (deploy, ansible)
SHALL ship built in. `mandala-core` SHALL be consumable as a Rust library
exposing the CLI builder and an engine-registration API, so that
operator-specific engines are composed at compile time: an operator package
(for example, the fictional `acme` operator package) builds its own `mandala` binary linking its engines, and
the composed environment puts that binary on PATH. The public repo SHALL
contain no reference to operator engines, and the public binary SHALL work
with no operator engines present.

#### Scenario: an operator engine plugs in without touching the public CLI
- **WHEN** the composed devshell provides the operator's downstream `mandala`
  binary (an operator package linking mandala-core plus its engines)
- **THEN** the operator's engines appear as first-class subcommands sharing
  the in-process Inventory, and the public package contains no reference to
  them

#### Scenario: a group selector resolves to projected members
- **WHEN** the operator runs a command with an `@group` selector
- **THEN** the CLI expands it to exactly the members the inventory projects
  for that group and dispatches to the chosen engine

#### Scenario: the public binary stands alone
- **WHEN** the public `mandala` binary runs in an environment with no
  operator engine package
- **THEN** every built-in command works, and only fleet-generic engines are
  listed

### Requirement: Read-only explorer tier
The system SHALL provide a TUI fleet explorer + drift dashboard, served
natively by the single static `mandala` binary, that only reads the
inventory; explorer and dashboard views never mutate fleet state.

#### Scenario: explorer views cannot mutate state
- **WHEN** the operator browses the fleet explorer or drift dashboard
- **THEN** no deploy, push, or write action is issued from those views

#### Scenario: the TUI ships in the same binary
- **WHEN** the operator runs `mandala tui` from the composed environment
- **THEN** the explorer opens from the same static binary that serves the
  CLI and MCP surfaces, with no separate runtime or interpreter involved

### Requirement: Deploy-runner view
The TUI SHALL provide a deploy-runner view that drives the native deploy
engine (`mandala deploy run`) as a subprocess and presents it — the
`--limit` guard, throttle, target-owned switch preflight, boot-only staging,
and deploy-rs magic rollback are never bypassed. Each deploy run SHALL write
its event streams into a discoverable per-user run registry under the Mandala
state directory rather than a private temporary directory, so additional
observers — a second TUI, the CLI, or the fleet MCP server — can attach to an
in-flight or recent run and render its per-host state from the same event
streams. Closing the deploy view SHALL detach the observer and never terminate
the run — launched and attached runs behave identically, because every run is
engine-owned and independent of any frontend. Terminating a run SHALL be a
distinct, explicitly confirmed action, never a side effect of leaving the
view.

#### Scenario: the batch build renders as a live tree
- **WHEN** the operator launches a deploy from the TUI
- **THEN** the eval-once batch build renders as a native derivation forest in the build pane, fed from the engine's build event stream

#### Scenario: per-host streams are demuxed and inspectable
- **WHEN** disposition and activation fan out to multiple hosts
- **THEN** each host gets its own tab, color-coded by state including
  confirmed, reboot-pending, rolled-back, and failed, with its full
  preflight/staging/activation stream inspectable, and a failed/rolled-back
  host is visibly flagged without aborting the others

#### Scenario: a run is discoverable by another frontend
- **WHEN** a deploy is launched from one frontend and another observer queries the run registry
- **THEN** the in-flight or recent run appears in the registry and the observer attaches to its event streams without the launching frontend exposing private state

#### Scenario: leaving the view backgrounds the run
- **WHEN** the operator dismisses the deploy view while the run is live —
  whether this TUI launched the run or attached to it
- **THEN** the view closes, the run continues under its supervisor, and the
  post-settlement refresh behavior still fires when it finishes

#### Scenario: termination is deliberate
- **WHEN** the operator invokes the terminate action on a live run
- **THEN** a confirmation gates it, and only on confirmation is the run's
  process group terminated, with the recorded outcome rendered

### Requirement: Selection-targeted action tier
The TUI SHALL provide actions (ping, reboot, deploy) that target the
explorer's selection — multi-select with file-manager semantics (toggle,
contiguous range, cursor movement without selection change), falling back
to the cursor row — and run them in pushed screens, never inline in the
read-only views. Mutating actions SHALL sit behind explicit confirmation,
and SHALL dispatch through the operator's wrapper scripts or playbooks
when present (which carry controller-side environment and guards the raw
tools lack), never bypassing those guards.

#### Scenario: a mutation requires deliberate confirmation
- **WHEN** the operator triggers reboot or deploy on a selection
- **THEN** a confirmation modal gates the action, and the underlying
  playbook's own guards (--limit, drain handling) remain in force

#### Scenario: the selection becomes the limit
- **WHEN** multiple members (or groups) are selected and an action runs
- **THEN** the action targets exactly the selected names, comma-joined
  into the ansible limit

#### Scenario: reboot gathers batch order and drain safety
- **WHEN** the operator triggers reboot on a selection
- **THEN** the gating modal collects the batch order (serial =
  one-at-a-time, rolling = a small batch in flight, or all-at-once) and
  whether Kubernetes nodes are cordoned and drained first, and passes
  them to the reboot playbook as the `reboot_serial` and `drain`
  extra-vars — so the playbook's `serial`/drain behaviour is chosen at
  invocation, while its --limit guard remains in force

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
in-sync, reboot-pending, or drift. A drift refresh SHALL run expected-toplevel
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

### Requirement: Deploy UI completion is rollback-aware
The deploy-runner SHALL derive its terminal success indicator, exit value,
summary heading, and colors from the effective fleet deploy result, which
combines the controller process exit with sticky per-host outcomes. Any
`failed` or `rolled-back` host MUST produce a failed/red overall completion.
`reboot-pending` SHALL be a successful but visibly incomplete terminal state,
counted separately from confirmed, and a run containing it MUST NOT display
“all hosts confirmed.”

#### Scenario: zero process exit with a rollback
- **WHEN** a deploy process exits zero and the event stream contains a rolled-back host
- **THEN** the deploy screen presents a failed overall summary, retains the rolled-back host tab and diagnostics, and returns a non-zero result

#### Scenario: successful mixed disposition
- **WHEN** a deploy finishes with confirmed and reboot-pending hosts and no failed or rolled-back host
- **THEN** the screen returns success, distinguishes both counts and colors, and states that the staged hosts require reboot

#### Scenario: attached UI observes a pre-fix run record
- **WHEN** an attached TUI reads run metadata containing `rc: 0` and durable events containing a failed or rolled-back host
- **THEN** it reports the run as failed rather than trusting the stale successful metadata

### Requirement: Deploy runs are always registry-backed
Every native-engine deploy run SHALL write the versioned per-host event
streams and run metadata to the registry unconditionally — there is no
opt-in — while keeping terminal output human-usable for headless runs.
Frontends SHALL render runs from the streams without knowing which engine
emitted them. The engine SHALL be the sole registry owner: after successful
preflight it SHALL publish and flush the run id before build work begins;
launching frontends SHALL use their durable process supervisor and attach to
that engine-owned run rather than preallocating registry state.

#### Scenario: a headless run is attachable afterward
- **WHEN** `mandala deploy run` executes in a plain terminal with no TUI or
  MCP observer
- **THEN** the run is registered with full event streams, and a later
  frontend renders its per-host outcome from the registry

### Requirement: Native build-forest rendering
The TUI SHALL render nix build progress with `nix-output-monitor` (`nom
--json`) hosted in a pane-sized PTY and fed verbatim internal-json build
events. It SHALL attach the feed before the first event poll, preserve early
records for owned and attached runs, propagate pane resizes, EOF the renderer
when the batch build completes, and degrade to a non-fatal notice if the
renderer cannot start. Packaged Mandala binaries SHALL carry
`nix-output-monitor` as an explicit runtime dependency.

Mandala SHALL also parse the same internal-json stream into native structured
state: a bounded activity projection with per-derivation status, recent owned
logs, transfer endpoints/progress, elision counts, and a complete diagnostic
forest. Headless output SHALL render from that native state and SHALL NOT
scrape `nom` output.

The build pane SHALL be a first-class text pane: it takes the standard theme,
focus indication, and scrollback machinery. Headless runs (CLI, CI) SHALL
render live progress and a final summary from the same state, with no tty
requirement. Unknown message/activity types SHALL be tolerated (ignored but
counted), never fatal. Successful per-derivation durations SHALL be persisted
under the Mandala state directory and used to show an ETA when history for
that derivation name exists; cache failures SHALL NOT fail a build.

#### Scenario: the fleet-scale build pane uses nom
- **WHEN** the operator views a build whose complete derivation graph is larger
  than the terminal pane
- **THEN** the pane displays `nix-output-monitor`'s activity-focused rendering
  from the live internal-json stream

#### Scenario: current builder output appears above the forest
- **WHEN** concurrent derivations emit build log lines
- **THEN** the pane renders a bounded owner-labelled recent tail above the
  activity forest without losing the full per-status summary

#### Scenario: transfer progress identifies available endpoints
- **WHEN** Nix emits a download, substitution, or store-copy activity with
  endpoint and progress fields
- **THEN** the pane identifies what is moving, the available source/destination
  or host, and formatted progress without inventing absent endpoint data

#### Scenario: the build pane is a first-class pane
- **WHEN** the operator views a deploy's build tab
- **THEN** the PTY renderer fills the pane, remains focusable, follows terminal
  resize, and does not leak its child process when the screen closes

#### Scenario: a failed derivation is visible at both levels
- **WHEN** one derivation fails during a batch build
- **THEN** the activity forest flags the failed derivation, retains its recent
  diagnostic tail, and the summary reflects the failure count while unaffected
  derivations continue rendering

#### Scenario: headless runs get real progress
- **WHEN** a deploy or build runs in a plain terminal without the TUI
- **THEN** live progress and a final build summary render from the same forest
  state, with no tty or emulator involved

#### Scenario: historical durations inform an ETA
- **WHEN** a planned or building derivation has successful timing history
- **THEN** its forest snapshot and renderers expose the averaged remaining
  estimate, and the completed duration updates the persistent history

#### Scenario: packaged renderer availability
- **WHEN** the composed environment is built
- **THEN** `nix-output-monitor` is available to the TUI without relying on an
  ambient development-shell `PATH`

### Requirement: Automatic local contract reload
The TUI SHALL observe the current Git commit of its resolved local flake
checkout without blocking the terminal loop. When that commit changes, it
SHALL automatically adopt an inventory freshly evaluated from the new commit.
Dirty-worktree transitions without a commit change SHALL NOT trigger automatic
evaluation, and a non-local or non-Git flake SHALL continue without a watcher.

#### Scenario: Commit moves while the TUI is open
- **WHEN** the checkout's current commit changes after the TUI has started
- **THEN** the TUI automatically reloads and renders the inventory evaluated from the new commit without an operator keypress

#### Scenario: Worktree edit does not trigger
- **WHEN** tracked files become dirty but the checkout's current commit does not change
- **THEN** the TUI performs no automatic inventory evaluation and manual reload remains available

#### Scenario: Commit moves during the initial evaluation
- **WHEN** the checkout commit changes while the TUI's initial inventory evaluation is still running
- **THEN** the stale generation does not become authoritative and a queued reload converges on the newest observed commit

#### Scenario: Unsupported flake reference degrades cleanly
- **WHEN** the TUI runs against a flake reference for which a local Git HEAD cannot be resolved
- **THEN** the TUI continues operating without automatic watching and without reporting a watcher failure

### Requirement: Runs are re-attachable for their registry lifetime
Any run in the registry — live or settled, launched by any frontend — SHALL
be attachable and re-attachable: detaching an observer never consumes the
run's attachability. The TUI SHALL provide a runs view listing registry
runs (identity, kind, target, liveness, phase, timing, result) from which
the operator attaches to a selected run; and a direct CLI entry SHALL open
an observer screen on a named run id, exiting with the run's effective
result when observed to settlement. Automatic attachment on an
MCP-triggered launch SHALL occur at most once per run per TUI session and
only when no other screen is open — it never prevents later manual
attachment.

#### Scenario: a detached run is foregrounded again
- **WHEN** the operator detaches from a live deploy and later selects it in
  the runs view
- **THEN** the deploy view re-attaches to the same run and renders its
  current per-host state and build progress from the registry streams

#### Scenario: an MCP-launched run is foregrounded on demand
- **WHEN** an MCP client launches a deploy while the operator's TUI shows
  another screen (or no TUI is running)
- **THEN** the run is reachable afterwards — from the runs view, or by the
  direct attach entry naming its run id — and renders exactly as an
  attached run

#### Scenario: auto-attach does not re-summon a dismissed screen
- **WHEN** an MCP-triggered run auto-attached once and the operator
  detached from it
- **THEN** no further automatic attachment of that run occurs in the
  session, while manual attachment from the runs view still works

#### Scenario: the attach entry validates its identifier
- **WHEN** the direct attach entry is invoked with a malformed or unknown
  run id
- **THEN** it refuses with a structured error before touching any file
  outside the run registry

### Requirement: Boot mode is selectable and visible across CLI and TUI
The headless deploy CLI and standalone deploy TUI SHALL accept a `--boot`
per-run option. The explorer TUI SHALL expose the same option in its gated
deploy flow. Owned and attached deploy views SHALL render whether the run
requested boot mode.

#### Scenario: headless and standalone commands request boot mode
- **WHEN** the operator invokes either `mandala deploy run` or `mandala tui
  deploy` with `--boot`
- **THEN** the launched native deploy run carries the boot override

#### Scenario: explorer confirmation selects boot mode
- **WHEN** the operator toggles boot mode in the explorer deploy confirmation
  and confirms the action
- **THEN** the explorer launches the same boot-mode native deploy run while
  retaining its existing confirmation gate

#### Scenario: attached view reconstructs boot intent
- **WHEN** a TUI attaches to a registry run whose metadata records boot mode
- **THEN** the deploy subtitle and terminal summary identify the run as boot
  mode regardless of which frontend launched it

### Requirement: Uniform keyboard navigation
The TUI SHALL accept arrow keys and `hjkl` as equivalent navigation in
every view (`h`/`l` previous/next tab or leftward/rightward focus, `j`/`k`
down/up), plus `g`/`G` (top/bottom), `Ctrl-d`/`Ctrl-u` (half page), and
PageUp/PageDown in scrollable contexts. Bindings SHALL be defined in one
keymap layer from which the footer hints derive, so displayed hints can
never desync from actual bindings, and a letter binding SHALL never shadow
a different action in the same context.

#### Scenario: hjkl mirrors the arrows
- **WHEN** the operator navigates any view with `hjkl`
- **THEN** the behavior is identical to the corresponding arrow keys, in
  every view

#### Scenario: hints reflect the keymap
- **WHEN** a view's available actions are rendered in the footer
- **THEN** every shown hint corresponds to a live binding in the current
  context, derived from the keymap definition

### Requirement: Mouse interaction
The TUI SHALL support click and wheel input: clicking focuses and activates
targets the keyboard can reach (tabs, table rows, modal choices), and the
wheel scrolls the pane under the cursor. Every mouse action SHALL map to an
action reachable by keyboard — the TUI remains fully operable without a
mouse, and a terminal where capture fails degrades to keyboard-only rather
than erroring. Mouse capture SHALL be released on suspend, panic, and quit.

#### Scenario: click activates a keyboard-reachable target
- **WHEN** the operator clicks a host tab or a table row
- **THEN** the same state change occurs as via the keyboard binding
  (tab focused, cursor moved/row selected)

#### Scenario: capture never outlives the TUI
- **WHEN** the TUI suspends, panics, or quits
- **THEN** the terminal's mouse handling is restored along with the rest
  of the terminal state

### Requirement: Text-pane scrollback with follow mode
The TUI SHALL provide scrollback in every text output pane (task screens,
attached log tails, deploy host, ansible, and native build-forest tabs, and the
activity pane) with follow-mode semantics: pinned to the tail until the
operator scrolls up, viewport stable against new lines while unpinned,
re-pinned by scroll-to-bottom (`G`, End, or wheel to tail), with a visible
scrollbar indicating position.

#### Scenario: reading scrollback during a live run
- **WHEN** the operator scrolls up in a deploy host tab while the host
  keeps streaming
- **THEN** the viewport holds position as new lines arrive, and returning
  to the bottom re-pins tail-follow

#### Scenario: position is visible
- **WHEN** a text pane is scrolled away from the tail
- **THEN** a scrollbar shows the viewport's position in the buffer

### Requirement: Focus and state affordances
The TUI SHALL visually indicate the focused pane (distinct border/title
styling from one theme layer) and SHALL surface deploy progress as a
gauge (completed/total hosts) in the deploy view, alongside the existing
per-host state colors.

#### Scenario: focus is unambiguous
- **WHEN** multiple panes are visible
- **THEN** exactly one pane renders with the focused styling and input
  routes to it

#### Scenario: deploy progress at a glance
- **WHEN** a multi-host deploy is in flight
- **THEN** the deploy view renders a progress gauge reflecting completed
  versus total hosts
