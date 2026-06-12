# mandala-fleet (CLI)

Fleet porcelain: dispatch + present, nothing else. The CLI reads the
fleet exclusively through the versioned aggregate output
(`nix eval --json .#mandala` — see the fleet flakeModule); it never
scrapes per-tool outputs and never implements orchestration — deploy-rs,
ansible, and sops stay the engines.

- Import package: `mandala_fleet` (PyPI-style dist name `mandala-fleet`;
  nix-only distribution for now).
- Engines are Typer sub-apps discovered through the `mandala.engines`
  entry-point group. The built-in `deploy` and `ansible` engines register
  through the same group an operator plugin package uses — no privileged
  path. v1 plugin surface: a sub-app + the inventory core + (optionally)
  emitting the `mandala.fleet` JSONL event protocol.
- The TUI layers (see the fleet-cli-tui change) render events without
  knowing which engine emitted them.

```sh
mandala members            # the merged member view, one line per member
mandala groups             # taxonomy groups -> members
mandala deploy run -l k3s  # fan-out deploy via mandala.fleet.deploy
mandala ansible inventory  # the projected inventory, as JSON
```
