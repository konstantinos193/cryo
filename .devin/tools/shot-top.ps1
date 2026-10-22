param([string]$Out = "E:\programming\cryo\shot_icon.png", [string]$Proc = "cryo", [int]$CropH = 0)
Add-Type -AssemblyName System.Drawing
$src = @"
using System;
using System.Runtime.InteropServices;
public class W32 {
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
Add-Type -TypeDefinition $src -ReferencedAssemblies System.Drawing
$p = Get-Process $Proc | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $p) { Write-Output "NO WINDOW"; exit 1 }
$h = $p.MainWindowHandle
$HWND_TOPMOST = [IntPtr]::Subtract([IntPtr]::Zero, 1)
$HWND_NOTOPMOST = [IntPtr]::Subtract([IntPtr]::Zero, 2)
[W32]::SetWindowPos($h, $HWND_TOPMOST, 0,0,0,0, 0x0002 -bor 0x0001) | Out-Null
Start-Sleep -Milliseconds 400
$r = New-Object W32+RECT
[W32]::GetWindowRect($h, [ref]$r) | Out-Null
$w = $r.Right - $r.Left; $hh = $r.Bottom - $r.Top
if ($CropH -gt 0 -and $hh -gt $CropH) { $hh = $CropH }
$bmp = New-Object System.Drawing.Bitmap $w, $hh
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
$bmp.Save($Out)
$g.Dispose(); $bmp.Dispose()
[W32]::SetWindowPos($h, $HWND_NOTOPMOST, 0,0,0,0, 0x0002 -bor 0x0001) | Out-Null
Write-Output "saved $Out ${w}x${hh}"
