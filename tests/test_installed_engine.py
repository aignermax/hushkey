"""Release smoke test: a fresh installer must leave a working persisted engine."""
import os
from pathlib import Path
import ssl
import urllib.request

import pytest


@pytest.mark.skipif(os.environ.get('HUSHKEY_TEST_INSTALLED') != '1',
                    reason='requires an actual completed installation')
def test_installed_selection_transcribes_real_speech(tmp_path):
    import certifi
    from acceleration import load_settings
    from whisper_cpp import requested_engine, WhisperCppModel
    import transcribe
    settings = load_settings()
    assert settings['version'] == 'v0.10.1'
    assert requested_engine() == settings['engine']
    sample = tmp_path / 'speech.wav'
    url = ('https://raw.githubusercontent.com/ggml-org/whisper.cpp/'
           '927cfce34f31707e17f2bff35c349632fb9e2c3a/samples/jfk.wav')
    with urllib.request.urlopen(url, context=ssl.create_default_context(cafile=certifi.where())) as response:
        sample.write_bytes(response.read())
    if settings['engine'] in ('vulkan', 'metal'):
        assert Path(settings['server']).is_file()
        model = WhisperCppModel('tiny', engine=settings['engine'])
    else:
        from faster_whisper import WhisperModel
        device, compute, _ = transcribe.pick_device()
        assert device == settings['device']
        model = WhisperModel('tiny', device=device, compute_type=compute)
    try:
        segments, info = model.transcribe(str(sample), language='en', vad_filter=True, beam_size=5)
        text = ' '.join(segment.text for segment in segments)
        assert 'country' in text.lower(), text
        assert info.language == 'en'
    finally:
        if hasattr(model, 'close'):
            model.close()
