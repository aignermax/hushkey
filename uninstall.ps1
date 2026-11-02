# Remove hushkey: autostart, Start Menu entry and program. Settings and log
# stay in %LOCALAPPDATA%\hushkey-rs.
$ErrorActionPreference = "Stop"
$Exe = Join-Path $env:LOCALAPPDATA "Programs\hushkey\hushkey.exe"
if (-not (Test-Path $Exe)) { Write-Host "hushkey is not installed"; exit 0 }
# The program cannot delete itself while running; uninstall from a copy.
$Copy = Join-Path $env:TEMP "hushkey-uninstall.exe"
Copy-Item $Exe $Copy -Force
$p = Start-Process -FilePath $Copy -ArgumentList "--uninstall" -PassThru
$p.WaitForExit()
Remove-Item $Copy -Force -ErrorAction SilentlyContinue
if ($p.ExitCode -ne 0) { throw "hushkey --uninstall failed (exit $($p.ExitCode))" }
Remove-Item (Split-Path $Exe) -Recurse -Force -ErrorAction SilentlyContinue
Write-Host "hushkey removed"
