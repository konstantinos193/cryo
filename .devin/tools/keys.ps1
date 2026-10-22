param(
    [Parameter(Mandatory=$true)][string]$Keys,
    [string]$Proc = "cryo",
    [int]$DelayMs = 400
)
# Send keystrokes to the running cryo window. $Keys uses SendKeys syntax:
# ^ ctrl, + shift, % alt. Example: keys.ps1 -Keys "^t" sends Ctrl+T.
Add-Type -AssemblyName System.Windows.Forms
$src = @"
using System;
using System.Runtime.InteropServices;
public class W32K {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
    [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
}
"@
Add-Type -TypeDefinition $src -ReferencedAssemblies System.Windows.Forms
$p = Get-Process $Proc -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $p) { Write-Output "NO WINDOW for $Proc"; exit 1 }
$hwnd = $p.MainWindowHandle
$fg = [W32K]::GetForegroundWindow()
$fgPid = 0; $fgTid = [W32K]::GetWindowThreadProcessId($fg, [ref]$fgPid)
$myTid = [W32K]::GetCurrentThreadId()
$tpid = 0; $tTid = [W32K]::GetWindowThreadProcessId($hwnd, [ref]$tpid)
[W32K]::AttachThreadInput($myTid, $tTid, $true) | Out-Null
[W32K]::AttachThreadInput($myTid, $fgTid, $true) | Out-Null
[W32K]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 200
[System.Windows.Forms.SendKeys]::SendWait($Keys)
Start-Sleep -Milliseconds $DelayMs
Write-Output "sent '$Keys' to '$($p.MainWindowTitle)'"
