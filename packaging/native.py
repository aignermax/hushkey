"""Package portable release engines and their integrity manifest (CI only)."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import zipfile


def package(prefix, system, arch, output):
    if system not in ('windows', 'linux', 'macos') or arch not in ('x64', 'arm64'):
        raise ValueError('Unsupported native platform')
    binary = prefix / 'bin' / ('whisper-server.exe' if system == 'windows' else 'whisper-server')
    license_file = prefix / 'source-v1.9.4/LICENSE'
    files = [(binary, 'bin/' + binary.name), (license_file, 'LICENSE-whisper.cpp.txt')]
    if system == 'windows':
        # The SDK installer supplies the redistributable Khronos loader. Drivers
        # still provide the device ICD; no Vulkan SDK is needed on the target.
        loader = Path(os.environ.get('WINDIR', 'C:/Windows')) / 'System32/vulkan-1.dll'
        files.append((loader, 'bin/vulkan-1.dll'))
        sdk = Path(os.environ['VULKAN_SDK'])
        for path in sorted((sdk / 'Licenses').rglob('*')):
            if path.is_file():
                files.append((path, 'licenses-vulkan/' + path.relative_to(sdk / 'Licenses').as_posix()))
    for path, _ in files:
        if not path.is_file():
            raise FileNotFoundError(path)
    output.mkdir(parents=True, exist_ok=True)
    archive = output / f'hushkey-engine-{system}-{arch}.zip'
    with zipfile.ZipFile(archive, 'w', compression=zipfile.ZIP_DEFLATED) as bundle:
        for source, name in files:
            bundle.write(source, name)
    return archive


def manifest(directory, version):
    assets = {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
              for path in sorted(directory.glob('hushkey-engine-*.zip'))}
    if not assets:
        raise ValueError('No native archives to publish')
    (directory / 'native-manifest.json').write_text(
        json.dumps({'version': version, 'assets': assets}, indent=2) + '\n', encoding='utf-8')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prefix', type=Path)
    parser.add_argument('--system')
    parser.add_argument('--arch')
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--manifest', metavar='VERSION')
    args = parser.parse_args()
    if args.manifest:
        manifest(args.out, args.manifest)
    else:
        if not all((args.prefix, args.system, args.arch)):
            parser.error('--prefix, --system and --arch are required for packaging')
        print(package(args.prefix, args.system, args.arch, args.out))


if __name__ == '__main__':
    main()
