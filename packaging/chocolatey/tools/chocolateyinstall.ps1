$ErrorActionPreference = 'Stop'

$packageName = 'thurbox'
$toolsDir    = Split-Path -Parent $MyInvocation.MyCommand.Definition

# --- Bumped by packaging/chocolatey/bump-nuspec.py on every release. ---
# Keep these three as single-quoted, single-line assignments: the bump script
# rewrites each literal in place (mirrors the Homebrew bump-formula.py approach).
$version    = '0.79.46'
$url64      = 'https://github.com/Thurbeen/thurbox/releases/download/v0.79.46/thurbox-v0.79.46-x86_64-pc-windows-msvc.zip'
$checksum64 = '0000000000000000000000000000000000000000000000000000000000000000'
# --- end bumped block ---

$packageArgs = @{
  packageName    = $packageName
  unzipLocation  = $toolsDir
  url64bit       = $url64
  checksum64     = $checksum64
  checksumType64 = 'sha256'
}

# Extracts the release zip (thurbox.exe, thurbox-cli.exe, LICENSE) into the
# package's tools dir; Chocolatey auto-shims the .exe files onto PATH. The
# extensionless LICENSE is ignored by shimming.
Install-ChocolateyZipPackage @packageArgs

# thurbox drives psmux (a native-Windows tmux clone) as its session backend.
# There is no Chocolatey package for psmux, so it can't be a hard <dependency>;
# warn at install time if it isn't already on PATH (UX hint, never fatal).
if (-not (Get-Command 'psmux' -ErrorAction SilentlyContinue)) {
  Write-Warning ("thurbox needs psmux (a native-Windows tmux clone) on your PATH " +
    "to run sessions, and it was not found. Install it from " +
    "https://github.com/psmux/psmux and reopen your terminal.")
}
