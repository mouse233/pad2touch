param(
    [ValidateSet('probe','apply','restore','watchdog','self-test')][string]$Mode='probe',
    [string]$ExpectedBuild,
    [ValidateRange(15,120)][int]$Seconds=75,
    [string]$RunDirectory,
    [string]$Journal
)
$ErrorActionPreference='Stop'
$repoPath=Split-Path -Parent $PSScriptRoot
$helper=Join-Path $repoPath 'artifacts\gesture-cache-probe.exe'
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
function Get-RestoreAction($old,$current) {
    if(Test-SamePreference $old $current){return 'already-original'}
    if($current.exists -and $current.kind -ceq 'DWord' -and $current.bytes -ceq 'AAAAAA=='){return 'restore-owned-zero'}
    return 'conflict'
}
function Save-NewJson([string]$path,$value) {
    $json=$value | ConvertTo-Json -Depth 8
    $stream=[IO.File]::Open($path,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::Read)
    try {$data=[Text.UTF8Encoding]::new($false).GetBytes($json);$stream.Write($data,0,$data.Length);$stream.Flush($true)} finally {$stream.Dispose()}
}
function Read-Cache {
    $output=@(& $helper --read-cache)
    if($LASTEXITCODE -ne 0){throw 'Cache read probe failed'}
    $values=@{}
    foreach($line in $output){
        if($line -match '^candidate_setting=(\w+).* dword_hr=0x00000000 dword_value=(\d+)$'){$values[$Matches[1]]=[uint32]$Matches[2]}
    }
    if($values.Count -ne 4){throw 'Four successful DWORD cache reads are required'}
    return $values
}
function Reload-Cache {
    $output=@(& $helper --reload-four)
    $output | ForEach-Object {Write-Host $_}
    if($LASTEXITCODE -ne 0){throw 'At least one precise cache notification failed'}
}
function Read-Journal([string]$path) {
    $resolved=(Resolve-Path -LiteralPath $path).Path
    $artifactRoot=[IO.Path]::GetFullPath((Join-Path $repoPath 'artifacts'))+[IO.Path]::DirectorySeparatorChar
    if(!$resolved.StartsWith($artifactRoot,[StringComparison]::OrdinalIgnoreCase)){throw 'Recovery journal must be under this checkout artifacts directory'}
    $j=Get-Content -LiteralPath $resolved -Raw | ConvertFrom-Json
    if($j.format -ne 1 -or $j.subkey -cne $subkey -or @($j.preferences).Count -ne 4){throw 'Unsupported recovery journal'}
    if(@($j.preferences.name | Sort-Object -Unique).Count -ne 4){throw 'Duplicate recovery names'}
    foreach($p in $j.preferences){
        if($names -cnotcontains $p.name){throw 'Unexpected recovery name'}
        if($p.exists){
            if($p.kind -cne 'DWord' -or [Convert]::FromBase64String($p.bytes).Length -ne 4){throw 'Unsupported recovery bytes'}
        } elseif($p.kind -cne 'Absent' -or $p.bytes -cne ''){throw 'Invalid absent-value snapshot'}
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
        $key=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($subkey,$true)
        if($null -eq $key){throw 'Original touchpad preferences key is unavailable'}
        $conflicts=@()
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
        Reload-Cache
        if($conflicts.Count){throw ('Restored other owned values; retained external/unsupported values requiring review: '+($conflicts -join ','))}
        $cache=Read-Cache
        foreach($p in $j.preferences){if($cache[$p.name] -ne [uint32]$p.cacheValue){throw ('Cache restoration mismatch: '+$p.name)}}
        Save-NewJson $done ([ordered]@{restored=$true;registry_exact=$true;cache_exact=$true;utc=[DateTime]::UtcNow.ToString('o')})
        Write-Host 'Exact registry presence/type/bytes and cache values restored.'
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
    Write-Output 'restoration_decision_tests=6 passed; registry_access=false'
    return
}
if(!(Test-Path -LiteralPath $helper)){throw 'Compile experiments/gesture-cache-probe.rs into artifacts/gesture-cache-probe.exe first'}
if($Mode -eq 'restore'){Restore-Preferences $Journal;return}
if($Mode -eq 'watchdog'){
    $j=Read-Journal $Journal
    $dir=Split-Path -Parent $Journal
    [void](Read-Cache)
    Save-NewJson (Join-Path $dir 'watchdog-ready.json') ([ordered]@{pid=$PID;ready=$true})
    while(!(Test-Path -LiteralPath (Join-Path $dir 'restored.json'))){
        $parent=Get-Process -Id $j.parentPid -ErrorAction SilentlyContinue
        if($null -eq $parent -or $parent.StartTime.ToUniversalTime().Ticks -ne [long]$j.parentStartedTicks -or [DateTime]::UtcNow -ge [DateTime]::Parse($j.deadlineUtc).ToUniversalTime()){break}
        Start-Sleep -Milliseconds 250
    }
    if(!(Test-Path -LiteralPath (Join-Path $dir 'restored.json'))){Restore-Preferences $Journal}
    return
}
$version=Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
$cache=Read-Cache
$key=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($subkey,$false)
if($null -eq $key){throw 'Touchpad preferences key is unavailable'}
try {$original=@(foreach($name in $names){$p=Get-Preference $key $name;$p | Add-Member -NotePropertyName cacheValue -NotePropertyValue $cache[$name];$p})} finally {$key.Dispose()}
if($Mode -eq 'probe'){
    [ordered]@{read_only=$true;build=$version.CurrentBuildNumber;display_version=$version.DisplayVersion;preferences=$original} | ConvertTo-Json -Depth 5
    return
}
if(!$ExpectedBuild -or $ExpectedBuild -cne $version.CurrentBuildNumber){throw 'Apply requires -ExpectedBuild matching the explicitly reviewed current Windows build; no automatic version compatibility is claimed'}
foreach($p in $original){if($p.exists -and [BitConverter]::ToUInt32([Convert]::FromBase64String($p.bytes),0) -ne $p.cacheValue){throw ('Registry/cache prerequisite mismatch: '+$p.name)}}
if(!$RunDirectory){throw '-RunDirectory is required for apply'}
if(!(Test-Path -LiteralPath (Join-Path $repoPath 'target\release\padbridge-phase0.exe'))){throw 'Build the raw capture executable before applying settings'}
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
        if(!$completion.restored -or !$safeCompletion){throw ('Earlier experiment requires recovery review: '+$previous.journal)}
    }
    # Re-snapshot under the global lease after excluding an unfinished session.
    $cache=Read-Cache
    $key=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($subkey,$false)
    if($null -eq $key){throw 'Touchpad preferences key is unavailable'}
    try {$original=@(foreach($name in $names){$p=Get-Preference $key $name;$p | Add-Member -NotePropertyName cacheValue -NotePropertyValue $cache[$name];$p})} finally {$key.Dispose()}
    foreach($p in $original){if($p.exists -and [BitConverter]::ToUInt32([Convert]::FromBase64String($p.bytes),0) -ne $p.cacheValue){throw ('Registry/cache prerequisite mismatch: '+$p.name)}}
    New-Item -ItemType Directory -Path $runPath | Out-Null
    $journalPath=Join-Path $runPath 'original.json'
    Save-NewJson $journalPath ([ordered]@{format=1;id=[Guid]::NewGuid().ToString('N');subkey=$subkey;build=$version.CurrentBuildNumber;parentPid=$PID;parentStartedTicks=(Get-Process -Id $PID).StartTime.ToUniversalTime().Ticks;deadlineUtc=[DateTime]::UtcNow.AddSeconds($Seconds+20).ToString('o');preferences=$original})
    New-Item -ItemType Directory -Path $leaseDirectory -Force | Out-Null
    # A malformed lease after an interrupted write blocks the next experiment.
    # The immutable original journal remains the recovery source of truth.
    $leaseData=[Text.UTF8Encoding]::new($false).GetBytes(([ordered]@{journal=$journalPath} | ConvertTo-Json))
    $leaseStream=[IO.File]::Open($leasePath,[IO.FileMode]::Create,[IO.FileAccess]::Write,[IO.FileShare]::Read)
    try {$leaseStream.Write($leaseData,0,$leaseData.Length);$leaseStream.Flush($true)} finally {$leaseStream.Dispose()}
    $shellExe=(Get-Process -Id $PID).Path
    $watchdog=Start-Process -FilePath $shellExe -ArgumentList @('-NoProfile','-File',('"'+$PSCommandPath+'"'),'-Mode','watchdog','-Journal',('"'+$journalPath+'"')) -WindowStyle Hidden -RedirectStandardOutput (Join-Path $runPath 'watchdog-stdout.txt') -RedirectStandardError (Join-Path $runPath 'watchdog-stderr.txt') -PassThru
    $startup=[Diagnostics.Stopwatch]::StartNew()
    while(!(Test-Path -LiteralPath (Join-Path $runPath 'watchdog-ready.json'))){
        if($watchdog.HasExited -or $startup.Elapsed.TotalSeconds -gt 8){throw 'Independent recovery watchdog did not become ready; preferences unchanged'}
        Start-Sleep -Milliseconds 100
    }
    $mutationStarted=$false
    try {
        $j=Read-Journal $journalPath
        $writeLock=[Threading.Mutex]::new($false,('Local\PadBridgeGestureRestore-'+$j.id))
        $writeHeld=$false
        try {
            try {$writeHeld=$writeLock.WaitOne(3000)} catch [Threading.AbandonedMutexException] {$writeHeld=$true}
            if(!$writeHeld){throw 'Recovery owns the mutation lock; refusing to write'}
            if((Test-Path -LiteralPath (Join-Path $runPath 'restored.json')) -or $watchdog.HasExited -or [DateTime]::UtcNow -ge [DateTime]::Parse($j.deadlineUtc).ToUniversalTime()){throw 'Recovery completed, unavailable, or expired before mutation; refusing to write'}
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
        } finally {if($writeHeld){$writeLock.ReleaseMutex()};$writeLock.Dispose()}
        Reload-Cache
        $disabled=Read-Cache
        foreach($name in $names){if($disabled[$name] -ne 0){throw ('Disabled cache readback mismatch: '+$name)}}
        if((Test-Path -LiteralPath (Join-Path $runPath 'restored.json')) -or $watchdog.HasExited -or [DateTime]::UtcNow -ge [DateTime]::Parse($j.deadlineUtc).ToUniversalTime()){throw 'Recovery began before capture; refusing to start'}
        Save-NewJson (Join-Path $runPath 'active.json') ([ordered]@{active=$true;utc=[DateTime]::UtcNow.ToString('o');capture_target_seconds=$Seconds;watchdog_deadline_utc=$j.deadlineUtc;scope='current-user';undocumented=$true})
        Write-Host "Experimental override active; capture target=$Seconds seconds; watchdog deadline UTC=$($j.deadlineUtc); journal=$journalPath"
        & (Join-Path $repoPath 'target\release\padbridge-phase0.exe') --capture --background --contacts 5 --seconds $Seconds --out (Join-Path $runPath 'capture')
        if($LASTEXITCODE -ne 0){throw 'Background capture failed; restoring preferences'}
    } finally {
        if($mutationStarted){Restore-Preferences $journalPath}
        elseif(!(Test-Path -LiteralPath (Join-Path $runPath 'restored.json'))){Save-NewJson (Join-Path $runPath 'restored.json') ([ordered]@{restored=$true;no_mutation=$true})}
    }
} finally {if($owned){$owner.ReleaseMutex()};$owner.Dispose()}
