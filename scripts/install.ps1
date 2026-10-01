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
# Downloads are verified against the digest GitHub records for the asset in the
# release JSON, so a stale or corrupted mirror copy is refused.
#
# One-liner (latest release):
#   irm https://raw.githubusercontent.com/guuzaa/oven/master/scripts/install.ps1 | iex

param(
    [string]$Tag
)

$ErrorActionPreference = 'Stop'
# PowerShell 5.1 renders a progress bar per transfer, which makes a multi-megabyte
# download crawl.
$ProgressPreference = 'SilentlyContinue'

$repo = 'guuzaa/oven'
$binName = 'oven'
$installDir = Join-Path $env:USERPROFILE '.oven'
$binDir = Join-Path $installDir 'bin'

# Base URL of the distribution mirror, tried before github.com and shaped as
# $mirror/latest, $mirror/tags/<tag> and $mirror/dl/<tag>/oven-<tag>-<target>.zip.
# Point $env:OVEN_MIRROR somewhere else, or set it to an empty string to skip the
# mirror.
$defaultMirror = 'https://oven.paulden.site'
# An explicitly empty value disables the mirror; only an unset variable falls
# back to the default.
$mirror = if ($env:OVEN_MIRROR -ne $null) { $env:OVEN_MIRROR } else { $defaultMirror }
while ($mirror.EndsWith('/')) {
    $mirror = $mirror.Substring(0, $mirror.Length - 1)
}

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

# --- Resolve the release ----------------------------------------------------
# The release JSON is the source of the tag and of the digest recorded for every
# asset, so it is fetched once and read throughout.
$pinnedTag = $Tag
if (-not $pinnedTag) { $pinnedTag = $env:OVEN_VERSION }

# Tags are v-prefixed; accept either form.
$releasePath = if ($pinnedTag) {
    if ($pinnedTag -like 'v*') { "tags/$pinnedTag" } else { "tags/v$pinnedTag" }
} else {
    'latest'
}

$bases = @()
if ($mirror) { $bases += $mirror }
$bases += "https://api.github.com/repos/$repo/releases"

$release = $null
foreach ($base in $bases) {
    try {
        Write-Host "Resolving $base/$releasePath ..."
        $release = Invoke-RestMethod -Uri "$base/$releasePath"
        break
    } catch {
        Write-Host "release lookup failed: $_"
    }
}
if (-not $release) {
    throw 'error: could not determine the release; pass it explicitly, e.g. install.ps1 v0.1.0'
}

$resolvedTag = $release.tag_name

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
$downloadBases = @()
if ($mirror) { $downloadBases += "$mirror/dl/$Tag" }
$downloadBases += "https://github.com/$repo/releases/download/$Tag"

# GitHub records a sha256 for every uploaded asset, so an install is only as
# trustworthy as its digest. Refusing without one keeps a stale or tampered
# archive from reaching the disk.
function Test-AssetDigest {
    param([Parameter(Mandatory)][string]$File)
    $digest = ($release.assets | Where-Object { $_.name -eq $asset } | Select-Object -First 1).digest
    if (-not $digest) {
        throw "error: $Tag publishes no digest for $asset; refusing to install it"
    }

    $expected = ($digest -replace '^sha256:', '').ToLowerInvariant()
    $actual = (Get-FileHash -Path $File -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "error: $asset does not match the digest published with $Tag`n  expected: $expected`n  actual:   $actual`n  The mirror may still serve an earlier build of this tag."
    }
    Write-Host "Verified $asset"
}

# --- Download and extract -------------------------------------------------
$tmp = Join-Path $env:TEMP ("oven-install-" + [guid]::NewGuid().ToString('N'))
try {
    New-Item -ItemType Directory -Path $tmp | Out-Null

    $archive = $null
    foreach ($base in $downloadBases) {
        $url = "$base/$asset"
        try {
            Write-Host "Downloading $url ..."
            $downloaded = Join-Path $tmp $asset
            Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $downloaded
            $stream = [System.IO.File]::OpenRead($downloaded)
            try {
                $magic = New-Object byte[] 2
                $read = $stream.Read($magic, 0, 2)
            } finally {
                $stream.Dispose()
            }
            if ($read -ne 2 -or $magic[0] -ne 0x50 -or $magic[1] -ne 0x4B) {
                throw "response is not a zip archive"
            }
            $archive = $downloaded
            break
        } catch {
            Write-Host "download failed: $_"
        }
    }
    if (-not $archive) {
        throw "error: failed to download $asset from any mirror"
    }

    Test-AssetDigest -File $archive

    Write-Host 'Extracting...'
    Expand-Archive -Path $archive -DestinationPath $tmp -Force

    New-Item -ItemType Directory -Path $binDir -Force | Out-Null
    # Stage inside the temporary directory and move, so an interrupted install
    # cannot leave a truncated executable behind.
    $staged = Join-Path $tmp "$binName.exe"
    Copy-Item -Path (Join-Path $tmp "oven-$target\$binName.exe") -Destination $staged -Force
    Move-Item -Path $staged -Destination (Join-Path $binDir "$binName.exe") -Force

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
