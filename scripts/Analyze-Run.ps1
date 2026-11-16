param([Parameter(Mandatory=$true)][string]$RunDirectory)
$ErrorActionPreference = 'Stop'
$runPath = (Resolve-Path -LiteralPath $RunDirectory).Path
$lines = Get-Content -LiteralPath (Join-Path $runPath 'events.log')
if($lines | Select-String -Pattern ' RUN format=2 ' -Quiet){
    $exePath=Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release\padbridge-phase0.exe'
    if(!(Test-Path -LiteralPath $exePath)){throw 'Build first: cargo build --release --locked'}
    $analysis=& $exePath --verify-log (Join-Path $runPath 'events.log')
    $code=$LASTEXITCODE
    $analysis | Set-Content -LiteralPath (Join-Path $runPath 'analysis.json') -Encoding utf8
    $analysis
    if($code -ne 0){throw 'Touch log verification failed; inspect analysis.json and events.log'}
    return
}
$injected = [System.Collections.Generic.List[object]]::new()
$received = [System.Collections.Generic.List[object]]::new()
$rawCount = 0
$frameCount = 0
$peaks = @{}
$firstRaw = $null
$lastRaw = $null
$active = $false
$lastPoint = ''
$injectionViolations = [System.Collections.Generic.List[string]]::new()
$receiverViolations = [System.Collections.Generic.List[string]]::new()
$receiverActive = @{}
$apiDurations = [System.Collections.Generic.List[long]]::new()
foreach ($line in $lines) {
    if ($line -match '^t_us=(\d+) RAW ') {
        $rawCount++
        if ($null -eq $firstRaw) { $firstRaw = [long]$Matches[1] }
        $lastRaw = [long]$Matches[1]
    }
    if ($line -match ' FRAME active=(\d+) records=(\d+)') {
        $frameCount++
        $n = [string]$Matches[1]
        if (!$peaks.ContainsKey($n)) { $peaks[$n] = 0 }
        $peaks[$n]++
    }
    if ($line -match ' INJECT_OK flags=0x([0-9A-F]+) x=(-?\d+) y=(-?\d+) api_us=(\d+)') {
        $flags = [Convert]::ToUInt32($Matches[1],16)
        $point = "$($Matches[2]),$($Matches[3])"
        $apiDurations.Add([long]$Matches[4])
        $kind = if ($flags -band 0x10000) { 'DOWN' } elseif ($flags -band 0x40000) { 'UP' } else { 'UPDATE' }
        if ($kind -eq 'DOWN') {
            if ($active) { $injectionViolations.Add('DOWN while active') }
            $active = $true
        } elseif (!$active) { $injectionViolations.Add("$kind without DOWN") }
        if ($kind -eq 'UP') {
            if ($point -ne $lastPoint) { $injectionViolations.Add('UP differs from previous UPDATE coordinate') }
            $active = $false
        }
        $lastPoint = $point
        $injected.Add([pscustomobject]@{ Kind=$kind; Point=$point })
    }
    if ($line -match ' RECEIVE_PT_TOUCH (DOWN|UPDATE|UP) id=(\d+) frame=\d+ x=(-?\d+) y=(-?\d+)') {
        $kind=$Matches[1]; $id=$Matches[2]; $point="$($Matches[3]),$($Matches[4])"
        if ($kind -eq 'DOWN') {
            if ($receiverActive.ContainsKey($id)) { $receiverViolations.Add("duplicate DOWN id=$id") }
            $receiverActive[$id]=$true
        } elseif (!$receiverActive.ContainsKey($id)) { $receiverViolations.Add("$kind without DOWN id=$id") }
        if ($kind -eq 'UP') { $receiverActive.Remove($id) }
        $received.Add([pscustomobject]@{ Kind=$kind; Point=$point })
    }
}
# WM_POINTERUPDATE may be coalesced by Windows. Require the received events to
# match an ordered subsequence of actual successful injections, rather than
# incorrectly demanding an identical number of UPDATE messages.
$cursor=0; $unmatched=0
foreach ($event in $received) {
    $found=$false
    while ($cursor -lt $injected.Count) {
        $candidate=$injected[$cursor]; $cursor++
        if ($candidate.Kind -eq $event.Kind -and $candidate.Point -eq $event.Point) { $found=$true; break }
    }
    if (!$found) { $unmatched++ }
}
$sorted=@($apiDurations | Sort-Object)
$p95=if($sorted.Count){$sorted[[int][Math]::Floor(($sorted.Count-1)*0.95)]}else{$null}
$result=[ordered]@{
    run=$runPath
    raw_reports=$rawCount
    raw_first_to_last_ms=if($null -ne $firstRaw){[Math]::Round(($lastRaw-$firstRaw)/1000.0,3)}else{0}
    complete_frames=$frameCount
    frames_by_active_contact_count=$peaks
    injected_frames=$injected.Count
    received_PT_TOUCH_events=$received.Count
    received_down=@($received | Where-Object Kind -eq 'DOWN').Count
    received_update=@($received | Where-Object Kind -eq 'UPDATE').Count
    received_up=@($received | Where-Object Kind -eq 'UP').Count
    injection_sequence_violations=@($injectionViolations)
    receiver_sequence_violations=@($receiverViolations)
    injected_contact_left_active=$active
    received_contacts_left_active=$receiverActive.Count
    received_events_without_ordered_injection_match=$unmatched
    injection_api_p95_us=$p95
    interpretation='Ordered event/coordinate evidence only. No claim about physical latency, report loss, or other hardware.'
}
$json=$result | ConvertTo-Json -Depth 6
$json | Set-Content -LiteralPath (Join-Path $runPath 'analysis.json') -Encoding utf8
$json
