<#
.SYNOPSIS
  Captura las ventanas de un proceso de Liviano en un PNG, sin activarlas.

.DESCRIPTION
  Usa PrintWindow con PW_RENDERFULLCONTENT sobre cada ventana visible de
  primer nivel del proceso (la principal, el campo flotante, la barra de
  contraseñas, Ajustes) y las compone en el orden en que están apiladas, así
  la captura muestra lo mismo que la pantalla aunque la ventana esté tapada.
  No manda teclas ni clics y no trae la ventana al frente.

  PrintWindow no ve lo que dibuja DWM (esquinas redondeadas, sombra, borde):
  las esquinas se recortan aquí con -Corner (el radio de Windows 11, 8 px a
  100 %) para que la captura se parezca a la pantalla. -Corner 0 lo apaga.

.EXAMPLE
  tools\capture.ps1 -ProcessId 1234 -Out docs\capturas\claro-horizontal.png
.EXAMPLE
  tools\capture.ps1 -ProcessId 1234 -Out ajustes.png -Class LivianoSettings
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
if (-not ('LivianoCapture' -as [type])) {
    Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Text;

public static class LivianoCapture {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    delegate bool EnumProc(IntPtr hwnd, IntPtr lparam);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc proc, IntPtr lparam);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, StringBuilder name, int max);
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr hwnd, int attr, out RECT rect, int size);

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
        if (shot.Class == "LivianoTooltip") KeyOutBlack(cut);
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

$shots = [LivianoCapture]::Windows([uint32]$ProcessId)
if ($Class) { $shots = @($shots | Where-Object { $_.Class -eq $Class }) }
if (-not $shots -or $shots.Count -eq 0) { throw "El proceso $ProcessId no tiene ventanas visibles" }
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
    $image = [LivianoCapture]::Grab($shot)
    $x = $shot.Frame.Left - $base.Frame.Left
    $y = $shot.Frame.Top - $base.Frame.Top
    $r = $Corner * $dpi
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
