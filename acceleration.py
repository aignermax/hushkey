"""Per-user acceleration selection, shared by tray, services and CLI."""
import json
import os
from pathlib import Path
import sys
import tempfile


def data_dir():
    if sys.platform == 'win32':
        base = Path(os.environ.get('LOCALAPPDATA', Path.home() / 'AppData/Local'))
    elif sys.platform == 'darwin':
        base = Path.home() / 'Library/Application Support'
    else:
        base = Path(os.environ.get('XDG_DATA_HOME', Path.home() / '.local/share'))
    return base / 'hushkey'


def load_settings():
    path = data_dir() / 'acceleration.json'
    try:
        settings = json.loads(path.read_text(encoding='utf-8'))
        if not isinstance(settings, dict) or settings.get('engine') not in ('faster-whisper', 'vulkan', 'metal'):
            raise ValueError('invalid engine settings')
        device = settings.get('device')
        if settings['engine'] == 'faster-whisper':
            if device not in ('cpu', 'cuda'):
                raise ValueError('invalid faster-whisper device')
        elif type(device) is not int or device < 0:
            raise ValueError('invalid native GPU index')
        if 'server' in settings and (not isinstance(settings['server'], str) or not settings['server']):
            raise ValueError('invalid native server path')
        return settings
    except FileNotFoundError:
        return {}
    except (ValueError, OSError) as exc:
        raise RuntimeError(f'Cannot read {path}: {exc}; run python setup_acceleration.py to repair') from exc


def save_settings(settings):
    folder = data_dir()
    folder.mkdir(parents=True, exist_ok=True)
    # Atomic replace prevents a concurrently starting tray from reading half JSON.
    with tempfile.NamedTemporaryFile('w', encoding='utf-8', dir=folder, delete=False) as stream:
        temporary = Path(stream.name)
        json.dump(settings, stream, indent=2)
        stream.flush()
        os.fsync(stream.fileno())
    try:
        os.replace(temporary, folder / 'acceleration.json')
    finally:
        temporary.unlink(missing_ok=True)


def saved_device():
    # Explicit engine choice retains its historical auto-detection behavior.
    return None if 'WHISPER_ENGINE' in os.environ else load_settings().get('device')
