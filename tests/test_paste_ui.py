"""Real paste into a real window: the clipboard comes back afterwards.

A separate process shows a Tk text field and takes the focus; the daemon's
own injector pastes into it with a synthetic Ctrl+V through the real
clipboard. Pins both halves of paste mode end to end, which the clipboard
tests alone cannot: the dictation lands in the target, and the previous
clipboard text is restored once the target has fetched it.
"""
import os
import subprocess
import sys
import time

import pytest

import dictate

pytestmark = [
    pytest.mark.skipif(sys.platform != "win32", reason="Windows paste path"),
    pytest.mark.skipif(os.environ.get("HUSHKEY_TEST_PASTE_UI") != "1",
                       reason="needs an interactive desktop; enabled in CI"),
]

# The target window. It writes "ready <pid>" once it holds the focus (its own
# pid: a venv's python.exe is a launcher with a child interpreter), then the
# text field's contents to `out` as soon as something was pasted.
TARGET = r"""
import ctypes, os, sys, tkinter as tk
out = sys.argv[1]
root = tk.Tk()
root.title("hushkey paste target")
field = tk.Text(root, width=40, height=4)
field.pack()

def take_focus():
    user32 = ctypes.windll.user32
    hwnd = user32.GetParent(root.winfo_id()) or root.winfo_id()
    # Windows only hands the foreground to a process that just saw input;
    # a synthetic Alt tap counts and has no effect of its own here.
    user32.keybd_event(0x12, 0, 0, 0)
    user32.keybd_event(0x12, 0, 2, 0)
    user32.SetForegroundWindow(hwnd)
    field.focus_force()
    root.after(200, report_ready)

def report_ready():
    print("ready", os.getpid(), flush=True)
    root.after(50, poll)

def poll():
    text = field.get("1.0", "end-1c")
    if text:
        with open(out, "w", encoding="utf-8") as fh:
            fh.write(text)
        root.destroy()
        return
    root.after(50, poll)

root.after(300, take_focus)
root.after(20000, root.destroy)
root.mainloop()
"""


@pytest.fixture(autouse=True)
def isolated_state(monkeypatch, tmp_path):
    monkeypatch.setattr(dictate, "LOG_PATH", str(tmp_path / "dictate.log"))


@pytest.fixture
def user_clipboard():
    """Whatever the developer had copied comes back after the test."""
    before = dictate._win_clipboard_read()
    yield
    if before is not None:
        dictate._win_clipboard_write(before)


def _foreground_pid():
    import ctypes
    from ctypes import wintypes
    user32 = ctypes.windll.user32
    user32.GetForegroundWindow.restype = wintypes.HWND
    user32.GetWindowThreadProcessId.argtypes = (wintypes.HWND,
                                                ctypes.POINTER(wintypes.DWORD))
    pid = wintypes.DWORD()
    user32.GetWindowThreadProcessId(user32.GetForegroundWindow(),
                                    ctypes.byref(pid))
    return pid.value


def test_paste_lands_in_the_window_and_restores_the_clipboard(
        monkeypatch, tmp_path, user_clipboard):
    monkeypatch.setenv("PTT_INSERT", "paste")
    monkeypatch.delenv("PTT_KEEP_CLIPBOARD", raising=False)
    text = "Grüße & 名字 — ok?"
    dictate._win_clipboard_write("OLD")
    out = tmp_path / "pasted.txt"
    target = subprocess.Popen([sys.executable, "-c", TARGET, str(out)],
                              stdout=subprocess.PIPE, text=True)
    try:
        ready, pid = target.stdout.readline().split()
        assert ready == "ready"
        assert _foreground_pid() == int(pid), \
            "the target window did not get the focus"
        dictate.PynputInjector().insert(text)
        target.wait(timeout=20)
        assert out.read_text(encoding="utf-8") == text
        assert dictate._win_clipboard_read() == "OLD"
    finally:
        if target.poll() is None:
            target.kill()
