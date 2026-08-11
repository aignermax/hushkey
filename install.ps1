# whisper-ptt installer for Windows: venv + dependencies + autostart task.
# Idempotent - safe to re-run (e.g. after git pull).
# Usage:  powershell -ExecutionPolicy Bypass -File install.ps1 [-NoAutostart]
[CmdletBinding()]
param(
    [switch]$NoAutostart  # skip the Task Scheduler entry (manual start only)
)

$ErrorActionPreference = "Stop"

# One-liner install straight from the web (no script path when piped via iex):
#   irm https://raw.githubusercontent.com/aignermax/hushkey/master/install.ps1 | iex
$Repo = "https://github.com/aignermax/hushkey"
$Dir = $PSScriptRoot
if (-not $Dir -or -not (Test-Path (Join-Path $Dir "dictate.py"))) {
    $Dir = Join-Path ([Environment]::GetFolderPath("LocalApplicationData")) "Programs\whisper-ptt"
    Write-Host "==> fetching whisper-ptt into $Dir"
    $hasGit = [bool](Get-Command git -ErrorAction SilentlyContinue)
    if ((Test-Path (Join-Path $Dir ".git")) -and $hasGit) {
        git -C $Dir pull --ff-only
    } elseif ($hasGit -and -not (Test-Path $Dir)) {
        git clone "$Repo.git" $Dir
    } else {
        # No git (or a zip install already present): plain download works too.
        $zip = Join-Path $env:TEMP "whisper-ptt-master.zip"
        $tmp = Join-Path $env:TEMP ("whisper-ptt-" + [guid]::NewGuid().ToString("N"))
        Invoke-WebRequest -UseBasicParsing "$Repo/archive/refs/heads/master.zip" -OutFile $zip
        Expand-Archive $zip -DestinationPath $tmp
        New-Item -ItemType Directory -Force $Dir | Out-Null
        Copy-Item (Join-Path $tmp "whisper-ptt-master\*") $Dir -Recurse -Force
        Remove-Item $tmp, $zip -Recurse -Force
    }
}
$Venv = Join-Path $Dir ".venv"
$TaskName = "whisper-ptt"

Write-Host "==> checking prerequisites"
if (Get-Command python -ErrorAction SilentlyContinue) {
    $Python = "python"; $PyArgs = @()
} elseif (Get-Command py -ErrorAction SilentlyContinue) {
    $Python = "py"; $PyArgs = @("-3")
} else {
    Write-Error "Python 3 not found. Install it from https://www.python.org/downloads/ (tick 'Add python.exe to PATH')."
    exit 1
}
$verOk = & $Python @PyArgs -c "import sys; print(sys.version_info >= (3, 10))" 2>$null
if ($verOk -ne "True") {
    Write-Error "Python 3.10+ required (if the Microsoft Store just opened, install real Python from python.org first)."
    exit 1
}

Write-Host "==> creating venv at $Venv"
& $Python @PyArgs -m venv $Venv
if ($LASTEXITCODE -ne 0) { Write-Error "venv creation failed"; exit 1 }
$VenvPython = Join-Path $Venv "Scripts\python.exe"
& $VenvPython -m pip install -q --upgrade pip

Write-Host "==> installing python dependencies"
& $VenvPython -m pip install -q -r (Join-Path $Dir "requirements.txt")
if ($LASTEXITCODE -ne 0) { Write-Error "dependency install failed"; exit 1 }
if (Get-Command nvidia-smi -ErrorAction SilentlyContinue) {
    Write-Host "==> NVIDIA GPU detected - installing CUDA libraries"
    & $VenvPython -m pip install -q -r (Join-Path $Dir "requirements-gpu.txt")
} else {
    Write-Host "==> no NVIDIA GPU - CPU mode (works fine, just slower)"
}

if (-not $NoAutostart) {
    $Pythonw = Join-Path $Venv "Scripts\pythonw.exe"  # no console window
    $Daemon = Join-Path $Dir "tray.py"  # tray supervises dictate.py as its child
    $Tr = "`"$Pythonw`" `"$Daemon`""
    # native stderr would abort the script under EAP=Stop, so relax it locally
    $ErrorActionPreference = "Continue"
    $null = schtasks /create /f /tn $TaskName /sc onlogon /delay 0000:30 /tr $Tr 2>&1
    $taskOk = ($LASTEXITCODE -eq 0)
    $ErrorActionPreference = "Stop"
    if ($taskOk) {
        Write-Host "==> autostart: scheduled task '$TaskName' (starts at every logon)"
        schtasks /run /tn $TaskName | Out-Null  # start now, no relogin needed
    } else {
        # No rights for Task Scheduler (e.g. restricted account): Startup folder
        # shortcut works without any special permissions.
        $LnkPath = Join-Path ([Environment]::GetFolderPath("Startup")) "whisper-ptt.lnk"
        Write-Host "==> schtasks denied - autostart via Startup folder shortcut instead:"
        Write-Host "    $LnkPath"
        $Ws = New-Object -ComObject WScript.Shell
        $Lnk = $Ws.CreateShortcut($LnkPath)
        $Lnk.TargetPath = $Pythonw
        $Lnk.Arguments = "`"$Daemon`""
        $Lnk.WorkingDirectory = $Dir
        $Lnk.Save()
        Start-Process $Pythonw -ArgumentList "`"$Daemon`"" -WindowStyle Hidden
    }
}

Write-Host ""
Write-Host "Done. Hold Right Ctrl in any window, speak, release - text gets typed."
Write-Host "First dictation downloads the whisper model (~0.5-1.5 GB), then it is offline."
Write-Host "Config: setx PTT_KEY f9 (also WHISPER_MODEL / WHISPER_LANG) - applies at next logon or daemon restart."
Write-Host "Logs:   $env:LOCALAPPDATA\whisper-ptt\dictate.log"
Write-Host "Remove: powershell -ExecutionPolicy Bypass -File uninstall.ps1"
