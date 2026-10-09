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
