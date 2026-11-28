<#
.SYNOPSIS
  Library for the Nea showcase (docs/showcase). It starts a separate Nea
  with synthetic data and controls it through the bench pipe. It takes
  screenshots and records frames for the video.

.DESCRIPTION
  Dot-source the script:
    . tools\showcase.ps1
    Start-Showcase -Theme light            # synthetic data, release exe
    Invoke-Nea open @{ url = 'https://example.com/' }
    Save-Shot 01-tabs-top-light
    Start-Recording; ...; Stop-Recording   # frames and a timeline for encode_mp4
    Stop-Showcase                          # closes Nea and deletes the temporary files

  The script sends no global input. It sends clicks and mouse moves with
  PostMessage or SendMessage only to the windows of the Nea that it starts.
  All other commands go through the bench pipe (NEA_BENCH=1). The data is
  in a separate temporary folder (NEA_DATA_DIR). The passwords use a
  separate Credential Manager prefix that has no entries.

  Environment variables:
    SHOWCASE_TMP   the scratch folder (use a path without spaces)
    SHOWCASE_EXE   an exe from a different --target-dir
  The script also reads the old names MUESTRA_TMP and MUESTRA_EXE.
#>

$ErrorActionPreference = 'Stop'
$script:Root = Split-Path -Parent $PSScriptRoot
# A separate pipe, so that the script never controls the Nea that you use every day.
$env:NEA_BENCH_PIPE = 'showcase'
. (Join-Path $PSScriptRoot 'bench.ps1')
Add-Type -AssemblyName System.Drawing

if (-not ('NeaShowcase' -as [type])) {
    Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

public static class NeaShowcase {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    delegate bool EnumProc(IntPtr hwnd, IntPtr lparam);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc proc, IntPtr lparam);
    [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr parent, EnumProc proc, IntPtr lparam);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] static extern bool GetLayeredWindowAttributes(IntPtr h, out uint key, out byte alpha, out uint flags);
    [DllImport("user32.dll")] static extern int GetWindowLong(IntPtr h, int index);
    [DllImport("user32.dll")] static extern int GetDlgCtrlID(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetParent(IntPtr h);
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr hwnd, int attr, out RECT rect, int size);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, StringBuilder name, int max);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);

    public class Win { public IntPtr Hwnd; public RECT Frame; public RECT Window; public string Class; }

    public static string ClassOf(IntPtr hwnd) {
        var name = new StringBuilder(256);
        GetClassName(hwnd, name, 256);
        return name.ToString();
    }

    // Visible top-level windows of the process, top of the stack first.
    public static List<Win> Windows(uint pid) {
        var found = new List<Win>();
        EnumWindows((hwnd, _) => {
            uint owner;
            GetWindowThreadProcessId(hwnd, out owner);
            if (owner != pid || !IsWindowVisible(hwnd)) return true;
            RECT window, frame;
            GetWindowRect(hwnd, out window);
            if (DwmGetWindowAttribute(hwnd, 9, out frame, Marshal.SizeOf(typeof(RECT))) != 0) frame = window;
            if (window.Right - window.Left < 2 || window.Bottom - window.Top < 2) return true;
            found.Add(new Win { Hwnd = hwnd, Frame = frame, Window = window, Class = ClassOf(hwnd) });
            return true;
        }, IntPtr.Zero);
        return found;
    }

    public static IntPtr Find(uint pid, string cls) {
        foreach (var w in Windows(pid)) if (w.Class == cls) return w.Hwnd;
        return IntPtr.Zero;
    }

    // The first descendant of a class (the field's edit control).
    public static IntPtr ChildOfClass(IntPtr parent, string cls) {
        IntPtr hit = IntPtr.Zero;
        EnumChildWindows(parent, (h, _) => { if (ClassOf(h) == cls) { hit = h; return false; } return true; }, IntPtr.Zero);
        return hit;
    }

    // A descendant control by its id (the Settings combos, for instance).
    public static IntPtr Control(IntPtr parent, int id) {
        IntPtr hit = IntPtr.Zero;
        EnumChildWindows(parent, (h, _) => { if (GetDlgCtrlID(h) == id) { hit = h; return false; } return true; }, IntPtr.Zero);
        return hit;
    }

    static float Opacity(IntPtr hwnd) {
        const int GWL_EXSTYLE = -20, WS_EX_LAYERED = 0x80000;
        if ((GetWindowLong(hwnd, GWL_EXSTYLE) & WS_EX_LAYERED) == 0) return 1f;
        uint key, flags; byte alpha;
        if (!GetLayeredWindowAttributes(hwnd, out key, out alpha, out flags)) return 1f;
        return (flags & 2) != 0 ? alpha / 255f : 1f;
    }

    static Bitmap Grab(Win w) {
        int width = w.Window.Right - w.Window.Left, height = w.Window.Bottom - w.Window.Top;
        var bitmap = new Bitmap(width, height, PixelFormat.Format32bppArgb);
        using (var g = Graphics.FromImage(bitmap)) {
            IntPtr hdc = g.GetHdc();
            PrintWindow(w.Hwnd, hdc, 2);
            g.ReleaseHdc(hdc);
        }
        if (w.Class == "NeaTooltip" || w.Class == "NeaSpaceCard") KeyOutBlack(bitmap);
        return bitmap;
    }

    // A per-pixel-alpha window comes out of PrintWindow with its clear pixels
    // black: those are keyed out again. LockBits, since this runs per frame.
    static void KeyOutBlack(Bitmap b) {
        var data = b.LockBits(new Rectangle(0, 0, b.Width, b.Height), ImageLockMode.ReadWrite, PixelFormat.Format32bppArgb);
        var px = new byte[data.Stride * b.Height];
        Marshal.Copy(data.Scan0, px, 0, px.Length);
        for (int i = 0; i < px.Length; i += 4)
            if (px[i] < 12 && px[i + 1] < 12 && px[i + 2] < 12) { px[i] = px[i + 1] = px[i + 2] = px[i + 3] = 0; }
        Marshal.Copy(px, 0, data.Scan0, px.Length);
        b.UnlockBits(data);
    }

    // One frame as the screen shows it: the main window (the biggest of the
    // process) and every window of the process above it, each at its
    // opacity, cropped to the main window's frame. Menus (#32768) included.
    public static Bitmap Frame(uint pid, Color background) {
        var wins = Windows(pid);
        if (wins.Count == 0) return null;
        Win main = null;
        foreach (var w in wins)
            if (w.Class == "NeaWindow") main = w;
        if (main == null) return null;
        int fw = main.Frame.Right - main.Frame.Left, fh = main.Frame.Bottom - main.Frame.Top;
        var canvas = new Bitmap(fw, fh, PixelFormat.Format32bppArgb);
        using (var g = Graphics.FromImage(canvas)) {
            g.Clear(background);
            bool started = false;
            for (int i = wins.Count - 1; i >= 0; i--) {
                var w = wins[i];
                if (w.Hwnd == main.Hwnd) started = true;
                if (!started) continue;
                float opacity = Opacity(w.Hwnd);
                if (opacity <= 0f) continue;
                using (var image = Grab(w)) {
                    int x = w.Window.Left - main.Frame.Left, y = w.Window.Top - main.Frame.Top;
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

    // ---------- recorder ----------
    // A thread captures frames as fast as `intervalMs` allows; two more
    // write them as PNG. The clock stops while paused, so the video only
    // holds what was recorded, each stretch at its real speed.
    static Thread capturer;
    static Thread[] savers;
    static BlockingCollection<Tuple<Bitmap, string>> queue;
    static volatile bool running, paused;
    static Stopwatch clock = new Stopwatch();
    static long offset;
    static int count;
    static StreamWriter timeline;
    static readonly object gate = new object();
    public static int Dropped;

    public static long Elapsed { get { return offset + clock.ElapsedMilliseconds; } }
    public static int Count { get { return count; } }

    public static void Start(uint pid, string dir, int intervalMs, Color background) {
        Directory.CreateDirectory(dir);
        bool append = File.Exists(Path.Combine(dir, "timeline.txt"));
        timeline = new StreamWriter(Path.Combine(dir, "timeline.txt"), append);
        if (append) {
            // Continue after what an earlier run of the same video left.
            long last = 0; int n = 0;
            foreach (var line in File.ReadAllLines(Path.Combine(dir, "timeline.txt"))) {
                var parts = line.Split('\t');
                long ms; if (parts.Length == 2 && long.TryParse(parts[0], out ms)) { last = Math.Max(last, ms); n++; }
            }
            offset = last + intervalMs; count = n;
        } else { offset = 0; count = 0; }
        queue = new BlockingCollection<Tuple<Bitmap, string>>(48);
        running = true; paused = false; Dropped = 0;
        clock.Reset(); clock.Start();
        savers = new Thread[3];
        for (int i = 0; i < savers.Length; i++) {
            savers[i] = new Thread(() => {
                foreach (var job in queue.GetConsumingEnumerable()) {
                    job.Item1.Save(job.Item2, ImageFormat.Png);
                    job.Item1.Dispose();
                }
            });
            savers[i].IsBackground = true;
            savers[i].Start();
        }
        capturer = new Thread(() => {
            long next = 0;
            while (running) {
                if (paused) { Thread.Sleep(2); continue; }
                long now = clock.ElapsedMilliseconds;
                if (now < next) { Thread.Sleep(1); continue; }
                next = now + intervalMs;
                long at = offset + now;
                Bitmap frame = null;
                try { frame = Frame(pid, background); } catch { }
                if (frame == null) continue;
                string name;
                lock (gate) {
                    name = string.Format("f{0:D5}.png", count++);
                    timeline.WriteLine(at + "\t" + name);
                }
                if (!queue.TryAdd(Tuple.Create(frame, Path.Combine(dir, name)), 2000)) { Dropped++; frame.Dispose(); }
            }
        });
        capturer.IsBackground = true;
        capturer.Start();
    }

    public static void Pause() { paused = true; Thread.Sleep(5); clock.Stop(); }
    public static void Resume() { clock.Start(); paused = false; }

    public static void Stop() {
        running = false;
        capturer.Join();
        queue.CompleteAdding();
        foreach (var s in savers) s.Join();
        clock.Stop();
        timeline.Flush();
        timeline.Dispose();
    }
}
'@
}

$script:WM_MOUSEMOVE = 0x0200
$script:WM_LBUTTONDOWN = 0x0201
$script:WM_LBUTTONUP = 0x0202
$script:WM_RBUTTONDOWN = 0x0204
$script:WM_RBUTTONUP = 0x0205
$script:WM_KEYDOWN = 0x0100
$script:WM_COMMAND = 0x0111
$script:WM_CANCELMODE = 0x001F
$script:MK_LBUTTON = 1

$script:Scratch = if ($env:SHOWCASE_TMP) { $env:SHOWCASE_TMP } elseif ($env:MUESTRA_TMP) { $env:MUESTRA_TMP } else { Join-Path ([IO.Path]::GetTempPath()) 'nea-showcase' }
# An exe from a different build folder (a different --target-dir).
$script:ShowcaseExe = if ($env:SHOWCASE_EXE) { $env:SHOWCASE_EXE } else { $env:MUESTRA_EXE }
$script:Out = Join-Path $script:Root 'docs\showcase'
$script:StateFile = Join-Path $script:Scratch 'state.json'

function Get-ShowcaseState {
    if (Test-Path $script:StateFile) { Get-Content $script:StateFile -Raw | ConvertFrom-Json } else { $null }
}

function Get-ShowcasePid { (Get-ShowcaseState).pid }

function Get-MainWindow { [NeaShowcase]::Find([uint32](Get-ShowcasePid), 'NeaWindow') }

function Write-JsonFile([string]$Path, $Value) {
    $json = $Value | ConvertTo-Json -Depth 8
    [IO.File]::WriteAllText($Path, $json, (New-Object System.Text.UTF8Encoding($false)))
}

# Synthetic history, bookmarks, downloads and one local, disabled extension.
# The panels and the field show this data. None of the data is real.
function Initialize-ShowcaseData([string]$Data, [string]$Theme, [bool]$Vertical, [hashtable]$Extra) {
    New-Item -ItemType Directory -Force -Path $Data | Out-Null
    $now = [int64]([DateTimeOffset]::UtcNow.ToUnixTimeSeconds())
    $settings = [ordered]@{ theme = $Theme; bench_enabled = $true; check_updates = $false; hover_preview = $true; split_view = $true }
    foreach ($key in $Extra.Keys) { $settings[$key] = $Extra[$key] }
    Write-JsonFile (Join-Path $Data 'settings.json') $settings
    # The window frame (what DWM draws) at 1280x800: the outer rect adds the
    # invisible resize borders (7 px left, right and bottom at 100 %).
    $x = 3600; $y = 120
    Write-JsonFile (Join-Path $Data 'session.json') ([ordered]@{
        tabs = @(); active = 0; vertical_tabs = $Vertical; sidebar_collapsed = $false
        window = @($x, $y, ($x + 1294), ($y + 807)); maximized = $false; splits = @()
    })
    $history = @(
        @('https://en.wikipedia.org/wiki/Buenos_Aires', 'Buenos Aires - Wikipedia', 14),
        @('https://en.wikipedia.org/wiki/Main_Page', 'Wikipedia, the free encyclopedia', 22),
        @('https://en.wikipedia.org/wiki/Patagonia', 'Patagonia - Wikipedia', 6),
        @('https://en.wikipedia.org/wiki/Web_browser', 'Web browser - Wikipedia', 9),
        @('https://en.wikipedia.org/wiki/Windows_11', 'Windows 11 - Wikipedia', 3),
        @('https://en.wikipedia.org/wiki/Rust_(programming_language)', 'Rust (programming language) - Wikipedia', 11),
        @('https://github.com/rust-lang/rust', 'GitHub - rust-lang/rust: Empowering everyone to build reliable and efficient software.', 8),
        @('https://github.com/microsoft/windows-rs', 'GitHub - microsoft/windows-rs: Rust for Windows', 12),
        @('https://github.com/MicrosoftEdge/WebView2Feedback', 'GitHub - MicrosoftEdge/WebView2Feedback', 4),
        @('https://github.com/explore', 'Explore GitHub', 2),
        @('https://www.theguardian.com/international', 'News, sport and opinion from the Guardian''s global edition | The Guardian', 18),
        @('https://www.theguardian.com/business', 'Business | The Guardian', 5),
        @('https://www.theguardian.com/sport', 'Sport | The Guardian', 4),
        @('https://example.com/', 'Example Domain', 7),
        @('https://developer.mozilla.org/en-US/docs/Web/HTML', 'HTML: HyperText Markup Language | MDN', 6),
        @('https://developer.mozilla.org/en-US/docs/Web/CSS', 'CSS: Cascading Style Sheets | MDN', 5),
        @('https://developer.mozilla.org/en-US/docs/Web/JavaScript', 'JavaScript | MDN', 7),
        @('https://doc.rust-lang.org/book/', 'The Rust Programming Language', 9),
        @('https://docs.rs/windows/latest/windows/', 'windows - Rust', 10),
        @('https://learn.microsoft.com/en-us/microsoft-edge/webview2/', 'Introduction to Microsoft Edge WebView2', 6),
        @('https://learn.microsoft.com/en-us/windows/win32/direct2d/direct2d-portal', 'Direct2D - Win32 apps', 4),
        @('https://www.openstreetmap.org/', 'OpenStreetMap', 3),
        @('https://news.ycombinator.com/', 'Hacker News', 9),
        @('https://www.bbc.com/news', 'BBC News - Breaking news, video and the latest top stories', 5),
        @('https://duckduckgo.com/?q=buenos+aires+weather', 'buenos aires weather at DuckDuckGo', 2),
        @('https://www.wikidata.org/wiki/Wikidata:Main_Page', 'Wikidata', 1)
    )
    $i = 0
    Write-JsonFile (Join-Path $Data 'history.json') @($history | ForEach-Object {
        $i++
        [ordered]@{ url = $_[0]; title = $_[1]; visits = $_[2]; last = $now - $i * 3600 * 5 }
    })
    $bookmarks = @(
        @('https://en.wikipedia.org/wiki/Buenos_Aires', 'Buenos Aires - Wikipedia'),
        @('https://github.com/microsoft/windows-rs', 'microsoft/windows-rs'),
        @('https://doc.rust-lang.org/book/', 'The Rust Programming Language'),
        @('https://developer.mozilla.org/en-US/docs/Web/JavaScript', 'JavaScript | MDN'),
        @('https://learn.microsoft.com/en-us/microsoft-edge/webview2/', 'WebView2'),
        @('https://www.theguardian.com/international', 'The Guardian'),
        @('https://www.openstreetmap.org/', 'OpenStreetMap'),
        @('https://news.ycombinator.com/', 'Hacker News')
    )
    $i = 0
    Write-JsonFile (Join-Path $Data 'bookmarks.json') @($bookmarks | ForEach-Object {
        $i++
        [ordered]@{ url = $_[0]; title = $_[1]; added = $now - $i * 86400 }
    })
    $files = Join-Path $Data 'downloads'
    New-Item -ItemType Directory -Force -Path $files | Out-Null
    $downloads = @(
        @('quarterly-report.pdf', 2457600, 'completed'),
        @('vacation-photos.zip', 48234496, 'completed'),
        @('rustup-init.exe', 9175040, 'completed'),
        @('presentation.pptx', 5242880, 'interrupted'),
        @('expenses.xlsx', 61440, 'completed')
    )
    $id = 0
    Write-JsonFile (Join-Path $Data 'downloads.json') @($downloads | ForEach-Object {
        $id++
        $path = Join-Path $files $_[0]
        if ($_[2] -eq 'completed') { [IO.File]::WriteAllBytes($path, [byte[]](1..16)) }
        [ordered]@{ id = $id; url = "https://example.com/files/$($_[0])"; path = $path; state = $_[2]
            received = if ($_[2] -eq 'completed') { $_[1] } else { [int64]($_[1] * 0.4) }; total = $_[1]; started = $now - $id * 7200 }
    })
    # A minimal unpacked extension of its own, disabled: the button shows and
    # the engine starts without extension support.
    $extension = Join-Path $Data 'showcase-extension'
    New-Item -ItemType Directory -Force -Path $extension | Out-Null
    Write-JsonFile (Join-Path $extension 'manifest.json') ([ordered]@{
        manifest_version = 3; name = 'Showcase'; version = '1.0'; action = [ordered]@{ default_popup = 'popup.html' } })
    [IO.File]::WriteAllText((Join-Path $extension 'popup.html'), '<!doctype html><title>Showcase</title><p>Showcase</p>')
    Write-JsonFile (Join-Path $Data 'extensions.json') ([ordered]@{
        extensions = @([ordered]@{ path = $extension; name = 'Showcase'; version = '1.0'
            store_id = $null; engine_id = $null; enabled = $false; pinned = $true; popup = 'popup.html'; options = $null; side_panel = $null })
        removed = @()
    })
}

# Local test pages: a page that opens a pop-up without a click, and an element to hide.
function Initialize-ShowcaseSite([string]$Dir) {
    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    $style = '<style>body{font:15px/1.6 "Segoe UI",sans-serif;max-width:760px;margin:40px auto;padding:0 24px;color:#222}h1{font-weight:500}.promo{position:relative;margin:24px 0;padding:20px 24px;border-radius:12px;background:#fff4d6;border:1px solid #f0d58a}.promo b{font-weight:500}.card{margin:16px 0;padding:16px 20px;border-radius:10px;background:#f3f4f6}</style>'
    $lorem = '<p>Nea is a browser for Windows with a single row of tabs, one field and the page. This is a test page served from the local machine.</p><p>The engine is WebView2; the interface is native, drawn with Direct2D, no HTML.</p>'
    [IO.File]::WriteAllText((Join-Path $Dir 'popup.html'), "<!doctype html><meta charset=utf-8><title>Pop-up window</title>$style<h1>Pop-up window</h1>$lorem<div class=card>This page tries to open a new window without anyone clicking.</div><script>setTimeout(function(){window.open('window.html','_blank','width=400,height=300')},700)</script>")
    [IO.File]::WriteAllText((Join-Path $Dir 'window.html'), "<!doctype html><meta charset=utf-8><title>Advertisement</title><h1>You won!</h1>")
    [IO.File]::WriteAllText((Join-Path $Dir 'hide.html'), "<!doctype html><meta charset=utf-8><title>Hide an element</title>$style<h1>This week's recipes</h1><div class=promo id=promo><b>Subscribe to the newsletter!</b><br>Get the new recipes every Monday. This box is the one that gets hidden.</div>$lorem<div class=card>Monday: lentil soup. Tuesday: vegetable pie. Wednesday: roast chicken with mashed potatoes.</div><script>setInterval(function(){var p=document.getElementById('promo');if(p)p.dispatchEvent(new MouseEvent('mouseover',{bubbles:true}))},150)</script>")
    [IO.File]::WriteAllText((Join-Path $Dir 'index.html'), "<!doctype html><meta charset=utf-8><title>Local page</title>$style<h1>Local page</h1>$lorem")
}

function Start-Showcase {
    # -Settings: more fields for settings.json (for example, block_trackers = $false).
    param([ValidateSet('light', 'dark')][string]$Theme = 'light', [switch]$Vertical, [hashtable]$Settings = @{})
    if (Get-ShowcaseState) { Stop-Showcase }
    Disconnect-Nea
    New-Item -ItemType Directory -Force -Path $script:Scratch | Out-Null
    $data = Join-Path $script:Scratch ('data-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
    $site = Join-Path $script:Scratch 'site'
    $port = Get-Random -Minimum 41000 -Maximum 59000
    Initialize-ShowcaseData $data $Theme $Vertical.IsPresent $Settings
    Initialize-ShowcaseSite $site
    $server = Start-Process -FilePath 'python' -ArgumentList @('-I', '-m', 'http.server', $port, '--bind', '127.0.0.1', '--directory', $site) -WindowStyle Hidden -PassThru
    $env:NEA_DATA_DIR = $data
    $env:NEA_BENCH = '1'
    $env:NEA_BENCH_PIPE = 'showcase'
    $env:NEA_CREDENTIAL_PREFIX = 'NeaShowcaseEmpty:'
    $exe = if ($script:ShowcaseExe) { $script:ShowcaseExe } else { Join-Path $script:Root 'target\release\nea.exe' }
    $app = Start-Process -FilePath $exe -PassThru
    Write-JsonFile $script:StateFile ([ordered]@{ pid = $app.Id; server = $server.Id; port = $port; data = $data; theme = $Theme })
    Connect-Nea -TimeoutSec 30
    Invoke-Nea ping | Out-Null
    $deadline = (Get-Date).AddSeconds(10)
    while (-not (Get-MainWindow) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 100 }
    "Nea $($app.Id) · server 127.0.0.1:$port"
}

function Stop-Showcase {
    $state = Get-ShowcaseState
    if (-not $state) { return }
    try { Connect-Nea -TimeoutSec 3; Invoke-Nea quit -NoThrow | Out-Null } catch { }
    Disconnect-Nea
    foreach ($id in @($state.pid, $state.server)) {
        $p = Get-Process -Id $id -ErrorAction SilentlyContinue
        if ($p) { if (-not $p.WaitForExit(8000)) { Stop-Process -Id $id -Force } }
    }
    # The engine's processes are children of the app: wait for them to go.
    Start-Sleep -Milliseconds 1500
    Remove-Item -Recurse -Force $state.data -ErrorAction SilentlyContinue
    Remove-Item $script:StateFile -Force
}

function Get-SiteUrl([string]$Page) { "http://127.0.0.1:$((Get-ShowcaseState).port)/$Page" }

# The pipe serves one client at a time: every helper reconnects as needed.
function Nea([string]$Cmd, [hashtable]$Arguments = @{}) {
    if (-not $script:NeaConnection) { Connect-Nea }
    Invoke-Nea $Cmd $Arguments
}

function Open-Page([string]$Url, [switch]$Background, [int]$Wait = 20000) {
    $tab = (Nea open @{ url = $Url; background = $Background.IsPresent }).tab
    if ($Wait -gt 0) { try { Nea wait @{ tab = $tab; timeout_ms = $Wait } | Out-Null } catch { Write-Warning $_ } }
    $tab
}

function Run([string]$Name) { Nea run @{ name = $Name } | Out-Null }
function Type-Field([string]$Text, [switch]$Submit) { Nea type @{ text = $Text; submit = $Submit.IsPresent } | Out-Null }
function Get-State { Nea state }
function Set-Hover([int]$X, [int]$Y) { (Nea hover @{ x = $X; y = $Y }).hit }
function Clear-Hover { Set-Hover -1 -1 | Out-Null }

# Where each thing of the strip is: scans the strip through the hover
# command and keeps the middle of each run of the same hit.
function Get-HitMap([int]$Y = 20, [int]$X = 20, [switch]$Column, [int]$Step = 4, [int]$Max = 1280) {
    $runs = [ordered]@{}
    for ($p = 0; $p -lt $Max; $p += $Step) {
        $hit = if ($Column) { Set-Hover $X $p } else { Set-Hover $p $Y }
        if (-not $hit) { continue }
        if (-not $runs.Contains($hit)) { $runs[$hit] = @($p, $p) } else { $runs[$hit][1] = $p }
    }
    Clear-Hover
    $map = [ordered]@{}
    foreach ($k in $runs.Keys) {
        $mid = [int](($runs[$k][0] + $runs[$k][1]) / 2)
        $map[$k] = if ($Column) { @($X, $mid) } else { @($mid, $Y) }
    }
    $map
}

function Send-Mouse([uint32]$Msg, [int]$X, [int]$Y, [int]$Keys = 0) {
    $l = [IntPtr](($Y -shl 16) -bor ($X -band 0xFFFF))
    [NeaShowcase]::PostMessage((Get-MainWindow), $Msg, [IntPtr]$Keys, $l) | Out-Null
}

# A drag in client pixels, as posted messages to our own window.
function Invoke-Drag([int]$X1, [int]$Y1, [int]$X2, [int]$Y2, [int]$Steps = 24, [int]$StepMs = 16) {
    Send-Mouse $script:WM_LBUTTONDOWN $X1 $Y1 $script:MK_LBUTTON
    Start-Sleep -Milliseconds 60
    for ($i = 1; $i -le $Steps; $i++) {
        $x = [int]($X1 + ($X2 - $X1) * $i / $Steps); $y = [int]($Y1 + ($Y2 - $Y1) * $i / $Steps)
        Send-Mouse $script:WM_MOUSEMOVE $x $y $script:MK_LBUTTON
        Start-Sleep -Milliseconds $StepMs
    }
    Send-Mouse $script:WM_LBUTTONUP $X2 $Y2
}

function Open-ContextMenu([int]$X, [int]$Y) {
    Send-Mouse $script:WM_RBUTTONDOWN $X $Y
    Send-Mouse $script:WM_RBUTTONUP $X $Y
}

# Closes an open menu (TrackPopupMenu) of our process: Escape to the menu
# window, posted, the way its own loop reads it.
function Close-Menus {
    $pid_ = [uint32](Get-ShowcasePid)
    foreach ($w in [NeaShowcase]::Windows($pid_)) {
        if ($w.Class -eq '#32768') { [NeaShowcase]::PostMessage($w.Hwnd, $script:WM_KEYDOWN, [IntPtr]0x1B, [IntPtr]1) | Out-Null }
    }
    Start-Sleep -Milliseconds 150
    foreach ($w in [NeaShowcase]::Windows($pid_)) {
        if ($w.Class -eq '#32768') { [NeaShowcase]::PostMessage((Get-MainWindow), $script:WM_CANCELMODE, [IntPtr]0, [IntPtr]0) | Out-Null }
    }
}

# Picks the Settings theme as a person would: the combo's selection, then the
# notification its parent gets.
function Set-ThemeInSettings([ValidateSet('system', 'light', 'dark')][string]$Theme) {
    $settings = [NeaShowcase]::Find([uint32](Get-ShowcasePid), 'NeaSettings')
    if ($settings -eq [IntPtr]::Zero) { throw 'Settings is not open.' }
    $combo = [NeaShowcase]::Control($settings, 101)
    $index = @{ system = 0; light = 1; dark = 2 }[$Theme]
    [NeaShowcase]::SendMessage($combo, 0x014E, [IntPtr]$index, [IntPtr]0) | Out-Null   # CB_SETCURSEL
    $parent = [NeaShowcase]::GetParent($combo)
    $w = [IntPtr]((1 -shl 16) -bor 101)                                                   # CBN_SELCHANGE
    [NeaShowcase]::SendMessage($parent, $script:WM_COMMAND, $w, $combo) | Out-Null
}

function Close-Settings {
    $settings = [NeaShowcase]::Find([uint32](Get-ShowcasePid), 'NeaSettings')
    if ($settings -ne [IntPtr]::Zero) { [NeaShowcase]::PostMessage($settings, 0x0010, [IntPtr]0, [IntPtr]0) | Out-Null }
}

# Escape on the field's own edit control: the field (or panel) closes.
function Close-Field {
    $field = [NeaShowcase]::Find([uint32](Get-ShowcasePid), 'NeaOmnibar')
    if ($field -eq [IntPtr]::Zero) { return }
    $edit = [NeaShowcase]::ChildOfClass($field, 'Edit')
    if ($edit -ne [IntPtr]::Zero) { [NeaShowcase]::SendMessage($edit, $script:WM_KEYDOWN, [IntPtr]0x1B, [IntPtr]1) | Out-Null }
}

# A key on the field's edit control (Down, Enter…), as its own message.
function Send-FieldKey([int]$VirtualKey) {
    $field = [NeaShowcase]::Find([uint32](Get-ShowcasePid), 'NeaOmnibar')
    if ($field -eq [IntPtr]::Zero) { return }
    $edit = [NeaShowcase]::ChildOfClass($field, 'Edit')
    if ($edit -ne [IntPtr]::Zero) { [NeaShowcase]::SendMessage($edit, $script:WM_KEYDOWN, [IntPtr]$VirtualKey, [IntPtr]1) | Out-Null }
}

# Shows a tab: the bench screenshot command selects it first.
function Select-Tab([int64]$Id) {
    $reply = Invoke-Nea screenshot @{ tab = $Id; path = (Join-Path $script:Scratch 'selection.png') } -NoThrow
    # A sleeping tab was selected all the same: it wakes and loads.
    if (-not $reply.ok) { try { Nea wait @{ tab = $Id; timeout_ms = 20000 } | Out-Null } catch { } }
}

# Types like a person: the field gets one more character at a time.
function Write-Slowly([string]$Text, [int]$DelayMs = 90, [switch]$Submit) {
    for ($i = 1; $i -le $Text.Length; $i++) {
        Type-Field $Text.Substring(0, $i)
        Start-Sleep -Milliseconds $DelayMs
    }
    if ($Submit) { Type-Field $Text -Submit }
}

# A part of a saved photo, optionally enlarged (the strip's icons).
function Save-Crop([string]$From, [string]$Name, [int]$X, [int]$Y, [int]$W, [int]$H, [double]$Scale = 1) {
    $src = [System.Drawing.Image]::FromFile($From)
    $dst = New-Object System.Drawing.Bitmap([int]($W * $Scale), [int]($H * $Scale))
    $g = [System.Drawing.Graphics]::FromImage($dst)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.DrawImage($src, (New-Object System.Drawing.Rectangle(0, 0, $dst.Width, $dst.Height)), $X, $Y, $W, $H, [System.Drawing.GraphicsUnit]::Pixel)
    $g.Dispose(); $src.Dispose()
    $path = Join-Path $script:Out "$Name.png"
    $dst.Save($path, [System.Drawing.Imaging.ImageFormat]::Png); $dst.Dispose()
    $path
}

# Two photos side by side, each with a caption above it.
function Save-SideBySide([string]$Left, [string]$LeftLabel, [string]$Right, [string]$RightLabel, [string]$Name, [double]$Scale = 0.75) {
    $a = [System.Drawing.Image]::FromFile($Left); $b = [System.Drawing.Image]::FromFile($Right)
    $w = [int]($a.Width * $Scale); $h = [int]($a.Height * $Scale); $gap = 24; $label = 44
    $dst = New-Object System.Drawing.Bitmap(($w * 2 + $gap * 3), ($h + $label + $gap))
    $g = [System.Drawing.Graphics]::FromImage($dst)
    $g.Clear([System.Drawing.Color]::White)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
    $font = New-Object System.Drawing.Font('Segoe UI', 13)
    $ink = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(23, 23, 23))
    $g.DrawString($LeftLabel, $font, $ink, $gap, 10)
    $g.DrawString($RightLabel, $font, $ink, ($gap * 2 + $w), 10)
    $g.DrawImage($a, $gap, $label, $w, $h)
    $g.DrawImage($b, ($gap * 2 + $w), $label, $w, $h)
    $g.Dispose(); $a.Dispose(); $b.Dispose()
    $path = Join-Path $script:Out "$Name.png"
    $dst.Save($path, [System.Drawing.Imaging.ImageFormat]::Png); $dst.Dispose()
    $path
}

function Save-Shot([string]$Name, [string]$Class = '') {
    New-Item -ItemType Directory -Force -Path $script:Out | Out-Null
    $path = Join-Path $script:Out "$Name.png"
    $args_ = @{ ProcessId = (Get-ShowcasePid); Out = $path }
    if ($Class) { $args_.Class = $Class }
    & (Join-Path $PSScriptRoot 'capture.ps1') @args_ | Out-Null
    $path
}

function Get-FrameBackground {
    if ((Get-ShowcaseState).theme -eq 'dark') { [System.Drawing.Color]::FromArgb(32, 32, 32) } else { [System.Drawing.Color]::FromArgb(245, 245, 245) }
}

function Start-Recording([string]$Dir, [int]$IntervalMs = 33) {
    [NeaShowcase]::Start([uint32](Get-ShowcasePid), $Dir, $IntervalMs, (Get-FrameBackground))
}
function Suspend-Recording { [NeaShowcase]::Pause() }
function Resume-Recording { [NeaShowcase]::Resume() }
function Stop-Recording {
    [NeaShowcase]::Stop()
    "$([NeaShowcase]::Count) frames, $([NeaShowcase]::Elapsed) ms, $([NeaShowcase]::Dropped) dropped"
}
