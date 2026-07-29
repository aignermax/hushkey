"""Recorder selection and the sounddevice backend (with a fake PortAudio)."""
import os
import sys
import types
import wave

import numpy as np

import recorder


class FakePortAudioError(Exception):
    pass


def fake_sd_module(fail_at=(), emit=True):
    """A fake `sounddevice` module.

    Rates in `fail_at` raise PortAudioError when opened; with emit=True the
    stream delivers one 100 ms int16 chunk when started.
    """
    sd = types.ModuleType("sounddevice")
    sd.PortAudioError = FakePortAudioError
    sd.default = types.SimpleNamespace(device=[0, 1])
    sd.query_devices = lambda idx: {"default_samplerate": 44100.0}

    class Stream:
        def __init__(self, samplerate, channels, dtype, callback):
            if samplerate in fail_at:
                raise FakePortAudioError(f"rate {samplerate} unsupported")
            self.callback = callback

        def start(self):
            if emit:
                self.callback(np.full((1600, 1), 7, dtype=np.int16),
                              1600, None, None)

        def stop(self):
            pass

        def close(self):
            pass

    sd.InputStream = Stream
    return sd


def test_prefers_pw_record(monkeypatch):
    monkeypatch.setattr(recorder.shutil, "which", lambda cmd: "/usr/bin/pw-record")
    assert isinstance(recorder.pick_recorder(), recorder.PwRecordRecorder)


def test_falls_back_to_sounddevice(monkeypatch):
    monkeypatch.setattr(recorder.shutil, "which", lambda cmd: None)
    monkeypatch.setitem(sys.modules, "sounddevice", fake_sd_module())
    assert isinstance(recorder.pick_recorder(), recorder.SoundDeviceRecorder)


def test_no_backend_returns_none(monkeypatch):
    monkeypatch.setattr(recorder.shutil, "which", lambda cmd: None)
    monkeypatch.setitem(sys.modules, "sounddevice", None)  # makes import raise
    assert recorder.pick_recorder() is None


def test_sounddevice_writes_valid_wav(monkeypatch):
    monkeypatch.setitem(sys.modules, "sounddevice", fake_sd_module())
    rec = recorder.SoundDeviceRecorder()
    rec.start()
    wav = rec.stop()
    try:
        with wave.open(wav, "rb") as fh:
            assert fh.getnchannels() == 1
            assert fh.getsampwidth() == 2  # int16
            assert fh.getframerate() == 16000
            assert len(fh.readframes(fh.getnframes())) == 1600 * 2
    finally:
        os.remove(wav)


def test_sounddevice_falls_back_to_device_rate(monkeypatch):
    monkeypatch.setitem(sys.modules, "sounddevice", fake_sd_module(fail_at={16000}))
    rec = recorder.SoundDeviceRecorder()
    rec.start()
    wav = rec.stop()
    try:
        with wave.open(wav, "rb") as fh:
            assert fh.getframerate() == 44100
    finally:
        os.remove(wav)


def test_stop_without_audio_returns_none(monkeypatch):
    monkeypatch.setitem(sys.modules, "sounddevice", fake_sd_module(emit=False))
    rec = recorder.SoundDeviceRecorder()
    rec.start()
    assert rec.stop() is None


def test_pw_recorder_stop_without_start():
    assert recorder.PwRecordRecorder().stop() is None
