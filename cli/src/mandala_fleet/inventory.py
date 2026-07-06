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


def surfaces(member: dict) -> str:
    """Compact management-surface flags: a(nsible) d(eploy-rs) s(ops)."""
    d = member.get("deployment", {})
    return "".join([
        "a" if d.get("ansible", {}).get("enable") else "-",
        "d" if d.get("deployRs", {}).get("enable") else "-",
        "s" if d.get("sops", {}).get("recipient") else "-",
    ])


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
        """Selector taxonomy -> sorted member names. Parts (separated by
        `,` or ansible's `:`) union; a `!`-prefixed part excludes after
        the unions, and `all` is the whole membership — so `all,!vishnu`
        (or ansible-spelled `all:!vishnu`) is "everything except vishnu".
        A bare exclusion (`!@k3s`) implies `all` as the base set."""
        include: set[str] = set()
        exclude: set[str] = set()
        saw_include = False
        for part in selector.replace(":", ",").split(","):
            part = part.strip()
            if not part:
                continue
            negate = part.startswith("!")
            names = self._resolve_part(part[1:] if negate else part)
            if negate:
                exclude.update(names)
            else:
                saw_include = True
                include.update(names)
        if not saw_include:
            if not exclude:
                raise InventoryError("empty selector")
            include = set(self.members)
        resolved = sorted(include - exclude)
        if not resolved:
            raise InventoryError(f"selector resolves to no members: {selector}")
        return resolved

    def _resolve_part(self, part: str) -> list[str]:
        """One taxonomy atom: `all`, `@group`, or a member name."""
        if part == "all":
            return sorted(self.members)
        if part.startswith("@"):
            group = part[1:]
            try:
                return sorted(self.groups[group])
            except KeyError as e:
                raise InventoryError(f"no such group: {group}") from e
        if part not in self.members:
            raise InventoryError(f"no such member: {part}")
        return [part]

    def to_limit(self, selector: str) -> str:
        """Selector -> explicit ansible --limit list. Always fully
        resolved, so the fan-out target set (`all`, `@group`, exclusions)
        is pinned by the CLI's resolution, not re-derived ansible-side —
        and an unknown member is refused before anything launches."""
        return ",".join(self.resolve(selector))
