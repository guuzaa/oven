# Install the `oven` binary from a GitHub release.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File install.ps1 [TAG]   # e.g. install.ps1 v0.1.0 (defaults to latest release)
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
if (-not $Tag) { $Tag = $env:OVEN_VERSION }
if (-not $Tag) {
    Write-Host 'Resolving the latest release tag...'
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest"
    $Tag = $release.tag_name
}
if (-not $Tag) {
    throw 'error: could not determine the release tag; pass it explicitly, e.g. install.ps1 v0.1.0'
}

# Release tags are v-prefixed; accept either form.
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
