"""Audio retention is opt-in, local to one machine, and strictly bounded."""
import hashlib
import json
import threading
from types import SimpleNamespace

import pytest

import dictate


@pytest.fixture
def debug(monkeypatch, tmp_path):
    monkeypatch.setattr(dictate, 'STATE_DIR', str(tmp_path))
    monkeypatch.setattr(dictate, 'STATE_PATH', str(tmp_path / 'state.json'))
    monkeypatch.setattr(dictate, 'CONFIG_PATH', str(tmp_path / 'config.json'))
    monkeypatch.setattr(dictate, 'LOG_PATH', str(tmp_path / 'dictate.log'))
    monkeypatch.setattr(dictate, 'audio_debug_machine_id', lambda: 'this-machine', raising=False)
    monkeypatch.setattr(dictate, 'notify', lambda *a: None)
    return tmp_path


def test_disabled_by_default_and_config_must_match_machine(debug):
    assert not dictate.audio_debug_enabled()
    config = debug / 'debug-audio.json'
    for value in [None, [], {}, {'enabled': True},
                  {'enabled': True, 'machine': 'another-machine'},
                  {'enabled': 'true', 'machine': 'this-machine'}]:
        config.write_text(json.dumps(value), encoding='utf-8')
        assert not dictate.audio_debug_enabled()
    config.write_text('{broken', encoding='utf-8')
    assert not dictate.audio_debug_enabled()
    dictate.configure_audio_debug(True)
    assert dictate.audio_debug_enabled()
    dictate.configure_audio_debug(False)
    assert not dictate.audio_debug_enabled()


def test_retains_exactly_last_two_originals_with_metadata(debug):
    dictate.configure_audio_debug(True)
    for i in range(4):
        wav = debug / f'input-{i}.wav'
        wav.write_bytes(b'original unprocessed audio' + bytes([i]))
        dictate.retain_debug_audio(str(wav), {'text': f'text {i}', 'model': 'large-v3-turbo'})
    folder = debug / 'debug-audio'
    wavs = sorted(folder.glob('recording-*.wav'))
    assert len(wavs) == 2
    assert [p.read_bytes()[-1] for p in wavs] == [2, 3]
    assert len(list(folder.glob('*.json'))) == 2
    for p in wavs:
        meta = json.loads(p.with_suffix('.json').read_text(encoding='utf-8'))
        assert meta['audio_sha256'] == hashlib.sha256(p.read_bytes()).hexdigest()
        assert meta['text'] == f'text {p.read_bytes()[-1]}'
    unrelated = folder / 'my-own-recording.wav'
    unrelated.write_bytes(b'keep me')
    dictate.configure_audio_debug(False)
    assert not list(folder.glob('recording-*'))
    assert unrelated.read_bytes() == b'keep me'


@pytest.mark.parametrize('enabled,fail', [(False, False), (True, False), (True, True)])
def test_final_recording_retained_on_success_or_error_only_when_enabled(debug, enabled, fail):
    if enabled:
        dictate.configure_audio_debug(True)
    wav = debug / 'capture.wav'
    wav.write_bytes(b'exact original wav')
    daemon = dictate.DictationDaemon.__new__(dictate.DictationDaemon)
    daemon.busy_lock = threading.Lock()
    daemon.recording = None
    daemon.model = SimpleNamespace(device='Vulkan0')
    inserted = []
    daemon.injector = SimpleNamespace(insert=inserted.append)
    def transcribe(*args):
        if fail:
            raise RuntimeError('decode error')
        return [SimpleNamespace(text='Transcript with hallucinated ending.')]
    daemon._transcribe = transcribe
    daemon._transcribe_and_insert(str(wav), 120.0)
    assert not wav.exists()
    saved = list((debug / 'debug-audio').glob('*.wav'))
    assert len(saved) == int(enabled)
    if enabled:
        assert saved[0].read_bytes() == b'exact original wav'
        meta = json.loads(saved[0].with_suffix('.json').read_text(encoding='utf-8'))
        assert meta['duration_seconds'] == 120.0
        assert meta['error_type'] == ('RuntimeError' if fail else None)
        assert meta['text'] == (None if fail else 'Transcript with hallucinated ending.')
    assert inserted == ([] if fail else ['Transcript with hallucinated ending. '])


def test_debug_write_failure_is_best_effort(debug, monkeypatch):
    dictate.configure_audio_debug(True)
    (debug / 'debug-audio').write_text('not a directory', encoding='utf-8')
    wav = debug / 'input.wav'
    wav.write_bytes(b'audio')
    # Retention is explicitly best effort; original cleanup is still owned by
    # the final dictation path even if the private debug folder is unavailable.
    dictate.retain_debug_audio(str(wav), {'text': 'result'})
    assert wav.read_bytes() == b'audio'


def test_disk_and_log_failure_still_cleans_original(debug, monkeypatch):
    dictate.configure_audio_debug(True)
    (debug / 'debug-audio').write_text('blocked', encoding='utf-8')
    def log(message):
        if message.startswith('audio debug'):
            raise OSError('disk full')
    monkeypatch.setattr(dictate, 'log', log)
    wav = debug / 'original.wav'
    wav.write_bytes(b'audio')
    daemon = dictate.DictationDaemon.__new__(dictate.DictationDaemon)
    daemon.busy_lock = threading.Lock()
    daemon.recording = None
    daemon._transcribe = lambda *args: [SimpleNamespace(text='result')]
    inserted = []
    daemon.injector = SimpleNamespace(insert=inserted.append)
    daemon._transcribe_and_insert(str(wav), 1.0)
    assert not wav.exists()
    assert inserted == ['result ']
    assert json.loads((debug / 'state.json').read_text())['state'] == 'idle'


def test_cli_status_does_not_start_daemon_and_off_purges(debug, monkeypatch, capsys):
    monkeypatch.setattr(dictate.DictationDaemon, 'run', lambda _: pytest.fail('started daemon'))
    assert dictate.main(['--debug-audio', 'status']) == 0
    assert 'off' in capsys.readouterr().out
    assert dictate.main(['--debug-audio', 'on']) == 0
    assert dictate.audio_debug_enabled()
    wav = debug / 'original.wav'
    wav.write_bytes(b'audio')
    dictate.retain_debug_audio(str(wav), {})
    assert dictate.main(['--debug-audio', 'off']) == 0
    assert not list((debug / 'debug-audio').glob('recording-*'))


def test_concurrent_retention_stays_bounded(debug):
    from concurrent.futures import ThreadPoolExecutor
    dictate.configure_audio_debug(True)
    inputs = [debug / f'original-{i}.wav' for i in range(6)]
    for i, p in enumerate(inputs):
        p.write_bytes(bytes([i]) * 8192)
    with ThreadPoolExecutor(max_workers=3) as workers:
        list(workers.map(lambda p: dictate.retain_debug_audio(str(p), {}), inputs))
    kept = list((debug / 'debug-audio').glob('recording-*.wav'))
    assert len(kept) == 2
    assert len(list((debug / 'debug-audio').glob('recording-*.json'))) == 2
    assert all(p.with_suffix('.json').exists() for p in kept)
