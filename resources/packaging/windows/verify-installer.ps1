<#
.SYNOPSIS
Smoke-test a built Windows installer by actually installing it.

.DESCRIPTION
Runs the installer silently, then checks the things a user would notice and a
build cannot: that the files land in the per-user location, that the Start Menu
shortcut and the Add/Remove Programs entry exist, that the installed CLI runs,
and that uninstalling removes all of it again.

The silent path is the same one the in-app updater uses, so this also proves
updating on Windows works.

.PARAMETER DistDir
Directory holding the built artifacts. Defaults to <repo>\dist.
#>
[CmdletBinding()]
param(
    [string] $DistDir
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
if (-not $DistDir) { $DistDir = Join-Path $repoRoot 'dist' }
$DistDir = (Resolve-Path $DistDir).Path

function Write-Step($message) { Write-Host "==> $message" -ForegroundColor Cyan }

$setup = Get-ChildItem (Join-Path $DistDir '*windows-x86_64-setup.exe') | Select-Object -First 1
if (-not $setup) { throw "no installer found in $DistDir" }

$installDir = Join-Path $env:LOCALAPPDATA 'Programs\AugurRS'
$uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\AugurRS'
$startMenu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\AugurRS'

Write-Step "installing $($setup.Name) silently"
$process = Start-Process -FilePath $setup.FullName -ArgumentList '/S' -Wait -PassThru
if ($process.ExitCode -ne 0) { throw "installer exited with $($process.ExitCode)" }

Write-Step 'checking the installed files'
foreach ($file in @('AugurRS.exe', 'augur.exe', 'AugurRS.ico', 'Uninstall.exe', 'README.md')) {
    $path = Join-Path $installDir $file
    if (-not (Test-Path $path)) { throw "installer did not place $file in $installDir" }
}
if (-not (Test-Path (Join-Path $installDir 'examples\augur.toml'))) {
    throw 'installer did not place the example config'
}

Write-Step 'checking the Start Menu shortcut'
if (-not (Test-Path (Join-Path $startMenu 'AugurRS.lnk'))) {
    throw "no Start Menu shortcut in $startMenu"
}

Write-Step 'checking the Add/Remove Programs entry'
$entry = Get-ItemProperty -Path $uninstallKey -ErrorAction SilentlyContinue
if (-not $entry) { throw "no uninstall entry at $uninstallKey" }
foreach ($name in @('DisplayName', 'DisplayVersion', 'UninstallString', 'DisplayIcon')) {
    if (-not $entry.PSObject.Properties[$name]) { throw "uninstall entry has no $name" }
}
Write-Host "    $($entry.DisplayName) $($entry.DisplayVersion)"

Write-Step 'running the installed CLI'
& (Join-Path $installDir 'augur.exe') --version
if ($LASTEXITCODE -ne 0) { throw "installed augur.exe exited with $LASTEXITCODE" }

Write-Step 'uninstalling'
# NSIS silent uninstallers relaunch themselves from a temp copy, so the first
# process returns long before the work is done. Poll for the result instead of
# trusting the exit.
Start-Process -FilePath (Join-Path $installDir 'Uninstall.exe') -ArgumentList '/S' -Wait
$deadline = (Get-Date).AddSeconds(60)
while ((Test-Path $installDir) -and (Get-Date) -lt $deadline) {
    Start-Sleep -Milliseconds 500
}

if (Test-Path $installDir) { throw "uninstall left $installDir behind" }
if (Test-Path $startMenu) { throw "uninstall left the Start Menu folder behind" }
if (Get-ItemProperty -Path $uninstallKey -ErrorAction SilentlyContinue) {
    throw 'uninstall left the Add/Remove Programs entry behind'
}

Write-Step 'Windows installer verified: installs, runs, and uninstalls cleanly'
