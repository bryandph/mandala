"""Selector resolution over an injected aggregate (no nix eval)."""

import pytest

from mandala_fleet.inventory import Inventory, InventoryError


def _inv() -> Inventory:
    inv = Inventory(flake=".")
    # cached_property: inject the aggregate instead of shelling to nix.
    inv.__dict__["aggregate"] = {
        "schemaVersion": 1,
        "members": {"web": {}, "cache": {}, "router": {}},
        "groups": {"k3s": ["cache", "web"], "gateway": ["router"]},
        "projections": {},
    }
    return inv


def test_group_selector_expands_to_projected_members() -> None:
    assert _inv().resolve("@k3s") == ["cache", "web"]


def test_member_and_union_selectors() -> None:
    inv = _inv()
    assert inv.resolve("router") == ["router"]
    assert inv.resolve("@k3s,router") == ["cache", "router", "web"]


def test_unknown_selectors_fail_by_name() -> None:
    with pytest.raises(InventoryError, match="no such group: nope"):
        _inv().resolve("@nope")
    with pytest.raises(InventoryError, match="no such member: ghost"):
        _inv().resolve("ghost")


def test_to_limit_pins_groups_but_passes_plain_lists() -> None:
    inv = _inv()
    assert inv.to_limit("@k3s") == "cache,web"
    assert inv.to_limit("web,cache") == "web,cache"  # untouched, no @
