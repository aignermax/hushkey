"""Run the packaged desktop launcher without a real system bus or privileges."""
import os
from pathlib import Path
import shutil
import subprocess

import pytest

pytestmark = pytest.mark.skipif(os.name != "posix", reason="Linux launcher")
ROOT = Path(__file__).resolve().parent.parent


@pytest.mark.parametrize("configured", [False, True])
def test_desktop_launch_completes_deferred_setup(tmp_path, configured):
    payload = tmp_path / "payload"
    payload.mkdir()
    shutil.copy(ROOT / "packaging/linux/launch.sh", payload)
    (payload / ".release-version").write_text("0.9.0")
    home = tmp_path / "home"
    home.mkdir()
    if configured:
        dest = home / ".local/share/whisper-ptt"
        dest.mkdir(parents=True)
        (dest / ".setup-version").write_text("0.9.0")
    shims = tmp_path / "shims"
    shims.mkdir()
    log = tmp_path / "log"
    for name in ("pkexec", "systemctl"):
        path = shims / name
        path.write_text('#!/bin/sh\nprintf "%s\\n" "$0 $*" >> "$SHIM_LOG"\n')
        path.chmod(0o755)
    env = dict(os.environ, HOME=str(home), PATH=f"{shims}:/usr/bin:/bin", SHIM_LOG=str(log))
    proc = subprocess.run(["bash", str(payload / "launch.sh")], env=env, capture_output=True, text=True)
    assert proc.returncode == 0, proc.stderr
    calls = log.read_text()
    assert ("pkexec" in calls) == (not configured)
    assert "systemctl --user restart whisper-ptt.service" in calls
