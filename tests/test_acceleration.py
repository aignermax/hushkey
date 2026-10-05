import hashlib
import json
from pathlib import Path
import zipfile

import pytest


def test_settings_survive_restart_and_environment_wins(tmp_path, monkeypatch):
    import acceleration as a
    import whisper_cpp
    monkeypatch.setattr(a, 'data_dir', lambda: tmp_path)
    monkeypatch.delenv('WHISPER_ENGINE', raising=False)
    a.save_settings({'engine': 'metal', 'device': 0, 'server': '/native/server'})
    assert whisper_cpp.requested_engine() == 'metal'
    assert whisper_cpp.server_path() == '/native/server'
    monkeypatch.setenv('WHISPER_ENGINE', 'faster-whisper')
    assert whisper_cpp.requested_engine() == 'faster-whisper'


@pytest.mark.parametrize('member', ['../escape', '/absolute', 'C:/escape', 'bin/../../escape', 'bin\\..\\escape'])
def test_archive_rejects_unsafe_paths(tmp_path, member):
    import setup_acceleration as s
    archive = tmp_path / 'asset.zip'
    with zipfile.ZipFile(archive, 'w') as z:
        z.writestr(member, b'bad')
    with pytest.raises(ValueError, match='Unsafe'):
        s.extract_archive(archive, tmp_path / 'output')


def test_bundle_checksum_and_version_validation(tmp_path, monkeypatch):
    import setup_acceleration as s
    import acceleration as a
    monkeypatch.setattr(a, 'data_dir', lambda: tmp_path / 'data')
    asset = s.asset_name()
    archive = tmp_path / asset
    with zipfile.ZipFile(archive, 'w') as z:
        z.writestr('bin/' + s.server_name(), b'native')
    manifest = {'version': s.VERSION, 'assets': {asset: hashlib.sha256(archive.read_bytes()).hexdigest()}}
    (tmp_path / 'native-manifest.json').write_text(json.dumps(manifest))
    executable = s.install_native(tmp_path)
    assert Path(executable).read_bytes() == b'native'
    archive.write_bytes(b'corrupt')
    with pytest.raises(ValueError, match='checksum'):
        s.install_native(tmp_path)
    assert Path(executable).read_bytes() == b'native'


def test_auto_selection_and_unsupported_fallback(monkeypatch):
    import setup_acceleration as s
    monkeypatch.setattr(s, 'probe_cuda', lambda: False)
    monkeypatch.setattr(s, 'probe_native', lambda *a: {'engine': 'vulkan', 'device': 1})
    assert s.select_backend('/server')['device'] == 1
    def unsupported(*args):
        raise s.NoHardware('no GPU found')
    monkeypatch.setattr(s, 'probe_native', unsupported)
    assert s.select_backend('/server')['device'] == 'cpu'
    def broken(*args):
        raise RuntimeError('out of memory')
    monkeypatch.setattr(s, 'probe_native', broken)
    with pytest.raises(RuntimeError, match='out of memory'):
        s.select_backend('/server')


def test_cuda_is_preferred_only_after_successful_probe(monkeypatch):
    import setup_acceleration as s
    monkeypatch.setattr(s, 'probe_cuda', lambda: True)
    monkeypatch.setattr(s, 'probe_native', lambda *a: pytest.fail('unneeded native probe'))
    assert s.select_backend('/server') == {'engine': 'faster-whisper', 'device': 'cuda'}


def test_metal_backend_proof():
    from whisper_cpp import native_device
    assert native_device('whisper_backend_init_gpu: using Metal backend') == 'Metal'
    assert native_device('whisper_backend_init_gpu: using MTL0 backend') == 'MTL0'
    assert native_device('ggml_metal: device Apple M2') is None


def test_settings_invalid_json_has_actionable_error(tmp_path, monkeypatch):
    import acceleration as a
    monkeypatch.setattr(a, 'data_dir', lambda: tmp_path)
    (tmp_path / 'acceleration.json').write_text('{broken')
    with pytest.raises(RuntimeError, match='setup_acceleration'):
        a.load_settings()


def test_failed_setup_does_not_overwrite_existing_settings(tmp_path, monkeypatch):
    import acceleration as a
    import setup_acceleration as s
    monkeypatch.setattr(a, 'data_dir', lambda: tmp_path)
    a.save_settings({'engine': 'faster-whisper', 'device': 'cpu'})
    before = (tmp_path / 'acceleration.json').read_bytes()
    monkeypatch.setattr(s, 'install_native', lambda: (_ for _ in ()).throw(ValueError('checksum')))
    assert s.main() == 1
    assert (tmp_path / 'acceleration.json').read_bytes() == before


def test_probe_prefers_dedicated_and_runs_inference(monkeypatch):
    import whisper_cpp
    import setup_acceleration as s
    monkeypatch.delenv('WHISPER_CPP_DEVICE', raising=False)
    monkeypatch.setattr(whisper_cpp, 'resolve_model', lambda _: 'tiny.bin')
    calls = []
    class Model:
        device = 'Vulkan1'
        gpu_devices = {0: {'name': 'Integrated', 'integrated': True},
                       1: {'name': 'Dedicated', 'integrated': False}}
        def __init__(self, *args, **kwargs):
            calls.append(kwargs['gpu'])
        def close(self):
            calls.append('close')
        def transcribe(self, *args, **kwargs):
            calls.append('inference')
    monkeypatch.setattr(whisper_cpp, 'WhisperCppModel', Model)
    assert s.probe_native('/server', 'vulkan')['device'] == 1
    assert calls == [0, 'close', 1, 'inference', 'close']


def test_cuda_process_crash_falls_through_to_native(monkeypatch):
    import setup_acceleration as s
    import ctranslate2
    import faster_whisper.utils
    from types import SimpleNamespace
    monkeypatch.setattr(ctranslate2, 'get_cuda_device_count', lambda: 1)
    monkeypatch.setattr(faster_whisper.utils, 'download_model', lambda _: '/tiny')
    monkeypatch.setattr(s, 'ensure_cuda_libraries', lambda: None)
    monkeypatch.setattr(s.subprocess, 'run', lambda *a, **kw: SimpleNamespace(returncode=-6, stdout='missing cudnn'))
    assert s.probe_cuda() is False


def test_cuda_dependencies_installed_in_active_venv_only_when_missing(monkeypatch):
    import setup_acceleration as s
    calls = []
    monkeypatch.setattr(s.importlib.util, 'find_spec', lambda _: None)
    monkeypatch.setattr(s.importlib.metadata, 'version', lambda name: '8.9.0' if 'cudnn' in name else '12.5.0')
    monkeypatch.setattr(s.subprocess, 'run', lambda command, **kwargs: calls.append((command, kwargs)))
    s.ensure_cuda_libraries()
    assert calls[0][0][:4] == [s.sys.executable, '-m', 'pip', 'install']
    assert calls[0][1]['check'] is True
    monkeypatch.setattr(s.importlib.util, 'find_spec', lambda _: object())
    monkeypatch.setattr(s.importlib.metadata, 'version', lambda name: '9.5.0' if 'cudnn' in name else '12.5.0')
    s.ensure_cuda_libraries()
    assert len(calls) == 1


@pytest.mark.parametrize('cudnn,cublas', [('8.9.0', '12.5.0'), ('10.0.0', '12.5.0'), ('9.5.0', '13.0.0')])
def test_cuda_incompatible_installed_version_is_repaired(monkeypatch, cudnn, cublas):
    import setup_acceleration as s
    monkeypatch.setattr(s.importlib.util, 'find_spec', lambda _: object())
    monkeypatch.setattr(s.importlib.metadata, 'version', lambda name: cudnn if 'cudnn' in name else cublas)
    calls = []
    monkeypatch.setattr(s.subprocess, 'run', lambda *args, **kwargs: calls.append(args))
    s.ensure_cuda_libraries()
    assert len(calls) == 1


def test_setup_resolves_explicit_server_on_path(monkeypatch):
    import setup_acceleration as s
    monkeypatch.setenv('WHISPER_ENGINE', 'vulkan')
    monkeypatch.setenv('WHISPER_CPP_SERVER', 'whisper-server')
    monkeypatch.setattr(s, 'install_native', lambda: '/bundled/server')
    monkeypatch.setattr(s.shutil, 'which', lambda command: '/path/server' if command == 'whisper-server' else None)
    probed = []
    def probe(server, engine):
        probed.append(server)
        return {'server': server, 'engine': engine, 'device': 0}
    monkeypatch.setattr(s, 'probe_native', probe)
    saved = []
    monkeypatch.setattr(s.acceleration, 'save_settings', saved.append)
    assert s.main() == 0
    assert probed == ['/path/server']
    assert saved[0]['server'] == '/path/server'


@pytest.mark.parametrize('settings', [
    {'engine': 'vulkan', 'device': -1}, {'engine': 'metal', 'device': 'cpu'},
    {'engine': 'faster-whisper', 'device': 'typo'}, {'engine': 'vulkan', 'device': True},
    {'engine': 'vulkan', 'device': 0, 'server': 123},
])
def test_invalid_settings_rejected(settings, tmp_path, monkeypatch):
    import acceleration as a
    monkeypatch.setattr(a, 'data_dir', lambda: tmp_path)
    (tmp_path / 'acceleration.json').write_text(json.dumps(settings))
    with pytest.raises(RuntimeError, match='setup_acceleration'):
        a.load_settings()


def test_corrupt_installed_engine_repairs_to_fresh_directory(tmp_path, monkeypatch):
    import setup_acceleration as s
    import acceleration as a
    monkeypatch.setattr(a, 'data_dir', lambda: tmp_path / 'data')
    archive = tmp_path / s.asset_name()
    with zipfile.ZipFile(archive, 'w') as z:
        z.writestr('bin/' + s.server_name(), b'native')
    (tmp_path / 'native-manifest.json').write_text(json.dumps({
        'version': s.VERSION, 'assets': {s.asset_name(): hashlib.sha256(archive.read_bytes()).hexdigest()}}))
    first = Path(s.install_native(tmp_path))
    first.write_bytes(b'corrupted')
    second = Path(s.install_native(tmp_path))
    assert second != first and second.read_bytes() == b'native'


@pytest.mark.parametrize('message,exception', [
    ('whisper.cpp did not initialize a hardware GPU\nwhisper_backend_init_gpu: no GPU found', 'NoHardware'),
    ('whisper-server exited during startup; missing DLL', 'RuntimeError'),
    ('Vulkan ErrorIncompatibleDriver', 'NoHardware'),
    ('ggml_vulkan: Vulkan 1.2 required.\nErrorFeatureNotPresent', 'NoHardware'),
    ('shader pipeline creation: ErrorFeatureNotPresent', 'RuntimeError'),
    ('device memory allocation failed; no GPU found', 'RuntimeError'),
])
def test_probe_distinguishes_hardware_from_broken_install(monkeypatch, message, exception):
    import whisper_cpp
    import setup_acceleration as s
    monkeypatch.setattr(whisper_cpp, 'resolve_model', lambda _: 'tiny.bin')
    def fail(*args, **kwargs):
        raise RuntimeError(message)
    monkeypatch.setattr(whisper_cpp, 'WhisperCppModel', fail)
    with pytest.raises(RuntimeError) as error:
        s.probe_native('/server', 'vulkan')
    assert type(error.value).__name__ == exception


def test_setup_rejects_software_vulkan(monkeypatch):
    import whisper_cpp
    import setup_acceleration as s
    from types import SimpleNamespace
    monkeypatch.delenv('WHISPER_CPP_DEVICE', raising=False)
    monkeypatch.setattr(whisper_cpp, 'resolve_model', lambda _: 'tiny.bin')
    model = SimpleNamespace(gpu_devices={0: {'name': 'llvmpipe', 'integrated': False}},
                            close=lambda: None)
    monkeypatch.setattr(whisper_cpp, 'WhisperCppModel', lambda *a, **kw: model)
    with pytest.raises(s.NoHardware, match='software'):
        s.probe_native('/server', 'vulkan')


def test_metal_without_system_device_is_cpu_before_model_download(monkeypatch):
    import setup_acceleration as s
    import whisper_cpp
    monkeypatch.setattr(s, 'metal_available', lambda: False)
    monkeypatch.setattr(whisper_cpp, 'resolve_model', lambda _: pytest.fail('unneeded model download'))
    with pytest.raises(s.NoHardware, match='Metal'):
        s.probe_native('/server', 'metal')


@pytest.mark.parametrize('device', [None, 123])
def test_metal_preflight_calls_system_framework_without_developer_tools(monkeypatch, device):
    import setup_acceleration as s
    from types import SimpleNamespace
    class Function:
        def __init__(self, result):
            self.result = result
        def __call__(self, *args):
            return self.result
    create = Function(device)
    releases = []
    def release(*args):
        releases.append(args)
    metal = SimpleNamespace(MTLCreateSystemDefaultDevice=create)
    objc = SimpleNamespace(sel_registerName=Function(456), objc_msgSend=release)
    monkeypatch.setattr(s.ctypes, 'CDLL', lambda path: metal if '/Metal.framework/' in path else objc)
    assert s.metal_available() is bool(device)
    assert len(releases) == bool(device)
