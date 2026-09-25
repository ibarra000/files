<#
.SYNOPSIS
    Builds the installer.

.DESCRIPTION
    Four steps: build the release binaries, read the version out of Cargo.toml,
    hand both to WiX, and say where the result is.

    WiX 5 rather than 3, and `wix build` rather than `cargo wix`. WiX 3 reached
    end of life in February 2025 and is what `cargo-wix` drives; WiX 7 requires
    accepting a paid maintenance-fee licence. WiX 5 is the current version that
    is free to build with, and it is a dotnet tool rather than an installer:

        dotnet tool install --global wix --version 5.0.2
        wix extension add --global WixToolset.UI.wixext/5.0.2

.EXAMPLE
    pwsh tools/make_msi.ps1

.EXAMPLE
    pwsh tools/make_msi.ps1 -Release

    Builds the installer and the manifest a GitHub release carries, into
    target\release\dist, and - if the GitHub CLI is installed - publishes them
    as release v<version>. Every copy of files that checks GitHub is then
    offered that version within four hours.
#>
[CmdletBinding()]
param(
    # Skip `cargo build --release`, for when the binaries are already current.
    [switch]$NoBuild,

    # Copy the installer to this folder and write the manifest beside it, so
    # every running copy is offered the version that was just built. Both are
    # written from the same $version, so the manifest and the installer it
    # names cannot disagree - which is the whole reason this lives here rather
    # than in somebody`s notes.
    [string]$Publish,

    # Build the two files a GitHub release carries - the installer and the
    # latest.toml naming it - into target\release\dist, and publish them as
    # release v<version> with the GitHub CLI if it is installed. The tag's
    # `v` matters: `update::github` fetches the installer from
    # releases/download/v<version>/.
    [switch]$Release
)

$ErrorActionPreference = 'Stop'

# The manifest every copy of files reads, written beside the installer it
# names. Both come from the same $Version, so they cannot disagree.
#
# UTF-8 without a byte-order mark, whichever PowerShell runs this: Windows
# PowerShell 5.1's `-Encoding utf8` writes one. The reader copes either way,
# but a file on a release page should be what it says it is.
function Write-Manifest([string]$Folder, [string]$Version, [string]$Msi) {
    $hash = (Get-FileHash -Path (Join-Path $Folder $Msi) -Algorithm SHA256).Hash.ToLowerInvariant()
    $text = "version = `"$Version`"`nmsi     = `"$Msi`"`nsha256  = `"$hash`"`n"
    # Written to a temporary name and moved into place, so nobody reads a
    # half-written manifest off a share.
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

    # From Cargo.toml, so the installer and the executables can never claim
    # different versions - which is exactly the sort of thing nobody notices
    # until an upgrade refuses to install over itself.
    $manifest = Get-Content 'Cargo.toml' -Raw
    if ($manifest -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
        throw 'could not read the version out of Cargo.toml'
    }
    $version = $Matches[1]
    Write-Host "version $version"

    $target = Join-Path $root 'target\release'
    $out = Join-Path $target "files-$version-x64.msi"

    wix build `
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
        # The installer first, and the manifest only once it has landed. The
        # other order leaves a window in which every client is told about a
        # version whose installer is not there yet - and they would all try.
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
            # A full release, not a draft or a pre-release: GitHub's "latest"
            # skips both, so either would be published and offered to nobody.
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
