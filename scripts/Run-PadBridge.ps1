param(
    [ValidateSet('probe','capture','self-test')][string]$Mode='probe',
    [switch]$Inject,
    [ValidateRange(1,5)][int]$Contacts=5,
    [ValidateRange(1,86400)][int]$Seconds=120,
    [ValidateRange(0,65535)][int]$Device=0
)
$ErrorActionPreference='Stop'
$repoPath=Split-Path -Parent $PSScriptRoot
$exePath=Join-Path $repoPath 'target\release\padbridge-phase0.exe'
if(!(Test-Path -LiteralPath $exePath)){throw 'Build first: cargo build --release --locked'}
Push-Location -LiteralPath $repoPath
try {
    if($Mode -eq 'probe'){& $exePath --probe;if($LASTEXITCODE -ne 0){throw 'Device discovery failed'};return}
    if($Inject -and $Mode -ne 'capture'){throw '-Inject is only used with capture; self-test injects automatically'}
    $stamp=Get-Date -Format 'yyyyMMdd-HHmmss-fff'
    $runPath=Join-Path $repoPath "artifacts\phase12-$Mode-$Contacts-$stamp"
    $runArgs=@("--$Mode",'--contacts',"$Contacts",'--seconds',"$Seconds",'--out',$runPath)
    if($Mode -eq 'capture'){$runArgs+=@('--device',"$Device");if($Inject){$runArgs+='--inject'}}
    & $exePath @runArgs
    if($LASTEXITCODE -ne 0){throw "Experiment failed; inspect $runPath"}
    if($Inject -or $Mode -eq 'self-test'){& (Join-Path $PSScriptRoot 'Analyze-Run.ps1') -RunDirectory $runPath}
    Write-Output "Local results: $runPath"
} finally {Pop-Location}
