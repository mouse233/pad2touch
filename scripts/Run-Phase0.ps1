param(
    [ValidateSet('probe','capture','self-test')][string]$Mode='probe',
    [switch]$Inject,
    [int]$Seconds=90,
    [int]$Device=0
)
$ErrorActionPreference='Stop'
$repoPath=Split-Path -Parent $PSScriptRoot
$exePath=Join-Path $repoPath 'target\release\Pad2Touch.exe'
if (!(Test-Path -LiteralPath $exePath)) { throw 'Build first: cargo build --release --locked' }
Push-Location -LiteralPath $repoPath
try {
    if ($Mode -eq 'probe') { & $exePath --probe; exit $LASTEXITCODE }
    $stamp=Get-Date -Format 'yyyyMMdd-HHmmss'
    $runPath=Join-Path $repoPath "artifacts\$Mode-$stamp"
    $arguments=@("--$Mode",'--contacts','1','--seconds',"$Seconds",'--out',$runPath)
    if ($Mode -eq 'capture') { $arguments+=@('--device',"$Device"); if ($Inject) { $arguments+='--inject' } }
    & $exePath @arguments
    if ($LASTEXITCODE -ne 0) { throw "Experiment exited with code $LASTEXITCODE" }
    if($Inject -or $Mode -eq 'self-test'){& (Join-Path $PSScriptRoot 'Analyze-Run.ps1') -RunDirectory $runPath}
} finally { Pop-Location }
