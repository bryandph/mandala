## Implementation
- [x] Add typed copy telemetry and tolerant internal-JSON decoding.
- [x] Persist per-host events and expose registry/CLI/TUI/MCP projections.
- [x] Add effect-isolated stub tests for modes, counts, empty copies, failures and legacy absence.

## Verification
- [ ] Run cargo test --workspace and scoped clippy --no-deps -D warnings.
- [ ] Run all local CI steps from .github/workflows/ci.yml, including nix fmt -- --check . and nix flake check (which builds the platform checks).
- [ ] Verify public-content neutrality and document Nix versions/count semantics.
