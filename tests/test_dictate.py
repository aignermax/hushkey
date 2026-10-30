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
from conftest import fake_x11_owner


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
    by the dedicated tests further down. PTT_KEY is pinned too: the module
    resolves it from the developer's real tray config at import, and a
    caps_lock there would fire the caps restore in every daemon test.
    """
    monkeypatch.setenv("PTT_BACKEND", "pynput")
    monkeypatch.setattr(dictate, "PTT_KEY", "ctrl_r")
    monkeypatch.setattr(dictate, "PREVIEW", False, raising=False)


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

    def fake_transcribe(w, duration, stream=None):
        seen["wav"] = w
        done.set()

    monkeypatch.setattr(d, "_transcribe_and_insert", fake_transcribe)
    d.start_recording()
    d.recording = time.time() - 1.0  # pretend the key was held for 1 s
    d.stop_recording()
    assert done.wait(2)
    assert seen["wav"] == wav


def _caps_daemon(monkeypatch, tmp_path, taps):
    """A daemon with a fake model and an injector recording caps taps."""
    d, rec, wav = make_daemon(monkeypatch, tmp_path)
    monkeypatch.setattr(dictate, "notify", lambda *a: None)
    d.model = type("M", (), {"transcribe": lambda _s, *a, **k:
                             ([type("S", (), {"text": " x "})()], None)})()
    d.injector = type("I", (), {"insert": lambda _s, t: None,
                                "tap_caps_lock": lambda _s: taps.append(1)})()
    monkeypatch.setattr(dictate.time, "sleep", lambda s: None)
    return d, rec, wav


def test_caps_lock_hold_restores_the_caps_state(monkeypatch, tmp_path):
    """Caps Lock as the PTT key: a hold long enough to dictate also toggled
    capitals on the press — the release must flip them back off."""
    monkeypatch.setattr(dictate, "PTT_KEY", "caps_lock")
    taps = []
    d, rec, wav = _caps_daemon(monkeypatch, tmp_path, taps)
    d.start_recording()
    d.recording = time.time() - 1.0  # held long enough to dictate
    d.stop_recording()
    assert taps == [1]
    assert d._caps_restore_until > time.time()  # synthetic tap is suppressed


def test_caps_lock_short_tap_keeps_its_caps_function(monkeypatch, tmp_path):
    """A short tap is 'caps lock', not a dictation: the toggle stays."""
    monkeypatch.setattr(dictate, "PTT_KEY", "caps_lock")
    taps = []
    d, rec, wav = _caps_daemon(monkeypatch, tmp_path, taps)
    d.start_recording()
    d.stop_recording()  # ~0 s: below MIN_SECONDS, dropped as accidental
    assert taps == []


def test_caps_restore_press_is_not_a_new_dictation(monkeypatch, tmp_path):
    """The synthetic restore tap must not look like a fresh PTT press."""
    monkeypatch.setattr(dictate, "PTT_KEY", "caps_lock")
    taps = []
    d, rec, wav = _caps_daemon(monkeypatch, tmp_path, taps)
    d._caps_restore_until = time.time() + 1.0
    d._caps_restore_pending = 1
    d.start_recording()
    assert rec.started == 0
    assert d._caps_restore_pending == 0  # consumed by the synthetic tap


def test_press_right_after_the_restore_tap_still_works(monkeypatch, tmp_path):
    """Only the synthetic tap is swallowed: a real re-press within the
    suppression window starts a dictation (and its own restore at release)."""
    monkeypatch.setattr(dictate, "PTT_KEY", "caps_lock")
    taps = []
    d, rec, wav = _caps_daemon(monkeypatch, tmp_path, taps)
    d._caps_restore_until = time.time() + 1.0
    d._caps_restore_pending = 1
    d.start_recording()  # the synthetic tap: swallowed
    assert rec.started == 0
    d.start_recording()  # a real press right after: must work
    assert rec.started == 1


def test_caps_restore_also_fires_when_the_recording_is_bad(monkeypatch,
                                                           tmp_path):
    """The hold already flipped caps on the press — the restore must not
    depend on the audio being usable."""
    monkeypatch.setattr(dictate, "PTT_KEY", "caps_lock")
    taps = []
    d, rec, wav = _caps_daemon(monkeypatch, tmp_path, taps)
    d.start_recording()
    d.recording = time.time() - 1.0
    os.remove(wav)  # recorder produced nothing usable
    d.stop_recording()
    assert taps == [1]


def test_no_caps_restore_for_other_ptt_keys(monkeypatch, tmp_path):
    monkeypatch.setattr(dictate, "PTT_KEY", "ctrl_r")
    taps = []
    d, rec, wav = _caps_daemon(monkeypatch, tmp_path, taps)
    d.start_recording()
    d.recording = time.time() - 1.0
    d.stop_recording()
    assert taps == []


def test_pynput_tap_caps_lock_toggles_once(monkeypatch):
    injector = dictate.PynputInjector()
    injector.controller = FakeController()
    monkeypatch.setattr(dictate, "_CHORD_KEYS", {"caps_lock": "<caps>"})
    monkeypatch.setattr(dictate.time, "sleep", lambda s: None)
    injector.tap_caps_lock()
    assert injector.controller.events == [("down", "<caps>"), ("up", "<caps>")]


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


def test_concurrent_state_writes_never_publish_mixed_json(monkeypatch, tmp_path):
    """The key listener and the transcribe worker publish concurrently; a
    shared temp file let one writer truncate the other's (JSON 'Extra data')."""
    state_file = tmp_path / "state.json"
    monkeypatch.setattr(dictate, "STATE_DIR", str(tmp_path))
    monkeypatch.setattr(dictate, "STATE_PATH", str(state_file))
    errors = []

    def hammer(state):
        for _ in range(200):
            dictate.write_state(state)
            try:
                json.loads(state_file.read_text(encoding="utf-8"))
            except ValueError as exc:
                errors.append(exc)
            except OSError:
                pass  # Windows: replace() racing a read

    threads = [threading.Thread(target=hammer, args=(s,))
               for s in ("recording", "idle", "transcribing")]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    assert not errors
    assert not list(tmp_path.glob("*.tmp"))  # a failed replace cleans up too


def test_worker_reports_a_recording_that_started_during_transcription(
        monkeypatch, tmp_path):
    """Pressing PTT again while the previous hold transcribes: the worker's
    final publish must not paint the new recording 'idle'."""
    states = []
    monkeypatch.setattr(dictate, "write_state", states.append)
    monkeypatch.setattr(dictate, "notify", lambda *a, **k: None)
    monkeypatch.setattr(dictate, "log", lambda *a, **k: None)
    d = dictate.DictationDaemon.__new__(dictate.DictationDaemon)
    d.busy_lock = threading.Lock()
    d.recording = time.time()  # the next hold is already running
    d._transcribe = lambda source, lang: []
    wav = tmp_path / "a.wav"
    wav.write_bytes(b"")
    monkeypatch.setattr(dictate, "_audio_rms", lambda w: 0.5)
    d._transcribe_and_insert(str(wav), 1.0)
    assert states[-1] == "recording"
    d.recording = None
    wav.write_bytes(b"")
    d._transcribe_and_insert(str(wav), 1.0)
    assert states[-1] == "idle"


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


def test_configured_lang_precedence(monkeypatch, tmp_path):
    cfg = tmp_path / "config.json"
    monkeypatch.setattr(dictate, "CONFIG_PATH", str(cfg))
    monkeypatch.delenv("WHISPER_LANG", raising=False)
    assert dictate.configured_lang() == "de"         # default
    cfg.write_text('{"lang": "it"}', encoding="utf-8")
    assert dictate.configured_lang() == "it"         # tray choice
    cfg.write_text('{"lang": "auto"}', encoding="utf-8")
    assert dictate.configured_lang() is None         # auto-detect
    monkeypatch.setenv("WHISPER_LANG", "es")
    assert dictate.configured_lang() == "es"         # env wins
    monkeypatch.setenv("WHISPER_LANG", "")
    assert dictate.configured_lang() is None         # empty env = auto-detect
    monkeypatch.setenv("WHISPER_LANG", "auto")
    assert dictate.configured_lang() is None         # "auto" env = auto-detect
    monkeypatch.setenv("WHISPER_LANG", "xx")
    assert dictate.configured_lang() == "de"         # unknown code -> fallback


def test_transcribe_passes_configured_lang_through(monkeypatch, tmp_path):
    """The daemon must hand the configured language to faster-whisper."""
    d, rec, wav = make_daemon(monkeypatch, tmp_path)
    monkeypatch.setattr(dictate, "notify", lambda *a: None)
    seen = {}

    class FakeModel:
        def transcribe(self, wav, language=None, vad_filter=False, beam_size=1,
                       initial_prompt=None):
            seen["language"] = language
            return [type("Seg", (), {"text": " ciao "})()], None

    d.model = FakeModel()
    d.injector = type("I", (), {"insert": lambda _self, t: None})()
    monkeypatch.setattr(dictate, "configured_lang", lambda: "it")
    d._transcribe_and_insert(wav, 1.0)
    assert seen["language"] == "it"


def test_transcribe_redecodes_auto_detected_chinese(monkeypatch, tmp_path):
    """Whisper's multilingual models mix Traditional characters into Mandarin
    output at random. On auto-detect the first pass runs prompt-free (a
    Chinese prompt would bias every language); only when it reports zh does a
    second pass with the Simplified prompt follow — pinned to temperature 0,
    the fallback chain's higher temperatures repeat syllables."""
    d, rec, wav = make_daemon(monkeypatch, tmp_path)
    calls = []

    class FakeModel:
        def transcribe(self, source, **kw):
            calls.append(kw)
            info = type("Info", (), {"language": "zh"})()
            text = "繁體字" if len(calls) == 1 else "简体字"
            return [type("Seg", (), {"text": text})()], info

    d.model = FakeModel()
    segs = d._transcribe(wav, None)
    assert [s.text for s in segs] == ["简体字"]  # the second pass wins
    assert len(calls) == 2
    assert "initial_prompt" not in calls[0] and "temperature" not in calls[0]
    assert calls[1]["language"] == "zh"
    assert calls[1]["initial_prompt"] == dictate.ZH_PROMPT
    assert calls[1]["temperature"] == 0.0


def test_transcribe_redecodes_when_chinese_heard_despite_wrong_guess(
        monkeypatch, tmp_path):
    """Whisper's language guess on real Mandarin is a coin flip (seen live:
    'en' at p~0.5), and the unprompted decode under the wrong guess repeats
    syllables. If the first pass still produced CJK characters, the pinned
    zh decode must follow anyway."""
    d, rec, wav = make_daemon(monkeypatch, tmp_path)
    calls = []

    class FakeModel:
        def transcribe(self, source, **kw):
            calls.append(kw)
            info = type("Info", (), {"language": "en"})()
            text = "名名" if len(calls) == 1 else "名字"
            return [type("Seg", (), {"text": text})()], info

    d.model = FakeModel()
    segs = d._transcribe(wav, None)
    assert [s.text for s in segs] == ["名字"]  # the zh pass wins
    assert len(calls) == 2
    assert calls[1]["language"] == "zh"
    assert calls[1]["initial_prompt"] == dictate.ZH_PROMPT
    assert calls[1]["temperature"] == 0.0


def test_transcribe_does_not_redecode_japanese(monkeypatch, tmp_path):
    """Japanese shares the CJK characters — re-decoding it with a Chinese
    prompt would corrupt it. ja is exempt from the character-based trigger."""
    d, rec, wav = make_daemon(monkeypatch, tmp_path)
    calls = []

    class FakeModel:
        def transcribe(self, source, **kw):
            calls.append(kw)
            info = type("Info", (), {"language": "ja"})()
            return [type("Seg", (), {"text": "日本語"})()], info

    d.model = FakeModel()
    segs = d._transcribe(wav, None)
    assert [s.text for s in segs] == ["日本語"]
    assert len(calls) == 1


def test_transcribe_does_not_redecode_cantonese(monkeypatch, tmp_path):
    """Cantonese (yue) shares the characters too — same exemption as ja."""
    d, rec, wav = make_daemon(monkeypatch, tmp_path)
    calls = []

    class FakeModel:
        def transcribe(self, source, **kw):
            calls.append(kw)
            info = type("Info", (), {"language": "yue"})()
            return [type("Seg", (), {"text": "廣東話"})()], info

    d.model = FakeModel()
    segs = d._transcribe(wav, None)
    assert [s.text for s in segs] == ["廣東話"]
    assert len(calls) == 1


def test_transcribe_redecodes_on_zh_guess_even_without_cjk_text(monkeypatch,
                                                                tmp_path):
    """When the guess already is zh, the re-decode fires without any CJK
    text in pass 1 (e.g. Chinese names in Latin transcription) — and pass 1
    is not even decoded for the check."""
    d, rec, wav = make_daemon(monkeypatch, tmp_path)
    calls = []

    class FakeModel:
        def transcribe(self, source, **kw):
            calls.append(kw)
            info = type("Info", (), {"language": "zh"})()
            text = "ni hao" if len(calls) == 1 else "你好"
            return [type("Seg", (), {"text": text})()], info

    d.model = FakeModel()
    segs = d._transcribe(wav, None)
    assert [s.text for s in segs] == ["你好"]
    assert len(calls) == 2


def test_contains_cjk():
    assert dictate._contains_cjk("名字")
    assert dictate._contains_cjk("mix 中文 here")
    assert not dictate._contains_cjk("hallo welt")
    assert not dictate._contains_cjk("カタカナ")  # kana alone is not CJK


def test_transcribe_applies_the_prompt_directly_when_zh_is_pinned(monkeypatch,
                                                                  tmp_path):
    """Pinned zh: the prompt goes into the single pass, no re-decode."""
    d, rec, wav = make_daemon(monkeypatch, tmp_path)
    calls = []

    class FakeModel:
        def transcribe(self, source, **kw):
            calls.append(kw)
            return [], type("Info", (), {"language": "zh"})()

    d.model = FakeModel()
    d._transcribe(wav, "zh")
    assert len(calls) == 1
    assert calls[0]["initial_prompt"] == dictate.ZH_PROMPT
    assert calls[0]["temperature"] == 0.0


def test_transcribe_passes_no_prompt_for_other_languages(monkeypatch, tmp_path):
    """German/English must never see the Chinese prompt — one pass, no bias."""
    d, rec, wav = make_daemon(monkeypatch, tmp_path)
    calls = []

    class FakeModel:
        def transcribe(self, source, **kw):
            calls.append(kw)
            return [], type("Info", (), {"language": "de"})()

    d.model = FakeModel()
    d._transcribe(wav, None)
    assert len(calls) == 1
    assert "initial_prompt" not in calls[0]


def test_transcribe_zh_prompt_can_be_disabled(monkeypatch, tmp_path):
    """WHISPER_ZH_PROMPT='' (Traditional-Chinese users): no prompt, and no
    re-decode for auto-detected Chinese either."""
    monkeypatch.setattr(dictate, "ZH_PROMPT", "")
    d, rec, wav = make_daemon(monkeypatch, tmp_path)
    calls = []

    class FakeModel:
        def transcribe(self, source, **kw):
            calls.append(kw)
            return [], type("Info", (), {"language": "zh"})()

    d.model = FakeModel()
    d._transcribe(wav, None)
    d._transcribe(wav, "zh")
    assert len(calls) == 2
    assert all(c.get("initial_prompt") is None for c in calls)
    # the temperature-0 decode stays even with the prompt disabled
    assert calls[1]["temperature"] == 0.0


def _write_wav(path, amplitude):
    import wave
    import numpy as np
    samples = (np.full(16000, amplitude, dtype=np.int16))
    with wave.open(str(path), "wb") as fh:
        fh.setnchannels(1)
        fh.setsampwidth(2)
        fh.setframerate(16000)
        fh.writeframes(samples.tobytes())


def test_silent_recording_gets_a_mic_warning(monkeypatch, tmp_path):
    """A dead input device must not be reported as 'nothing recognized'."""
    d, rec, wav = make_daemon(monkeypatch, tmp_path)
    notes = []
    monkeypatch.setattr(dictate, "notify", lambda title, body: notes.append(body))
    d.model = type("M", (), {"transcribe": lambda _s, *a, **k: ([], None)})()
    d.injector = type("I", (), {"insert": lambda _self, t: None})()

    _write_wav(tmp_path / "silent.wav", 0)
    d._transcribe_and_insert(str(tmp_path / "silent.wav"), 1.0)
    assert "silence" in notes[-1]

    _write_wav(tmp_path / "loud.wav", 8000)
    d._transcribe_and_insert(str(tmp_path / "loud.wav"), 1.0)
    assert notes[-1] == "nothing recognized"


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
        def transcribe(self, wav, language=None, vad_filter=False, beam_size=1,
                       initial_prompt=None):
            return [type("Seg", (), {"text": " hallo "})()], None

    d.model = FakeModel()
    insert_started = threading.Event()
    finish_insert = threading.Event()
    inserted = []

    class FakeInjector:
        def insert(self, text):
            insert_started.set()
            finish_insert.wait(15)
            inserted.append(text)

    d.injector = FakeInjector()

    def state():
        return json.loads(state_file.read_text(encoding="utf-8"))["state"]

    def wait_for(expected):
        deadline = time.time() + 15  # macOS CI runners are that slow sometimes
        while time.time() < deadline:
            if state() == expected:
                return True
            time.sleep(0.05)
        return False

    d.start_recording()
    assert state() == "recording"
    d.recording = time.time() - 1.0  # pretend the key was held for 1 s
    d.stop_recording()
    # Wait on the event first: it blocks without churning the GIL, unlike a
    # tight state-file polling loop, which starves the worker thread exactly
    # on loaded CI runners (the macOS flakes). While insert() is blocked on
    # finish_insert, the daemon is by definition still "transcribing".
    assert insert_started.wait(15)
    assert state() == "transcribing"
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


def test_typing_ignores_the_chord_override(monkeypatch):
    """_stream_tick passes chord= unconditionally; typed text has no chord, so
    dropping the kwarg here would break streaming on X11/Windows/macOS."""
    injector = dictate.PynputInjector()
    injector.controller = FakeController()
    monkeypatch.setattr(dictate, "TYPE_DELAY", 0)
    injector.insert("ab", chord="shift+insert")
    assert injector.controller.events == [("down", "a"), ("up", "a"),
                                          ("down", "b"), ("up", "b")]


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


class _FakeXWindow:
    """A window in the X tree; parent=None mimics the root window, whose
    query_tree().parent comes back as int 0 (X.NONE), not as a window."""

    def __init__(self, wm_class=None, parent=None):
        self.id = id(self)  # python-xlib Window objects have a unique .id
        self._wm_class = wm_class
        self._parent = parent

    def get_wm_class(self):
        return self._wm_class

    def query_tree(self):
        import types
        return types.SimpleNamespace(
            parent=self._parent if self._parent is not None else 0)


def _fake_xlib_tree(monkeypatch, focus):
    """Install a fake Xlib.display module whose input focus is `focus`.

    Returns the fake Display so tests can assert it got closed.
    """
    import types
    disp = types.SimpleNamespace(
        get_input_focus=lambda: types.SimpleNamespace(focus=focus),
        closed=0)
    disp.close = lambda: setattr(disp, "closed", disp.closed + 1)
    xlib = types.ModuleType("Xlib")
    xlib.display = types.SimpleNamespace(Display=lambda: disp)
    monkeypatch.setitem(sys.modules, "Xlib", xlib)
    monkeypatch.setitem(sys.modules, "Xlib.display", xlib.display)
    return disp


def _pin_x11(monkeypatch):
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setenv("DISPLAY", ":0")
    monkeypatch.setenv("XDG_SESSION_TYPE", "x11")


def test_terminal_detection_walks_up_from_a_focus_child(monkeypatch):
    """GNOME hands the input focus to a *child* window without WM_CLASS
    (seen live: LibreWolf's focus window has none; its parent carries
    ('Navigator', 'librewolf')). Terminals must still be recognized,
    otherwise the console silently falls back to paced typing."""
    _pin_x11(monkeypatch)
    terminal = _FakeXWindow(("gnome-terminal-server", "Gnome-terminal"))
    _fake_xlib_tree(monkeypatch, _FakeXWindow(parent=terminal))
    assert dictate.foreground_window_is_terminal() is True


def test_terminal_detection_walks_up_to_a_non_terminal(monkeypatch):
    _pin_x11(monkeypatch)
    browser = _FakeXWindow(("Navigator", "librewolf"), parent=_FakeXWindow())
    _fake_xlib_tree(monkeypatch, _FakeXWindow(parent=browser))
    assert dictate.foreground_window_is_terminal() is False


def test_terminal_detection_stops_at_the_root_window(monkeypatch):
    """No WM_CLASS anywhere up the chain: the walk must end at the root
    window (whose parent is int 0), not chase ints forever."""
    _pin_x11(monkeypatch)
    _fake_xlib_tree(monkeypatch, _FakeXWindow(parent=_FakeXWindow()))
    assert dictate.foreground_window_is_terminal() is False


def test_terminal_detection_matches_wm_class_case_insensitively(monkeypatch):
    """Alacritty reports ('Alacritty', 'Alacritty') — capitalized in both
    slots, so the comparison must fold case, not get lucky on one entry."""
    _pin_x11(monkeypatch)
    _fake_xlib_tree(monkeypatch, _FakeXWindow(("Alacritty", "Alacritty")))
    assert dictate.foreground_window_is_terminal() is True


def test_terminal_detection_handles_focus_none_and_pointer_root(monkeypatch):
    """X.NONE (0) and PointerRoot (1, focus-follows-mouse) come back as
    plain ints — there is no window to judge by."""
    _pin_x11(monkeypatch)
    _fake_xlib_tree(monkeypatch, 0)
    assert dictate.foreground_window_is_terminal() is False
    _fake_xlib_tree(monkeypatch, 1)
    assert dictate.foreground_window_is_terminal() is False


def test_terminal_detection_survives_a_self_parenting_window(monkeypatch):
    """A window listing itself as parent must not spin the walk forever
    (python-xlib hands out a fresh Window object per query, so the guard
    compares ids, not object identity)."""
    _pin_x11(monkeypatch)
    win = _FakeXWindow()
    win._parent = win
    _fake_xlib_tree(monkeypatch, win)
    assert dictate.foreground_window_is_terminal() is False


def test_terminal_detection_caps_a_bottomless_chain(monkeypatch):
    """The walk gives up after 16 hops rather than chasing parents forever
    — even if a terminal class hides beyond the cap."""
    _pin_x11(monkeypatch)
    win = _FakeXWindow(("gnome-terminal-server", "Gnome-terminal"))
    for _ in range(40):
        win = _FakeXWindow(parent=win)
    _fake_xlib_tree(monkeypatch, win)
    assert dictate.foreground_window_is_terminal() is False


def test_terminal_detection_survives_a_window_dying_mid_walk(monkeypatch):
    """A window destroyed between the focus query and the class read raises;
    the heuristic must fail safe (paced typing), not crash the dictation."""
    _pin_x11(monkeypatch)

    class ZombieWindow(_FakeXWindow):
        def get_wm_class(self):
            raise RuntimeError("window destroyed")

    terminal = _FakeXWindow(("gnome-terminal-server", "Gnome-terminal"))
    _fake_xlib_tree(monkeypatch, ZombieWindow(parent=terminal))
    assert dictate.foreground_window_is_terminal() is False


def test_terminal_detection_closes_the_display(monkeypatch):
    """One X connection per dictation — a leaked one would pile up."""
    _pin_x11(monkeypatch)
    disp = _fake_xlib_tree(monkeypatch, _FakeXWindow(("xterm", "XTerm")))
    dictate.foreground_window_is_terminal()
    assert disp.closed == 1


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


class FakeMappingController(FakeController):
    """A pynput Controller stand-in that also models what the typeability
    introspection in PynputInjector reads: the keyboard mapping and the
    already borrowed keysyms."""

    def __init__(self, mapped=(), borrowed=(), skip=()):
        super().__init__()
        self.keyboard_mapping = {k: (1, 0) for k in mapped}
        self._borrows = {k: (1, 0, 0) for k in borrowed}
        self.skip = skip  # chars whose press/release raises InvalidKeyException

    def press(self, ch):
        if ch in self.skip:
            raise Exception(f"InvalidKeyException: {ch!r}")
        super().press(ch)

    def release(self, ch):
        if ch in self.skip:
            raise Exception(f"InvalidKeyException: {ch!r}")
        super().release(ch)


def _pin_clipboard(monkeypatch, previous=b"OLD"):
    """Fake the X11 clipboard helpers; returns the data owned, in order."""
    owned = []
    monkeypatch.setattr(dictate, "_x11_clipboard_read", lambda: previous)
    fake_x11_owner(monkeypatch, owned)
    return owned


def test_untypeable_text_is_pasted_not_typed(monkeypatch):
    """GNOME's keymap has no unused keycode row, so CJK cannot be typed at
    all (pynput's borrow trick has nowhere to go) — the text must go in
    through the clipboard plus one paste chord."""
    _pin_x11(monkeypatch)
    injector = dictate.PynputInjector()
    injector.controller = FakeMappingController(mapped={ord("a"), ord(" ")})
    owned = _pin_clipboard(monkeypatch)
    monkeypatch.setattr(dictate, "_CHORD_KEYS", {"ctrl": "<ctrl>"})
    monkeypatch.setattr(dictate, "foreground_window_is_terminal", lambda: False)
    monkeypatch.setattr(dictate.time, "sleep", lambda s: None)
    injector.insert("你好 a")
    # transcript until the target fetched it, then the old contents served
    # indefinitely (a bounded restore would destroy the owner window)
    assert owned == ["你好 a".encode(), b"OLD"]
    keys = [k for event, k in injector.controller.events if event == "down"]
    assert keys == ["<ctrl>", "v"]  # chord only, no per-char typing


def test_untypeable_text_in_a_terminal_pastes_with_ctrl_shift_v(monkeypatch):
    _pin_x11(monkeypatch)
    injector = dictate.PynputInjector()
    injector.controller = FakeMappingController(mapped=set())
    owned = _pin_clipboard(monkeypatch, previous=None)
    monkeypatch.setattr(dictate, "_CHORD_KEYS",
                        {"ctrl": "<ctrl>", "shift": "<shift>"})
    monkeypatch.setattr(dictate, "foreground_window_is_terminal", lambda: True)
    monkeypatch.setattr(dictate.time, "sleep", lambda s: None)
    injector.insert("你好")
    keys = [k for event, k in injector.controller.events if event == "down"]
    assert keys == ["<ctrl>", "<shift>", "v"]
    # nothing to restore: the transcript stays pasteable
    assert owned == ["你好".encode()]


def test_paste_honours_the_streaming_chord_override(monkeypatch):
    """Mid-hold streaming inserts pass chord=shift+insert (a held AltGr
    remaps letter chords); the clipboard path must use it, not ctrl+v."""
    _pin_x11(monkeypatch)
    injector = dictate.PynputInjector()
    injector.controller = FakeMappingController(mapped=set())
    _pin_clipboard(monkeypatch, previous=None)
    monkeypatch.setattr(dictate, "_CHORD_KEYS",
                        {"shift": "<shift>", "insert": "<insert>"})
    monkeypatch.setattr(dictate.time, "sleep", lambda s: None)
    injector.insert("你好", chord="shift+insert")
    keys = [k for event, k in injector.controller.events if event == "down"]
    assert keys == ["<shift>", "<insert>"]


def test_typeable_text_is_typed_not_pasted(monkeypatch):
    _pin_x11(monkeypatch)
    injector = dictate.PynputInjector()
    injector.controller = FakeMappingController(
        mapped={ord(c) for c in "abc"})
    owned = _pin_clipboard(monkeypatch)
    monkeypatch.setattr(dictate, "foreground_window_is_terminal", lambda: False)
    monkeypatch.setattr(dictate, "TYPE_DELAY", 0)
    injector.insert("abc")
    assert owned == []
    assert [k for event, k in injector.controller.events
            if event == "down"] == list("abc")


def test_cjk_uses_the_clipboard_even_when_a_keycode_row_is_free(monkeypatch):
    """A free keycode row does not make CJK typeable: borrow-remapping the
    row races the client's keymap refresh and permutes the output (measured
    live: every second syllable becomes a copy of the first). Unmapped
    characters must always go through the clipboard."""
    _pin_x11(monkeypatch)
    injector = dictate.PynputInjector()
    injector.controller = FakeMappingController(mapped=set())
    owned = _pin_clipboard(monkeypatch, previous=None)
    monkeypatch.setattr(dictate, "foreground_window_is_terminal", lambda: False)
    monkeypatch.setattr(dictate, "TYPE_DELAY", 0)
    monkeypatch.setattr(dictate, "_CHORD_KEYS", {"ctrl": "<ctrl>"})
    injector.insert("你a")
    # the whole text goes through the clipboard as one paste, no typing
    assert owned == ["你a".encode()]
    keys = [k for event, k in injector.controller.events if event == "down"]
    assert keys == ["<ctrl>", "v"]


def test_legacy_keysym_characters_are_typed_natively(monkeypatch):
    """Characters with a legacy keysym (Cyrillic_de, EuroSign, ...) are in
    xkb keymaps under *that* keysym, so pynput types them natively without
    any remapping — they must not take the clipboard detour."""
    _pin_x11(monkeypatch)
    injector = dictate.PynputInjector()
    injector.controller = FakeMappingController(mapped={0x6c4, 0x20ac})
    monkeypatch.setattr(dictate, "_XORG_CHARS",
                        {"д": "Cyrillic_de", "€": "EuroSign"})
    monkeypatch.setattr(dictate, "_XORG_SYMBOLS",
                        {"Cyrillic_de": (0x6c4,), "EuroSign": (0x20ac,)})
    owned = _pin_clipboard(monkeypatch)
    monkeypatch.setattr(dictate, "foreground_window_is_terminal", lambda: False)
    monkeypatch.setattr(dictate, "TYPE_DELAY", 0)
    injector.insert("д€")
    assert owned == []  # typed natively, clipboard untouched
    assert [k for event, k in injector.controller.events
            if event == "down"] == ["д", "€"]


def test_legacy_keysym_character_unmapped_uses_the_clipboard(monkeypatch):
    """The legacy keysym only helps when it is actually in the keymap."""
    _pin_x11(monkeypatch)
    injector = dictate.PynputInjector()
    injector.controller = FakeMappingController(mapped=set())
    monkeypatch.setattr(dictate, "_XORG_CHARS", {"€": "EuroSign"})
    monkeypatch.setattr(dictate, "_XORG_SYMBOLS", {"EuroSign": (0x20ac,)})
    owned = _pin_clipboard(monkeypatch, previous=None)
    monkeypatch.setattr(dictate, "foreground_window_is_terminal", lambda: False)
    monkeypatch.setattr(dictate, "TYPE_DELAY", 0)
    monkeypatch.setattr(dictate, "_CHORD_KEYS", {"ctrl": "<ctrl>"})
    injector.insert("€")
    assert owned == ["€".encode()]


def test_borrowed_unicode_keysyms_are_never_typed(monkeypatch):
    """A Unicode-remap-space keysym (0x01000000+) is untypeable *even when
    present in the keymap*: it can only have arrived there by runtime
    borrowing, and a borrowed slot resolves to its neighbour in the client's
    XLookupString — the exact permutation this guards against (seen live:
    pynput borrows from earlier runs persisted in the server keymap)."""
    _pin_x11(monkeypatch)
    injector = dictate.PynputInjector()
    # 名's Unicode keysym sits in the mapping, as if borrowed earlier
    injector.controller = FakeMappingController(mapped={0x100540D})
    owned = _pin_clipboard(monkeypatch, previous=None)
    monkeypatch.setattr(dictate, "foreground_window_is_terminal", lambda: False)
    monkeypatch.setattr(dictate, "TYPE_DELAY", 0)
    monkeypatch.setattr(dictate, "_CHORD_KEYS", {"ctrl": "<ctrl>"})
    monkeypatch.setattr(dictate.time, "sleep", lambda s: None)
    injector.insert("名")
    # only the paste chord, no per-character typing
    assert [k for event, k in injector.controller.events
            if event == "down"] == ["<ctrl>", "v"]
    assert owned == ["名".encode()]  # clipboard instead


def test_clipboard_failure_falls_back_to_typing_what_it_can(monkeypatch,
                                                            tmp_path):
    """If the clipboard cannot be set up, unmappable characters are skipped
    (and logged) instead of aborting the whole dictation."""
    _pin_x11(monkeypatch)
    injector = dictate.PynputInjector()
    injector.controller = FakeMappingController(mapped={ord("a")})

    def broken_read():
        raise OSError("no X connection")

    def broken_own(data, on_request=None):
        raise OSError("no X connection")

    monkeypatch.setattr(dictate, "_x11_clipboard_read", broken_read)
    # a live X display would otherwise really take the clipboard
    monkeypatch.setattr(dictate, "_x11_clipboard_own", broken_own)
    monkeypatch.setattr(dictate, "foreground_window_is_terminal", lambda: False)
    monkeypatch.setattr(dictate, "TYPE_DELAY", 0)
    injector.insert("你a")
    assert [k for event, k in injector.controller.events
            if event == "down"] == ["a"]
    log_text = (tmp_path / "dictate.log").read_text(encoding="utf-8")
    assert "skipped untypeable character '你'" in log_text


def test_clipboard_fallback_never_triggers_off_x11(monkeypatch):
    """Windows/macOS type Unicode natively — no clipboard detour there."""
    monkeypatch.setattr(sys, "platform", "win32")
    injector = dictate.PynputInjector()
    injector.controller = FakeController()  # no mapping introspection at all
    owned = _pin_clipboard(monkeypatch)
    reads = []
    monkeypatch.setattr(dictate, "_x11_clipboard_read",
                        lambda: reads.append("read"))
    monkeypatch.setattr(dictate, "TYPE_DELAY", 0)
    injector.insert("你好")
    assert owned == [] and reads == []  # the platform guard fired, not a fallback
    assert [k for event, k in injector.controller.events
            if event == "down"] == ["你", "好"]


class _FakeSelectionWindow:
    """The 1x1 window the clipboard helpers create; records property writes."""

    def __init__(self, prop_format=8, prop_value=b""):
        self.id = 1234
        self.properties = []  # change_property calls
        self.notifications = []  # send_event calls
        self.owner_of = []
        self._prop = (prop_format, prop_value)
        self.converted = []

    def set_selection_owner(self, selection, time):
        self.owner_of.append(selection)

    def change_property(self, prop, target, fmt, data):
        self.properties.append((prop, target, fmt, data))

    def send_event(self, ev):
        self.notifications.append(ev)

    def convert_selection(self, selection, target, prop, time):
        self.converted.append((selection, target, prop))

    def get_full_property(self, prop, type):
        import types
        fmt, value = self._prop
        return types.SimpleNamespace(format=fmt, value=value)

    def delete_property(self, prop):
        pass


class _FakeSelectionDisplay:
    """A python-xlib Display stand-in: atoms are their names, events are
    scripted, every method the helpers call is recorded or answered."""

    def __init__(self, window, events=()):
        import types
        self.window = window
        self._events = list(events)
        self.closed = 0
        self._root = types.SimpleNamespace(create_window=lambda *a: window)
        self.display = types.SimpleNamespace(
            info=types.SimpleNamespace(resource_id_mask=0x1FFFFF))

    def get_atom(self, name):
        return name

    def screen(self):
        import types
        return types.SimpleNamespace(root=self._root)

    def get_selection_owner(self, selection):
        return self.window  # ownership check compares .id — it matches

    def flush(self):
        pass

    def sync(self):
        pass

    def pending_events(self):
        return len(self._events)

    def next_event(self):
        return self._events.pop(0)

    def close(self):
        self.closed += 1


def _fake_selection_xlib(monkeypatch, disp):
    """Install a fake Xlib tree whose Display() returns `disp`."""
    import types
    X = types.SimpleNamespace(
        CopyFromParent=0, InputOutput=1, CurrentTime=0, NONE=0,
        AnyPropertyType=0, SelectionClear=29, SelectionRequest=30,
        SelectionNotify=31)
    Xatom = types.SimpleNamespace(ATOM=4, STRING=31)
    xevent = types.SimpleNamespace(
        SelectionNotify=lambda **kw: ("SelectionNotify", kw))
    xlib = types.ModuleType("Xlib")
    xlib.X = X
    xlib.Xatom = Xatom
    xlib.display = types.SimpleNamespace(Display=lambda: disp)
    protocol = types.ModuleType("Xlib.protocol")
    protocol.event = xevent
    monkeypatch.setitem(sys.modules, "Xlib", xlib)
    monkeypatch.setitem(sys.modules, "Xlib.protocol", protocol)
    return X


def _request(target, requestor, X):
    import types
    return types.SimpleNamespace(type=X.SelectionRequest, target=target,
                                 property=555, requestor=requestor, time=0,
                                 selection="CLIPBOARD")


def test_clipboard_own_serves_text_targets_and_refusals(monkeypatch):
    """The serve loop answers TARGETS and UTF8_STRING, refuses targets it
    does not offer (property=None per ICCCM), and exits on SelectionClear —
    closing its connection, which releases the selection."""
    import types
    window = _FakeSelectionWindow()
    requestor = _FakeSelectionWindow()
    disp = _FakeSelectionDisplay(window)
    X = _fake_selection_xlib(monkeypatch, disp)
    disp._events.extend([
        _request("TARGETS", requestor, X),
        _request("UTF8_STRING", requestor, X),
        _request("TIMESTAMP", requestor, X),  # not offered -> refuse
        types.SimpleNamespace(type=X.SelectionClear),
    ])
    thread = dictate._x11_clipboard_own("你好".encode())
    thread.join(5)
    assert not thread.is_alive()
    assert disp.closed == 1
    assert window.owner_of == ["CLIPBOARD"]
    answers = [n[1]["property"] for n in requestor.notifications]
    assert answers == [555, 555, 0]  # X.NONE on the unsupported target
    texts = [p for p in requestor.properties if p[1] == "UTF8_STRING"]
    assert texts == [(555, "UTF8_STRING", 8, "你好".encode())]


def test_clipboard_read_returns_text_and_rejects_incr(monkeypatch):
    """A property with format != 8 is an INCR size marker, not text — the
    read must refuse it instead of restoring garbage bytes later."""
    import types
    X = None
    for fmt, value, expected in ((8, b"hello", b"hello"), (32, b"\x00", None)):
        window = _FakeSelectionWindow(prop_format=fmt, prop_value=value)
        disp = _FakeSelectionDisplay(window)
        X = _fake_selection_xlib(monkeypatch, disp)
        disp._events.append(types.SimpleNamespace(
            type=X.SelectionNotify, property="WHISPER_PTT_CLIPBOARD_READ"))
        assert dictate._x11_clipboard_read(timeout=1.0) == expected
        assert disp.closed == 1


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


def test_paste_chord_presses_then_releases_in_reverse(monkeypatch):
    pytest.importorskip("evdev")  # Linux-only dependency
    monkeypatch.setattr(dictate.WaylandInjector, "_ydotool_key_style",
                        staticmethod(lambda: "code"))
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
    state = {"calls": [], "cmds": [], "procs": [], "paste": b"old clipboard",
             "copy_alive": True}

    def fake_popen(cmd, **kwargs):
        state["calls"].append(cmd[0])
        state["cmds"].append(cmd)
        proc = FakeCopyProc(alive=state["copy_alive"])
        state["procs"].append(proc)
        return proc

    def fake_run(cmd, **kwargs):
        state["calls"].append(cmd[0])
        state["cmds"].append(cmd)
        if cmd[0] == "wl-paste":
            return subprocess.CompletedProcess(cmd, 0, state["paste"], b"")
        return subprocess.CompletedProcess(cmd, 0, b"", b"")

    monkeypatch.setattr(dictate.subprocess, "Popen", fake_popen)
    monkeypatch.setattr(dictate.subprocess, "run", fake_run)
    # The style detection would otherwise add an extra ydotool --help call.
    # These tests assume the 1.x KEYCODE:STATE format.
    monkeypatch.setattr(dictate.WaylandInjector, "_ydotool_key_style",
                        staticmethod(lambda: "code"))
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


def test_wayland_socket_resolution_order(monkeypatch, tmp_path):
    monkeypatch.setenv("YDOTOOL_SOCKET", "/run/custom.sock")
    assert dictate.WaylandInjector._socket_path() == "/run/custom.sock"
    monkeypatch.delenv("YDOTOOL_SOCKET")

    runtime = tmp_path / "runtime"
    runtime.mkdir()
    monkeypatch.setenv("XDG_RUNTIME_DIR", str(runtime))
    runtime_sock = runtime / ".ydotool_socket"
    # Both sockets connectable: the runtime-dir one must win. Patching
    # os.access hermetically pins this — otherwise the assert only passes
    # because /tmp/.ydotool_socket happens not to exist on the test host.
    monkeypatch.setattr("os.access", lambda p, m: True)
    assert dictate.WaylandInjector._socket_path() == str(runtime_sock)

    # Older ydotoold (e.g. Ubuntu 24.04's 0.1.x) ignores --socket-path and
    # creates /tmp/.ydotool_socket. The runtime-dir socket is gone, so we
    # must fall back to the legacy path.
    monkeypatch.setattr("os.access",
                        lambda p, m: str(p) == "/tmp/.ydotool_socket")
    assert dictate.WaylandInjector._socket_path() == "/tmp/.ydotool_socket"

    monkeypatch.delenv("XDG_RUNTIME_DIR")
    # Without a runtime dir we still know the legacy fallback path.
    assert dictate.WaylandInjector._socket_path() == "/tmp/.ydotool_socket"


def test_wayland_socket_skips_foreign_socket(monkeypatch):
    """Multi-user box: another user's 0.1.x socket exists in /tmp but is
    not connectable for us — it must not be picked."""
    monkeypatch.delenv("YDOTOOL_SOCKET", raising=False)
    monkeypatch.setenv("XDG_RUNTIME_DIR", "/run/user/1000")
    monkeypatch.setattr("os.access", lambda p, m: False)
    # Nothing usable: report the preferred path so the error names it.
    assert dictate.WaylandInjector._socket_path() == \
        os.path.join("/run/user/1000", ".ydotool_socket")


def test_wayland_check_accepts_legacy_tmp_socket(monkeypatch):
    """The Ubuntu 24.04 crash loop: only /tmp/.ydotool_socket exists —
    check() must find the backend usable instead of exiting."""
    pytest.importorskip("evdev")
    monkeypatch.delenv("YDOTOOL_SOCKET", raising=False)
    monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)
    monkeypatch.setattr(dictate.shutil, "which", lambda t: f"/usr/bin/{t}")
    monkeypatch.setattr("os.access",
                        lambda p, m: str(p) == "/tmp/.ydotool_socket")
    monkeypatch.setattr(dictate.WaylandInjector, "_ydotool_key_style",
                        staticmethod(lambda: "name"))
    injector = dictate.WaylandInjector()
    assert injector.check() is None


def test_build_chord_uses_key_names_for_ydotool_01x(monkeypatch):
    pytest.importorskip("evdev")
    monkeypatch.setattr(dictate.WaylandInjector, "_ydotool_key_style",
                        staticmethod(lambda: "name"))
    assert dictate.WaylandInjector._build_chord("ctrl+v") == ["ctrl+v"]
    assert dictate.WaylandInjector._build_chord("ctrl+shift+v") == ["ctrl+shift+v"]


def test_build_chord_uses_keycodes_for_ydotool_1x(monkeypatch):
    pytest.importorskip("evdev")
    monkeypatch.setattr(dictate.WaylandInjector, "_ydotool_key_style",
                        staticmethod(lambda: "code"))
    args = dictate.WaylandInjector._build_chord("ctrl+v")
    # press ctrl, press v, release v, release ctrl
    assert args == ["29:1", "47:1", "47:0", "29:0"]


def test_ydotool_key_style_detects_01x_from_help_stdout(monkeypatch):
    def fake_run(cmd, **kwargs):
        return subprocess.CompletedProcess(cmd, 0,
            "Usage: key [--delay <ms>] ...\n"
            "Each key sequence can be any number of modifiers and keys, "
            "separated by plus (+)\n", "")
    monkeypatch.setattr(dictate.subprocess, "run", fake_run)
    assert dictate.WaylandInjector._ydotool_key_style() == "name"


def test_ydotool_key_style_detects_01x_from_help_stderr(monkeypatch):
    def fake_run(cmd, **kwargs):
        return subprocess.CompletedProcess(cmd, 0, "",
            "Usage: key [--delay <ms>] ...\n"
            "Each key sequence can be any number of modifiers and keys, "
            "separated by plus (+)\n")
    monkeypatch.setattr(dictate.subprocess, "run", fake_run)
    assert dictate.WaylandInjector._ydotool_key_style() == "name"


def test_ydotool_key_style_defaults_to_code_format(monkeypatch):
    def fake_run(cmd, **kwargs):
        return subprocess.CompletedProcess(cmd, 0,
            "Usage: key [--delay <ms>] <KEYCODE:STATE> ...\n", "")
    monkeypatch.setattr(dictate.subprocess, "run", fake_run)
    assert dictate.WaylandInjector._ydotool_key_style() == "code"


def test_ydotool_key_style_defaults_to_code_on_probe_error(monkeypatch):
    def fake_run(cmd, **kwargs):
        raise subprocess.TimeoutExpired(cmd, 5)
    monkeypatch.setattr(dictate.subprocess, "run", fake_run)
    assert dictate.WaylandInjector._ydotool_key_style() == "code"


def test_build_chord_normalizes_aliases_for_ydotool_01x(monkeypatch):
    """0.1.x's key table knows ctrl, not control — an unknown token would
    silently degrade to its first letter and press 'c'."""
    pytest.importorskip("evdev")
    monkeypatch.setattr(dictate.WaylandInjector, "_ydotool_key_style",
                        staticmethod(lambda: "name"))
    assert dictate.WaylandInjector._build_chord("control+v") == ["ctrl+v"]
    assert dictate.WaylandInjector._build_chord("KEY_BACKSPACE") == ["backspace"]


def test_insert_with_ydotool_01x_sends_key_names(wayland, monkeypatch):
    monkeypatch.setattr(dictate.WaylandInjector, "_ydotool_key_style",
                        staticmethod(lambda: "name"))
    wayland["injector"].insert("hallo")
    paste = [c for c in wayland["cmds"] if c[0] == "ydotool"]
    assert paste == [["ydotool", "key", "ctrl+v"]]


def test_insert_with_chord_override_ydotool_01x(wayland, monkeypatch):
    monkeypatch.setattr(dictate.WaylandInjector, "_ydotool_key_style",
                        staticmethod(lambda: "name"))
    wayland["injector"].insert("hallo", chord="shift+insert")
    paste = [c for c in wayland["cmds"] if c[0] == "ydotool"]
    assert paste == [["ydotool", "key", "shift+insert"]]


def test_insert_with_chord_override_ydotool_1x(wayland):
    pytest.importorskip("evdev")
    from evdev import ecodes
    # the wayland fixture pins the 1.x KEYCODE:STATE style
    wayland["injector"].insert("hallo", chord="shift+insert")
    shift, ins = ecodes.KEY_LEFTSHIFT, ecodes.KEY_INSERT
    paste = [c for c in wayland["cmds"] if c[0] == "ydotool"]
    assert paste == [["ydotool", "key", f"{shift}:1", f"{ins}:1",
                      f"{ins}:0", f"{shift}:0"]]


def test_tap_caps_lock_ydotool_1x(wayland):
    """The caps restore tap goes through ydotool as one press+release; the
    evdev listener ignores the ydotoold device, so it cannot retrigger."""
    pytest.importorskip("evdev")
    from evdev import ecodes
    wayland["injector"].tap_caps_lock()
    caps = ecodes.KEY_CAPSLOCK
    taps = [c for c in wayland["cmds"] if c[0] == "ydotool"]
    assert taps == [["ydotool", "key", f"{caps}:1", f"{caps}:0"]]


def test_tap_caps_lock_ydotool_01x(wayland, monkeypatch):
    """ydotool 0.1.x takes the key-name spelling — 'capslock' is the evdev
    name its key table knows."""
    pytest.importorskip("evdev")
    monkeypatch.setattr(dictate.WaylandInjector, "_ydotool_key_style",
                        staticmethod(lambda: "name"))
    wayland["injector"].tap_caps_lock()
    taps = [c for c in wayland["cmds"] if c[0] == "ydotool"]
    assert taps == [["ydotool", "key", "capslock"]]


def test_insert_uses_style_cached_by_check(wayland, monkeypatch):
    """Production order: check() probes and caches the key style at daemon
    startup — insert() must never probe again (a stateless fake style would
    hide that regression, so this one counts)."""
    probes = []
    monkeypatch.setattr(dictate.WaylandInjector, "_ydotool_key_style",
                        staticmethod(lambda: probes.append(1) or "name"))
    monkeypatch.setattr(dictate.shutil, "which", lambda t: f"/usr/bin/{t}")
    monkeypatch.setattr("os.access", lambda p, m: True)
    inj = wayland["injector"]
    inj._chord_args = {}
    inj._chord_style = None
    assert inj.check() is None
    inj.insert("hallo", chord="shift+insert")
    paste = [c for c in wayland["cmds"] if c[0] == "ydotool"]
    assert paste == [["ydotool", "key", "shift+insert"]]
    assert len(probes) == 1  # probed once, in check(), not again on insert


def test_missing_backend_does_not_crash(monkeypatch):
    monkeypatch.setattr(dictate, "pick_recorder", lambda: None)
    notes = []
    monkeypatch.setattr(dictate, "notify", lambda *a: notes.append(a))
    d = dictate.DictationDaemon()
    d.start_recording()
    assert d.recording is None
    assert notes  # user got a "no recorder" notification


# --------------------------------------------------------------------------
# streaming mode (PTT_STREAMING)


class _Seg:
    def __init__(self, start, end, text):
        self.start, self.end, self.text = start, end, text


def _stream_daemon(inserts, results):
    """A bare daemon shell for streaming ticks: fake recorder/model/injector.

    results: queue of (segments, info) return values, one per transcribe call.
    inserts collects (text, kwargs) per injector.insert call.
    """
    d = dictate.DictationDaemon.__new__(dictate.DictationDaemon)
    d.busy_lock = threading.Lock()
    d.recorder = type("R", (), {"snapshot_wav": lambda _s: "snap.wav"})()
    d.model = type("M", (), {"transcribe": lambda _s, win, **kw:
                             results.pop(0)})()
    d.injector = type("I", (), {"insert": lambda _s, t, **kw:
                                inserts.append((t, kw))})()
    return d


def test_stream_tick_skips_when_too_little_new_audio(monkeypatch):
    import numpy as np
    monkeypatch.setattr(dictate, "_decode_wav_16k",
                        lambda p: np.zeros(16000, dtype=np.float32))  # 1.0 s
    calls = []
    d = _stream_daemon([], [])
    d.model = type("M", (), {"transcribe": lambda _s, w, **kw:
                             calls.append(w) or ([], None)})()
    d._stream_tick(dictate._StreamSession())
    assert calls == []  # 1.0 s - tail guard < STREAM_MIN_SLICE: not worth it


def test_stream_tick_commits_only_finished_segments(monkeypatch):
    import numpy as np
    monkeypatch.setattr(dictate, "PTT_KEY", "f9")  # no chord override
    monkeypatch.setattr(dictate, "_decode_wav_16k",
                        lambda p: np.zeros(16000 * 5, dtype=np.float32))  # 5 s
    inserts = []
    segs = [_Seg(0.2, 2.0, " erster Block "), _Seg(3.0, 4.6, " läuft noch")]
    d = _stream_daemon(inserts, [(segs, None)])
    session = dictate._StreamSession()
    d._stream_tick(session)
    # window is [0, 4.7]; the second segment reaches the edge → left for later
    assert inserts == [("erster Block ", {"chord": None})]
    assert session.committed_end == pytest.approx(2.0)
    assert session.inserted is True


def test_stream_tick_commits_nothing_when_speech_runs_to_the_edge(monkeypatch):
    import numpy as np
    monkeypatch.setattr(dictate, "_decode_wav_16k",
                        lambda p: np.zeros(16000 * 5, dtype=np.float32))
    inserts = []
    d = _stream_daemon(inserts, [([_Seg(0.2, 4.6, "noch im Fluss")], None)])
    session = dictate._StreamSession()
    d._stream_tick(session)
    assert inserts == []
    assert session.committed_end == 0.0  # nothing committed, all re-read later


def test_stream_tick_ignores_empty_transcription(monkeypatch):
    import numpy as np
    monkeypatch.setattr(dictate, "_decode_wav_16k",
                        lambda p: np.zeros(16000 * 5, dtype=np.float32))
    inserts = []
    d = _stream_daemon(inserts, [([_Seg(0.0, 2.0, "   ")], None)])
    session = dictate._StreamSession()
    d._stream_tick(session)
    assert inserts == []
    assert session.committed_end == 0.0


def test_start_recording_streams_only_when_enabled(monkeypatch, tmp_path):
    monkeypatch.setattr(dictate, "notify", lambda *a: None)
    monkeypatch.setattr(dictate, "write_state", lambda *a: None)
    for name in ("a.wav", "b.wav", "c.wav"):
        (tmp_path / name).write_bytes(b"\0" * 10)

    class SnapRecorder(FakeRecorder):
        def snapshot_wav(self):
            return None

    # default: off — no worker even with a capable recorder
    monkeypatch.setattr(dictate, "STREAMING", False)
    rec = SnapRecorder(str(tmp_path / "a.wav"))
    monkeypatch.setattr(dictate, "pick_recorder", lambda: rec)
    d = dictate.DictationDaemon()
    d.start_recording()
    assert d._stream is None
    d.stop_recording()

    # on + capable recorder: worker starts and is cleaned up on release
    monkeypatch.setattr(dictate, "STREAMING", True)
    monkeypatch.setattr(dictate, "pick_recorder",
                        lambda: SnapRecorder(str(tmp_path / "b.wav")))
    d2 = dictate.DictationDaemon()
    d2.start_recording()
    assert d2._stream is not None
    d2.stop_recording()
    assert d2._stream is None

    # on but recorder cannot snapshot: stay non-streaming
    rec3 = FakeRecorder(str(tmp_path / "c.wav"))
    monkeypatch.setattr(dictate, "pick_recorder", lambda: rec3)
    d3 = dictate.DictationDaemon()
    d3.start_recording()
    assert d3._stream is None
    d3.stop_recording()


def test_tail_pass_transcribes_only_after_committed(monkeypatch, tmp_path):
    import numpy as np
    d, _rec, wav = make_daemon(monkeypatch, tmp_path)
    monkeypatch.setattr(dictate, "notify", lambda *a: None)
    seen = {}

    class FakeModel:
        def transcribe(self, audio, **kw):
            seen["audio"] = audio
            return [type("Seg", (), {"text": " Rest "})()], None

    d.model = FakeModel()
    inserts = []
    d.injector = type("I", (), {"insert": lambda _s, t, **kw:
                                inserts.append((t, kw))})()
    monkeypatch.setattr(dictate, "_decode_wav_16k",
                        lambda p: np.zeros(16000 * 10, dtype=np.float32))
    monkeypatch.setattr(dictate.time, "sleep", lambda s: None)
    session = dictate._StreamSession()
    session.committed_end = 6.0
    session.inserted = True
    d._transcribe_and_insert(wav, 10.0, stream=session)
    assert len(seen["audio"]) == 16000 * 4  # only the 4 s tail was transcribed
    assert inserts == [("Rest ", {})]  # no chord override at release time


def test_streamed_dictation_leaves_recovery_transcript(monkeypatch, tmp_path):
    """AltGr + terminal: mid-hold pastes may silently no-op (VTE only takes a
    letter chord, which the held modifier remaps). The full transcript must
    land on the clipboard as the recovery copy — after the tail insert,
    whose internal restore would otherwise clobber it."""
    import numpy as np
    monkeypatch.setattr(dictate, "PTT_KEY", "alt_gr")
    d, _rec, wav = make_daemon(monkeypatch, tmp_path)
    monkeypatch.setattr(dictate, "notify", lambda *a: None)
    d.model = type("M", (), {"transcribe":
                             lambda _s, a, **kw:
                             ([type("Seg", (), {"text": " Ende "})()], None)})()
    events = []
    d.injector = type("I", (), {
        "insert": lambda _s, t, **kw: events.append(("insert", t)),
        "leave_in_clipboard": lambda _s, t: events.append(("leave", t))})()
    monkeypatch.setattr(dictate, "_decode_wav_16k",
                        lambda p: np.zeros(16000 * 10, dtype=np.float32))
    monkeypatch.setattr(dictate.time, "sleep", lambda s: None)
    session = dictate._StreamSession()
    session.committed_end = 6.0
    session.inserted = True
    session.texts = ["Block eins", "Block zwei"]
    d._transcribe_and_insert(wav, 10.0, stream=session)
    assert events == [("insert", "Ende "),
                      ("leave", "Block eins Block zwei Ende")]


def test_recovery_transcript_also_when_tail_is_empty(monkeypatch, tmp_path):
    """The common case: the user stops speaking just before release, so the
    tail is empty — the streamed blocks still need their recovery copy."""
    import numpy as np
    monkeypatch.setattr(dictate, "PTT_KEY", "alt_gr")
    d, _rec, wav = make_daemon(monkeypatch, tmp_path)
    monkeypatch.setattr(dictate, "notify", lambda *a: None)
    d.model = type("M", (), {"transcribe": lambda _s, a, **kw: ([], None)})()
    events = []
    d.injector = type("I", (), {
        "insert": lambda _s, t, **kw: events.append(("insert", t)),
        "leave_in_clipboard": lambda _s, t: events.append(("leave", t))})()
    monkeypatch.setattr(dictate, "_decode_wav_16k",
                        lambda p: np.zeros(16000 * 10, dtype=np.float32))
    session = dictate._StreamSession()
    session.committed_end = 9.0
    session.inserted = True
    session.texts = ["Block eins", "Block zwei"]
    d._transcribe_and_insert(wav, 10.0, stream=session)
    assert events == [("leave", "Block eins Block zwei")]


def test_no_recovery_transcript_without_streaming(monkeypatch, tmp_path):
    """Classic dictation must not clobber the clipboard — with or without a
    remapping modifier as the PTT key."""
    import numpy as np
    d, _rec, wav = make_daemon(monkeypatch, tmp_path)
    monkeypatch.setattr(dictate, "notify", lambda *a: None)
    d.model = type("M", (), {"transcribe":
                             lambda _s, a, **kw:
                             ([type("Seg", (), {"text": " hallo "})()], None)})()
    events = []
    d.injector = type("I", (), {
        "insert": lambda _s, t, **kw: events.append(("insert", t)),
        "leave_in_clipboard": lambda _s, t: events.append(("leave", t))})()
    monkeypatch.setattr(dictate.time, "sleep", lambda s: None)
    for key in ("f9", "alt_gr"):
        monkeypatch.setattr(dictate, "PTT_KEY", key)
        d._transcribe_and_insert(wav, 1.0)  # no stream session at all
        wav = tmp_path / "clip2.wav"  # the first call consumed the file
        wav.write_bytes(b"\0" * 2048)
    assert events == [("insert", "hallo "), ("insert", "hallo ")]


def test_empty_tail_after_streaming_stays_quiet(monkeypatch, tmp_path):
    """Blocks already inserted; an empty tail must not cry 'nothing recognized'."""
    import numpy as np
    d, _rec, wav = make_daemon(monkeypatch, tmp_path)
    notes = []
    monkeypatch.setattr(dictate, "notify", lambda *a: notes.append(a))
    d.model = type("M", (), {"transcribe": lambda _s, a, **kw: ([], None)})()
    d.injector = type("I", (), {"insert": lambda _s, t, **kw: None})()
    monkeypatch.setattr(dictate, "_decode_wav_16k",
                        lambda p: np.zeros(16000 * 10, dtype=np.float32))
    session = dictate._StreamSession()
    session.committed_end = 9.0
    session.inserted = True
    d._transcribe_and_insert(wav, 10.0, stream=session)
    assert notes == [("… transcribing", "")]  # no "nothing recognized" noise


def test_streaming_paste_chord_only_for_alt_gr(monkeypatch):
    monkeypatch.setattr(dictate, "PTT_KEY", "alt_gr")
    assert dictate._streaming_paste_chord() == "shift+insert"
    for key in ("ctrl_r", "f9", "menu"):
        monkeypatch.setattr(dictate, "PTT_KEY", key)
        assert dictate._streaming_paste_chord() is None


def test_stream_tick_uses_insert_chord_with_alt_gr(monkeypatch):
    """Mid-hold inserts with a letter chord would never paste: held AltGr
    remaps its keysyms at the compositor (AltGr+Shift+V = ‚ on a de layout).
    Insert is level-stable, and Shift+Insert pastes in GTK and Qt apps."""
    import numpy as np
    monkeypatch.setattr(dictate, "PTT_KEY", "alt_gr")
    monkeypatch.setattr(dictate, "_decode_wav_16k",
                        lambda p: np.zeros(16000 * 5, dtype=np.float32))
    inserts = []
    d = _stream_daemon(inserts, [([_Seg(0.2, 2.0, " Block")], None)])
    d._stream_tick(dictate._StreamSession())
    assert inserts == [("Block ", {"chord": "shift+insert"})]


def test_stream_tick_uses_default_chord_for_plain_keys(monkeypatch):
    import numpy as np
    monkeypatch.setattr(dictate, "PTT_KEY", "f9")
    monkeypatch.setattr(dictate, "_decode_wav_16k",
                        lambda p: np.zeros(16000 * 5, dtype=np.float32))
    inserts = []
    d = _stream_daemon(inserts, [([_Seg(0.2, 2.0, " Block")], None)])
    d._stream_tick(dictate._StreamSession())
    assert inserts == [("Block ", {"chord": None})]


def test_evdev_listener_ignores_the_ydotoold_device(monkeypatch):
    """The injector's synthetic keyboard must not feed the listener: our own
    paste chords would otherwise look like real PTT key presses."""
    pytest.importorskip("evdev")
    import types
    from evdev import ecodes

    class FakeDev:
        def __init__(self, name):
            self.name = name

        def capabilities(self):
            return {ecodes.EV_KEY: (ecodes.KEY_A, ecodes.KEY_Z)}

        def close(self):
            pass

    devices = {"/dev/input/event3": FakeDev("AT Translated Set 2 keyboard"),
               "/dev/input/event19": FakeDev("ydotoold virtual device")}
    fake_evdev = types.SimpleNamespace(
        list_devices=lambda: list(devices),
        InputDevice=lambda path: devices[path],
        ecodes=ecodes)
    monkeypatch.setitem(sys.modules, "evdev", fake_evdev)
    names = [d.name for d in dictate.EvdevListener._keyboards()]
    assert names == ["AT Translated Set 2 keyboard"]


def test_streaming_inserts_blocks_before_release(monkeypatch):
    """Integration: growing fake recording + fake whisper + fake injector.
    Blocks must land while the key is (virtually) still held, in order,
    without duplicates; the release pass adds exactly the tail."""
    import wave

    import numpy as np

    import recorder as recorder_mod

    monkeypatch.setattr(dictate, "STREAMING", True)
    monkeypatch.setattr(dictate, "STREAM_INTERVAL", 0.05)
    monkeypatch.setattr(dictate, "STREAM_MIN_SLICE", 0.5)
    monkeypatch.setattr(dictate, "STREAM_TAIL_GUARD", 0.1)
    monkeypatch.setattr(dictate, "STREAM_TRAILING_SILENCE", 0.4)
    monkeypatch.setattr(dictate, "notify", lambda *a: None)
    monkeypatch.setattr(dictate, "write_state", lambda *a: None)

    class GrowingRecorder:
        """16 kHz mono PCM, fed in chunks as if speech were coming in."""

        def __init__(self):
            self.pcm = bytearray()

        def start(self):
            pass

        def feed(self, seconds):
            self.pcm += b"\0" * int(seconds * 16000) * 2

        def snapshot_wav(self):
            return recorder_mod._write_wav(bytes(self.pcm), 16000)

        def stop(self):
            return recorder_mod._write_wav(bytes(self.pcm), 16000)

    def fake_decode(path):  # cheap stand-in for faster_whisper's PyAV decode
        with wave.open(path, "rb") as fh:
            frames = fh.readframes(fh.getnframes())
        return np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0
    monkeypatch.setattr(dictate, "_decode_wav_16k", fake_decode)

    call_n = [0]

    class FakeModel:
        def transcribe(self, window, **kw):
            # one block per call, ending inside the trailing-silence margin,
            # so every tick with enough new audio commits exactly one block;
            # a window too short for that yields nothing (and no counter bump)
            dur = len(window) / 16000
            end = dur - 0.45
            if end <= 0.1:
                return [], None
            call_n[0] += 1
            return [_Seg(0.1, end, f" Block{call_n[0]}")], None

    inserts = []
    rec = GrowingRecorder()
    monkeypatch.setattr(dictate, "pick_recorder", lambda: rec)
    d = dictate.DictationDaemon()
    d.model = FakeModel()
    d.injector = type("I", (), {"insert": lambda _s, t, **kw: inserts.append(t)})()

    d.start_recording()
    # Feed until two blocks are in — wall-clock counts flake on loaded CI
    # runners, a condition does not.
    deadline = time.time() + 15
    while len(inserts) < 2 and time.time() < deadline:
        rec.feed(0.5)
        time.sleep(0.05)
    d._stream.stop.set()          # freeze streaming; the rest is tail work
    d._stream.thread.join(2)
    rec.feed(1.5)                 # speech the ticks never see
    before_release = len(inserts)
    d.recording = time.time() - 6.0  # pretend a 6 s hold (> MIN_SECONDS)
    d.stop_recording()

    deadline = time.time() + 5
    while time.time() < deadline and len(inserts) == before_release:
        time.sleep(0.02)  # wait for the tail pass

    assert before_release >= 2  # streaming really streamed, mid-hold
    assert len(inserts) == before_release + 1  # the release pass adds the tail
    nums = [int(t.strip().removeprefix("Block")) for t in inserts]
    # strictly increasing, no duplicates: a tick whose insert is dropped by
    # the stop guard leaves a hole in the numbering (the tail re-covers the
    # audio), but never a duplicate or a reorder
    assert nums == sorted(set(nums))


def test_history_keeps_the_last_three_newest_first():
    for text in ("one", "two", "three", "four"):
        dictate.record_history(text)
    assert [e["text"] for e in dictate.read_history()] == ["four", "three", "two"]


def test_history_ignores_empty_and_survives_corruption(tmp_path):
    dictate.record_history("   ")
    assert dictate.read_history() == []
    (tmp_path / "history.json").write_text("{not json", encoding="utf-8")
    assert dictate.read_history() == []
    dictate.record_history("hello")
    assert [e["text"] for e in dictate.read_history()] == ["hello"]


def test_history_opt_out(monkeypatch):
    dictate.record_history("older")
    monkeypatch.setenv("PTT_HISTORY", "0")
    dictate.record_history("secret")
    assert dictate.read_history() == []
    assert not os.path.exists(dictate.HISTORY_PATH)


def test_transcript_is_recorded_even_when_insert_fails(monkeypatch, tmp_path):
    """The whole point: a lost insert must stay recoverable from the tray."""
    monkeypatch.setattr(dictate, "write_state", lambda *a, **k: None)
    monkeypatch.setattr(dictate, "notify", lambda *a, **k: None)
    monkeypatch.setattr(dictate, "log", lambda *a, **k: None)
    monkeypatch.setattr(dictate.time, "sleep", lambda s: None)
    d = dictate.DictationDaemon.__new__(dictate.DictationDaemon)
    d.busy_lock = threading.Lock()
    d.recording = None

    class Seg:
        text = " lost words "

    d._transcribe = lambda source, lang: [Seg()]

    class BrokenInjector:
        def insert(self, text, chord=None):
            raise RuntimeError("no window")

    d.injector = BrokenInjector()
    wav = tmp_path / "a.wav"
    wav.write_bytes(b"")
    d._transcribe_and_insert(str(wav), 1.0)
    assert [e["text"] for e in dictate.read_history()] == ["lost words"]


class FakeOffer:
    def __init__(self, writes, text, fetched, owned=True):
        writes.append((text, "offer"))
        self.fetched, self.armed, self.closed = fetched, False, False
        self._owned = owned
        self.waited = False

    def arm(self):
        self.armed = True

    def wait(self, timeout):
        assert self.armed  # only a fetch after the chord may count
        self.waited = True
        return self.fetched

    def owned(self):
        return self._owned

    def close(self):
        self.closed = True


def _paste_mode(monkeypatch, platform, previous="OLD", terminal=False,
                fetched=True, owned=True):
    """Paste mode on `platform` with a stubbed clipboard; returns the writes."""
    monkeypatch.setenv("PTT_INSERT", "paste")
    monkeypatch.setattr(sys, "platform", platform)
    writes = []
    monkeypatch.setattr(dictate.PynputInjector, "_clipboard_read",
                        staticmethod(lambda: previous))
    monkeypatch.setattr(dictate.PynputInjector, "_clipboard_write",
                        staticmethod(lambda data:
                                     writes.append((data, "restore"))))
    monkeypatch.setattr(dictate.PynputInjector, "_clipboard_offer",
                        classmethod(lambda cls, text:
                                    FakeOffer(writes, text, fetched, owned)))
    monkeypatch.setattr(dictate, "foreground_window_is_terminal",
                        lambda: terminal)
    monkeypatch.setattr(dictate, "_CHORD_KEYS",
                        {"ctrl": "<ctrl>", "shift": "<shift>",
                         "insert": "<insert>", "cmd": "<cmd>"})
    monkeypatch.setattr(dictate.time, "sleep", lambda s: None)
    injector = dictate.PynputInjector()
    injector.controller = FakeController()
    return injector, writes


def _downs(injector):
    return [k for event, k in injector.controller.events if event == "down"]


def test_paste_mode_pastes_with_ctrl_v_and_restores_the_clipboard(monkeypatch):
    """Default mode: the whole dictation goes in as one paste, not one
    keystroke per character (3,000 chars took 15+ s to type)."""
    injector, writes = _paste_mode(monkeypatch, "win32")
    injector.insert("Hallo, Welt! ")
    assert _downs(injector) == ["<ctrl>", "v"]
    assert writes == [("Hallo, Welt! ", "offer"), ("OLD", "restore")]


def test_paste_mode_uses_shift_insert_in_windows_terminals(monkeypatch):
    """mintty (Git Bash) does not paste on Ctrl+V; every Windows terminal
    takes Shift+Insert."""
    injector, _writes = _paste_mode(monkeypatch, "win32", terminal=True)
    injector.insert("ls -la")
    assert _downs(injector) == ["<shift>", "<insert>"]


def test_paste_mode_uses_cmd_v_on_macos(monkeypatch):
    injector, _writes = _paste_mode(monkeypatch, "darwin")
    injector.insert("Hallo")
    assert _downs(injector) == ["<cmd>", "v"]


def test_paste_mode_types_into_x11_terminals(monkeypatch):
    """xterm/urxvt bind no Ctrl+Shift+V paste — the text would be lost and
    the next key inserted literally. Terminals are typed into instead."""
    injector, writes = _paste_mode(monkeypatch, "linux", terminal=True)
    monkeypatch.setattr(dictate, "TYPE_DELAY", 0)
    monkeypatch.setattr(injector, "_typeable", lambda ch: True)
    injector.insert("ls")
    assert writes == []
    assert _downs(injector) == ["l", "s"]


def test_paste_mode_uses_ctrl_v_in_x11_windows(monkeypatch):
    injector, _writes = _paste_mode(monkeypatch, "linux")
    injector.insert("Hallo")
    assert _downs(injector) == ["<ctrl>", "v"]


def test_paste_mode_does_not_restore_when_the_target_never_fetched(monkeypatch):
    """A busy app or a Remote Desktop session may fetch the clipboard late;
    restoring then would paste the previous contents (a copied password)
    instead of the dictation. Without a confirmed fetch: no restore."""
    injector, writes = _paste_mode(monkeypatch, "win32", fetched=False)
    injector.insert("Hallo")
    assert writes == [("Hallo", "offer")]


def test_paste_mode_keeps_a_copy_made_while_the_paste_settled(monkeypatch):
    """Whatever was copied after the dictation is newer than the previous
    contents; the restore must not overwrite it."""
    injector, writes = _paste_mode(monkeypatch, "win32", owned=False)
    injector.insert("Hallo")
    assert writes == [("Hallo", "offer")]


def test_paste_mode_leaves_the_transcript_when_nothing_to_restore(monkeypatch):
    """An image on the clipboard cannot be restored — the transcript stays."""
    injector, writes = _paste_mode(monkeypatch, "win32", previous=None)
    offers = []
    real_offer = dictate.PynputInjector._clipboard_offer
    monkeypatch.setattr(dictate.PynputInjector, "_clipboard_offer",
                        classmethod(lambda cls, text: offers.append(
                            real_offer(text)) or offers[-1]))
    injector.insert("Hallo")
    assert writes == [("Hallo", "offer")]
    # Nothing to restore, so nothing to wait for: no stall per dictation.
    assert not offers[0].waited


def test_paste_mode_pastes_even_when_the_previous_clipboard_is_unreadable(
        monkeypatch):
    """Typing a long dictation takes seconds; losing the restore does not
    justify that."""
    injector, writes = _paste_mode(monkeypatch, "win32")

    def busy():
        raise OSError("the clipboard is busy")

    monkeypatch.setattr(dictate.PynputInjector, "_clipboard_read",
                        staticmethod(busy))
    injector.insert("Hallo")
    assert _downs(injector) == ["<ctrl>", "v"]
    assert writes == [("Hallo", "offer")]


def test_paste_mode_puts_the_previous_text_back_when_the_offer_fails(
        monkeypatch):
    """The offer may fail after it emptied the clipboard; the previous text
    must not be lost on the way to the typing fallback."""
    injector, writes = _paste_mode(monkeypatch, "win32")
    reads = iter(["OLD", None])
    monkeypatch.setattr(dictate.PynputInjector, "_clipboard_read",
                        staticmethod(lambda: next(reads)))

    def emptied_then_failed(*args, **kwargs):
        raise OSError("clipboard did not take the dictation")

    monkeypatch.setattr(dictate.PynputInjector, "_clipboard_offer",
                        staticmethod(emptied_then_failed))
    monkeypatch.setattr(dictate, "TYPE_DELAY", 0)
    injector.insert("ab")
    assert _downs(injector) == ["a", "b"]
    assert writes == [("OLD", "restore")]


def test_paste_mode_honours_the_streaming_chord_override(monkeypatch):
    injector, _writes = _paste_mode(monkeypatch, "win32")
    injector.insert("Hallo", chord="shift+insert")
    assert _downs(injector) == ["<shift>", "<insert>"]


def test_paste_mode_falls_back_to_typing_without_a_clipboard(monkeypatch):
    injector, _writes = _paste_mode(monkeypatch, "win32")

    def broken(*args, **kwargs):
        raise OSError("the clipboard is busy")

    monkeypatch.setattr(dictate.PynputInjector, "_clipboard_offer",
                        staticmethod(broken))
    monkeypatch.setattr(dictate, "TYPE_DELAY", 0)
    injector.insert("ab")
    assert _downs(injector) == ["a", "b"]


def test_insert_mode_config(monkeypatch, tmp_path):
    cfg = tmp_path / "config.json"
    monkeypatch.setattr(dictate, "CONFIG_PATH", str(cfg))
    monkeypatch.delenv("PTT_INSERT", raising=False)
    assert dictate.configured_insert_mode() == "paste"  # the default
    cfg.write_text(json.dumps({"insert": "type"}), encoding="utf-8")
    assert dictate.configured_insert_mode() == "type"
    cfg.write_text(json.dumps({"insert": "bogus"}), encoding="utf-8")
    assert dictate.configured_insert_mode() == "paste"
    monkeypatch.setenv("PTT_INSERT", "type")
    assert dictate.configured_insert_mode() == "type"  # env wins


def test_keep_clipboard_skips_the_restore(monkeypatch):
    monkeypatch.setenv("PTT_KEEP_CLIPBOARD", "1")
    monkeypatch.setattr(sys, "platform", "win32")
    monkeypatch.setattr(dictate, "_win_clipboard_read",
                        lambda: pytest.fail("must not read"))
    assert dictate.PynputInjector._clipboard_read() is None


@pytest.mark.skipif(sys.platform != "win32", reason="real Windows clipboard")
def test_windows_clipboard_round_trip_is_private_and_exact():
    """Real clipboard: Unicode survives, and the transcript carries the
    formats that keep it out of Win+V history and the cloud clipboard."""
    import ctypes
    before = dictate._win_clipboard_read()
    text = "Grüße & 名字 — ok?"
    try:
        dictate._win_clipboard_write(text, private=True)
        assert dictate._win_clipboard_read() == text
        user32 = ctypes.windll.user32
        user32.RegisterClipboardFormatW.restype = ctypes.c_uint
        fmt = user32.RegisterClipboardFormatW("CanIncludeInClipboardHistory")
        assert user32.IsClipboardFormatAvailable(fmt)
    finally:
        if before is not None:
            dictate._win_clipboard_write(before)


@pytest.mark.skipif(sys.platform != "win32", reason="real Windows clipboard")
def test_windows_paste_serves_a_second_target_read_then_restores(monkeypatch):
    """A target may read once to inspect the clipboard and again, a little
    later, to insert. Only the first read renders, so the restore waits
    CLIPBOARD_SETTLE: the second read still gets the transcript, and only
    then does the previous text come back."""
    before = dictate._win_clipboard_read()
    text = "Grüße & 名字 — hotfix"
    reads = []

    class InspectingTarget:
        def press(self, key):
            if key == "v":
                reads.append(dictate._win_clipboard_read())
                late = threading.Timer(0.1, lambda: reads.append(
                    dictate._win_clipboard_read()))
                late.start()
                self.late = late

        def release(self, key):
            pass

    monkeypatch.setattr(dictate, "_default_paste_chord", lambda: "ctrl+v")
    monkeypatch.setattr(dictate, "CLIPBOARD_SETTLE", 0.4)
    monkeypatch.delenv("PTT_KEEP_CLIPBOARD", raising=False)
    injector = dictate.PynputInjector()
    injector.controller = InspectingTarget()
    try:
        dictate._win_clipboard_write("OLD")
        assert injector._paste_text(text)
        injector.controller.late.join()
        assert reads == [text, text]
        assert dictate._win_clipboard_read() == "OLD"
    finally:
        if before is not None:
            dictate._win_clipboard_write(before)


@pytest.mark.skipif(sys.platform != "win32", reason="real Windows clipboard")
def test_windows_offer_owns_the_clipboard_without_rendering_it():
    """The offer is checked before the chord without reading it back — a
    read would render the text and the target's fetch would go unseen."""
    before = dictate._win_clipboard_read()
    try:
        offer = dictate._WinClipboardOffer("checked")
        try:
            assert offer.owned()
            assert not offer.fetched.is_set()
        finally:
            offer.close()
    finally:
        if before is not None:
            dictate._win_clipboard_write(before)


def test_macos_clipboard_runs_pbcopy_with_a_utf8_locale(monkeypatch):
    """A LaunchAgent has no locale; pbcopy would read UTF-8 as MacRoman."""
    calls = []
    monkeypatch.setattr(dictate.subprocess, "run",
                        lambda cmd, **kw: calls.append((cmd, kw)))
    dictate._mac_clipboard_write("Grüße")
    cmd, kw = calls[0]
    assert cmd == ["pbcopy"] and kw["input"] == "Grüße".encode("utf-8")
    assert kw["env"]["LC_ALL"] == "en_US.UTF-8"


def test_macos_does_not_restore_over_a_copied_file(monkeypatch):
    """pbpaste flattens a Finder file copy to its name; restoring that would
    replace the copied file with a string."""
    import types

    def run(cmd, **kw):
        if cmd[0] == "osascript":
            return types.SimpleNamespace(stdout=b"\xc2\xabclass furl\xc2\xbb, 42")
        pytest.fail("must not read the text")

    monkeypatch.setattr(dictate.subprocess, "run", run)
    assert dictate._mac_clipboard_read() is None


def test_macos_restores_plain_text(monkeypatch):
    import types

    def run(cmd, **kw):
        if cmd[0] == "osascript":
            return types.SimpleNamespace(stdout=b"\xc2\xabclass utf8\xc2\xbb, 6")
        return types.SimpleNamespace(stdout="Grüße".encode("utf-8"))

    monkeypatch.setattr(dictate.subprocess, "run", run)
    assert dictate._mac_clipboard_read() == "Grüße"


@pytest.mark.skipif(sys.platform != "win32", reason="real Windows clipboard")
def test_windows_offer_reports_the_fetch_and_renders_exact_text(monkeypatch):
    """Delayed rendering, end to end on the real clipboard: a read by the
    paste target (here: this process) renders the exact text and reports
    the fetch."""
    monkeypatch.setattr(dictate, "_win_foreground_window", lambda: 1)
    real_pid = dictate._win_window_pid
    monkeypatch.setattr(dictate, "_win_window_pid",
                        lambda hwnd: os.getpid() if hwnd == 1 else real_pid(hwnd))
    before = dictate._win_clipboard_read()
    text = "Grüße & 名字 — ok?"
    try:
        offer = dictate._WinClipboardOffer(text)
        try:
            offer.arm()
            assert not offer.fetched.is_set()
            assert dictate._win_clipboard_read() == text  # triggers the render
            assert offer.wait(2)
        finally:
            offer.close()
    finally:
        if before is not None:
            dictate._win_clipboard_write(before)


@pytest.mark.skipif(sys.platform != "win32", reason="real Windows clipboard")
def test_windows_offer_ignores_reads_before_arm_and_survives_close():
    """A clipboard monitor reading before the paste chord must not count as
    the target's fetch; closing an unfetched offer keeps the text."""
    before = dictate._win_clipboard_read()
    text = "early reader"
    try:
        offer = dictate._WinClipboardOffer(text)
        try:
            assert dictate._win_clipboard_read() == text  # monitor, unarmed
            offer.arm()
            assert not offer.wait(0.3)
        finally:
            offer.close()
        unfetched = dictate._WinClipboardOffer("still here")
        unfetched.close()  # never read: WM_RENDERALLFORMATS must keep it
        assert dictate._win_clipboard_read() == "still here"
    finally:
        if before is not None:
            dictate._win_clipboard_write(before)


def _read_clipboard_with_a_window():
    """Read CF_UNICODETEXT like a clipboard monitor does: with its own
    (message-only) window, so the reader is attributable to this process."""
    import ctypes
    from ctypes import wintypes
    user32 = ctypes.windll.user32
    user32.CreateWindowExW.restype = wintypes.HWND
    user32.CreateWindowExW.argtypes = (
        wintypes.DWORD, wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.DWORD,
        ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, wintypes.HWND,
        wintypes.HMENU, wintypes.HINSTANCE, wintypes.LPVOID)
    hwnd = user32.CreateWindowExW(0, "STATIC", None, 0, 0, 0, 0, 0,
                                  wintypes.HWND(-3), None, None, None)
    assert hwnd
    try:
        ctypes_, user32_, kernel32 = dictate._win_clipboard_api()
        user32.OpenClipboard.argtypes = (wintypes.HWND,)
        assert user32.OpenClipboard(hwnd)
        try:
            handle = user32_.GetClipboardData(dictate._CF_UNICODETEXT)
            ptr = kernel32.GlobalLock(handle)
            try:
                return ctypes.wstring_at(ptr)
            finally:
                kernel32.GlobalUnlock(handle)
        finally:
            user32.CloseClipboard()
    finally:
        user32.DestroyWindow(hwnd)


@pytest.mark.skipif(sys.platform != "win32", reason="real Windows clipboard")
def test_windows_offer_counts_a_child_window_process_as_the_target(monkeypatch):
    """UWP: the foreground frame belongs to ApplicationFrameHost, the app
    that reads the clipboard owns a child window inside it."""
    monkeypatch.setattr(dictate, "_win_foreground_window", lambda: 1)
    monkeypatch.setattr(dictate, "_win_window_pids",
                        lambda hwnd: {999999, os.getpid()} if hwnd == 1 else set())
    before = dictate._win_clipboard_read()
    try:
        offer = dictate._WinClipboardOffer("uwp")
        try:
            offer.arm()
            assert _read_clipboard_with_a_window() == "uwp"
            assert offer.wait(2)
        finally:
            offer.close()
    finally:
        if before is not None:
            dictate._win_clipboard_write(before)


@pytest.mark.skipif(sys.platform != "win32", reason="real Windows clipboard")
def test_windows_offer_does_not_count_a_failed_render_as_a_fetch(monkeypatch):
    """A render that could not hand out the text left the target with
    nothing to paste; restoring then would lose the dictation entirely."""
    before = dictate._win_clipboard_read()
    try:
        offer = dictate._WinClipboardOffer("never delivered")
        try:
            offer.arm()

            def out_of_memory(fmt, data):
                raise OSError("GlobalAlloc failed")

            monkeypatch.setattr(dictate, "_win_clipboard_put", out_of_memory)
            assert dictate._win_clipboard_read() is None
            assert not offer.wait(0.3)
        finally:
            monkeypatch.undo()
            offer.close()
    finally:
        if before is not None:
            dictate._win_clipboard_write(before)


@pytest.mark.skipif(sys.platform != "win32", reason="real Windows clipboard")
def test_windows_offer_ignores_readers_other_than_the_paste_target(monkeypatch):
    """A clipboard monitor reading *after* the chord must not count either:
    restoring then could let the slower target paste the previous contents."""
    monkeypatch.setattr(dictate, "_win_foreground_window", lambda: 1)
    real_pid = dictate._win_window_pid
    monkeypatch.setattr(dictate, "_win_window_pid",
                        lambda hwnd: 999999 if hwnd == 1 else real_pid(hwnd))
    before = dictate._win_clipboard_read()
    try:
        offer = dictate._WinClipboardOffer("monitor read")
        try:
            offer.arm()  # the target is pid 999999, not us
            assert _read_clipboard_with_a_window() == "monitor read"
            assert not offer.wait(0.3)
        finally:
            offer.close()
    finally:
        if before is not None:
            dictate._win_clipboard_write(before)


def test_macos_restores_office_text_despite_a_pdf_rendition(monkeypatch):
    """Office/iWork put a PDF/image rendition next to copied text."""
    import types

    def run(cmd, **kw):
        if cmd[0] == "osascript":
            return types.SimpleNamespace(
                stdout="«class PDF », 900, «class utf8», 5, string, 5".encode())
        return types.SimpleNamespace(stdout=b"cells")

    monkeypatch.setattr(dictate.subprocess, "run", run)
    assert dictate._mac_clipboard_read() == "cells"


def test_macos_does_not_restore_over_an_image(monkeypatch):
    import types

    def run(cmd, **kw):
        if cmd[0] == "osascript":
            return types.SimpleNamespace(stdout="«class PNGf», 900".encode())
        pytest.fail("must not read the text")

    monkeypatch.setattr(dictate.subprocess, "run", run)
    assert dictate._mac_clipboard_read() is None


FLAT = ("man muss dazu sagen dass ich gestern auch noch den agx tor von nvidia "
        "geschnappt habe weil da ein paar rumliegen und ich habe gefragt ob ich "
        "den probieren kann")


def test_transcribe_redecodes_long_text_without_punctuation(monkeypatch, tmp_path):
    """Regression (debug recording, v0.10.4): on a 189 s German dictation
    whisper.cpp returned every word lowercase without a single punctuation
    mark, deterministically. A punctuated prompt in the detected language
    restores the style; the re-decode is pinned to that language."""
    d, rec, wav = make_daemon(monkeypatch, tmp_path)
    calls = []

    class FakeModel:
        def transcribe(self, source, **kw):
            calls.append(kw)
            text = FLAT if len(calls) == 1 else "Man muss dazu sagen, dass ich es weiß."
            return [type("Seg", (), {"text": text})()], type("Info", (), {"language": "de"})()

    d.model = FakeModel()
    segs = d._transcribe(wav, None)
    assert [s.text for s in segs] == ["Man muss dazu sagen, dass ich es weiß."]
    assert len(calls) == 2
    assert calls[1]["language"] == "de"
    assert calls[1]["initial_prompt"] == dictate.STYLE_PROMPTS["de"]


def test_transcribe_keeps_short_or_punctuated_text_in_one_pass(monkeypatch, tmp_path):
    d, rec, wav = make_daemon(monkeypatch, tmp_path)
    calls = []
    texts = iter(["ja genau", FLAT.capitalize() + "."])

    class FakeModel:
        def transcribe(self, source, **kw):
            calls.append(kw)
            return [type("Seg", (), {"text": next(texts)})()], type("Info", (), {"language": "de"})()

    d.model = FakeModel()
    d._transcribe(wav, None)
    d._transcribe(wav, None)
    assert len(calls) == 2  # no re-decode for either


def test_transcribe_keeps_first_pass_when_redecode_is_no_better(monkeypatch, tmp_path):
    d, rec, wav = make_daemon(monkeypatch, tmp_path)
    calls = []

    class FakeModel:
        def transcribe(self, source, **kw):
            calls.append(kw)
            text = FLAT if len(calls) == 1 else "x"
            return [type("Seg", (), {"text": text})()], type("Info", (), {"language": "de"})()

    d.model = FakeModel()
    segs = d._transcribe(wav, None)
    assert [s.text for s in segs] == [FLAT]


def test_transcribe_style_redecode_uses_pinned_language(monkeypatch, tmp_path):
    d, rec, wav = make_daemon(monkeypatch, tmp_path)
    calls = []

    class FakeModel:
        def transcribe(self, source, **kw):
            calls.append(kw)
            text = FLAT if len(calls) == 1 else "Fine, thanks."
            return [type("Seg", (), {"text": text})()], None

    d.model = FakeModel()
    d._transcribe(wav, "en")
    assert calls[1]["language"] == "en"
    assert calls[1]["initial_prompt"] == dictate.STYLE_PROMPTS["en"]


def test_preview_skips_the_punctuation_redecode(monkeypatch, tmp_path):
    """The preview holds busy_lock; a second pass would delay the final one."""
    d, rec, wav = make_daemon(monkeypatch, tmp_path)
    calls = []

    class FakeModel:
        def transcribe(self, source, **kw):
            calls.append(kw)
            return [type("Seg", (), {"text": FLAT})()], type("Info", (), {"language": "de"})()

    d.model = FakeModel()
    d._transcribe(wav, None, restyle=False)
    assert len(calls) == 1


def _x11_offer(monkeypatch, focus=None, owner=0x4A00001):
    """A real _X11ClipboardOffer over a stub owner thread we drive by hand;
    `focus` is the input-focus window id, `owner` the CLIPBOARD owner id."""
    from conftest import _FakeOwnerThread
    served = {"thread": _FakeOwnerThread(window_id=0x4A00001)}

    def own(data, on_request=None):
        served["data"], served["on_request"] = data, on_request
        return served["thread"]

    monkeypatch.setattr(dictate, "_x11_clipboard_own", own)
    monkeypatch.setattr(dictate, "_x11_focus_window_id", lambda: focus)
    monkeypatch.setattr(dictate, "_x11_clipboard_owner_id",
                        lambda: served.get("owner", owner))
    return dictate._X11ClipboardOffer("Hallo"), served


CHROMIUM_FOCUS = 0x3A00003      # the focused Chromium window
CHROMIUM_CLIPBOARD = 0x3A00000  # its hidden window that reads the clipboard
CLIPBOARD_MANAGER = 0x6008AE    # another X client


def test_x11_offer_waits_for_the_targets_fetch_and_then_for_quiet(monkeypatch):
    """Chromium/Electron (Teams) read in several round trips and drop the paste
    when the owner changes in between: the restore waits for the text fetch
    and then for the requests to stop, not for a fixed timer."""
    monkeypatch.setattr(dictate, "CLIPBOARD_SETTLE", 0.2)
    offer, served = _x11_offer(monkeypatch, focus=CHROMIUM_FOCUS)
    assert served["data"] == "Hallo".encode()
    served["on_request"](True, CHROMIUM_CLIPBOARD)  # before the chord
    assert not offer.fetched.is_set()
    offer.arm()

    def chromium():
        for text in (False, True, False, False):  # TARGETS, text, TARGETS…
            time.sleep(0.1)
            served["on_request"](text, CHROMIUM_CLIPBOARD)

    t0 = time.monotonic()
    threading.Thread(target=chromium).start()
    assert offer.wait(5)
    # last request at ~0.4 s, then 0.2 s of quiet
    assert time.monotonic() - t0 >= 0.55


def test_x11_offer_ignores_a_clipboard_manager(monkeypatch):
    """A clipboard manager fetching every new owner is not the paste target:
    counting it would restore before a slow target even started reading."""
    offer, served = _x11_offer(monkeypatch, focus=CHROMIUM_FOCUS)
    offer.arm()
    served["on_request"](True, CLIPBOARD_MANAGER)
    assert not offer.wait(0.1)
    served["on_request"](True, CHROMIUM_CLIPBOARD)
    assert offer.fetched.is_set()


def test_x11_offer_counts_every_client_without_a_focus_window(monkeypatch):
    offer, served = _x11_offer(monkeypatch, focus=None)
    offer.arm()
    served["on_request"](True, CLIPBOARD_MANAGER)
    assert offer.fetched.is_set()


def test_x11_offer_reports_no_fetch_on_timeout(monkeypatch):
    offer, served = _x11_offer(monkeypatch)
    offer.arm()
    served["on_request"](False, CHROMIUM_CLIPBOARD)  # TARGETS only
    assert not offer.wait(0.1)


def test_x11_offer_gives_a_late_fetch_its_quiet_period(monkeypatch):
    """A fetch just before the timeout is still followed by the rest of the
    target's handshake — the quiet period gets its own budget."""
    monkeypatch.setattr(dictate, "CLIPBOARD_SETTLE", 0.1)
    offer, served = _x11_offer(monkeypatch)
    offer.arm()
    threading.Timer(0.15, served["on_request"], (True, None)).start()
    t0 = time.monotonic()
    assert offer.wait(0.2)
    # fetch at 0.15 s + 0.1 s quiet: past the first 0.2 s budget
    assert time.monotonic() - t0 >= 0.24


def test_x11_offer_is_not_owned_once_the_server_names_another_owner(
        monkeypatch):
    """A copy made during the paste must survive: ask the server, the
    serving thread may not have seen the SelectionClear yet."""
    offer, served = _x11_offer(monkeypatch)
    assert offer.owned()
    served["owner"] = 0x5000001  # the user copied something meanwhile
    assert not offer.owned()
    served["owner"] = 0x4A00001
    served["thread"].alive = False  # SelectionClear ended the thread
    assert not offer.owned()


def test_a_bad_paste_chord_leaves_the_clipboard_untouched(monkeypatch):
    """The chord is resolved before the clipboard changes hands."""
    _pin_x11(monkeypatch)
    injector = dictate.PynputInjector()
    injector.controller = FakeMappingController(mapped=set())
    owned = _pin_clipboard(monkeypatch)

    def bad_chord(chord):
        raise ValueError(f"unknown key in {chord!r}")

    monkeypatch.setattr(injector, "_chord_keys", bad_chord)
    with pytest.raises(ValueError):
        injector._paste_text("Hallo")
    assert owned == []


@pytest.mark.parametrize("requestor, restored", [
    (CHROMIUM_CLIPBOARD, True),  # the focused app fetched: restore
    (CLIPBOARD_MANAGER, False),  # only a clipboard manager did: keep it
])
def test_x11_paste_restores_only_after_the_focused_app_fetched(
        monkeypatch, requestor, restored):
    _pin_x11(monkeypatch)
    injector = dictate.PynputInjector()
    injector.controller = FakeMappingController(mapped=set())
    owned = []
    monkeypatch.setattr(dictate, "_x11_clipboard_read", lambda: b"OLD")
    fake_x11_owner(monkeypatch, owned, focus=CHROMIUM_FOCUS,
                   requestor=requestor)
    monkeypatch.setattr(dictate, "PASTE_FETCH_TIMEOUT", 0.1)
    monkeypatch.setattr(dictate, "_CHORD_KEYS", {"ctrl": "<ctrl>"})
    monkeypatch.setattr(dictate, "foreground_window_is_terminal", lambda: False)
    assert injector._paste_text("名")
    assert owned == (["名".encode(), b"OLD"] if restored else ["名".encode()])


def test_x11_offer_stops_waiting_once_another_owner_took_over(monkeypatch):
    offer, served = _x11_offer(monkeypatch)
    offer.arm()
    served["thread"].alive = False  # SelectionClear: someone copied
    t0 = time.monotonic()
    offer.wait(5)
    assert time.monotonic() - t0 < 1
    assert not offer.owned()


def test_x11_offer_counts_every_client_when_the_root_has_focus(monkeypatch):
    """Focus on the root (focus-follows-mouse): no app to attribute to."""
    offer, served = _x11_offer(monkeypatch, focus=0x000101)
    offer.arm()
    served["on_request"](True, CHROMIUM_CLIPBOARD)
    assert offer.fetched.is_set()
