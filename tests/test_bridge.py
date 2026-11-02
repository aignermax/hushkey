"""The Python hushkey's last tray.py hands over to the native program."""
import os
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import tray  # noqa: E402


@pytest.fixture
def home(tmp_path, monkeypatch):
    for var in ("HOME", "USERPROFILE"):
        monkeypatch.setenv(var, str(tmp_path))
    monkeypatch.setenv("LOCALAPPDATA", str(tmp_path / "AppData" / "Local"))
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path / "state"))
    monkeypatch.delenv("INVOCATION_ID", raising=False)
    return tmp_path


def test_without_the_program_the_installer_runs(home):
    cmd = tray.handover_command()
    script = "install.ps1" if sys.platform == "win32" else "install.sh"
    assert cmd[-1] == os.path.join(tray.DIR, script)


def test_an_installed_program_takes_over_itself(home):
    exe = Path(tray.native_exe())
    exe.parent.mkdir(parents=True)
    exe.write_bytes(b"")
    assert tray.handover_command() == [str(exe), "--install", "--from-python"]


def test_under_systemd_the_handover_leaves_the_service(home, monkeypatch):
    monkeypatch.setenv("INVOCATION_ID", "x")
    monkeypatch.setattr(tray.shutil, "which", lambda name: "/usr/bin/" + name)
    assert tray.outside_the_service(["a"])[:3] == ["systemd-run", "--user", "--scope"]
    monkeypatch.delenv("INVOCATION_ID")
    assert tray.outside_the_service(["a"]) == ["a"]


def test_the_handover_runs_logs_and_clears_the_update_marker(home, monkeypatch):
    state = Path(tray.state_dir())
    state.mkdir(parents=True)
    (state / "update-pending").write_text("")
    ran = []

    def fake_run(cmd, **kwargs):
        ran.append(cmd)
        return subprocess.CompletedProcess(cmd, 0)

    monkeypatch.setattr(tray.subprocess, "run", fake_run)
    assert tray.main() == 0
    assert ran == [tray.handover_command()]
    assert not (state / "update-pending").exists()
    log = (state / "update.log").read_text()
    assert "handover finished rc=0" in log


class FakeDaemon:
    def __init__(self):
        self.terminated = False

    def poll(self):
        return 0 if self.terminated else None

    def terminate(self):
        self.terminated = True


def test_a_failed_handover_keeps_dictation_and_tries_again(home, monkeypatch):
    results = iter([1, 2, 0])
    ran, pauses, notes, daemons = [], [], [], []

    def fake_run(cmd, **kwargs):
        ran.append(cmd)
        return subprocess.CompletedProcess(cmd, next(results))

    def fake_daemon():
        daemons.append(FakeDaemon())
        return daemons[-1]

    monkeypatch.setattr(tray.subprocess, "run", fake_run)
    monkeypatch.setattr(tray.time, "sleep", pauses.append)
    monkeypatch.setattr(tray, "python_dictation", fake_daemon)
    monkeypatch.setattr(tray, "notify", lambda *a: notes.append(a))
    assert tray.main() == 0
    assert len(ran) == 3
    # Paused in between (no service-manager restart loop), told the user
    # once, and kept one Python dictation running until the handover held.
    assert sum(pauses) == sum(tray.RETRY_PAUSES[:2])
    assert max(pauses) <= tray.WATCH
    assert len(notes) == 1
    assert len(daemons) == 1 and daemons[0].terminated
    log = (Path(tray.state_dir()) / "update.log").read_text()
    assert "rc=1" in log and "rc=0" in log


def test_a_session_that_cannot_run_it_waits_a_day(home, monkeypatch):
    results = iter([tray.NOT_READY, 0])
    pauses = []
    monkeypatch.setattr(tray.subprocess, "run",
                        lambda cmd, **kw: subprocess.CompletedProcess(cmd, next(results)))
    monkeypatch.setattr(tray.time, "sleep", pauses.append)
    monkeypatch.setattr(tray, "python_dictation", FakeDaemon)
    monkeypatch.setattr(tray, "notify", lambda *a: None)
    assert tray.main() == 0
    assert sum(pauses) == tray.NOT_READY_PAUSE


def test_a_python_dictation_that_exits_comes_back_during_a_pause(home, monkeypatch):
    results = iter([tray.NOT_READY, 0])
    started = []

    class Exits(FakeDaemon):
        def poll(self):
            return 0  # gone at once

    def daemon():
        started.append(Exits())
        return started[-1]

    monkeypatch.setattr(tray.subprocess, "run",
                        lambda cmd, **kw: subprocess.CompletedProcess(cmd, next(results)))
    monkeypatch.setattr(tray.time, "sleep", lambda s: None)
    monkeypatch.setattr(tray, "python_dictation", daemon)
    monkeypatch.setattr(tray, "notify", lambda *a: None)
    tray.main()
    # Once at the failure, then at every look during the day's pause.
    assert len(started) == 1 + tray.NOT_READY_PAUSE // tray.WATCH


def test_a_second_tray_waits_for_the_first(home, tmp_path):
    # The lock is held by another process; ours must not get it.
    holder = subprocess.Popen(
        [sys.executable, "-c",
         "import sys, time; sys.path.insert(0, sys.argv[1]); import tray; "
         "l = tray.single_instance(); print('held', flush=True); time.sleep(30)",
         tray.DIR],
        stdout=subprocess.PIPE, text=True, env=dict(os.environ))
    try:
        assert holder.stdout.readline().strip() == "held"
        waiter = subprocess.Popen(
            [sys.executable, "-c",
             "import sys; sys.path.insert(0, sys.argv[1]); import tray; "
             "tray.single_instance(); print('got it', flush=True)",
             tray.DIR],
            stdout=subprocess.PIPE, text=True, env=dict(os.environ))
        with pytest.raises(subprocess.TimeoutExpired):
            waiter.wait(timeout=2)
        holder.kill()
        holder.wait()
        out, _ = waiter.communicate(timeout=15)
        assert out.strip() == "got it"
    finally:
        holder.kill()


def test_the_python_dictation_is_only_started_where_it_is_left(home, monkeypatch):
    monkeypatch.setattr(tray, "DIR", str(home))
    assert tray.python_dictation() is None


def test_the_update_helper_still_imports():
    # The old tray spawns update_helper.py after fetching this code.
    import update_helper  # noqa: F401
