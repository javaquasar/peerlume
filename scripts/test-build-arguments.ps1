$ErrorActionPreference = 'Stop'
# A singleton emitted by an if expression becomes System.String unless typed.
# Native splatting then splits it into characters. Exercise the real argv bridge.
foreach ($flavor in @('desktop', 'tui')) {
    [string[]]$features = if ($flavor -eq 'tui') { @('--no-default-features', '--features', 'tui') } else { @('--all-features') }
    $actual = python -c 'import json,sys; print(json.dumps(sys.argv[1:]))' @features
    if ($LASTEXITCODE -ne 0) { throw 'Argument probe failed' }
    $expected = if ($flavor -eq 'tui') { '["--no-default-features", "--features", "tui"]' } else { '["--all-features"]' }
    if ($actual -ne $expected) { throw "Incorrect $flavor arguments: $actual" }
}
