param([string]$BinaryDirectory = 'target/debug', [string]$OutputDirectory = 'dist/peerlume-windows-x64')
$ErrorActionPreference = 'Stop'
$repository = Split-Path $PSScriptRoot -Parent
$destination = [System.IO.Path]::GetFullPath((Join-Path $repository $OutputDirectory))
if (-not $destination.StartsWith($repository + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw 'Package output must remain inside the repository.'
}
if (Test-Path -LiteralPath $destination) { throw 'Package destination already exists; choose a new output directory.' }
$binaries = Join-Path $repository $BinaryDirectory
$names = @('peerlume.exe', 'peerlume-tui.exe', 'peerlume-control.exe', 'peerlume-mcp.exe')
foreach ($name in $names) {
    if (-not (Test-Path -LiteralPath (Join-Path $binaries $name))) { throw "Missing binary: $name. Build with cargo build --locked --all-features --bins." }
}
& (Join-Path $PSScriptRoot 'check-app-icon.ps1') -BinaryPath (Join-Path $binaries 'peerlume.exe')
New-Item -ItemType Directory -Path $destination | Out-Null
foreach ($name in $names) { Copy-Item -LiteralPath (Join-Path $binaries $name) -Destination $destination }
Copy-Item -LiteralPath (Join-Path $repository 'docs/agent-control.md') -Destination $destination
$checksums = foreach ($name in $names) {
    $hash = Get-FileHash -LiteralPath (Join-Path $destination $name) -Algorithm SHA256
    "$($hash.Hash.ToLowerInvariant())  $name"
}
$checksums | Set-Content -LiteralPath (Join-Path $destination 'SHA256SUMS') -Encoding utf8
& (Join-Path $destination 'peerlume-control.exe') --version
if ($LASTEXITCODE -ne 0) { throw 'Packaged CLI smoke check failed.' }
Compress-Archive -LiteralPath $destination -DestinationPath ($destination + '.zip')
Write-Output ($destination + '.zip')
