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
    New-Item -ItemType Directory -Force -Path $out | Out-Null
    foreach ($name in $Files) {
        $file = Get-ChildItem -Path $unpacked -Recurse -File -Filter $name | Select-Object -First 1
        if (-not $file) { throw "$Asset has no $name" }
        Copy-Item -Path $file.FullName -Destination (Join-Path $out $name) -Force
    }
    Write-Host "kensaku $Version is in $out"
}
finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
