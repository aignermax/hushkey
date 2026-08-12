"""Platform helpers and recording control flow of the dictation daemon."""
import builtins
import json
import os
import subprocess
import sys
import threading
import time

import pytest

import dictate


@pytest.fixture(autouse=True)
def isolated_log(monkeypatch, tmp_path):
    """Keep tests out of the real operational log, state and config files."""
    monkeypatch.setattr(dictate, "STATE_DIR", str(tmp_path))
    monkeypatch.setattr(dictate, "LOG_PATH", str(tmp_path / "dictate.log"))
    monkeypatch.setattr(dictate, "STATE_PATH", str(tmp_path / "state.json"))
    monkeypatch.setattr(dictate, "CONFIG_PATH", str(tmp_path / "config.json"))


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


def test_write_state_publishes_json(monkeypatch, tmp_path):
    state_file = tmp_path / "state.json"
    monkeypatch.setattr(dictate, "STATE_PATH", str(state_file))
    monkeypatch.setattr(dictate, "CURRENT_MODEL", "small")
    monkeypatch.setattr(dictate, "PTT_KEY", "ctrl_r")  # pin: it is import-time
    dictate.write_state("recording")
    data = json.loads(state_file.read_text(encoding="utf-8"))
    assert data["state"] == "recording"
    assert data["pid"] == os.getpid()
    assert data["version"] == dictate.VERSION
    assert data["model"] == "small"    # the tray shows this in its Model menu
    assert data["ptt_key"] == "ctrl_r"  # …and this in its key menu
    assert data["ts"] > 0


def test_configured_ptt_key_precedence(monkeypatch, tmp_path):
    monkeypatch.delenv("PTT_KEY", raising=False)
    cfg = tmp_path / "config.json"
    monkeypatch.setattr(dictate, "CONFIG_PATH", str(cfg))
    assert dictate.configured_ptt_key() == "ctrl_r"  # default
    cfg.write_text('{"ptt_key": "f9"}', encoding="utf-8")
    assert dictate.configured_ptt_key() == "f9"      # tray choice
    monkeypatch.setenv("PTT_KEY", "f6")
    assert dictate.configured_ptt_key() == "f6"      # env wins
    cfg.write_text('["oops"]', encoding="utf-8")
    monkeypatch.delenv("PTT_KEY")
    assert dictate.configured_ptt_key() == "ctrl_r"  # wrong shape -> default


def test_configured_model_env_wins_over_tray_config(monkeypatch, tmp_path):
    monkeypatch.setenv("WHISPER_MODEL", "tiny")
    cfg = tmp_path / "config.json"
    cfg.write_text('{"model": "medium"}', encoding="utf-8")
    monkeypatch.setattr(dictate, "CONFIG_PATH", str(cfg))
    assert dictate.configured_model() == "tiny"


def test_configured_model_from_tray_config(monkeypatch, tmp_path):
    monkeypatch.delenv("WHISPER_MODEL", raising=False)
    cfg = tmp_path / "config.json"
    monkeypatch.setattr(dictate, "CONFIG_PATH", str(cfg))
    assert dictate.configured_model() is None  # nothing set -> device default
    cfg.write_text('{"model": "medium"}', encoding="utf-8")
    assert dictate.configured_model() == "medium"
    cfg.write_text('{broken', encoding="utf-8")
    assert dictate.configured_model() is None  # corrupt config is ignored
    cfg.write_text('"small"', encoding="utf-8")
    assert dictate.configured_model() is None  # valid JSON, wrong shape: ignored


def test_daemon_publishes_state_transitions(monkeypatch, tmp_path):
    """recording -> transcribing -> idle, readable by the tray at each step."""
    state_file = tmp_path / "state.json"
    monkeypatch.setattr(dictate, "STATE_PATH", str(state_file))
    monkeypatch.setattr(dictate, "notify", lambda *a: None)
    d, rec, wav = make_daemon(monkeypatch, tmp_path)

    class FakeModel:
        def transcribe(self, wav, language=None, vad_filter=False, beam_size=1):
            return [type("Seg", (), {"text": " hallo "})()], None

    d.model = FakeModel()
    insert_started = threading.Event()
    finish_insert = threading.Event()
    inserted = []

    class FakeInjector:
        def insert(self, text):
            insert_started.set()
            finish_insert.wait(5)
            inserted.append(text)

    d.injector = FakeInjector()

    def state():
        return json.loads(state_file.read_text(encoding="utf-8"))["state"]

    def wait_for(expected):
        deadline = time.time() + 5
        while time.time() < deadline:
            if state() == expected:
                return True
            time.sleep(0.05)
        return False

    d.start_recording()
    assert state() == "recording"
    d.recording = time.time() - 1.0  # pretend the key was held for 1 s
    d.stop_recording()
    assert wait_for("transcribing") and insert_started.wait(5)
    finish_insert.set()
    assert wait_for("idle")
    assert inserted == ["hallo "]


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
    monkeypatch.setattr(dictate, "foreground_window_is_terminal", lambda: False)
    monkeypatch.setattr(dictate.time, "sleep", lambda s: sleeps.append(s))
    injector.insert("ab ")
    assert injector.controller.events == [("down", "a"), ("up", "a"),
                                          ("down", "b"), ("up", "b"),
                                          ("down", " "), ("up", " ")]
    assert sleeps == [0.02] * 3


def test_typing_uses_terminal_delay_in_terminals(monkeypatch):
    injector = dictate.PynputInjector()
    injector.controller = FakeController()
    sleeps = []
    monkeypatch.setattr(dictate, "TYPE_DELAY", 0.02)
    monkeypatch.setattr(dictate, "TERMINAL_TYPE_DELAY", 0)
    monkeypatch.setattr(dictate, "foreground_window_is_terminal", lambda: True)
    monkeypatch.setattr(dictate.time, "sleep", lambda s: sleeps.append(s))
    injector.insert("ab")
    assert injector.controller.events == [("down", "a"), ("up", "a"),
                                          ("down", "b"), ("up", "b")]
    assert sleeps == []  # delay 0: full speed, no pacing at all


def test_typing_never_speeds_up_beyond_type_delay(monkeypatch):
    """A terminal delay *above* the normal one must not slow typing down."""
    injector = dictate.PynputInjector()
    injector.controller = FakeController()
    sleeps = []
    monkeypatch.setattr(dictate, "TYPE_DELAY", 0.01)
    monkeypatch.setattr(dictate, "TERMINAL_TYPE_DELAY", 0.05)
    monkeypatch.setattr(dictate, "foreground_window_is_terminal", lambda: True)
    monkeypatch.setattr(dictate.time, "sleep", lambda s: sleeps.append(s))
    injector.insert("ab")
    assert sleeps == [0.01] * 2


def _fake_xlib(monkeypatch, wm_class):
    """Install a fake Xlib.display module reporting the given WM_CLASS."""
    import types
    window = types.SimpleNamespace(get_wm_class=lambda: wm_class)
    disp = types.SimpleNamespace(
        get_input_focus=lambda: types.SimpleNamespace(focus=window),
        close=lambda: None)
    xlib = types.ModuleType("Xlib")
    xlib.display = types.SimpleNamespace(Display=lambda: disp)
    monkeypatch.setitem(sys.modules, "Xlib", xlib)
    monkeypatch.setitem(sys.modules, "Xlib.display", xlib.display)


def test_terminal_detection_off_without_display(monkeypatch):
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.delenv("DISPLAY", raising=False)
    assert dictate.foreground_window_is_terminal() is False


def test_terminal_detection_off_on_wayland(monkeypatch):
    """Wayland hides the focused window — never claim it is a terminal."""
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setenv("DISPLAY", ":0")  # XWayland sets it; must not matter
    monkeypatch.setenv("XDG_SESSION_TYPE", "wayland")
    _fake_xlib(monkeypatch, ("gnome-terminal-server", "Gnome-terminal"))
    assert dictate.foreground_window_is_terminal() is False


def test_terminal_detection_x11_terminal(monkeypatch):
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setenv("DISPLAY", ":0")
    monkeypatch.setenv("XDG_SESSION_TYPE", "x11")
    _fake_xlib(monkeypatch, ("gnome-terminal-server", "Gnome-terminal"))
    assert dictate.foreground_window_is_terminal() is True


def test_terminal_detection_x11_non_terminal(monkeypatch):
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setenv("DISPLAY", ":0")
    monkeypatch.setenv("XDG_SESSION_TYPE", "x11")
    _fake_xlib(monkeypatch, ("code", "Code"))
    assert dictate.foreground_window_is_terminal() is False


def test_terminal_detection_x11_without_xlib(monkeypatch):
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setenv("DISPLAY", ":0")
    monkeypatch.delenv("XDG_SESSION_TYPE", raising=False)
    monkeypatch.setitem(sys.modules, "Xlib", None)  # import raises ImportError
    assert dictate.foreground_window_is_terminal() is False


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


class FakeCopyProc:
    """A wl-copy stand-in. alive=False models one that exited immediately."""

    def __init__(self, alive=True, returncode=1):
        self._alive = alive
        self.returncode = None if alive else returncode
        self.written = b""
        self.signalled = []
        self.stdin = self

    # -- stdin file object --
    def write(self, data):
        self.written += data

    def close(self):
        pass

    # -- process --
    def poll(self):
        return self.returncode

    def kill(self):
        self.signalled.append("kill")

    def terminate(self):
        self.signalled.append("terminate")

    def wait(self, timeout=None):
        return self.returncode


@pytest.fixture
def wayland(monkeypatch):
    """A WaylandInjector whose helpers are all faked, with a call log."""
    pytest.importorskip("evdev")
    monkeypatch.setattr(dictate.time, "sleep", lambda s: None)
    state = {"calls": [], "procs": [], "paste": b"old clipboard",
             "copy_alive": True}

    def fake_popen(cmd, **kwargs):
        state["calls"].append(cmd[0])
        proc = FakeCopyProc(alive=state["copy_alive"])
        state["procs"].append(proc)
        return proc

    def fake_run(cmd, **kwargs):
        state["calls"].append(cmd[0])
        if cmd[0] == "wl-paste":
            return subprocess.CompletedProcess(cmd, 0, state["paste"], b"")
        return subprocess.CompletedProcess(cmd, 0, b"", b"")

    monkeypatch.setattr(dictate.subprocess, "Popen", fake_popen)
    monkeypatch.setattr(dictate.subprocess, "run", fake_run)
    state["injector"] = dictate.WaylandInjector()
    return state


def test_wl_copy_is_never_waited_for_or_killed(wayland):
    """The regression that broke a whole session's clipboard.

    wl-copy owns the selection for as long as the text is on the clipboard, so
    it must not be run through subprocess.run: that waits for an exit which may
    never come, and on timeout run() KILLS it. A killed owner leaves the
    compositor referring to a dead client and every later clipboard operation
    in the session hangs — system-wide, not just for this daemon.
    """
    wayland["injector"].insert("langes diktat")
    # Two writes: the transcript, then the restored previous clipboard.
    transcript, restore = wayland["procs"]
    assert transcript.written == b"langes diktat"
    assert restore.written == b"old clipboard"
    for proc in (transcript, restore):
        assert proc.signalled == [], \
            "wl-copy was signalled — that is what breaks the whole session"
    # The last writer is the live owner and stays tracked, not reaped away.
    assert wayland["injector"]._copy_proc is restore


def test_no_five_second_timeout_anywhere(wayland):
    """The reported symptom: a 29 s dictation died on a 5 s limit."""
    timeouts = []
    real_run = dictate.subprocess.run

    def recording_run(cmd, **kwargs):
        timeouts.append((cmd[0], kwargs.get("timeout")))
        return real_run(cmd, **kwargs)

    import unittest.mock
    with unittest.mock.patch.object(dictate.subprocess, "run", recording_run):
        wayland["injector"].insert("hallo")
    assert timeouts, "expected some helper to be waited on"
    for name, timeout in timeouts:
        assert timeout != 5, f"{name} still uses the old 5 s timeout"
        assert timeout is None or timeout >= 30, f"{name} timeout too tight: {timeout}"


def test_unset_cmd_timeout_means_no_limit(monkeypatch):
    monkeypatch.setenv("PTT_CMD_TIMEOUT", "0")
    assert (dictate._env_float("PTT_CMD_TIMEOUT", 30) or None) is None
    monkeypatch.setenv("PTT_CMD_TIMEOUT", "45")
    assert (dictate._env_float("PTT_CMD_TIMEOUT", 30) or None) == 45


def test_clipboard_write_that_dies_prevents_the_paste(wayland):
    """No owner means the paste would insert the previous contents."""
    wayland["copy_alive"] = False
    with pytest.raises(RuntimeError, match="exited straight away"):
        wayland["injector"].insert("geheim")
    assert "ydotool" not in wayland["calls"]  # never got as far as pasting


def test_clipboard_restore_failure_is_only_logged(wayland, monkeypatch):
    """The transcript is already inserted — a failed restore must not raise."""
    calls = {"n": 0}
    real_popen = dictate.subprocess.Popen

    def flaky_popen(cmd, **kwargs):
        calls["n"] += 1
        if calls["n"] == 2:  # the restore write
            raise OSError("compositor gone")
        return real_popen(cmd, **kwargs)

    monkeypatch.setattr(dictate.subprocess, "Popen", flaky_popen)
    wayland["injector"].insert("hallo")  # must not raise
    assert wayland["procs"][0].written == b"hallo"


def test_keep_clipboard_skips_read_and_restore(monkeypatch, wayland):
    monkeypatch.setenv("PTT_KEEP_CLIPBOARD", "1")
    injector = dictate.WaylandInjector()
    injector.insert("hallo")
    assert wayland["calls"] == ["wl-copy", "ydotool"]  # no wl-paste, no restore


def test_clipboard_settle_is_configurable(monkeypatch):
    monkeypatch.setenv("PTT_CLIPBOARD_SETTLE", "1.5")
    assert dictate._env_float("PTT_CLIPBOARD_SETTLE", 0.4) == 1.5


def test_pynput_check_reports_missing_display_instead_of_raising(monkeypatch):
    """Forcing the pynput backend on Wayland must explain itself, not traceback."""
    real_import = builtins.__import__

    def fake_import(name, *args, **kwargs):
        if name == "pynput":
            raise ImportError("this platform is not supported: no DISPLAY")
        return real_import(name, *args, **kwargs)

    monkeypatch.setattr(builtins, "__import__", fake_import)
    problem = dictate.PynputListener("ctrl_r").check()
    assert problem and "PTT_BACKEND" in problem


class FakeSelectorKey:
    """Stands in for selectors.SelectorKey — only .fileobj is used."""

    def __init__(self, fileobj):
        self.fileobj = fileobj


class FakeSelector:
    """Minimal selectors.BaseSelector stand-in; every device is always ready."""

    def __init__(self):
        self.registered = {}
        self.closed = False

    def register(self, fileobj, _events):
        self.registered[fileobj] = FakeSelectorKey(fileobj)

    def unregister(self, fileobj):
        del self.registered[fileobj]

    def get_map(self):
        return self.registered

    def select(self):
        return [(key, None) for key in list(self.registered.values())]

    def close(self):
        self.closed = True


class FakeInputDevice:
    def __init__(self, path, reads):
        self.path = path
        self._reads = list(reads)  # each item: list of events, or an OSError
        self.closed = False

    def read(self):
        if not self._reads:
            raise OSError("no more data")
        nxt = self._reads.pop(0)
        if isinstance(nxt, OSError):
            raise nxt
        return nxt

    def close(self):
        self.closed = True


def _key_event(code, value):
    from evdev import ecodes
    return type("Ev", (), {"type": ecodes.EV_KEY, "code": code, "value": value})()


def test_evdev_lost_device_is_dropped_not_fatal(monkeypatch):
    """Unplugging one keyboard must not take the daemon (and the model) down."""
    pytest.importorskip("evdev")
    import selectors

    from evdev import ecodes
    code = ecodes.KEY_RIGHTCTRL
    gone = FakeInputDevice("/dev/input/event1",
                           [[_key_event(code, 1), _key_event(code, 0)],
                            OSError("device removed")])
    survivor = FakeInputDevice("/dev/input/event2", [[_key_event(code, 1)]])

    listener = dictate.EvdevListener("ctrl_r")
    listener.code = code
    monkeypatch.setattr(dictate.EvdevListener, "_keyboards",
                        staticmethod(lambda: [gone, survivor]))
    monkeypatch.setattr(selectors, "DefaultSelector", FakeSelector)

    presses, releases = [], []
    with pytest.raises(SystemExit):  # only once every keyboard is gone
        listener.listen(lambda: presses.append(1), lambda: releases.append(1))

    # The healthy device's press was still delivered after the other vanished.
    assert len(presses) == 2
    # One real release, plus one synthetic release per lost device (both fakes
    # run dry here) so an interrupted hold never leaves a recording running.
    assert len(releases) == 3
    assert gone.closed and survivor.closed


def test_wayland_socket_prefers_explicit_env(monkeypatch):
    monkeypatch.setenv("YDOTOOL_SOCKET", "/run/custom.sock")
    assert dictate.WaylandInjector._socket_path() == "/run/custom.sock"
    monkeypatch.delenv("YDOTOOL_SOCKET")
    monkeypatch.setenv("XDG_RUNTIME_DIR", "/run/user/1000")
    # os.path.join, so the separator is the host's — this asserts the lookup
    # order, not the spelling. The path itself is only ever used on Linux.
    assert dictate.WaylandInjector._socket_path() == os.path.join(
        "/run/user/1000", ".ydotool_socket")
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
