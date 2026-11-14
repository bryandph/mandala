## Why
Operators need per-host measurements when choosing whether destinations substitute from their own caches.

## What changes
Record copy mode, duration and Nix-reported path and NAR-byte counts in additive durable host events. Expose them in registry readers, CLI status, TUI and MCP status.

## Non-goals
No copy, substitution, cache-publication, activation or default-setting changes. No network-wire accounting or destination cache-download estimates.
