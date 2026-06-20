#Requires -Version 5.1
<#
.SYNOPSIS
    Thurbox installer for native Windows (PowerShell).

.DESCRIPTION
    Downloads the latest thurbox release for Windows, verifies its SHA256
    checksum, extracts thurbox.exe / thurbox-cli.exe into an install directory,
    and adds that directory to the user PATH.

    Mirrors scripts/install.sh (Linux/macOS). Windows uses psmux as the terminal
    multiplexer (a native, drop-in tmux replacement) instead of tmux.

.EXAMPLE
    # One-liner (pipe to PowerShell):
    irm https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.ps1 | iex

.EXAMPLE
    # Pin a version / custom dir (run as a file):
    .\install.ps1 -Version v0.1.0 -InstallDir C:\tools\thurbox

.NOTES
    Configuration can also be supplied via environment variables, which is the
    reliable path for the pipe-to-iex form that cannot pass parameters:
      $env:THURBOX_VERSION      = 'v0.1.0'
      $env:THURBOX_INSTALL_DIR  = 'C:\tools\thurbox'
      $env:THURBOX_REPO         = 'Thurbeen/thurbox'
#>

[CmdletBinding()]
param(
    [string]$Version,
    [string]$InstallDir,
    [string]$Repo
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# --- Configuration (parameter overrides env var overrides default) ----------
if (-not $Repo)    { $Repo    = if ($env:THURBOX_REPO)    { $env:THURBOX_REPO }    else { 'Thurbeen/thurbox' } }
if (-not $Version) { $Version = if ($env:THURBOX_VERSION) { $env:THURBOX_VERSION } else { '' } }
if (-not $InstallDir) {
    if ($env:THURBOX_INSTALL_DIR) { $InstallDir = $env:THURBOX_INSTALL_DIR }
    elseif ($env:LOCALAPPDATA)    { $InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\thurbox' }
    else                          { $InstallDir = Join-Path $HOME '.thurbox\bin' }  # degenerate fallback
}

# --- Pretty output ----------------------------------------------------------
function Write-Info    { param($m) Write-Host "> $m"  -ForegroundColor Cyan }
function Write-Step    { param($m) Write-Host "  $m"   -ForegroundColor DarkGray }
function Write-Ok      { param($m) Write-Host "ok $m"  -ForegroundColor Green }
function Write-Warn    { param($m) Write-Host "!  $m"  -ForegroundColor Yellow }
function Write-Err     { param($m) Write-Host "x  Error: $m" -ForegroundColor Red }

function Show-Banner {
    Write-Host @'
   _____ _   _ _   _____________  _______   __
  |_   _| | | | | | | ___ \ ___ \|  _  \ \ / /
    | | | |_| | | | | |_/ / |_/ /| | | |\ V /
    | | |  _  | | | |    /| ___ \| | | |/   \
    | | | | | | |_| | |\ \| |_/ /\ \_/ / /^\ \
    \_/ \_| |_/\___/\_| \_\____/  \___/\/   \/
'@ -ForegroundColor Magenta
    Write-Host "  multi-session coding-agent orchestrator`n" -ForegroundColor DarkGray
}

# --- Platform detection -----------------------------------------------------
function Get-Target {
    # ARM64 Windows runs x64 binaries under emulation, so x86_64 is the target
    # for every Windows arch until a native aarch64-pc-windows-msvc build ships.
    $arch = $env:PROCESSOR_ARCHITECTURE
    switch ($arch) {
        'AMD64' { return 'x86_64-pc-windows-msvc' }
        'ARM64' {
            Write-Warn 'ARM64 Windows detected - installing the x86_64 build (runs under emulation).'
            return 'x86_64-pc-windows-msvc'
        }
        'x86'   { throw '32-bit Windows is not supported.' }
        default { throw "Unsupported architecture: $arch" }
    }
}

# --- Version resolution -----------------------------------------------------
function Get-LatestVersion {
    if ($Version) { return $Version }

    # GitHub API first.
    try {
        $rel = Invoke-RestMethod -UseBasicParsing -Uri "https://api.github.com/repos/$Repo/releases/latest" `
            -Headers @{ 'User-Agent' = 'thurbox-installer' }
        if ($rel.tag_name) { return $rel.tag_name }
    } catch {
        Write-Step "GitHub API unavailable ($($_.Exception.Message)); scraping releases page..."
    }

    # Fallback: scrape the releases page for the newest tag.
    try {
        $page = Invoke-WebRequest -UseBasicParsing -Uri "https://github.com/$Repo/releases"
        $m = [regex]::Match($page.Content, 'releases/tag/(v[0-9][0-9A-Za-z.\-+]*)')
        if ($m.Success) { return $m.Groups[1].Value }
    } catch {
        Write-Verbose "Releases-page scrape failed: $($_.Exception.Message)"
    }

    throw "Could not determine the latest version. Pin one with -Version v0.1.0 (or `$env:THURBOX_VERSION)."
}

# --- Checksum verification --------------------------------------------------
function Get-ExpectedChecksum {
    param([string]$ChecksumFile, [string]$ArchiveName)
    foreach ($line in Get-Content $ChecksumFile) {
        # sha256sum format: "<hash>  <filename>"
        if ($line -match "([0-9a-fA-F]{64})\s+\*?(.*$([regex]::Escape($ArchiveName)))\s*$") {
            return $Matches[1].ToLower()
        }
    }
    throw "Checksum for $ArchiveName not found in checksums file."
}

# --- PATH management --------------------------------------------------------
function Add-ToUserPath {
    param([string]$Dir)
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = @()
    if ($userPath) { $entries = $userPath -split ';' | Where-Object { $_ -ne '' } }
    if ($entries -contains $Dir) {
        return $false
    }
    $newPath = (@($entries) + $Dir) -join ';'
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    # Reflect into the current session too.
    $env:Path = "$env:Path;$Dir"
    return $true
}

# --- Main -------------------------------------------------------------------
function Invoke-Install {
    Show-Banner

    $target = Get-Target
    Write-Info "Target:   $target"

    $ver = Get-LatestVersion
    Write-Info "Version:  $ver"

    $archive = "thurbox-$ver-$target.zip"
    $base = "https://github.com/$Repo/releases/download/$ver"
    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("thurbox-install-" + [System.Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp -Force | Out-Null

    try {
        Write-Info 'Downloading checksums...'
        $checksumPath = Join-Path $tmp 'checksums.txt'
        try {
            Invoke-WebRequest -UseBasicParsing -Uri "$base/thurbox-$ver-checksums.txt" -OutFile $checksumPath
        } catch {
            throw "Release assets for $ver are not available. Check https://github.com/$Repo/releases/tag/$ver"
        }

        Write-Info 'Downloading binary...'
        $zipPath = Join-Path $tmp $archive
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$archive" -OutFile $zipPath

        Write-Info 'Verifying checksum...'
        $expected = Get-ExpectedChecksum -ChecksumFile $checksumPath -ArchiveName $archive
        $actual = (Get-FileHash -Algorithm SHA256 -Path $zipPath).Hash.ToLower()
        if ($actual -ne $expected) {
            throw "Checksum mismatch for $archive`n  expected: $expected`n  actual:   $actual"
        }

        Write-Info "Installing to $InstallDir ..."
        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
        Expand-Archive -Path $zipPath -DestinationPath $InstallDir -Force
    }
    finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }

    Write-Host ''
    Write-Ok "Thurbox installed to $InstallDir\thurbox.exe"

    if (Add-ToUserPath $InstallDir) {
        Write-Warn "Added $InstallDir to your user PATH - restart your terminal for it to take effect."
    }

    Write-Host "`nNext steps" -ForegroundColor Magenta
    Write-Step '* Install psmux (the Windows multiplexer): https://github.com/psmux/psmux'
    Write-Step '* Install a coding-agent CLI (claude, codex, gemini, opencode, aider, ...)'
    Write-Step '* Launch the TUI:    thurbox'
    Write-Step '* Scriptable CLI:    thurbox-cli'
    Write-Host ''
    Write-Ok 'Installation complete! Happy hacking.'
}

# Run unless dot-sourced for testing ($env:THURBOX_PS_TEST set).
if (-not $env:THURBOX_PS_TEST) {
    Invoke-Install
}
