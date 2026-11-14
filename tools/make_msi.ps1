<#
.SYNOPSIS
    Builds the installer.

.DESCRIPTION
    Publishes the self-contained files.exe, reads the version from
    src/Files/Files.csproj and passes both to WiX 5. files.exe and the MSI are
    both signed with the release certificate, which must be in the current
    user's certificate store; the updater refuses installers signed with any
    other certificate. Requires signtool (Windows SDK), the WiX dotnet tool and
    its UI extension:

        dotnet tool install --global wix --version 5.0.2
        wix extension add --global WixToolset.UI.wixext/5.0.2

.EXAMPLE
    pwsh tools/make_msi.ps1

.EXAMPLE
    pwsh tools/make_msi.ps1 -Volumes volumes.json -Publish \\server\files-updates

    Builds an installer with preset volumes and publishes it to a shared
    update folder. volumes.json is a JSON array such as
    [{ "name": "Drawings", "folder": "\\\\server\\drawings", "liveOnly": false }];
    each volume is added once on every PC the installer runs on.

.EXAMPLE
    pwsh tools/make_msi.ps1 -Release

    Builds the installer and latest.json into artifacts\dist and, if the
    GitHub CLI is installed, publishes them as release v<version>.
#>
[CmdletBinding()]
param(
    # Skip `dotnet publish`, for when artifacts\publish is already current.
    [switch]$NoBuild,

    # A volumes.json to build into the installer: volumes added once on each PC.
    [string]$Volumes,

    # Copy the installer to this folder and write latest.json beside it.
    [string]$Publish,

    # Publish the installer and latest.json as GitHub release v<version>. The
    # `v` prefix is required: the updater downloads from releases/download/v<version>/.
    [switch]$Release,

    # The release-signing certificate in Cert:\CurrentUser\My. Must match
    # Updater.PublisherThumbprint, or installed copies will refuse the update.
    [string]$Thumbprint = '5EAA01DFACB29C29C8D7C21F961F616CFB6B3728',

    # Timestamping keeps signatures valid after the certificate expires.
    [string]$TimestampUrl = 'http://timestamp.digicert.com'
)

$ErrorActionPreference = 'Stop'

# Writes latest.json beside the installer it names, through a temporary file so
# readers never see a partial manifest. UTF-8 without a BOM.
function Write-Manifest([string]$Folder, [string]$Version, [string]$Msi) {
    $hash = (Get-FileHash -Path (Join-Path $Folder $Msi) -Algorithm SHA256).Hash.ToLowerInvariant()
    $json = [ordered]@{ version = $Version; msi = $Msi; sha256 = $hash } | ConvertTo-Json
    $tmp = Join-Path $Folder 'latest.json.tmp'
    [System.IO.File]::WriteAllText($tmp, $json, (New-Object System.Text.UTF8Encoding $false))
    Move-Item -Path $tmp -Destination (Join-Path $Folder 'latest.json') -Force
}

# Checks a volumes.json and copies it beside files.exe, or removes a stale copy when there is none.
function Stage-Volumes([string]$Source, [string]$Folder) {
    $staged = Join-Path $Folder 'volumes.json'
    if (-not $Source) {
        Remove-Item -Path $staged -ErrorAction SilentlyContinue
        return $false
    }
    if (-not (Test-Path $Source)) { throw "no such file: $Source" }
    $entries = @(Get-Content $Source -Raw | ConvertFrom-Json)
    if ($entries.Count -eq 0) { throw "$Source lists no volumes" }
    foreach ($entry in $entries) {
        if (-not ($entry.name -is [string] -and $entry.name.Trim()) -or -not ($entry.folder -is [string] -and $entry.folder.Trim())) {
            throw "$Source has a volume without a name or folder: $($entry | ConvertTo-Json -Compress)"
        }
    }
    Copy-Item -Path $Source -Destination $staged -Force
    Write-Host "bundling $($entries.Count) volumes from $Source"
    return $true
}

# Signs a file with the release certificate and checks the result.
function Sign-File([string]$Path) {
    & $signtool sign /sha1 $Thumbprint /s My /fd SHA256 /tr $TimestampUrl /td SHA256 /q $Path
    if ($LASTEXITCODE -ne 0) { throw "signtool could not sign $Path ($LASTEXITCODE)" }
    $signer = (Get-AuthenticodeSignature $Path).SignerCertificate
    if ($signer.Thumbprint -ne $Thumbprint) { throw "$Path is not signed by $Thumbprint" }
    Write-Host "signed $(Split-Path -Leaf $Path)"
}

$root = Split-Path -Parent $PSScriptRoot
$project = Join-Path $root 'src\Files\Files.csproj'
$artifacts = Join-Path $root 'artifacts'
$publishDir = Join-Path $artifacts 'publish'

Push-Location $root
try {
    $version = ([xml](Get-Content $project -Raw)).Project.PropertyGroup.Version | Where-Object { $_ } | Select-Object -First 1
    if (-not $version) { throw "could not read <Version> from $project" }
    Write-Host "version $version"

    if (-not (Test-Path "Cert:\CurrentUser\My\$Thumbprint")) {
        throw "the signing certificate $Thumbprint is not in Cert:\CurrentUser\My"
    }
    $signtool = (Get-Command signtool -ErrorAction SilentlyContinue).Source
    if (-not $signtool) {
        $signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" -ErrorAction SilentlyContinue |
            Sort-Object FullName -Descending | Select-Object -First 1 -ExpandProperty FullName
        if (-not $signtool) { throw 'signtool not found; install the Windows SDK' }
    }

    if (-not $NoBuild) {
        Write-Host 'publishing files.exe...'
        dotnet publish $project -c Release -o $publishDir
        if ($LASTEXITCODE -ne 0) { throw "dotnet publish failed ($LASTEXITCODE)" }
        Sign-File (Join-Path $publishDir 'files.exe')
    }

    # `dotnet tool install --global` puts wix in ~\.dotnet\tools, which is not always on PATH.
    $wix = (Get-Command wix -ErrorAction SilentlyContinue).Source
    if (-not $wix) {
        $wix = Join-Path $HOME '.dotnet\tools\wix.exe'
        if (-not (Test-Path $wix)) {
            throw 'wix not found on PATH or in ~\.dotnet\tools; see the install steps at the top of this script'
        }
    }

    if ($Volumes -and $Release) {
        # A public release is for everyone; preset volumes belong in an installer for one organisation.
        throw '-Volumes cannot be used with -Release; use -Publish to a shared update folder instead'
    }
    $defines = @('-define', "Version=$version", '-define', "TargetDir=$publishDir")
    if (Stage-Volumes -Source $Volumes -Folder $publishDir) {
        $defines += @('-define', 'Volumes=1')
    }

    $name = "files-$version-x64.msi"
    $out = Join-Path $artifacts $name
    & $wix build -arch x64 @defines -ext WixToolset.UI.wixext -out $out (Join-Path $root 'wix\files.wxs')
    if ($LASTEXITCODE -ne 0) { throw "wix build failed ($LASTEXITCODE)" }
    Sign-File $out
    Write-Host "wrote $out"

    if ($Publish) {
        if (-not (Test-Path $Publish)) { throw "no such folder: $Publish" }
        # The installer goes first, so clients are never offered a version whose installer is missing.
        Copy-Item -Path $out -Destination (Join-Path $Publish $name) -Force
        Write-Manifest -Folder $Publish -Version $version -Msi $name
        Write-Host "published $version to $Publish"
    }

    if ($Release) {
        $dist = Join-Path $artifacts 'dist'
        New-Item -ItemType Directory -Force -Path $dist | Out-Null
        Copy-Item -Path $out -Destination (Join-Path $dist $name) -Force
        Write-Manifest -Folder $dist -Version $version -Msi $name
        $assets = @((Join-Path $dist $name), (Join-Path $dist 'latest.json'))

        if (Get-Command gh -ErrorAction SilentlyContinue) {
            # A full release: GitHub's "latest" skips drafts and pre-releases.
            gh release create "v$version" @assets --title "files $version" --generate-notes
            if ($LASTEXITCODE -ne 0) { throw "gh release create failed ($LASTEXITCODE)" }
            Write-Host "released v$version on GitHub"
        }
        else {
            Write-Host "no GitHub CLI, so nothing was uploaded. Create a release tagged v$version"
            Write-Host '(not a draft, not a pre-release) and attach both of these:'
            $assets | ForEach-Object { Write-Host "  $_" }
        }
    }
}
finally {
    Pop-Location
}
