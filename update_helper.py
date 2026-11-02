#!/usr/bin/env python3
"""One-shot update helper — spawned detached by the Python tray right before
it exits, after it fetched this code (Windows and macOS; under systemd the
restarted service starts tray.py instead).

Waits for the old tray to die, then starts tray.py, which now hands over to
the native hushkey. The handover itself happens there and only there: under
its single-instance lock, so a launchd restart next to this helper's start
cannot run two at once.

Everything is logged to update.log — a failed update must be diagnosable.
"""
import os
import subprocess
import sys
import time

DIR = os.path.dirname(os.path.abspath(__file__))

sys.path.insert(0, DIR)
import tray  # noqa: E402  local module: the state folder

STATE_DIR = tray.state_dir()
LOG = os.path.join(STATE_DIR, "update.log")
TRAY = os.path.join(DIR, "tray.py")


def log(msg):
    os.makedirs(STATE_DIR, exist_ok=True)
    with open(LOG, "ab") as out:
        out.write(f"--- helper {time.strftime('%H:%M:%S')} {msg}\n".encode())


def pid_alive(pid):
    try:
        if sys.platform == "win32":
            import ctypes
            handle = ctypes.windll.kernel32.OpenProcess(0x1000, False, pid)
            if not handle:
                return False
            ctypes.windll.kernel32.CloseHandle(handle)
            return True
        os.kill(pid, 0)
        return True
    except OSError:
        return False


def start_tray():
    """Start tray.py on its own; one retry."""
    kwargs = {"cwd": DIR, "stdout": subprocess.DEVNULL,
              "stderr": subprocess.DEVNULL, "close_fds": True}
    if sys.platform == "win32":
        kwargs["creationflags"] = (subprocess.DETACHED_PROCESS
                                   | subprocess.CREATE_NEW_PROCESS_GROUP)
    else:
        kwargs["start_new_session"] = True
    for attempt in (1, 2):
        try:
            proc = subprocess.Popen([sys.executable, TRAY], **kwargs)
            log(f"tray.py started, pid {proc.pid} — helper done")
            return
        except OSError as exc:
            log(f"tray.py could not start (attempt {attempt}): {exc}")
            time.sleep(2)


def main():
    try:
        old_pid = int(sys.argv[1]) if len(sys.argv) > 1 else None
    except ValueError:
        old_pid = None
    log(f"started (waiting for tray pid {old_pid} to exit)")
    deadline = time.time() + 60
    while old_pid and pid_alive(old_pid) and time.time() < deadline:
        time.sleep(0.5)
    if old_pid and pid_alive(old_pid):
        log("old tray still alive after 60s — proceeding anyway")
    # A second tray.py (launchd's restart) just waits on the lock.
    start_tray()


if __name__ == "__main__":
    main()
