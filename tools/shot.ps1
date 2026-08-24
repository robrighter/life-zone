# Capture the Life Zone window at 1:1.
#
# Three things this has to get right, all learned the hard way:
#  * per-monitor DPI awareness, or the bitmap comes back at the scaled size and
#    every measurement taken from it is wrong;
#  * no resampling — a downscaled screenshot hides exactly the class of
#    rendering bug worth looking for (M1 had two of them);
#  * PrintWindow with PW_RENDERFULLCONTENT rather than CopyFromScreen, because
#    the latter captures whatever is physically on the glass. An occluded window
#    then reads as a black client area, which is indistinguishable from a
#    webview that failed to paint.
param([string]$Out = "C:\dev\life-zone\shot.png")

Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win {
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@
[void][Win]::SetProcessDpiAwarenessContext([IntPtr](-4))

$p = Get-Process -Name "life-zone" -ErrorAction SilentlyContinue |
     Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $p) { Write-Output "no life-zone window"; exit 1 }
$h = $p.MainWindowHandle
[void][Win]::ShowWindow($h, 9)   # SW_RESTORE
Start-Sleep -Milliseconds 700

$r = New-Object Win+RECT
[void][Win]::GetWindowRect($h, [ref]$r)
$w = $r.R - $r.L; $ht = $r.B - $r.T
$bmp = New-Object System.Drawing.Bitmap $w, $ht
$g = [System.Drawing.Graphics]::FromImage($bmp)
$dc = $g.GetHdc()
# 2 = PW_RENDERFULLCONTENT, which is what makes this work for WebView2.
[void][Win]::PrintWindow($h, $dc, 2)
$g.ReleaseHdc($dc)
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
Write-Output "saved $Out ($w x $ht)"
