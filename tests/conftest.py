import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


import threading
import time

import pytest


@pytest.fixture(autouse=True)
def _join_stray_threads(monkeypatch):
    """Let a test's worker threads finish before the next test starts.

    The daemon hands transcription to background threads that read module
    globals such as dictate.STATE_PATH when they run. A worker still alive
    when its test ends would write into the *next* test's monkeypatched state
    file — on slow macOS runners that published a stray 'idle' mid-assertion.

    Depends on monkeypatch so it is torn down first: the joined workers must
    finish against the test's patches, not against the real state dir.
    """
    before = set(threading.enumerate())
    yield
    deadline = time.monotonic() + 10  # one budget per test, not per thread
    for thread in set(threading.enumerate()) - before:
        if thread.daemon and _started_by_our_code(thread):
            thread.join(timeout=max(0.0, deadline - time.monotonic()))


def _started_by_our_code(thread):
    """Only our own workers: libraries (huggingface, tqdm) keep monitor
    threads alive for the whole process and would eat the whole budget. A
    worker spawned indirectly (a test thread calling stop_recording) still
    counts — its own target is dictate's."""
    target = getattr(thread, "_target", None)
    module = getattr(target, "__module__", None) or ""
    return module.split(".")[0] in {"dictate", "tray", "recorder", "transcribe"}


@pytest.fixture(autouse=True)
def _isolated_history(monkeypatch, tmp_path):
    """Dictation tests must never write into the user's real history.json."""
    import dictate
    monkeypatch.setattr(dictate, "HISTORY_PATH", str(tmp_path / "history.json"))


@pytest.fixture(autouse=True)
def _typing_by_default(monkeypatch):
    """Most insert tests predate paste mode; pin typing, which also keeps
    every test off the real clipboard (paste tests opt in and stub it)."""
    monkeypatch.setenv("PTT_INSERT", "type")


class _FakeOwnerThread:
    alive = True
    id_mask = 0x1FFFFF

    def __init__(self, window_id=0x4A00001):
        self.window_id = window_id

    def is_alive(self):
        return self.alive


def fake_x11_owner(monkeypatch, owned, fetch=True, focus=None,
                   requestor=None):
    """Replace dictate._x11_clipboard_own with a recorder of the data owned.
    With fetch, `requestor` fetches the text as soon as an offer is armed —
    what a real target does in reply to the chord. `focus` is the id of the
    input-focus window (None: no window to attribute the paste to)."""
    import dictate
    owners = []

    def own(data, on_request=None):
        owned.append(data)
        owners.append(_FakeOwnerThread(window_id=0x4A00001 + len(owners)))
        return owners[-1]

    monkeypatch.setattr(dictate, "_x11_clipboard_own", own)
    monkeypatch.setattr(dictate, "_x11_focus_window_id", lambda: focus)
    monkeypatch.setattr(dictate, "_x11_clipboard_owner_id",
                        lambda: owners[-1].window_id)
    monkeypatch.setattr(dictate, "CLIPBOARD_SETTLE", 0.0)
    if fetch:
        arm = dictate._X11ClipboardOffer.arm

        def arm_and_fetch(self):
            arm(self)
            self._on_request(True, requestor)

        monkeypatch.setattr(dictate._X11ClipboardOffer, "arm", arm_and_fetch)
