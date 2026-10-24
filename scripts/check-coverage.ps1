param([string]$Report='coverage/summary.json',[string]$Baseline='config/coverage-baseline.json',[switch]$SelfTest)
$ErrorActionPreference='Stop'
function Read-Metrics($document) {
    $totals=$document.data[0].totals
    $result=@{}
    foreach($name in @('lines','functions')) {
        $metric=$totals.$name
        if($null -eq $metric -or $metric.count -le 0 -or $metric.covered -lt 0 -or $metric.covered -gt $metric.count) { throw "Invalid $name counts" }
        $percent=100.0*$metric.covered/$metric.count
        if([double]::IsNaN($percent) -or [double]::IsInfinity($percent) -or [math]::Abs($percent-$metric.percent) -gt 0.0001) { throw "Invalid $name percentage" }
        $result[$name]=$percent
    }
    return $result
}
function Assert-Baseline($metrics,$baseline) {
    if($baseline.scope -ne 'linux-core-tui' -or $baseline.schema -ne 1) { throw 'Invalid coverage baseline scope or schema' }
    foreach($name in @('lines','functions')) {
        $minimum=$baseline.$name
        if($null -eq $minimum -or $minimum -isnot [ValueType] -or $minimum -lt 0 -or $minimum -gt 100) { throw "Invalid $name baseline" }
        if($metrics[$name]+0.000001 -lt $minimum) { throw "$name coverage $($metrics[$name])% is below baseline $minimum%" }
    }
}
if($SelfTest) {
    $metrics=Read-Metrics ('{"data":[{"totals":{"lines":{"count":100,"covered":90,"percent":90},"functions":{"count":10,"covered":8,"percent":80}}}]}'|ConvertFrom-Json)
    Assert-Baseline $metrics ([pscustomobject]@{schema=1;scope='linux-core-tui';lines=90;functions=80})
    foreach($bad in @(
        [pscustomobject]@{schema=1;scope='linux-core-tui';lines=91;functions=80},
        [pscustomobject]@{schema=1;scope='linux-core-tui';lines=90;functions=81},
        [pscustomobject]@{schema=1;scope='wrong';lines=0;functions=0},
        [pscustomobject]@{schema=1;scope='linux-core-tui';lines=-1;functions=80},
        [pscustomobject]@{schema=1;scope='linux-core-tui';lines='90';functions=80}
    )) { $rejected=$false;try{Assert-Baseline $metrics $bad}catch{$rejected=$true};if(-not $rejected){throw 'Invalid baseline accepted'} }
    $rejected=$false;try{Read-Metrics ('{"data":[{"totals":{"lines":{"count":0,"covered":0,"percent":100}}}]}'|ConvertFrom-Json)}catch{$rejected=$true};if(-not $rejected){throw 'Empty report accepted'}
    Write-Output 'Coverage gate self-tests passed';exit 0
}
$metrics=Read-Metrics (Get-Content -LiteralPath $Report -Raw|ConvertFrom-Json)
Assert-Baseline $metrics (Get-Content -LiteralPath $Baseline -Raw|ConvertFrom-Json)
Write-Output "Coverage baseline passed: lines=$($metrics.lines)%, functions=$($metrics.functions)%"
