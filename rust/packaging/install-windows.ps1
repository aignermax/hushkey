# Install a locally built hushkey (Rust) for trying it out, next to the
# Python hushkey: its own folder, its own settings, a Start Menu entry
# "hushkey (Rust)", no autostart.
#
#   powershell -ExecutionPolicy Bypass -File install-windows.ps1 -Exe <path to hushkey.exe> [-Uninstall]
[CmdletBinding()]
param(
    [string]$Exe,
    [switch]$Uninstall
)

$ErrorActionPreference = "Stop"
$Dir = Join-Path $env:LOCALAPPDATA "Programs\hushkey-rs"
$MenuLnk = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\hushkey (Rust).lnk"

# A running copy keeps hushkey.exe locked. CIM, as Get-Process often
# leaves Path empty for a process started elsewhere.
Get-CimInstance Win32_Process -Filter "Name = 'hushkey.exe'" |
    Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($Dir, [StringComparison]::OrdinalIgnoreCase) } |
    ForEach-Object {
        $process = Get-Process -Id $_.ProcessId -ErrorAction SilentlyContinue
        if ($process) {
            $process.Kill()
            $process.WaitForExit(5000) | Out-Null  # the file stays locked until it has exited
            Write-Host "stopped the running hushkey (Rust)"
        }
    }

if ($Uninstall) {
    Remove-Item -LiteralPath $MenuLnk -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $Dir -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "hushkey (Rust) removed; settings and log stay in $env:LOCALAPPDATA\hushkey-rs"
    return
}

if (-not $Exe -or -not (Test-Path -LiteralPath $Exe)) {
    throw "pass -Exe with the built hushkey.exe (e.g. target\release\hushkey.exe)"
}
New-Item -ItemType Directory -Force $Dir | Out-Null
Copy-Item -LiteralPath $Exe -Destination (Join-Path $Dir "hushkey.exe") -Force

$Shell = New-Object -ComObject WScript.Shell
$Lnk = $Shell.CreateShortcut($MenuLnk)
$Lnk.TargetPath = Join-Path $Dir "hushkey.exe"
$Lnk.WorkingDirectory = $Dir
$Lnk.Description = "hushkey on the Rust core (trial)"
$Lnk.Save()

Write-Host "installed: $Dir\hushkey.exe"
Write-Host "Start Menu: hushkey (Rust)"
Write-Host "settings:   $env:LOCALAPPDATA\hushkey-rs\config.json (most are in the tray menu)"
