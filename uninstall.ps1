# Remove whisper-ptt autostart (scheduled task or Startup shortcut), stop the
# daemon and (optionally) remove the venv.
# Usage:  powershell -ExecutionPolicy Bypass -File uninstall.ps1 [-Purge]
[CmdletBinding()]
param(
    [switch]$Purge  # also remove the .venv
)

$Dir = Split-Path -Parent $MyInvocation.MyCommand.Path
$TaskName = "whisper-ptt"

schtasks /query /tn $TaskName 2>$null | Out-Null
if ($LASTEXITCODE -eq 0) {
    schtasks /end /tn $TaskName 2>$null | Out-Null
    schtasks /delete /f /tn $TaskName | Out-Null
    Write-Host "scheduled task '$TaskName' removed"
} else {
    Write-Host "no scheduled task '$TaskName' found"
}

$LnkPath = Join-Path ([Environment]::GetFolderPath("Startup")) "whisper-ptt.lnk"
if (Test-Path $LnkPath) {
    Remove-Item $LnkPath -Force
    Write-Host "startup shortcut removed"
}

# Stop tray + daemon started from this checkout (no-op if none is running)
Get-CimInstance Win32_Process -Filter "Name = 'pythonw.exe' OR Name = 'python.exe'" |
    Where-Object { $_.CommandLine -and $_.CommandLine.Contains("$Dir") -and ($_.CommandLine.Contains("tray.py") -or $_.CommandLine.Contains("dictate.py")) } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force; Write-Host "daemon stopped (PID $($_.ProcessId))" }

if ($Purge) {
    Remove-Item -Recurse -Force (Join-Path $Dir ".venv") -ErrorAction SilentlyContinue
    Write-Host "venv removed"
}
