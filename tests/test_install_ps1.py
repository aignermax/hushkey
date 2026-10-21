"""Execute the Windows installer with a real venv and offline pip boundary."""
import os
from pathlib import Path
import shutil
import subprocess
import sys

import pytest

pytestmark = pytest.mark.skipif(os.name != "nt", reason="Windows installer")
ROOT = Path(__file__).resolve().parent.parent


def ps_quote(value):
    return "'" + str(value).replace("'", "''") + "'"


START_MENU = Path("Microsoft/Windows/Start Menu/Programs")


def installer_env(tmp_path):
    """Keep the installer's Start Menu entry out of the real user profile."""
    appdata = tmp_path / "appdata"
    (appdata / START_MENU).mkdir(parents=True)
    return dict(os.environ, PYTHONPATH=str(tmp_path), APPDATA=str(appdata))


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
    env = installer_env(tmp_path)
    proc = subprocess.run([
        shutil.which("powershell"), "-NoProfile", "-ExecutionPolicy", "Bypass",
        "-File", str(tmp_path / "install.ps1"), "-NoAutostart"],
        env=env, text=True, capture_output=True, timeout=60)
    assert (tmp_path / "setup-ran").exists(), proc.stdout + proc.stderr
    assert (proc.returncode == 0) == (setup_code == 0)
    assert "autostart:" not in proc.stdout


@pytest.mark.parametrize("valid_hash", [False, True])
def test_fresh_machine_bootstrap_verifies_before_running_python_installer(tmp_path, valid_hash):
    shutil.copy(ROOT / "install.ps1", tmp_path)
    (tmp_path / "dictate.py").write_text("")
    (tmp_path / "pip.py").write_text("")
    (tmp_path / "setup_acceleration.py").write_text("from pathlib import Path\nPath(__file__).with_name('configured').touch()\n")
    subprocess.run([sys.executable, "-m", "venv", "--without-pip", str(tmp_path / "ready-venv")], check=True)
    digest = "67b5635e80ea51072b87941312d00ec8927c4db9ba18938f7ad2d27b328b95fb" if valid_hash else "bad-download"
    wrapper = tmp_path / "run.ps1"
    wrapper.write_text(f"""
$global:bootstrapped = $false
function Get-Command {{ param($Name, [switch]$All, $CommandType, $ErrorAction) return $null }}
function Get-ChildItem {{
  param($Path, $ErrorAction)
  if ($global:bootstrapped) {{ [pscustomobject]@{{ FullName = {ps_quote(sys.executable)} }} }}
}}
function Invoke-WebRequest {{
  param([switch]$UseBasicParsing, $Uri, $OutFile)
  if ($Uri -notlike 'https://www.python.org/*') {{ throw 'unexpected download' }}
  [IO.File]::WriteAllText($OutFile, 'offline installer boundary')
}}
function Get-FileHash {{ param($LiteralPath, $Algorithm) [pscustomobject]@{{ Hash = '{digest}' }} }}
function Start-Process {{
  param($FilePath, $ArgumentList, [switch]$Wait, [switch]$PassThru, $WindowStyle)
  [IO.File]::WriteAllText({ps_quote(tmp_path / 'installer-ran')}, $ArgumentList)
  Move-Item {ps_quote(tmp_path / 'ready-venv')} {ps_quote(tmp_path / '.venv')}
  $global:bootstrapped = $true
  [pscustomobject]@{{ ExitCode = 0 }}
}}
& {ps_quote(tmp_path / 'install.ps1')} -NoAutostart
""", encoding="utf-8")
    proc = subprocess.run([shutil.which("powershell"), "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(wrapper)],
                          env=installer_env(tmp_path), text=True, capture_output=True, timeout=60)
    assert (tmp_path / "installer-ran").exists() == valid_hash, proc.stdout + proc.stderr
    assert (tmp_path / "configured").exists() == valid_hash, proc.stdout + proc.stderr
    assert (proc.returncode == 0) == valid_hash


def test_existing_venv_needs_no_global_python_or_bootstrap(tmp_path):
    shutil.copy(ROOT / "install.ps1", tmp_path)
    (tmp_path / "dictate.py").write_text("")
    (tmp_path / "pip.py").write_text("")
    (tmp_path / "setup_acceleration.py").write_text("from pathlib import Path\nPath(__file__).with_name('configured').touch()\n")
    subprocess.run([sys.executable, "-m", "venv", "--without-pip", str(tmp_path / ".venv")], check=True)
    before = (tmp_path / ".venv/pyvenv.cfg").read_bytes()
    wrapper = tmp_path / "run.ps1"
    wrapper.write_text(f"""
function Get-Command {{ param($Name, [switch]$All, $CommandType, $ErrorAction) return $null }}
function Get-ChildItem {{ param($Path, $ErrorAction) return $null }}
function Invoke-WebRequest {{ throw 'must not bootstrap Python during an update' }}
& {ps_quote(tmp_path / 'install.ps1')} -NoAutostart
""", encoding="utf-8")
    proc = subprocess.run([shutil.which("powershell"), "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(wrapper)],
                          env=installer_env(tmp_path), text=True, capture_output=True, timeout=60)
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert (tmp_path / "configured").exists()
    assert (tmp_path / ".venv/pyvenv.cfg").read_bytes() == before


def test_python_install_manager_runtime_is_discovered(tmp_path):
    # Exercise the actual function while keeping global machine discovery out.
    wrapper = tmp_path / "find.ps1"
    wrapper.write_text(f"""
$tokens = $null; $errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile({ps_quote(ROOT / 'install.ps1')}, [ref]$tokens, [ref]$errors)
$fn = $ast.Find({{param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq 'Find-CompatiblePython'}}, $true)
Invoke-Expression $fn.Extent.Text
function Get-Command {{ param($Name, [switch]$All, $CommandType, $ErrorAction) return $null }}
function Get-ChildItem {{
  param($Path, $ErrorAction)
  if ($Path -like '*\\Python\\pythoncore-*\\python.exe') {{ [pscustomobject]@{{ FullName = {ps_quote(sys.executable)} }} }}
}}
$found = Find-CompatiblePython
if ($found -ne {ps_quote(sys.executable)}) {{ throw 'manager runtime not discovered' }}
""", encoding="utf-8")
    proc = subprocess.run([shutil.which("powershell"), "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(wrapper)],
                          text=True, capture_output=True, timeout=60)
    assert proc.returncode == 0, proc.stdout + proc.stderr


def read_shortcut(lnk):
    # UTF-8 on both ends: a non-ASCII user profile must not garble the paths.
    proc = subprocess.run([shutil.which("powershell"), "-NoProfile", "-Command",
                           "[Console]::OutputEncoding = [Text.Encoding]::UTF8; "
                           f"$l = (New-Object -ComObject WScript.Shell).CreateShortcut({ps_quote(lnk)}); "
                           "$l.TargetPath; $l.Arguments"],
                          capture_output=True, timeout=60)
    lines = proc.stdout.decode("utf-8").splitlines()
    assert len(lines) == 2, proc.stdout + proc.stderr
    return lines


def test_start_menu_entry_launches_the_tray_and_only_its_uninstall_removes_it(tmp_path):
    """Another checkout's uninstall (or a test of it) must not take the entry
    of the real install along."""
    install, other = tmp_path / "install", tmp_path / "other"
    install.mkdir()
    other.mkdir()
    shutil.copy(ROOT / "install.ps1", install)
    for target in (install, other):
        shutil.copy(ROOT / "uninstall.ps1", target)
    (install / "dictate.py").write_text("")
    (install / "pip.py").write_text("")
    (install / "setup_acceleration.py").write_text("")
    subprocess.run([sys.executable, "-m", "venv", "--without-pip", str(install / ".venv")], check=True)
    env = installer_env(tmp_path)
    env["PYTHONPATH"] = str(install)
    powershell = [shutil.which("powershell"), "-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]
    proc = subprocess.run([*powershell, str(install / "install.ps1"), "-NoAutostart"],
                          env=env, text=True, capture_output=True, timeout=60)
    assert proc.returncode == 0, proc.stdout + proc.stderr
    lnk = Path(env["APPDATA"]) / START_MENU / "hushkey.lnk"
    target, arguments = read_shortcut(lnk)
    assert target.lower().endswith(r"\.venv\scripts\pythonw.exe")
    assert arguments == f'"{install / "tray.py"}"'
    for checkout, kept in ((other, True), (install, False)):
        proc = subprocess.run([*powershell, str(checkout / "uninstall.ps1")],
                              env=env, text=True, capture_output=True, timeout=60)
        assert proc.returncode == 0, proc.stdout + proc.stderr
        assert lnk.exists() == kept, proc.stdout
