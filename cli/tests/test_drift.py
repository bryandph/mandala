"""Drift core: cache semantics, snapshot trust boundary, staleness."""

import json
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

from mandala_fleet import drift

NOW = datetime(2026, 6, 12, 12, 0, 0, tzinfo=timezone.utc)


def _snap(tmp_path: Path, stem: str, **fields) -> None:
    body = {"host": stem, "unreachable": False,
            "current": "/nix/store/aaa-x", "booted": "/nix/store/aaa-x",
            "captured_at": NOW.isoformat(), **fields}
    (tmp_path / f"{stem}.json").write_text(json.dumps(body))


def test_expected_cache_roundtrip_and_freshness(tmp_path: Path) -> None:
    drift.save_expected("rev1", {"web": "/nix/store/aaa-x"}, tmp_path)
    rev, toplevels = drift.load_expected(tmp_path)
    assert rev == "rev1" and toplevels == {"web": "/nix/store/aaa-x"}

    assert drift.cache_fresh("rev1", "rev1")
    assert not drift.cache_fresh("rev1", "rev2")  # contract moved
    assert not drift.cache_fresh("rev1-dirty", "rev1-dirty")  # dirty never matches
    assert not drift.cache_fresh(None, "rev1")
    assert not drift.cache_fresh("rev1", None)


def test_snapshots_keyed_by_filename_not_embedded_host(tmp_path: Path) -> None:
    # A file claiming to be another host must not impersonate it.
    _snap(tmp_path, "evil", host="web", current="/nix/store/fake-x")
    snapshots = drift.read_snapshots(tmp_path)
    assert "web" not in snapshots
    assert snapshots["evil"]["current"] == "/nix/store/fake-x"


def test_stale_and_incomplete_are_distinct_judgements(tmp_path: Path) -> None:
    _snap(tmp_path, "old", captured_at=(NOW - timedelta(days=3)).isoformat())
    _snap(tmp_path, "broken", current=None, booted=None)
    _snap(tmp_path, "fresh")
    entries = {e.host: e for e in drift.compare(
        ["old", "broken", "fresh", "never"],
        drift.read_snapshots(tmp_path),
        {"old": "/nix/store/aaa-x", "broken": "/nix/store/aaa-x", "fresh": "/nix/store/aaa-x"},
        now=NOW,
    )}
    assert entries["old"].status == drift.DriftStatus.STALE
    assert entries["broken"].status == drift.DriftStatus.INCOMPLETE
    assert entries["never"].status == drift.DriftStatus.NO_SNAPSHOT
    assert entries["fresh"].status == drift.DriftStatus.IN_SYNC


def test_drift_and_reboot_pending(tmp_path: Path) -> None:
    _snap(tmp_path, "moved", current="/nix/store/bbb-x", booted="/nix/store/bbb-x")
    _snap(tmp_path, "pending", current="/nix/store/aaa-x", booted="/nix/store/zzz-old")
    entries = {e.host: e for e in drift.compare(
        ["moved", "pending"],
        drift.read_snapshots(tmp_path),
        {"moved": "/nix/store/aaa-x", "pending": "/nix/store/aaa-x"},
        now=NOW,
    )}
    assert entries["moved"].status == drift.DriftStatus.DRIFT
    assert entries["pending"].status == drift.DriftStatus.REBOOT_PENDING


def test_eval_expected_rejects_hostile_names() -> None:
    # The aggregate is a trust boundary: a name with '' would escape the
    # Nix indented string. Reject before any subprocess is spawned.
    with pytest.raises(ValueError, match="invalid member name"):
        drift.eval_expected(".", ["ok-host", "bad''(import <nixpkgs>)"])


def test_state_dir_resolved_at_call_time(monkeypatch, tmp_path: Path) -> None:
    monkeypatch.setenv("MANDALA_FLEET_STATE", str(tmp_path / "a"))
    assert drift.state_dir() == tmp_path / "a"
    monkeypatch.setenv("MANDALA_FLEET_STATE", str(tmp_path / "b"))
    assert drift.state_dir() == tmp_path / "b"  # not frozen at import
    monkeypatch.delenv("MANDALA_FLEET_STATE")
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path / "xdg"))
    assert drift.state_dir() == tmp_path / "xdg" / "mandala" / "fleet"
