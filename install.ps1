# hushkey installer for Windows: downloads the program from the latest GitHub
# release, checks it against SHA256SUMS, and lets it install itself (Start
# Menu entry, start with Windows). An existing Python hushkey is taken over:
# its settings and history move along, and it is switched off.
#
# Usage:  powershell -ExecutionPolicy Bypass -File install.ps1 [-Pre] [-Exe <file>]
#   or:   irm https://raw.githubusercontent.com/aignermax/hushkey/master/install.ps1 | iex
#
# This is also the last step of the Python hushkey's own updater, which runs
# master's install.ps1 after an update.
[CmdletBinding()]
param(
    [switch]$Pre,      # include pre-releases
    [string]$Exe,      # install this local build instead of downloading
    [string]$LogPath   # the old setup.exe passed this; kept harmless
)

$ErrorActionPreference = "Stop"
if ($LogPath) { Start-Transcript -Path $LogPath -Append | Out-Null }
# Windows PowerShell 5.1 may still default to TLS 1.0.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$Repo = "aignermax/hushkey"
$Asset = "hushkey-windows-x86_64.exe"
$Headers = @{ "User-Agent" = "hushkey-installer"; "Accept" = "application/vnd.github+json" }

$Tmp = Join-Path $env:TEMP ("hushkey-install-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force $Tmp | Out-Null
try {
    if (-not $Exe) {
        if ($Pre) {
            $releases = Invoke-RestMethod -Headers $Headers "https://api.github.com/repos/$Repo/releases?per_page=30"
            $release = $releases | Where-Object { -not $_.draft -and ($_.assets.name -contains $Asset) } | Select-Object -First 1
        } else {
            $release = Invoke-RestMethod -Headers $Headers "https://api.github.com/repos/$Repo/releases/latest"
        }
        if (-not $release) { throw "no release of $Repo carries $Asset" }
        $url = ($release.assets | Where-Object name -eq $Asset).browser_download_url
        $sumsUrl = ($release.assets | Where-Object name -eq "SHA256SUMS").browser_download_url
        if (-not $url -or -not $sumsUrl) { throw "release $($release.tag_name) lacks $Asset or SHA256SUMS" }

        Write-Host "==> downloading hushkey $($release.tag_name)"
        $Exe = Join-Path $Tmp $Asset
        Invoke-WebRequest -UseBasicParsing -Headers $Headers $url -OutFile $Exe
        $sums = (Invoke-WebRequest -UseBasicParsing -Headers $Headers $sumsUrl).Content
        if ($sums -is [byte[]]) { $sums = [Text.Encoding]::UTF8.GetString($sums) }
        $line = ($sums -split "`n") | Where-Object { $_ -match ("[\s*]" + [regex]::Escape($Asset) + "\s*$") } | Select-Object -First 1
        if (-not $line) { throw "SHA256SUMS lists no $Asset" }
        $expected = ($line -split "\s+")[0].ToLower()
        $actual = (Get-FileHash -Algorithm SHA256 $Exe).Hash.ToLower()
        if ($actual -ne $expected) { throw "$Asset is damaged: hash $actual, expected $expected" }
    }

    Write-Host "==> installing"
    # A window program: '&' would not wait for it, and Start-Process -Wait
    # would also wait for the hushkey it starts (a child), forever.
    $p = Start-Process -FilePath $Exe -ArgumentList "--install", "--from-python" -PassThru
    $p.WaitForExit()
    if ($p.ExitCode -ne 0) {
        throw "hushkey --install failed (exit $($p.ExitCode)); see $env:LOCALAPPDATA\hushkey-rs\hushkey.log"
    }
    Write-Host ""
    Write-Host "Done: hushkey runs (tray icon) and starts with Windows."
    Write-Host "Hold your key (Caps Lock unless you picked another), speak, release."
    Write-Host "Settings are in the tray menu. Remove: run uninstall.ps1, or"
    Write-Host "  $env:LOCALAPPDATA\Programs\hushkey\hushkey.exe --uninstall"
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
    if ($LogPath) { Stop-Transcript | Out-Null }
}
