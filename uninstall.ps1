# Remove whisper-ptt autostart (scheduled task or Startup shortcut), the Start
# Menu entry, stop the daemon and (optionally) remove the venv.
# Usage:  powershell -ExecutionPolicy Bypass -File uninstall.ps1 [-Purge]
[CmdletBinding()]
param(
    [switch]$Purge  # also remove the .venv
)

$Dir = Split-Path -Parent $MyInvocation.MyCommand.Path
$TaskName = "whisper-ptt"

# Entries are per user, not per install: only remove the ones that start THIS
# install, so uninstalling a second checkout leaves the real one working.
function Test-OwnsCommand([string]$Text) {
    return $Text -and $Text.IndexOf($Dir, [StringComparison]::OrdinalIgnoreCase) -ge 0
}

$TaskXml = (schtasks /query /tn $TaskName /xml 2>$null) -join "`n"
if ($LASTEXITCODE -ne 0) {
    Write-Host "no scheduled task '$TaskName' found"
} elseif (Test-OwnsCommand $TaskXml) {
    schtasks /end /tn $TaskName 2>$null | Out-Null
    schtasks /delete /f /tn $TaskName | Out-Null
    Write-Host "scheduled task '$TaskName' removed"
} else {
    Write-Host "scheduled task '$TaskName' starts another install - kept"
}

$Shell = New-Object -ComObject WScript.Shell
foreach ($Lnk in @(
        @{ Path = Join-Path ([Environment]::GetFolderPath("Startup")) "whisper-ptt.lnk"; Name = "startup shortcut" },
        @{ Path = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\hushkey.lnk"; Name = "Start Menu entry" })) {
    if (-not (Test-Path -LiteralPath $Lnk.Path)) { continue }
    if (Test-OwnsCommand $Shell.CreateShortcut($Lnk.Path).Arguments) {
        Remove-Item -LiteralPath $Lnk.Path -Force
        Write-Host "$($Lnk.Name) removed"
    } else {
        Write-Host "$($Lnk.Name) starts another install - kept"
    }
}

# Stop tray + daemon started from this checkout (no-op if none is running)
Get-CimInstance Win32_Process -Filter "Name = 'pythonw.exe' OR Name = 'python.exe'" |
    Where-Object { $_.CommandLine -and $_.CommandLine.Contains("$Dir") -and ($_.CommandLine.Contains("tray.py") -or $_.CommandLine.Contains("dictate.py")) } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force; Write-Host "daemon stopped (PID $($_.ProcessId))" }

if ($Purge) {
    Remove-Item -Recurse -Force (Join-Path $Dir ".venv") -ErrorAction SilentlyContinue
    Write-Host "venv removed"
}
