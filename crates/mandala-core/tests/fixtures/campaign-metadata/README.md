# Fetcher-metadata regression fixture

The system/profile derivation embeds root and child fetcher metadata, including
`self.rev`/`self.dirtyRev`, `self.lastModified`, the full sourceInfo metadata and
both source/flake paths. Replacing the graph with bare path semantics changes its
drvPath; faithful capsule replay must preserve it. The builder is `/bin/false`
and the fictional destination is reserved `.invalid`: never deploy this fixture.

The Rust source-command fixture uses this source and substitutes all Nix/Git
commands. Its guard tests inject stripped profile or system metadata and require
a named refusal before binding. Real-Nix verification can additionally run an
explicit creation **preview only**, with a fresh temporary state directory, on
supported Nix 2.34.x. Capture itself compares both live/frozen profile and system
drvPaths; no attempt or host work is launched by preview. Exercise committed Git,
dirty Git and relative-input forms in temporary repositories, never by modifying
an operator source or deploying the fixture.
