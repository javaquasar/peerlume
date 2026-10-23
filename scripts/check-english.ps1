param(
    [string]$Root = (Join-Path $PSScriptRoot '..'),
    [switch]$SelfTest
)

Set-StrictMode -Version 3
$ErrorActionPreference = 'Stop'

# English is the repository language. Non-ASCII punctuation and UI symbols are
# allowed; reject Cyrillic in publication paths and text, not Unicode generally.
$cyrillic = [regex]::new('[\u0400-\u052F\u1C80-\u1C8F\u2DE0-\u2DFF\uA640-\uA69F\uFE2E-\uFE2F]|\uD838[\uDC30-\uDC8F]')

if ($SelfTest) {
    foreach ($codePoint in @(0x0410, 0x044F, 0x0500, 0x1C80, 0x2DE0, 0xA640, 0xFE2E, 0x1E030)) {
        if (-not $cyrillic.IsMatch([char]::ConvertFromUtf32($codePoint))) {
            throw ('Missing Cyrillic code point U+{0:X}' -f $codePoint)
        }
    }
    foreach ($allowed in @('English documentation', 'src/tracker.rs', ('Download ' + [char]0x2192 + ' complete'))) {
        if ($cyrillic.IsMatch($allowed)) {
            throw 'An English string or an allowed UI symbol was rejected'
        }
    }
    Write-Output 'English-only checker self-tests passed.'
    exit 0
}

$repositoryRoot = (Resolve-Path -LiteralPath $Root).Path
$gitRoot = $repositoryRoot.Replace('\', '/')
# Use the repository ignore rules, not machine-specific global ignore settings.
# NUL-delimited output also supports spaces and newlines in tracked file names.
$listed = @(& git -C $repositoryRoot -c "safe.directory=$gitRoot" -c core.quotepath=false -c core.excludesFile= ls-files --cached --others --exclude-standard -z)
if ($LASTEXITCODE -ne 0) {
    throw 'Could not list repository files with git ls-files'
}
$paths = [string]::Join("`n", $listed).Split([char]0, [System.StringSplitOptions]::RemoveEmptyEntries) | Sort-Object -Unique
$violations = [System.Collections.Generic.List[string]]::new()
$utf8 = [System.Text.UTF8Encoding]::new($false, $true)
$textFiles = 0
$binaryFiles = 0
$existingPaths = 0

foreach ($relative in $paths) {
    $path = Join-Path $repositoryRoot $relative
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        continue # Deleted tracked paths will not be published.
    }
    $existingPaths++
    if ($cyrillic.IsMatch($relative)) {
        $violations.Add("${relative}: Cyrillic characters in file name")
    }
    $item = Get-Item -LiteralPath $path
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        $violations.Add("${relative}: symbolic links require manual language review")
        continue
    }
    $bytes = [System.IO.File]::ReadAllBytes($path)
    $hasWideBom = ($bytes.Length -ge 2 -and (
        ($bytes[0] -eq 0xFF -and $bytes[1] -eq 0xFE) -or
        ($bytes[0] -eq 0xFE -and $bytes[1] -eq 0xFF))) -or (
        $bytes.Length -ge 4 -and $bytes[0] -eq 0 -and $bytes[1] -eq 0 -and
        $bytes[2] -eq 0xFE -and $bytes[3] -eq 0xFF)
    if ($hasWideBom) {
        $text = [System.IO.File]::ReadAllText($path)
    } elseif ($bytes -contains 0) {
        $binaryFiles++
        continue
    } else {
        try {
            $text = $utf8.GetString($bytes)
        } catch {
            $violations.Add("${relative}: text is not valid UTF-8; review its encoding")
            continue
        }
    }
    $textFiles++
    $match = $cyrillic.Match($text)
    if ($match.Success) {
        $line = $text.Substring(0, $match.Index).Split([char]10).Length
        # Do not print source lines: a local file might contain credentials.
        $violations.Add("${relative}:${line}: Cyrillic text; translate it to English")
    }
}

if ($violations.Count -gt 0) {
    $violations | Write-Output
    Write-Output "English-only check failed: $($violations.Count) issue(s)."
    exit 1
}
Write-Output "English-only check passed: $textFiles text files, $existingPaths file names; $binaryFiles binary file(s) skipped."
