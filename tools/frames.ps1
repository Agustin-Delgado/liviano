<#
.SYNOPSIS
  Graba los cuadros intermedios de una animación de Liviano en una tira PNG.

.DESCRIPTION
  Corre -Trigger (lo que arranca la animación: un comando del pipe de bench,
  un PostMessage a una ventana propia) y enseguida captura las ventanas del
  proceso con PrintWindow cada -IntervalMs, -Frames veces. Cada ventana en
  capas se compone con la opacidad que tiene en ese momento
  (GetLayeredWindowAttributes), así un fundido se ve como en pantalla.
  Los cuadros se recortan a -Crop (x,y,ancho,alto relativos a la ventana
  principal) y se pegan de izquierda a derecha, con el tiempo de cada uno
  debajo. No manda teclas ni clics y no trae nada al frente.

  Si -Trigger usa Invoke-Liviano, quien llama tiene que soltar antes su
  conexión al pipe (Disconnect-Liviano): el pipe atiende a uno por vez, y
  este script suelta la suya al terminar.

.EXAMPLE
  tools\frames.ps1 -ProcessId 1234 -Out docs\capturas\tira-campo.png -Crop 0,0,900,320 -Trigger { Invoke-Liviano run @{ name = 'focus_address' } }
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][int]$ProcessId,
    [Parameter(Mandatory)][string]$Out,
    [Parameter(Mandatory)][scriptblock]$Trigger,
    [int]$Frames = 8,
    [int]$IntervalMs = 25,
    [int[]]$Crop = @(),
    [double]$Scale = 1.0,
    # Frames per row (0: all in one row); a wide crop reads better stacked.
    [int]$Columns = 0
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
if (-not ('LivianoFrames' -as [type])) {
    Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;

public static class LivianoFrames {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    delegate bool EnumProc(IntPtr hwnd, IntPtr lparam);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc proc, IntPtr lparam);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] static extern bool GetLayeredWindowAttributes(IntPtr h, out uint key, out byte alpha, out uint flags);
    [DllImport("user32.dll")] static extern int GetWindowLong(IntPtr h, int index);
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr hwnd, int attr, out RECT rect, int size);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, System.Text.StringBuilder name, int max);
    [DllImport("user32.dll")] static extern IntPtr GetWindow(IntPtr hwnd, uint cmd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindowEx(IntPtr parent, IntPtr after, string cls, string title);

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

    // A per-pixel window's own opacity can't be read from outside; the
    // field's backdrop carries it in its pixels (coming and going it draws
    // the whole field, faded, while the field itself stands at 0), and
    // answers WM_PRINT with the frame on screen.
    static float PerPixelOpacity(IntPtr hwnd) { return 1f; }

    class Win { public IntPtr Hwnd; public RECT Frame; public RECT Window; }

    static List<Win> Windows(uint pid) {
        var found = new List<Win>();
        EnumWindows((hwnd, _) => {
            uint owner;
            GetWindowThreadProcessId(hwnd, out owner);
            if (owner != pid || !IsWindowVisible(hwnd)) return true;
            RECT window, frame;
            GetWindowRect(hwnd, out window);
            if (DwmGetWindowAttribute(hwnd, 9, out frame, Marshal.SizeOf(typeof(RECT))) != 0) frame = window;
            if (window.Right - window.Left < 2 || window.Bottom - window.Top < 2) return true;
            found.Add(new Win { Hwnd = hwnd, Frame = frame, Window = window });
            return true;
        }, IntPtr.Zero);
        return found;
    }

    // 1 for an opaque window; the constant alpha of a layered one.
    static float Opacity(IntPtr hwnd) {
        const int GWL_EXSTYLE = -20, WS_EX_LAYERED = 0x80000;
        if ((GetWindowLong(hwnd, GWL_EXSTYLE) & WS_EX_LAYERED) == 0) return 1f;
        uint key, flags; byte alpha;
        if (!GetLayeredWindowAttributes(hwnd, out key, out alpha, out flags)) return PerPixelOpacity(hwnd);
        return (flags & 2) != 0 ? alpha / 255f : 1f;
    }

    static Bitmap Grab(Win w) {
        int width = w.Window.Right - w.Window.Left, height = w.Window.Bottom - w.Window.Top;
        // A region or per-pixel alpha is the window's own shape: kept whole.
        if (PerPixel(w.Hwnd) || HasRegion(w.Hwnd)) {
            var shaped = GrabAlpha(w.Hwnd, width, height);
            if (shaped != null) return shaped;
        }
        var bitmap = new Bitmap(width, height, PixelFormat.Format32bppArgb);
        using (var g = Graphics.FromImage(bitmap)) {
            IntPtr hdc = g.GetHdc();
            PrintWindow(w.Hwnd, hdc, 2);
            g.ReleaseHdc(hdc);
        }
        var name = new System.Text.StringBuilder(256);
        GetClassName(w.Hwnd, name, 256);
        if (name.ToString() == "LivianoTooltip") KeyOutBlack(bitmap);
        return bitmap;
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


    // One frame: every window of the process, bottom to top, each at its
    // opacity, cropped to `crop` (relative to the biggest window's frame).
    public static Bitmap Frame(uint pid, Rectangle crop) {
        var wins = Windows(pid);
        if (wins.Count == 0) return null;
        Win main = wins[0];
        foreach (var w in wins)
            if ((w.Frame.Right - w.Frame.Left) * (w.Frame.Bottom - w.Frame.Top) > (main.Frame.Right - main.Frame.Left) * (main.Frame.Bottom - main.Frame.Top)) main = w;
        if (crop.Width == 0) crop = new Rectangle(0, 0, main.Frame.Right - main.Frame.Left, main.Frame.Bottom - main.Frame.Top);
        var canvas = new Bitmap(crop.Width, crop.Height, PixelFormat.Format32bppArgb);
        using (var g = Graphics.FromImage(canvas)) {
            g.Clear(Color.Transparent);
            bool started = false;
            for (int i = wins.Count - 1; i >= 0; i--) {
                var w = wins[i];
                if (w.Hwnd == main.Hwnd) started = true;
                if (!started) continue;
                float opacity = Opacity(w.Hwnd);
                if (opacity <= 0f) continue;
                using (var image = Grab(w)) {
                    int x = w.Window.Left - main.Frame.Left - crop.X, y = w.Window.Top - main.Frame.Top - crop.Y;
                    var matrix = new ColorMatrix { Matrix33 = opacity };
                    using (var attrs = new ImageAttributes()) {
                        attrs.SetColorMatrix(matrix);
                        g.DrawImage(image, new Rectangle(x, y, image.Width, image.Height), 0, 0, image.Width, image.Height, GraphicsUnit.Pixel, attrs);
                    }
                }
            }
        }
        return canvas;
    }
}
'@
}

$box = if ($Crop.Count -eq 4) { New-Object System.Drawing.Rectangle($Crop[0], $Crop[1], $Crop[2], $Crop[3]) } else { [System.Drawing.Rectangle]::Empty }
$shots = New-Object System.Collections.Generic.List[object]
$clock = [Diagnostics.Stopwatch]::StartNew()
& $Trigger | Out-Null
$clock.Restart()
for ($i = 0; $i -lt $Frames; $i++) {
    $due = $i * $IntervalMs
    while ($clock.ElapsedMilliseconds -lt $due) { Start-Sleep -Milliseconds 1 }
    $at = $clock.ElapsedMilliseconds
    $frame = [LivianoFrames]::Frame([uint32]$ProcessId, $box)
    if ($frame) { $shots.Add([pscustomobject]@{ At = $at; Image = $frame }) }
}
# A trigger that spoke over the bench pipe (toolsench.ps1) opened its own
# connection in this script's scope: let go of it, the caller needs the pipe.
if (Get-Command Disconnect-Liviano -ErrorAction SilentlyContinue) { Disconnect-Liviano }
if ($shots.Count -eq 0) { throw "El proceso $ProcessId no tiene ventanas visibles" }

$w = [int]($shots[0].Image.Width * $Scale); $h = [int]($shots[0].Image.Height * $Scale)
$gap = 8; $label = 22
$cols = if ($Columns -gt 0) { [math]::Min($Columns, $shots.Count) } else { $shots.Count }
$rows = [math]::Ceiling($shots.Count / $cols)
$stripW = ($w + $gap) * $cols - $gap
$stripH = ($h + $label) * $rows
$strip = New-Object System.Drawing.Bitmap($stripW, $stripH)
$g = [System.Drawing.Graphics]::FromImage($strip)
$g.Clear([System.Drawing.Color]::White)
$g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
$g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
$font = New-Object System.Drawing.Font('Segoe UI', 9)
$ink = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(105, 105, 105))
for ($i = 0; $i -lt $shots.Count; $i++) {
    $x = ($i % $cols) * ($w + $gap)
    $y = [math]::Floor($i / $cols) * ($h + $label)
    $g.DrawImage($shots[$i].Image, $x, $y, $w, $h)
    $g.DrawString("$($shots[$i].At) ms", $font, $ink, $x + 2, $y + $h + 3)
    $shots[$i].Image.Dispose()
}
$g.Dispose()
$dir = Split-Path -Parent ([IO.Path]::GetFullPath($Out))
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir | Out-Null }
$strip.Save([IO.Path]::GetFullPath($Out), [System.Drawing.Imaging.ImageFormat]::Png)
$strip.Dispose()
Write-Output ([IO.Path]::GetFullPath($Out))
