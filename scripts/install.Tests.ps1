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
    # The one list of shipped programs, as scripts/ci/release-archive.sh packs it.
    $packer = Get-Content -Raw (Join-Path $PSScriptRoot 'ci/release-archive.sh')
    $m = [regex]::Match($packer, '(?m)^BINARIES=\(([^)]*)\)')
    $script:Shipped = @($m.Groups[1].Value.Trim() -split '\s+')
    # And the part of it Fleet runs without thurbox.
    $m = [regex]::Match($packer, '(?m)^FLEET_BINARIES=\(([^)]*)\)')
    $script:FleetShipped = @($m.Groups[1].Value.Trim() -split '\s+')
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
        @{ Name = 'Show-NextSteps' }
    ) {
        Get-Command -Name $Name -CommandType Function -ErrorAction SilentlyContinue |
            Should -Not -BeNullOrEmpty
    }
}

Describe 'The shipped programs' {
    It 'are the ones the release archive packs' {
        $script:Shipped.Count | Should -BeGreaterThan 1
        $Binaries -join ' ' | Should -Be ($script:Shipped -join ' ')
    }

    It 'name the Fleet programs the release archive names' {
        $script:FleetShipped.Count | Should -BeGreaterThan 1
        $FleetBinaries -join ' ' | Should -Be ($script:FleetShipped -join ' ')
    }
}

Describe 'Show-NextSteps' {
    BeforeEach {
        $script:Dir = Join-Path ([System.IO.Path]::GetTempPath()) ('thurbox-test-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:Dir -Force | Out-Null
        $script:Printed = [System.Collections.Generic.List[string]]::new()
        Mock Write-Host { $script:Printed.Add([string]$Object) }
    }

    AfterEach {
        Remove-Item -Recurse -Force $script:Dir -ErrorAction SilentlyContinue
    }

    It 'points at fleet init when fleet was installed' {
        Set-Content -Path (Join-Path $script:Dir 'fleet.exe') -Value ''
        Show-NextSteps -Dir $script:Dir
        ($script:Printed -join "`n") | Should -Match 'fleet init'
        ($script:Printed -join "`n") | Should -Not -Match 'no Fleet'
    }

    It 'names only Fleet''s steps for a fleet-only install' {
        Set-Content -Path (Join-Path $script:Dir 'fleet.exe') -Value ''
        Show-NextSteps -Dir $script:Dir -FleetOnly
        $text = $script:Printed -join "`n"
        foreach ($step in 'fleet init', 'fleet up', 'fleet apply', 'fleet doctor') {
            $text | Should -Match $step
        }
        $text | Should -Not -Match 'thurbox-cli'
        $text | Should -Not -Match 'Launch the TUI'
    }

    It 'says a 3.x release is needed when the release has no fleet' {
        Show-NextSteps -Dir $script:Dir
        ($script:Printed -join "`n") | Should -Not -Match 'fleet init'
        ($script:Printed -join "`n") | Should -Match 'no Fleet'
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
            foreach ($name in $script:Shipped) {
                Set-Content -Path (Join-Path $src "$name.exe") -Value $Payload -NoNewline
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

    It 'installs every program the release ships into an empty directory' {
        Install-Archive -ZipPath $script:Zip -Destination $script:Dest
        foreach ($name in $script:Shipped) {
            Get-Content -Raw (Join-Path $script:Dest "$name.exe") | Should -Be 'new'
        }
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

    It 'names a running Fleet manager when its backup is still running too' -Skip:(-not $script:OnWindows) {
        # `fleet up` runs as long as sessions do, so it is the program an
        # update most often finds running.
        $exe = Join-Path $script:Dest 'fleet.exe'
        $ping = Join-Path $env:SystemRoot 'System32\PING.EXE'
        Copy-Item $ping $exe
        $first = Start-Process -FilePath $exe -ArgumentList '-n', '60', '127.0.0.1' -WindowStyle Hidden -PassThru
        $second = $null
        try {
            Install-Archive -ZipPath $script:Zip -Destination $script:Dest
            Copy-Item $ping $exe -Force
            $second = Start-Process -FilePath $exe -ArgumentList '-n', '60', '127.0.0.1' -WindowStyle Hidden -PassThru
            { Install-Archive -ZipPath $script:Zip -Destination $script:Dest } |
                Should -Throw "*in use by fleet (PID *$($first.Id)*Close it and run the installer again*"
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

Describe 'A fleet-only install' {
    BeforeAll {
        $script:SavedArch = $env:PROCESSOR_ARCHITECTURE
        $env:PROCESSOR_ARCHITECTURE = 'AMD64'
    }

    AfterAll {
        $env:PROCESSOR_ARCHITECTURE = $script:SavedArch
        Remove-Item Env:\THURBOX_FLEET_ONLY, Env:\THURBOX_VERSION, Env:\THURBOX_INSTALL_DIR -ErrorAction SilentlyContinue
        # Back to the full install every other block expects.
        . $script:ScriptPath
    }

    BeforeEach {
        # A release of every shipped program, served as the release assets are.
        $script:Work = Join-Path ([System.IO.Path]::GetTempPath()) ('thurbox-test-' + [guid]::NewGuid().ToString('N'))
        $src = Join-Path $script:Work 'src'
        $script:Assets = Join-Path $script:Work 'assets'
        $script:Dest = Join-Path $script:Work 'install'
        New-Item -ItemType Directory -Path $src, $script:Assets -Force | Out-Null
        foreach ($name in $script:Shipped) {
            Set-Content -Path (Join-Path $src "$name.exe") -Value 'new' -NoNewline
        }
        Set-Content -Path (Join-Path $src 'LICENSE') -Value 'license'
        $zip = Join-Path $script:Assets 'thurbox-v9.9.9-x86_64-pc-windows-msvc.zip'
        Compress-Archive -Path (Join-Path $src '*') -DestinationPath $zip
        $hash = (Get-FileHash -Algorithm SHA256 -Path $zip).Hash.ToLower()
        Set-Content -Path (Join-Path $script:Assets 'thurbox-9.9.9-sha256sums.txt') `
            -Value "$hash  thurbox-v9.9.9-x86_64-pc-windows-msvc.zip"
        $env:THURBOX_VERSION = 'v9.9.9'
        $env:THURBOX_INSTALL_DIR = $script:Dest
        Remove-Item Env:\THURBOX_FLEET_ONLY -ErrorAction SilentlyContinue

        $script:Printed = [System.Collections.Generic.List[string]]::new()
        Mock Write-Host { $script:Printed.Add([string]$Object) }
        # A missing asset throws, as a 404 does.
        Mock Invoke-WebRequest { Copy-Item (Join-Path $script:Assets (Split-Path -Leaf $Uri)) $OutFile -ErrorAction Stop }
        Mock Add-ToUserPath { $false }
    }

    AfterEach {
        Remove-Item -Recurse -Force $script:Work -ErrorAction SilentlyContinue
    }

    It 'installs only Fleet''s programs and names only Fleet''s steps, from <Form>' -ForEach @(
        @{ Form = '-FleetOnly' }
        @{ Form = 'THURBOX_FLEET_ONLY=1' }
    ) {
        if ($Form -eq '-FleetOnly') {
            . $script:ScriptPath -FleetOnly
        } else {
            $env:THURBOX_FLEET_ONLY = '1'
            . $script:ScriptPath
        }
        Invoke-Install
        $installed = @(Get-ChildItem -LiteralPath $script:Dest -File | ForEach-Object { $_.Name } | Sort-Object)
        $installed -join ' ' | Should -Be (($script:FleetShipped | ForEach-Object { "$_.exe" } | Sort-Object) -join ' ')
        $text = $script:Printed -join "`n"
        foreach ($step in 'fleet init', 'fleet up', 'fleet apply', 'fleet doctor') {
            $text | Should -Match $step
        }
        $text | Should -Not -Match 'thurbox-cli'
        $text | Should -Not -Match 'Launch the TUI'
        # Only the env var can leave a session installing Fleet alone unasked.
        if ($Form -eq '-FleetOnly') {
            $text | Should -Not -Match 'THURBOX_FLEET_ONLY'
        } else {
            $text | Should -Match 'THURBOX_FLEET_ONLY is set'
        }
    }

    It 'installs every shipped program when THURBOX_FLEET_ONLY is 0' {
        $env:THURBOX_FLEET_ONLY = '0'
        . $script:ScriptPath
        Invoke-Install
        foreach ($name in $script:Shipped) {
            Test-Path -LiteralPath (Join-Path $script:Dest "$name.exe") | Should -BeTrue
        }
    }

    It 'lets an explicit -FleetOnly:$false win over THURBOX_FLEET_ONLY' {
        $env:THURBOX_FLEET_ONLY = '1'
        . $script:ScriptPath -FleetOnly:$false
        Invoke-Install
        foreach ($name in $script:Shipped) {
            Test-Path -LiteralPath (Join-Path $script:Dest "$name.exe") | Should -BeTrue
        }
    }

    # A 3.x release never ships thurbox-v<V>-checksums.txt: every 2.x updater
    # fetches that name first (docs/thurbox/releasing.md, "v2 and v3").
    It 'verifies a 3.x release by its sha256sums file, never asking for checksums.txt' {
        . $script:ScriptPath
        Invoke-Install
        Should -Invoke Invoke-WebRequest -ParameterFilter { $Uri -like '*/thurbox-9.9.9-sha256sums.txt' } -Times 1
        Should -Invoke Invoke-WebRequest -ParameterFilter { $Uri -like '*checksums.txt' } -Times 0
    }

    It 'verifies a pinned 2.x release by its checksums.txt' {
        Rename-Item -LiteralPath (Join-Path $script:Assets 'thurbox-9.9.9-sha256sums.txt') -NewName 'thurbox-v9.9.9-checksums.txt'
        . $script:ScriptPath
        Invoke-Install
        foreach ($name in $script:Shipped) {
            Test-Path -LiteralPath (Join-Path $script:Dest "$name.exe") | Should -BeTrue
        }
    }

    It 'installs every shipped program without it' {
        . $script:ScriptPath
        Invoke-Install
        foreach ($name in $script:Shipped) {
            Test-Path -LiteralPath (Join-Path $script:Dest "$name.exe") | Should -BeTrue
        }
        ($script:Printed -join "`n") | Should -Match 'thurbox-cli doctor'
    }
}
