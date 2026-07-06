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


def test_to_limit_always_pins_the_resolved_set() -> None:
    inv = _inv()
    assert inv.to_limit("@k3s") == "cache,web"
    # Plain lists are canonicalized (sorted) and validated, so the confirm
    # string is stable however the operator spelled the selector.
    assert inv.to_limit("web,cache") == "cache,web"
    with pytest.raises(InventoryError, match="no such member: ghost"):
        inv.to_limit("ghost")


def test_all_keyword_and_exclusions() -> None:
    inv = _inv()
    assert inv.resolve("all") == ["cache", "router", "web"]
    assert inv.resolve("all,!router") == ["cache", "web"]
    assert inv.resolve("all,!@k3s") == ["router"]
    # A bare exclusion implies `all` as the base set.
    assert inv.resolve("!@k3s") == ["router"]
    with pytest.raises(InventoryError, match="resolves to no members"):
        inv.resolve("all,!all")
    with pytest.raises(InventoryError, match="empty selector"):
        inv.resolve("")


def test_ansible_colon_spelling() -> None:
    # `all:!vishnu`-style separators work like commas.
    assert _inv().resolve("all:!router") == ["cache", "web"]
    assert _inv().to_limit("@k3s:router") == "cache,router,web"
