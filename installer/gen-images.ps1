# Generates the Inno wizard art from assets/icon.png.
# Run from the repo root:  powershell -File installer\gen-images.ps1
# Outputs BMPs into installer/images\ — commit the results; the .iss
# lists every DPI variant comma-separated and Inno picks by scaling.

Add-Type -AssemblyName System.Drawing

$root   = Split-Path $PSScriptRoot -Parent
$icon   = [Drawing.Image]::FromFile("$root\assets\icon.png")
$outDir = "$PSScriptRoot\images"
New-Item -ItemType Directory -Force $outDir | Out-Null

# Brand palette lifted from assets/icon.svg.
$navyTop = [Drawing.Color]::FromArgb(12, 36, 51)    # #0C2433
$navyBot = [Drawing.Color]::FromArgb(23, 69, 90)    # #17455A
$ice     = [Drawing.Color]::FromArgb(234, 249, 253) # #EAF9FD
$muted   = [Drawing.Color]::FromArgb(127, 182, 201) # #7FB6C9

function New-WizardBanner([int]$w, [int]$h, [string]$path) {
    $bmp = New-Object Drawing.Bitmap $w, $h
    $g = [Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode      = 'HighQuality'
    $g.InterpolationMode  = 'HighQualityBicubic'
    $g.TextRenderingHint  = 'AntiAlias'

    $rect  = New-Object Drawing.Rectangle 0, 0, $w, $h
    $brush = New-Object Drawing.Drawing2D.LinearGradientBrush `
        $rect, $navyTop, $navyBot, 90
    $g.FillRectangle($brush, $rect)

    # Icon centered horizontally in the upper half.
    $s = [int]($w * 0.62)
    $g.DrawImage($icon, [int](($w - $s) / 2), [int]($h * 0.14), $s, $s)

    $sf = New-Object Drawing.StringFormat
    $sf.Alignment = 'Center'

    $wordmark = New-Object Drawing.Font 'Segoe UI', ($w * 0.21),
        ([Drawing.FontStyle]::Bold), ([Drawing.GraphicsUnit]::Pixel)
    $g.DrawString('cryo', $wordmark,
        (New-Object Drawing.SolidBrush $ice),
        (New-Object Drawing.RectangleF 0, ($h * 0.55), $w, ($w * 0.3)), $sf)

    $tagline = New-Object Drawing.Font 'Segoe UI', ($w * 0.058),
        ([Drawing.GraphicsUnit]::Pixel)
    $g.DrawString('tabs freeze. budgets hold.', $tagline,
        (New-Object Drawing.SolidBrush $muted),
        (New-Object Drawing.RectangleF 0, ($h * 0.72), $w, ($w * 0.2)), $sf)

    $bmp.Save($path, [Drawing.Imaging.ImageFormat]::Bmp)
    $g.Dispose(); $bmp.Dispose()
}

function New-CornerIcon([int]$s, [string]$path) {
    $bmp = New-Object Drawing.Bitmap $s, $s
    $g = [Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode     = 'HighQuality'
    $g.InterpolationMode = 'HighQualityBicubic'
    # The modern header strip is light — white field, icon fills it.
    $g.Clear([Drawing.Color]::White)
    $pad = [int]($s * 0.05)
    $g.DrawImage($icon, $pad, $pad, $s - 2 * $pad, $s - 2 * $pad)
    $bmp.Save($path, [Drawing.Imaging.ImageFormat]::Bmp)
    $g.Dispose(); $bmp.Dispose()
}

# 100% / 150% / 200% DPI variants.
New-WizardBanner 164 314 "$outDir\wizard.bmp"
New-WizardBanner 246 471 "$outDir\wizard@150.bmp"
New-WizardBanner 328 628 "$outDir\wizard@200.bmp"
New-CornerIcon   55 "$outDir\wizard-small.bmp"
New-CornerIcon   83 "$outDir\wizard-small@150.bmp"
New-CornerIcon  110 "$outDir\wizard-small@200.bmp"

$icon.Dispose()
Write-Output "Wrote 6 images to $outDir"
