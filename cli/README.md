# mandala-fleet (Python: TUI + cores)

Fleet porcelain: dispatch + present, nothing else. The package reads the
fleet exclusively through the versioned aggregate output
(`nix eval --json .#mandala` — see the fleet flakeModule); it never
scrapes per-tool outputs and never implements orchestration — deploy-rs,
ansible, and sops stay the engines.

The headless surfaces (root fleet views, the deploy/ansible engines, the
stdio MCP server) live in the Rust porcelain (`crates/`). What remains
here, until the rewrite's phase 2:

- The cores: `inventory`/`drift`/`runner`/`registry` — shared by the TUI
  and the interop golden-fixture generator (`tests/fixtures/interop/`).
- The Textual TUI tiers (`mandala tui`): read-only explorer + drift
  dashboard; deploy runner (`mandala tui deploy`).
- `mcp/`: the TUI-hosted loopback HTTP MCP server (`mandala tui --mcp`)
  and the golden-fixture capture script (`tests/fixtures/mcp/`).
- Import package: `mandala_fleet` (PyPI-style dist name `mandala-fleet`;
  nix-only distribution for now). Console script `mandala` — reached via
  the parent devshell's `mandala-py` wrapper and the Rust binary's
  `mandala tui` exec-shim.

```sh
mandala-py tui                  # fleet explorer (+ --mcp HTTP host)
mandala-py tui deploy -l @k3s   # deploy-runner view
```
