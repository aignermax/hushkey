"""Execute the Windows installer with a real venv and offline pip boundary."""
import os
from pathlib import Path
import shutil
import subprocess
import sys

import pytest

pytestmark = pytest.mark.skipif(os.name != "nt", reason="Windows installer")
ROOT = Path(__file__).resolve().parent.parent


@pytest.mark.parametrize("setup_code", [0, 37])
def test_setup_runs_and_failures_propagate_without_autostart(tmp_path, setup_code):
    shutil.copy(ROOT / "install.ps1", tmp_path)
    (tmp_path / "dictate.py").write_text("")
    (tmp_path / "pip.py").write_text("# Offline dependency installation boundary\n")
    (tmp_path / "setup_acceleration.py").write_text(
        "from pathlib import Path\n"
        "Path(__file__).with_name('setup-ran').write_text('yes')\n"
        f"raise SystemExit({setup_code})\n")
    subprocess.run([sys.executable, "-m", "venv", "--without-pip", str(tmp_path / ".venv")], check=True)
    env = dict(os.environ, PYTHONPATH=str(tmp_path))
    proc = subprocess.run([
        shutil.which("powershell"), "-NoProfile", "-ExecutionPolicy", "Bypass",
        "-File", str(tmp_path / "install.ps1"), "-NoAutostart"],
        env=env, text=True, capture_output=True, timeout=60)
    assert (tmp_path / "setup-ran").exists(), proc.stdout + proc.stderr
    assert (proc.returncode == 0) == (setup_code == 0)
    assert "autostart:" not in proc.stdout
