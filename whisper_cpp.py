"""Persistent, local whisper.cpp Vulkan adapter for dictation and batch mode.

The supervisor entry point owns the native child until the parent's stdin pipe
closes. This also works when the tray uses TerminateProcess on Windows, where
Python finally/atexit handlers in the daemon do not run.
"""
from __future__ import annotations

import atexit
from collections import deque
import io
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import socket
import subprocess
import sys
import threading
import time
from types import SimpleNamespace
import urllib.error
import urllib.request
import wave


def requested_engine():
    from acceleration import load_settings
    engine = (os.environ['WHISPER_ENGINE'] if 'WHISPER_ENGINE' in os.environ
              else load_settings().get('engine', 'faster-whisper')).strip().lower()
    if engine not in ('faster-whisper', 'vulkan', 'metal'):
        raise ValueError('WHISPER_ENGINE must be faster-whisper, vulkan or metal')
    return engine


def runtime_dir():
    if sys.platform == 'win32':
        base = Path(os.environ.get('LOCALAPPDATA', Path.home() / 'AppData/Local'))
    else:
        base = Path(os.environ.get('XDG_DATA_HOME', Path.home() / '.local/share'))
    return base / 'hushkey' / 'vulkan'


def server_path():
    override = os.environ.get('WHISPER_CPP_SERVER')
    if override:
        return shutil.which(override) or override
    from acceleration import load_settings
    installed_settings = load_settings()
    if installed_settings.get('server'):
        return installed_settings['server']
    name = 'whisper-server.exe' if sys.platform == 'win32' else 'whisper-server'
    installed = runtime_dir() / 'bin' / name
    return str(installed) if installed.is_file() else shutil.which(name)


def resolve_model(name):
    if Path(name).is_file():
        return str(Path(name).resolve())
    name = {'large': 'large-v3', 'turbo': 'large-v3-turbo'}.get(name, name)
    supported = {'tiny', 'tiny.en', 'base', 'base.en', 'small', 'small.en',
                 'medium', 'medium.en', 'large-v1', 'large-v2', 'large-v3',
                 'large-v3-turbo'}
    if name not in supported:
        raise ValueError(f'Unsupported whisper.cpp model {name!r}; use a standard '
                         'Whisper size or a local GGML .bin file')
    from huggingface_hub import hf_hub_download
    return hf_hub_download(repo_id='ggerganov/whisper.cpp', filename=f'ggml-{name}.bin')


def vulkan_device(log):
    match = re.search(r'whisper_backend_init_gpu: using (Vulkan\d+) backend', log)
    return match.group(1) if match else None


def native_device(log):
    match = re.search(r'whisper_backend_init_gpu: using (Vulkan\d+|Metal\d*|MTL\d+) backend', log)
    return match.group(1) if match else None


def server_environment():
    env = os.environ.copy()
    # Capture/overlay layers can intercept compute and stall inference (observed
    # on RX 7600 XT). Scope this to our child, leaving other GPU apps untouched.
    # Requires Vulkan loader >= 1.3.262; older loaders ignore the filter.
    if env.get('WHISPER_CPP_ALLOW_LAYERS') != '1':
        env.setdefault('VK_LOADER_LAYERS_DISABLE', '~implicit~')
    return env


# whisper.cpp verbose_json uses full language names, whereas the application's
# Chinese re-decode logic and note metadata need Whisper language codes.
_LANGUAGES = dict(pair.split('=') for pair in '''
english=en chinese=zh german=de spanish=es russian=ru korean=ko french=fr
japanese=ja portuguese=pt turkish=tr polish=pl catalan=ca dutch=nl arabic=ar
swedish=sv italian=it indonesian=id hindi=hi finnish=fi vietnamese=vi hebrew=he
ukrainian=uk greek=el malay=ms czech=cs romanian=ro danish=da hungarian=hu tamil=ta
norwegian=no thai=th urdu=ur croatian=hr bulgarian=bg lithuanian=lt latin=la maori=mi
malayalam=ml welsh=cy slovak=sk telugu=te persian=fa latvian=lv bengali=bn serbian=sr
azerbaijani=az slovenian=sl kannada=kn estonian=et macedonian=mk breton=br basque=eu
icelandic=is armenian=hy nepali=ne mongolian=mn bosnian=bs kazakh=kk albanian=sq
swahili=sw galician=gl marathi=mr punjabi=pa sinhala=si khmer=km shona=sn yoruba=yo
somali=so afrikaans=af occitan=oc georgian=ka belarusian=be tajik=tg sindhi=sd
gujarati=gu amharic=am yiddish=yi lao=lo uzbek=uz faroese=fo haitian_creole=ht
pashto=ps turkmen=tk nynorsk=nn maltese=mt sanskrit=sa luxembourgish=lb myanmar=my
tibetan=bo tagalog=tl malagasy=mg assamese=as tatar=tt hawaiian=haw lingala=ln
hausa=ha bashkir=ba javanese=jw sundanese=su cantonese=yue
'''.split())


def _http(request, timeout):
    # Never send dictation through HTTP_PROXY, even if NO_PROXY is misconfigured.
    return urllib.request.build_opener(urllib.request.ProxyHandler({})).open(
        request, timeout=timeout)


class WhisperCppModel:
    def __init__(self, name, *, startup_timeout=180, executable=None, gpu=None, engine=None):
        executable = executable or server_path()
        if not executable or not Path(executable).is_file():
            raise RuntimeError('Native engine missing: run python setup_acceleration.py '
                               '(legacy: setup_vulkan.py) '
                               'or set WHISPER_CPP_SERVER to whisper-server')
        from acceleration import load_settings
        if gpu is None:
            saved = load_settings().get('device', 0)
            gpu = int(os.environ.get('WHISPER_CPP_DEVICE', saved if isinstance(saved, int) else 0))
        self.engine = engine or requested_engine()
        if gpu < 0:
            raise ValueError('WHISPER_CPP_DEVICE must be a non-negative GPU index')
        model = resolve_model(name)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        token = '/' + secrets.token_hex(24)
        self.url = f'http://127.0.0.1:{port}{token}'
        self.lock = threading.Lock()
        self.device = None
        self._startup_error = False
        self._startup_log = deque(maxlen=80)
        self.gpu_devices = {}
        self._ready = False
        command = [str(Path(executable).resolve()), '-m', model, '--host', '127.0.0.1',
                   '--port', str(port), '--request-path', token, '--device', str(gpu),
                   '--no-language-probabilities']
        self.proc = subprocess.Popen(
            [sys.executable, str(Path(__file__).resolve()), '--supervise', *command],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            env=server_environment(),
            creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
        self.reader = threading.Thread(target=self._drain, daemon=True)
        self.reader.start()
        try:
            deadline = time.monotonic() + startup_timeout
            while time.monotonic() < deadline:
                if self.proc.poll() is not None:
                    self.reader.join(timeout=1)
                    raise RuntimeError('whisper-server exited during startup; check the '
                                       'model, GPU driver and executable dependencies.\n' +
                                       ''.join(self._startup_log))
                try:
                    with _http(self.url + '/health', timeout=.5) as response:
                        ready = json.load(response).get('status') == 'ok'
                    if ready:
                        # Logs are drained concurrently; allow the reader to catch up.
                        for _ in range(20):
                            if self.device or self._startup_error:
                                break
                            time.sleep(.05)
                        if not self.device or self._startup_error:
                            raise RuntimeError('whisper.cpp did not initialize a hardware GPU; '
                                               'check drivers and WHISPER_CPP_DEVICE. '
                                               f'CPU fallback is disabled for WHISPER_ENGINE={self.engine}.\n' +
                                               ''.join(self._startup_log))
                        expected = ('metal', 'mtl') if self.engine == 'metal' else (self.engine,)
                        if self.engine in ('vulkan', 'metal') and not self.device.lower().startswith(expected):
                            raise RuntimeError(f'Requested {self.engine}, but initialized {self.device}')
                        atexit.register(self.close)
                        self._ready = True
                        self._startup_log.clear()
                        return
                except (urllib.error.URLError, TimeoutError, OSError):
                    pass
                time.sleep(.1)
            raise RuntimeError('GPU model startup timed out; check driver and available VRAM\n' + ''.join(self._startup_log))
        except BaseException:
            self.close()
            raise

    def _drain(self):
        # Discard all native output rather than storing transcripts or unbounded
        # logs. Retain only the startup GPU identity and failure flag.
        for raw in self.proc.stdout:
            line = raw.decode('utf-8', errors='replace')
            if not self._ready:
                self._startup_log.append(line)
            device = native_device(line)
            if device:
                self.device = device
            match = re.search(r'ggml_vulkan: (\d+) = (.*?) \| uma: (\d)', line)
            if match:
                self.gpu_devices[int(match[1])] = {'name': match[2], 'integrated': match[3] == '1'}
            if re.search(r'llvmpipe|lavapipe|swiftshader|software rasterizer', line, re.I):
                # Only the selected Vulkan device is rejected below; systems may
                # enumerate a software driver alongside their real GPU.
                self.gpu_devices.setdefault(-1, {'name': line, 'integrated': True})
            if 'failed to initialize' in line and 'backend' in line:
                self._startup_error = True

    def close(self):
        proc = getattr(self, 'proc', None)
        if proc is None:
            return
        atexit.unregister(self.close)
        if proc.stdin and not proc.stdin.closed:
            proc.stdin.close()  # supervisor sees EOF and reaps the native server
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
        if getattr(self, 'reader', None):
            self.reader.join(timeout=2)
        if proc.stdout:
            proc.stdout.close()

    def transcribe(self, source, *, language=None, vad_filter=True, beam_size=5,
                   initial_prompt=None, temperature=None):
        import numpy as np
        from faster_whisper.audio import decode_audio
        from faster_whisper.vad import get_speech_timestamps

        with self.lock:
            if self.proc.poll() is not None:
                raise RuntimeError('Vulkan transcription server stopped; restart hushkey')
            audio = source if isinstance(source, np.ndarray) else decode_audio(
                os.fspath(source) if isinstance(source, os.PathLike) else source,
                sampling_rate=16000)
            audio = np.asarray(audio, dtype=np.float32)
            if audio.ndim != 1 or not np.isfinite(audio).all():
                raise ValueError('Audio must be finite mono PCM at 16000 Hz')
            duration = len(audio) / 16000
            if not len(audio):
                return [], SimpleNamespace(language=language, duration=duration)
            if vad_filter:
                speech = get_speech_timestamps(audio)
                if not speech:
                    return [], SimpleNamespace(language=language, duration=duration)
                # Mask rather than concatenate to keep streaming timestamps on
                # the original recording timeline.
                masked = np.zeros_like(audio)
                for chunk in speech:
                    masked[chunk['start']:chunk['end']] = audio[chunk['start']:chunk['end']]
                audio = masked
            buffer = io.BytesIO()
            with wave.open(buffer, 'wb') as wav:
                wav.setnchannels(1)
                wav.setsampwidth(2)
                wav.setframerate(16000)
                wav.writeframes((np.clip(audio, -1, 1) * 32767).astype('<i2').tobytes())
            fields = {'response_format': 'verbose_json', 'language': language or 'auto',
                      'token_timestamps': 'false',
                      'beam_size': str(beam_size), 'prompt': initial_prompt or '',
                      'temperature': str(0.0 if temperature is None else temperature),
                      'temperature_inc': '0.2' if temperature is None else '0.0',
                      'no_language_probabilities': 'true'}
            boundary = secrets.token_hex(24)
            parts = []
            for key, value in fields.items():
                parts.append(f'--{boundary}\r\nContent-Disposition: form-data; '
                             f'name="{key}"\r\n\r\n{value}\r\n'.encode())
            parts.extend([f'--{boundary}\r\nContent-Disposition: form-data; name="file"; '
                          'filename="audio.wav"\r\nContent-Type: audio/wav\r\n\r\n'.encode(),
                          buffer.getvalue(), f'\r\n--{boundary}--\r\n'.encode()])
            request = urllib.request.Request(self.url + '/inference', data=b''.join(parts),
                headers={'Content-Type': f'multipart/form-data; boundary={boundary}'})
            try:
                with _http(request, timeout=600) as response:
                    result = json.load(response)
            except (urllib.error.URLError, OSError, ValueError) as exc:
                raise RuntimeError('Vulkan transcription failed; restart hushkey if the '
                                   'server is unresponsive') from exc
            if 'segments' not in result or not isinstance(result['segments'], list):
                raise RuntimeError('Invalid whisper.cpp response; use the version from setup_vulkan.py')
            segments = [SimpleNamespace(text=s['text'], start=s['start'], end=s['end'])
                        for s in result['segments']]
            detected = result.get('language', language)
            code = _LANGUAGES.get(str(detected).replace(' ', '_'), detected)
            return segments, SimpleNamespace(language=code, duration=duration)


def supervise(command):
    """Run until child exit or parent pipe EOF; never leave the native GPU child."""
    child = subprocess.Popen(command, stdin=subprocess.DEVNULL,
                             creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
    parent_gone = threading.Event()

    def watch_parent():
        sys.stdin.buffer.read()
        parent_gone.set()

    threading.Thread(target=watch_parent, daemon=True).start()
    try:
        while child.poll() is None and not parent_gone.wait(.1):
            pass
    finally:
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
    return child.returncode


if __name__ == '__main__':
    if len(sys.argv) > 2 and sys.argv[1] == '--supervise':
        sys.exit(supervise(sys.argv[2:]))
    sys.exit('Use setup_vulkan.py to install, then WHISPER_ENGINE=vulkan to run')
