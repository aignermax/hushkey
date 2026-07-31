"""Platform helpers and recording control flow of the dictation daemon."""
import os
import sys
import threading
import time

import pytest

import dictate


@pytest.fixture(autouse=True)
def isolated_log(monkeypatch, tmp_path):
    """Keep tests out of the real operational log."""
    monkeypatch.setattr(dictate, "STATE_DIR", str(tmp_path))
    monkeypatch.setattr(dictate, "LOG_PATH", str(tmp_path / "dictate.log"))


@pytest.fixture(autouse=True)
def pinned_backend(monkeypatch):
    """Pin the backend so results don't depend on the developer's session.

    Without this, DictationDaemon() picks evdev+ydotool when the suite happens
    to run inside a Wayland session, and the pynput assertions below would be
    testing a backend that isn't in use. The backend choice itself is covered
    by the dedicated tests further down.
    """
    monkeypatch.setenv("PTT_BACKEND", "pynput")


def test_state_dir_linux(monkeypatch):
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setenv("XDG_STATE_HOME", "/tmp/xdg")
    assert dictate._state_dir() == os.path.join("/tmp/xdg", "whisper-ptt")


def test_state_dir_macos(monkeypatch):
    monkeypatch.setattr(sys, "platform", "darwin")
    assert dictate._state_dir().endswith("Library/Logs/whisper-ptt")


def test_state_dir_windows(monkeypatch):
    monkeypatch.setattr(sys, "platform", "win32")
    monkeypatch.setenv("LOCALAPPDATA", r"C:\Users\x\AppData\Local")
    assert dictate._state_dir() == os.path.join(r"C:\Users\x\AppData\Local",
                                                "whisper-ptt")


def test_notify_falls_back_to_print(monkeypatch, capsys):
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setattr(dictate.shutil, "which", lambda cmd: None)
    dictate.notify("hello", "world")
    assert "hello world" in capsys.readouterr().err


def test_notify_windows_uses_powershell(monkeypatch):
    monkeypatch.setattr(sys, "platform", "win32")
    monkeypatch.setattr(dictate.shutil, "which", lambda cmd: None)
    calls = []
    monkeypatch.setattr(dictate.subprocess, "Popen", lambda *a, **k: calls.append(a))
    dictate.notify("t", "b")
    assert calls and "powershell" in calls[0][0][0]


def test_preload_cuda_libs_safe_without_nvidia():
    dictate.preload_cuda_libs()  # must not raise, with or without nvidia packages


def test_pick_device_returns_known_combo():
    device, compute, model = dictate.pick_device()
    assert device in ("cpu", "cuda")
    assert compute and model


class FakeRecorder:
    def __init__(self, wav):
        self.wav = wav
        self.started = 0
        self.stopped = 0

    def start(self):
        self.started += 1

    def stop(self):
        self.stopped += 1
        return self.wav


def make_daemon(monkeypatch, tmp_path, wav_size=2048):
    wav = tmp_path / "clip.wav"
    wav.write_bytes(b"\0" * wav_size)
    rec = FakeRecorder(str(wav))
    monkeypatch.setattr(dictate, "pick_recorder", lambda: rec)
    return dictate.DictationDaemon(), rec, str(wav)


def test_key_autorepeat_is_ignored(monkeypatch, tmp_path):
    d, rec, _ = make_daemon(monkeypatch, tmp_path)
    d.start_recording()
    d.start_recording()
    assert rec.started == 1


def test_short_recording_is_dropped(monkeypatch, tmp_path):
    d, rec, wav = make_daemon(monkeypatch, tmp_path, wav_size=10)  # < 1000 bytes
    d.start_recording()
    d.stop_recording()
    assert rec.stopped == 1
    assert not (tmp_path / "clip.wav").exists()  # cleaned up, not transcribed


def test_valid_recording_is_transcribed(monkeypatch, tmp_path):
    d, rec, wav = make_daemon(monkeypatch, tmp_path)
    done = threading.Event()
    seen = {}

    def fake_transcribe(w, duration):
        seen["wav"] = w
        done.set()

    monkeypatch.setattr(d, "_transcribe_and_insert", fake_transcribe)
    d.start_recording()
    d.recording = time.time() - 1.0  # pretend the key was held for 1 s
    d.stop_recording()
    assert done.wait(2)
    assert seen["wav"] == wav


class FakeController:
    def __init__(self):
        self.events = []

    def press(self, ch):
        self.events.append(("down", ch))

    def release(self, ch):
        self.events.append(("up", ch))


def test_typing_paces_each_character(monkeypatch):
    injector = dictate.PynputInjector()
    injector.controller = FakeController()
    sleeps = []
    monkeypatch.setattr(dictate, "TYPE_DELAY", 0.02)
    monkeypatch.setattr(dictate.time, "sleep", lambda s: sleeps.append(s))
    injector.insert("ab ")
    assert injector.controller.events == [("down", "a"), ("up", "a"),
                                          ("down", "b"), ("up", "b"),
                                          ("down", " "), ("up", " ")]
    assert sleeps == [0.02] * 3


def test_typing_maps_control_chars_to_keys(monkeypatch):
    injector = dictate.PynputInjector()
    injector.controller = FakeController()
    monkeypatch.setattr(dictate, "_CONTROL_KEYS",
                        {"\n": "<enter>", "\t": "<tab>"})
    monkeypatch.setattr(dictate, "TYPE_DELAY", 0)
    injector.insert("a\nb\t")
    assert injector.controller.events == [("down", "a"), ("up", "a"),
                                          ("down", "<enter>"), ("up", "<enter>"),
                                          ("down", "b"), ("up", "b"),
                                          ("down", "<tab>"), ("up", "<tab>")]


def test_daemon_inserts_via_the_selected_injector(monkeypatch, tmp_path):
    """The daemon must delegate insertion, not type directly.

    Guards the seam the Wayland backend hangs off: on Wayland the transcript
    goes through the clipboard, so the daemon may not reach for pynput itself.
    """
    d, _, wav = make_daemon(monkeypatch, tmp_path)
    inserted = []

    class FakeModel:
        def transcribe(self, *a, **k):
            class Seg:
                text = "hallo welt"
            return [Seg()], None

    d.model = FakeModel()
    d.injector = type("I", (), {"insert": lambda _self, t: inserted.append(t)})()
    monkeypatch.setattr(dictate.time, "sleep", lambda s: None)
    d._transcribe_and_insert(wav, 1.0)
    assert inserted == ["hallo welt "]  # trailing space separates dictations


def test_env_float_parsing(monkeypatch):
    monkeypatch.setenv("PTT_TYPE_DELAY", "0.02")
    assert dictate._env_float("PTT_TYPE_DELAY", 0.01) == 0.02
    for bad in ("bogus", "", "-1", "nan", "inf"):
        monkeypatch.setenv("PTT_TYPE_DELAY", bad)
        assert dictate._env_float("PTT_TYPE_DELAY", 0.01) == 0.01
    monkeypatch.delenv("PTT_TYPE_DELAY")
    assert dictate._env_float("PTT_TYPE_DELAY", 0.01) == 0.01


def test_backend_defaults_to_session_type(monkeypatch):
    monkeypatch.delenv("PTT_BACKEND")  # set by the pinned_backend fixture
    monkeypatch.setenv("XDG_SESSION_TYPE", "wayland")
    assert dictate.backend_name() == "wayland"
    monkeypatch.setenv("XDG_SESSION_TYPE", "x11")
    assert dictate.backend_name() == "pynput"
    monkeypatch.delenv("XDG_SESSION_TYPE")  # Windows/macOS have no such variable
    assert dictate.backend_name() == "pynput"


def test_ptt_backend_overrides_session_and_accepts_x11_alias(monkeypatch):
    monkeypatch.setenv("XDG_SESSION_TYPE", "wayland")
    monkeypatch.setenv("PTT_BACKEND", "x11")  # documented by earlier versions
    assert dictate.backend_name() == "pynput"
    monkeypatch.setenv("PTT_BACKEND", "pynput")
    assert dictate.backend_name() == "pynput"
    monkeypatch.delenv("XDG_SESSION_TYPE")
    monkeypatch.setenv("PTT_BACKEND", "wayland")
    assert dictate.backend_name() == "wayland"


def test_make_backends_pairs_listener_and_injector(monkeypatch):
    listener, injector = dictate.make_backends("ctrl_r")
    assert isinstance(listener, dictate.PynputListener)
    assert isinstance(injector, dictate.PynputInjector)
    monkeypatch.setenv("PTT_BACKEND", "wayland")
    listener, injector = dictate.make_backends("ctrl_r")
    assert isinstance(listener, dictate.EvdevListener)
    assert isinstance(injector, dictate.WaylandInjector)


def test_paste_chord_presses_then_releases_in_reverse():
    pytest.importorskip("evdev")  # Linux-only dependency
    from evdev import ecodes
    args = dictate.WaylandInjector._build_chord("ctrl+shift+v")
    ctrl, shift, v = ecodes.KEY_LEFTCTRL, ecodes.KEY_LEFTSHIFT, ecodes.KEY_V
    assert args == [f"{ctrl}:1", f"{shift}:1", f"{v}:1",
                    f"{v}:0", f"{shift}:0", f"{ctrl}:0"]


def test_paste_chord_rejects_garbage():
    pytest.importorskip("evdev")
    for bad in ("ctrl+nosuchkey", "", "+"):
        with pytest.raises(ValueError):
            dictate.WaylandInjector._build_chord(bad)


def test_evdev_keycode_resolves_documented_ptt_key_names():
    pytest.importorskip("evdev")
    from evdev import ecodes
    assert dictate.evdev_keycode("ctrl_r") == ecodes.KEY_RIGHTCTRL
    assert dictate.evdev_keycode("f9") == ecodes.KEY_F9
    assert dictate.evdev_keycode("KEY_RIGHTCTRL") == ecodes.KEY_RIGHTCTRL
    assert dictate.evdev_keycode("definitely_not_a_key") is None


def test_wayland_socket_prefers_explicit_env(monkeypatch):
    monkeypatch.setenv("YDOTOOL_SOCKET", "/run/custom.sock")
    assert dictate.WaylandInjector._socket_path() == "/run/custom.sock"
    monkeypatch.delenv("YDOTOOL_SOCKET")
    monkeypatch.setenv("XDG_RUNTIME_DIR", "/run/user/1000")
    assert dictate.WaylandInjector._socket_path() == "/run/user/1000/.ydotool_socket"
    monkeypatch.delenv("XDG_RUNTIME_DIR")
    assert dictate.WaylandInjector._socket_path() is None


def test_missing_backend_does_not_crash(monkeypatch):
    monkeypatch.setattr(dictate, "pick_recorder", lambda: None)
    notes = []
    monkeypatch.setattr(dictate, "notify", lambda *a: notes.append(a))
    d = dictate.DictationDaemon()
    d.start_recording()
    assert d.recording is None
    assert notes  # user got a "no recorder" notification
