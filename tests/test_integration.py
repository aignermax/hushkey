"""Integration: a full dictation cycle through the real daemon wiring.

Key press (with auto-repeat) -> recorder -> transcription -> insertion, with
fakes only at the hardware seams (recorder, whisper model, X server, pynput
controller). Pins the three user-visible guarantees that broke on a machine
where an old, second dictation daemon ran alongside this one:

- one press/release cycle inserts the transcript exactly once — never double
- the focused window sets the pace: full speed in terminals, paced elsewhere
- the configured language reaches whisper unchanged, and nothing ever asks
  for a translation (faster-whisper only translates with task='translate')
"""
import sys
import threading
import time
import types

import pytest

import dictate


@pytest.fixture(autouse=True)
def isolated_state(monkeypatch, tmp_path):
    """Keep tests out of the real operational log, state and config files."""
    monkeypatch.setattr(dictate, "STATE_DIR", str(tmp_path))
    monkeypatch.setattr(dictate, "LOG_PATH", str(tmp_path / "dictate.log"))
    monkeypatch.setattr(dictate, "STATE_PATH", str(tmp_path / "state.json"))
    monkeypatch.setattr(dictate, "CONFIG_PATH", str(tmp_path / "config.json"))


@pytest.fixture(autouse=True)
def pinned_setup(monkeypatch):
    """Deterministic pynput backend and pacing, regardless of the dev box."""
    monkeypatch.setenv("PTT_BACKEND", "pynput")
    monkeypatch.delenv("WHISPER_LANG", raising=False)
    monkeypatch.setattr(dictate, "TYPE_DELAY", 0.01)
    monkeypatch.setattr(dictate, "TERMINAL_TYPE_DELAY", 0.0)


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


class FakeModel:
    """Records every transcribe() call's kwargs; returns one segment."""

    def __init__(self, text="hallo welt"):
        self.calls = []
        self.text = text

    def transcribe(self, wav, **kwargs):
        self.calls.append(kwargs)
        return [types.SimpleNamespace(text=self.text, start=0.0, end=1.0)], None


class FakeController:
    """Stands in for pynput's Controller: what the focused window receives."""

    def __init__(self):
        self.events = []

    def press(self, ch):
        self.events.append(("down", ch))

    def release(self, ch):
        self.events.append(("up", ch))


class FakeXWindow:
    """A window in the X tree; parent=None mimics the root window, whose
    query_tree().parent comes back as int 0 (X.NONE), not as a window."""

    def __init__(self, wm_class=None, parent=None):
        self.id = id(self)  # python-xlib Window objects have a unique .id
        self._wm_class = wm_class
        self._parent = parent

    def get_wm_class(self):
        return self._wm_class

    def query_tree(self):
        return types.SimpleNamespace(
            parent=self._parent if self._parent is not None else 0)


def make_daemon(monkeypatch, tmp_path, focus_window, model=None):
    """A DictationDaemon on a fake X11 session whose focus is `focus_window`.

    The focus sits on a *child* window without WM_CLASS, the way GNOME
    actually reports it — pass e.g.
    FakeXWindow(parent=FakeXWindow(("gnome-terminal-server", "Gnome-terminal"))).
    """
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setenv("DISPLAY", ":0")
    monkeypatch.setenv("XDG_SESSION_TYPE", "x11")
    disp = types.SimpleNamespace(
        get_input_focus=lambda: types.SimpleNamespace(focus=focus_window),
        close=lambda: None)
    xlib = types.ModuleType("Xlib")
    xlib.display = types.SimpleNamespace(Display=lambda: disp)
    monkeypatch.setitem(sys.modules, "Xlib", xlib)
    monkeypatch.setitem(sys.modules, "Xlib.display", xlib.display)

    wav = tmp_path / "clip.wav"
    wav.write_bytes(b"\0" * 2048)
    rec = FakeRecorder(str(wav))
    monkeypatch.setattr(dictate, "pick_recorder", lambda: rec)
    monkeypatch.setattr(dictate, "notify", lambda *a: None)

    d = dictate.DictationDaemon()
    d.model = model or FakeModel()
    d.injector.controller = FakeController()  # skip the real pynput import

    sleeps = []
    monkeypatch.setattr(dictate.time, "sleep", lambda s: sleeps.append(s))

    idle = threading.Event()
    real_write_state = dictate.write_state

    def watched_write_state(state):
        real_write_state(state)
        if state == "idle":
            idle.set()  # the worker thread's last act after a dictation

    monkeypatch.setattr(dictate, "write_state", watched_write_state)
    return d, rec, idle, sleeps


def dictate_one_cycle(d, idle, hold_seconds=1.0):
    """Press (with auto-repeat), hold, release; wait for the worker thread."""
    d.start_recording()
    d.start_recording()  # key auto-repeat while held must not restart anything
    d.recording = time.time() - hold_seconds  # fake the hold, don't sleep it
    d.stop_recording()
    assert idle.wait(5), "dictation worker did not finish"


def typed_text(d):
    return "".join(ch for event, ch in d.injector.controller.events
                   if event == "down")


def test_dictation_cycle_types_the_transcript_exactly_once(monkeypatch, tmp_path):
    """Double typing was two daemons each typing the text; one daemon must
    insert exactly once per press/release cycle — and pace every character
    when the focused window is not a terminal."""
    focus = FakeXWindow(parent=FakeXWindow(("Navigator", "librewolf")))
    d, rec, idle, sleeps = make_daemon(monkeypatch, tmp_path, focus)
    dictate_one_cycle(d, idle)
    assert rec.started == 1 and rec.stopped == 1
    assert len(d.model.calls) == 1
    text = "hallo welt "  # trailing space separates consecutive dictations
    assert typed_text(d) == text
    assert len(d.injector.controller.events) == 2 * len(text)  # down+up each
    # 0.15 s modifier-release settle, then one pacing sleep per character
    assert sleeps == [0.15] + [0.01] * len(text)


def test_dictation_cycle_runs_full_speed_in_a_terminal(monkeypatch, tmp_path):
    """Consoles keep up with full-speed typing, so no pacing there — but only
    if the terminal is actually recognized: the X11 focus sits on a child
    window without WM_CLASS (GNOME), so detection must walk up the tree."""
    focus = FakeXWindow(parent=FakeXWindow(("gnome-terminal-server",
                                            "Gnome-terminal")))
    d, rec, idle, sleeps = make_daemon(monkeypatch, tmp_path, focus)
    dictate_one_cycle(d, idle)
    assert typed_text(d) == "hallo welt "
    assert sleeps == [0.15]  # only the release settle, not one sleep per char


@pytest.mark.parametrize("configured,expected", [("auto", None),
                                                 ("zh", "zh"),
                                                 ("de", "de")])
def test_dictation_cycle_passes_the_language_through(monkeypatch, tmp_path,
                                                     configured, expected):
    """Whisper transcribes in the spoken language, it never translates:
    the tray's language choice arrives as whisper's `language` ('auto' as
    None = auto-detect) and no `task` kwarg is ever passed — with
    task='translate' faster-whisper would render everything as English."""
    cfg = tmp_path / "config.json"
    cfg.write_text(f'{{"lang": "{configured}"}}', encoding="utf-8")
    focus = FakeXWindow(parent=FakeXWindow(("Navigator", "librewolf")))
    d, rec, idle, _ = make_daemon(monkeypatch, tmp_path, focus)
    dictate_one_cycle(d, idle)
    assert len(d.model.calls) == 1
    assert d.model.calls[0]["language"] == expected
    assert "task" not in d.model.calls[0]
    # the Simplified-script prompt goes into the pass only when zh is pinned
    assert d.model.calls[0]["initial_prompt"] == (
        dictate.ZH_PROMPT if configured == "zh" else None)


def test_dictation_cycle_redecodes_auto_detected_chinese(monkeypatch, tmp_path):
    """Auto-detect reporting zh triggers one prompt-free pass plus one with
    the Simplified prompt (whisper otherwise mixes Traditional characters
    into Mandarin output at random) — and the second pass is what lands in
    the focused window. Only Chinese dictations pay for the extra decode."""
    class ZhModel:
        def __init__(self):
            self.calls = []

        def transcribe(self, wav, **kwargs):
            self.calls.append(kwargs)
            text = "繁體字" if len(self.calls) == 1 else "简体字"
            info = types.SimpleNamespace(language="zh")
            return [types.SimpleNamespace(text=text, start=0.0, end=1.0)], info

    cfg = tmp_path / "config.json"
    cfg.write_text('{"lang": "auto"}', encoding="utf-8")
    focus = FakeXWindow(parent=FakeXWindow(("Navigator", "librewolf")))
    d, rec, idle, _ = make_daemon(monkeypatch, tmp_path, focus, model=ZhModel())
    dictate_one_cycle(d, idle)
    assert len(d.model.calls) == 2
    assert d.model.calls[0]["language"] is None
    assert d.model.calls[0]["initial_prompt"] is None
    assert d.model.calls[1]["language"] == "zh"
    assert d.model.calls[1]["initial_prompt"] == dictate.ZH_PROMPT
    assert typed_text(d) == "简体字 "
