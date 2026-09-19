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
    [string]$Publish
)

$ErrorActionPreference = 'Stop'
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

        $hash = (Get-FileHash -Path $out -Algorithm SHA256).Hash.ToLowerInvariant()
        $manifest = @"
version = "$version"
msi     = "$name"
sha256  = "$hash"
"@
        # Written to a temporary name and moved into place, so nobody reads a
        # half-written manifest off the share.
        $tmp = Join-Path $Publish "latest.toml.tmp"
        Set-Content -Path $tmp -Value $manifest -Encoding utf8 -NoNewline
        Move-Item -Path $tmp -Destination (Join-Path $Publish "latest.toml") -Force

        Write-Host "published $version to $Publish"
    }
}
finally {
    Pop-Location
}
