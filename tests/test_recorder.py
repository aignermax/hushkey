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


def _riff(pcm, declared_size=None):
    """Minimal RIFF/WAVE (int16 mono 16 kHz) around pcm; declared_size may lie."""
    fmt = (b"fmt " + (16).to_bytes(4, "little") + (1).to_bytes(2, "little")
           + (1).to_bytes(2, "little") + (16000).to_bytes(4, "little")
           + (32000).to_bytes(4, "little") + (2).to_bytes(2, "little")
           + (16).to_bytes(2, "little"))
    if declared_size is None:
        declared_size = len(pcm)
    body = fmt + b"data" + declared_size.to_bytes(4, "little") + pcm
    return b"RIFF" + (len(body)).to_bytes(4, "little") + b"WAVE" + body


def test_growing_wav_pcm_reads_unfinalized_file(tmp_path):
    pcm = b"\x01\x02" * 100
    f = tmp_path / "growing.wav"
    # pw-record finalizes sizes only on close — mid-record the header lies,
    # and how it lies depends on the writer (libsndfile uses 0, others -1)
    for declared in (0, 0xFFFFFFFF):
        f.write_bytes(_riff(pcm, declared_size=declared))
        assert recorder._growing_wav_pcm(str(f)) == pcm


def test_growing_wav_pcm_rejects_garbage(tmp_path):
    f = tmp_path / "junk.wav"
    f.write_bytes(b"nope")
    assert recorder._growing_wav_pcm(str(f)) is None


def test_pw_recorder_snapshot_copies_current_audio(tmp_path):
    pcm = b"\x00\x00" * 1600  # 0.1 s of silence
    src = tmp_path / "src.wav"
    src.write_bytes(_riff(pcm, declared_size=0))  # libsndfile's live placeholder
    rec = recorder.PwRecordRecorder()
    rec._proc = object()  # pretend pw-record is running
    rec._wav = str(src)
    snap = rec.snapshot_wav()
    try:
        with wave.open(snap, "rb") as fh:
            assert fh.getframerate() == 16000
            assert fh.readframes(fh.getnframes()) == pcm
    finally:
        os.remove(snap)


def test_pw_recorder_snapshot_without_recording():
    assert recorder.PwRecordRecorder().snapshot_wav() is None


def test_sounddevice_snapshot_without_recording(monkeypatch):
    monkeypatch.setitem(sys.modules, "sounddevice", fake_sd_module())
    assert recorder.SoundDeviceRecorder().snapshot_wav() is None


def test_sounddevice_snapshot_keeps_recording(monkeypatch):
    monkeypatch.setitem(sys.modules, "sounddevice", fake_sd_module())
    rec = recorder.SoundDeviceRecorder()
    rec.start()
    snap = rec.snapshot_wav()
    try:
        with wave.open(snap, "rb") as fh:
            assert fh.getnframes() == 1600
    finally:
        os.remove(snap)
    wav = rec.stop()  # the stream is unaffected by the snapshot
    try:
        with wave.open(wav, "rb") as fh:
            assert fh.getnframes() == 1600
    finally:
        os.remove(wav)
