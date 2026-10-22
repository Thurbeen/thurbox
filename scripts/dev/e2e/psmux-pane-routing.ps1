# Native Windows regression (#1557): every pane command reaches its own pane.
#
# thurbox names a psmux pane as `thurbox:.%N` (`qualify_target`). psmux 3.3.7
# reads that spelling as no pane at all and runs the command on the session's
# ACTIVE pane: keystrokes typed into another session, its screen read back as
# this one's, `kill-pane` killing the wrong agent. So either thurbox refuses the
# psmux it is given, saying it is too old, or each session's input lands in that
# session and nowhere else, and stopping one leaves the others running.
#
# The agents are a PowerShell fixture that logs every line it reads, which is
# what makes a landing attributable. A random socket, disposable config, data
# and psmux registry directories, all removed in `finally`.
param(
    [Parameter(Mandatory=$true)][string]$BinaryDir,
    # A directory holding the psmux.exe to test, put first on PATH.
    [string]$PsmuxDir,
    # A directory holding an older psmux.exe to start thurbox's server with
    # first: the server an upgrade of the psmux on PATH leaves running.
    [string]$ServerPsmuxDir
)
$ProgressPreference='SilentlyContinue'
$ErrorActionPreference='Stop'
$cli=(Resolve-Path "$BinaryDir/thurbox-cli.exe").Path
$token=[Guid]::NewGuid().ToString('N')
$socket="psmux-routing-test-$token"
$run=Join-Path $env:LOCALAPPDATA "thurbox-test/$token"
if ($PsmuxDir) { $env:PATH="$((Resolve-Path $PsmuxDir).Path);"+$env:PATH }
$env:THURBOX_SOCKET=$socket
Remove-Item Env:THURBOX_SOCKET_FOR -ErrorAction SilentlyContinue
Remove-Item Env:THURBOX_SESSION -ErrorAction SilentlyContinue
$env:THURBOX_CONFIG_DIR="$run/config"
$env:THURBOX_DATA_DIR="$run/data"
# psmux keeps its registry under the profile, and the user's own servers
# would sweep this one's files there. 3.3.7 has no PSMUX_DATA_DIR, so the
# profile itself moves: one registry for every psmux version in a run.
$env:USERPROFILE="$run/psmux"
$env:HOME="$run/psmux"
Remove-Item Env:PSMUX_DATA_DIR -ErrorAction SilentlyContinue
$env:PATH="$(Split-Path $cli);"+$env:PATH
New-Item -ItemType Directory -Force "$run/config", "$run/data", "$run/repo", "$run/psmux" | Out-Null
@'
[features]
automations = false
version_check = false
auto_update = false
'@ | Set-Content "$run/config/settings.toml"
@'
param([string]$Name)
while ($true) {
    [Console]::Write('> ')
    $line = [Console]::ReadLine()
    if ($null -eq $line) { break }
    Add-Content -Path "$PSScriptRoot/$Name.log" -Value $line
}
'@ | Set-Content "$run/agent.ps1"
function Invoke-Tbx([string[]]$a) {
    $ErrorActionPreference='Continue'
    $out=& $cli --json @a 2> "$run/last-cli-stderr" | Out-String
    return [pscustomobject]@{ code = $LASTEXITCODE; out = $out; err = (Get-Content -Raw "$run/last-cli-stderr") }
}
function Get-Windows {
    $ErrorActionPreference='Continue'
    @(& psmux -L $socket list-windows -a -F '#{window_name}' 2> $null | ForEach-Object { $_.Trim() })
}
function Wait-Line([string]$name, [string]$text) {
    $deadline=[DateTime]::UtcNow.AddSeconds(20)
    while ([DateTime]::UtcNow -lt $deadline) {
        if ((Test-Path "$run/$name.log") -and (Get-Content "$run/$name.log") -contains $text) { return $true }
        Start-Sleep -Milliseconds 200
    }
    return $false
}
try {
    $mux = (& psmux -V) -join ' '
    Write-Output "psmux: $mux"
    if ($ServerPsmuxDir) {
        # thurbox's session: a dev build's is `thurbox-dev` (TMUX_SESSION).
        $ErrorActionPreference='Continue'
        $version = (& $cli --json version 2> $null | ConvertFrom-Json).version
        $session = if ($version -eq '0.0.0-dev') { 'thurbox-dev' } else { 'thurbox' }
        $old = (Resolve-Path "$ServerPsmuxDir/psmux.exe").Path
        & $old -L $socket new-session -d -s $session 2>&1 | Out-Null
        $ErrorActionPreference='Stop'
        Start-Sleep -Seconds 6
        $mux = "server " + ((& $old -V) -join ' ') + ", client $mux"
        Write-Output "running: $mux"
    }
    foreach ($extension in @('hooks','ui-skill')) {
        Invoke-Tbx @('extension','deactivate',$extension) | Out-Null
    }
    $ps=(Get-Command powershell.exe).Source
    $names=@('route-a','route-b','route-c')
    $sessions=@{}
    foreach ($name in $names) {
        $r=Invoke-Tbx @('session','create','--name',$name,'--repo-path',"$run/repo",'--command',$ps,'--arg','-NoProfile','--arg','-ExecutionPolicy','--arg','Bypass','--arg','-File','--arg',"$run/agent.ps1",'--arg',$name)
        if ($r.code -ne 0) {
            # A refusal passes only for a psmux this run has and the floor
            # really excludes: refusing 3.3.8 would be a bug of its own.
            if ("$($r.out)$($r.err)" -match 'psmux (\d+\.\d+\.\d+) is too old') {
                $refused = $matches[1]
                if ([version]$refused -ge [version]'3.3.8' -or $mux -notmatch "(psmux|tmux) $([regex]::Escape($refused))\b") {
                    throw "thurbox refused psmux $refused, which this run ($mux) does not make too old"
                }
                Write-Output "PASS psmux $refused refused before any pane was addressed"
                exit 0
            }
            throw "session create $name failed ($($r.code)): $($r.out) $($r.err)"
        }
        $sessions[$name]=($r.out | ConvertFrom-Json).id
    }
    Start-Sleep -Seconds 3
    $failures=@()
    foreach ($name in $names) {
        $mark="mark-$name-$token"
        $r=Invoke-Tbx @('session','send',$sessions[$name],$mark)
        if (-not (Wait-Line $name $mark)) { $failures+="$name never read its own input (send exited $($r.code))" }
        foreach ($other in $names | Where-Object { $_ -ne $name }) {
            if ((Test-Path "$run/$other.log") -and (Get-Content "$run/$other.log") -contains $mark) {
                $failures+="$name's input was read by $other"
            }
        }
    }
    $before=Get-Windows
    $r=Invoke-Tbx @('session','stop',$sessions['route-b'])
    Start-Sleep -Seconds 3
    $after=Get-Windows
    if ($after -contains 'tb-route-b') { $failures+="stopping route-b left its window ($($r.out) $($r.err))" }
    foreach ($kept in @('tb-route-a','tb-route-c')) {
        if ($before -contains $kept -and $after -notcontains $kept) { $failures+="stopping route-b killed $kept" }
    }
    # With no server left, a key has nowhere to go: refused as absent, never
    # held up by the version question that finds nothing to ask.
    $ErrorActionPreference='Continue'
    & psmux -L $socket kill-server 2>&1 | Out-Null
    $ErrorActionPreference='Stop'
    Start-Sleep -Seconds 2
    $r=Invoke-Tbx @('session','key',$sessions['route-a'],'Enter')
    if ($r.code -eq 0) { $failures+="a key to a server that is gone reported success" }
    if ("$($r.out)$($r.err)" -match 'could not ask the server its version|is too old') {
        $failures+="a server that is gone was refused as unanswering: $($r.out) $($r.err)"
    }
    if ($failures.Count -gt 0) { throw "pane commands reached the wrong pane on $mux`n" + ($failures -join "`n") }
    Write-Output "PASS each session's input reached that session only, a stop ended only its own window, and a key to a server that is gone failed as absent"
    # The kill-server in `finally` finds no server and fails; that status is
    # not this run's verdict.
    exit 0
} finally {
    $ErrorActionPreference='Continue'
    & psmux -L $socket kill-server 2>&1 | Out-Null
    for ($attempt=0; $attempt -lt 30 -and (Test-Path $run); $attempt++) {
        Remove-Item -Recurse -Force $run -ErrorAction SilentlyContinue
        if (Test-Path $run) { Start-Sleep -Milliseconds 100 }
    }
}
