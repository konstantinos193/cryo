param([int]$X, [int]$Y, [string]$Proc = "cryo")
$src = @"
using System;
using System.Runtime.InteropServices;
public class M32 {
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, IntPtr i);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
Add-Type -TypeDefinition $src
$p = Get-Process $Proc | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $p) { Write-Output "NO WINDOW"; exit 1 }
[M32]::ShowWindow($p.MainWindowHandle, 9) | Out-Null
[M32]::SetForegroundWindow($p.MainWindowHandle) | Out-Null
$r = New-Object M32+RECT
[M32]::GetWindowRect($p.MainWindowHandle, [ref]$r) | Out-Null
# X,Y are window-relative
$cx = $r.Left + $X; $cy = $r.Top + $Y
[M32]::SetCursorPos($cx, $cy) | Out-Null
Start-Sleep -Milliseconds 150
[M32]::mouse_event(0x0002, 0, 0, 0, [IntPtr]::Zero)  # LEFTDOWN
Start-Sleep -Milliseconds 60
[M32]::mouse_event(0x0004, 0, 0, 0, [IntPtr]::Zero)  # LEFTUP
Write-Output "clicked at $cx,$cy"
