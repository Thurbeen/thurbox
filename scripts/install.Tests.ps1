#Requires -Version 5.1
<#
.SYNOPSIS
    Pester tests for scripts/install.ps1.

.DESCRIPTION
    Mirrors the coverage style of scripts/install.bats: structural assertions
    (the helpers exist, the source is ASCII-only / BOM-free / parses) plus
    behavioral tests for the two pure helpers exposed for testing -
    Get-Target and Get-ExpectedChecksum.

    install.ps1 guards the installer behind $env:THURBOX_PS_TEST, so dot-sourcing
    it here defines every function without running Invoke-Install.

.EXAMPLE
    Invoke-Pester -Path scripts/install.Tests.ps1

.NOTES
    Requires Pester 5+. The helpers are platform-independent (Get-Target only
    reads $env:PROCESSOR_ARCHITECTURE, which the tests set explicitly), so this
    runs on any OS where PowerShell is available - not just native Windows.
#>

BeforeAll {
    $script:ScriptPath = Join-Path $PSScriptRoot 'install.ps1'
    $env:THURBOX_PS_TEST = '1'
    # Dot-source so the helper functions land in this scope; the
    # THURBOX_PS_TEST guard keeps Invoke-Install from firing.
    . $script:ScriptPath
}

AfterAll {
    Remove-Item Env:\THURBOX_PS_TEST -ErrorAction SilentlyContinue
}

Describe 'install.ps1 source' {
    It 'exists' {
        Test-Path $script:ScriptPath | Should -BeTrue
    }

    It 'has valid PowerShell syntax' {
        $tokens = $null
        $errors = $null
        [System.Management.Automation.Language.Parser]::ParseFile(
            $script:ScriptPath, [ref]$tokens, [ref]$errors) | Out-Null
        $errors | Should -BeNullOrEmpty
    }

    It 'is ASCII-only (survives irm | iex on Windows PowerShell 5.1)' {
        $bytes = [System.IO.File]::ReadAllBytes($script:ScriptPath)
        ($bytes | Where-Object { $_ -gt 127 } | Measure-Object).Count | Should -Be 0
    }

    It 'has no UTF-8 BOM' {
        $bytes = [System.IO.File]::ReadAllBytes($script:ScriptPath)
        ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) |
            Should -BeFalse
    }

    It 'guards the installer behind $env:THURBOX_PS_TEST' {
        (Get-Content $script:ScriptPath -Raw) | Should -Match 'THURBOX_PS_TEST'
    }

    It 'defines the <Name> function' -ForEach @(
        @{ Name = 'Get-Target' }
        @{ Name = 'Get-LatestVersion' }
        @{ Name = 'Get-ExpectedChecksum' }
        @{ Name = 'Add-ToUserPath' }
        @{ Name = 'Install-Archive' }
        @{ Name = 'Invoke-Install' }
        @{ Name = 'Show-Banner' }
        @{ Name = 'Set-Layout' }
    ) {
        Get-Command -Name $Name -CommandType Function -ErrorAction SilentlyContinue |
            Should -Not -BeNullOrEmpty
    }
}

Describe 'Get-Target' {
    BeforeAll {
        $script:SavedArch = $env:PROCESSOR_ARCHITECTURE
    }

    AfterAll {
        $env:PROCESSOR_ARCHITECTURE = $script:SavedArch
    }

    It 'maps AMD64 to the x86_64 MSVC target' {
        $env:PROCESSOR_ARCHITECTURE = 'AMD64'
        Get-Target | Should -Be 'x86_64-pc-windows-msvc'
    }

    It 'maps ARM64 to the x86_64 build (runs under emulation)' {
        $env:PROCESSOR_ARCHITECTURE = 'ARM64'
        Get-Target | Should -Be 'x86_64-pc-windows-msvc'
    }

    It 'rejects 32-bit Windows' {
        $env:PROCESSOR_ARCHITECTURE = 'x86'
        { Get-Target } | Should -Throw '*32-bit*'
    }

    It 'rejects an unknown architecture' {
        $env:PROCESSOR_ARCHITECTURE = 'SPARC'
        { Get-Target } | Should -Throw '*Unsupported architecture*'
    }
}

Describe 'Get-ExpectedChecksum' {
    BeforeAll {
        $script:Archive = 'thurbox-v1.2.3-x86_64-pc-windows-msvc.zip'
        $script:Hash    = '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef'
    }

    BeforeEach {
        $script:Checksums = New-TemporaryFile
    }

    AfterEach {
        Remove-Item $script:Checksums -ErrorAction SilentlyContinue
    }

    It 'returns the hash for the matching archive' {
        Set-Content -Path $script:Checksums -Value "$script:Hash  $script:Archive"
        Get-ExpectedChecksum -ChecksumFile $script:Checksums -ArchiveName $script:Archive |
            Should -Be $script:Hash
    }

    It 'lowercases an uppercase hash' {
        $upper = $script:Hash.ToUpper()
        Set-Content -Path $script:Checksums -Value "$upper  $script:Archive"
        Get-ExpectedChecksum -ChecksumFile $script:Checksums -ArchiveName $script:Archive |
            Should -Be $script:Hash
    }

    It 'accepts the sha256sum binary marker (asterisk before the name)' {
        Set-Content -Path $script:Checksums -Value "$script:Hash *$script:Archive"
        Get-ExpectedChecksum -ChecksumFile $script:Checksums -ArchiveName $script:Archive |
            Should -Be $script:Hash
    }

    It 'selects the correct line among several entries' {
        $other = 'a' * 64
        Set-Content -Path $script:Checksums -Value @(
            "$other  thurbox-v1.2.3-x86_64-unknown-linux-musl.tar.gz"
            "$script:Hash  $script:Archive"
            "$other  thurbox-v1.2.3-aarch64-apple-darwin.tar.gz"
        )
        Get-ExpectedChecksum -ChecksumFile $script:Checksums -ArchiveName $script:Archive |
            Should -Be $script:Hash
    }

    It 'throws when the archive is absent from the checksums file' {
        Set-Content -Path $script:Checksums -Value "$script:Hash  some-other-file.zip"
        { Get-ExpectedChecksum -ChecksumFile $script:Checksums -ArchiveName $script:Archive } |
            Should -Throw "*$script:Archive*"
    }
}

BeforeDiscovery {
    # -Skip is read at discovery, before any BeforeAll. $IsWindows does not
    # exist on Windows PowerShell 5.1, which only runs on Windows.
    $script:OnWindows = ($PSVersionTable.PSEdition -eq 'Desktop') -or $IsWindows
}

Describe 'Install-Archive' {
    BeforeAll {
        function New-ReleaseZip {
            param([string]$Dir, [string]$Payload)
            $src = Join-Path $Dir 'zip-src'
            New-Item -ItemType Directory -Path $src -Force | Out-Null
            foreach ($name in 'thurbox.exe', 'thurbox-cli.exe') {
                Set-Content -Path (Join-Path $src $name) -Value $Payload -NoNewline
            }
            $zip = Join-Path $Dir 'release.zip'
            Compress-Archive -Path (Join-Path $src '*') -DestinationPath $zip -Force
            return $zip
        }
    }

    BeforeEach {
        $script:Work = Join-Path ([System.IO.Path]::GetTempPath()) ('thurbox-test-' + [guid]::NewGuid().ToString('N'))
        $script:Dest = Join-Path $script:Work 'install'
        New-Item -ItemType Directory -Path $script:Dest -Force | Out-Null
        $script:Zip = New-ReleaseZip -Dir $script:Work -Payload 'new'
    }

    AfterEach {
        Remove-Item -Recurse -Force $script:Work -ErrorAction SilentlyContinue
    }

    It 'installs into an empty directory' {
        Install-Archive -ZipPath $script:Zip -Destination $script:Dest
        Get-Content -Raw (Join-Path $script:Dest 'thurbox.exe') | Should -Be 'new'
        Get-Content -Raw (Join-Path $script:Dest 'thurbox-cli.exe') | Should -Be 'new'
    }

    It 'replaces an existing install and leaves no backup behind' {
        foreach ($name in 'thurbox.exe', 'thurbox-cli.exe') {
            Set-Content -Path (Join-Path $script:Dest $name) -Value 'old' -NoNewline
        }
        Install-Archive -ZipPath $script:Zip -Destination $script:Dest
        Get-Content -Raw (Join-Path $script:Dest 'thurbox.exe') | Should -Be 'new'
        @(Get-ChildItem -Force $script:Dest -Filter '*.old').Count | Should -Be 0
    }

    It 'replaces thurbox.exe while it is running' -Skip:(-not $script:OnWindows) {
        # A real executable, so Windows maps it the way it maps a running
        # thurbox: deleting it is refused, renaming it is not.
        $exe = Join-Path $script:Dest 'thurbox.exe'
        Copy-Item (Join-Path $env:SystemRoot 'System32\PING.EXE') $exe
        $proc = Start-Process -FilePath $exe -ArgumentList '-n', '60', '127.0.0.1' -WindowStyle Hidden -PassThru
        try {
            Install-Archive -ZipPath $script:Zip -Destination $script:Dest
            Get-Content -Raw $exe | Should -Be 'new'
        }
        finally {
            Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
            $proc.WaitForExit()
        }
    }

    It 'names the process to close when the backup is still running too' -Skip:(-not $script:OnWindows) {
        $exe = Join-Path $script:Dest 'thurbox.exe'
        $ping = Join-Path $env:SystemRoot 'System32\PING.EXE'
        Copy-Item $ping $exe
        $first = Start-Process -FilePath $exe -ArgumentList '-n', '60', '127.0.0.1' -WindowStyle Hidden -PassThru
        $second = $null
        try {
            # The first update moves the running image to .thurbox.exe.old ...
            Install-Archive -ZipPath $script:Zip -Destination $script:Dest
            # ... and a second thurbox, started from the new binary, then holds
            # both files the next update has to move.
            Copy-Item $ping $exe -Force
            $second = Start-Process -FilePath $exe -ArgumentList '-n', '60', '127.0.0.1' -WindowStyle Hidden -PassThru
            { Install-Archive -ZipPath $script:Zip -Destination $script:Dest } |
                Should -Throw "*in use by thurbox (PID *$($first.Id)*Close it and run the installer again*"
        }
        finally {
            foreach ($p in @($first, $second) | Where-Object { $_ }) {
                Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
                $p.WaitForExit()
            }
        }
    }

    It 'puts the installed file back when the new one cannot be moved in' {
        $exe = Join-Path $script:Dest 'thurbox.exe'
        Set-Content -Path $exe -Value 'old' -NoNewline
        # Only the move of the new thurbox.exe fails - after the installed one
        # has been moved aside, which is the state that must not be left.
        $real = Get-Command Move-Item -CommandType Cmdlet
        Mock Move-Item { & $real @PesterBoundParameters }
        Mock Move-Item { throw 'simulated failure' } -ParameterFilter {
            $LiteralPath -like '*.install-*' -and (Split-Path -Leaf $LiteralPath) -eq 'thurbox.exe'
        }
        { Install-Archive -ZipPath $script:Zip -Destination $script:Dest } |
            Should -Throw '*simulated failure*'
        Get-Content -Raw $exe | Should -Be 'old'
    }

    It 'removes the backup a previous update left once nothing runs from it' {
        $old = Join-Path $script:Dest '.thurbox.exe.old'
        Set-Content -Path $old -Value 'stale' -NoNewline
        Install-Archive -ZipPath $script:Zip -Destination $script:Dest
        Test-Path $old | Should -BeFalse
    }
}

Describe 'Set-Layout' {
    It 'takes the preset from THURBOX_LAYOUT when no -Layout is passed' {
        $env:THURBOX_LAYOUT = 'split-shell'
        try {
            . $script:ScriptPath
            $Layout | Should -Be 'split-shell'
        }
        finally {
            Remove-Item Env:\THURBOX_LAYOUT -ErrorAction SilentlyContinue
        }
    }

    It 'does nothing when thurbox-cli is not installed there' {
        $empty = Join-Path ([System.IO.Path]::GetTempPath()) ('thurbox-layout-' + [System.Guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $empty -Force | Out-Null
        try {
            $script:Layout = 'split-shell'
            { Set-Layout $empty } | Should -Not -Throw
        }
        finally {
            Remove-Item -Recurse -Force $empty -ErrorAction SilentlyContinue
        }
    }
}
