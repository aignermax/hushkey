"""End-to-end: real faster-whisper 'tiny' model on a synthetic WAV.

Downloads ~75 MB on first run (cached in ~/.cache/huggingface). Runs on every
OS in CI — this is what verifies the ctranslate2/PyAV wheels per platform,
without needing a microphone or a display. No transcript content is asserted
(a sine tone may legitimately transcribe to nothing).
"""
import math
import wave

import numpy as np

import transcribe


def make_tone(path, seconds=1.5, rate=16000, freq=440.0):
    t = np.arange(int(seconds * rate)) / rate
    pcm = (np.sin(2 * math.pi * freq * t) * 10000).astype(np.int16)
    with wave.open(str(path), "wb") as fh:
        fh.setnchannels(1)
        fh.setsampwidth(2)
        fh.setframerate(rate)
        fh.writeframes(pcm.tobytes())


def test_tiny_model_transcribes_synthetic_wav(tmp_path):
    wav = tmp_path / "tone.wav"
    make_tone(wav)
    out = tmp_path / "notes"
    rc = transcribe.main([str(wav), "--out", str(out), "--model", "tiny",
                          "--language", "en"])
    assert rc == 0
    note = out / "tone.md"
    assert note.is_file()
    assert "type: transcript" in note.read_text(encoding="utf-8")
