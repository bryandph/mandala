---
type: Reference
title: Mandala knowledge bundle
description: Entry point for durable Mandala repository knowledge in OKF 0.2.
tags: [okf, knowledge, mandala]
status: draft
generated:
  by: worker-mandala-BPH-66-1/openai-gpt-6.1-sol
  at: 2026-11-15T05:38:27Z
sources:
  - id: bph-66
    resource: https://youtrack.k8s.bph/issue/BPH-66
---

# Mandala knowledge bundle

This minimal Open Knowledge Format 0.2 bundle migrates two Mandala memories
from nixspace for BPH-66, Phase 1 decision K2.[^bph-66] Both concepts were
checked against Mandala commit `0ddbf438258725739a21f32ecfa559d057207827`;
each records what was kept, corrected or dropped.

- [Build renderer split](/build-renderer-split.md) — why the interactive
  `nom` renderer and native structured forest have different consumers.
- [Evaluator reload freshness](/evaluator-reload-freshness.md) — why a
  moved checkout needs a new worker, and where reload ownership lives.

The concepts explain implementation and historical rationale. Behavior
contracts remain in the linked OpenSpec specifications, not in this bundle.

[^bph-66]: https://youtrack.k8s.bph/issue/BPH-66
