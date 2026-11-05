#requires -Version 7.3
$ErrorActionPreference = 'Stop'
# These are only command-plan requests. No compiler/client/profile is opened.
[string[]]$forwarded = @('--profile', 'synthetic profile with spaces', '--open', 'literal & Unicode ☃.torrent', '', '--dev')
$text = & (Join-Path $PSScriptRoot 'run-client.ps1') -PrintCommand -ClientArguments $forwarded
if ($LASTEXITCODE -ne 0) { throw 'Release launcher plan failed' }
$plan = ($text -join "`n") | ConvertFrom-Json
if ($plan.profile -ne 'release') { throw 'Application --dev changed the build profile' }
if ($plan.client_command.Count -ne $forwarded.Count + 1) { throw 'Launcher changed argv count' }
for ($index = 0; $index -lt $forwarded.Count; $index++) {
    if ($plan.client_command[$index + 1] -cne $forwarded[$index]) { throw "Launcher changed argv item $index" }
}
$text = & (Join-Path $PSScriptRoot 'run-client.ps1') -Dev -Tui -NoBuild -PrintCommand -ClientArguments @('--help')
if ($LASTEXITCODE -ne 0) { throw 'Development launcher plan failed' }
$plan = ($text -join "`n") | ConvertFrom-Json
if ($plan.profile -ne 'debug' -or $plan.frontend -ne 'tui' -or $plan.build_command.Count -ne 0 -or $plan.client_command[1] -ne '--help') {
    throw 'Explicit development/TUI launcher switches were not preserved'
}
Write-Output 'PowerShell launcher preserved build selection and literal argv'
