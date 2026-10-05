#!/usr/bin/env python3
"""Build the optional Vulkan engine. Requires Git, CMake, a C++ compiler and
Vulkan development tools (see README). Never changes drivers or system packages.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

from whisper_cpp import runtime_dir

VERSION = 'v1.9.4'
COMMIT = '927cfce34f31707e17f2bff35c349632fb9e2c3a'
REPOSITORY = 'https://github.com/ggml-org/whisper.cpp.git'


def windows_toolchain(cmake, generator):
    generator = generator or os.environ.get('CMAKE_GENERATOR')
    vswhere = Path(os.environ.get('ProgramFiles(x86)', 'C:/Program Files (x86)')) / \
        'Microsoft Visual Studio/Installer/vswhere.exe'
    if vswhere.is_file():
        installs = json.loads(subprocess.check_output(
            [str(vswhere), '-latest', '-products', '*', '-requires',
             'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-format', 'json', '-utf8'],
            text=True, encoding='utf-8'))
        if installs:
            version = installs[0]['installationVersion'].split('.')[0]
            generator = generator or {'17': 'Visual Studio 17 2022',
                                      '18': 'Visual Studio 18 2026'}.get(version)
            bundled = Path(installs[0]['installationPath']) / \
                'Common7/IDE/CommonExtensions/Microsoft/CMake/CMake/bin/cmake.exe'
            if cmake == 'cmake' and bundled.is_file():
                cmake = str(bundled)
    return cmake, generator


def build_commands(cmake, source, build, generator, jobs):
    configure = [cmake, '-S', str(source), '-B', str(build),
                 '-DCMAKE_BUILD_TYPE=Release', '-DGGML_VULKAN=ON',
                 '-DBUILD_SHARED_LIBS=OFF', '-DGGML_NATIVE=OFF',
                 '-DWHISPER_BUILD_TESTS=OFF', '-DWHISPER_BUILD_SERVER=ON']
    if generator:
        configure += ['-G', generator]
    if sys.platform == 'win32':
        # Upstream uses main(char**), then interprets model filenames as UTF-8.
        # Ask Windows 10 1903+ to supply UTF-8 argv, including Unicode user homes.
        build.mkdir(parents=True, exist_ok=True)
        manifest = build / 'hushkey-utf8.manifest'
        manifest.write_text('''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <application xmlns="urn:schemas-microsoft-com:asm.v3"><windowsSettings>
    <activeCodePage xmlns="http://schemas.microsoft.com/SMI/2019/WindowsSettings">UTF-8</activeCodePage>
  </windowsSettings></application>
</assembly>
''', encoding='utf-8')
        configure += [f'-DCMAKE_EXE_LINKER_FLAGS=/MANIFEST:EMBED /MANIFESTINPUT:"{manifest}"']
    build_cmd = [cmake, '--build', str(build), '--config', 'Release',
                 '--target', 'whisper-server', '--parallel', str(jobs)]
    return configure, build_cmd


def install_binary(build, destination):
    candidates = [build / 'bin/Release/whisper-server.exe',
                  build / 'bin/whisper-server.exe', build / 'bin/whisper-server']
    binary = next((p for p in candidates if p.is_file()), None)
    if binary is None:
        raise RuntimeError('Build did not produce whisper-server')
    target = destination / 'bin' / binary.name
    target.parent.mkdir(parents=True, exist_ok=True)
    temporary = target.with_suffix(target.suffix + '.tmp')
    shutil.copy2(binary, temporary)
    os.replace(temporary, target)
    return target


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prefix', type=Path, default=runtime_dir(),
                        help='install directory (default: per-user hushkey/vulkan)')
    parser.add_argument('--cmake', default='cmake', help='CMake executable')
    parser.add_argument('--generator', default=None, help='CMake generator, e.g. Ninja')
    parser.add_argument('--jobs', type=int, default=min(os.cpu_count() or 2, 8))
    args = parser.parse_args(argv)
    if sys.platform not in ('win32', 'linux'):
        parser.error('This Vulkan setup supports Windows and Linux')
    if args.jobs < 1:
        parser.error('--jobs must be positive')
    if sys.platform == 'win32':
        args.cmake, args.generator = windows_toolchain(args.cmake, args.generator)
    for tool in ('git', args.cmake):
        if not shutil.which(tool):
            parser.error(f'{tool} missing; install the build prerequisites in README')
    prefix = args.prefix.expanduser().resolve()
    source = prefix / f'source-{VERSION}'
    build = prefix / f'build-{VERSION}'
    prefix.mkdir(parents=True, exist_ok=True)
    if not source.exists():
        subprocess.run(['git', 'clone', '--depth', '1', '--branch', VERSION,
                        REPOSITORY, str(source)], check=True)
    actual = subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD'],
                                     text=True).strip()
    dirty = subprocess.check_output(['git', '-C', str(source), 'status', '--porcelain'],
                                    text=True).strip()
    if actual != COMMIT or dirty:
        raise RuntimeError(f'{source} must be an unmodified checkout of {COMMIT}; '
                           'use a fresh --prefix')
    for command in build_commands(args.cmake, source, build, args.generator, args.jobs):
        subprocess.run(command, check=True)
    binary = install_binary(build, prefix)
    print(f'Installed {binary}\nEnable with WHISPER_ENGINE=vulkan and restart hushkey.')
    if prefix != runtime_dir().resolve():
        print(f'Custom prefix: also set WHISPER_CPP_SERVER={binary}')
    print('First use downloads the selected GGML model; Vulkan GPU initialization '
          'is verified when hushkey loads it.')
    return 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError) as exc:
        sys.exit(f'Vulkan setup failed: {exc}')
