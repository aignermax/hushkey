"""Provisional text must never alter committed dictation or outlive its hold."""
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time
from types import SimpleNamespace

import numpy as np
import pytest

import dictate
import tray


@pytest.fixture
def preview(monkeypatch, tmp_path):
    monkeypatch.setattr(dictate, 'STATE_DIR', str(tmp_path))
    monkeypatch.setattr(dictate, 'STATE_PATH', str(tmp_path / 'state.json'))
    monkeypatch.setattr(dictate, 'LOG_PATH', str(tmp_path / 'dictate.log'))
    monkeypatch.setattr(dictate, 'CONFIG_PATH', str(tmp_path / 'config.json'))
    monkeypatch.setenv('PTT_BACKEND', 'pynput')
    wav = tmp_path / 'snapshot.wav'
    wav.write_bytes(b'snapshot')
    monkeypatch.setattr(dictate, 'pick_recorder', lambda: SimpleNamespace(
        snapshot_wav=lambda: str(wav)))
    monkeypatch.setattr(dictate, '_decode_wav_16k', lambda _: np.ones(120 * 16000))
    daemon = dictate.DictationDaemon()
    daemon.recording = 1.0
    session = dictate._StreamSession()
    daemon._preview = session
    return daemon, session, wav


def test_preview_only_publishes_recent_text(preview, monkeypatch):
    daemon, session, wav = preview
    seen = []
    def transcribe(audio, lang):
        seen.append(len(audio))
        return [SimpleNamespace(text='English preview. 中文。')]
    monkeypatch.setattr(daemon, '_transcribe', transcribe)
    daemon.injector = SimpleNamespace(insert=lambda *_: pytest.fail('preview typed'))
    daemon._preview_tick(session)
    state = json.loads(open(dictate.STATE_PATH, encoding='utf-8').read())
    assert state['state'] == 'recording'
    assert state['preview'] == 'English preview. 中文。'
    assert seen == [30 * 16000]
    assert session.committed_end == 0
    assert not wav.exists()


def test_late_preview_cannot_overwrite_released_or_new_recording(preview, monkeypatch):
    daemon, session, wav = preview
    def transcribe(*_):
        session.stop.set()
        daemon._preview = dictate._StreamSession()
        dictate.write_state('recording')
        return [SimpleNamespace(text='stale')]
    monkeypatch.setattr(daemon, '_transcribe', transcribe)
    daemon._preview_tick(session)
    state = json.loads(open(dictate.STATE_PATH, encoding='utf-8').read())
    assert 'preview' not in state
    assert not wav.exists()


def test_preview_skips_when_final_inference_or_insertion_busy(preview, monkeypatch):
    daemon, session, wav = preview
    monkeypatch.setattr(daemon, '_transcribe', lambda *_: pytest.fail('must skip'))
    with daemon.busy_lock:
        daemon._preview_tick(session)
    assert wav.exists()  # no snapshot was taken


def test_preview_exception_cleans_snapshot_and_releases_lock(preview, monkeypatch):
    daemon, session, wav = preview
    monkeypatch.setattr(daemon, '_transcribe', lambda *_: (_ for _ in ()).throw(ValueError('decode')))
    with pytest.raises(ValueError):
        daemon._preview_tick(session)
    assert not wav.exists()
    assert daemon.busy_lock.acquire(blocking=False)
    daemon.busy_lock.release()


def test_idle_clears_preview(tmp_path, monkeypatch):
    monkeypatch.setattr(dictate, 'STATE_DIR', str(tmp_path))
    monkeypatch.setattr(dictate, 'STATE_PATH', str(tmp_path / 'state.json'))
    dictate.write_state('recording', preview='temporary words')
    dictate.write_state('idle')
    assert 'preview' not in json.loads((tmp_path / 'state.json').read_text())


def test_overlay_displays_bounded_preview_only_while_recording():
    assert tray.pill_for('recording', 'hello')[1] == 'hello'
    assert len(tray.pill_for('recording', 'x' * 2000)[1]) <= 501
    assert tray.pill_for('transcribing', 'stale')[1] == tray.S['transcribing']
    assert tray.pill_for('idle', 'stale') is None


@pytest.mark.parametrize('platform', ['win32', 'linux', 'darwin'])
def test_overlay_enabled_on_all_platforms(monkeypatch, platform):
    monkeypatch.setattr(tray.sys, 'platform', platform)
    monkeypatch.delenv('PTT_OVERLAY', raising=False)
    assert tray.overlay_wanted()
    monkeypatch.setenv('PTT_OVERLAY', '0')
    assert not tray.overlay_wanted()


def test_overlay_has_own_process_and_parent_lifetime(monkeypatch):
    calls = []
    child = SimpleNamespace(stdin=SimpleNamespace(close=lambda: calls.append('closed')))
    monkeypatch.setattr(tray.subprocess, 'Popen', lambda *a, **k: (calls.append((a, k)) or child))
    overlay = tray.RecordingOverlay()
    overlay.start()
    assert '--overlay' in calls[0][0][0]
    assert calls[0][1]['stdin'] == tray.subprocess.PIPE
    overlay.stop()
    assert calls[-1] == 'closed'


def test_release_during_preview_clears_text_and_inserts_full_recording_once(preview, monkeypatch):
    daemon, _, snapshot = preview
    original = snapshot.with_name('full.wav')
    original.write_bytes(b'full recording' * 200)
    monkeypatch.setattr(dictate, 'PREVIEW', True)
    monkeypatch.setattr(dictate, 'PREVIEW_INTERVAL', 60)
    monkeypatch.setattr(dictate, 'STREAMING', False)
    monkeypatch.setattr(dictate, 'PTT_KEY', 'ctrl_r')
    monkeypatch.setattr(dictate, 'notify', lambda *_: None)
    daemon.recorder.start = lambda: None
    daemon.recorder.stop = lambda: str(original)
    entered, finish, inserted = threading.Event(), threading.Event(), threading.Event()
    completed = threading.Event()
    write_state = dictate.write_state
    def publish(state, **kwargs):
        write_state(state, **kwargs)
        if state == 'idle':
            completed.set()
    monkeypatch.setattr(dictate, 'write_state', publish)
    inputs, outputs = [], []
    def transcribe(source, lang):
        inputs.append(source)
        if isinstance(source, np.ndarray):
            entered.set()
            assert finish.wait(5)
            return [SimpleNamespace(text='stale draft')]
        assert source == str(original)
        return [SimpleNamespace(text='Complete final transcription.')]
    def insert(text):
        outputs.append(text)
        inserted.set()
    monkeypatch.setattr(daemon, '_transcribe', transcribe)
    daemon.injector = SimpleNamespace(insert=insert)
    daemon.recording = None
    daemon.start_recording()
    daemon.recording -= 120
    session = daemon._preview
    dictate.write_state('recording', preview='draft')
    worker = threading.Thread(target=daemon._preview_tick, args=(session,))
    worker.start()
    try:
        assert entered.wait(3)
        daemon.stop_recording()
        state = json.loads(open(dictate.STATE_PATH, encoding='utf-8').read())
        assert state['state'] == 'transcribing'
        assert 'preview' not in state
        assert not outputs
    finally:
        finish.set()
        worker.join(5)
    assert inserted.wait(5)
    assert completed.wait(5)  # final cleanup must finish before globals are restored
    assert outputs == ['Complete final transcription. ']
    assert len(inputs) == 2
    session.thread.join(3)


def test_overlay_pythonw_uses_hidden_console_interpreter(monkeypatch):
    calls = []
    monkeypatch.setattr(tray.sys, 'executable', r'C:\app\.venv\Scripts\pythonw.exe')
    monkeypatch.setattr(tray.subprocess, 'Popen', lambda *a, **kw: calls.append((a, kw)))
    tray.RecordingOverlay().start()
    assert calls[0][0][0][0] == r'C:\app\.venv\Scripts\python.exe'


@pytest.mark.parametrize('platform', ['darwin', 'linux'])
def test_overlay_requests_nonactivating_platform_style(monkeypatch, platform):
    calls = []
    root = SimpleNamespace(_w='.', tk=SimpleNamespace(call=lambda *a: calls.append(a)),
                           attributes=lambda *a: calls.append(a))
    monkeypatch.setattr(tray.sys, 'platform', platform)
    tray.configure_overlay_focus(root)
    assert ('noActivates' in calls[0] if platform == 'darwin'
            else calls[0] == ('-type', 'notification'))


@pytest.mark.skipif(os.environ.get('HUSHKEY_TEST_OVERLAY_UI') != '1',
                    reason='requires a desktop or Xvfb; enabled in CI')
def test_real_overlay_renders_and_exits_on_parent_pipe_eof(tmp_path):
    """Exercise actual Tk main-thread startup, Unicode preview and pipe lifetime."""
    state = tmp_path / 'state.json'
    rendered = tmp_path / 'rendered'
    state.write_text(json.dumps(dict(state='recording', pid=os.getpid(),
                                    preview='Provisional English · Deutsch · 中文')), encoding='utf-8')
    code = f'''
import tkinter as tk
import sys
from pathlib import Path
import tray
tray.STATE_PATH = {str(state)!r}
tray.dictate.LOG_PATH = {str(tmp_path / 'overlay.log')!r}
area = [20, 60, 640, 600]
tray.OverlayMonitor.bounds = lambda self: tuple(area)
mainloop = tk.Misc.mainloop
def checked_mainloop(root, *args):
    def inspect():
        labels = [child.cget('text') for frame in root.winfo_children()
                  for child in frame.winfo_children() if isinstance(child, tk.Label)]
        assert 'Provisional English · Deutsch · 中文' in labels, labels
        assert root.winfo_viewable()
        assert abs(root.winfo_rootx() - (20 + (640 - root.winfo_width()) // 2)) <= 2
        assert abs(root.winfo_rooty() - 66) <= 2
        # Simulate the pointer crossing to another monitor without changing
        # recording state or text. The existing window must follow it.
        area[:] = [660, 100, 520, 600]
        def inspect_move():
            assert abs(root.winfo_rootx() - (660 + (520 - root.winfo_width()) // 2)) <= 2
            assert abs(root.winfo_rooty() - 106) <= 2
            assert root.winfo_viewable()
            if sys.platform == 'win32':
                import ctypes
                from ctypes import wintypes
                api = ctypes.WinDLL('user32')
                api.GetParent.argtypes = [wintypes.HWND]
                api.GetParent.restype = wintypes.HWND
                api.GetForegroundWindow.restype = wintypes.HWND
                assert api.GetForegroundWindow() != api.GetParent(root.winfo_id())
            def finish():
                Path({str(rendered)!r}).write_text('visible', encoding='utf-8')
                print('OVERLAY_RENDERED', flush=True)
            if sys.platform != 'darwin':
                # Real Tk parsing must treat '+-900' as an absolute negative
                # coordinate, not a distance from the right/bottom screen edge.
                area[:] = [-900, -600, 800, 500]
                def inspect_negative():
                    assert abs(root.winfo_rootx() - (-900 + (800 - root.winfo_width()) // 2)) <= 2
                    assert abs(root.winfo_rooty() - (-594)) <= 2
                    finish()
                root.after(600, inspect_negative)
            else:
                finish()  # macOS may constrain windows to physically attached displays
        root.after(600, inspect_move)
    root.after(400, inspect)
    return mainloop(root, *args)
tk.Misc.mainloop = checked_mainloop
tray.RecordingOverlay().run_child()
'''
    child = subprocess.Popen([sys.executable, '-c', code],
                             cwd=Path(tray.__file__).parent, stdin=subprocess.PIPE,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                             creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
    try:
        # Cold Cocoa/Tk startup varies across CI runners. Wait for actual
        # rendering before closing the parent pipe, not an arbitrary sleep.
        deadline = time.monotonic() + 20
        while not rendered.exists() and child.poll() is None and time.monotonic() < deadline:
            time.sleep(0.05)
        if not rendered.exists():
            if child.poll() is None:
                child.kill()
            child.wait(timeout=8)
            log = tmp_path / 'overlay.log'
            pytest.fail('Overlay never rendered:\n' + child.stderr.read().decode(errors='replace')
                        + (log.read_text(encoding='utf-8') if log.exists() else ''))
        assert child.poll() is None
        child.stdin.close()
        assert child.wait(timeout=8) == 0, child.stderr.read().decode(errors='replace')
        assert b'OVERLAY_RENDERED' in child.stdout.read()
    finally:
        if child.poll() is None:
            child.kill()
            child.wait()
