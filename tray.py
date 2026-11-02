#!/usr/bin/env python3
"""hushkey moved to a native program; this file hands over to it.

The Python hushkey is started as `tray.py` by its autostart (Task Scheduler
or Startup shortcut on Windows, the whisper-ptt systemd user service on
Linux, the com.whisper-ptt LaunchAgent on macOS) and by its updater after an
update. All of them now land here, and this:

- installs the native hushkey through install.ps1 / install.sh when it is
  not installed yet (that is how the Python updater's last update ends), or
- runs the installed one with `--install --from-python`, which is quick when
  it is already in place: it takes over the Python settings once, switches
  the Python autostart off, and starts the program unless it runs.

Until the handover has worked, the Python dictation keeps running (its
dictate.py is still there after an archive update), and the handover is
tried again after a pause: a failed one must neither leave the user without
dictation nor hammer GitHub from a service manager's restart loop.

Only the standard library: this runs in whatever Python started tray.py.
"""
import os
import shutil
import subprocess
import sys
import time

DIR = os.path.dirname(os.path.abspath(__file__))


def state_dir():
    if sys.platform == "darwin":
        return os.path.expanduser("~/Library/Logs/whisper-ptt")
    if sys.platform == "win32":
        return os.path.join(os.environ.get(
            "LOCALAPPDATA", os.path.expanduser("~/AppData/Local")),
            "whisper-ptt")
    return os.path.join(os.environ.get("XDG_STATE_HOME",
                                       os.path.expanduser("~/.local/state")),
                        "whisper-ptt")


def native_exe():
    """Where install.ps1 / install.sh put the native program."""
    if sys.platform == "win32":
        return os.path.join(os.environ.get(
            "LOCALAPPDATA", os.path.expanduser("~/AppData/Local")),
            "Programs", "hushkey", "hushkey.exe")
    if sys.platform == "darwin":
        return os.path.expanduser(
            "~/Applications/hushkey.app/Contents/MacOS/hushkey")
    return os.path.expanduser("~/.local/bin/hushkey")


def handover_command():
    exe = native_exe()
    if os.path.isfile(exe):
        return [exe, "--install", "--from-python"]
    if sys.platform == "win32":
        return ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass",
                "-File", os.path.join(DIR, "install.ps1")]
    return ["bash", os.path.join(DIR, "install.sh")]


def outside_the_service(cmd):
    """Run `cmd` so that switching the Python service off cannot stop it.

    The handover itself disables the service that started us: systemd stops
    every process in the service's cgroup, launchd the job's process group.
    A systemd scope and a new session keep the handover (and the program it
    starts) out of both.
    """
    if os.environ.get("INVOCATION_ID") and shutil.which("systemd-run"):
        return ["systemd-run", "--user", "--scope", "--quiet", "--collect"] + cmd
    return cmd


# Pauses between handover attempts (seconds); the last one repeats.
RETRY_PAUSES = (120, 600, 1800, 3600)
# The native program said this session cannot run it yet (exit code 3):
# that does not change by the hour, and each attempt is a download.
NOT_READY = 3
NOT_READY_PAUSE = 24 * 3600


def single_instance():
    """Hold tray.lock (the old tray's lock file) for as long as we run.

    More than one tray.py can be started — launchd's KeepAlive restart next
    to the update helper's start, or the old Start Menu entry clicked — and
    two would start two Python dictations, typing everything twice. A
    second one waits here until the first is done, then finds the native
    program installed and the handover a formality.
    """
    os.makedirs(state_dir(), exist_ok=True)
    fh = open(os.path.join(state_dir(), "tray.lock"), "a+b")
    if sys.platform == "win32":
        import msvcrt
        fh.seek(0)
        fh.write(b"\0")  # a byte to lock, as the old tray did
        fh.flush()
        while True:
            try:
                fh.seek(0)
                msvcrt.locking(fh.fileno(), msvcrt.LK_NBLCK, 1)
                return fh
            except OSError:
                time.sleep(5)
    import fcntl
    fcntl.flock(fh, fcntl.LOCK_EX)
    return fh


def handover(log_path):
    """One attempt; the installer's exit code (0: the native program took
    over), or None when it could not run at all."""
    cmd = outside_the_service(handover_command())
    kwargs = {"cwd": DIR, "stdin": subprocess.DEVNULL}
    if sys.platform == "win32":
        kwargs["creationflags"] = (subprocess.CREATE_NEW_PROCESS_GROUP
                                   | subprocess.CREATE_NO_WINDOW)
    else:
        kwargs["start_new_session"] = True
    with open(log_path, "ab") as log:
        log.write(f"--- {time.strftime('%Y-%m-%d %H:%M:%S')} handover to the "
                  f"native hushkey: {cmd}\n".encode())
        log.flush()
        try:
            rc = subprocess.run(cmd, stdout=log, stderr=subprocess.STDOUT,
                                timeout=900, **kwargs).returncode
        except (OSError, subprocess.SubprocessError) as exc:
            log.write(f"--- handover failed: {exc}\n".encode())
            return None
        log.write(f"--- handover finished rc={rc}\n".encode())
    return rc


def python_dictation():
    """Start the Python dictation daemon left from before the update, so the
    key keeps working while the handover waits. None if it is gone (a git
    install's pull removed it) or does not start."""
    daemon = os.path.join(DIR, "dictate.py")
    if not os.path.isfile(daemon):
        return None
    kwargs = {"cwd": DIR, "stdin": subprocess.DEVNULL}
    if sys.platform == "win32":
        kwargs["creationflags"] = subprocess.CREATE_NO_WINDOW
    try:
        return subprocess.Popen([sys.executable, daemon], **kwargs)
    except OSError:
        return None


def notify(title, text):
    """A desktop notification, where one is easy to send; best effort."""
    try:
        if sys.platform == "darwin":
            script = f'display notification {text!r} with title {title!r}'
            subprocess.run(["osascript", "-e", script.replace("'", '"')],
                           timeout=10, capture_output=True)
        elif sys.platform.startswith("linux") and shutil.which("notify-send"):
            subprocess.run(["notify-send", "-a", "hushkey", title, text],
                           timeout=10, capture_output=True)
    except (OSError, subprocess.SubprocessError):
        pass


def main():
    lock = single_instance()  # noqa: F841  held until we exit
    log_path = os.path.join(state_dir(), "update.log")
    # A pending marker of the Python updater is answered by this handover.
    try:
        os.remove(os.path.join(state_dir(), "update-pending"))
    except OSError:
        pass
    fallback = None
    attempt = 0
    while True:
        rc = handover(log_path)
        if rc == 0:
            break
        if fallback is None or fallback.poll() is not None:
            fallback = python_dictation()
        if attempt == 0:
            notify("hushkey update not finished yet",
                   "Dictation keeps working; the update is tried again "
                   "later. Details: " + log_path)
        # Staying alive, rather than exiting for a service manager to start
        # us again, keeps this pause: launchd's KeepAlive would otherwise
        # rerun the download every few seconds.
        if rc == NOT_READY:
            time.sleep(NOT_READY_PAUSE)
        else:
            time.sleep(RETRY_PAUSES[min(attempt, len(RETRY_PAUSES) - 1)])
        attempt += 1
    # The native program runs; it stopped the Python processes it found,
    # but the fallback is ours to end in any case.
    if fallback is not None and fallback.poll() is None:
        fallback.terminate()
    return 0


if __name__ == "__main__":
    sys.exit(main())
