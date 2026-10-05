"""Real Vulkan inference; CI supplies Mesa software Vulkan, local runs use AMD.

Run with HUSHKEY_TEST_VULKAN=1 and WHISPER_CPP_SERVER pointing at the build.
"""
import os
from pathlib import Path
import shutil
import subprocess
import threading
import time
from types import SimpleNamespace
import urllib.request

import numpy as np
import pytest

@pytest.mark.skipif(os.environ.get('HUSHKEY_TEST_VULKAN') != '1',
                   reason='requires a built Vulkan whisper-server and Vulkan device')
def test_real_vulkan_speech_and_repeated_requests(tmp_path, monkeypatch):
    from whisper_cpp import WhisperCppModel, resolve_model
    sample = tmp_path / 'speech.wav'
    urllib.request.urlretrieve(
        'https://raw.githubusercontent.com/ggml-org/whisper.cpp/'
        '927cfce34f31707e17f2bff35c349632fb9e2c3a/samples/jfk.wav', sample)
    local_model = tmp_path / '模型-é.bin'
    shutil.copyfile(resolve_model('tiny'), local_model)
    model = WhisperCppModel(str(local_model))
    try:
        assert model.device.startswith('Vulkan')
        for _ in range(2):
            segments, info = model.transcribe(sample, language='en')
            assert 'country' in ' '.join(s.text for s in segments).lower()
            assert info.language == 'en'
            assert all(0 <= s.start <= s.end for s in segments)
        segments, _ = model.transcribe(np.zeros(16000, dtype=np.float32), language='de')
        assert segments == []
        # Real press/release -> native GPU model -> text insertion flow, with
        # only the microphone and desktop replaced by controlled test seams.
        import dictate
        clip = tmp_path / 'recording.wav'
        shutil.copyfile(sample, clip)
        inserted = []
        idle = threading.Event()
        monkeypatch.setenv('WHISPER_LANG', 'en')
        monkeypatch.setattr(dictate, 'PTT_KEY', 'f9')
        monkeypatch.setattr(dictate, 'STREAMING', False)
        monkeypatch.setattr(dictate, 'notify', lambda *args: None)
        monkeypatch.setattr(dictate, 'log', lambda text: None)
        monkeypatch.setattr(dictate, 'write_state',
                            lambda state: idle.set() if state == 'idle' else None)
        monkeypatch.setattr(dictate, 'pick_recorder', lambda: SimpleNamespace(
            start=lambda: None, stop=lambda: str(clip)))
        monkeypatch.setattr(dictate, 'make_backends', lambda key: (
            None, SimpleNamespace(insert=lambda text: inserted.append(text))))
        daemon = dictate.DictationDaemon()
        daemon.model = model
        daemon.start_recording()
        daemon.start_recording()  # key autorepeat must not duplicate output
        daemon.recording = time.time() - 11
        daemon.stop_recording()
        assert idle.wait(120), 'GPU dictation worker did not finish'
        assert len(inserted) == 1 and 'country' in inserted[0].lower()
        assert not clip.exists()
    finally:
        model.close()
    assert model.proc.poll() is not None


@pytest.mark.skipif(os.environ.get('HUSHKEY_TEST_CPP_BINARY') != '1',
                   reason='requires a built whisper-server; no GPU necessary')
def test_native_unicode_model_loads_and_cpu_fallback_is_rejected(tmp_path, monkeypatch):
    import whisper_cpp
    local_model = tmp_path / '模型-é.bin'
    shutil.copyfile(whisper_cpp.resolve_model('tiny'), local_model)
    real_popen = subprocess.Popen

    def cpu_server(command, **kwargs):
        return real_popen(command + ['--no-gpu'], **kwargs)

    monkeypatch.setattr(whisper_cpp.subprocess, 'Popen', cpu_server)
    with pytest.raises(RuntimeError, match='did not initialize a hardware GPU'):
        whisper_cpp.WhisperCppModel(str(local_model), startup_timeout=30, engine='vulkan')
