"""Real Vulkan inference; CI supplies Mesa software Vulkan, local runs use AMD.

Run with HUSHKEY_TEST_VULKAN=1 and WHISPER_CPP_SERVER pointing at the build.
"""
import os
from pathlib import Path
import urllib.request

import numpy as np
import pytest

pytestmark = pytest.mark.skipif(os.environ.get('HUSHKEY_TEST_VULKAN') != '1',
                                reason='requires a built Vulkan whisper-server')


def test_real_vulkan_speech_and_repeated_requests(tmp_path):
    from whisper_cpp import WhisperCppModel
    sample = tmp_path / 'speech.wav'
    urllib.request.urlretrieve(
        'https://raw.githubusercontent.com/ggml-org/whisper.cpp/'
        '927cfce34f31707e17f2bff35c349632fb9e2c3a/samples/jfk.wav', sample)
    model = WhisperCppModel('tiny')
    try:
        assert model.device.startswith('Vulkan')
        for _ in range(2):
            segments, info = model.transcribe(sample, language='en')
            assert 'country' in ' '.join(s.text for s in segments).lower()
            assert info.language == 'en'
            assert all(0 <= s.start <= s.end for s in segments)
        segments, _ = model.transcribe(np.zeros(16000, dtype=np.float32), language='de')
        assert segments == []
    finally:
        model.close()
    assert model.proc.poll() is not None
