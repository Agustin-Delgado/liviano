<#
.SYNOPSIS
  Measures the total memory of Nea with 1, 10 and 40 awake tabs: at rest,
  after a long idle time, minimized and after tab churn.

.DESCRIPTION
  Each scenario starts a new Nea with a temporary NEA_DATA_DIR, script
  control on a separate pipe and a saved session of N tabs. The pages come
  from a small server that this script starts on the loopback interface
  only (127.0.0.1 and ::1). Each tab has its own host (s1.localhost,
  s2.localhost...), so each tab gets its own renderer, as different sites
  do. A page has a large DOM, four decoded images, about 10 MB of script
  objects, a canvas animation and a timer. With -Heavy, some tabs show real
  pages instead, if they are reachable.

  The script selects each tab one time (the `next_tab` command), so all the
  tabs are awake. Then it measures these phases:

    idle       -SettleSec after the last tab was selected
    idle-Nmin  after -IdleMin more minutes with no input
    minimized  after -MinimizedSec with the window minimized (ShowWindow,
               no activation, no input)
    churn      after -ChurnRounds rounds: open 10 tabs, select them, close
               them, and select the first 10 tabs of the session again

  A measurement is the median (by total private working set) of -Samples
  samples. It adds the private working set and the commit (private bytes)
  of nea.exe and of each process in its process tree only, grouped by the
  type of WebView2 process (browser, gpu, utility, renderer, crashpad).
  Other Nea or Edge processes on the computer are not counted and are
  never stopped.

  The script sends no keys or clicks to the system and takes no focus.

.EXAMPLE
  tools\memory.ps1 -Exe C:\Temp\nearam\release\nea.exe -Out C:\Temp\mem-before.json
.EXAMPLE
  tools\memory.ps1 -Tabs 10 -IdleMin 1 -EngineArgs '--disable-features=SpareRendererForSitePerProcess'
#>
[CmdletBinding()]
param(
    [string]$Exe = '',
    [int[]]$Tabs = @(1, 10, 40),
    [int]$SettleSec = 30,
    [double]$IdleMin = 5,
    [int]$MinimizedSec = 60,
    [int]$ChurnRounds = 3,
    [int]$Samples = 3,
    [switch]$Heavy,
    [string[]]$HeavyUrls = @(
        'https://en.wikipedia.org/wiki/Web_browser',
        'https://github.com/rust-lang/rust',
        'https://developer.mozilla.org/en-US/docs/Web/JavaScript',
        'https://www.infobae.com/'
    ),
    # Extra engine switches (WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS), to try a flag before building it in.
    [string]$EngineArgs = '',
    # With a value, the script measures each 5 s for this long after the look
    # at each tab, and then stops: how the memory changes after the suspends.
    [int]$SeriesSec = 0,
    # Settings merged into settings.json, to try a policy without a build.
    [hashtable]$Settings = @{},
    [string]$Out = ''
)

$ErrorActionPreference = 'Stop'
$Tabs = @($Tabs | ForEach-Object { "$_" -split ',' } | Where-Object { $_ } | ForEach-Object { [int]$_ })
. (Join-Path $PSScriptRoot 'bench.ps1')
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Exe) { $Exe = 'C:\Temp\nearam\release\nea.exe' }
$Exe = (Resolve-Path $Exe).Path

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class NeaMemoryProbe {
    [StructLayout(LayoutKind.Sequential)]
    struct Counters {
        public uint cb; public uint PageFaultCount;
        public UIntPtr PeakWorkingSetSize, WorkingSetSize, QuotaPeakPagedPoolUsage, QuotaPagedPoolUsage,
            QuotaPeakNonPagedPoolUsage, QuotaNonPagedPoolUsage, PagefileUsage, PeakPagefileUsage,
            PrivateUsage, PrivateWorkingSetSize;
        public ulong SharedCommitUsage;
    }
    [DllImport("kernel32.dll")] static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    [DllImport("psapi.dll")] static extern bool GetProcessMemoryInfo(IntPtr process, ref Counters counters, uint cb);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hwnd, int command);
    // { private working set, commit } in bytes; zeros when the process is gone.
    public static ulong[] Read(uint pid) {
        IntPtr h = OpenProcess(0x1000, false, pid);
        if (h == IntPtr.Zero) return new ulong[] { 0, 0 };
        var c = new Counters();
        c.cb = (uint)Marshal.SizeOf(typeof(Counters));
        bool ok = GetProcessMemoryInfo(h, ref c, c.cb);
        CloseHandle(h);
        return ok ? new ulong[] { (ulong)c.PrivateWorkingSetSize, (ulong)c.PrivateUsage } : new ulong[] { 0, 0 };
    }
}
'@

# ---------- local pages ----------

$server = {
    param([int]$port)
    # A 512 x 512 24-bit BMP: one decoded image of 1 MB in the page.
    $w = 512; $h = 512; $row = $w * 3; $size = 54 + $row * $h
    $bmp = New-Object byte[] $size
    [BitConverter]::GetBytes([uint16]0x4D42).CopyTo($bmp, 0)
    [BitConverter]::GetBytes([uint32]$size).CopyTo($bmp, 2)
    [BitConverter]::GetBytes([uint32]54).CopyTo($bmp, 10)
    [BitConverter]::GetBytes([uint32]40).CopyTo($bmp, 14)
    [BitConverter]::GetBytes([int32]$w).CopyTo($bmp, 18)
    [BitConverter]::GetBytes([int32]$h).CopyTo($bmp, 22)
    [BitConverter]::GetBytes([uint16]1).CopyTo($bmp, 26)
    [BitConverter]::GetBytes([uint16]24).CopyTo($bmp, 28)
    for ($y = 0; $y -lt $h; $y++) {
        for ($x = 0; $x -lt $w; $x++) {
            $o = 54 + $y * $row + $x * 3
            $bmp[$o] = [byte](($x * 7 + $y) % 256); $bmp[$o + 1] = [byte](($y * 3) % 256); $bmp[$o + 2] = [byte](($x * $y) % 256)
        }
    }
    $items = (1..1500 | ForEach-Object { "<li>Row $_ <a href='#r$_'>link</a> <span>some text that a real page would have</span></li>" }) -join ''
    $listeners = @(
        [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $port),
        [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::IPv6Loopback, $port)
    )
    foreach ($l in $listeners) { try { $l.Start() } catch { } }
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    while ($true) {
        $served = $false
        foreach ($l in $listeners) {
            if (-not $l.Server.IsBound -or -not $l.Pending()) { continue }
            $served = $true
            $client = $l.AcceptTcpClient()
            try {
                $stream = $client.GetStream()
                $stream.ReadTimeout = 5000
                $reader = New-Object System.IO.StreamReader($stream, $utf8)
                $first = $reader.ReadLine()
                while ($true) { $line = $reader.ReadLine(); if ($null -eq $line -or $line -eq '') { break } }
                $path = if ($first) { ($first -split ' ')[1] } else { '/' }
                if ($path -like '/img/*') {
                    $body = $bmp; $type = 'image/bmp'
                } elseif ($path -eq '/favicon.ico') {
                    $body = [byte[]]@(); $type = 'image/x-icon'
                } else {
                    $n = ($path -replace '[^0-9]', ''); if (-not $n) { $n = '0' }
                    $html = @"
<!doctype html><html><head><meta charset="utf-8"><title>Local page $n</title>
<style>body{font:14px system-ui;margin:24px}li{padding:2px 0}img{width:256px;height:256px;margin:4px}</style></head>
<body><h1>Local page $n</h1><p id="clock"></p><canvas id="c" width="640" height="160"></canvas>
<div><img src="/img/a$n.bmp"><img src="/img/b$n.bmp"><img src="/img/c$n.bmp"><img src="/img/d$n.bmp"></div>
<ul>$items</ul>
<script>
const state = []; for (let i = 0; i < 100000; i++) state.push({ id: i, name: 'item ' + i, tags: [i % 7, i % 11] });
const ctx = document.getElementById('c').getContext('2d'); let t = 0;
(function frame() { t++; ctx.fillStyle = '#fff'; ctx.fillRect(0, 0, 640, 160); ctx.fillStyle = '#36c';
  ctx.fillRect((t * 3) % 640, 40, 60, 80); requestAnimationFrame(frame); })();
setInterval(() => { document.getElementById('clock').textContent = new Date().toISOString() + ' ' + state.length; }, 1000);
</script></body></html>
"@
                    $body = $utf8.GetBytes($html); $type = 'text/html; charset=utf-8'
                }
                $head = $utf8.GetBytes("HTTP/1.1 200 OK`r`nContent-Type: $type`r`nContent-Length: $($body.Length)`r`nCache-Control: max-age=3600`r`nConnection: close`r`n`r`n")
                $stream.Write($head, 0, $head.Length)
                if ($body.Length -gt 0) { $stream.Write($body, 0, $body.Length) }
                $stream.Flush()
            } catch { } finally { $client.Close() }
        }
        if (-not $served) { Start-Sleep -Milliseconds 5 }
    }
}

function Get-FreePort {
    $probe = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $probe.Start(); $port = $probe.LocalEndpoint.Port; $probe.Stop()
    $port
}

# ---------- measuring ----------

function Get-ProcessKind($p) {
    if ($p.Name -ne 'msedgewebview2.exe') { return 'other' }
    $cmd = [string]$p.CommandLine
    if ($cmd -notmatch '--type=([a-z-]+)') { return 'browser' }
    switch ($Matches[1]) {
        'renderer' { 'renderer' }
        'gpu-process' { 'gpu' }
        'utility' { 'utility' }
        'crashpad-handler' { 'crashpad' }
        default { $Matches[1] }
    }
}

# The launched app and every process below it, measured now.
function Get-TreeSample([int]$rootPid) {
    $all = @(Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId, Name, CommandLine)
    $byParent = @{}
    foreach ($p in $all) {
        if ($p.ProcessId -eq $p.ParentProcessId) { continue }
        if (-not $byParent.ContainsKey([int]$p.ParentProcessId)) { $byParent[[int]$p.ParentProcessId] = New-Object System.Collections.Generic.List[object] }
        $byParent[[int]$p.ParentProcessId].Add($p)
    }
    $kinds = [ordered]@{}
    foreach ($k in 'app', 'browser', 'gpu', 'utility', 'renderer', 'crashpad', 'other') { $kinds[$k] = [pscustomobject]@{ pws = 0.0; commit = 0.0; count = 0 } }
    $add = { param($kind, $procId) $m = [NeaMemoryProbe]::Read([uint32]$procId); $e = $kinds[$kind]; if (-not $e) { $e = $kinds['other'] }; $e.pws += $m[0]; $e.commit += $m[1]; $e.count++ }
    & $add 'app' $rootPid
    $seen = @{ $rootPid = $true }
    $pending = New-Object System.Collections.Generic.Stack[int]
    $pending.Push($rootPid)
    while ($pending.Count -gt 0) {
        $parent = $pending.Pop()
        if (-not $byParent.ContainsKey($parent)) { continue }
        foreach ($child in $byParent[$parent]) {
            $id = [int]$child.ProcessId
            if ($seen.ContainsKey($id)) { continue }
            $seen[$id] = $true
            & $add (Get-ProcessKind $child) $id
            $pending.Push($id)
        }
    }
    $pws = 0.0; $commit = 0.0
    foreach ($e in $kinds.Values) { $pws += $e.pws; $commit += $e.commit }
    [pscustomobject]@{ kinds = $kinds; pws = $pws; commit = $commit }
}

function Get-LiveCounts {
    $s = Invoke-Nea state
    $c = [ordered]@{ ready = 0; suspended = 0; asleep = 0; creating = 0 }
    foreach ($t in $s.tabs) { $c[$t.live]++ }
    $c
}

function Measure-Phase($run, [int]$n, [string]$phase) {
    $taken = @()
    for ($i = 0; $i -lt $Samples; $i++) {
        if ($i -gt 0) { Start-Sleep -Seconds 2 }
        $taken += , (Get-TreeSample $run.Process.Id)
    }
    $sorted = @($taken | Sort-Object pws)
    $m = $sorted[[int][Math]::Floor(($sorted.Count - 1) / 2)]
    $live = Get-LiveCounts
    $mb = { param($b) [Math]::Round($b / 1MB, 1) }
    $row = [ordered]@{
        tabs = $n; phase = $phase
        total_pws_mb = & $mb $m.pws; total_commit_mb = & $mb $m.commit
        ready = $live.ready; suspended = $live.suspended; asleep = $live.asleep
    }
    foreach ($k in $m.kinds.Keys) {
        $e = $m.kinds[$k]
        if ($e.count -eq 0) { continue }
        $row["${k}_pws_mb"] = & $mb $e.pws
        $row["${k}_commit_mb"] = & $mb $e.commit
        $row["${k}_count"] = $e.count
    }
    $row = [pscustomobject]$row
    $script:Rows.Add($row)
    $webview = $m.pws - $m.kinds['app'].pws
    Write-Host ("  {0,2} tabs {1,-10} total {2,7:N1} MB pws / {3,7:N1} MB commit | app {4,5:N1} | webview2 {5,7:N1} ({6} renderers) | ready {7}, suspended {8}, asleep {9}" -f `
            $n, $phase, $row.total_pws_mb, $row.total_commit_mb, (& $mb $m.kinds['app'].pws), (& $mb $webview), $m.kinds['renderer'].count, $live.ready, $live.suspended, $live.asleep)
}

# ---------- one scenario ----------

function Start-Nea([int]$n, [string[]]$urls) {
    $dir = Join-Path ([IO.Path]::GetTempPath()) ('nea-memory-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
    New-Item -ItemType Directory -Path $dir | Out-Null
    $all = @{ bench_enabled = $true; check_updates = $false }
    foreach ($k in $Settings.Keys) { $all[$k] = $Settings[$k] }
    [IO.File]::WriteAllText((Join-Path $dir 'settings.json'), ($all | ConvertTo-Json -Compress -Depth 5))
    $saved = @()
    for ($i = 0; $i -lt $n; $i++) { $saved += @{ url = $urls[$i]; title = "Tab $i"; pinned = $false } }
    $session = @{ tabs = $saved; active = 0; vertical_tabs = $false; sidebar_collapsed = $false; window = @(60, 60, 1460, 960); maximized = $false }
    [IO.File]::WriteAllText((Join-Path $dir 'session.json'), ($session | ConvertTo-Json -Compress -Depth 6))
    # The blocker lists are marked fresh, so nothing is downloaded.
    New-Item -ItemType Directory -Path (Join-Path $dir 'shield') | Out-Null
    [IO.File]::WriteAllText((Join-Path $dir 'shield\updated'), '')
    $env:NEA_DATA_DIR = $dir
    $env:NEA_BENCH = '1'
    $env:NEA_BENCH_PIPE = 'memory-' + [Guid]::NewGuid().ToString('N').Substring(0, 6)
    $env:NEA_CREDENTIAL_PREFIX = 'nea-memory-test'
    # Without -Heavy, each host other than the loopback resolves to nothing:
    # the run sends nothing outside this computer.
    $engine = if ($Heavy) { $EngineArgs } else { ('--host-resolver-rules="MAP * ~NOTFOUND, EXCLUDE 127.0.0.1, EXCLUDE *.localhost" ' + $EngineArgs).Trim() }
    if ($engine) { $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = $engine }
    $process = Start-Process -FilePath $Exe -PassThru
    Connect-Nea -TimeoutSec 20
    $ping = Invoke-Nea ping
    if ($ping.pid -ne $process.Id) { throw "A different process ($($ping.pid)) answers on the pipe." }
    [pscustomobject]@{ Process = $process; Dir = $dir }
}

function Stop-Nea($run) {
    try { Invoke-Nea quit | Out-Null } catch { }
    Disconnect-Nea
    if (-not $run.Process.WaitForExit(15000)) { Stop-Process -Id $run.Process.Id -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 800
    # Only engine processes that use this run's own data dir: ours.
    foreach ($p in @(Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($run.Dir) })) {
        Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue
    }
    Remove-Item Env:NEA_DATA_DIR, Env:NEA_BENCH, Env:NEA_BENCH_PIPE, Env:NEA_CREDENTIAL_PREFIX, Env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS -ErrorAction SilentlyContinue
    for ($i = 0; $i -lt 10; $i++) {
        try { Remove-Item -Recurse -Force -LiteralPath $run.Dir -ErrorAction Stop; return } catch { Start-Sleep -Milliseconds 500 }
    }
}

function Wait-Active { Invoke-Nea wait @{ timeout_ms = 20000 } -NoThrow | Out-Null }

function Select-Next([int]$times, [int]$pauseMs) {
    for ($i = 0; $i -lt $times; $i++) {
        Invoke-Nea run @{ name = 'next_tab' } | Out-Null
        Wait-Active
        Start-Sleep -Milliseconds $pauseMs
    }
}

function Invoke-Scenario([int]$n, [string[]]$urls) {
    Write-Host "Scenario: $n tabs"
    $run = Start-Nea $n $urls
    try {
        Wait-Active
        Select-Next ($n - 1) 400
        # Back on the first tab, as after a look through all of them.
        if ($n -gt 1) { Select-Next 1 400 }
        if ($SeriesSec -gt 0) {
            $watch = [Diagnostics.Stopwatch]::StartNew()
            while ($watch.Elapsed.TotalSeconds -lt $SeriesSec) {
                Start-Sleep -Seconds 5
                Measure-Phase $run $n ('{0:N0}s' -f $watch.Elapsed.TotalSeconds)
            }
            return
        }
        Start-Sleep -Seconds $SettleSec
        Measure-Phase $run $n 'idle'
        Start-Sleep -Seconds ([int]($IdleMin * 60))
        Measure-Phase $run $n ("idle-{0}min" -f $IdleMin)
        $hwnd = $run.Process.MainWindowHandle
        $run.Process.Refresh(); $hwnd = $run.Process.MainWindowHandle
        if ($MinimizedSec -gt 0 -and $hwnd -ne [IntPtr]::Zero) {
            [NeaMemoryProbe]::ShowWindow($hwnd, 7) | Out-Null   # SW_SHOWMINNOACTIVE
            Start-Sleep -Seconds $MinimizedSec
            Measure-Phase $run $n 'minimized'
            [NeaMemoryProbe]::ShowWindow($hwnd, 4) | Out-Null   # SW_SHOWNOACTIVATE
            Start-Sleep -Seconds 2
        }
        for ($r = 0; $r -lt $ChurnRounds; $r++) {
            for ($i = 0; $i -lt 10; $i++) {
                $u = $script:Base -f (1000 + $r * 10 + $i)
                $tab = (Invoke-Nea open @{ url = $u }).tab
                Invoke-Nea wait @{ tab = $tab; timeout_ms = 20000 } -NoThrow | Out-Null
                Start-Sleep -Milliseconds 300
            }
            Invoke-Nea close | Out-Null
            Select-Next ([Math]::Min($n, 10)) 300
        }
        Start-Sleep -Seconds $SettleSec
        Measure-Phase $run $n 'churn'
    } finally {
        Stop-Nea $run
    }
}

# ---------- main ----------

$port = Get-FreePort
$job = Start-Job -ScriptBlock $server -ArgumentList $port
$script:Base = "http://s{0}.localhost:$port/p/{0}"
$script:Rows = New-Object System.Collections.Generic.List[object]
try {
    $deadline = (Get-Date).AddSeconds(15)
    while ($true) {
        try { Invoke-WebRequest -UseBasicParsing -TimeoutSec 2 "http://127.0.0.1:$port/p/0" | Out-Null; break } catch {
            if ((Get-Date) -gt $deadline) { throw "The local page server did not start: $(Receive-Job $job 2>&1)" }
            Start-Sleep -Milliseconds 300
        }
    }
    $heavyOk = @()
    if ($Heavy) {
        foreach ($u in $HeavyUrls) {
            try { Invoke-WebRequest -UseBasicParsing -Method Head -TimeoutSec 6 $u | Out-Null; $heavyOk += $u } catch { Write-Host "Not reachable, left out: $u" }
        }
    }
    $engine = Get-ChildItem -Path "${env:ProgramFiles(x86)}\Microsoft\EdgeWebView\Application" -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^\d' } | Sort-Object { [version]$_.Name } | Select-Object -Last 1
    $commit = (git -C $root rev-parse --short HEAD 2>$null)
    Write-Host ("Nea {0} at {1}, WebView2 {2}, heavy pages: {3}" -f $Exe, $commit, $engine.Name, $heavyOk.Count)
    foreach ($n in $Tabs) {
        $urls = @(for ($i = 0; $i -lt $n; $i++) { $script:Base -f $i })
        # Real pages replace tabs 2, 3, 4...: the first tab stays local.
        for ($i = 0; $i -lt $heavyOk.Count -and ($i + 1) -lt $n; $i++) { $urls[$i + 1] = $heavyOk[$i] }
        Invoke-Scenario $n $urls
    }
} finally {
    Stop-Job $job -ErrorAction SilentlyContinue
    Remove-Job $job -Force -ErrorAction SilentlyContinue
}

if ($Out) {
    $report = [pscustomobject]@{
        date = (Get-Date).ToString('s'); commit = $commit; webview2 = $engine.Name; exe = $Exe
        engine_args = $EngineArgs; settings = $Settings; heavy = $heavyOk; rows = $script:Rows
    }
    [IO.File]::WriteAllText($Out, ($report | ConvertTo-Json -Depth 6))
    Write-Host "Written: $Out"
}
