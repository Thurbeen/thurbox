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

# --- Configuration (parameter overrides env var overrides default) ----------
if (-not $Repo)    { $Repo    = if ($env:THURBOX_REPO)    { $env:THURBOX_REPO }    else { 'Thurbeen/thurbox' } }
if (-not $Version) { $Version = if ($env:THURBOX_VERSION) { $env:THURBOX_VERSION } else { '' } }
# Where the release assets are fetched from; a mirror, or a local copy in tests.
$DownloadUrl = if ($env:THURBOX_DOWNLOAD_URL) { $env:THURBOX_DOWNLOAD_URL } else { "https://github.com/$Repo/releases/download" }
$Binaries = @('thurbox', 'thurbox-cli', 'fleet', 'runtime-mux', 'runtime-herdr', 'workspace-git')
$FleetBinaries = @('fleet', 'runtime-mux', 'runtime-herdr', 'workspace-git')
# The env var is a default: an explicit -FleetOnly or -FleetOnly:$false wins.
$FleetOnlyFromEnv = $false
if (-not $PSBoundParameters.ContainsKey('FleetOnly') -and $env:THURBOX_FLEET_ONLY -and $env:THURBOX_FLEET_ONLY -ne '0') {
    $FleetOnly = [switch]$true
    $FleetOnlyFromEnv = $true
}
# What this run installs.
$Programs = if ($FleetOnly) { $FleetBinaries } else { $Binaries }
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

# --- Extraction -------------------------------------------------------------
# The running processes of the shipped programs started from any of $Path, for
# the message that asks the user to close them. `fleet up`, the Fleet manager,
# runs for as long as sessions do, so it is the likeliest one. Path is $null
# for a process we may not inspect.
function Get-LockingProcess {
    param([string[]]$Path)
    Get-Process -Name $Binaries -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and ($Path -contains $_.Path) }
}

# Extract the release zip into $Destination, replacing what is there.
#
# Not Expand-Archive -Force straight into $Destination: that deletes each file
# it overwrites, and Windows refuses to delete an executable a process is
# running from, so updating while thurbox (or a thurbox-cli a hook started) was
# running died with Remove-Item's access-denied trace. Windows does let such a
# file be renamed, so each one is moved aside to .<name>.old - the backup name
# the in-app updater (src/update/self_update.rs) uses - and the new one moved
# in. A backup a process still runs from stays until the next update.
function Install-Archive {
    # -Only: the program stems to install; every file in the archive otherwise.
    param([string]$ZipPath, [string]$Destination, [string[]]$Only)
    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
    $Destination = (Resolve-Path -LiteralPath $Destination).ProviderPath
    # Inside $Destination so every move below is a rename on one volume.
    $staging = Join-Path $Destination ('.install-' + [System.Guid]::NewGuid().ToString('N'))
    try {
        Expand-Archive -LiteralPath $ZipPath -DestinationPath $staging
        $files = @(Get-ChildItem -LiteralPath $staging -File)
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
                # Leave the old file in place rather than nothing at all.
                if ((Test-Path -LiteralPath $backup) -and -not (Test-Path -LiteralPath $target)) {
                    Move-Item -LiteralPath $backup -Destination $target -ErrorAction SilentlyContinue
                }
                throw
            }
            Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue
        }
    }
    finally {
        Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# --- Next steps -------------------------------------------------------------
# `fleet init` only when the release shipped Fleet: a 2.x release has none.
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

# --- Main -------------------------------------------------------------------
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
        # A 3.x release's sums file is thurbox-<V>-sha256sums.txt: it never ships
        # thurbox-v<V>-checksums.txt, the name every 2.x updater fetches first,
        # so those updaters fail safe on it (docs/thurbox/releasing.md, "v2 and
        # v3"). A pinned 2.x release has only that old name.
        $sums = @("thurbox-$($ver.TrimStart('v'))-sha256sums.txt", "thurbox-$ver-checksums.txt")
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
    # A session that kept the variable from an earlier install must not skip thurbox silently.
    if ($FleetOnlyFromEnv) {
        Write-Warn 'THURBOX_FLEET_ONLY is set, so thurbox was not installed. Remove it for a full install: Remove-Item Env:\THURBOX_FLEET_ONLY'
    }
    Write-Host ''
    Write-Ok 'Installation complete! Happy hacking.'
}

# Run unless dot-sourced for testing ($env:THURBOX_PS_TEST set).
if (-not $env:THURBOX_PS_TEST) {
    Invoke-Install
}
