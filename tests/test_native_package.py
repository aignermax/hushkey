import hashlib
import importlib.util
import json
from pathlib import Path
import zipfile

import pytest

ROOT = Path(__file__).resolve().parents[1]


def helper():
    spec = importlib.util.spec_from_file_location('native_package', ROOT / 'packaging/native.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_package_includes_binary_license_and_checksum(tmp_path):
    prefix = tmp_path / 'prefix'
    (prefix / 'bin').mkdir(parents=True)
    (prefix / 'bin/whisper-server').write_bytes(b'ELF fake')
    (prefix / 'source-v1.9.4').mkdir()
    (prefix / 'source-v1.9.4/LICENSE').write_text('MIT license')
    native = helper()
    archive = native.package(prefix, 'linux', 'x64', tmp_path / 'out')
    with zipfile.ZipFile(archive) as bundle:
        assert bundle.read('bin/whisper-server') == b'ELF fake'
        assert bundle.read('LICENSE-whisper.cpp.txt') == b'MIT license'
    native.manifest(tmp_path / 'out', 'v0.9.0')
    data = json.loads((tmp_path / 'out/native-manifest.json').read_text())
    assert data['assets'][archive.name] == hashlib.sha256(archive.read_bytes()).hexdigest()


def test_missing_engine_cannot_be_published(tmp_path):
    native = helper()
    with pytest.raises(FileNotFoundError):
        native.package(tmp_path, 'linux', 'x64', tmp_path / 'out')


def test_unknown_platform_is_rejected(tmp_path):
    with pytest.raises(ValueError):
        helper().package(tmp_path, 'unknown', 'x64', tmp_path / 'out')
