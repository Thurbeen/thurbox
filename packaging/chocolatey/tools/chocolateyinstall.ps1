$ErrorActionPreference = 'Stop'

$packageName = 'thurbox'
$toolsDir    = Split-Path -Parent $MyInvocation.MyCommand.Definition

$version    = '0.79.46'
$url64      = 'https://github.com/Thurbeen/thurbox/releases/download/v0.79.46/thurbox-v0.79.46-x86_64-pc-windows-msvc.zip'
$checksum64 = '0000000000000000000000000000000000000000000000000000000000000000'

$packageArgs = @{
  packageName    = $packageName
  unzipLocation  = $toolsDir
  url64bit       = $url64
  checksum64     = $checksum64
  checksumType64 = 'sha256'
}

Install-ChocolateyZipPackage @packageArgs

# psmux has no Chocolatey package, so it cannot be a <dependency>: warn instead.
if (-not (Get-Command 'psmux' -ErrorAction SilentlyContinue)) {
  Write-Warning ("thurbox needs psmux (a native-Windows tmux clone) on your PATH " +
    "to run sessions, and it was not found. Install it from " +
    "https://github.com/psmux/psmux and reopen your terminal.")
}
