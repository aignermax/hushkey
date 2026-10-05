# whisper-ptt installer for Windows: venv + dependencies + autostart task.
# Idempotent - safe to re-run (e.g. after git pull).
# Usage:  powershell -ExecutionPolicy Bypass -File install.ps1 [-NoAutostart]
[CmdletBinding()]
param(
    [switch]$NoAutostart,  # skip the Task Scheduler entry (manual start only)
    [string]$LogPath      # setup.exe saves its hidden console output here
)

$ErrorActionPreference = "Stop"
if ($LogPath) { Start-Transcript -Path $LogPath -Append | Out-Null }

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
        Copy-Item (Join-Path $tmp "hushkey-master\*") $Dir -Recurse -Force
        Remove-Item $tmp, $zip -Recurse -Force
    }
}
$Venv = Join-Path $Dir ".venv"
$TaskName = "whisper-ptt"

function Find-CompatiblePython {
    # Store aliases can open a store window; never execute them as probes.
    $candidates = @()
    foreach ($name in @("python3.12", "python", "python3")) {
        $candidates += @(Get-Command $name -All -CommandType Application -ErrorAction SilentlyContinue |
            Where-Object { $_.Source -notmatch '\\WindowsApps\\' } |
            ForEach-Object { $_.Source })
    }
    $candidates += @(Get-ChildItem "$env:LOCALAPPDATA\Programs\Python\Python3*\python.exe" -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending | ForEach-Object { $_.FullName })
    $launcher = Get-Command py -CommandType Application -ErrorAction SilentlyContinue
    if ($launcher -and $launcher.Source -notmatch '\\WindowsApps\\') {
        foreach ($version in @("-3.12", "-3.13", "-3.11", "-3.10")) {
            try {
                $path = & $launcher.Source $version -c "import sys; print(sys.executable)" 2>$null
                if ($LASTEXITCODE -eq 0) { $candidates += $path }
            } catch { }
        }
    }
    foreach ($candidate in $candidates | Select-Object -Unique) {
        try {
            $valid = & $candidate -c "import sys,struct; print((3,10) <= sys.version_info < (3,14) and struct.calcsize('P') == 8)" 2>$null
            if ($LASTEXITCODE -eq 0 -and $valid -eq "True") { return $candidate }
        } catch { }
    }
    return $null
}

Write-Host "==> checking prerequisites"
$Python = Find-CompatiblePython
if (-not $Python) {
    Write-Host "==> installing Python 3.12"
    $winget = Get-Command winget -ErrorAction SilentlyContinue
    if ($winget) {
        & $winget.Source install -e --id Python.Python.3.12 --scope user --silent --accept-source-agreements --accept-package-agreements
        $Python = Find-CompatiblePython
    }
    if (-not $Python) {
        $installer = Join-Path $env:TEMP ("hushkey-python-" + [guid]::NewGuid().ToString("N") + ".exe")
        try {
            Invoke-WebRequest -UseBasicParsing https://www.python.org/ftp/python/3.12.10/python-3.12.10-amd64.exe -OutFile $installer
            if ((Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash -ne "67b5635e80ea51072b87941312d00ec8927c4db9ba18938f7ad2d27b328b95fb") {
                throw "Python download checksum mismatch"
            }
            $result = Start-Process -FilePath $installer -ArgumentList "/quiet InstallAllUsers=0 PrependPath=1 Include_test=0" -Wait -PassThru -WindowStyle Hidden
            if ($result.ExitCode -notin @(0, 3010)) { throw "Python installer failed: $($result.ExitCode)" }
        } finally {
            Remove-Item -LiteralPath $installer -Force -ErrorAction SilentlyContinue
        }
        $Python = Find-CompatiblePython
    }
    if (-not $Python) { throw "Compatible 64-bit Python could not be installed." }
}
$PyArgs = @()

$VenvPython = Join-Path $Venv "Scripts\python.exe"
Write-Host "==> creating venv at $Venv"
$needVenv = -not (Test-Path $VenvPython)
if (-not $needVenv) {
    # exists — but is it functional? (a removed/upgraded base Python breaks it)
    & $VenvPython -c "pass" 2>$null
    $needVenv = ($LASTEXITCODE -ne 0)
}
# only (re)create when needed: recreation copies fresh launchers over running
# ones, which fails while a tray/daemon/update-helper is using them
if ($needVenv) {
    & $Python @PyArgs -m venv $Venv
    if ($LASTEXITCODE -ne 0) { Write-Error "venv creation failed"; exit 1 }
}
& $VenvPython -m pip install -q --upgrade pip
if ($LASTEXITCODE -ne 0) { throw "pip upgrade failed" }

Write-Host "==> installing python dependencies"
& $VenvPython -m pip install -q -r (Join-Path $Dir "requirements.txt")
if ($LASTEXITCODE -ne 0) { Write-Error "dependency install failed"; exit 1 }
Write-Host "==> configuring automatic hardware acceleration"
& $VenvPython (Join-Path $Dir "setup_acceleration.py")
if ($LASTEXITCODE -ne 0) { throw "Automatic acceleration setup failed ($LASTEXITCODE)" }

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
Write-Host "Config: setx PTT_KEY f9 (also WHISPER_LANG) - applies at next logon or daemon restart."
Write-Host "        the whisper model is easiest picked in the tray menu (or setx WHISPER_MODEL)."
Write-Host "Logs:   $env:LOCALAPPDATA\whisper-ptt\dictate.log"
Write-Host "Remove: powershell -ExecutionPolicy Bypass -File uninstall.ps1"
