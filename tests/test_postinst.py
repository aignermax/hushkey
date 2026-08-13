"""Execute the deb's postinst and assert whose home it installs into.

The bug this file exists for: postinst resolved the target user from SUDO_USER
alone, so a graphical install (GNOME Software, gdebi, double-clicking the .deb)
went through pkexec/PackageKit, found nothing, printed a hint into a log nobody
reads and exited 0. dpkg reported success while only /opt had been populated —
no venv, no systemd units, no working install.

test_packaging.py checks the maintainer scripts with `bash -n`, which parses but
never runs them, so it could not catch this. Here the script really executes
with sudo/chown/getent/logname/loginctl replaced by shims, which makes the
identity it settles on observable.
"""
import os
import stat
import subprocess

import pytest

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
POSTINST = os.path.join(REPO, "packaging", "linux", "postinst")

pytestmark = pytest.mark.skipif(
    os.name != "posix", reason="postinst is a POSIX shell script")

LOGGING_SHIM = """#!/usr/bin/env bash
printf '%s' "$(basename "$0")" >> "$SHIM_LOG"
for a in "$@"; do printf ' %s' "$a" >> "$SHIM_LOG"; done
printf '\\n' >> "$SHIM_LOG"
exit 0
"""

# One fixed passwd record for whatever key it is asked about, so the fake user
# resolves both by name (USER_HOME) and by uid (the PKEXEC_UID path).
GETENT_SHIM = """#!/usr/bin/env bash
printf '%s:x:4242:4242::%s:/bin/bash\\n' "${FAKE_USER:-tester}" "$FAKE_HOME"
"""

ID_SHIM = """#!/usr/bin/env bash
case "$1" in
  -gn) echo "${FAKE_USER:-tester}" ;;
  *)   echo 4242 ;;
esac
"""

# No controlling terminal is the whole point of the GUI case: logname fails.
LOGNAME_SHIM = """#!/usr/bin/env bash
if [ -n "${FAKE_LOGNAME:-}" ]; then echo "$FAKE_LOGNAME"; exit 0; fi
echo "logname: no login name" >&2
exit 1
"""

LOGINCTL_SHIM = """#!/usr/bin/env bash
# FAKE_SESSIONS: one "sid|user|class|type" per line.
case "$1" in
  list-sessions)
    printf '%s\n' "${FAKE_SESSIONS:-}" | awk -F'|' 'NF {print $1" 1000 "$2" seat0 "$4}'
    ;;
  show-session)
    sid="$2"; prop=""
    shift 2
    while [ $# -gt 0 ]; do
      case "$1" in
        -p) prop="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    line="$(printf '%s\n' "${FAKE_SESSIONS:-}" | awk -F'|' -v s="$sid" '$1==s')"
    case "$prop" in
      Class) printf '%s' "$line" | cut -d'|' -f3 ;;
      Type)  printf '%s' "$line" | cut -d'|' -f4 ;;
      Name)  printf '%s' "$line" | cut -d'|' -f2 ;;
    esac
    echo
    ;;
esac
exit 0
"""


def _write_exec(path, content):
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(content)
    os.chmod(path, os.stat(path).st_mode | stat.S_IEXEC | stat.S_IXGRP)


@pytest.fixture
def sandbox(tmp_path):
    """A payload to copy, a home to copy into, and shims for the privileged bits."""
    payload = tmp_path / "payload"
    payload.mkdir()
    (payload / "dictate.py").write_text("# stand-in for the real payload\n")
    (payload / "install.sh").write_text("#!/bin/sh\nexit 0\n")

    home = tmp_path / "home"
    home.mkdir()

    shims = tmp_path / "shims"
    shims.mkdir()
    for name in ("sudo", "chown"):
        _write_exec(shims / name, LOGGING_SHIM)
    _write_exec(shims / "getent", GETENT_SHIM)
    _write_exec(shims / "id", ID_SHIM)
    _write_exec(shims / "logname", LOGNAME_SHIM)
    _write_exec(shims / "loginctl", LOGINCTL_SHIM)

    log = tmp_path / "shim.log"
    log.write_text("")

    env = dict(os.environ)
    env.update(
        PATH=f"{shims}:/usr/bin:/bin",
        SHIM_LOG=str(log),
        HUSHKEY_PAYLOAD=str(payload),
        FAKE_HOME=str(home),
    )
    for stale in ("SUDO_USER", "PKEXEC_UID", "FAKE_LOGNAME", "FAKE_SESSIONS"):
        env.pop(stale, None)
    return {"env": env, "home": home, "log": log}


def run_postinst(sandbox, **overrides):
    env = dict(sandbox["env"])
    env.update({k: str(v) for k, v in overrides.items()})
    proc = subprocess.run(["sh", POSTINST], env=env,
                          capture_output=True, text=True, timeout=60)
    return proc, sandbox["log"].read_text()


def _dest(sandbox):
    return sandbox["home"] / ".local/share/whisper-ptt"


def test_sudo_install_uses_sudo_user(sandbox):
    proc, log = run_postinst(sandbox, SUDO_USER="alice", FAKE_USER="alice")
    assert proc.returncode == 0
    assert (_dest(sandbox) / "dictate.py").exists(), "payload was not copied"
    assert "sudo -u alice" in log


def test_graphical_install_falls_back_to_pkexec_uid(sandbox):
    """pkexec states the caller in PKEXEC_UID, never in SUDO_USER."""
    proc, log = run_postinst(sandbox, PKEXEC_UID=4242, FAKE_USER="bob")
    assert proc.returncode == 0
    assert (_dest(sandbox) / "dictate.py").exists(), \
        "GUI install copied nothing — the original bug"
    assert "sudo -u bob" in log


def test_packagekit_install_falls_back_to_the_logged_in_user(sandbox):
    """PackageKit leaves no caller identity at all; the desktop session does."""
    proc, log = run_postinst(
        sandbox, FAKE_USER="carol",
        FAKE_SESSIONS="3|carol|user|wayland")
    assert proc.returncode == 0
    assert (_dest(sandbox) / "dictate.py").exists()
    assert "sudo -u carol" in log


def test_ambiguous_sessions_install_nothing_rather_than_guess(sandbox):
    """Two logged-in users: installing into either home would be a coin flip."""
    proc, log = run_postinst(
        sandbox, FAKE_USER="carol",
        FAKE_SESSIONS="3|carol|user|wayland\n5|dave|user|x11")
    assert proc.returncode == 0, "a user-level step must never fail the package"
    assert not _dest(sandbox).exists()
    assert "finish setup as your desktop user" in proc.stdout
    assert "sudo -u" not in log


def test_greeter_sessions_are_ignored(sandbox):
    """A hanging gdm greeter session must not create false ambiguity."""
    proc, log = run_postinst(
        sandbox, FAKE_USER="carol",
        FAKE_SESSIONS="c1|gdm|greeter|wayland\n3|carol|user|wayland")
    assert proc.returncode == 0
    assert (_dest(sandbox) / "dictate.py").exists()
    assert "sudo -u carol" in log


def test_only_a_greeter_session_installs_nothing(sandbox):
    """A lone greeter is not a user — installing into gdm's home would be wrong."""
    proc, log = run_postinst(
        sandbox, FAKE_USER="carol",
        FAKE_SESSIONS="c1|gdm|greeter|wayland")
    assert proc.returncode == 0
    assert not _dest(sandbox).exists()
    assert "finish setup as your desktop user" in proc.stdout


def test_same_user_with_two_sessions_is_unambiguous(sandbox):
    """One user, two graphical sessions (e.g. switched seats): still one home."""
    proc, log = run_postinst(
        sandbox, FAKE_USER="carol",
        FAKE_SESSIONS="3|carol|user|wayland\n7|carol|user|x11")
    assert proc.returncode == 0
    assert (_dest(sandbox) / "dictate.py").exists()
    assert "sudo -u carol" in log


def test_root_only_environment_still_prints_the_manual_hint(sandbox):
    """No sudo, no pkexec, no terminal, no session — the honest answer."""
    proc, log = run_postinst(sandbox, SUDO_USER="root")
    assert proc.returncode == 0
    assert not _dest(sandbox).exists()
    assert "finish setup as your desktop user" in proc.stdout
    assert "sudo -u" not in log
