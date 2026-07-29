"""Audio recording backends for whisper-ptt, picked per platform.

- Linux with PipeWire: pw-record subprocess (the original backend).
- Everywhere else (Windows, macOS, Linux without PipeWire): sounddevice,
  i.e. PortAudio (WASAPI / Core Audio / ALSA).

Both recorders expose start() / stop(); stop() returns the path of the
recorded WAV (or None if nothing was captured). faster-whisper resamples
internally via PyAV, so the sounddevice backend may write the device's
native sample rate when 16 kHz is refused — no manual resampling needed.
"""
from __future__ import annotations

import os
import shutil
import signal
import subprocess
import tempfile
import time
import wave

TARGET_RATE = 16000


def _wav_path():
    return os.path.join(tempfile.gettempdir(),
                        f"whisper-ptt-{int(time.time() * 1000)}.wav")


class PwRecordRecorder:
    """Linux/PipeWire: pw-record writes the WAV file directly."""

    def __init__(self):
        self._proc = None
        self._wav = None

    def start(self):
        self._wav = _wav_path()
        self._proc = subprocess.Popen(
            ["pw-record", "--rate", str(TARGET_RATE), "--channels", "1",
             self._wav],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def stop(self):
        proc, wav = self._proc, self._wav
        self._proc = self._wav = None
        if proc is None:
            return None
        proc.send_signal(signal.SIGINT)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        return wav if wav and os.path.isfile(wav) else None


class SoundDeviceRecorder:
    """Windows/macOS (and Linux fallback): record via PortAudio."""

    def __init__(self):
        import sounddevice as sd  # raises a clear error if PortAudio is missing
        self._sd = sd
        self._stream = None
        self._frames = []
        self._rate = TARGET_RATE

    def _open(self, rate):
        self._rate = rate
        self._stream = self._sd.InputStream(
            samplerate=rate, channels=1, dtype="int16",
            callback=lambda data, frames, t, status:
                self._frames.append(data.copy()))
        self._stream.start()

    def start(self):
        self._frames = []
        try:
            self._open(TARGET_RATE)
        except self._sd.PortAudioError:
            # Some devices refuse 16 kHz — fall back to the device's own rate
            # (faster-whisper resamples to 16 kHz while decoding).
            info = self._sd.query_devices(self._sd.default.device[0])
            self._open(int(info["default_samplerate"]))

    def stop(self):
        stream = self._stream
        self._stream = None
        if stream is None:
            return None
        stream.stop()
        stream.close()
        if not self._frames:
            return None
        import numpy as np
        pcm = np.concatenate(self._frames)
        wav = _wav_path()
        with wave.open(wav, "wb") as fh:
            fh.setnchannels(1)
            fh.setsampwidth(2)  # int16
            fh.setframerate(self._rate)
            fh.writeframes(pcm.tobytes())
        return wav


def pick_recorder():
    """Return a recorder for this platform, or None if none is available."""
    if shutil.which("pw-record"):
        return PwRecordRecorder()
    try:
        return SoundDeviceRecorder()
    except Exception:
        return None
