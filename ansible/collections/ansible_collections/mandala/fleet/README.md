# mandala.fleet

Generic fleet adapters: ansible action plugins that wrap `nix build` and
`deploy-rs` on the controller, plus the eval-once + fan-out deploy
playbook. Extracted from an operator collection — nothing in here names a
real fleet; everything fleet-specific arrives from the projected inventory
(`mandala`'s ansible projection) and task arguments.

Two output modes:

- **tty** — `nix-output-monitor` (nom) owns `/dev/tty` and renders a live
  build tree. Used when a controlling terminal is available.
- **flat** — no nom; we parse Nix's `--log-format internal-json` ourselves
  and emit one summary line every ~2 seconds via ansible's display.
  Append-only and safe under `tee`, CI, or piped stdout.

Mode is selected per task via `output_mode: auto|tty|flat` (default `auto`,
which probes `/dev/tty`).

## Modules

### `mandala.fleet.build`

Wraps `nix build`. Returns:

- `out_paths` — store paths from `--print-out-paths`
- `summary` — `{built, fetched, errors, duration_s}` (parsed from internal-json)
- `mode` — the actual mode used after auto-resolution / fallback
- `build_log` — on failure, a compact dump of errors + the largest captured
  per-derivation build log (last 200 lines)

`changed` is `true` only when `summary.built > 0` (a pure cache-hit build
reports `changed: false`, so handlers no longer fire on no-op rebuilds).

```yaml
- mandala.fleet.build:
    installable: .#nixosConfigurations.example.config.system.build.toplevel
    cwd: "{{ playbook_dir }}/.."

- mandala.fleet.build:                # force flat output for clean CI logs
    installable: nixpkgs#hello
    output_mode: flat
```

### `mandala.fleet.deploy`

Two-phase wrapper for deploy-rs:

1. **Prebuild** (auto-enabled when `target` matches `.#<name>`): builds
   `.#deploy.nodes.<name>.profiles.system.path` with the same dual-mode
   output as `mandala.fleet.build`. Disable with `prebuild: false` or
   override the attribute with `prebuild_attr`.
2. **Deploy**: invokes `deploy` against `target` (or `targets`). deploy-rs
   emits free-form text status, NOT internal-json, so its output is
   line-streamed raw through ansible's display.

When prebuild runs, `--skip-checks` is forced for phase 2 (the derivation is
already in the store; deploy-rs's `nix flake check` would duplicate work, and
often fails in flakes that need `--impure`).

## Playbook: `mandala.fleet.deploy` (fan-out)

`playbooks/deploy.yml` — the eval-once, fan-out fleet deploy:

- Play 1 (controller, run_once): batch-builds EVERY targeted host's
  deploy-rs profile in ONE `nix build` — one flake eval, one build
  schedule. Refuses to run without an explicit `--limit`.
- Play 2 (per host, throttled): `deploy --skip-checks .#<host>` — deploy-rs
  stays the per-host activation primitive (copy, activation, magic-rollback
  over fresh ssh), so a bricked host rolls back without aborting the rest.

Targets the `deploy_rs` guard group from the projected inventory (members
deploy-rs can activate). Variables: `fleet_flake_root` (flake with
`deploy.nodes`; default `$PWD`), `deploy_throttle` (default 4),
`deploy_dry_activate` (default false).

```sh
ansible-playbook mandala.fleet.deploy -l k3s
ansible-playbook mandala.fleet.deploy -l host-a,host-b -e deploy_dry_activate=true
```

## Event channel (protocol v1)

When `MANDALA_FLEET_EVENTS` names a writable directory, both plugins
append JSONL events to `<dir>/<inventory_hostname>.jsonl`: `status`
(start/done + rc), `line` (raw output), `progress` (build counters), and
`milestone` (parsed deploy-rs transitions:
`eval|build|copy|activate|wait|confirm|rollback`). Every record carries
`v` (protocol version), `ts`, `host`, and `plugin`. Porcelain (a TUI, a
log collector) tails the files to render per-host state — including
flagging a rolled-back host while the rest of the fan-out proceeds —
without parsing display output.

**The protocol is the plugin↔porcelain contract: any shape change bumps
`PROTOCOL_VERSION`** (`plugins/module_utils/events.py`). With the variable
unset, no emitter is constructed and headless output is byte-identical to
the channel-less behavior. Emission is best-effort; channel I/O errors
never fail a deploy.

## Requirements

`nix` and `deploy-rs` on the ansible controller; `nix-output-monitor`
(nom) only for `tty` mode — `flat` runs without it.

## Notes

- `display.display()` is thread-safe in ansible-core (Display lock), but
  every call is serialized through the worker fork's `_final_q` and rendered
  by the parent — that's why `nom`'s ANSI redraw frames cannot be tunneled
  through it. `tty` mode bypasses Display by handing nom the `/dev/tty` fd
  directly; `flat` mode emits one append-only summary line at a time, which
  Display handles cleanly.
- There is no ansible callback hook that fires *during* a running task; the
  execution model is fork-and-wait. That is why this collection uses an
  action plugin + subprocess rather than a stdout callback — and why the
  event channel is plugin-emitted JSONL files rather than callback output.
