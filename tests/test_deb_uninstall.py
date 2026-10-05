"""PackageKit removal resolves managed homes even without SUDO_USER."""
import os
from pathlib import Path
import subprocess

import pytest

pytestmark = pytest.mark.skipif(os.name != "posix", reason="Linux maintainer scripts")
ROOT = Path(__file__).resolve().parent.parent


def test_graphical_removal_stops_services_and_purge_removes_managed_data(tmp_path):
    home = tmp_path / "home"
    dest = home / ".local/share/whisper-ptt"
    dest.mkdir(parents=True)
    (dest / ".deb-managed").touch()
    native = home / ".local/share/hushkey/native"
    native.mkdir(parents=True)
    (native / "engine").touch()
    shims = tmp_path / "shims"
    shims.mkdir()
    log = tmp_path / "log"
    commands = {
        "getent": '#!/bin/sh\nprintf "tester:x:4242:4242::%s:/bin/bash\\n" "$FAKE_HOME"\n',
        "logname": "#!/bin/sh\nexit 1\n",
        "id": "#!/bin/sh\necho 4242\n",
        "sudo": '#!/bin/sh\nshift 2\nexec env "$@"\n',
        "systemctl": '#!/bin/sh\necho "$*" >> "$SHIM_LOG"\n',
        "rm": '#!/bin/sh\ncase "$*" in */etc/*) exit 0;; esac\nexec /bin/rm "$@"\n',
    }
    for name, code in commands.items():
        path = shims / name
        path.write_text(code)
        path.chmod(0o755)
    env = dict(os.environ, HOME=str(home), FAKE_HOME=str(home), PATH=f"{shims}:/usr/bin:/bin", SHIM_LOG=str(log))
    env.pop("SUDO_USER", None)
    for script, arg in (("prerm", "remove"), ("postrm", "remove")):
        proc = subprocess.run(["sh", str(ROOT / "packaging/linux" / script), arg], env=env, capture_output=True, text=True)
        assert proc.returncode == 0, proc.stderr
    assert "disable --now whisper-ptt.service" in log.read_text()
    assert (native / "engine").exists(), "remove must preserve user data"
    proc = subprocess.run(["sh", str(ROOT / "packaging/linux/postrm"), "purge"], env=env, capture_output=True, text=True)
    assert proc.returncode == 0, proc.stderr
    assert not dest.exists() and not native.exists()
