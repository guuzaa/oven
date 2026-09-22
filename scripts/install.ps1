# Install the `oven` binary from a GitHub release.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File install.ps1 [TAG]   # e.g. install.ps1 v0.1.0 (defaults to latest release)
#
# When no TAG is given and the requested version is already installed, the
# download is skipped.
#
# You can also pin the version with OVEN_VERSION:
#   $env:OVEN_VERSION='v0.1.0'; powershell -ExecutionPolicy Bypass -File install.ps1
#
# One-liner (latest release):
#   irm https://raw.githubusercontent.com/guuzaa/oven/master/scripts/install.ps1 | iex

param(
    [string]$Tag
)

$ErrorActionPreference = 'Stop'

$repo = 'guuzaa/oven'
$binName = 'oven'
$installDir = Join-Path $env:USERPROFILE '.oven'
$binDir = Join-Path $installDir 'bin'

# GitHub requires TLS 1.2; PowerShell 5.1 does not negotiate it by default.
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

# --- Detect architecture ---------------------------------------------------
$procArch = $env:PROCESSOR_ARCHITECTURE
$procArchWow = $env:PROCESSOR_ARCHITEW6432
if ($procArch -eq 'AMD64' -or $procArchWow -eq 'AMD64') {
    $target = 'x86_64-pc-windows-gnu'
} else {
    throw "error: no prebuilt binary for $procArch"
}

# --- Resolve the release tag ----------------------------------------------
$pinnedTag = $Tag
if (-not $pinnedTag) { $pinnedTag = $env:OVEN_VERSION }
$resolvedTag = $pinnedTag
if (-not $resolvedTag) {
    Write-Host 'Resolving the latest release tag...'
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest"
    $resolvedTag = $release.tag_name
}
if (-not $resolvedTag) {
    throw 'error: could not determine the release tag; pass it explicitly, e.g. install.ps1 v0.1.0'
}

# Release tags are v-prefixed; the installed binary reports a bare version.
$version = $resolvedTag -replace '^v', ''

if (-not $pinnedTag) {
    $installedBin = $null
    $installedVersion = $null

    $candidates = @()
    $managed = Join-Path $binDir "$binName.exe"
    if (Test-Path $managed) { $candidates += $managed }
    $onPath = Get-Command $binName -ErrorAction SilentlyContinue
    if ($onPath) { $candidates += $onPath.Source }

    foreach ($candidate in $candidates) {
        try {
            $line = & $candidate -V 2>&1 | Select-Object -First 1
        } catch {
            continue
        }
        $parsed = [regex]::Match($line, '^oven\s+(\d+(?:\.\d+)*)')
        if ($parsed.Success) {
            $installedBin = $candidate
            $installedVersion = $parsed.Groups[1].Value
            break
        }
    }

    if ($installedBin -and $installedVersion -eq $version) {
        Write-Host "oven $version is already installed at $installedBin"
        $reinstall = $false
        try {
            $reply = Read-Host 'Reinstall anyway? [y/N]'
            $reinstall = ($reply -match '^[Yy]')
        } catch {
            $reinstall = $false
        }
        if ($reinstall) {
            Write-Host "Reinstalling oven $version ..."
        } else {
            return
        }
    } elseif ($installedBin) {
        Write-Host "Found oven $installedVersion at $installedBin, upgrading to $version ..."
    }
}

# Tags are v-prefixed; accept either form.
$Tag = $resolvedTag
if ($Tag -notlike 'v*') { $Tag = "v$Tag" }

$asset = "oven-$Tag-$target.zip"
$url = "https://github.com/$repo/releases/download/$Tag/$asset"

# --- Download and extract -------------------------------------------------
$tmp = Join-Path $env:TEMP ("oven-install-" + [guid]::NewGuid().ToString('N'))
try {
    New-Item -ItemType Directory -Path $tmp | Out-Null

    Write-Host "Downloading $url ..."
    Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile (Join-Path $tmp $asset)

    Write-Host 'Extracting...'
    Expand-Archive -Path (Join-Path $tmp $asset) -DestinationPath $tmp -Force

    New-Item -ItemType Directory -Path $binDir -Force | Out-Null
    Copy-Item -Path (Join-Path $tmp "oven-$target\$binName.exe") -Destination (Join-Path $binDir "$binName.exe") -Force

    # --- Add to user PATH --------------------------------------------------
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $paths = @($userPath -split ';' | Where-Object { $_ })
    if ($paths -notcontains $binDir) {
        $newPath = if ($paths.Count -gt 0) { ($paths + $binDir) -join ';' } else { $binDir }
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        Write-Host "Added $binDir to PATH"
    } else {
        Write-Host "$binDir is already in PATH"
    }

    Write-Host ''
    Write-Host "oven $Tag installed to $binDir\$binName.exe"
    Write-Host 'Restart your terminal for the PATH change to take effect.'
    Write-Host 'Verify with: oven --help'
}
finally {
    Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
