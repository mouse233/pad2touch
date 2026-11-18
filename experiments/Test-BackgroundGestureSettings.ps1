param(
    [ValidateSet('probe','apply','session','restore','watchdog','self-test')][string]$Mode='probe',
    [string]$ExpectedBuild,
    [ValidateRange(15,120)][int]$Seconds=75,
    [string]$RunDirectory,
    [string]$Journal,
    [string]$CaptureExecutable,
    [switch]$WaitForStart,
    [switch]$SuppressTouchscreen,
    [switch]$KeepTouchpadGestures,
    [int]$OwnerPid,
    [long]$OwnerStartedFileTime
)
$ErrorActionPreference='Stop'
$repoPath=Split-Path -Parent $PSScriptRoot
$helper=Join-Path $repoPath 'artifacts\gesture-cache-probe.exe'
$captureExe=if($CaptureExecutable){[IO.Path]::GetFullPath($CaptureExecutable)}else{Join-Path $repoPath 'target\release\Pad2Touch.exe'}
$subkey='Software\Microsoft\Windows\CurrentVersion\PrecisionTouchPad'
$names=@('ThreeFingerTapEnabled','FourFingerTapEnabled','ThreeFingerSlideEnabled','FourFingerSlideEnabled')
$leaseDirectory=Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'PadBridge\ExperimentalGestureCache'
$leasePath=Join-Path $leaseDirectory 'active.json'

function Get-Preference($key,[string]$name) {
    if($key.GetValueNames() -notcontains $name){return [pscustomobject]@{name=$name;exists=$false;kind='Absent';bytes=''}}
    if($key.GetValueKind($name) -ne [Microsoft.Win32.RegistryValueKind]::DWord){throw "Unsupported registry type: $name"}
    $bytes=[BitConverter]::GetBytes([int]$key.GetValue($name))
    return [pscustomobject]@{name=$name;exists=$true;kind='DWord';bytes=[Convert]::ToBase64String($bytes)}
}
function Test-SamePreference($a,$b) {
    return $a.exists -eq $b.exists -and $a.kind -ceq $b.kind -and $a.bytes -ceq $b.bytes
}
function Get-TouchscreenGestures {
    $runtime=& $helper --read-touchscreen
    if($LASTEXITCODE -ne 0 -or $runtime -notin @('0','1')){throw 'Cannot read touchscreen gesture state'}
    return [uint32]$runtime
}
function Set-TouchscreenGestures([uint32]$value) {
    $runtime=& $helper --set-touchscreen $value
    if($LASTEXITCODE -ne 0 -or $runtime -ne [string]$value){throw 'Touchscreen gesture live readback mismatch'}
}
function Get-TouchscreenSnapshot {
    $key=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Control Panel\Desktop',$false)
    if($null -eq $key){throw 'Desktop preferences key is unavailable'}
    try {$preference=Get-Preference $key 'TouchGestureSetting'} finally {$key.Dispose()}
    return [pscustomobject]@{runtime=(Get-TouchscreenGestures);preference=$preference}
}
function Restore-TouchscreenGestures($snapshot) {
    $current=Get-TouchscreenSnapshot
    if(!(Test-SamePreference $snapshot.preference $current.preference)){
        throw 'Touchscreen preference changed externally; retained the new setting'
    }
    if($current.runtime -ne $snapshot.runtime){Set-TouchscreenGestures ([uint32]$snapshot.runtime)}
    $after=Get-TouchscreenSnapshot
    if($after.runtime -ne $snapshot.runtime -or !(Test-SamePreference $snapshot.preference $after.preference)){
        throw 'Touchscreen gesture restoration mismatch'
    }
}
function Get-RestoreAction($old,$current) {
    if(Test-SamePreference $old $current){return 'already-original'}
    if($current.exists -and $current.kind -ceq 'DWord' -and $current.bytes -ceq 'AAAAAA=='){return 'restore-owned-zero'}
    return 'conflict'
}
function Get-DeadlineUtc($value) {
    # PowerShell 7 can materialize JSON ISO timestamps as DateTime. Passing
    # that object through Parse(string) loses its zone and subsecond precision.
    if($value -is [DateTime]){return $value.ToUniversalTime()}
    return [DateTime]::Parse([string]$value,[Globalization.CultureInfo]::InvariantCulture,[Globalization.DateTimeStyles]::RoundtripKind).ToUniversalTime()
}
function Test-DeadlineExpired($journal) {
    return $null -ne $journal.deadlineUtc -and [DateTime]::UtcNow -ge (Get-DeadlineUtc $journal.deadlineUtc)
}
function Test-SessionExpired([string]$mode,[double]$elapsed,[int]$limit) {
    # GUI sessions are owned by processes, not by a fixed capture duration.
    return $mode -ne 'session' -and $elapsed -ge $limit
}
function Update-Heartbeat([string]$directory) {
    [IO.File]::SetLastWriteTimeUtc((Join-Path $directory 'heartbeat.json'),[DateTime]::UtcNow)
}
function Save-NewJson([string]$path,$value) {
    $json=$value | ConvertTo-Json -Depth 8
    $stream=[IO.File]::Open($path,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::Read)
    try {$data=[Text.UTF8Encoding]::new($false).GetBytes($json);$stream.Write($data,0,$data.Length);$stream.Flush($true)} finally {$stream.Dispose()}
}
function Read-Cache([switch]$Reload) {
    $operation=if($Reload){'--reload-four'}else{'--read-cache'}
    $output=@(& $helper $operation)
    if($LASTEXITCODE -ne 0){throw 'Cache read probe failed'}
    $values=@{}
    foreach($line in $output){
        if($line -match '^candidate_setting=(\w+).* dword_hr=0x00000000 dword_value=(\d+)$'){$values[$Matches[1]]=[uint32]$Matches[2]}
    }
    if($values.Count -ne 4){throw 'Four successful DWORD cache reads are required'}
    return $values
}
function Test-DisabledPreferences($preferences) {
    return @($preferences).Count -eq 4 -and @($preferences | Where-Object {
        !$_.exists -or $_.kind -cne 'DWord' -or $_.bytes -cne 'AAAAAA=='
    }).Count -eq 0
}
function Assert-SessionGuard {
    if($SuppressTouchscreen){
        $currentTouchscreen=Get-TouchscreenSnapshot
        if($currentTouchscreen.runtime -ne 0){throw 'Touchscreen system gestures became enabled during capture'}
        if(!(Test-SamePreference $touchscreen.preference $currentTouchscreen.preference)){
            throw 'Touchscreen preference changed during capture; stopping without overwriting the change'
        }
    }
    if($KeepTouchpadGestures){return}
    # Never overwrite a concurrent settings edit. Only refresh the cache while
    # the exact four registry values are still the zeros owned by this session.
    $key=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($subkey,$false)
    if($null -eq $key){throw 'Touchpad preferences key disappeared during capture'}
    try {$current=@(foreach($name in $names){Get-Preference $key $name})} finally {$key.Dispose()}
    if(!(Test-DisabledPreferences $current)){throw 'Gesture preferences changed during capture; stopping without overwriting the change'}
    $cache=Read-Cache
    if(@($names | Where-Object {$cache[$_] -ne 0}).Count){
        Write-Host 'Background guard cache drift detected; refreshing the four owned settings.'
        $cache=Read-Cache -Reload
        foreach($name in $names){if($cache[$name] -ne 0){throw ('Background guard cache remains enabled: '+$name)}}
    }
}
function Read-Journal([string]$path) {
    $resolved=(Resolve-Path -LiteralPath $path).Path
    $artifactRoot=[IO.Path]::GetFullPath((Join-Path $repoPath 'artifacts'))+[IO.Path]::DirectorySeparatorChar
    if(!$resolved.StartsWith($artifactRoot,[StringComparison]::OrdinalIgnoreCase)){throw 'Recovery journal must be under this checkout artifacts directory'}
    $j=Get-Content -LiteralPath $resolved -Raw | ConvertFrom-Json
    if($j.format -ne 1 -or $j.subkey -cne $subkey -or @($j.preferences).Count -notin @(0,4)){throw 'Unsupported recovery journal'}
    if(@($j.preferences | ForEach-Object {$_.name} | Sort-Object -Unique).Count -ne @($j.preferences).Count){throw 'Duplicate recovery names'}
    if(@($j.preferences).Count -eq 0 -and $null -eq $j.touchscreen){throw 'Empty recovery journal'}
    foreach($p in $j.preferences){
        if($names -cnotcontains $p.name){throw 'Unexpected recovery name'}
        if($p.exists){
            if($p.kind -cne 'DWord' -or [Convert]::FromBase64String($p.bytes).Length -ne 4){throw 'Unsupported recovery bytes'}
        } elseif($p.kind -cne 'Absent' -or $p.bytes -cne ''){throw 'Invalid absent-value snapshot'}
    }
    if($null -ne $j.touchscreen){
        if($j.touchscreen.runtime -notin @(0,1) -or $j.touchscreen.preference.name -cne 'TouchGestureSetting'){
            throw 'Unsupported touchscreen recovery snapshot'
        }
        $p=$j.touchscreen.preference
        if($p.exists){
            if($p.kind -cne 'DWord' -or [Convert]::FromBase64String($p.bytes).Length -ne 4){throw 'Unsupported touchscreen preference bytes'}
        } elseif($p.kind -cne 'Absent' -or $p.bytes -cne ''){throw 'Invalid absent touchscreen preference'}
    }
    return $j
}
function Restore-Preferences([string]$path) {
    $j=Read-Journal $path
    $dir=Split-Path -Parent $path
    $done=Join-Path $dir 'restored.json'
    $lock=[Threading.Mutex]::new($false,('Local\PadBridgeGestureRestore-'+$j.id))
    $held=$false
    try {
        while(!$held){
            try {$held=$lock.WaitOne(500)} catch [Threading.AbandonedMutexException] {$held=$true}
            if(!$held -and (Test-Path -LiteralPath $done)){return}
        }
        if(Test-Path -LiteralPath $done){return}
        if(!(Test-Path -LiteralPath (Join-Path $dir 'mutation-started.json'))){
            Save-NewJson $done ([ordered]@{restored=$true;no_mutation=$true})
            return
        }
        $conflicts=@()
        if(@($j.preferences).Count){
            try {
                $key=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($subkey,$true)
                if($null -eq $key){throw 'Original touchpad preferences key is unavailable'}
                try {
                    foreach($p in $j.preferences){
                        try {
                            $current=Get-Preference $key $p.name
                            switch(Get-RestoreAction $p $current){
                                'already-original' {}
                                'restore-owned-zero' {
                                    if($p.exists){$key.SetValue($p.name,[BitConverter]::ToInt32([Convert]::FromBase64String($p.bytes),0),[Microsoft.Win32.RegistryValueKind]::DWord)}
                                    else {$key.DeleteValue($p.name,$false)}
                                }
                                default {$conflicts+=$p.name}
                            }
                        } catch {$conflicts+=$p.name}
                    }
                    $key.Flush()
                    foreach($p in $j.preferences){
                        if($conflicts -notcontains $p.name -and !(Test-SamePreference $p (Get-Preference $key $p.name))){$conflicts+=$p.name}
                    }
                } finally {$key.Dispose()}
            } catch {$conflicts+=$_.Exception.Message}
            try {
                $cache=Read-Cache -Reload
                foreach($p in $j.preferences){if($cache[$p.name] -ne [uint32]$p.cacheValue){throw ('Cache restoration mismatch: '+$p.name)}}
            } catch {$conflicts+=$_.Exception.Message}
        }
        # Restore the other independent path even if touchpad restoration failed.
        if($null -ne $j.touchscreen -and (Test-Path -LiteralPath (Join-Path $dir 'touchscreen-mutation-started.json'))){
            try {Restore-TouchscreenGestures $j.touchscreen} catch {$conflicts+=$_.Exception.Message}
        }
        if($conflicts.Count){throw ('Restored other owned settings; retained external/unsupported values requiring review: '+($conflicts -join ','))}
        Save-NewJson $done ([ordered]@{restored=$true;registry_exact=$true;cache_exact=$true;touchscreen_exact=$true;utc=[DateTime]::UtcNow.ToString('o')})
        Write-Host 'Exact registry presence/type/bytes and cache values restored.'
        try {[Console]::Beep(650,250)} catch {}
    } finally {if($held){$lock.ReleaseMutex()};$lock.Dispose()}
}

if($Mode -eq 'self-test'){
    $absent=[pscustomobject]@{exists=$false;kind='Absent';bytes=''}
    $zero=[pscustomobject]@{exists=$true;kind='DWord';bytes='AAAAAA=='}
    $one=[pscustomobject]@{exists=$true;kind='DWord';bytes='AQAAAA=='}
    $two=[pscustomobject]@{exists=$true;kind='DWord';bytes='AgAAAA=='}
    foreach($case in @(@($absent,$zero,'restore-owned-zero'),@($one,$zero,'restore-owned-zero'),@($one,$one,'already-original'),@($absent,$absent,'already-original'),@($one,$two,'conflict'),@($absent,$two,'conflict'))){
        if((Get-RestoreAction $case[0] $case[1]) -cne $case[2]){throw 'Restoration decision self-test failed'}
    }
    if(!(Test-DisabledPreferences @($zero,$zero,$zero,$zero))){throw 'Owned disabled preferences rejected'}
    foreach($preferences in @(@($zero,$zero,$zero),@($zero,$zero,$zero,$one),@($zero,$zero,$zero,$absent))){
        if(Test-DisabledPreferences $preferences){throw 'Incomplete or externally changed preferences accepted'}
    }
    $utcText='2099-12-31T11:46:57.2856908Z'
    $jsonDate=('{"deadlineUtc":"'+$utcText+'"}') | ConvertFrom-Json
    $expectedTicks=[DateTime]::Parse($utcText,[Globalization.CultureInfo]::InvariantCulture,[Globalization.DateTimeStyles]::RoundtripKind).Ticks
    foreach($value in @($utcText,$jsonDate.deadlineUtc)){
        if((Get-DeadlineUtc $value).Ticks -ne $expectedTicks){throw 'Deadline timezone/precision regression'}
    }
    foreach($elapsed in @(0,120,121,86400,31536000)){
        if(Test-SessionExpired 'session' $elapsed 120){throw 'GUI capture acquired a time limit'}
    }
    if(!(Test-SessionExpired 'apply' 121 120)){throw 'Bounded experiment lost its time limit'}
    if(Test-DeadlineExpired ([pscustomobject]@{deadlineUtc=$null})){throw 'Unlimited session deadline expired'}
    if(!(Test-DeadlineExpired ([pscustomobject]@{deadlineUtc='2000-01-01T00:00:00Z'}))){throw 'Bounded deadline did not expire'}
    # Exercise restore against fake live state: no registry or SPI writes.
    function Get-TouchscreenSnapshot {return $script:fakeTouchscreen}
    function Set-TouchscreenGestures([uint32]$value) {$script:fakeTouchscreen.runtime=$value;$script:fakeWrites++}
    foreach($preference in @($absent,$zero,$one)){
        $script:fakeWrites=0
        $script:fakeTouchscreen=[pscustomobject]@{runtime=0;preference=$preference}
        Restore-TouchscreenGestures ([pscustomobject]@{runtime=1;preference=$preference})
        if($script:fakeTouchscreen.runtime -ne 1 -or $script:fakeWrites -ne 1){throw 'Touchscreen restore did not recover runtime state'}
        Restore-TouchscreenGestures ([pscustomobject]@{runtime=1;preference=$preference})
        if($script:fakeWrites -ne 1){throw 'Touchscreen restore was not idempotent'}
    }
    $script:fakeTouchscreen=[pscustomobject]@{runtime=1;preference=$one}
    $script:fakeWrites=0
    $conflictDetected=$false
    try {Restore-TouchscreenGestures ([pscustomobject]@{runtime=0;preference=$absent})} catch {$conflictDetected=$true}
    if(!$conflictDetected -or $script:fakeWrites -ne 0){throw 'External touchscreen preference was overwritten'}
    Write-Output 'restoration_decision_tests=6 passed; disabled_ownership_tests=4 passed; deadline_roundtrip_tests=2 passed; unlimited_session_tests=8 passed; touchscreen_restore_tests=7 passed; registry_access=false'
    return
}
if(!(Test-Path -LiteralPath $helper)){throw 'Compile experiments/gesture-cache-probe.rs into artifacts/gesture-cache-probe.exe first'}
if($Mode -eq 'restore'){Restore-Preferences $Journal;return}
if($Mode -eq 'watchdog'){
    $j=Read-Journal $Journal
    $dir=Split-Path -Parent $Journal
    if(@($j.preferences).Count){[void](Read-Cache)}
    Save-NewJson (Join-Path $dir 'watchdog-ready.json') ([ordered]@{pid=$PID;ready=$true})
    while(!(Test-Path -LiteralPath (Join-Path $dir 'restored.json'))){
        $parent=Get-Process -Id $j.parentPid -ErrorAction SilentlyContinue
        if($null -eq $parent -or $parent.StartTime.ToUniversalTime().Ticks -ne [long]$j.parentStartedTicks -or (Test-DeadlineExpired $j)){break}
        if($null -eq $j.deadlineUtc){
            $guiOwner=Get-Process -Id $j.ownerPid -ErrorAction SilentlyContinue
            if($null -eq $guiOwner -or $guiOwner.StartTime.ToUniversalTime().ToFileTimeUtc() -ne [long]$j.ownerStartedFileTime){break}
            # A stalled settings backend must not hold the override forever.
            $heartbeat=Get-Item -LiteralPath (Join-Path $dir 'heartbeat.json') -ErrorAction SilentlyContinue
            if($null -eq $heartbeat -or ([DateTime]::UtcNow-$heartbeat.LastWriteTimeUtc).TotalSeconds -gt 30){break}
        }
        Start-Sleep -Milliseconds 250
    }
    if(!(Test-Path -LiteralPath (Join-Path $dir 'restored.json'))){Restore-Preferences $Journal}
    return
}
if($WaitForStart){
    if($Mode -ne 'apply'){throw '-WaitForStart is only valid with apply'}
    if(!('Pad2TouchExperimentStart' -as [type])){
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class Pad2TouchExperimentStart {
    [DllImport("user32.dll", CharSet=CharSet.Unicode)]
    public static extern int MessageBox(IntPtr owner, string text, string caption, uint flags);
}
'@
    }
    $choice=[Pad2TouchExperimentStart]::MessageBox([IntPtr]::Zero,"Click OK only when ready to operate the touchpad immediately. After activation, test three- and four-finger swipes and taps for $Seconds seconds, then lift all fingers. A start tone and restoration tone mark the test. Settings are unchanged while this dialog is open.",'Pad2Touch - start bounded gesture test',0x50041)
    if($choice -ne 1){Write-Output 'Experiment cancelled before any preference changes.';return}
}
$version=Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
if($Mode -eq 'session'){
    if($version.CurrentBuildNumber -cne '26300' -or [int]$version.UBR -ne 9974){throw 'GUI background guard is validated only on Windows build 26300.9974'}
    $guiOwner=Get-Process -Id $OwnerPid -ErrorAction SilentlyContinue
    if($null -eq $guiOwner -or $guiOwner.StartTime.ToUniversalTime().ToFileTimeUtc() -ne $OwnerStartedFileTime){throw 'GUI owner identity is unavailable or changed'}
}
if($SuppressTouchscreen -and ($Mode -notin @('session','probe') -or $version.CurrentBuildNumber -cne '26300' -or [int]$version.UBR -ne 9974)){
    throw 'Touchscreen gesture control requires a session or read-only probe on build 26300.9974'
}
if($Mode -eq 'probe'){
    $original=@()
    if(!$KeepTouchpadGestures){
        $cache=Read-Cache
        $key=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($subkey,$false)
        if($null -eq $key){throw 'Touchpad preferences key is unavailable'}
        try {$original=@(foreach($name in $names){$p=Get-Preference $key $name;$p | Add-Member -NotePropertyName cacheValue -NotePropertyValue $cache[$name];$p})} finally {$key.Dispose()}
    }
    $touchscreen=if($SuppressTouchscreen){Get-TouchscreenSnapshot}else{$null}
    [ordered]@{read_only=$true;build=$version.CurrentBuildNumber;display_version=$version.DisplayVersion;preferences=$original;touchscreen=$touchscreen} | ConvertTo-Json -Depth 5
    return
}
if(!$ExpectedBuild -or $ExpectedBuild -cne $version.CurrentBuildNumber){throw 'Apply requires -ExpectedBuild matching the explicitly reviewed current Windows build; no automatic version compatibility is claimed'}
if($KeepTouchpadGestures -and !$SuppressTouchscreen){throw 'Select at least one gesture override'}
if(!$RunDirectory){throw '-RunDirectory is required for apply'}
if($Mode -ne 'session' -and !(Test-Path -LiteralPath $captureExe)){throw 'Build the raw capture executable before applying settings'}
$runPath=[IO.Path]::GetFullPath($RunDirectory)
$allowedRoot=[IO.Path]::GetFullPath((Join-Path $repoPath 'artifacts'))+[IO.Path]::DirectorySeparatorChar
if(!$runPath.StartsWith($allowedRoot,[StringComparison]::OrdinalIgnoreCase)){throw 'RunDirectory must be under this checkout artifacts directory'}
if(Test-Path -LiteralPath $runPath){throw 'Choose a new experiment directory'}
$owner=[Threading.Mutex]::new($false,'Local\PadBridgeGestureCacheExperiment')
$owned=$false
try {
    try {$owned=$owner.WaitOne(0)} catch [Threading.AbandonedMutexException] {$owned=$true}
    if(!$owned){throw 'Another gesture settings experiment is active'}
    if(Test-Path -LiteralPath $leasePath){
        $previous=Get-Content -LiteralPath $leasePath -Raw | ConvertFrom-Json
        if(!$previous.journal){throw 'Invalid existing experiment lease; review recovery state before continuing'}
        $previousDirectory=Split-Path -Parent $previous.journal
        $previousDone=Join-Path $previousDirectory 'restored.json'
        if(!(Test-Path -LiteralPath $previousDone)){throw ('Earlier experiment is not recovered: '+$previous.journal)}
        $completion=Get-Content -LiteralPath $previousDone -Raw | ConvertFrom-Json
        $safeCompletion=($completion.registry_exact -and $completion.cache_exact) -or ($completion.no_mutation -and !(Test-Path -LiteralPath (Join-Path $previousDirectory 'mutation-started.json')))
        if((Test-Path -LiteralPath (Join-Path $previousDirectory 'touchscreen-required.json')) -and !$completion.touchscreen_exact -and !$completion.no_mutation){$safeCompletion=$false}
        if(!$completion.restored -or !$safeCompletion){throw ('Earlier experiment requires recovery review: '+$previous.journal)}
    }
    # Take the snapshot under the lease so it belongs to this session.
    $original=@()
    if(!$KeepTouchpadGestures){
        $cache=Read-Cache
        $key=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($subkey,$false)
        if($null -eq $key){throw 'Touchpad preferences key is unavailable'}
        try {$original=@(foreach($name in $names){$p=Get-Preference $key $name;$p | Add-Member -NotePropertyName cacheValue -NotePropertyValue $cache[$name];$p})} finally {$key.Dispose()}
    }
    foreach($p in $original){if($p.exists -and [BitConverter]::ToUInt32([Convert]::FromBase64String($p.bytes),0) -ne $p.cacheValue){throw ('Registry/cache prerequisite mismatch: '+$p.name)}}
    $touchscreen=if($SuppressTouchscreen){Get-TouchscreenSnapshot}else{$null}
    New-Item -ItemType Directory -Path $runPath | Out-Null
    $journalPath=Join-Path $runPath 'original.json'
    $deadline=if($Mode -eq 'session'){$null}else{[DateTime]::UtcNow.AddSeconds($Seconds+20).ToString('o')}
    Save-NewJson $journalPath ([ordered]@{format=1;id=[Guid]::NewGuid().ToString('N');subkey=$subkey;build=$version.CurrentBuildNumber;parentPid=$PID;parentStartedTicks=(Get-Process -Id $PID).StartTime.ToUniversalTime().Ticks;ownerPid=$OwnerPid;ownerStartedFileTime=$OwnerStartedFileTime;deadlineUtc=$deadline;preferences=$original;touchscreen=$touchscreen})
    Save-NewJson (Join-Path $runPath 'heartbeat.json') @{started=$true}
    if($SuppressTouchscreen){Save-NewJson (Join-Path $runPath 'touchscreen-required.json') @{required=$true}}
    New-Item -ItemType Directory -Path $leaseDirectory -Force | Out-Null
    # A malformed lease after an interrupted write blocks the next experiment.
    # The immutable original journal remains the recovery source of truth.
    $leaseData=[Text.UTF8Encoding]::new($false).GetBytes(([ordered]@{journal=$journalPath} | ConvertTo-Json))
    $leaseStream=[IO.File]::Open($leasePath,[IO.FileMode]::Create,[IO.FileAccess]::Write,[IO.FileShare]::Read)
    try {$leaseStream.Write($leaseData,0,$leaseData.Length);$leaseStream.Flush($true)} finally {$leaseStream.Dispose()}
    $shellExe=(Get-Process -Id $PID).Path
    if($Mode -eq 'session'){
        $backendStarted=(Get-Process -Id $PID).StartTime.ToUniversalTime().ToFileTimeUtc()
        $watchdogExe=$helper
        $watchdogArgs=@('--watchdog',('"'+$shellExe+'"'),('"'+$PSCommandPath+'"'),('"'+$journalPath+'"'),$PID,$backendStarted,$OwnerPid,$OwnerStartedFileTime)
    } else {
        $watchdogExe=$shellExe
        $watchdogArgs=@('-NoProfile','-File',('"'+$PSCommandPath+'"'),'-Mode','watchdog','-Journal',('"'+$journalPath+'"'))
    }
    $watchdog=Start-Process -FilePath $watchdogExe -ArgumentList $watchdogArgs -WindowStyle Hidden -RedirectStandardOutput (Join-Path $runPath 'watchdog-stdout.txt') -RedirectStandardError (Join-Path $runPath 'watchdog-stderr.txt') -PassThru
    $startup=[Diagnostics.Stopwatch]::StartNew()
    while(!(Test-Path -LiteralPath (Join-Path $runPath 'watchdog-ready.json'))){
        if($watchdog.HasExited -or $startup.Elapsed.TotalSeconds -gt 8){throw 'Independent recovery watchdog did not become ready; preferences unchanged'}
        Start-Sleep -Milliseconds 25
    }
    $mutationStarted=$false
    try {
        $j=Read-Journal $journalPath
        $writeLock=[Threading.Mutex]::new($false,('Local\PadBridgeGestureRestore-'+$j.id))
        $writeHeld=$false
        try {
            try {$writeHeld=$writeLock.WaitOne(3000)} catch [Threading.AbandonedMutexException] {$writeHeld=$true}
            if(!$writeHeld){throw 'Recovery owns the mutation lock; refusing to write'}
            if((Test-Path -LiteralPath (Join-Path $runPath 'restored.json')) -or (Test-Path -LiteralPath (Join-Path $runPath 'stop-requested.json')) -or $watchdog.HasExited -or (Test-DeadlineExpired $j)){throw 'Recovery completed, cancellation requested, unavailable, or expired before mutation; refusing to write'}
            if(!$KeepTouchpadGestures){
                $key=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($subkey,$true)
                if($null -eq $key){throw 'Cannot open the exact preferences key for this experiment'}
                try {
                    foreach($p in $original){if(!(Test-SamePreference $p (Get-Preference $key $p.name))){throw 'Preferences changed since snapshot; no mutation permitted'}}
                    Save-NewJson (Join-Path $runPath 'mutation-started.json') ([ordered]@{utc=[DateTime]::UtcNow.ToString('o');names=$names})
                    $mutationStarted=$true
                    foreach($name in $names){$key.SetValue($name,0,[Microsoft.Win32.RegistryValueKind]::DWord)}
                    $key.Flush()
                    foreach($name in $names){if((Get-Preference $key $name).bytes -cne 'AAAAAA=='){throw 'Disabled registry readback mismatch'}}
                } finally {$key.Dispose()}
            }
            if($SuppressTouchscreen){
                $currentTouchscreen=Get-TouchscreenSnapshot
                if($currentTouchscreen.runtime -ne $touchscreen.runtime -or !(Test-SamePreference $touchscreen.preference $currentTouchscreen.preference)){
                    throw 'Touchscreen settings changed since snapshot; refusing to override'
                }
                if(!$mutationStarted){
                    Save-NewJson (Join-Path $runPath 'mutation-started.json') ([ordered]@{utc=[DateTime]::UtcNow.ToString('o');names=@()})
                    $mutationStarted=$true
                }
                Save-NewJson (Join-Path $runPath 'touchscreen-mutation-started.json') @{utc=[DateTime]::UtcNow.ToString('o')}
                Set-TouchscreenGestures 0
                $afterTouchscreen=Get-TouchscreenSnapshot
                if(!(Test-SamePreference $touchscreen.preference $afterTouchscreen.preference)){throw 'Live touchscreen override unexpectedly changed the persistent preference'}
                Write-Host 'Touchscreen three/four-finger system gestures disabled; application multitouch preserved.'
            }
        } finally {if($writeHeld){$writeLock.ReleaseMutex()};$writeLock.Dispose()}
        if(!$KeepTouchpadGestures){
            $disabled=Read-Cache -Reload
            foreach($name in $names){if($disabled[$name] -ne 0){throw ('Disabled cache readback mismatch: '+$name)}}
        }
        Update-Heartbeat $runPath
        if((Test-Path -LiteralPath (Join-Path $runPath 'restored.json')) -or $watchdog.HasExited -or (Test-DeadlineExpired $j)){throw 'Recovery began before capture; refusing to start'}
        Save-NewJson (Join-Path $runPath 'active.json') ([ordered]@{active=$true;utc=[DateTime]::UtcNow.ToString('o');capture_target_seconds=$(if($Mode -eq 'session'){0}else{$Seconds});watchdog_deadline_utc=$j.deadlineUtc;scope='current-user';undocumented=$true})
        Write-Host "Gesture override active; deadline UTC=$($j.deadlineUtc) (empty means until stop); journal=$journalPath"
        try {[Console]::Beep(1100,250)} catch {}
        if($Mode -eq 'session'){
            $activeTimer=[Diagnostics.Stopwatch]::StartNew()
            $healthTimer=[Diagnostics.Stopwatch]::StartNew()
            while(!(Test-SessionExpired $Mode $activeTimer.Elapsed.TotalSeconds $Seconds) -and !(Test-Path -LiteralPath (Join-Path $runPath 'stop-requested.json'))){
                if(Test-Path -LiteralPath (Join-Path $runPath 'restored.json')){throw 'Recovery began during capture'}
                Update-Heartbeat $runPath
                $guiOwner=Get-Process -Id $OwnerPid -ErrorAction SilentlyContinue
                if($null -eq $guiOwner -or $guiOwner.StartTime.ToUniversalTime().ToFileTimeUtc() -ne $OwnerStartedFileTime){break}
                if($healthTimer.Elapsed.TotalSeconds -ge 1){
                    try {
                        if($watchdog.HasExited){throw 'Background guard recovery watchdog exited during capture'}
                        Assert-SessionGuard
                    } catch {
                        Save-NewJson (Join-Path $runPath 'failed.json') ([ordered]@{failed=$true;reason=$_.Exception.Message;utc=[DateTime]::UtcNow.ToString('o')})
                        # Let the GUI stop and drain injected contacts before restoration.
                        $drainTimer=[Diagnostics.Stopwatch]::StartNew()
                        while($drainTimer.Elapsed.TotalSeconds -lt 3 -and !(Test-Path -LiteralPath (Join-Path $runPath 'stop-requested.json'))){Start-Sleep -Milliseconds 50}
                        throw
                    }
                    $healthTimer.Restart()
                }
                Start-Sleep -Milliseconds 100
            }
            if(Test-SessionExpired $Mode $activeTimer.Elapsed.TotalSeconds $Seconds){
                Save-NewJson (Join-Path $runPath 'expired.json') ([ordered]@{expired=$true;utc=[DateTime]::UtcNow.ToString('o')})
                $drainTimer=[Diagnostics.Stopwatch]::StartNew()
                while($drainTimer.Elapsed.TotalSeconds -lt 3 -and !(Test-Path -LiteralPath (Join-Path $runPath 'stop-requested.json'))){Start-Sleep -Milliseconds 50}
            }
        } else {
            & $captureExe --capture --background --contacts 5 --seconds $Seconds --out (Join-Path $runPath 'capture')
            if($LASTEXITCODE -ne 0){throw 'Background capture failed; restoring preferences'}
        }
    } finally {
        if($mutationStarted){Restore-Preferences $journalPath}
        elseif(!(Test-Path -LiteralPath (Join-Path $runPath 'restored.json'))){Save-NewJson (Join-Path $runPath 'restored.json') ([ordered]@{restored=$true;no_mutation=$true})}
    }
} finally {if($owned){$owner.ReleaseMutex()};$owner.Dispose()}
