<#
.SYNOPSIS
Build the Windows release artifacts.

.DESCRIPTION
Produces:
  AugurRS-<version>-windows-x86_64-setup.exe   GUI installer and update payload
  augur-<version>-windows-x86_64.zip           portable archive

The installer is per-user (see augur.nsi for why), which is what lets the
in-app updater re-run it with /S and no UAC prompt.

.PARAMETER OutDir
Where to place the artifacts. Defaults to <repo>\dist.

.PARAMETER SkipBuild
Reuse binaries already in target\release instead of running cargo.
#>
[CmdletBinding()]
param(
    [string] $OutDir,
    [switch] $SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
if (-not $OutDir) { $OutDir = Join-Path $repoRoot 'dist' }

function Write-Step($message) { Write-Host "==> $message" -ForegroundColor Cyan }

# Workspace version from [workspace.package]; the first bare `version = ` line.
$cargoToml = Get-Content (Join-Path $repoRoot 'Cargo.toml')
$versionLine = $cargoToml | Where-Object { $_ -match '^version = "(.+)"' } | Select-Object -First 1
if (-not $versionLine) { throw "failed to read workspace version from Cargo.toml" }
$version = [regex]::Match($versionLine, '^version = "(.+)"').Groups[1].Value

# VIProductVersion insists on a four-part numeric version.
$parts = $version -split '[.\-+]' | Where-Object { $_ -match '^\d+$' }
while ($parts.Count -lt 4) { $parts += '0' }
$versionQuad = ($parts[0..3]) -join '.'

Write-Step "packaging AugurRS $version"

Push-Location $repoRoot
try {
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

    if (-not $SkipBuild) {
        Write-Step 'building release binaries'
        cargo build --release --locked --bin augur --bin AugurRS
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
    }

    $releaseDir = Join-Path $repoRoot 'target\release'
    foreach ($exe in @('AugurRS.exe', 'augur.exe')) {
        if (-not (Test-Path (Join-Path $releaseDir $exe))) {
            throw "missing $exe in $releaseDir - build first or drop -SkipBuild"
        }
    }

    # One staging tree feeds both the installer and the portable zip, so the two
    # can never disagree about what a release actually contains.
    Write-Step 'staging payload'
    $stage = Join-Path $repoRoot 'target\windows-stage'
    if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
    New-Item -ItemType Directory -Force -Path (Join-Path $stage 'examples') | Out-Null

    Copy-Item (Join-Path $releaseDir 'AugurRS.exe') $stage
    Copy-Item (Join-Path $releaseDir 'augur.exe') $stage
    Copy-Item (Join-Path $repoRoot 'assets\AugurRS.ico') $stage
    foreach ($doc in @('README.md', 'LICENSE', 'CONTRIBUTING.md', 'CHANGELOG.md')) {
        Copy-Item (Join-Path $repoRoot $doc) $stage
    }
    Copy-Item (Join-Path $repoRoot 'examples\augur.toml') (Join-Path $stage 'examples')

    Write-Step 'locating NSIS'
    $makensis = (Get-Command makensis -ErrorAction SilentlyContinue)?.Source
    if (-not $makensis) {
        $candidate = Join-Path ${env:ProgramFiles(x86)} 'NSIS\makensis.exe'
        if (Test-Path $candidate) { $makensis = $candidate }
    }
    if (-not $makensis) {
        Write-Step 'NSIS not found, installing via chocolatey'
        choco install nsis -y --no-progress
        if ($LASTEXITCODE -ne 0) { throw "choco install nsis failed with exit code $LASTEXITCODE" }
        $makensis = Join-Path ${env:ProgramFiles(x86)} 'NSIS\makensis.exe'
    }
    if (-not (Test-Path $makensis)) { throw "makensis not found at $makensis" }

    Write-Step 'building installer'
    $setupName = "AugurRS-$version-windows-x86_64-setup.exe"
    $setupPath = Join-Path $OutDir $setupName
    if (Test-Path $setupPath) { Remove-Item -Force $setupPath }

    & $makensis `
        "/DAPP_VERSION=$version" `
        "/DAPP_VERSION_QUAD=$versionQuad" `
        "/DOUT_FILE=$setupPath" `
        "/DSTAGE_DIR=$stage" `
        "/DICON_FILE=$(Join-Path $repoRoot 'assets\AugurRS.ico')" `
        "/DLICENSE_FILE=$(Join-Path $repoRoot 'LICENSE')" `
        (Join-Path $PSScriptRoot 'augur.nsi')
    if ($LASTEXITCODE -ne 0) { throw "makensis failed with exit code $LASTEXITCODE" }

    Write-Step 'packaging portable archive'
    $zipName = "augur-$version-windows-x86_64.zip"
    $zipPath = Join-Path $OutDir $zipName
    if (Test-Path $zipPath) { Remove-Item -Force $zipPath }

    $portable = Join-Path $repoRoot 'target\windows-portable\AugurRS'
    if (Test-Path (Split-Path $portable)) { Remove-Item -Recurse -Force (Split-Path $portable) }
    New-Item -ItemType Directory -Force -Path $portable | Out-Null
    Copy-Item -Recurse (Join-Path $stage '*') $portable
    Copy-Item (Join-Path $PSScriptRoot 'AugurRS.cmd') $portable
    Compress-Archive -Path $portable -DestinationPath $zipPath

    Write-Step "artifacts in $OutDir"
    Get-ChildItem $setupPath, $zipPath | Format-Table Name, Length
}
finally {
    Pop-Location
}
