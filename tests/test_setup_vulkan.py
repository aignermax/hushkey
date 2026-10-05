from pathlib import Path

import pytest


def test_build_commands_pin_vulkan_and_static_runtime(tmp_path):
    from setup_vulkan import build_commands
    configure, build = build_commands('cmake', tmp_path / 'source', tmp_path / 'build', None, 2)
    assert '-DGGML_VULKAN=ON' in configure
    assert '-DBUILD_SHARED_LIBS=OFF' in configure
    assert '-DGGML_NATIVE=OFF' in configure
    assert build[-4:] == ['--target', 'whisper-server', '--parallel', '2']


def test_setup_does_not_publish_missing_binary(tmp_path):
    from setup_vulkan import install_binary
    with pytest.raises(RuntimeError, match='whisper-server'):
        install_binary(tmp_path / 'missing', tmp_path / 'install')
    assert not (tmp_path / 'install/bin').exists()


def test_setup_installs_both_platform_layouts(tmp_path):
    from setup_vulkan import install_binary
    for subdir, name in [('bin', 'whisper-server'), ('bin/Release', 'whisper-server.exe')]:
        build = tmp_path / name
        binary = build / subdir / name
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b'native')
        binary.chmod(0o755)
        installed = install_binary(build, tmp_path / ('installed-' + name))
        assert installed.read_bytes() == b'native'


def test_both_installers_ship_vulkan_modules():
    root = Path(__file__).resolve().parents[1]
    for file in ['packaging/windows/hushkey.iss', 'packaging/linux/build-deb.sh']:
        source = (root / file).read_text(encoding='utf-8')
        for module in ['whisper_cpp.py', 'setup_vulkan.py']:
            assert module in source


def test_windows_generator_matches_installed_visual_studio(tmp_path, monkeypatch):
    import setup_vulkan
    import json
    tools = tmp_path / 'Microsoft Visual Studio/Installer'
    tools.mkdir(parents=True)
    (tools / 'vswhere.exe').write_bytes(b'fake')
    monkeypatch.setenv('ProgramFiles(x86)', str(tmp_path))
    monkeypatch.delenv('CMAKE_GENERATOR', raising=False)
    monkeypatch.setattr(setup_vulkan.subprocess, 'check_output', lambda *a, **kw:
        json.dumps([{'installationVersion': '18.0.1', 'installationPath': str(tmp_path / 'VS')}]))
    cmake, generator = setup_vulkan.windows_toolchain('cmake', None)
    assert generator == 'Visual Studio 18 2026'


def test_windows_build_embeds_utf8_manifest(tmp_path, monkeypatch):
    import setup_vulkan
    monkeypatch.setattr(setup_vulkan.sys, 'platform', 'win32')
    configure, _ = setup_vulkan.build_commands('cmake', tmp_path / 's', tmp_path / 'b', None, 2)
    assert any('/MANIFESTINPUT:' in arg for arg in configure)
    assert 'UTF-8' in (tmp_path / 'b/hushkey-utf8.manifest').read_text()
