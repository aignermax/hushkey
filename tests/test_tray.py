"""Tests for tray.py — pure logic only; no icon is ever shown."""

import json

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
