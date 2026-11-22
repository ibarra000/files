<#
.SYNOPSIS
    Builds the bundled kensaku (content search) into artifacts\kensaku.

.DESCRIPTION
    Builds kensaku-daemon.exe and kensaku-cli.exe from the pinned submodule in
    third_party\kensaku, downloads ONNX Runtime (which kensaku loads at run
    time) and checks it against the pinned SHA256, then copies the three files
    and the licences to artifacts\kensaku, where make_msi.ps1 and a build run
    from source find them. See docs\KENSAKU.md for how to update the pins.

    Needs Rust 1.98 or newer with the MSVC toolchain, mt.exe from the Windows
    SDK, and bun, which builds the web files the kensaku library embeds:

        winget install Rustlang.Rustup
        winget install Oven-sh.Bun

    The first build takes a few minutes on a fast PC and up to half an hour on
    a slow one; later builds reuse cargo's cache.

.EXAMPLE
    pwsh tools/build_kensaku.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

# ONNX Runtime, as kensaku's release.yml pins it. Microsoft's NuGet package
# holds the same official build as the GitHub zip upstream names.
$OrtVersion = '1.31.0'
$OrtUrl = "https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime/$OrtVersion/microsoft.ml.onnxruntime.$OrtVersion.nupkg"
$OrtSha256 = '7e35fdb4f5adb62f01e60798e2e5ed05108f5fbbe1cca03fbcc37903cf7c5e3b'

# kensaku 1.0.18 depends on mail-parser 1.0, which needs Rust 1.98.
$MinRust = [version]'1.98'

$root = Split-Path -Parent $PSScriptRoot
$source = Join-Path $root 'third_party\kensaku'
$out = Join-Path $root 'artifacts\kensaku'
$work = Join-Path $root 'artifacts\kensaku-work'

function Require([string]$Command, [string]$Install) {
    if (-not (Get-Command $Command -ErrorAction SilentlyContinue)) {
        throw "$Command was not found. Install it with: $Install"
    }
}

# Returns mt.exe, from PATH or the newest Windows SDK.
function Check-Toolchain {
    Require 'cargo' 'winget install Rustlang.Rustup'
    Require 'rustc' 'winget install Rustlang.Rustup'
    Require 'bun' 'winget install Oven-sh.Bun'
    $rust = [version]((rustc --version) -replace '^rustc (\d+\.\d+).*$', '$1')
    if ($rust -lt $MinRust) { throw "Rust $rust is too old; kensaku needs $MinRust or newer (rustup update)" }
    if (-not (Test-Path (Join-Path $source 'cli\Cargo.toml'))) {
        throw 'third_party\kensaku is empty; run: git submodule update --init third_party/kensaku'
    }
    $mt = (Get-Command mt -ErrorAction SilentlyContinue).Source
    if (-not $mt) {
        $mt = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\mt.exe" -ErrorAction SilentlyContinue |
            Sort-Object FullName -Descending | Select-Object -First 1 -ExpandProperty FullName
        if (-not $mt) { throw 'mt.exe not found; install the Windows SDK' }
    }
    return $mt
}

# Builds the web files first: the kensaku library embeds ..\dist when it compiles.
function Build-Kensaku {
    Push-Location $source
    try {
        bun install --frozen-lockfile
        if ($LASTEXITCODE -ne 0) { throw "bun install failed ($LASTEXITCODE)" }
        bun run build
        if ($LASTEXITCODE -ne 0) { throw "bun run build failed ($LASTEXITCODE)" }
    }
    finally {
        Pop-Location
    }
    # Not --locked: upstream's cli\Cargo.lock lags its dependencies, and its own release builds without it.
    # The lock file cargo updates is put back, so the submodule stays at the pinned commit.
    try {
        cargo build --release --manifest-path (Join-Path $source 'cli\Cargo.toml') --bin kensaku-daemon --bin kensaku-cli
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
    }
    finally {
        git -C $source checkout -- cli/Cargo.lock
    }
}

# Downloads ONNX Runtime once, checks it, and unpacks it into the work folder.
function Get-OnnxRuntime {
    New-Item -ItemType Directory -Force -Path $work | Out-Null
    $package = Join-Path $work "onnxruntime-$OrtVersion.zip"
    if (-not (Test-Path $package) -or (Get-FileHash -Algorithm SHA256 $package).Hash -ne $OrtSha256) {
        Write-Host "downloading ONNX Runtime $OrtVersion..."
        Invoke-WebRequest -Uri $OrtUrl -OutFile $package -UseBasicParsing
    }
    $actual = (Get-FileHash -Algorithm SHA256 $package).Hash.ToLowerInvariant()
    if ($actual -ne $OrtSha256) { throw "ONNX Runtime checksum mismatch: $actual" }
    $unpacked = Join-Path $work 'onnxruntime'
    Remove-Item -Recurse -Force $unpacked -ErrorAction SilentlyContinue
    Expand-Archive -Path $package -DestinationPath $unpacked
    return $unpacked
}

function Copy-Bundle([string]$Ort, [string]$Mt) {
    Remove-Item -Recurse -Force $out -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path $out | Out-Null
    $release = Join-Path $source 'cli\target\release'
    $daemon = Join-Path $out 'kensaku-daemon.exe'
    Copy-Item (Join-Path $release 'kensaku-daemon.exe') $daemon
    # The daemon calls Common Controls v6 through the kensaku library, whose build embeds the manifest asking
    # for v6 only in its own binaries; without one Windows ends the daemon with STATUS_ENTRYPOINT_NOT_FOUND.
    & $Mt -nologo -manifest (Join-Path $source 'src-tauri\windows-app-manifest.xml') "-outputresource:$daemon;#1"
    if ($LASTEXITCODE -ne 0) { throw "mt.exe could not embed the manifest ($LASTEXITCODE)" }
    Copy-Item (Join-Path $release 'kensaku-cli.exe') $out
    Copy-Item (Join-Path $Ort 'runtimes\win-x64\native\onnxruntime.dll') $out
    Copy-Item (Join-Path $source 'LICENSE') (Join-Path $out 'LICENSE-kensaku.txt')
    Copy-Item (Join-Path $Ort 'LICENSE') (Join-Path $out 'LICENSE-onnxruntime.txt')
    Copy-Item (Join-Path $Ort 'ThirdPartyNotices.txt') (Join-Path $out 'ThirdPartyNotices-onnxruntime.txt')
}

$mt = Check-Toolchain
Build-Kensaku
$ort = Get-OnnxRuntime
Copy-Bundle -Ort $ort -Mt $mt
$size = (Get-ChildItem $out | Measure-Object -Property Length -Sum).Sum / 1MB
Write-Host ("wrote {0} ({1:N1} MB)" -f $out, $size)
