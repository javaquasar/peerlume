param([string]$IconDirectory = (Join-Path $PSScriptRoot '../assets/icons'))
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

# Packaging only: resize the approved master without changing its artwork.
$directory = (Resolve-Path -LiteralPath $IconDirectory).Path
$master = [System.Drawing.Bitmap]::new((Join-Path $directory 'peerlume-master.png'))
try {
    if ($master.Width -ne $master.Height) { throw 'The icon master must be square.' }
    if ($master.GetPixel(0, 0).A -ne 0) { throw 'The icon master must have a transparent background.' }
    $sizes = @(16, 24, 32, 48, 64, 128, 256, 512)
    $images = @{}
    foreach ($size in $sizes) {
        $bitmap = [System.Drawing.Bitmap]::new($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        $stream = [System.IO.MemoryStream]::new()
        try {
            $graphics.Clear([System.Drawing.Color]::Transparent)
            $graphics.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
            $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
            $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
            $graphics.DrawImage($master, [System.Drawing.Rectangle]::new(0, 0, $size, $size))
            $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
            $images[$size] = $stream.ToArray()
            [System.IO.File]::WriteAllBytes((Join-Path $directory "peerlume-$size.png"), $images[$size])
        } finally {
            $stream.Dispose()
            $graphics.Dispose()
            $bitmap.Dispose()
        }
    }

    # ICO directory plus PNG-compressed entries; 0 denotes a 256-pixel dimension.
    $icoSizes = @($sizes | Where-Object { $_ -le 256 })
    $ico = [System.IO.MemoryStream]::new()
    $writer = [System.IO.BinaryWriter]::new($ico)
    try {
        $writer.Write([uint16]0)
        $writer.Write([uint16]1)
        $writer.Write([uint16]$icoSizes.Count)
        $offset = 6 + 16 * $icoSizes.Count
        foreach ($size in $icoSizes) {
            $dimension = if ($size -eq 256) { 0 } else { $size }
            $writer.Write([byte]$dimension)
            $writer.Write([byte]$dimension)
            $writer.Write([byte]0)
            $writer.Write([byte]0)
            $writer.Write([uint16]1)
            $writer.Write([uint16]32)
            $writer.Write([uint32]$images[$size].Length)
            $writer.Write([uint32]$offset)
            $offset += $images[$size].Length
        }
        foreach ($size in $icoSizes) { $writer.Write([byte[]]$images[$size]) }
        $writer.Flush()
        [System.IO.File]::WriteAllBytes((Join-Path $directory 'peerlume.ico'), $ico.ToArray())
    } finally {
        $writer.Dispose()
        $ico.Dispose()
    }
    Write-Output "Exported PNG sizes $($sizes -join ', ') and the multi-resolution Windows ICO."
} finally {
    $master.Dispose()
}
