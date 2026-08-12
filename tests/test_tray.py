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
    monkeypatch.setattr(sys, "platform", "darwin")
    assert tray.overlay_wanted() is False      # never on macOS (main-thread clash)


def test_pill_for_known_and_hidden_states():
    color, label = tray.pill_for("recording")
    assert color == tray.OVERLAY_COLORS["recording"] and label
    assert tray.pill_for("idle") is None
    assert tray.pill_for("stopped") is None
    # every pill state must have a display string (and vice versa)
    assert set(tray.OVERLAY_COLORS) <= set(tray.S)


def test_icon_key_priority():
    assert tray.icon_key_for("recording", True) == "recording"  # activity beats update
    assert tray.icon_key_for("transcribing", True) == "transcribing"
    assert tray.icon_key_for("idle", True) == "update"          # blue badge when idle
    assert tray.icon_key_for("idle", False) == "idle"
    assert tray.icon_key_for("stopped", True) == "stopped"      # problem beats update
    assert tray.icon_key_for("starting", False) == "starting"


def test_write_config_merges_instead_of_overwriting(tmp_path, monkeypatch):
    cfg = tmp_path / "config.json"
    monkeypatch.setattr(tray, "CONFIG_PATH", str(cfg))
    tray.write_config(model="base")
    tray.write_config(ptt_key="f9")
    assert json.loads(cfg.read_text(encoding="utf-8")) == {
        "model": "base", "ptt_key": "f9"}


def test_clear_env_var_posix_clears_only_process_env(monkeypatch):
    import os
    import sys
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setenv("WHISPER_MODEL", "small")
    tray.clear_env_var("WHISPER_MODEL")
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
    assert [i.text for i in items] == [f"{n} ({s})" for n, s in tray.MODELS]
    items[0](None)  # activate "tiny" — must reach _set_model
    assert chosen == [tray.MODELS[0][0]]
    monkeypatch.setattr(tray, "current_model", lambda: "small")
    checked = [bool(i.checked) for i in items]
    assert checked == [name == "small" for name, _ in tray.MODELS]


def test_key_menu_constructs_and_actions_fire(monkeypatch):
    import pytest
    if not tray.load_tray_backend():
        pytest.skip("pystray not installed")
    t = tray.Tray.__new__(tray.Tray)
    chosen = []
    monkeypatch.setattr(t, "_set_ptt_key", chosen.append)
    items = list(t._key_menu().items)
    assert len(items) == len(tray.PTT_KEYS)
    assert [i.text for i in items] == [label for _, label in tray.PTT_KEYS]
    items[0](None)
    assert chosen == [tray.PTT_KEYS[0][0]]
    monkeypatch.setattr(tray, "current_ptt_key", lambda: "f9")
    checked = [bool(i.checked) for i in items]
    assert checked == [name == "f9" for name, _ in tray.PTT_KEYS]


def test_lang_menu_constructs_and_actions_fire(monkeypatch):
    import pytest
    if not tray.load_tray_backend():
        pytest.skip("pystray not installed")
    t = tray.Tray.__new__(tray.Tray)
    chosen = []
    monkeypatch.setattr(t, "_set_lang", chosen.append)
    items = list(t._lang_menu().items)
    assert [i.text for i in items] == [label for _, label in tray.LANGS]
    items[3](None)  # Italiano
    assert chosen == ["it"]
    monkeypatch.setattr(tray, "current_lang", lambda: "auto")
    checked = [bool(i.checked) for i in items]
    assert checked == [name == "auto" for name, _ in tray.LANGS]


def test_current_lang_maps_none_to_auto(monkeypatch):
    monkeypatch.setattr(tray.dictate, "configured_lang", lambda: None)
    assert tray.current_lang() == "auto"
    monkeypatch.setattr(tray.dictate, "configured_lang", lambda: "es")
    assert tray.current_lang() == "es"


def test_set_lang_restarts_only_when_env_masks_the_config(tmp_path, monkeypatch):
    """An env var in the daemon's process env wins over the config file —
    so a language switch must restart the daemon exactly in that case."""
    import os
    t = tray.Tray.__new__(tray.Tray)
    calls = []
    t.daemon = type("D", (), {"restart": lambda _s: calls.append("restart")})()
    t.icon = type("I", (), {"update_menu": lambda _s: calls.append("menu"),
                            "notify": lambda *a: None})()
    monkeypatch.setattr(tray, "CONFIG_PATH", str(tmp_path / "config.json"))
    monkeypatch.setattr(tray, "current_lang", lambda: "de")

    monkeypatch.delenv("WHISPER_LANG", raising=False)
    t._lang_worker("it")
    assert calls == ["menu"]  # config is enough — no restart

    monkeypatch.setenv("WHISPER_LANG", "de")  # as if the daemon inherited it
    t._lang_worker("es")
    assert "restart" in calls
    assert "WHISPER_LANG" not in os.environ  # cleared from the tray's env


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
    t.pending_update = None
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
