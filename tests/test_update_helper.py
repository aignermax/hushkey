"""Behavior tests for update_helper.py — mocked subprocess, no real spawns."""

import sys

import pytest

import update_helper


@pytest.fixture
def helper(tmp_path, monkeypatch):
    """update_helper with all paths in tmp_path and subprocess mocked out."""
    marker = tmp_path / "update-pending"
    monkeypatch.setattr(update_helper, "MARKER", str(marker))
    monkeypatch.setattr(update_helper, "LOG", str(tmp_path / "update.log"))
    monkeypatch.setattr(sys, "argv", ["update_helper.py"])  # no old pid -> no wait
    calls = {"run": [], "popen": []}

    class Proc:
        returncode = 0
        pid = 123

    monkeypatch.setattr(update_helper.subprocess, "run",
                        lambda *a, **k: calls["run"].append(a) or Proc())
    monkeypatch.setattr(update_helper.subprocess, "Popen",
                        lambda *a, **k: calls["popen"].append(a) or Proc())
    return marker, calls


def test_no_marker_just_respawns_tray(helper):
    marker, calls = helper
    update_helper.main()
    assert calls["run"] == []          # no installer without an update marker
    assert len(calls["popen"]) == 1    # but the tray always comes back


def test_marker_runs_installer_and_is_consumed(helper):
    marker, calls = helper
    marker.write_text("")
    update_helper.main()
    assert calls["run"]                # installer ran
    assert not marker.exists()         # marker consumed on success
    assert len(calls["popen"]) == 1


def test_failed_installer_keeps_the_marker(helper, monkeypatch):
    """The marker is the safety net's trigger — it must survive a failure."""
    marker, calls = helper
    marker.write_text("")
    monkeypatch.setattr(update_helper.subprocess, "run",
                        lambda *a, **k: type("R", (), {"returncode": 1})())
    update_helper.main()
    assert marker.exists()             # next tray start retries the install
    assert len(calls["popen"]) == 1    # tray still respawns


def test_garbage_argv_does_not_crash_before_logging(helper, monkeypatch):
    marker, calls = helper
    monkeypatch.setattr(sys, "argv", ["update_helper.py", "not-a-pid"])
    update_helper.main()               # must not raise
    assert len(calls["popen"]) == 1
