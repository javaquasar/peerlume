param(
    [int]$MinimumLineCoverage = 55
)

$ErrorActionPreference = 'Stop'

cargo llvm-cov `
    --locked `
    --all-features `
    --workspace `
    --fail-under-lines $MinimumLineCoverage

if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}
