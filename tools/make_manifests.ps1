<#
.SYNOPSIS
    Writes the winget and Chocolatey packages for a released installer.

.DESCRIPTION
    Reads the version, ProductCode and SHA256 from a signed MSI and writes, under
    -Out, a winget manifest (winget\manifests\i\ibarra000\files\<version>) and a
    Chocolatey package (chocolatey\files-search). Both point at the MSI on the
    GitHub release v<version>, so run this for an MSI that is (or will be) attached
    to that release; make_msi.ps1 -Release does. Nothing is submitted: the commands
    to submit are printed at the end.

    Scoop is left out on purpose: it cannot run a per-machine MSI, and unpacking
    the MSI instead would leave the app's own updater installing over it.

.EXAMPLE
    pwsh tools/make_manifests.ps1 -Msi artifacts\dist\files-1.7.4-x64.msi -Out artifacts\dist\packages
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string]$Msi,
    [Parameter(Mandatory)] [string]$Out
)

$ErrorActionPreference = 'Stop'

$Id = 'ibarra000.files'
$ChocoId = 'files-search'
$Repo = 'https://github.com/ibarra000/files'
# Must match wix/files.wxs; winget uses it to recognise any installed version.
$UpgradeCode = '{4008C9CD-48D4-41AF-9DF8-4900FD2FDDA3}'
$Summary = 'Instant file-name search across folders and network shares on Windows.'

# A property of the MSI's Property table, read through the Windows Installer COM API.
function Get-MsiProperty([string]$Path, [string]$Name) {
    $installer = New-Object -ComObject WindowsInstaller.Installer
    $database = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @($Path, 0))
    $view = $database.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $database, @("SELECT Value FROM Property WHERE Property = '$Name'"))
    $view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
    $record = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
    $value = if ($record) { $record.GetType().InvokeMember('StringData', 'GetProperty', $null, $record, 1) } else { $null }
    $view.GetType().InvokeMember('Close', 'InvokeMethod', $null, $view, $null) | Out-Null
    [System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($installer) | Out-Null
    if (-not $value) { throw "$Path has no $Name" }
    return $value
}

# UTF-8 without a BOM, CRLF, which both winget and Chocolatey accept.
function Write-Text([string]$Path, [string]$Text) {
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Path) | Out-Null
    [System.IO.File]::WriteAllText($Path, ($Text -replace "`r?`n", "`r`n"), (New-Object System.Text.UTF8Encoding $false))
}

$Msi = (Resolve-Path $Msi).Path
$version = Get-MsiProperty $Msi 'ProductVersion'
$productCode = Get-MsiProperty $Msi 'ProductCode'
$name = Split-Path -Leaf $Msi
if ($name -ne "files-$version-x64.msi") { throw "$name does not match its version $version" }
$sha256 = (Get-FileHash -Path $Msi -Algorithm SHA256).Hash
$url = "$Repo/releases/download/v$version/$name"

$winget = Join-Path $Out "winget\manifests\i\ibarra000\files\$version"
Write-Text (Join-Path $winget "$Id.yaml") @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.version.1.6.0.schema.json
PackageIdentifier: $Id
PackageVersion: $version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.6.0
"@
Write-Text (Join-Path $winget "$Id.installer.yaml") @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.installer.1.6.0.schema.json
PackageIdentifier: $Id
PackageVersion: $version
MinimumOSVersion: 10.0.19041.0
InstallerType: wix
Scope: machine
UpgradeBehavior: install
ProductCode: '$productCode'
AppsAndFeaturesEntries:
- UpgradeCode: '$UpgradeCode'
Installers:
- Architecture: x64
  InstallerUrl: $url
  InstallerSha256: $sha256
ManifestType: installer
ManifestVersion: 1.6.0
"@
Write-Text (Join-Path $winget "$Id.locale.en-US.yaml") @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.defaultLocale.1.6.0.schema.json
PackageIdentifier: $Id
PackageVersion: $version
PackageLocale: en-US
Publisher: files
PublisherUrl: $Repo
PublisherSupportUrl: $Repo/issues
PackageName: files
PackageUrl: $Repo
License: MIT
LicenseUrl: $Repo/blob/master/LICENSE
ShortDescription: $Summary
Tags:
- search
- file-search
- network-share
- smb
ReleaseNotesUrl: $Repo/releases/tag/v$version
ManifestType: defaultLocale
ManifestVersion: 1.6.0
"@

$choco = Join-Path $Out "chocolatey\$ChocoId"
Write-Text (Join-Path $choco "$ChocoId.nuspec") @"
<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2015/06/nuspec.xsd">
  <metadata>
    <id>$ChocoId</id>
    <version>$version</version>
    <title>files</title>
    <authors>Edward Ibarra</authors>
    <owners>ibarra000</owners>
    <projectUrl>$Repo</projectUrl>
    <licenseUrl>$Repo/blob/master/LICENSE</licenseUrl>
    <requireLicenseAcceptance>false</requireLicenseAcceptance>
    <projectSourceUrl>$Repo</projectSourceUrl>
    <bugTrackerUrl>$Repo/issues</bugTrackerUrl>
    <releaseNotes>$Repo/releases/tag/v$version</releaseNotes>
    <tags>search file-search network-share smb</tags>
    <summary>$Summary</summary>
    <description>$Summary A global hotkey opens a small panel; matching files from every folder you added show up as you type, even across millions of names, and search keeps working while a share is offline.</description>
  </metadata>
  <files>
    <file src="tools\**" target="tools" />
  </files>
</package>
"@
# Uninstall needs no script: Chocolatey's automatic uninstaller removes the MSI by its Apps & Features entry.
Write-Text (Join-Path $choco 'tools\chocolateyinstall.ps1') @"
`$ErrorActionPreference = 'Stop'
`$packageArgs = @{
  packageName    = `$env:ChocolateyPackageName
  fileType       = 'msi'
  url64bit       = '$url'
  checksum64     = '$sha256'
  checksumType64 = 'sha256'
  silentArgs     = '/qn /norestart'
  validExitCodes = @(0, 3010, 1641)
}
Install-ChocolateyPackage @packageArgs
"@

Write-Host "wrote winget and Chocolatey packages for $version under $Out"
Write-Host 'After the GitHub release is published, submit them (not done here):'
Write-Host "  winget validate --manifest `"$winget`""
Write-Host "  wingetcreate submit `"$winget`"            # opens a PR on microsoft/winget-pkgs"
Write-Host "  choco pack `"$(Join-Path $choco "$ChocoId.nuspec")`" --out `"$choco`""
Write-Host "  choco push `"$(Join-Path $choco "$ChocoId.$version.nupkg")`" --source https://push.chocolatey.org/"
