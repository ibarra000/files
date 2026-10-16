<#
.SYNOPSIS
    Builds the installer.

.DESCRIPTION
    Builds the release binaries, reads the version from Cargo.toml and passes
    both to WiX 5. Requires the WiX dotnet tool and its UI extension:

        dotnet tool install --global wix --version 5.0.2
        wix extension add --global WixToolset.UI.wixext/5.0.2

.EXAMPLE
    pwsh tools/make_msi.ps1

.EXAMPLE
    pwsh tools/make_msi.ps1 -Release

    Builds the installer and its release manifest into target\release\dist
    and, if the GitHub CLI is installed, publishes them as release v<version>.
#>
[CmdletBinding()]
param(
    # Skip `cargo build --release`, for when the binaries are already current.
    [switch]$NoBuild,

    # Copy the installer to this folder and write the manifest beside it.
    [string]$Publish,

    # Build the installer and latest.toml into target\release\dist and publish
    # them as release v<version> with the GitHub CLI, if installed. The tag's
    # `v` prefix is required: `update::github` downloads from
    # releases/download/v<version>/.
    [switch]$Release
)

$ErrorActionPreference = 'Stop'

# Writes latest.toml beside the installer it names. Both use the same
# $Version, so they cannot disagree. Written as UTF-8 without a BOM, which
# Windows PowerShell 5.1's `-Encoding utf8` would add.
function Write-Manifest([string]$Folder, [string]$Version, [string]$Msi) {
    $hash = (Get-FileHash -Path (Join-Path $Folder $Msi) -Algorithm SHA256).Hash.ToLowerInvariant()
    $text = "version = `"$Version`"`nmsi     = `"$Msi`"`nsha256  = `"$hash`"`n"
    # Write to a temporary name and move into place, so readers never see a
    # partial file.
    $tmp = Join-Path $Folder 'latest.toml.tmp'
    [System.IO.File]::WriteAllText($tmp, $text, (New-Object System.Text.UTF8Encoding $false))
    Move-Item -Path $tmp -Destination (Join-Path $Folder 'latest.toml') -Force
}
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    if (-not $NoBuild) {
        Write-Host 'building the release binaries...'
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
    }

    # Read from Cargo.toml so the installer and executables share one version.
    $manifest = Get-Content 'Cargo.toml' -Raw
    if ($manifest -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
        throw 'could not read the version out of Cargo.toml'
    }
    $version = $Matches[1]
    Write-Host "version $version"

    $target = Join-Path $root 'target\release'
    $out = Join-Path $target "files-$version-x64.msi"

    # `dotnet tool install --global` puts wix in ~\.dotnet\tools, which is not
    # always on PATH.
    $wix = (Get-Command wix -ErrorAction SilentlyContinue).Source
    if (-not $wix) {
        $wix = Join-Path $HOME '.dotnet\tools\wix.exe'
        if (-not (Test-Path $wix)) {
            throw 'wix not found on PATH or in ~\.dotnet\tools; see the install steps at the top of this script'
        }
    }

    & $wix build `
        -arch x64 `
        -define "Version=$version" `
        -define "TargetDir=$target" `
        -ext WixToolset.UI.wixext `
        -out $out `
        (Join-Path $root 'wix\files.wxs')

    if ($LASTEXITCODE -ne 0) { throw "wix build failed ($LASTEXITCODE)" }
    Write-Host "wrote $out"

    if ($Publish) {
        if (-not (Test-Path $Publish)) { throw "no such folder: $Publish" }

        $name = Split-Path -Leaf $out
        # Copy the installer before writing the manifest, so clients are never
        # offered a version whose installer is not there yet.
        Copy-Item -Path $out -Destination (Join-Path $Publish $name) -Force
        Write-Manifest -Folder $Publish -Version $version -Msi $name

        Write-Host "published $version to $Publish"
    }

    if ($Release) {
        $dist = Join-Path $target 'dist'
        New-Item -ItemType Directory -Force -Path $dist | Out-Null
        $name = Split-Path -Leaf $out
        Copy-Item -Path $out -Destination (Join-Path $dist $name) -Force
        Write-Manifest -Folder $dist -Version $version -Msi $name
        $assets = @((Join-Path $dist $name), (Join-Path $dist 'latest.toml'))

        if (Get-Command gh -ErrorAction SilentlyContinue) {
            # A full release: GitHub's "latest" skips drafts and pre-releases.
            gh release create "v$version" @assets --title "files $version" --generate-notes
            if ($LASTEXITCODE -ne 0) { throw "gh release create failed ($LASTEXITCODE)" }
            Write-Host "released v$version on GitHub"
        }
        else {
            Write-Host "no GitHub CLI, so nothing was uploaded. Create a release tagged v$version"
            Write-Host "(not a draft, not a pre-release) and attach both of these:"
            $assets | ForEach-Object { Write-Host "  $_" }
        }
    }
}
finally {
    Pop-Location
}
