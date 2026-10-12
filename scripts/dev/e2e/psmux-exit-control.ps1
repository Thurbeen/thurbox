# Native Windows regression: a crash-loop hold must reach the interface.
param(
 [Parameter(Mandatory=$true)][string]$BinaryDir
)
$ProgressPreference='SilentlyContinue'
$ErrorActionPreference='Stop'
$cli=(Resolve-Path "$BinaryDir/thurbox-cli.exe").Path
$tui=(Resolve-Path "$BinaryDir/thurbox.exe").Path
$token=[Guid]::NewGuid().ToString('N')
$socket="psmux-exit-test-$token"
$run=Join-Path $env:LOCALAPPDATA "thurbox-test/$token"
$env:THURBOX_SOCKET=$socket
Remove-Item Env:THURBOX_SOCKET_FOR -ErrorAction SilentlyContinue
Remove-Item Env:THURBOX_SESSION -ErrorAction SilentlyContinue
$env:THURBOX_CONFIG_DIR="$run/config"
$env:THURBOX_DATA_DIR="$run/data"
$env:PATH="$(Split-Path $cli);"+$env:PATH
New-Item -ItemType Directory -Force "$run/config", "$run/data", "$run/repo" | Out-Null
@'
[features]
automations = false
version_check = false
auto_update = false
'@ | Set-Content "$run/config/settings.toml"
function Invoke-Tbx([string[]]$a) {
 $ErrorActionPreference='Continue'
 $out=& $cli --json @a 2> "$run/last-cli-stderr" | Out-String
 if ($LASTEXITCODE -ne 0) { throw "CLI failed: $out" }
 return ($out | ConvertFrom-Json)
}
function Wait-WindowGone {
    $ErrorActionPreference='Continue'
    $deadline=[DateTime]::UtcNow.AddSeconds(40)
    while ([DateTime]::UtcNow -lt $deadline) {
        $windows=@(& psmux -L $socket list-windows -a -F '#{window_name}' 2> "$run/window-stderr")
        if ($LASTEXITCODE -eq 0 -and @($windows | Where-Object { $_.Trim() -eq 'tb-guard-probe' }).Count -eq 0) { return }
        Start-Sleep -Milliseconds 200
    }
    throw 'Fixture agent window did not close'
}
try {
    foreach ($extension in @('hooks','ui-skill')) {
        $ErrorActionPreference='Continue'
        $out=& $cli --json extension deactivate $extension 2> "$run/optout-stderr" | Out-String
        $code=$LASTEXITCODE
        $errorMessage=($out | ConvertFrom-Json).error
        if ($code -ne 1 -or $errorMessage -ne "extension '$extension' is not installed (no manifest found)") {
            throw 'Expected an opt-out in a fresh profile without an installed extension'
        }
    }
    $ErrorActionPreference='Continue'
    & psmux -L $socket new-session -d -s thurbox-dev -n exit-fixture 2> "$run/fixture-stderr" | Out-Null
    $ErrorActionPreference='Stop'
 @'
Start-Sleep -Seconds 4
Set-Content "$PSScriptRoot/agent-ran" 'yes'
exit 9
'@ | Set-Content "$run/native.ps1"
 $ps=(Get-Command powershell.exe).Source
 $agent=Invoke-Tbx @('session','create','--name','guard-probe','--repo-path',"$run/repo",'--command',$ps,'--arg','-NoProfile','--arg','-ExecutionPolicy','--arg','Bypass','--arg','-File','--arg',"$run/native.ps1")
 Wait-WindowGone
 for ($i=1; $i -le 3; $i++) {
  $result=Invoke-Tbx @('session','restart',$agent.id,'--if-missing')
  if ($null -eq $result.held_back -or $result.held_back) { throw "Expected an applied repair at $i" }
  Wait-WindowGone
 }
 if (!(Test-Path "$run/agent-ran")) { throw 'Fixture agent never executed' }
 $events=& $cli --json watch --session $agent.id --since 0 --for-secs 0
 $relaunches=@($events | ForEach-Object { $_ | ConvertFrom-Json } | Where-Object { $_.reason -eq 'relaunched' })
 if ($relaunches.Count -ne 3) { throw 'Expected three recorded repair relaunches before opening the interface' }
 Invoke-Tbx @('config','accept-interface') | Out-Null
 & $cli plugin dir | Out-Null
 New-Item -ItemType Directory -Force "$run/config/ui/plugins" | Out-Null
 @'
return {
    name = "exit_probe",
    slot = "exit_probe",
    strip = true,
    size = { len = 1 },
    events = { "command.done", "command.failed", "session.post_restart" },
    on_event = function(name, payload)
        if name == "command.done" and payload.kind == "restart" then
            state.result = "APPLIED"
        elseif name == "command.failed" and payload.kind == "restart" then
            state.result = "FAILED"
        elseif name == "session.post_restart" then
            state.post_restart = true
        end
    end,
    render = function()
        return {
            type = "text",
            text = "exit-probe=" .. (state.result or "WAIT")
                .. ";post=" .. tostring(state.post_restart == true),
        }
    end,
}
'@ | Set-Content "$run/config/ui/plugins/99_exit_probe.lua"

 $session=(& psmux -L $socket list-sessions -F '#{session_name}' | Select-Object -First 1)
 $boot="Set-Item Env:THURBOX_SOCKET '$socket'; Set-Item Env:THURBOX_CONFIG_DIR '$run/config'; Set-Item Env:THURBOX_DATA_DIR '$run/data'; Set-Location '$run/repo'; & '$tui'"
 & psmux -L $socket new-window -d -t "${session}:" -n exit-observer $boot
 $frames=@()
 for ($i=0; $i -lt 60; $i++) {
  $frames+=((& psmux -L $socket capture-pane -p -t "${session}:exit-observer" 2>&1) -join "`n")
  Start-Sleep -Milliseconds 250
 }
 $joined=$frames -join "`n"
 if ($joined -match 'restarted guard-probe') { throw 'Held-back restart falsely reported as restarted' }
 if ($joined -notmatch 'keeps exiting') { throw "Hold-back reason missing from the interface:`n$joined" }
 if ($joined -notmatch 'exit-probe=FAILED;post=false' -or $joined -match 'exit-probe=APPLIED|post=true') { throw 'Held-back restart must report a failure without session.post_restart' }
 Write-Output 'PASS held-back restart is displayed without a false relaunch toast'
} finally {
 $ErrorActionPreference='Continue'
 & psmux -L $socket kill-server 2>&1 | Out-Null
 for ($attempt=0; $attempt -lt 30 -and (Test-Path $run); $attempt++) {
  Remove-Item -Recurse -Force $run -ErrorAction SilentlyContinue
  if (Test-Path $run) { Start-Sleep -Milliseconds 100 }
 }
 if (Test-Path $run) { throw 'Disposable profile cleanup failed' }
}
