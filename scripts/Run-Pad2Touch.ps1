param(
    [ValidateSet('gui','probe','capture','self-test')][string]$Mode='gui',
    [switch]$Inject,
    [switch]$SuppressSystemGestures,
    [switch]$Background,
    [ValidateRange(1,5)][int]$Contacts=5,
    [ValidateRange(1,86400)][int]$Seconds=120,
    [ValidateRange(0,65535)][int]$Device=0,
    [switch]$Strict
)
$ErrorActionPreference='Stop'
$repoPath=Split-Path -Parent $PSScriptRoot
$exePath=Join-Path $repoPath 'target\release\Pad2Touch.exe'
if(!(Test-Path -LiteralPath $exePath)){throw 'Build first: cargo build --release --locked'}
Push-Location -LiteralPath $repoPath
try {
    if($Mode -eq 'gui'){
        if($Inject -or $SuppressSystemGestures -or $Background){throw 'Use the GUI controls for injection and foreground gesture ownership; -Background is a CLI capture diagnostic'}
        & $exePath --gui
        if($LASTEXITCODE -ne 0){throw 'Pad2Touch GUI failed'}
        return
    }
    if($Mode -eq 'probe'){& $exePath --probe;if($LASTEXITCODE -ne 0){throw 'Device discovery failed'};return}
    if($Inject -and $Mode -ne 'capture'){throw '-Inject is only used with capture; self-test injects automatically'}
    if($Background -and ($Mode -ne 'capture' -or $Inject)){throw '-Background requires raw-only -Mode capture; omit -Inject'}
    $stamp=Get-Date -Format 'yyyyMMdd-HHmmss-fff'
    $runPath=Join-Path $repoPath "artifacts\phase12-$Mode-$Contacts-$stamp"
    $runArgs=@("--$Mode",'--contacts',"$Contacts",'--seconds',"$Seconds",'--out',$runPath)
    if($Mode -eq 'capture'){$runArgs+=@('--device',"$Device");if($Inject){$runArgs+='--inject'}}
    if($SuppressSystemGestures){$runArgs+='--suppress-system-gestures'}
    if($Background){$runArgs+='--background'}
    & $exePath @runArgs
    $experimentCode=$LASTEXITCODE
    Write-Output "Local results: $runPath"
    if($Inject -or $Mode -eq 'self-test'){
        if(Test-Path -LiteralPath (Join-Path $runPath 'events.log')){
            & (Join-Path $PSScriptRoot 'Analyze-Run.ps1') -RunDirectory $runPath -Strict:$Strict
        }
    }
    if($experimentCode -ne 0){
        $message="Experiment did not complete cleanly (exit $experimentCode); inspect local results above."
        if($Strict){throw $message};Write-Warning $message
    }
} finally {Pop-Location}
