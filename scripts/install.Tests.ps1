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
        @{ Name = 'Invoke-Install' }
        @{ Name = 'Show-Banner' }
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
