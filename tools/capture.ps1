<#
.SYNOPSIS
  Captures the windows of a Nea process to a PNG file. The script does not
  activate the windows.

.DESCRIPTION
  The script runs PrintWindow with PW_RENDERFULLCONTENT on each visible
  top-level window of the process. These windows include the main window,
  the floating field, the password prompt and Settings. The script puts
  the windows together in their stacking order. Thus the capture shows the
  same image as the screen, also when a different window covers Nea. The
  script sends no keys or clicks. It does not move the window to the front.

  PrintWindow does not capture what DWM draws (rounded corners, shadow and
  border). The script cuts the corners with -Corner, so that the capture
  looks like the screen. The default is the Windows 11 radius, 8 px at
  100 %. -Corner 0 stops the corner cut.

.EXAMPLE
  tools\capture.ps1 -ProcessId 1234 -Out $env:TEMP\nea-light-top.png
.EXAMPLE
  tools\capture.ps1 -ProcessId 1234 -Out settings.png -Class NeaSettings
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][int]$ProcessId,
    [Parameter(Mandatory)][string]$Out,
    # Only this window class (and nothing composed over it).
    [string]$Class = '',
    # Scale the result down (1 = as captured).
    [double]$Scale = 1.0,
    # Corner radius DWM gives each window, in pixels at 100 %.
    [double]$Corner = 8
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
if (-not ('NeaCapture' -as [type])) {
    Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Text;

public static class NeaCapture {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    delegate bool EnumProc(IntPtr hwnd, IntPtr lparam);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc proc, IntPtr lparam);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, StringBuilder name, int max);
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr hwnd, int attr, out RECT rect, int size);
    [DllImport("user32.dll")] static extern int GetWindowLong(IntPtr h, int index);
    [DllImport("user32.dll")] static extern bool GetLayeredWindowAttributes(IntPtr h, out uint key, out byte alpha, out uint flags);

    [DllImport("user32.dll")] static extern int GetWindowRgn(IntPtr hwnd, IntPtr hrgn);
    [DllImport("gdi32.dll")] static extern IntPtr CreateRectRgn(int left, int top, int right, int bottom);
    [DllImport("gdi32.dll")] static extern bool PtInRegion(IntPtr rgn, int x, int y);
    [DllImport("gdi32.dll")] static extern IntPtr CreateCompatibleDC(IntPtr hdc);
    [DllImport("gdi32.dll")] static extern IntPtr CreateDIBSection(IntPtr hdc, ref DIBHEADER bmi, uint usage, out IntPtr bits, IntPtr section, uint offset);
    [DllImport("gdi32.dll")] static extern IntPtr SelectObject(IntPtr hdc, IntPtr obj);
    [DllImport("gdi32.dll")] static extern bool DeleteObject(IntPtr obj);
    [DllImport("gdi32.dll")] static extern bool DeleteDC(IntPtr hdc);
    [StructLayout(LayoutKind.Sequential)] struct DIBHEADER { public int biSize, biWidth, biHeight; public short biPlanes, biBitCount; public int biCompression, biSizeImage, biXPelsPerMeter, biYPelsPerMeter, biClrUsed, biClrImportant; }

    // A window drawn per pixel (UpdateLayeredWindow): layered, with neither
    // a constant alpha nor a colour key of its own.
    public static bool PerPixel(IntPtr hwnd) {
        const int GWL_EXSTYLE = -20, WS_EX_LAYERED = 0x80000;
        if ((GetWindowLong(hwnd, GWL_EXSTYLE) & WS_EX_LAYERED) == 0) return false;
        uint key, flags; byte alpha;
        return !GetLayeredWindowAttributes(hwnd, out key, out alpha, out flags) || flags == 0;
    }

    // Whether the window is cut by a region (SetWindowRgn).
    public static bool HasRegion(IntPtr hwnd) {
        IntPtr rgn = CreateRectRgn(0, 0, 0, 0);
        int kind = GetWindowRgn(hwnd, rgn);
        DeleteObject(rgn);
        return kind > 1;
    }

    // The window's picture with its alpha: a per-pixel window is asked with
    // WM_PRINT (PrintWindow without PW_RENDERFULLCONTENT), which those that
    // keep their picture answer premultiplied; null when it answers nothing.
    // Any other window comes opaque, cut to its region if it has one.
    public static Bitmap GrabAlpha(IntPtr hwnd, int w, int h) {
        bool perPixel = PerPixel(hwnd);
        var head = new DIBHEADER { biSize = 40, biWidth = w, biHeight = -h, biPlanes = 1, biBitCount = 32 };
        IntPtr bits, dc = CreateCompatibleDC(IntPtr.Zero);
        IntPtr dib = CreateDIBSection(dc, ref head, 0, out bits, IntPtr.Zero, 0);
        IntPtr old = SelectObject(dc, dib);
        PrintWindow(hwnd, dc, perPixel ? 0u : 2u);
        var px = new int[w * h];
        Marshal.Copy(bits, px, 0, px.Length);
        SelectObject(dc, old); DeleteObject(dib); DeleteDC(dc);
        if (perPixel) {
            bool any = false;
            foreach (int p in px) if (p != 0) { any = true; break; }
            if (!any) return null;
        } else {
            for (int i = 0; i < px.Length; i++) px[i] |= unchecked((int)0xFF000000);
            IntPtr rgn = CreateRectRgn(0, 0, 0, 0);
            if (GetWindowRgn(hwnd, rgn) > 1)
                for (int y = 0; y < h; y++)
                    for (int x = 0; x < w; x++)
                        if (!PtInRegion(rgn, x, y)) px[y * w + x] = 0;
            DeleteObject(rgn);
        }
        var bitmap = new Bitmap(w, h, System.Drawing.Imaging.PixelFormat.Format32bppPArgb);
        var data = bitmap.LockBits(new Rectangle(0, 0, w, h), System.Drawing.Imaging.ImageLockMode.WriteOnly, System.Drawing.Imaging.PixelFormat.Format32bppPArgb);
        Marshal.Copy(px, 0, data.Scan0, px.Length);
        bitmap.UnlockBits(data);
        return bitmap;
    }

    public class Shot { public string Class; public RECT Frame; public RECT Window; public IntPtr Hwnd; }

    // Top to bottom, as EnumWindows lists them.
    public static List<Shot> Windows(uint pid) {
        var found = new List<Shot>();
        EnumWindows((hwnd, _) => {
            uint owner;
            GetWindowThreadProcessId(hwnd, out owner);
            if (owner != pid || !IsWindowVisible(hwnd)) return true;
            var name = new StringBuilder(256);
            GetClassName(hwnd, name, 256);
            RECT window, frame;
            GetWindowRect(hwnd, out window);
            if (DwmGetWindowAttribute(hwnd, 9, out frame, Marshal.SizeOf(typeof(RECT))) != 0) frame = window;
            if (window.Right - window.Left < 2 || window.Bottom - window.Top < 2) return true;
            found.Add(new Shot { Class = name.ToString(), Frame = frame, Window = window, Hwnd = hwnd });
            return true;
        }, IntPtr.Zero);
        return found;
    }

    public static Bitmap Grab(Shot shot) {
        int w = shot.Window.Right - shot.Window.Left, h = shot.Window.Bottom - shot.Window.Top;
        // A region or per-pixel alpha is the window's own shape: kept whole.
        if (PerPixel(shot.Hwnd) || HasRegion(shot.Hwnd)) {
            var shaped = GrabAlpha(shot.Hwnd, w, h);
            if (shaped != null) return shaped;
        }
        var bitmap = new Bitmap(w, h, System.Drawing.Imaging.PixelFormat.Format32bppArgb);
        using (var g = Graphics.FromImage(bitmap)) {
            IntPtr hdc = g.GetHdc();
            PrintWindow(shot.Hwnd, hdc, 2);
            g.ReleaseHdc(hdc);
        }
        // Only what DWM shows: the window's frame without its invisible resize border.
        var crop = new Rectangle(shot.Frame.Left - shot.Window.Left, shot.Frame.Top - shot.Window.Top,
            shot.Frame.Right - shot.Frame.Left, shot.Frame.Bottom - shot.Frame.Top);
        var cut = bitmap.Clone(crop, System.Drawing.Imaging.PixelFormat.Format32bppArgb);
        bitmap.Dispose();
        if (shot.Class == "NeaTooltip" || shot.Class == "NeaSpaceCard") KeyOutBlack(cut);
        return cut;
    }

    // A per-pixel-alpha window (the tooltip) comes out of PrintWindow with
    // its clear pixels black: those are keyed out again (its shadow with them).
    public static void KeyOutBlack(Bitmap b) {
        for (int y = 0; y < b.Height; y++)
            for (int x = 0; x < b.Width; x++) {
                var c = b.GetPixel(x, y);
                if (c.R < 12 && c.G < 12 && c.B < 12) b.SetPixel(x, y, Color.Transparent);
            }
    }

}
'@
}

$shots = [NeaCapture]::Windows([uint32]$ProcessId)
if ($Class) { $shots = @($shots | Where-Object { $_.Class -eq $Class }) }
if (-not $shots -or $shots.Count -eq 0) { throw "Process $ProcessId has no visible windows." }
# The base is the biggest window (the browser, or the one asked for).
$base = $shots | Sort-Object { ($_.Frame.Right - $_.Frame.Left) * ($_.Frame.Bottom - $_.Frame.Top) } -Descending | Select-Object -First 1
$width = $base.Frame.Right - $base.Frame.Left
$height = $base.Frame.Bottom - $base.Frame.Top
$canvas = New-Object System.Drawing.Bitmap($width, $height)
$g = [System.Drawing.Graphics]::FromImage($canvas)
$g.Clear([System.Drawing.Color]::Transparent)
# Bottom first: EnumWindows lists the top of the stack first.
$ordered = @($shots)
[array]::Reverse($ordered)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$dpi = $g.DpiX / 96.0
$started = $false
foreach ($shot in $ordered) {
    if ($shot.Hwnd -eq $base.Hwnd) { $started = $true }
    if (-not $started) { continue }
    $image = [NeaCapture]::Grab($shot)
    $x = $shot.Frame.Left - $base.Frame.Left
    $y = $shot.Frame.Top - $base.Frame.Top
    $r = $Corner * $dpi
    # A window that shapes itself (a region, per-pixel alpha) gets no DWM corner.
    if ([NeaCapture]::PerPixel($shot.Hwnd) -or [NeaCapture]::HasRegion($shot.Hwnd)) { $r = 0 }
    if ($r -gt 0) {
        $path = New-Object System.Drawing.Drawing2D.GraphicsPath
        $d = 2 * $r
        $path.AddArc($x, $y, $d, $d, 180, 90)
        $path.AddArc($x + $image.Width - $d, $y, $d, $d, 270, 90)
        $path.AddArc($x + $image.Width - $d, $y + $image.Height - $d, $d, $d, 0, 90)
        $path.AddArc($x, $y + $image.Height - $d, $d, $d, 90, 90)
        $path.CloseFigure()
        $brush = New-Object System.Drawing.TextureBrush($image)
        $brush.TranslateTransform($x, $y)
        $g.FillPath($brush, $path)
        $brush.Dispose()
        $path.Dispose()
    } else {
        $g.DrawImage($image, $x, $y, $image.Width, $image.Height)
    }
    $image.Dispose()
}
$g.Dispose()
if ($Scale -ne 1.0) {
    $small = New-Object System.Drawing.Bitmap($canvas, [int]($width * $Scale), [int]($height * $Scale))
    $canvas.Dispose()
    $canvas = $small
}
$dir = Split-Path -Parent ([IO.Path]::GetFullPath($Out))
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir | Out-Null }
$canvas.Save([IO.Path]::GetFullPath($Out), [System.Drawing.Imaging.ImageFormat]::Png)
$canvas.Dispose()
Write-Output ([IO.Path]::GetFullPath($Out))
