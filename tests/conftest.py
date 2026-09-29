import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


import threading

import pytest


@pytest.fixture(autouse=True)
def _join_stray_threads():
    """Let a test's worker threads finish before the next test starts.

    The daemon hands transcription to background threads that read module
    globals such as dictate.STATE_PATH when they run. A worker still alive
    when its test ends would write into the *next* test's monkeypatched state
    file — on slow macOS runners that published a stray 'idle' mid-assertion.
    """
    before = set(threading.enumerate())
    yield
    for thread in set(threading.enumerate()) - before:
        if thread.daemon and _started_by_our_code(thread):
            thread.join(timeout=10)


def _started_by_our_code(thread):
    """Only our own workers: libraries (huggingface, tqdm) keep monitor
    threads alive for the whole process, and joining those costs 10 s each."""
    target = getattr(thread, "_target", None)
    module = getattr(target, "__module__", None) or ""
    return module.split(".")[0] in {"dictate", "tray", "recorder", "transcribe"}
