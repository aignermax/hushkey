#!/usr/bin/env python3
"""One-shot update helper — spawned detached by the tray right before it exits.

Waits for the old tray to die, runs the installer (phase 2), then starts a
fresh tray. Being a separate short-lived process means a self-update can
never leave the user with nothing running: even if the old tray dies
mid-handover, this helper still brings the new one up.

Everything is logged to update.log — a failed update must be diagnosable.
"""
import os
import subprocess
import sys
import time

DIR = os.path.dirname(os.path.abspath(__file__))

sys.path.insert(0, DIR)
import dictate  # noqa: E402  local module: STATE_DIR

STATE_DIR = dictate.STATE_DIR
MARKER = os.path.join(STATE_DIR, "update-pending")
LOG = os.path.join(STATE_DIR, "update.log")
TRAY = os.path.join(DIR, "tray.py")


def log(msg):
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


def main():
    old_pid = int(sys.argv[1]) if len(sys.argv) > 1 else None
    log(f"started (waiting for tray pid {old_pid} to exit)")

    deadline = time.time() + 60
    while old_pid and pid_alive(old_pid) and time.time() < deadline:
        time.sleep(0.5)

    if os.path.exists(MARKER):
        os.remove(MARKER)
        log("old tray gone, running installer")
        with open(LOG, "ab") as out:
            if sys.platform == "win32":
                cmd = ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass",
                       "-File", os.path.join(DIR, "install.ps1")]
            else:
                cmd = ["bash", os.path.join(DIR, "install.sh")]
            try:
                proc = subprocess.run(cmd, cwd=DIR, timeout=900,
                                      stdout=out, stderr=subprocess.STDOUT)
                log(f"installer finished rc={proc.returncode}")
            except (OSError, subprocess.SubprocessError) as exc:
                log(f"installer failed: {exc}")
    else:
        log("no update marker — nothing to install")

    # Whatever happened above: bring the tray back.
    kwargs = {"cwd": DIR, "stdout": subprocess.DEVNULL,
              "stderr": subprocess.DEVNULL, "close_fds": True}
    if sys.platform == "win32":
        kwargs["creationflags"] = (subprocess.DETACHED_PROCESS
                                   | subprocess.CREATE_NEW_PROCESS_GROUP)
    else:
        kwargs["start_new_session"] = True
    proc = subprocess.Popen([sys.executable, TRAY], **kwargs)
    log(f"new tray spawned, pid {proc.pid} — helper done")


if __name__ == "__main__":
    main()
