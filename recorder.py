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

import itertools
import os
import shutil
import signal
import subprocess
import tempfile
import time
import wave

TARGET_RATE = 16000

_wav_counter = itertools.count()


def _wav_path():
    # counter: snapshot mode can mint two of these in the same millisecond
    return os.path.join(tempfile.gettempdir(),
                        f"whisper-ptt-{int(time.time() * 1000)}-"
                        f"{next(_wav_counter)}.wav")


def _write_wav(pcm_bytes, rate):
    """Wrap raw int16 mono PCM in a proper WAV file; return its path."""
    wav = _wav_path()
    with wave.open(wav, "wb") as fh:
        fh.setnchannels(1)
        fh.setsampwidth(2)  # int16
        fh.setframerate(rate)
        fh.writeframes(pcm_bytes)
    return wav


def _growing_wav_pcm(path):
    """PCM payload of a WAV that pw-record is still writing.

    pw-record finalizes the RIFF sizes only on close, so a snapshot read
    cannot trust the declared lengths — take the data chunk's payload up to
    what is actually on disk.
    """
    try:
        with open(path, "rb") as fh:
            blob = fh.read()
    except OSError:
        return None
    if len(blob) < 12 or blob[:4] != b"RIFF":
        return None
    off = 12
    while off + 8 <= len(blob):
        fourcc = blob[off:off + 4]
        size = int.from_bytes(blob[off + 4:off + 8], "little")
        if fourcc == b"data":
            payload = blob[off + 8:]
            # an unfinalized size (0 or 0xFFFFFFFF, depending on the writer)
            # overstates what's on disk; clip to the real payload either way
            return payload[:size] if 0 < size <= len(payload) else payload
        off += 8 + size + (size & 1)
    return None


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

    def snapshot_wav(self):
        """A copy of the audio recorded so far, or None when not recording.

        Used by streaming mode (PTT_STREAMING) to transcribe completed blocks
        while the key is still held. The source file keeps growing meanwhile.
        """
        if self._proc is None or self._wav is None:
            return None
        pcm = _growing_wav_pcm(self._wav)
        if not pcm:
            return None
        return _write_wav(pcm, TARGET_RATE)


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
        return _write_wav(pcm.tobytes(), self._rate)

    def snapshot_wav(self):
        """A copy of the audio recorded so far, or None when not recording.

        Used by streaming mode (PTT_STREAMING). Frames keep accumulating in
        the PortAudio callback; the snapshot simply copies what is there.
        """
        if self._stream is None or not self._frames:
            return None
        import numpy as np
        pcm = np.concatenate(self._frames)
        return _write_wav(pcm.tobytes(), self._rate)


def pick_recorder():
    """Return a recorder for this platform, or None if none is available."""
    if shutil.which("pw-record"):
        return PwRecordRecorder()
    try:
        return SoundDeviceRecorder()
    except Exception:
        return None
