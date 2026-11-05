#requires -Version 7.3
param(
    [Alias('Development')][switch]$Dev,
    [switch]$Tui,
    [switch]$NoBuild,
    [switch]$PrintCommand,
    [Parameter(ValueFromRemainingArguments = $true)]
    [AllowEmptyString()][AllowEmptyCollection()][string[]]$ClientArguments = @()
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandArgumentPassing = 'Standard'
# PowerShell consumes its own -- marker. Bind launcher switches explicitly and
# insert a fresh native separator so application --dev/--help remain application args.
$arguments = [System.Collections.Generic.List[string]]::new()
if ($Dev) { $arguments.Add('--dev') }
if ($Tui) { $arguments.Add('--tui') }
if ($NoBuild) { $arguments.Add('--no-build') }
if ($PrintCommand) { $arguments.Add('--print-command') }
$arguments.Add('--')
foreach ($argument in $ClientArguments) { $arguments.Add($argument) }
& python (Join-Path $PSScriptRoot 'run_client.py') @arguments
exit $LASTEXITCODE
