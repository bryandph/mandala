## Decisions
The diagnostic log-format addition was approved before implementation; copy behavior and defaults are unchanged.

Use `nix copy --log-format internal-json` to retain machine-readable activity progress without changing copy semantics. Decode activity 103 (`actCopyPaths`) result 105 completed paths and activity 100 (`actCopyPath`) result 105 cumulative NAR bytes; sum the latest progress per activity, never individual updates. Bytes mean controller-to-destination uncompressed NAR payload, not SSH traffic or destination cache downloads. Ignore unrelated activity types. Nix 2.34 source defines these activities; unsupported or silent output leaves counts absent, not invented zero. Explicit reported zero remains zero. Human messages are decoded rather than dumping protocol noise.

Emit additive `copy` events at start and completion, including on spawn/process failure. Fields are `substitute_on_destination`, `duration_ms`, `success` and optional `paths_copied` / `controller_nar_bytes`; completion-only fields are absent at start. Existing milestones and sticky outcomes remain unchanged. Reconstruct a typed optional per-host projection from events, so no concurrent metadata update is needed. Legacy records have no projection. Frontends show unknown counts explicitly, never as zero.

## Protocol verification
Checked upstream Nix 2.34.0 `store-api.cc`, `legacy-ssh-store.cc` and `logging.hh`, and locally probed Nix 2.34.7+1 (the development shell) and 2.34.9+1 against isolated temporary local destination stores. Both emitted activity 103 path progress and activity 100 NAR-byte progress. No fleet host or live cache was used. The sanitized replay fixture is `crates/mandala-deploy/src/fixtures/copy-nix-2.34.jsonl`; automated deployment tests invoke only stub Nix/SSH programs. Realisation emits an empty activity-103 placeholder, which is excluded; only nonempty root copy activities contribute path progress. Byte activities must name the requested destination, excluding destination-side cache copies. Missing progress for any selected activity makes that aggregate unknown rather than silently undercounting.

A second copy to the same temporary store emitted `copying 0 paths` and only expected-byte result 106, not completed-progress result 105. Its counts therefore remain absent: neither human descriptions nor expected-byte counts are treated as completed measurements. Explicit zero completed-progress values are retained. These are internal Nix interfaces, not a guarantee for other versions; unsupported/malformed output logs a warning and cannot change the copy result.

## Example completion record
```json
{"v":2,"ts":42.0,"host":"alpha","plugin":"deploy","event":"copy","controller_nar_bytes":4096,"duration_ms":125,"paths_copied":2,"substitute_on_destination":true,"success":true}
```
The durable projection lives in host events, not `meta.json`; independent concurrent host writers never race to update shared metadata. MCP exposes it as `hosts.<name>.copy`, and text observers use `unknown` for absent counts. Failed completion events may contain partial progress. Start events contain only the mode.

## Alternatives
Alternatives: closure-size estimates cannot measure actual transfer; parsing translated human summaries is brittle; querying remote stores adds effects and races. Internal JSON is version-sensitive, so malformed/unknown records must not fail deployments and unknown counts remain absent.
