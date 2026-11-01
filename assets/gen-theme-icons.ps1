# Builds the window icon for each non-default theme: assets/icon-<slug>.ico,
# which assets/icon.rc compiles into the executable and
# shell::theme::Theme::icon_resource names.
#
# The mark's geometry is not repeated here. assets/icon.png is the one
# rendering of it, and every colour in it is one of the five glacier mark
# shades from src/shell/theme/tokens.css — so a themed icon is that image
# with its ramp remapped: each pixel's brightness picks the matching point
# on the theme's own five shades, which keeps the facets, the rounded plate
# and every antialiased edge exactly where they were.
#
# Run it after changing a theme's --mark-* tokens or the artwork:
#   powershell -ExecutionPolicy Bypass -File assets/gen-theme-icons.ps1
# It also writes a 256px PNG of each theme next to the .ico as a preview;
# those are not shipped.
[CmdletBinding()]
param([string]$Preview = $null)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$tokens = Get-Content (Join-Path $root 'src\shell\theme\tokens.css') -Raw
$source = Join-Path $root 'assets\icon.png'
# What LoadImage is asked for (16 and 32) plus the sizes Explorer, Alt+Tab
# and the task switcher reach for.
$sizes = @(16, 20, 24, 32, 40, 48, 64, 256)
$shades = @('plate', 'core', 'mid', 'hi', 'lit')

# The five --mark-* values out of one theme's block in tokens.css. The
# stylesheet is the one place a colour is written down; this reads it
# rather than keeping a second copy of the palette.
function Get-MarkRamp([string]$slug) {
    $at = $tokens.IndexOf("[data-theme=""$slug""]")
    if ($at -lt 0) { throw "tokens.css has no block for $slug" }
    $block = $tokens.Substring($at)
    $block = $block.Substring(0, $block.IndexOf('}'))
    $shades | ForEach-Object {
        $m = [regex]::Match($block, "--mark-$_\s*:\s*#([0-9a-fA-F]{6})\s*;")
        if (-not $m.Success) { throw "the $slug block does not define --mark-$_" }
        $hex = $m.Groups[1].Value
        [PSCustomObject]@{
            R = [Convert]::ToInt32($hex.Substring(0, 2), 16)
            G = [Convert]::ToInt32($hex.Substring(2, 2), 16)
            B = [Convert]::ToInt32($hex.Substring(4, 2), 16)
        }
    }
}

function Get-Luma($c) { 0.299 * $c.R + 0.587 * $c.G + 0.114 * $c.B }

# brightness -> colour: 256 entries of B, G, R laid end to end, the order
# GDI+ keeps its pixels in. The glacier shades say where each stop sits;
# the theme's shades say what colour it becomes, and anything between two
# stops is interpolated, which is what keeps every edge smooth.
function New-Lut($from, $to) {
    $stops = @($from | ForEach-Object { Get-Luma $_ })
    $lut = New-Object byte[] 768
    for ($i = 0; $i -lt 256; $i++) {
        $at = 0
        while ($at -lt $stops.Count - 2 -and $i -gt $stops[$at + 1]) { $at++ }
        $lo = $stops[$at]
        $hi = $stops[$at + 1]
        $t = if ($hi -gt $lo) { ($i - $lo) / ($hi - $lo) } else { 0 }
        $t = [Math]::Max(0.0, [Math]::Min(1.0, $t))
        $a = $to[$at]
        $b = $to[$at + 1]
        $lut[$i * 3] = [byte][Math]::Round($a.B + ($b.B - $a.B) * $t)
        $lut[$i * 3 + 1] = [byte][Math]::Round($a.G + ($b.G - $a.G) * $t)
        $lut[$i * 3 + 2] = [byte][Math]::Round($a.R + ($b.R - $a.R) * $t)
    }
    , $lut
}

# The source artwork with its ramp remapped, as a new bitmap.
function New-Recolored([System.Drawing.Bitmap]$src, $lut) {
    $out = New-Object System.Drawing.Bitmap $src.Width, $src.Height,
        ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $rect = New-Object System.Drawing.Rectangle 0, 0, $src.Width, $src.Height
    $read = $src.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $write = $out.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::WriteOnly,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    try {
        $count = [Math]::Abs($read.Stride) * $src.Height
        $bytes = New-Object byte[] $count
        [System.Runtime.InteropServices.Marshal]::Copy($read.Scan0, $bytes, 0, $count)
        for ($i = 0; $i -lt $count; $i += 4) {
            # GDI+ hands over BGRA.
            $luma = [int][Math]::Round(0.114 * $bytes[$i] + 0.587 * $bytes[$i + 1] +
                0.299 * $bytes[$i + 2])
            if ($luma -gt 255) { $luma = 255 }
            $at = $luma * 3
            $bytes[$i] = $lut[$at]
            $bytes[$i + 1] = $lut[$at + 1]
            $bytes[$i + 2] = $lut[$at + 2]
        }
        [System.Runtime.InteropServices.Marshal]::Copy($bytes, 0, $write.Scan0, $count)
    } finally {
        $src.UnlockBits($read)
        $out.UnlockBits($write)
    }
    $out
}

function New-Scaled([System.Drawing.Bitmap]$src, [int]$size) {
    $out = New-Object System.Drawing.Bitmap $size, $size,
        ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($out)
    try {
        $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
        $g.Clear([System.Drawing.Color]::Transparent)
        $g.DrawImage($src, (New-Object System.Drawing.Rectangle 0, 0, $size, $size))
    } finally { $g.Dispose() }
    $out
}

# One frame, as the bytes that go in the .ico. The sizes Windows actually
# loads for a window icon are written as plain DIBs — LoadImage has
# handled those since there were icons — and only the 256 is PNG, where
# a DIB would be a quarter of a megabyte.
function Get-Frame([System.Drawing.Bitmap]$bmp) {
    $w = $bmp.Width
    if ($w -eq 256) {
        $mem = New-Object System.IO.MemoryStream
        $bmp.Save($mem, [System.Drawing.Imaging.ImageFormat]::Png)
        # The comma keeps the byte[] from being unrolled into the pipeline.
        return , $mem.ToArray()
    }
    $rect = New-Object System.Drawing.Rectangle 0, 0, $w, $w
    $read = $bmp.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    try {
        $pixels = New-Object byte[] ($w * $w * 4)
        [System.Runtime.InteropServices.Marshal]::Copy($read.Scan0, $pixels, 0, $pixels.Length)
    } finally { $bmp.UnlockBits($read) }

    $mem = New-Object System.IO.MemoryStream
    $out = New-Object System.IO.BinaryWriter $mem
    # BITMAPINFOHEADER, with the doubled height an icon's two bitmaps need.
    $out.Write([uint32]40); $out.Write([int32]$w); $out.Write([int32]($w * 2))
    $out.Write([uint16]1); $out.Write([uint16]32); $out.Write([uint32]0)
    $out.Write([uint32]($w * $w * 4)); $out.Write([int32]0); $out.Write([int32]0)
    $out.Write([uint32]0); $out.Write([uint32]0)
    # The colour bitmap, bottom row first.
    for ($y = $w - 1; $y -ge 0; $y--) {
        $out.Write($pixels, $y * $w * 4, $w * 4)
    }
    # The 1bpp mask, rows padded to four bytes. Left at zero: a 32-bit
    # icon's own alpha is what Windows draws through.
    $stride = [int][Math]::Floor(($w + 31) / 32) * 4
    $mask = New-Object byte[] ($stride * $w)
    $out.Write($mask, 0, $mask.Length)
    $out.Flush()
    , $mem.ToArray()
}

function Write-Ico([string]$path, $frames) {
    $file = [System.IO.File]::Create($path)
    $out = New-Object System.IO.BinaryWriter $file
    try {
        $out.Write([uint16]0); $out.Write([uint16]1); $out.Write([uint16]$frames.Count)
        $offset = 6 + 16 * $frames.Count
        foreach ($frame in $frames) {
            $side = if ($frame.Size -eq 256) { 0 } else { $frame.Size }
            $out.Write([byte]$side); $out.Write([byte]$side)
            $out.Write([byte]0); $out.Write([byte]0)
            $out.Write([uint16]1); $out.Write([uint16]32)
            $out.Write([uint32]$frame.Bytes.Length); $out.Write([uint32]$offset)
            $offset += $frame.Bytes.Length
        }
        foreach ($frame in $frames) { $out.Write($frame.Bytes, 0, $frame.Bytes.Length) }
    } finally { $out.Dispose(); $file.Dispose() }
}

$glacier = Get-MarkRamp 'glacier'
$artwork = New-Object System.Drawing.Bitmap $source
try {
    # Glacier keeps the artwork it was drawn in — that icon is also the
    # one Explorer shows for cryo.exe, so it is not generated.
    foreach ($slug in @('light', 'lilac')) {
        $lut = New-Lut $glacier (Get-MarkRamp $slug)
        $recolored = New-Recolored $artwork $lut
        try {
            $frames = foreach ($size in $sizes) {
                $scaled = New-Scaled $recolored $size
                try {
                    [PSCustomObject]@{ Size = $size; Bytes = Get-Frame $scaled }
                } finally { $scaled.Dispose() }
            }
            $ico = Join-Path $root "assets\icon-$slug.ico"
            Write-Ico $ico $frames
            Write-Output "$ico ($($frames.Count) frames)"
            if ($Preview) {
                $shot = New-Scaled $recolored 256
                try {
                    $png = Join-Path $Preview "icon-$slug.png"
                    $shot.Save($png, [System.Drawing.Imaging.ImageFormat]::Png)
                    Write-Output $png
                } finally { $shot.Dispose() }
            }
        } finally { $recolored.Dispose() }
    }
} finally { $artwork.Dispose() }
