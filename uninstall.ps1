# Remove hushkey: autostart, Start Menu entry, its entry in Settings > Apps
# and the program. Settings and log stay in %LOCALAPPDATA%\hushkey-rs.
#
# -Purge is how the Python hushkey's own uninstaller (Inno Setup, still
# listed in Settings > Apps as "hushkey - old Python version") calls the copy
# of this file in its folder. That entry removes the old version's files and
# nothing else: the native hushkey has its own entry.
param([switch]$Purge)
$ErrorActionPreference = "Stop"

if ($Purge) {
    # Inno Setup deletes the files it installed after this; the virtual
    # environment the old installer created is not among them.
    $venv = Join-Path $PSScriptRoot ".venv"
    if ((Split-Path -Leaf $PSScriptRoot) -eq "whisper-ptt" -and (Test-Path $venv)) {
        Get-CimInstance Win32_Process -Filter "Name like 'python%'" |
            Where-Object { $_.CommandLine -match 'whisper-ptt' } |
            ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
        Remove-Item -Recurse -Force $venv -ErrorAction SilentlyContinue
    }
    # Its autostart, should it never have handed over.
    # (cmd's redirection: in Windows PowerShell 5.1 a native program's
    # stderr would be an error under "Stop" — the task is usually gone.)
    $null = cmd.exe /d /c "schtasks /Delete /F /TN whisper-ptt >nul 2>&1"
    $startup = Join-Path ([Environment]::GetFolderPath("Startup")) "whisper-ptt.lnk"
    Remove-Item $startup -Force -ErrorAction SilentlyContinue
    # Its Start Menu entry — only while it still points into the old folder:
    # after the handover the same name is the native hushkey's.
    $menu = Join-Path ([Environment]::GetFolderPath("Programs")) "hushkey.lnk"
    if (Test-Path $menu) {
        $target = (New-Object -ComObject WScript.Shell).CreateShortcut($menu).TargetPath
        if ($target -and $target.StartsWith($PSScriptRoot, [StringComparison]::OrdinalIgnoreCase)) {
            Remove-Item $menu -Force -ErrorAction SilentlyContinue
        }
    }
    Write-Host "old hushkey files removed; the native hushkey is not affected"
    exit 0
}

$Exe = Join-Path $env:LOCALAPPDATA "Programs\hushkey\hushkey.exe"
if (-not (Test-Path $Exe)) { Write-Host "hushkey is not installed"; exit 0 }
# The program cannot delete itself while running; uninstall from a copy.
$Copy = Join-Path $env:TEMP "hushkey-uninstall.exe"
Copy-Item $Exe $Copy -Force
$p = Start-Process -FilePath $Copy -ArgumentList "--uninstall" -PassThru
# Without the handle cached now, Windows PowerShell 5.1 can report no exit
# code for a process that ended before it looked.
$null = $p.Handle
$p.WaitForExit()
Remove-Item $Copy -Force -ErrorAction SilentlyContinue
if ($p.ExitCode -ne 0) { throw "hushkey --uninstall failed (exit $($p.ExitCode))" }
Remove-Item (Split-Path $Exe) -Recurse -Force -ErrorAction SilentlyContinue
Write-Host "hushkey removed"
