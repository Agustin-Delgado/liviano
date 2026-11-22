<#
.SYNOPSIS
  Looks for leaks in the app process: it opens, selects and closes tabs,
  opens and closes Settings and the field with its list, many times, and
  writes the commit, the working set and the handle counts after each
  round.

.DESCRIPTION
  The script starts a new Nea with a temporary NEA_DATA_DIR and script
  control on a separate pipe. The pages come from a small server on the
  loopback interface only. The script sends no keys or clicks to the
  system and takes no focus: it uses the pipe, and messages to the
  windows of the Nea that it started (WM_CLOSE to Settings, Escape to the
  field's own edit control).

  A round is (-Parts selects some of them):
    - open -TabsPerRound tabs, wait for each, select the next tab twice,
      then close the test tabs
    - open Settings, then close it
    - open the field, type a text (the list of suggestions opens), then
      press Escape in it

  Each line also has the commit of the engine processes of this Nea and of
  the engine's browser process alone.

  At the end, the script prints the first and the last values and the
  slope (by least squares) of the second half of the rounds, for each
  counter. A counter that goes up all the time is a leak.

.EXAMPLE
  tools\leaks.ps1 -Exe C:\Temp\nea-ram2\release\nea.exe -Rounds 200 -Out C:\Temp\leaks.json
#>
[CmdletBinding()]
param(
    [string]$Exe = 'C:\Temp\nearam\release\nea.exe',
    [int]$Rounds = 100,
    [int]$TabsPerRound = 3,
    [int]$StartTabs = 3,
    # Parts of a round to run: tabs, settings, field.
    [string[]]$Parts = @('tabs', 'settings', 'field'),
    # After the last round: a rest with no requests, then one more line. With
    # no input in the session for 2 minutes, Nea gives back its memory.
    [int]$RestSec = 0,
    [string]$Out = ''
)

$ErrorActionPreference = 'Stop'
$Parts = @($Parts | ForEach-Object { "$_" -split ',' } | Where-Object { $_ })
. (Join-Path $PSScriptRoot 'bench.ps1')
$Exe = (Resolve-Path $Exe).Path

Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class NeaLeakProbe {
    [StructLayout(LayoutKind.Sequential)]
    struct Counters {
        public uint cb; public uint PageFaultCount;
        public UIntPtr PeakWorkingSetSize, WorkingSetSize, QuotaPeakPagedPoolUsage, QuotaPagedPoolUsage,
            QuotaPeakNonPagedPoolUsage, QuotaNonPagedPoolUsage, PagefileUsage, PeakPagefileUsage,
            PrivateUsage, PrivateWorkingSetSize;
        public ulong SharedCommitUsage;
    }
    delegate bool EnumProc(IntPtr hwnd, IntPtr lparam);
    [DllImport("kernel32.dll")] static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll")] static extern bool GetProcessHandleCount(IntPtr process, out uint count);
    [DllImport("psapi.dll")] static extern bool GetProcessMemoryInfo(IntPtr process, ref Counters counters, uint cb);
    [DllImport("user32.dll")] static extern uint GetGuiResources(IntPtr process, uint flags);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc proc, IntPtr lparam);
    [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr parent, EnumProc proc, IntPtr lparam);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassNameW(IntPtr hwnd, StringBuilder name, int max);
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);
    // { private working set, commit, GDI objects, USER objects, handles }
    public static ulong[] Read(uint pid) {
        IntPtr h = OpenProcess(0x1000 | 0x0400, false, pid);
        if (h == IntPtr.Zero) return new ulong[] { 0, 0, 0, 0, 0 };
        var c = new Counters();
        c.cb = (uint)Marshal.SizeOf(typeof(Counters));
        GetProcessMemoryInfo(h, ref c, c.cb);
        uint handles; GetProcessHandleCount(h, out handles);
        ulong gdi = GetGuiResources(h, 0), user = GetGuiResources(h, 1);
        CloseHandle(h);
        return new ulong[] { (ulong)c.PrivateWorkingSetSize, (ulong)c.PrivateUsage, gdi, user, handles };
    }
    static string ClassOf(IntPtr hwnd) { var sb = new StringBuilder(64); GetClassNameW(hwnd, sb, 64); return sb.ToString(); }
    // The top-level windows of `pid` with the class `cls`.
    public static IntPtr[] TopLevel(uint pid, string cls) {
        var found = new List<IntPtr>();
        EnumWindows((hwnd, _) => { uint p; GetWindowThreadProcessId(hwnd, out p); if (p == pid && ClassOf(hwnd) == cls) found.Add(hwnd); return true; }, IntPtr.Zero);
        return found.ToArray();
    }
    // The first child with the class `cls` of the windows of `pid` with the class `parentCls`.
    public static IntPtr Child(uint pid, string parentCls, string cls) {
        foreach (var top in TopLevel(pid, parentCls)) {
            IntPtr hit = IntPtr.Zero;
            EnumChildWindows(top, (hwnd, _) => { if (ClassOf(hwnd).Equals(cls, StringComparison.OrdinalIgnoreCase)) { hit = hwnd; return false; } return true; }, IntPtr.Zero);
            if (hit != IntPtr.Zero) return hit;
        }
        return IntPtr.Zero;
    }
}
'@

$server = {
    param([int]$port)
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $port)
    $listener.Start()
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    $items = (1..300 | ForEach-Object { "<li>Row $_ <a href='#r$_'>link</a></li>" }) -join ''
    while ($true) {
        $client = $listener.AcceptTcpClient()
        try {
            $stream = $client.GetStream()
            $stream.ReadTimeout = 5000
            $reader = New-Object System.IO.StreamReader($stream, $utf8)
            $first = $reader.ReadLine()
            while ($true) { $line = $reader.ReadLine(); if ($null -eq $line -or $line -eq '') { break } }
            $path = if ($first) { ($first -split ' ')[1] } else { '/' }
            if ($path -eq '/favicon.ico') { $body = [byte[]]@(); $type = 'image/x-icon' } else {
                $n = ($path -replace '[^0-9]', '')
                $body = $utf8.GetBytes("<!doctype html><html><head><meta charset='utf-8'><title>Leak page $n</title></head><body><h1>Leak page $n</h1><ul>$items</ul></body></html>")
                $type = 'text/html; charset=utf-8'
            }
            $head = $utf8.GetBytes("HTTP/1.1 200 OK`r`nContent-Type: $type`r`nContent-Length: $($body.Length)`r`nConnection: close`r`n`r`n")
            $stream.Write($head, 0, $head.Length)
            if ($body.Length -gt 0) { $stream.Write($body, 0, $body.Length) }
            $stream.Flush()
        } catch { } finally { $client.Close() }
    }
}

function Get-FreePort {
    $probe = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $probe.Start(); $port = $probe.LocalEndpoint.Port; $probe.Stop()
    $port
}

# Commit of the engine's browser process and of all the engine processes
# of this Nea (its process tree), in bytes.
function Get-EngineCommit([int]$rootPid) {
    $all = @(Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId, Name, CommandLine)
    $mine = @{ $rootPid = $true }
    $added = $true
    while ($added) {
        $added = $false
        foreach ($p in $all) { if (-not $mine.ContainsKey([int]$p.ProcessId) -and $mine.ContainsKey([int]$p.ParentProcessId) -and $p.ProcessId -ne $p.ParentProcessId) { $mine[[int]$p.ProcessId] = $true; $added = $true } }
    }
    $browser = 0.0; $total = 0.0
    foreach ($p in $all) {
        if ([int]$p.ProcessId -eq $rootPid -or -not $mine.ContainsKey([int]$p.ProcessId)) { continue }
        $c = [double]([NeaLeakProbe]::Read([uint32]$p.ProcessId)[1])
        $total += $c
        if ($p.Name -eq 'msedgewebview2.exe' -and [string]$p.CommandLine -notmatch '--type=') { $browser += $c }
    }
    @($browser, $total)
}

function Get-Slope([double[]]$ys) {
    $n = $ys.Count
    if ($n -lt 2) { return 0.0 }
    $mx = ($n - 1) / 2.0; $my = ($ys | Measure-Object -Average).Average
    $num = 0.0; $den = 0.0
    for ($i = 0; $i -lt $n; $i++) { $num += ($i - $mx) * ($ys[$i] - $my); $den += ($i - $mx) * ($i - $mx) }
    $num / $den
}

$port = Get-FreePort
$job = Start-Job -ScriptBlock $server -ArgumentList $port
$base = "http://127.0.0.1:$port/p/{0}"
$dir = Join-Path ([IO.Path]::GetTempPath()) ('nea-leaks-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $dir | Out-Null
$rows = New-Object System.Collections.Generic.List[object]
$process = $null
try {
    [IO.File]::WriteAllText((Join-Path $dir 'settings.json'), (@{ bench_enabled = $true; check_updates = $false } | ConvertTo-Json -Compress))
    $saved = @(for ($i = 0; $i -lt $StartTabs; $i++) { @{ url = ($base -f $i); title = "Tab $i"; pinned = $false } })
    $session = @{ tabs = $saved; active = 0; vertical_tabs = $false; sidebar_collapsed = $false; window = @(60, 60, 1460, 960); maximized = $false }
    [IO.File]::WriteAllText((Join-Path $dir 'session.json'), ($session | ConvertTo-Json -Compress -Depth 6))
    $env:NEA_DATA_DIR = $dir
    $env:NEA_BENCH = '1'
    $env:NEA_BENCH_PIPE = 'leaks-' + [Guid]::NewGuid().ToString('N').Substring(0, 6)
    $env:NEA_CREDENTIAL_PREFIX = 'nea-leaks-test'
    $process = Start-Process -FilePath $Exe -PassThru
    Connect-Nea -TimeoutSec 20
    if ((Invoke-Nea ping).pid -ne $process.Id) { throw 'A different process answers on the pipe.' }
    $procId = [uint32]$process.Id
    Invoke-Nea wait @{ timeout_ms = 20000 } -NoThrow | Out-Null
    Start-Sleep -Seconds 5
    $sw = [Diagnostics.Stopwatch]::StartNew()
    for ($r = 0; $r -le $Rounds; $r++) {
        if ($r -gt 0 -and $Parts -contains 'tabs') {
            for ($i = 0; $i -lt $TabsPerRound; $i++) {
                $tab = (Invoke-Nea open @{ url = ($base -f (100 + ($r * $TabsPerRound + $i) % 50)) }).tab
                Invoke-Nea wait @{ tab = $tab; timeout_ms = 20000 } -NoThrow | Out-Null
            }
            for ($i = 0; $i -lt 2; $i++) { Invoke-Nea run @{ name = 'next_tab' } | Out-Null; Start-Sleep -Milliseconds 120 }
            Invoke-Nea close | Out-Null
        }
        if ($r -gt 0 -and $Parts -contains 'settings') {
            Invoke-Nea run @{ name = 'settings' } | Out-Null
            Start-Sleep -Milliseconds 250
            foreach ($w in [NeaLeakProbe]::TopLevel($procId, 'NeaSettings')) { [NeaLeakProbe]::PostMessageW($w, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null }
            Start-Sleep -Milliseconds 200
        }
        if ($r -gt 0 -and $Parts -contains 'field') {
            Invoke-Nea run @{ name = 'focus_address' } | Out-Null
            Invoke-Nea type @{ text = "leak page $r" } -NoThrow | Out-Null
            Start-Sleep -Milliseconds 150
            $edit = [NeaLeakProbe]::Child($procId, 'NeaOmnibar', 'EDIT')
            if ($edit -ne [IntPtr]::Zero) { [NeaLeakProbe]::SendMessageW($edit, 0x0100, [IntPtr]0x1B, [IntPtr]1) | Out-Null }
            Start-Sleep -Milliseconds 150
        }
        if ($r % [Math]::Max(1, [int]($Rounds / 20)) -eq 0 -or $r -eq $Rounds) {
            Start-Sleep -Seconds 2
            $m = [NeaLeakProbe]::Read($procId)
            $engine = Get-EngineCommit ([int]$procId)
            $row = [pscustomobject]@{ round = $r; sec = [int]$sw.Elapsed.TotalSeconds; pws_mb = [Math]::Round($m[0] / 1MB, 1); commit_mb = [Math]::Round($m[1] / 1MB, 1); gdi = $m[2]; user = $m[3]; handles = $m[4]; engine_browser_mb = [Math]::Round($engine[0] / 1MB, 1); engine_mb = [Math]::Round($engine[1] / 1MB, 1) }
            $rows.Add($row)
            Write-Host ("round {0,4} {1,5}s  pws {2,6:N1} MB  commit {3,6:N1} MB  gdi {4,4}  user {5,4}  handles {6,5} | engine {7,7:N1} MB, its browser process {8,6:N1} MB" -f $row.round, $row.sec, $row.pws_mb, $row.commit_mb, $row.gdi, $row.user, $row.handles, $row.engine_mb, $row.engine_browser_mb)
        }
    }
    if ($RestSec -gt 0) {
        Start-Sleep -Seconds $RestSec
        $m = [NeaLeakProbe]::Read($procId)
        $engine = Get-EngineCommit ([int]$procId)
        $rest = [pscustomobject]@{ round = 'rest'; sec = [int]$sw.Elapsed.TotalSeconds; pws_mb = [Math]::Round($m[0] / 1MB, 1); commit_mb = [Math]::Round($m[1] / 1MB, 1); gdi = $m[2]; user = $m[3]; handles = $m[4]; engine_browser_mb = [Math]::Round($engine[0] / 1MB, 1); engine_mb = [Math]::Round($engine[1] / 1MB, 1) }
        Write-Host ("after a rest of {0} s: pws {1:N1} MB  commit {2:N1} MB  gdi {3}  user {4}  handles {5} | engine {6:N1} MB" -f $RestSec, $rest.pws_mb, $rest.commit_mb, $rest.gdi, $rest.user, $rest.handles, $rest.engine_mb)
    }
} finally {
    try { Invoke-Nea quit | Out-Null } catch { }
    Disconnect-Nea
    if ($process -and -not $process.WaitForExit(15000)) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 800
    foreach ($p in @(Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($dir) })) {
        Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue
    }
    Remove-Item Env:NEA_DATA_DIR, Env:NEA_BENCH, Env:NEA_BENCH_PIPE, Env:NEA_CREDENTIAL_PREFIX -ErrorAction SilentlyContinue
    Stop-Job $job -ErrorAction SilentlyContinue; Remove-Job $job -Force -ErrorAction SilentlyContinue
    for ($i = 0; $i -lt 10; $i++) { try { Remove-Item -Recurse -Force -LiteralPath $dir -ErrorAction Stop; break } catch { Start-Sleep -Milliseconds 500 } }
}

$half = @($rows | Select-Object -Skip ([int]($rows.Count / 2)))
$per = if ($half.Count -gt 1) { ($half[-1].round - $half[0].round) / ($half.Count - 1) } else { 1 }
foreach ($k in 'commit_mb', 'pws_mb', 'gdi', 'user', 'handles', 'engine_browser_mb', 'engine_mb') {
    $slope = (Get-Slope @($half | ForEach-Object { [double]$_.$k })) / [Math]::Max(1, $per)
    Write-Host ("{0,-10} first {1,8}  last {2,8}  slope of the second half: {3:N3} per round" -f $k, $rows[0].$k, $rows[-1].$k, $slope)
}
if ($Out) { [IO.File]::WriteAllText($Out, ($rows | ConvertTo-Json -Depth 4)); Write-Host "Written: $Out" }
