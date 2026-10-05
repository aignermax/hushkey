"""Contract tests for the local whisper.cpp adapter (no GPU required)."""
import io
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import threading
import time
import wave
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from types import SimpleNamespace

import numpy as np
import pytest


@pytest.fixture
def endpoint():
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            requests.append(self.rfile.read(int(self.headers['Content-Length'])))
            payload = json.dumps({'language': 'german', 'duration': 2,
                                  'segments': [{'text': ' Hallo!', 'start': .2, 'end': 1.8}]}).encode()
            self.send_response(200)
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, *args):
            pass

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield f'http://127.0.0.1:{server.server_port}', requests
    server.shutdown()
    server.server_close()
    thread.join()


def client(endpoint):
    from whisper_cpp import WhisperCppModel
    model = WhisperCppModel.__new__(WhisperCppModel)
    model.url = endpoint[0]
    model.lock = threading.Lock()
    model.proc = SimpleNamespace(poll=lambda: None)
    return model


def test_pcm_transcription_preserves_timestamps_language_and_prompt(endpoint):
    model = client(endpoint)
    segments, info = model.transcribe(np.zeros(32000, dtype=np.float32),
                                      language=None, vad_filter=False,
                                      initial_prompt='简体中文', temperature=0.0, beam_size=5)
    assert [(s.text, s.start, s.end) for s in segments] == [(' Hallo!', .2, 1.8)]
    assert info.language == 'de' and info.duration == 2
    body = endpoint[1][0]
    assert b'RIFF' in body and b'auto' in body
    assert '简体中文'.encode() in body
    assert b'name="temperature_inc"\r\n\r\n0.0' in body
    # Native verbose_json otherwise defaults to token wrapping at 60 chars,
    # which can split a word before the app joins segments with spaces.
    assert b'name="token_timestamps"\r\n\r\nfalse' in body


def test_silence_does_not_call_server(endpoint, monkeypatch):
    import faster_whisper.vad
    monkeypatch.setattr(faster_whisper.vad, 'get_speech_timestamps', lambda audio: [])
    segments, info = client(endpoint).transcribe(np.zeros(16000, dtype=np.float32), language='de')
    assert list(segments) == [] and info.duration == 1
    assert endpoint[1] == []


def test_vad_keeps_original_timeline(endpoint, monkeypatch):
    import faster_whisper.vad
    monkeypatch.setattr(faster_whisper.vad, 'get_speech_timestamps',
                        lambda audio: [{'start': 16000, 'end': 32000}])
    model = client(endpoint)
    segments, _ = model.transcribe(np.zeros(48000, dtype=np.float32))
    assert segments[0].end == 1.8
    body = endpoint[1][0]
    wav_data = body[body.index(b'RIFF'):].split(b'\r\n--')[0]
    with wave.open(io.BytesIO(wav_data)) as wav:
        assert wav.getnframes() == 48000  # do not shift streaming offsets


def test_dead_server_has_actionable_error(endpoint):
    model = client(endpoint)
    model.proc = SimpleNamespace(poll=lambda: 1)
    with pytest.raises(RuntimeError, match='restart'):
        model.transcribe(np.ones(16000, dtype=np.float32), vad_filter=False)


def test_engine_selection_is_explicit_and_validated(monkeypatch):
    from whisper_cpp import requested_engine
    monkeypatch.delenv('WHISPER_ENGINE', raising=False)
    assert requested_engine() == 'faster-whisper'
    monkeypatch.setenv('WHISPER_ENGINE', 'vulkan')
    assert requested_engine() == 'vulkan'
    monkeypatch.setenv('WHISPER_ENGINE', 'typo')
    with pytest.raises(ValueError, match='WHISPER_ENGINE'):
        requested_engine()


def test_model_alias_and_local_path(tmp_path, monkeypatch):
    from whisper_cpp import resolve_model
    local = tmp_path / 'my model.bin'
    local.write_bytes(b'model')
    assert resolve_model(str(local)) == str(local)
    import huggingface_hub
    calls = []
    monkeypatch.setattr(huggingface_hub, 'hf_hub_download',
                        lambda **kw: calls.append(kw) or '/cached/model.bin')
    assert resolve_model('large') == '/cached/model.bin'
    assert calls[0]['filename'] == 'ggml-large-v3.bin'
    with pytest.raises(ValueError, match='model'):
        resolve_model('someone/faster-whisper-private')


def test_missing_executable_fails_before_model_download(monkeypatch):
    from whisper_cpp import WhisperCppModel
    monkeypatch.setenv('WHISPER_CPP_SERVER', '/missing/whisper-server')
    with pytest.raises(RuntimeError, match='setup_vulkan'):
        WhisperCppModel('tiny')


def test_gpu_proof_rejects_cpu_fallback():
    from whisper_cpp import vulkan_device
    assert vulkan_device('whisper_backend_init_gpu: using Vulkan0 backend') == 'Vulkan0'
    assert vulkan_device('ggml_vulkan: Found 1 Vulkan devices\nwhisper_backend_init_gpu: no GPU found') is None


def test_daemon_uses_vulkan_without_constructing_faster_whisper(monkeypatch, tmp_path):
    import dictate
    import whisper_cpp
    monkeypatch.setenv('WHISPER_ENGINE', 'vulkan')
    monkeypatch.setenv('WHISPER_MODEL', 'tiny')
    model = SimpleNamespace(device='Vulkan0')
    monkeypatch.setattr(whisper_cpp, 'WhisperCppModel', lambda name: model)
    monkeypatch.setattr(dictate, 'log', lambda text: None)
    daemon = dictate.DictationDaemon.__new__(dictate.DictationDaemon)
    daemon.load_model()
    assert daemon.model is model and dictate.CURRENT_MODEL == 'tiny'


def test_batch_uses_vulkan(monkeypatch, tmp_path):
    import transcribe
    import whisper_cpp
    monkeypatch.setenv('WHISPER_ENGINE', 'vulkan')
    closed = []
    model = SimpleNamespace(device='Vulkan0',
        transcribe=lambda *a, **kw: ([SimpleNamespace(text='Hallo AMD')],
                                     SimpleNamespace(language='de', duration=1)),
        close=lambda: closed.append(True))
    monkeypatch.setattr(whisper_cpp, 'WhisperCppModel', lambda name: model)
    audio = tmp_path / 'memo.wav'
    audio.write_bytes(b'wav')
    assert transcribe.main([str(audio), '--out', str(tmp_path / 'notes')]) == 0
    assert 'Hallo AMD' in (tmp_path / 'notes/memo.md').read_text(encoding='utf-8')
    assert closed == [True]


@pytest.fixture
def native_stub(tmp_path):
    """Real child process and HTTP socket, substituting only the native engine."""
    path = tmp_path / 'server with spaces.py'
    path.write_text('''
import argparse, json
from http.server import HTTPServer, BaseHTTPRequestHandler
p=argparse.ArgumentParser()
p.add_argument('--port', type=int)
p.add_argument('--request-path')
args,_=p.parse_known_args()
class Handler(BaseHTTPRequestHandler):
 def do_GET(self):
  self.send_response(200); self.end_headers()
  self.wfile.write(b'{"status":"ok"}')
 def log_message(self,*args): pass
server=HTTPServer(('127.0.0.1',args.port),Handler)
print('whisper_backend_init_gpu: using Vulkan0 backend',flush=True)
server.serve_forever()
''', encoding='utf-8')
    return path


def test_model_lifecycle_owns_server_and_close_is_idempotent(monkeypatch, native_stub):
    import whisper_cpp
    monkeypatch.setenv('WHISPER_CPP_SERVER', sys.executable)
    monkeypatch.setattr(whisper_cpp, 'resolve_model', lambda _: 'tiny.bin')
    real_popen = subprocess.Popen

    def spawn(command, **kwargs):
        return real_popen(command[:3] + [sys.executable, str(native_stub)] + command[4:], **kwargs)

    monkeypatch.setattr(whisper_cpp.subprocess, 'Popen', spawn)
    model = whisper_cpp.WhisperCppModel('tiny', startup_timeout=5)
    port = int(model.url.split(':')[2].split('/')[0])
    assert model.device == 'Vulkan0'
    model.close()
    model.close()
    with socket.socket() as sock:
        assert sock.connect_ex(('127.0.0.1', port)) != 0


def test_supervisor_reaps_child_when_application_is_forcibly_terminated(tmp_path, native_stub):
    import whisper_cpp
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    parent_script = tmp_path / 'parent.py'
    parent_script.write_text('''
import subprocess, sys, time
child = subprocess.Popen(sys.argv[1:], stdin=subprocess.PIPE)
time.sleep(60)
''', encoding='utf-8')
    parent = subprocess.Popen([sys.executable, str(parent_script), sys.executable,
        whisper_cpp.__file__, '--supervise', sys.executable, str(native_stub),
        '--port', str(port), '--request-path', '/test'], stdout=subprocess.PIPE,
        stderr=subprocess.PIPE)
    try:
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            with socket.socket() as sock:
                if sock.connect_ex(('127.0.0.1', port)) == 0:
                    break
            time.sleep(.1)
        else:
            pytest.fail('native test server did not start')
        parent.terminate()
        parent.wait(timeout=5)
        # EOF on stdout proves BOTH supervisor and its native child released
        # their inherited pipe handles, even though the application was killed.
        parent.communicate(timeout=10)
        with socket.socket() as sock:
            assert sock.connect_ex(('127.0.0.1', port)) != 0
    finally:
        if parent.poll() is None:
            parent.kill()
            parent.wait()


def test_startup_timeout_cleans_up_child(monkeypatch, native_stub):
    import whisper_cpp
    monkeypatch.setenv('WHISPER_CPP_SERVER', sys.executable)
    monkeypatch.setattr(whisper_cpp, 'resolve_model', lambda _: 'tiny.bin')
    processes = []
    real_popen = subprocess.Popen

    def spawn(command, **kwargs):
        child = real_popen(command[:3] + [sys.executable, '-c',
            'import time; time.sleep(60)'], **kwargs)
        processes.append(child)
        return child

    monkeypatch.setattr(whisper_cpp.subprocess, 'Popen', spawn)
    with pytest.raises(RuntimeError, match='timed out'):
        whisper_cpp.WhisperCppModel('tiny', startup_timeout=.1)
    assert processes[0].poll() is not None


def test_pythonw_supervisor_uses_a_working_stdin_pipe(tmp_path):
    if sys.platform != 'win32':
        pytest.skip('pythonw is Windows-only')
    import whisper_cpp
    pythonw = Path(sys.executable).with_name('pythonw.exe')
    proc = subprocess.Popen([str(pythonw), whisper_cpp.__file__, '--supervise',
        sys.executable, '-c', 'import time; time.sleep(60)'],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    proc.stdin.close()
    proc.wait(timeout=10)
    error = proc.stderr.read().decode(errors='replace')
    proc.stdout.close()
    proc.stderr.close()
    assert 'Exception' not in error and 'Traceback' not in error
