#Requires -Version 5.1
<#
.SYNOPSIS
    Thurbox installer for native Windows (PowerShell).

.DESCRIPTION
    Downloads the latest thurbox release for Windows, verifies its SHA256
    checksum, extracts its programs (thurbox, thurbox-cli, fleet, runtime-mux,
    runtime-herdr, workspace-git) into an install directory, and adds that
    directory to the user PATH.

    -FleetOnly installs Fleet alone, headless: fleet, runtime-mux,
    runtime-herdr and workspace-git, without thurbox or thurbox-cli.

    Mirrors scripts/install.sh (Linux/macOS). Windows uses psmux as the terminal
    multiplexer (a native, drop-in tmux replacement) instead of tmux.

.EXAMPLE
    # One-liner (pipe to PowerShell):
    irm https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.ps1 | iex

.EXAMPLE
    # Pin a version / custom dir (run as a file):
    .\install.ps1 -Version v0.1.0 -InstallDir C:\tools\thurbox

.EXAMPLE
    # Fleet alone, without thurbox (leaves no variable behind in the session):
    & ([scriptblock]::Create((irm https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.ps1))) -FleetOnly

.NOTES
    Configuration can also be supplied via environment variables, which is the
    reliable path for the pipe-to-iex form that cannot pass parameters:
      $env:THURBOX_VERSION      = 'v0.1.0'
      $env:THURBOX_INSTALL_DIR  = 'C:\tools\thurbox'
      $env:THURBOX_REPO         = 'Thurbeen/thurbox'
      $env:THURBOX_DOWNLOAD_URL = 'https://mirror.example/releases/download'
      $env:THURBOX_FLEET_ONLY   = '1'
#>

[CmdletBinding()]
param(
    [string]$Version,
    [string]$InstallDir,
    [string]$Repo,
    [switch]$FleetOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not $Repo)    { $Repo    = if ($env:THURBOX_REPO)    { $env:THURBOX_REPO }    else { 'Thurbeen/thurbox' } }
if (-not $Version) { $Version = if ($env:THURBOX_VERSION) { $env:THURBOX_VERSION } else { '' } }
$DownloadUrl = if ($env:THURBOX_DOWNLOAD_URL) { $env:THURBOX_DOWNLOAD_URL } else { "https://github.com/$Repo/releases/download" }
$Binaries = @('thurbox', 'thurbox-cli', 'fleet', 'runtime-mux', 'runtime-herdr', 'workspace-git')
$FleetBinaries = @('fleet', 'runtime-mux', 'runtime-herdr', 'workspace-git')
$FleetOnlyFromEnv = $false
if (-not $PSBoundParameters.ContainsKey('FleetOnly') -and $env:THURBOX_FLEET_ONLY -and $env:THURBOX_FLEET_ONLY -ne '0') {
    $FleetOnly = [switch]$true
    $FleetOnlyFromEnv = $true
}
$Programs = if ($FleetOnly) { $FleetBinaries } else { $Binaries }
if (-not $InstallDir) {
    if ($env:THURBOX_INSTALL_DIR) { $InstallDir = $env:THURBOX_INSTALL_DIR }
    elseif ($env:LOCALAPPDATA)    { $InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\thurbox' }
    else                          { $InstallDir = Join-Path $HOME '.thurbox\bin' }
}

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

function Get-Target {
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

function Get-LatestVersion {
    if ($Version) { return $Version }

    try {
        $rel = Invoke-RestMethod -UseBasicParsing -Uri "https://api.github.com/repos/$Repo/releases/latest" `
            -Headers @{ 'User-Agent' = 'thurbox-installer' }
        if ($rel.tag_name) { return $rel.tag_name }
    } catch {
        Write-Step "GitHub API unavailable ($($_.Exception.Message)); scraping releases page..."
    }

    try {
        $page = Invoke-WebRequest -UseBasicParsing -Uri "https://github.com/$Repo/releases"
        $m = [regex]::Match($page.Content, 'releases/tag/(v[0-9][0-9A-Za-z.\-+]*)')
        if ($m.Success) { return $m.Groups[1].Value }
    } catch {
        Write-Verbose "Releases-page scrape failed: $($_.Exception.Message)"
    }

    throw "Could not determine the latest version. Pin one with -Version v0.1.0 (or `$env:THURBOX_VERSION)."
}

function Get-ExpectedChecksum {
    param([string]$ChecksumFile, [string]$ArchiveName)
    foreach ($line in Get-Content $ChecksumFile) {
        if ($line -match "([0-9a-fA-F]{64})\s+\*?(.*$([regex]::Escape($ArchiveName)))\s*$") {
            return $Matches[1].ToLower()
        }
    }
    throw "Checksum for $ArchiveName not found in checksums file."
}

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
    $env:Path = "$env:Path;$Dir"
    return $true
}

function Get-LockingProcess {
    param([string[]]$Path)
    Get-Process -Name $Binaries -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and ($Path -contains $_.Path) }
}

# Windows refuses to delete a running executable but lets it be renamed, so each one moves aside.
function Install-Archive {
    param([string]$ZipPath, [string]$Destination, [string[]]$Only)
    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
    $Destination = (Resolve-Path -LiteralPath $Destination).ProviderPath
    $stagingOnDestinationVolume = Join-Path $Destination ('.install-' + [System.Guid]::NewGuid().ToString('N'))
    try {
        Expand-Archive -LiteralPath $ZipPath -DestinationPath $stagingOnDestinationVolume
        $files = @(Get-ChildItem -LiteralPath $stagingOnDestinationVolume -File)
        if ($Only) {
            $files = @($files | Where-Object { $Only -contains $_.BaseName -and $_.Extension -eq '.exe' })
            if (-not ($files | Where-Object { $_.BaseName -eq 'fleet' })) {
                throw 'This release has no Fleet. Install a 3.x release: $env:THURBOX_VERSION = "v3.x.y"'
            }
        }
        foreach ($file in $files) {
            $target = Join-Path $Destination $file.Name
            $backup = Join-Path $Destination ".$($file.Name).old"
            Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue
            if (Test-Path -LiteralPath $target) {
                try {
                    Move-Item -LiteralPath $target -Destination $backup -Force
                } catch {
                    $using = @(Get-LockingProcess -Path $target, $backup |
                        ForEach-Object { "$($_.ProcessName) (PID $($_.Id))" })
                    $who = if ($using) { $using -join ', ' } else { 'another program' }
                    throw "Cannot replace $target - it is in use by $who. Close it and run the installer again."
                }
            }
            try {
                Move-Item -LiteralPath $file.FullName -Destination $target
            } catch {
                if ((Test-Path -LiteralPath $backup) -and -not (Test-Path -LiteralPath $target)) {
                    Move-Item -LiteralPath $backup -Destination $target -ErrorAction SilentlyContinue
                }
                throw
            }
            Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue
        }
    }
    finally {
        Remove-Item -LiteralPath $stagingOnDestinationVolume -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Show-NextSteps {
    param([string]$Dir, [switch]$FleetOnly)
    Write-Host "`nNext steps" -ForegroundColor Magenta
    Write-Step '* Install psmux (the Windows multiplexer): https://github.com/psmux/psmux'
    Write-Step '* Install a coding-agent CLI (claude, codex, antigravity, opencode, aider, ...)'
    if ($FleetOnly) {
        Write-Step '* Set up Fleet:      fleet init'
        Write-Step '* Check the setup:   fleet doctor'
        Write-Step '* Run it:            fleet up'
        Write-Step '* Hand it a Task:    fleet apply -f task.json'
        return
    }
    if (Test-Path -LiteralPath (Join-Path $Dir 'fleet.exe')) {
        Write-Step '* Set up Fleet:      fleet init'
    } else {
        Write-Warn 'This release has no Fleet. thurbox v3 needs a 3.x release: $env:THURBOX_VERSION = "v3.x.y"'
    }
    Write-Step '* Check the setup:   thurbox-cli doctor'
    Write-Step '* Launch the TUI:    thurbox'
}

function Invoke-Install {
    Show-Banner

    $target = Get-Target
    Write-Info "Target:   $target"

    $ver = Get-LatestVersion
    Write-Info "Version:  $ver"

    $archive = "thurbox-$ver-$target.zip"
    $base = "$DownloadUrl/$ver"
    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("thurbox-install-" + [System.Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp -Force | Out-Null

    try {
        Write-Info 'Downloading checksums...'
        $checksumPath = Join-Path $tmp 'checksums.txt'
        $v3Sums = "thurbox-$($ver.TrimStart('v'))-sha256sums.txt"
        $v2Sums = "thurbox-$ver-checksums.txt"
        $sums = @($v3Sums, $v2Sums)
        $fetched = $false
        foreach ($name in $sums) {
            try {
                Invoke-WebRequest -UseBasicParsing -Uri "$base/$name" -OutFile $checksumPath
                $fetched = $true
                break
            } catch {
                continue
            }
        }
        if (-not $fetched) {
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
        $only = if ($FleetOnly) { $Programs } else { $null }
        Install-Archive -ZipPath $zipPath -Destination $InstallDir -Only $only
    }
    finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }

    Write-Host ''
    Write-Ok "$(if ($FleetOnly) { 'Fleet' } else { 'Thurbox' }) installed to $InstallDir"
    foreach ($bin in $Programs) {
        if (Test-Path -LiteralPath (Join-Path $InstallDir "$bin.exe")) { Write-Step "  $bin" }
    }

    if (Add-ToUserPath $InstallDir) {
        Write-Warn "Added $InstallDir to your user PATH - restart your terminal for it to take effect."
    }

    Write-Warn 'Windows support is experimental for now - expect rough edges and please report issues.'

    Show-NextSteps -Dir $InstallDir -FleetOnly:$FleetOnly
    if ($FleetOnlyFromEnv) {
        Write-Warn 'THURBOX_FLEET_ONLY is set, so thurbox was not installed. Remove it for a full install: Remove-Item Env:\THURBOX_FLEET_ONLY'
    }
    Write-Host ''
    Write-Ok 'Installation complete! Happy hacking.'
}

if (-not $env:THURBOX_PS_TEST) {
    Invoke-Install
}
