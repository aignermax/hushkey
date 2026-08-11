"""Tests for tray.py — pure logic only; no icon is ever shown."""

import json
import time

import tray


def test_version_tuple_parsing():
    assert tray.version_tuple("0.1.0") == (0, 1, 0)
    assert tray.version_tuple("v0.2.1") == (0, 2, 1)
    assert tray.version_tuple("1.10") == (1, 10, 0)
    assert tray.version_tuple("garbage") == (0, 0, 0)


def test_is_newer_comparison():
    assert tray.is_newer("v0.2.0", "0.1.0") is True
    assert tray.is_newer("0.1.0", "0.1.0") is False
    assert tray.is_newer("v0.1.1", "0.10.0") is False  # numeric, not lexical
    assert tray.is_newer("garbage", "0.1.0") is False


def test_read_state_missing_file(tmp_path, monkeypatch):
    monkeypatch.setattr(tray, "STATE_PATH", str(tmp_path / "nope.json"))
    assert tray.read_state() == {"state": "stopped"}


def test_read_state_corrupt_file(tmp_path, monkeypatch):
    path = tmp_path / "state.json"
    path.write_text("{not json", encoding="utf-8")
    monkeypatch.setattr(tray, "STATE_PATH", str(path))
    assert tray.read_state() == {"state": "stopped"}


def test_read_state_wrong_shape(tmp_path, monkeypatch):
    path = tmp_path / "state.json"
    path.write_text(json.dumps(["idle"]), encoding="utf-8")
    monkeypatch.setattr(tray, "STATE_PATH", str(path))
    assert tray.read_state() == {"state": "stopped"}


def test_read_state_roundtrip(tmp_path, monkeypatch):
    path = tmp_path / "state.json"
    path.write_text(json.dumps({"state": "recording", "pid": 123}),
                    encoding="utf-8")
    monkeypatch.setattr(tray, "STATE_PATH", str(path))
    assert tray.read_state()["state"] == "recording"


def test_latest_release_tag(monkeypatch):
    import io

    class FakeResponse(io.BytesIO):
        def __enter__(self):
            return self

        def __exit__(self, *args):
            return False

    monkeypatch.setattr(tray.urllib.request, "urlopen",
                        lambda req, timeout=0: FakeResponse(b'{"tag_name": "v9.9.9"}'))
    assert tray.latest_release_tag() == "v9.9.9"


def test_pid_alive_handles_garbage():
    assert tray.pid_alive(None) is False
    assert tray.pid_alive(-1) is False
    assert tray.pid_alive("abc") is False


def test_pid_alive_current_process():
    import os
    assert tray.pid_alive(os.getpid()) is True


def test_strings_complete_in_both_languages():
    assert set(tray._STRINGS["en"]) == set(tray._STRINGS["de"])


def test_ui_lang_defaults_to_english(monkeypatch):
    import sys
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.delenv("LC_ALL", raising=False)
    monkeypatch.delenv("LANG", raising=False)
    assert tray._ui_lang() == "en"


def test_ui_lang_detects_german(monkeypatch):
    import sys
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setenv("LANG", "de_DE.UTF-8")
    monkeypatch.delenv("LC_ALL", raising=False)
    assert tray._ui_lang() == "de"


def test_pending_update_runs_installer(tmp_path, monkeypatch):
    marker = tmp_path / "update-pending"
    marker.write_text("")
    monkeypatch.setattr(tray, "UPDATE_PENDING", str(marker))
    monkeypatch.setattr(tray, "UPDATE_LOG", str(tmp_path / "update.log"))
    calls = []
    monkeypatch.setattr(tray.subprocess, "run", lambda *a, **k: calls.append(a))
    tray.run_pending_update_if_any()
    assert calls and "install" in str(calls[0][0][-1])
    assert not marker.exists()  # consumed: never runs twice


def test_no_pending_update_is_a_noop(tmp_path, monkeypatch):
    monkeypatch.setattr(tray, "UPDATE_PENDING", str(tmp_path / "nope"))
    calls = []
    monkeypatch.setattr(tray.subprocess, "run", lambda *a, **k: calls.append(a))
    tray.run_pending_update_if_any()
    assert calls == []


def test_state_from_trusts_only_live_pids():
    import os
    assert tray.state_from({"state": "recording", "pid": os.getpid()}) == "recording"
    assert tray.state_from({"state": "recording", "pid": 99999999}) == "stopped"
    assert tray.state_from({"state": "recording"}) == "stopped"


def test_overlay_wanted_gate(monkeypatch):
    import sys
    monkeypatch.setattr(sys, "platform", "win32")
    monkeypatch.delenv("PTT_OVERLAY", raising=False)
    assert tray.overlay_wanted() is True       # default on for Windows
    monkeypatch.setenv("PTT_OVERLAY", "0")
    assert tray.overlay_wanted() is False      # explicit off
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.delenv("PTT_OVERLAY", raising=False)
    assert tray.overlay_wanted() is False      # default off elsewhere
    monkeypatch.setenv("PTT_OVERLAY", "1")
    assert tray.overlay_wanted() is True       # opt-in (X11)


def test_write_model_config_roundtrip(tmp_path, monkeypatch):
    cfg = tmp_path / "config.json"
    monkeypatch.setattr(tray, "CONFIG_PATH", str(cfg))
    tray.write_model_config("base")
    assert json.loads(cfg.read_text(encoding="utf-8")) == {"model": "base"}


def test_clear_model_env_posix_clears_only_process_env(monkeypatch):
    import os
    import sys
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setenv("WHISPER_MODEL", "small")
    tray.clear_model_env()
    assert "WHISPER_MODEL" not in os.environ


def test_model_menu_constructs_and_actions_fire(monkeypatch):
    """Regression test: pystray rejects actions with >2 args (incl. defaults)."""
    import pytest
    if not tray.load_tray_backend():
        pytest.skip("pystray not installed")
    t = tray.Tray.__new__(tray.Tray)
    chosen = []
    monkeypatch.setattr(t, "_set_model", chosen.append)
    menu = t._model_menu()
    items = list(menu.items)
    assert len(items) == len(tray.MODELS)
    items[0](None)  # activate "tiny" — must reach _set_model
    assert chosen == [tray.MODELS[0][0]]
    monkeypatch.setattr(tray, "current_model", lambda: "small")
    checked = [bool(i.checked) for i in items]
    assert checked == [name == "small" for name, _ in tray.MODELS]


def test_current_model_ignores_state_with_dead_pid(tmp_path, monkeypatch):
    import os
    state = tmp_path / "state.json"
    monkeypatch.setattr(tray, "STATE_PATH", str(state))
    monkeypatch.setattr(tray.dictate, "CONFIG_PATH", str(tmp_path / "cfg.json"))
    monkeypatch.delenv("WHISPER_MODEL", raising=False)
    (tmp_path / "cfg.json").write_text('{"model": "medium"}', encoding="utf-8")
    state.write_text(json.dumps({"state": "idle", "pid": 99999999,
                                 "model": "tiny"}), encoding="utf-8")
    assert tray.current_model() == "medium"  # dead pid -> config wins
    state.write_text(json.dumps({"state": "idle", "pid": os.getpid(),
                                 "model": "tiny"}), encoding="utf-8")
    assert tray.current_model() == "tiny"  # live daemon reports the truth


def test_poll_state_starting_while_child_alive_and_state_stale(tmp_path, monkeypatch):
    import threading
    monkeypatch.setattr(tray, "STATE_PATH", str(tmp_path / "missing.json"))
    t = tray.Tray.__new__(tray.Tray)
    t.state = "boot"
    t.stopping = threading.Event()
    t.daemon = type("D", (), {"alive": True})()
    t.images = {"idle": None, "starting": None}
    updates = []

    class FakeIcon:
        icon = None
        title = None

        def update_menu(self):
            updates.append(1)

    t.icon = FakeIcon()
    thread = threading.Thread(target=t.poll_state, daemon=True)
    thread.start()
    deadline = time.time() + 3
    while t.state != "starting" and time.time() < deadline:
        time.sleep(0.05)
    t.stopping.set()
    thread.join(3)
    assert t.state == "starting"
    assert updates  # the menu was rebuilt for the state change
