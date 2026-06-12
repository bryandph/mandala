#!/usr/bin/python
# -*- coding: utf-8 -*-

DOCUMENTATION = r"""
---
module: build
short_description: Run `nix build` on the controller with dual-mode output
description:
  - Wraps C(nix build) on the controller. Output is rendered in one of two modes selected by C(output_mode).
  - In C(tty) mode the build is piped through C(nix-output-monitor) (nom) which owns C(/dev/tty) and renders a live tree. Falls back to C(flat) if C(/dev/tty) or C(nom) is unavailable.
  - In C(flat) mode there is no nom; the action plugin parses Nix's C(--log-format internal-json) directly and emits one summary line every ~2 seconds via ansible's display. Append-only and safe under C(tee), CI, or piped stdout.
  - In C(auto) mode (the default) C(tty) is chosen when C(/dev/tty) opens and is a tty, otherwise C(flat).
  - The actual logic lives in the action plugin; this module shell only declares the argument spec.
options:
  installable:
    description: Flake reference or installable to build (e.g. C(.#nixosConfigurations.vishnu.config.system.build.toplevel)). Mutually exclusive with C(installables); one of the two is required.
    type: str
  installables:
    description: Multiple installables built in ONE C(nix build) invocation — one eval, one build schedule (the deploy fan-out's batch phase). Mutually exclusive with C(installable).
    type: list
    elements: str
  out_link:
    description: Path passed to C(--out-link). When unset, C(--no-link) is used.
    type: path
  print_out_paths:
    description: Pass C(--print-out-paths) and capture the resulting paths into the C(out_paths) return.
    type: bool
    default: true
  impure:
    description: Pass C(--impure) to allow impure evaluation.
    type: bool
    default: false
  cwd:
    description: Working directory for the C(nix build) invocation.
    type: path
  extra_args:
    description: Additional arguments appended verbatim to the C(nix build) command line.
    type: list
    elements: str
    default: []
  output_mode:
    description: Output rendering mode. C(auto) probes C(/dev/tty); C(tty) forces nom; C(flat) forces internal-json summary lines.
    type: str
    choices: [auto, tty, flat]
    default: auto
  nom_bin:
    description: Path or name of the C(nix-output-monitor) binary. Used only in C(tty) mode.
    type: str
    default: nom
author:
  - Bryan Prather-Huff (@bryandph)
"""

EXAMPLES = r"""
- name: Build the turing-rk1 installer SD image
  mandala.fleet.build:
    installable: .#nixosConfigurations.turing-rk1-installer.config.system.build.sdImage
    cwd: "{{ playbook_dir }}/.."

- name: Build a small derivation with an out-link
  mandala.fleet.build:
    installable: nixpkgs#hello
    out_link: /tmp/result-hello

- name: Force flat output for clean CI logs
  mandala.fleet.build:
    installable: nixpkgs#hello
    output_mode: flat
"""

RETURN = r"""
out_paths:
  description: Out-paths printed by C(--print-out-paths). Empty list if disabled.
  returned: success
  type: list
  elements: str
summary:
  description: Counters parsed from Nix's internal-json event stream.
  returned: always
  type: dict
  contains:
    built:
      description: Number of derivations actually built (0 means everything was cached).
      type: int
    fetched:
      description: Number of paths substituted from a binary cache.
      type: int
    errors:
      description: Number of error-level messages observed.
      type: int
    duration_s:
      description: Wall-clock duration of the build, in seconds.
      type: float
mode:
  description: The output mode actually used (C(tty) or C(flat)) after auto-resolution and any fallback.
  returned: always
  type: str
build_log:
  description: On failure, a compact dump of error messages and the largest captured per-derivation build log (last 200 lines).
  returned: failure
  type: str
rc:
  description: Exit code of the C(nix build) process.
  returned: always
  type: int
cmd:
  description: Argv as it was executed (for diagnostic purposes).
  returned: always
  type: list
  elements: str
"""
