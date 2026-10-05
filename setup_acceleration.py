#!/usr/bin/env python3
"""Install a verified prebuilt engine and persist a tested per-user backend."""
from __future__ import annotations

import hashlib
import ctypes
import importlib.util
import importlib.metadata
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import ssl
import stat
import subprocess
import sys
import tempfile
import urllib.request
import uuid
import zipfile

import acceleration

VERSION = 'v0.9.0'
RELEASE_URL = f'https://github.com/aignermax/hushkey/releases/download/{VERSION}'


class NoHardware(RuntimeError):
    """The engine ran but no supported hardware GPU is available."""


def server_name():
    return 'whisper-server.exe' if sys.platform == 'win32' else 'whisper-server'


def asset_name():
    system = {'win32': 'windows', 'linux': 'linux', 'darwin': 'macos'}.get(sys.platform)
    arch = {'amd64': 'x64', 'x86_64': 'x64', 'arm64': 'arm64', 'aarch64': 'arm64'}.get(platform.machine().lower())
    if not system or not arch:
        raise RuntimeError(f'Unsupported native platform: {sys.platform}/{platform.machine()}')
    return f'hushkey-engine-{system}-{arch}.zip'


def download(url, target):
    # Python.org macOS installs have no default CA bundle until the optional
    # Install Certificates.command is run. Use our dependency's trusted roots
    # directly, keeping certificate and hostname verification enabled.
    import certifi
    context = ssl.create_default_context(cafile=certifi.where())
    with urllib.request.urlopen(url, timeout=120, context=context) as source, open(target, 'wb') as dest:
        shutil.copyfileobj(source, dest)


def extract_archive(archive, destination):
    """Validate all entries first; never follow archive paths or links outside staging."""
    with zipfile.ZipFile(archive) as source:
        seen = set()
        for entry in source.infolist():
            path = PurePosixPath(entry.filename)
            mode = entry.external_attr >> 16
            if (path.is_absolute() or '..' in path.parts or '\\' in entry.filename
                    or ':' in entry.filename or stat.S_ISLNK(mode)
                    or entry.filename.casefold() in seen
                    or any(part.rstrip(' .') != part for part in path.parts)):
                raise ValueError(f'Unsafe native archive member: {entry.filename}')
            seen.add(entry.filename.casefold())
        if sum(entry.file_size for entry in source.infolist()) > 2 * 1024**3:
            raise ValueError('Unsafe native archive size')
        source.extractall(destination)
        for entry in source.infolist():
            path = Path(destination) / entry.filename
            if path.is_file() and (entry.external_attr >> 16) & 0o111:
                path.chmod(path.stat().st_mode | 0o111)


def install_native(bundle_dir=None):
    bundle = Path(bundle_dir) if bundle_dir else Path(__file__).resolve().parent / 'native'
    asset = asset_name()
    root = acceleration.data_dir() / 'engines'
    root.mkdir(parents=True, exist_ok=True)
    # A fresh version/hash directory avoids replacing DLLs held by a running tray.
    with tempfile.TemporaryDirectory(prefix='.stage-', dir=root) as temporary:
        stage = Path(temporary)
        manifest_path = bundle / 'native-manifest.json'
        manifest = json.loads(manifest_path.read_text(encoding='utf-8')) if manifest_path.is_file() else None
        # Source-overlay updates can leave an older installer's native bundle
        # behind. Its manifest AND identically named archive belong together;
        # never combine an old bundle with a newly downloaded manifest.
        use_bundle = manifest is not None and manifest.get('version') == VERSION
        if not use_bundle:
            manifest_path = stage / 'native-manifest.json'
            download(f'{RELEASE_URL}/native-manifest.json', manifest_path)
            manifest = json.loads(manifest_path.read_text(encoding='utf-8'))
        checksum = manifest.get('assets', {}).get(asset)
        if manifest.get('version') != VERSION or not isinstance(checksum, str) or not re.fullmatch('[0-9a-fA-F]{64}', checksum):
            raise ValueError(f'Invalid {VERSION} native manifest for {asset}')
        archive = bundle / asset
        if not use_bundle or not archive.is_file():
            archive = stage / asset
            download(f'{RELEASE_URL}/{asset}', archive)
        actual = hashlib.sha256(archive.read_bytes()).hexdigest()
        if actual != checksum.lower():
            raise ValueError(f'Native archive checksum mismatch: {asset}')
        destination = root / f'{VERSION}-{asset.removesuffix(".zip")}-{actual[:16]}'
        # Extract even on repeat installs so corruption cannot silently survive.
        unpacked = stage / 'unpacked'
        extract_archive(archive, unpacked)
        executable = unpacked / 'bin' / server_name()
        if not executable.is_file():
            raise ValueError('Native archive is missing bin/' + server_name())
        executable.chmod(executable.stat().st_mode | 0o111)
        if destination.exists():
            for path in unpacked.rglob('*'):
                if path.is_file():
                    installed = destination / path.relative_to(unpacked)
                    if not installed.is_file() or hashlib.sha256(installed.read_bytes()).digest() != hashlib.sha256(path.read_bytes()).digest():
                        destination = destination.with_name(destination.name + '-' + uuid.uuid4().hex[:12])
                        break
        if not destination.exists():
            try:
                unpacked.rename(destination)
            except OSError:
                # Another installer may win the same version/hash name. Keep
                # this verified extraction under a unique versioned directory.
                if not destination.exists():
                    raise
                destination = destination.with_name(destination.name + '-' + uuid.uuid4().hex[:12])
                unpacked.rename(destination)
        return str(destination / 'bin' / server_name())


def ensure_cuda_libraries():
    try:
        present = all(importlib.util.find_spec(name) is not None
                      for name in ('nvidia.cublas', 'nvidia.cudnn'))
        # CTranslate2 requires cuBLAS 12 and cuDNN 9. Older cuDNN packages use
        # the same import names, so module presence alone cannot prove this.
        compatible = all(int(importlib.metadata.version(package).split('.')[0]) == major
                         for package, major in (('nvidia-cublas-cu12', 12),
                                                ('nvidia-cudnn-cu12', 9)))
        present = present and compatible
    except (ImportError, importlib.metadata.PackageNotFoundError, ValueError):
        present = False
    if not present:
        print('Installing CUDA runtime libraries for the detected NVIDIA GPU...')
        subprocess.run([sys.executable, '-m', 'pip', 'install', '-r',
                        str(Path(__file__).resolve().parent / 'requirements-gpu.txt')], check=True)


def probe_cuda():
    if sys.platform == 'darwin':
        return False
    # Device count uses the driver. Load cuBLAS/cuDNN only inside the worker,
    # after pip has repaired versions; preloading would lock old DLLs on Windows.
    import ctranslate2
    if not ctranslate2.get_cuda_device_count():
        return False
    ensure_cuda_libraries()
    # Download errors are not hardware unavailability: do not mask them.
    from faster_whisper.utils import download_model
    model_path = download_model('tiny')
    # Missing CUDA libraries can abort the interpreter; isolate the actual load
    # and inference so the installer can still select Vulkan on the same GPU.
    result = subprocess.run([sys.executable, str(Path(__file__).resolve()), '--probe-cuda', model_path],
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            text=True, errors='replace', timeout=180,
                            creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
    if result.returncode == 0:
        return True
    if re.search(r'out of memory|alloc.*fail', result.stdout, re.I):
        raise RuntimeError(f'CUDA memory exhausted during probe: {result.stdout}')
    print(f'CUDA is not usable ({result.stdout[-2000:]}); checking native GPU backend.', file=sys.stderr)
    return False


def _cuda_worker(model_path):
    from transcribe import preload_cuda_libs
    preload_cuda_libs()
    import numpy as np
    from faster_whisper import WhisperModel
    model = WhisperModel(model_path, device='cuda', compute_type='float16')
    segments, _ = model.transcribe(np.zeros(16000, dtype=np.float32), vad_filter=False, language='en', beam_size=1)
    list(segments)


def _is_software(name):
    return bool(re.search(r'llvmpipe|lavapipe|swiftshader|software rasterizer', name, re.I))


def metal_available():
    """Ask the system Metal framework, without Xcode or command-line tools.

    whisper.cpp can register its Metal backend even when this function returns
    nil, then fail later without a useful 'no devices' message.
    """
    metal = ctypes.CDLL('/System/Library/Frameworks/Metal.framework/Metal')
    create = metal.MTLCreateSystemDefaultDevice
    create.argtypes = []
    create.restype = ctypes.c_void_p
    device = create()
    if not device:
        return False
    # MTLCreateSystemDefaultDevice follows the Create ownership convention.
    objc = ctypes.CDLL('/usr/lib/libobjc.A.dylib')
    objc.sel_registerName.argtypes = [ctypes.c_char_p]
    objc.sel_registerName.restype = ctypes.c_void_p
    objc.objc_msgSend.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    objc.objc_msgSend.restype = None
    objc.objc_msgSend(device, objc.sel_registerName(b'release'))
    return True


def probe_native(executable, engine=None):
    from whisper_cpp import WhisperCppModel, resolve_model
    import numpy as np
    engine = engine or ('metal' if sys.platform == 'darwin' else 'vulkan')
    if engine == 'metal' and not metal_available():
        raise NoHardware('The system Metal framework reports no hardware device')
    # Complete downloading separately, before classifying a startup error.
    model_path = resolve_model('tiny')
    gpu = int(os.environ.get('WHISPER_CPP_DEVICE', '0'))
    model = None
    try:
        model = WhisperCppModel(model_path, executable=executable, gpu=gpu, engine=engine)
        devices = model.gpu_devices
        if 'WHISPER_CPP_DEVICE' not in os.environ:
            candidates = [index for index, info in devices.items() if index >= 0 and not _is_software(info['name'])]
            if candidates:
                preferred = min(candidates, key=lambda index: (devices[index]['integrated'], index))
                if preferred != gpu:
                    model.close()
                    gpu = preferred
                    model = WhisperCppModel(model_path, executable=executable, gpu=gpu, engine=engine)
        info = devices.get(gpu, {})
        if _is_software(info.get('name', '')):
            raise NoHardware('Only software Vulkan is available')
        model.transcribe(np.zeros(16000, dtype=np.float32), language='en', vad_filter=False, beam_size=1)
        return {'engine': engine, 'device': gpu, 'server': executable, 'backend': model.device}
    except RuntimeError as exc:
        message = str(exc)
        # Memory, missing DLLs and broken engine builds are installation failures.
        if re.search(r'out of memory|alloc.*fail|timed out', message, re.I):
            raise
        if re.search(r'no GPU found|No devices found|no device found|ErrorIncompatibleDriver|Vulkan 1\.2 required', message, re.I):
            raise NoHardware(message) from exc
        raise
    finally:
        if model is not None:
            model.close()


def select_backend(executable):
    if probe_cuda():
        return {'engine': 'faster-whisper', 'device': 'cuda'}
    try:
        return probe_native(executable)
    except NoHardware as exc:
        print(f'No supported hardware GPU; using CPU. Details: {exc}', file=sys.stderr)
        return {'engine': 'faster-whisper', 'device': 'cpu'}


def main():
    try:
        # Always provision native files so later environment overrides also work.
        executable = install_native()
        override = os.environ.get('WHISPER_CPP_SERVER')
        if override:
            executable = shutil.which(override) or override
        explicit = os.environ.get('WHISPER_ENGINE', '').strip().lower()
        if explicit in ('vulkan', 'metal'):
            settings = probe_native(executable, explicit)
        elif explicit == 'faster-whisper':
            settings = {'engine': explicit, 'device': 'cuda' if probe_cuda() else 'cpu'}
        elif explicit:
            raise ValueError('WHISPER_ENGINE must be faster-whisper, vulkan or metal')
        else:
            settings = select_backend(executable)
        settings['version'] = VERSION
        settings.setdefault('server', executable)
        acceleration.save_settings(settings)
        print(f'Acceleration configured: {settings["engine"]} / {settings["device"]}')
        return 0
    except Exception as exc:
        print(f'Acceleration setup failed: {exc}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    if len(sys.argv) == 3 and sys.argv[1] == '--probe-cuda':
        _cuda_worker(sys.argv[2])
    else:
        sys.exit(main())
