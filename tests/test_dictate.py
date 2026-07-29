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

    monkeypatch.setattr(d, "_transcribe_and_type", fake_transcribe)
    d.start_recording()
    d.recording = time.time() - 1.0  # pretend the key was held for 1 s
    d.stop_recording()
    assert done.wait(2)
    assert seen["wav"] == wav


def test_missing_backend_does_not_crash(monkeypatch):
    monkeypatch.setattr(dictate, "pick_recorder", lambda: None)
    notes = []
    monkeypatch.setattr(dictate, "notify", lambda *a: notes.append(a))
    d = dictate.DictationDaemon()
    d.start_recording()
    assert d.recording is None
    assert notes  # user got a "no recorder" notification
