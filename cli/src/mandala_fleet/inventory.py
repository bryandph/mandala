"""Inventory core: the one read path onto a fleet.

Everything the CLI (and every plugged engine) knows about a fleet comes
from `nix eval --json <flake>#mandala` — the versioned aggregate the
fleet flakeModule emits. One eval, pure data, gated by schemaVersion.
Engines MUST take their member/group views from here so `mandala deploy
@k3s`, `ansible -l k3s`, and `.#deployBatch.k3s` resolve the same set.
"""

from __future__ import annotations

import json
import subprocess
from dataclasses import dataclass
from functools import cached_property

from . import SUPPORTED_SCHEMA_VERSION


class InventoryError(RuntimeError):
    pass


@dataclass
class Inventory:
    """Lazy view over one flake's aggregate output."""

    flake: str = "."

    @cached_property
    def aggregate(self) -> dict:
        try:
            out = subprocess.run(
                [
                    "nix", "eval", "--no-warn-dirty", "--json",
                    f"{self.flake}#mandala",
                ],
                check=True,
                capture_output=True,
                text=True,
            ).stdout
        except FileNotFoundError as e:
            raise InventoryError("nix not found on PATH") from e
        except subprocess.CalledProcessError as e:
            raise InventoryError(
                f"evaluating {self.flake}#mandala failed:\n{e.stderr.strip()}"
            ) from e

        data = json.loads(out)
        version = data.get("schemaVersion")
        if version != SUPPORTED_SCHEMA_VERSION:
            raise InventoryError(
                f"aggregate schemaVersion {version} unsupported "
                f"(this CLI understands {SUPPORTED_SCHEMA_VERSION})"
            )
        return data

    @property
    def members(self) -> dict[str, dict]:
        return self.aggregate["members"]

    @property
    def groups(self) -> dict[str, list[str]]:
        return self.aggregate["groups"]

    def resolve(self, selector: str) -> list[str]:
        """`@group` or member name -> sorted member names. Comma-separated
        selectors union (`@k3s,vishnu`)."""
        if "," in selector:
            names: set[str] = set()
            for part in selector.split(","):
                if part:
                    names.update(self.resolve(part))
            return sorted(names)
        if selector.startswith("@"):
            group = selector[1:]
            try:
                return sorted(self.groups[group])
            except KeyError as e:
                raise InventoryError(f"no such group: {group}") from e
        if selector not in self.members:
            raise InventoryError(f"no such member: {selector}")
        return [selector]

    def to_limit(self, selector: str) -> str:
        """Selector -> explicit ansible --limit list. `@group` expands to
        the exact projected members, so the fan-out target set is pinned
        by the CLI's resolution, not re-derived ansible-side."""
        if "@" not in selector:
            return selector
        return ",".join(self.resolve(selector))
