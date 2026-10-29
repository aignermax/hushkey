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

Only the standard library: this runs in whatever Python started tray.py.
"""
import os
import shutil
import subprocess
import sys

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


def main():
    os.makedirs(state_dir(), exist_ok=True)
    log_path = os.path.join(state_dir(), "update.log")
    # A pending marker of the Python updater is answered by this handover.
    try:
        os.remove(os.path.join(state_dir(), "update-pending"))
    except OSError:
        pass
    cmd = outside_the_service(handover_command())
    kwargs = {"cwd": DIR, "stdin": subprocess.DEVNULL}
    if sys.platform == "win32":
        kwargs["creationflags"] = (subprocess.CREATE_NEW_PROCESS_GROUP
                                   | subprocess.CREATE_NO_WINDOW)
    else:
        kwargs["start_new_session"] = True
    with open(log_path, "ab") as log:
        log.write(f"--- handover to the native hushkey: {cmd}\n".encode())
        log.flush()
        try:
            rc = subprocess.run(cmd, stdout=log, stderr=subprocess.STDOUT,
                                timeout=900, **kwargs).returncode
            log.write(f"--- handover finished rc={rc}\n".encode())
        except (OSError, subprocess.SubprocessError) as exc:
            log.write(f"handover failed: {exc}\n".encode())
    # Always 0: a failure here is retried at the next login, not in a
    # service-manager restart loop.
    return 0


if __name__ == "__main__":
    sys.exit(main())
