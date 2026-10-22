param([string]$Out = "E:\programming\cryo\shot_icon.png", [string]$Proc = "cryo", [int]$CropH = 0)
Add-Type -AssemblyName System.Drawing
$src = @"
using System;
using System.Runtime.InteropServices;
public class W32 {
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
Add-Type -TypeDefinition $src -ReferencedAssemblies System.Drawing
$p = Get-Process $Proc | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $p) { Write-Output "NO WINDOW"; exit 1 }
$r = New-Object W32+RECT
[W32]::GetWindowRect($p.MainWindowHandle, [ref]$r) | Out-Null
$w = $r.Right - $r.Left; $h = $r.Bottom - $r.Top
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$dc = $g.GetHdc()
$ok = [W32]::PrintWindow($p.MainWindowHandle, $dc, 2)  # PW_RENDERFULLCONTENT
$g.ReleaseHdc($dc); $g.Dispose()
Write-Output "printwindow=$ok ${w}x${h}"
if ($CropH -gt 0 -and $h -gt $CropH) {
    $c = New-Object System.Drawing.Bitmap $w, $CropH
    $g2 = [System.Drawing.Graphics]::FromImage($c)
    $g2.DrawImage($bmp, 0, 0, (New-Object System.Drawing.Rectangle 0,0,$w,$CropH), [System.Drawing.GraphicsUnit]::Pixel)
    $g2.Dispose(); $bmp.Dispose(); $bmp = $c
}
$bmp.Save($Out); $bmp.Dispose()
Write-Output "saved $Out"
