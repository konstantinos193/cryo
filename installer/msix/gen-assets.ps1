# Generates the MSIX tile art from assets/icon.png.
# Run from the repo root:  powershell -File installer\msix\gen-assets.ps1
# Outputs PNGs into installer\msix\assets\ — commit the results; the
# manifest lists each by name and pack.ps1 copies the folder whole.

Add-Type -AssemblyName System.Drawing

$root   = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$icon   = [Drawing.Image]::FromFile("$root\assets\icon.png")
$outDir = "$PSScriptRoot\assets"
New-Item -ItemType Directory -Force $outDir | Out-Null

# Brand palette lifted from assets/icon.svg — the same set the Inno
# wizard art uses.
$navyTop = [Drawing.Color]::FromArgb(12, 36, 51)    # #0C2433
$navyBot = [Drawing.Color]::FromArgb(23, 69, 90)    # #17455A
$ice     = [Drawing.Color]::FromArgb(234, 249, 253) # #EAF9FD
$muted   = [Drawing.Color]::FromArgb(127, 182, 201) # #7FB6C9

function New-Canvas([int]$w, [int]$h, [bool]$branded) {
    $bmp = New-Object Drawing.Bitmap $w, $h
    $g = [Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode     = 'HighQuality'
    $g.InterpolationMode = 'HighQualityBicubic'
    $g.TextRenderingHint = 'AntiAlias'
    if ($branded) {
        $rect  = New-Object Drawing.Rectangle 0, 0, $w, $h
        $brush = New-Object Drawing.Drawing2D.LinearGradientBrush `
            $rect, $navyTop, $navyBot, 90
        $g.FillRectangle($brush, $rect)
    } else {
        $g.Clear([Drawing.Color]::Transparent)
    }
    return @($bmp, $g)
}

# Square tiles stay unplated — transparent field, icon at 80% so the
# Store's own plating doesn't crowd the edges.
function New-SquareTile([int]$size, [string]$name) {
    $bmp, $g = New-Canvas $size $size $false
    $s = [int]($size * 0.8)
    $g.DrawImage($icon, [int](($size - $s) / 2), [int](($size - $s) / 2), $s, $s)
    $bmp.Save("$outDir\$name", [Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
}

# Wide tile and splash wear the brand: gradient field, icon and the
# wordmark centered — the same composition as the Inno wizard banner.
function New-WideTile([int]$w, [int]$h, [string]$name) {
    $bmp, $g = New-Canvas $w $h $true
    $s = [int]($h * 0.62)
    $g.DrawImage($icon, [int](($w - $s) / 2), [int]($h * 0.05), $s, $s)
    $sf = New-Object Drawing.StringFormat
    $sf.Alignment = 'Center'
    $wordmark = New-Object Drawing.Font 'Segoe UI', ($h * 0.16),
        ([Drawing.FontStyle]::Bold), ([Drawing.GraphicsUnit]::Pixel)
    $g.DrawString('cryo', $wordmark,
        (New-Object Drawing.SolidBrush $ice),
        (New-Object Drawing.RectangleF 0, ($h * 0.72), $w, ($h * 0.24)), $sf)
    $bmp.Save("$outDir\$name", [Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
}

function New-Splash([int]$w, [int]$h, [string]$name) {
    $bmp, $g = New-Canvas $w $h $true
    $s = [int]($h * 0.5)
    $g.DrawImage($icon, [int](($w - $s) / 2), [int](($h - $s) / 2), $s, $s)
    $bmp.Save("$outDir\$name", [Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
}

New-SquareTile 50  'StoreLogo.png'
New-SquareTile 44  'Square44x44Logo.png'
New-SquareTile 150 'Square150x150Logo.png'
New-WideTile   310 150 'Wide310x150Logo.png'
New-Splash     620 300 'SplashScreen.png'

$icon.Dispose()
Write-Output "MSIX assets written to $outDir"
