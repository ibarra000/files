<#
.SYNOPSIS
    Fetches the bundled kensaku (content search) into artifacts\kensaku.

.DESCRIPTION
    Downloads the pinned kensaku-headless release for Windows x86_64, checks
    it against the pinned SHA256, and extracts kensaku.exe, onnxruntime.dll
    and LICENSE to artifacts\kensaku, where make_msi.ps1 and a build run from
    source find them. To update, change the pins below (see docs\KENSAKU.md).

.EXAMPLE
    pwsh tools/fetch_kensaku.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
# Invoke-WebRequest's progress bar slows the download many times over.
$ProgressPreference = 'SilentlyContinue'

# The release, and the asset's hash from that release's SHA256SUMS. A changed upload fails the hash check.
$Version = '2.0.0'
$Asset = "kensaku-$Version-x86_64-pc-windows-msvc.zip"
$Sha256 = '0d53dac689d3e99cc7d52d6c8185c5de968a6fcc7f4fbd23d84dc539cd65ba02'
$Url = "https://github.com/ibarra000/kensaku-headless/releases/download/v$Version/$Asset"
$Files = @('kensaku.exe', 'onnxruntime.dll', 'LICENSE')

$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'artifacts\kensaku'
$work = Join-Path ([IO.Path]::GetTempPath()) "files-kensaku-$([guid]::NewGuid().ToString('N'))"

New-Item -ItemType Directory -Force -Path $work | Out-Null
try {
    $zip = Join-Path $work $Asset
    Write-Host "downloading $Url"
    Invoke-WebRequest -Uri $Url -OutFile $zip -UseBasicParsing

    $hash = (Get-FileHash -Algorithm SHA256 $zip).Hash
    if ($hash -ne $Sha256) {
        throw "$Asset has SHA256 $hash, not the pinned $Sha256; the release changed, so check it before updating the pin"
    }

    $unpacked = Join-Path $work 'unpacked'
    Expand-Archive -Path $zip -DestinationPath $unpacked
    # The zip holds one folder named after the asset; the files are taken from there by name, nothing else.
    $top = Join-Path $unpacked ([IO.Path]::GetFileNameWithoutExtension($Asset))
    $staged = Join-Path $work 'kensaku'
    New-Item -ItemType Directory -Force -Path $staged | Out-Null
    foreach ($name in $Files) {
        $file = Join-Path $top $name
        if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "$Asset has no $name in its top folder" }
        Copy-Item -LiteralPath $file -Destination (Join-Path $staged $name)
    }
    # Swapped in only once every file is there, so a failed fetch never leaves a half-updated artifacts\kensaku.
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $out) | Out-Null
    if (Test-Path -LiteralPath $out) { Remove-Item -LiteralPath $out -Recurse -Force }
    Move-Item -LiteralPath $staged -Destination $out
    Write-Host "kensaku $Version is in $out"
}
finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
