#!/usr/bin/env python3
"""One-shot update helper — spawned detached by the tray right before it exits.

Waits for the old tray to die, runs the installer (phase 2), then starts a
fresh tray. Being a separate short-lived process means a self-update can
never leave the user with nothing running: even if the old tray dies
mid-handover, this helper still brings the new one up.

Not used under systemd (the cgroup kill would take the helper down with the
tray) — there the restarted service completes the update via
run_pending_update_if_any().

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


def run_installer():
    """Run the platform installer, streaming into update.log. Returns rc."""
    with open(LOG, "ab") as out:
        if sys.platform == "win32":
            cmd = ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass",
                   "-File", os.path.join(DIR, "install.ps1")]
        else:
            cmd = ["bash", os.path.join(DIR, "install.sh")]
        try:
            return subprocess.run(cmd, cwd=DIR, timeout=900,
                                  stdout=out, stderr=subprocess.STDOUT).returncode
        except (OSError, subprocess.SubprocessError) as exc:
            log(f"installer could not run: {exc}")
            return -1


def start_tray():
    """Bring the tray back, whatever happened above. One retry, then we log
    and give up — the marker is still around, so the next boot retries."""
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
            log(f"new tray spawned, pid {proc.pid} — helper done")
            return
        except OSError as exc:
            log(f"tray spawn failed (attempt {attempt}): {exc}")
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

    if os.path.exists(MARKER):
        rc = run_installer()
        if rc == 0:
            try:
                os.remove(MARKER)
            except FileNotFoundError:
                pass  # a restarted tray's safety net consumed it first
            log("installer finished rc=0")
        else:
            # keep the marker: the next tray start retries the installer
            log(f"installer failed rc={rc} — marker kept for the next start")
    else:
        log("no update marker — nothing to install")

    # The installer may already have started a tray itself (install.ps1 does);
    # ours would then lose the single-instance lock after its 15 s wait and
    # bow out quietly — redundant but harmless.
    start_tray()


if __name__ == "__main__":
    main()
