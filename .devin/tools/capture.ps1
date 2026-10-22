param([string]$Out = "E:\programming\cryo\shot_now.png", [string]$Proc = "cryo")
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
$src = @"
using System;
using System.Runtime.InteropServices;
public class W32 {
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
    [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
Add-Type -TypeDefinition $src -ReferencedAssemblies System.Drawing, System.Windows.Forms
$p = Get-Process $Proc -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $p) { Write-Output "NO WINDOW for $Proc"; exit 1 }
Write-Output "window: '$($p.MainWindowTitle)' hwnd=$($p.MainWindowHandle)"
$hwnd = $p.MainWindowHandle
[W32]::ShowWindow($hwnd, 9) | Out-Null  # SW_RESTORE
$fg = [W32]::GetForegroundWindow()
$fgPid = 0; $fgTid = [W32]::GetWindowThreadProcessId($fg, [ref]$fgPid)
$myTid = [W32]::GetCurrentThreadId()
$tpid = 0; $tTid = [W32]::GetWindowThreadProcessId($hwnd, [ref]$tpid)
[W32]::AttachThreadInput($myTid, $tTid, $true) | Out-Null
[W32]::AttachThreadInput($myTid, $fgTid, $true) | Out-Null
[W32]::SetForegroundWindow($hwnd) | Out-Null
[W32]::AttachThreadInput($myTid, $tTid, $false) | Out-Null
[W32]::AttachThreadInput($myTid, $fgTid, $false) | Out-Null
Start-Sleep -Milliseconds 500
$r = New-Object W32+RECT
[W32]::GetWindowRect($hwnd, [ref]$r) | Out-Null
$w = $r.Right - $r.Left; $h = $r.Bottom - $r.Top
Write-Output "rect $($r.Left),$($r.Top) ${w}x${h}"
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
$bmp.Save($Out)
$g.Dispose(); $bmp.Dispose()
Write-Output "saved $Out"
