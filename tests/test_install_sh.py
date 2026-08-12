"""Run install.sh in a sandbox and assert what it decides.

The installer is the least testable part of this project: it wants root, a
systemd user session, a Wayland compositor and a package manager. So instead of
a VM, every privileged or stateful command is replaced with a shim on PATH that
records its arguments and does nothing. That leaves the actual decisions —
which units get written, which packages would be installed, whether a logout is
required — observable and assertable.

What this does NOT cover: anything that depends on a command's real effect.
The shims cannot tell us that udev applies the rule, that ydotoold can open
/dev/uinput, or that a paste arrives in the focused window. Reads of the host's
/etc and /dev are also left real (harmless, but see the fixed_host_state notes).
"""
import os
import shutil
import stat
import subprocess

import pytest

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

pytestmark = pytest.mark.skipif(
    not shutil.which("bash") or os.name != "posix",
    reason="install.sh is a bash script for Linux/macOS")


# Commands the installer must never really run. Each shim appends its argv to
# $SHIM_LOG so the test can assert on what would have happened.
SHIMMED = [
    "sudo", "apt-get", "dnf", "pacman", "brew", "usermod", "gpasswd",
    "modprobe", "udevadm", "systemctl", "launchctl", "tee", "install",
    "nvidia-smi", "ydotool", "ydotoold", "wl-copy", "wl-paste", "pw-record",
]

# uname is shimmed too, so the Linux paths are exercised on every runner
# including macOS — otherwise CI would only ever check the launchd branch there.
UNAME_SHIM = """#!/usr/bin/env bash
echo "${FAKE_UNAME:-Linux}"
"""

SHIM = """#!/usr/bin/env bash
printf '%s' "$(basename "$0")" >> "$SHIM_LOG"
for a in "$@"; do printf ' %s' "$a" >> "$SHIM_LOG"; done
printf '\\n' >> "$SHIM_LOG"
exit 0
"""

# systemctl needs to answer queries, not just log them: the installer branches on
# whether the distribution ships ydotool.service. $FAKE_PACKAGED_YDOTOOL selects.
SYSTEMCTL_SHIM = """#!/usr/bin/env bash
printf '%s' systemctl >> "$SHIM_LOG"
for a in "$@"; do printf ' %s' "$a" >> "$SHIM_LOG"; done
printf '\\n' >> "$SHIM_LOG"
for a in "$@"; do
  if [ "$a" = "list-unit-files" ]; then
    if [ "${FAKE_PACKAGED_YDOTOOL:-0}" = "1" ]; then
      echo "ydotool.service                    enabled enabled"
    fi
    exit 0
  fi
done
exit 0
"""


def _write_exec(path, content):
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(content)
    os.chmod(path, os.stat(path).st_mode | stat.S_IEXEC | stat.S_IXGRP)


@pytest.fixture
def sandbox(tmp_path):
    """A fake HOME, a fake venv and a PATH whose privileged commands are shims."""
    home = tmp_path / "home"
    (home / ".config/systemd/user").mkdir(parents=True)
    bindir = tmp_path / "shims"
    bindir.mkdir()
    log = tmp_path / "shim.log"
    log.write_text("")

    for name in SHIMMED:
        _write_exec(bindir / name, SYSTEMCTL_SHIM if name == "systemctl" else SHIM)
    _write_exec(bindir / "uname", UNAME_SHIM)

    # A pre-made venv: install.sh skips creation (and thus pip) when the
    # interpreter is already executable, which keeps this test fast and offline.
    repo = tmp_path / "whisper-ptt"
    shutil.copytree(REPO, repo, ignore=shutil.ignore_patterns(
        ".git", ".venv", "__pycache__", "*.pyc"))
    venv_bin = repo / ".venv/bin"
    venv_bin.mkdir(parents=True)
    for name in ("python", "pip"):
        _write_exec(venv_bin / name, SHIM)

    env = dict(os.environ)
    env.update(
        PATH=f"{bindir}:/usr/bin:/bin",
        HOME=str(home),
        SHIM_LOG=str(log),
        XDG_RUNTIME_DIR=str(tmp_path / "run"),
    )
    env.pop("SUDO_USER", None)
    return {"repo": repo, "home": home, "env": env, "log": log,
            "units": home / ".config/systemd/user"}


def run_installer(sandbox, session_type, packaged_ydotool="0", uname="Linux"):
    env = dict(sandbox["env"])
    env["XDG_SESSION_TYPE"] = session_type
    env["FAKE_PACKAGED_YDOTOOL"] = packaged_ydotool
    env["FAKE_UNAME"] = uname
    proc = subprocess.run(["bash", str(sandbox["repo"] / "install.sh")],
                          env=env, capture_output=True, text=True, timeout=180)
    return proc, sandbox["log"].read_text()


def test_sudo_is_really_shimmed(sandbox):
    """Guard the guard: if PATH shimming broke, every other test would be
    running privileged commands against the developer's machine."""
    which = subprocess.run(["bash", "-c", "command -v sudo"],
                           env=sandbox["env"], capture_output=True, text=True)
    assert which.stdout.strip() == str(sandbox["repo"].parent / "shims/sudo")


def test_wayland_uses_packaged_unit_when_present(sandbox):
    """The bug this test exists for: two ydotoold units fight over one socket."""
    proc, log = run_installer(sandbox, "wayland", packaged_ydotool="1")
    assert proc.returncode == 0, proc.stderr
    assert not (sandbox["units"] / "ydotoold.service").exists(), \
        "installed a second ydotoold unit despite the packaged one"
    assert "enable ydotool.service" in log
    assert "enable ydotoold.service" not in log


def test_wayland_installs_own_unit_without_packaged_one(sandbox):
    proc, log = run_installer(sandbox, "wayland", packaged_ydotool="0")
    assert proc.returncode == 0, proc.stderr
    unit = (sandbox["units"] / "ydotoold.service").read_text()
    assert "@YDOTOOLD@" not in unit and "@DIR@" not in unit  # fully substituted
    assert "/ydotoold --socket-path=" in unit
    assert "enable ydotoold.service" in log


def test_rerun_removes_a_redundant_unit(sandbox):
    """Repairing an existing install: ours must go when the packaged one exists."""
    stale = sandbox["units"] / "ydotoold.service"
    stale.write_text("[Service]\nExecStart=/usr/bin/ydotoold\n")
    proc, log = run_installer(sandbox, "wayland", packaged_ydotool="1")
    assert proc.returncode == 0, proc.stderr
    assert not stale.exists()
    assert "disable --now ydotoold.service" in log


def test_wayland_grants_uinput_and_group_access(sandbox):
    """Both grants are idempotent, so the expectation depends on host state.

    The installer compares against the real /etc/udev rule and the real group
    list — that is the point of it being re-runnable — so on a machine where
    whisper-ptt is already set up it correctly does nothing. Asserting
    unconditionally would only pass on machines that never ran it.
    """
    proc, log = run_installer(sandbox, "wayland", packaged_ydotool="1")
    assert proc.returncode == 0, proc.stderr

    rule = "/etc/udev/rules.d/99-whisper-ptt-uinput.rules"
    repo_rule = sandbox["repo"] / "udev/99-whisper-ptt-uinput.rules"
    rule_already_current = (
        os.path.exists(rule)
        and open(rule, "rb").read() == repo_rule.read_bytes())
    assert ("99-whisper-ptt-uinput.rules" in log) != rule_already_current
    assert ("udevadm control --reload-rules" in log) != rule_already_current

    # Ask the way the installer asks. Bare `id -nG` reports the current
    # process's credentials, `id -nG $user` reads /etc/group — and between
    # usermod and the next login the two disagree. The installer trusts the
    # latter, so probing with the former made this test fail on any machine
    # sitting in that window.
    me = subprocess.run(["id", "-un"], capture_output=True,
                        text=True).stdout.strip()
    in_input_group = "input" in subprocess.run(
        ["id", "-nG", me], capture_output=True, text=True).stdout.split()
    assert ("usermod -aG input" in log) != in_input_group


@pytest.mark.skipif(
    any(os.path.exists(os.path.join(d, "ydotoold")) for d in ("/usr/bin", "/bin")),
    reason="host ships a real ydotoold, and the sandbox PATH cannot hide it")
def test_missing_ydotoold_is_asked_for_as_its_own_package(sandbox):
    """Debian and Ubuntu ship the daemon in a separate 'ydotoold' package, so
    installing 'ydotool' leaves the installer without a daemon.

    This gap survived because the sandbox shims ydotoold like everything else:
    with the binary always on PATH the installer never had to ask for it. Here
    the shim is removed, which is the real Ubuntu 24.04 shape.

    Removing a shim can only uncover the host's own binary, so this skips where
    ydotoold is really installed — including, ironically, a machine that hit the
    bug and fixed it by hand. CI runners have no ydotoold, which is where it
    counts.
    """
    shims = sandbox["repo"].parent / "shims"
    os.remove(shims / "ydotoold")

    proc, log = run_installer(sandbox, "wayland", packaged_ydotool="0")

    assert "apt-get install -y ydotoold" in log, \
        "installer never asked for the ydotoold package"
    # The shim cannot actually install it, so the pre-existing guard must still
    # fire rather than writing a unit that points at a missing binary.
    assert proc.returncode != 0
    assert "ydotoold not found in PATH" in proc.stdout + proc.stderr
    assert not (sandbox["units"] / "ydotoold.service").exists()


def test_ydotoold_is_a_package_the_installer_knows_about():
    """Host-independent companion to the test above, which skips wherever
    ydotoold happens to be installed. Cheap, but it holds everywhere."""
    with open(os.path.join(REPO, "install.sh"), encoding="utf-8") as fh:
        text = fh.read()
    assert "install_pkg ydotoold" in text, \
        "installer never asks for the separate ydotoold package"


def test_venv_is_created_with_system_site_packages():
    """The tray needs the distro's PyGObject plus an AppIndicator typelib, and
    pip can supply neither. In a sealed venv pystray falls back to its Xorg
    backend, which on GNOME/Wayland draws the icon into a tray that is not
    there — dictation works, every menu is unreachable.

    Asserted on the source text rather than by running it: exercising this means
    letting the installer build a real venv and pip-install the whole dependency
    tree, which needs the network and minutes per test.
    """
    with open(os.path.join(REPO, "install.sh"), encoding="utf-8") as fh:
        lines = fh.read().splitlines()
    creating = [ln for ln in lines if "python3 -m venv" in ln]
    assert creating, "no venv creation found in install.sh"
    for line in creating:
        assert "--system-site-packages" in line, \
            f"venv created without --system-site-packages: {line.strip()}"


def test_x11_skips_the_whole_wayland_setup(sandbox):
    proc, log = run_installer(sandbox, "x11")
    assert proc.returncode == 0, proc.stderr
    assert not (sandbox["units"] / "ydotoold.service").exists()
    assert "ydotool" not in log     # no ydotool/ydotoold/packages touched
    assert "usermod" not in log     # no group grant on X11
    assert (sandbox["units"] / "whisper-ptt.service").exists()


def test_main_unit_is_always_written_and_substituted(sandbox):
    for session in ("x11", "wayland"):
        proc, _ = run_installer(sandbox, session, packaged_ydotool="1")
        assert proc.returncode == 0, proc.stderr
        unit = (sandbox["units"] / "whisper-ptt.service").read_text()
        assert "@DIR@" not in unit
        assert str(sandbox["repo"]) in unit
        assert "tray.py" in unit  # the tray is the entry point and runs the daemon


def test_unsupported_os_is_rejected(sandbox):
    """The OS gate: anything that is not Linux or Darwin must not proceed."""
    proc, _ = run_installer(sandbox, "wayland", uname="FreeBSD")
    assert proc.returncode != 0
    assert "unsupported OS" in proc.stdout + proc.stderr


def test_macos_installs_a_launchd_agent_and_no_wayland_bits(sandbox):
    """Darwin takes the launchd branch and must not touch any Linux setup."""
    proc, log = run_installer(sandbox, "", uname="Darwin")
    assert proc.returncode == 0, proc.stderr
    plist = sandbox["home"] / "Library/LaunchAgents/com.whisper-ptt.plist"
    assert plist.exists()
    content = plist.read_text()
    assert "@DIR@" not in content and "@HOME@" not in content
    assert "launchctl bootstrap" in log
    assert not (sandbox["units"] / "ydotoold.service").exists()
    assert "usermod" not in log and "udevadm" not in log
