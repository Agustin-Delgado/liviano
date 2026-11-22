<#
.SYNOPSIS
  A long run of random actions on a new Nea, to find crashes and hangs.

.DESCRIPTION
  The script starts a new Nea with a temporary NEA_DATA_DIR and script
  control on a separate pipe. The pages come from a small page server on
  the loopback interface only (127.0.0.1). The engine gets
  --host-resolver-rules that map every other host to nothing, the updater
  is off and the blocker lists are marked fresh: the run sends nothing
  outside this computer.

  Each step is one random action:
    open, open in the background, close, next and previous tab, follow a
    link in the page, back, forward, reload, discard, Settings (open and
    close), the address field, the tab layout, the DPI (96 to 192), the
    theme (WM_SETTINGCHANGE "ImmersiveColorSet" to the window of this
    process), and messages with bad pointers (WM_NOTIFY, WM_DRAWITEM,
    WM_NCCALCSIZE, WM_GETMINMAXINFO posted with a random lParam).
  With -Kill, also: end a page process, the GPU process or the whole
  engine (only the processes under the Nea that the script started).

  After each -CheckEvery steps, the script makes sure that Nea still
  answers. At the end, it shows crash.log and hang.log. The exit code is 1
  when Nea ended, stopped answering, wrote a panic or an exception, or
  (with -FailOnStall) stopped for 2 s or more.

  The script sends no keys or clicks to the system, and it does not move a
  window to the front.

.EXAMPLE
  tools\chaos.ps1 -Exe C:\Temp\nea-crash2\release\nea.exe -Steps 3000 -Kill -Seed 7
#>
param(
    [string]$Exe = '',
    [int]$Steps = 1500,
    [int]$Seed = 0,
    # The pause between two actions (ms): 0 sends them as fast as Nea answers.
    [int]$PauseMs = 40,
    [int]$CheckEvery = 50,
    # Also end engine processes (page, GPU, the whole engine).
    [switch]$Kill,
    [switch]$KeepData,
    [switch]$FailOnStall
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'bench.ps1')
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Exe) { $Exe = Join-Path $root 'target\release\nea.exe' }
$Exe = (Resolve-Path $Exe).Path
if (-not $Seed) { $Seed = Get-Random -Maximum 1000000 }
$random = New-Object System.Random($Seed)
Write-Host "== chaos: $Steps steps, seed $Seed"

if (-not ('NeaChaosWin32' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class NeaChaosWin32 {
    delegate bool EnumProc(IntPtr hwnd, IntPtr data);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc proc, IntPtr data);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassNameW(IntPtr hwnd, System.Text.StringBuilder name, int size);
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hwnd, uint msg, IntPtr wparam, IntPtr lparam);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    static extern IntPtr SendMessageTimeoutW(IntPtr hwnd, uint msg, IntPtr wparam, string lparam, uint flags, uint timeout, out IntPtr result);

    // The top-level windows of one process (all, or only the visible ones),
    // with their class names.
    public static List<KeyValuePair<IntPtr, string>> Windows(uint pid, bool visibleOnly) {
        var found = new List<KeyValuePair<IntPtr, string>>();
        EnumWindows((hwnd, data) => {
            uint owner;
            GetWindowThreadProcessId(hwnd, out owner);
            if (owner == pid && (!visibleOnly || IsWindowVisible(hwnd))) {
                var name = new System.Text.StringBuilder(256);
                GetClassNameW(hwnd, name, name.Capacity);
                found.Add(new KeyValuePair<IntPtr, string>(hwnd, name.ToString()));
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    // WM_SETTINGCHANGE with its text: the system copies a sent string into
    // the other process (a posted one would be a bad pointer there).
    public static bool SettingChange(IntPtr hwnd, string area) {
        IntPtr result;
        return SendMessageTimeoutW(hwnd, 0x001A, IntPtr.Zero, area, 0x0002, 5000, out result) != IntPtr.Zero;
    }
}
'@
}

$server = {
    param([int]$port)
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $port)
    $listener.Start()
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    while ($true) {
        $client = $listener.AcceptTcpClient()
        try {
            $stream = $client.GetStream()
            $stream.ReadTimeout = 5000
            $reader = New-Object System.IO.StreamReader($stream, $utf8)
            $first = $reader.ReadLine()
            while ($true) { $line = $reader.ReadLine(); if ($null -eq $line -or $line -eq '') { break } }
            $path = if ($first) { ($first -split ' ')[1] } else { '/' }
            $n = [int](($path -replace '[^0-9]', '') + '0') / 10
            # Pages of a few kinds: links, a frame, history.pushState, a timer
            # that changes the title, and a long page.
            # A frame page (/f/) is plain: a frame in each frame never ends loading.
            $kind = if ($path -like '/f/*') { 0 } else { $n % 5 }
            $links = ((1..6 | ForEach-Object { "<p><a href=`"/p/$($n * 7 + $_)`">Link $_</a></p>" }) -join '')
            $extra = switch ($kind) {
                1 { "<iframe src=`"/f/$n`" width=`"300`" height=`"120`"></iframe>" }
                2 { "<script>history.pushState({}, '', '/p/$($n + 1)'); history.pushState({}, '', '/p/$($n + 2)');</script>" }
                3 { "<script>let i = 0; setInterval(() => { document.title = 'Page $n ' + (++i); }, 50);</script>" }
                4 { (1..400 | ForEach-Object { "<p>Line $_ of page $n.</p>" }) -join '' }
                default { '' }
            }
            $html = "<!doctype html><html><head><meta charset=`"utf-8`"><title>Page $n</title></head>" +
                "<body style=`"font:14px system-ui;margin:24px`"><h1>Page $n</h1>$links$extra</body></html>"
            $body = if ($path -eq '/favicon.ico') { [byte[]]@() } else { $utf8.GetBytes($html) }
            $head = $utf8.GetBytes("HTTP/1.1 200 OK`r`nContent-Type: text/html; charset=utf-8`r`nContent-Length: $($body.Length)`r`nConnection: close`r`n`r`n")
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

# The engine processes under the Nea of this run: a process counts only if
# its parent chain reaches $rootPid. The script ends nothing else.
function Get-EngineProcesses([int]$rootPid) {
    $all = @(Get-CimInstance Win32_Process -Filter "Name = 'msedgewebview2.exe'" -ErrorAction SilentlyContinue)
    $parents = @{}
    foreach ($p in @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue)) { $parents[[int]$p.ProcessId] = [int]$p.ParentProcessId }
    foreach ($p in $all) {
        $at = [int]$p.ParentProcessId
        for ($depth = 0; $depth -lt 4 -and $at; $depth++) {
            if ($at -eq $rootPid) {
                $type = if ($p.CommandLine -match '--type=(\S+)') { $Matches[1] } else { 'browser' }
                [pscustomobject]@{ Id = [int]$p.ProcessId; Type = $type; Parent = [int]$p.ParentProcessId }
                break
            }
            $at = $parents[$at]
        }
    }
}

$port = Get-FreePort
$job = Start-Job -ScriptBlock $server -ArgumentList $port
$base = "http://127.0.0.1:$port/p/"
$data = Join-Path ([IO.Path]::GetTempPath()) ('neachaos-data-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $data | Out-Null
$process = $null
$failures = New-Object System.Collections.Generic.List[string]
$counts = @{}
$oldArgs = $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS
try {
    $deadline = (Get-Date).AddSeconds(15)
    while ($true) {
        try { Invoke-WebRequest -UseBasicParsing -TimeoutSec 2 "${base}0" | Out-Null; break } catch {
            if ((Get-Date) -gt $deadline) { throw "The local page server did not start: $(Receive-Job $job 2>&1)" }
            Start-Sleep -Milliseconds 300
        }
    }
    # No network outside this computer: no update check, the blocker lists
    # are fresh (the built-in ones), every host except 127.0.0.1 fails.
    $settings = @{ bench_enabled = $true; check_updates = $false; update_url = 'http://127.0.0.1:9/latest.json' }
    [IO.File]::WriteAllText((Join-Path $data 'settings.json'), ($settings | ConvertTo-Json -Compress))
    New-Item -ItemType Directory -Path (Join-Path $data 'shield') | Out-Null
    [IO.File]::WriteAllText((Join-Path $data 'shield\updated'), '')
    $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = '--host-resolver-rules="MAP * ~NOTFOUND, EXCLUDE 127.0.0.1"'
    $env:NEA_DATA_DIR = $data
    $env:NEA_BENCH = '1'
    $env:NEA_CREDENTIAL_PREFIX = 'nea-chaos-test'
    $env:NEA_BENCH_PIPE = 'chaos-' + [Guid]::NewGuid().ToString('N').Substring(0, 6)
    $process = Start-Process -FilePath $Exe -PassThru
    Connect-Nea -TimeoutSec 20
    if ((Invoke-Nea ping).pid -ne $process.Id) { throw 'A different process answers on the pipe.' }
    $nea = [uint32]$process.Id

    $page = 0
    function Next-Url { $script:page++; "$base$($script:page)" }
    function Main-Window {
        foreach ($w in [NeaChaosWin32]::Windows($nea, $true)) { if ($w.Value -eq 'NeaWindow') { return $w.Key } }
        [IntPtr]::Zero
    }
    function Bad-Pointer {
        # 0, small numbers, odd addresses and random high bits.
        $choices = @(0, 1, 0x10, 0x1001, [int64]0x7FFE0000, [int64]0xDEADBEEF, [int64]$random.Next() * 4096 + 3)
        [IntPtr][int64]$choices[$random.Next($choices.Count)]
    }

    $actions = @(
        @{ name = 'open'; weight = 10; do = { Invoke-Nea open @{ url = (Next-Url) } -NoThrow | Out-Null } }
        @{ name = 'open_background'; weight = 5; do = { Invoke-Nea open @{ url = (Next-Url); background = $true } -NoThrow | Out-Null } }
        @{ name = 'close'; weight = 7; do = {
            $state = (Invoke-Nea state -NoThrow).result
            if ($state -and @($state.tabs).Count -gt 2) { Invoke-Nea close @{} -NoThrow | Out-Null }
        } }
        @{ name = 'next_tab'; weight = 8; do = { Invoke-Nea run @{ name = 'next_tab' } -NoThrow | Out-Null } }
        @{ name = 'prev_tab'; weight = 4; do = { Invoke-Nea run @{ name = 'prev_tab' } -NoThrow | Out-Null } }
        @{ name = 'link'; weight = 8; do = { Invoke-Nea click @{ selector = "a:nth-of-type(1), p:nth-of-type($(1 + $random.Next(6))) a" } -NoThrow | Out-Null } }
        @{ name = 'back'; weight = 5; do = { Invoke-Nea run @{ name = 'back' } -NoThrow | Out-Null } }
        @{ name = 'forward'; weight = 2; do = { Invoke-Nea run @{ name = 'forward' } -NoThrow | Out-Null } }
        @{ name = 'reload'; weight = 2; do = { Invoke-Nea run @{ name = 'reload' } -NoThrow | Out-Null } }
        @{ name = 'wait'; weight = 3; do = { Invoke-Nea wait @{ timeout_ms = 3000 } -NoThrow | Out-Null } }
        @{ name = 'discard'; weight = 3; do = { Invoke-Nea discard -NoThrow | Out-Null } }
        @{ name = 'settings'; weight = 2; do = {
            Invoke-Nea run @{ name = 'settings' } -NoThrow | Out-Null
            Start-Sleep -Milliseconds (50 + $random.Next(400))
            foreach ($w in [NeaChaosWin32]::Windows($nea, $true)) {
                if ($w.Value -eq 'NeaSettings') { [void][NeaChaosWin32]::PostMessageW($w.Key, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) }
            }
        } }
        @{ name = 'field'; weight = 3; do = { Invoke-Nea run @{ name = 'focus_address' } -NoThrow | Out-Null } }
        @{ name = 'history'; weight = 1; do = { Invoke-Nea run @{ name = 'history' } -NoThrow | Out-Null } }
        @{ name = 'layout'; weight = 1; do = { Invoke-Nea run @{ name = 'toggle_tab_layout' } -NoThrow | Out-Null } }
        @{ name = 'sidebar'; weight = 1; do = { Invoke-Nea run @{ name = 'toggle_sidebar' } -NoThrow | Out-Null } }
        @{ name = 'reopen'; weight = 2; do = { Invoke-Nea run @{ name = 'reopen_tab' } -NoThrow | Out-Null } }
        @{ name = 'duplicate'; weight = 1; do = { Invoke-Nea run @{ name = 'duplicate_tab' } -NoThrow | Out-Null } }
        @{ name = 'split'; weight = 1; do = { Invoke-Nea run @{ name = @('split_page', 'unsplit')[$random.Next(2)] } -NoThrow | Out-Null } }
        @{ name = 'dpi'; weight = 2; do = { Invoke-Nea dpi @{ dpi = @(96, 120, 144, 168, 192)[$random.Next(5)] } -NoThrow | Out-Null } }
        @{ name = 'theme'; weight = 2; do = {
            $hwnd = Main-Window
            if ($hwnd -ne [IntPtr]::Zero) { [void][NeaChaosWin32]::SettingChange($hwnd, 'ImmersiveColorSet') }
        } }
        @{ name = 'bad_message'; weight = 3; do = {
            # WM_NOTIFY, WM_DRAWITEM, WM_NCCALCSIZE (wParam 1), WM_GETMINMAXINFO, WM_DPICHANGED.
            $messages = @(@(0x004E, 0), @(0x002B, 0), @(0x0083, 1), @(0x0024, 0), @(0x02E0, 0x00600060))
            $pick = $messages[$random.Next($messages.Count)]
            # Only to the windows of Nea's own classes: in a window of the
            # system (an IME, a tooltip of comctl32) the bad pointer is read
            # by Windows itself, not by code of Nea.
            foreach ($w in [NeaChaosWin32]::Windows($nea, $false)) {
                if ($w.Value -notlike 'Nea*') { continue }
                [void][NeaChaosWin32]::PostMessageW($w.Key, [uint32]$pick[0], [IntPtr][int64]$pick[1], (Bad-Pointer))
            }
        } }
    )
    if ($Kill) {
        $actions += @{ name = 'kill_page'; weight = 2; do = {
            $pages = @(Get-EngineProcesses $process.Id | Where-Object { $_.Type -eq 'renderer' })
            if ($pages.Count) { Stop-Process -Id $pages[$random.Next($pages.Count)].Id -Force -ErrorAction SilentlyContinue }
        } }
        $actions += @{ name = 'kill_gpu'; weight = 1; do = {
            Get-EngineProcesses $process.Id | Where-Object { $_.Type -eq 'gpu-process' } |
                ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
        } }
        $actions += @{ name = 'kill_engine'; weight = 1; do = {
            Get-EngineProcesses $process.Id | Where-Object { $_.Type -eq 'browser' -and $_.Parent -eq $process.Id } |
                ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
        } }
    }
    $total = ($actions | ForEach-Object { $_.weight } | Measure-Object -Sum).Sum

    for ($i = 0; $i -lt 5; $i++) { Invoke-Nea open @{ url = (Next-Url) } -NoThrow | Out-Null }
    Invoke-Nea wait @{ timeout_ms = 15000 } -NoThrow | Out-Null
    $watch = [Diagnostics.Stopwatch]::StartNew()
    for ($step = 1; $step -le $Steps; $step++) {
        $roll = $random.Next($total)
        $action = $null
        foreach ($a in $actions) { if ($roll -lt $a.weight) { $action = $a; break }; $roll -= $a.weight }
        $counts[$action.name] = 1 + [int]$counts[$action.name]
        try { & $action.do } catch {
            if ($process.HasExited) { $failures.Add("step $step ($($action.name)): Nea ended, code $($process.ExitCode)"); break }
            # The pipe closed or a reply did not come: try once more to connect.
            try { Connect-Nea -TimeoutSec 10 } catch { $failures.Add("step $step ($($action.name)): no answer: $_"); break }
        }
        if ($process.HasExited) { $failures.Add("step $step ($($action.name)): Nea ended, code $($process.ExitCode)"); break }
        if ($step % $CheckEvery -eq 0) {
            $ping = Invoke-Nea ping -NoThrow
            if (-not $ping.ok) { $failures.Add("step ${step}: no answer to ping"); break }
            $state = (Invoke-Nea state -NoThrow).result
            Write-Host ("  step {0,5}  {1,4:N0} s  tabs {2}" -f $step, $watch.Elapsed.TotalSeconds, @($state.tabs).Count)
        }
        if ($PauseMs) { Start-Sleep -Milliseconds $PauseMs }
    }
    if (-not $process.HasExited) {
        # Last proof: the visible tab still loads a page.
        $tab = (Invoke-Nea open @{ url = (Next-Url) } -NoThrow).result.tab
        # After many engine kills, the engine waits for the end of its
        # restart window (2 min) before it starts again.
        $limit = if ($Kill) { 150000 } else { 20000 }
        $waited = Invoke-Nea wait @{ tab = $tab; timeout_ms = $limit } -NoThrow
        if (-not $waited.ok) {
            $state = (Invoke-Nea state -NoThrow).result | ConvertTo-Json -Compress -Depth 6
            $failures.Add("after the run: a new tab did not load ($($waited.error)); state: $state")
        }
    }
} finally {
    if ($process -and -not $process.HasExited) {
        try { Invoke-Nea quit -NoThrow | Out-Null } catch { }
    }
    Disconnect-Nea
    if ($process -and -not $process.WaitForExit(20000)) {
        $failures.Add('Nea did not end after quit')
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    }
    Stop-Job $job -ErrorAction SilentlyContinue; Remove-Job $job -Force -ErrorAction SilentlyContinue
    Remove-Item Env:NEA_DATA_DIR, Env:NEA_BENCH, Env:NEA_BENCH_PIPE, Env:NEA_CREDENTIAL_PREFIX -ErrorAction SilentlyContinue
    if ($null -eq $oldArgs) { Remove-Item Env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS -ErrorAction SilentlyContinue }
    else { $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = $oldArgs }
}

Write-Host ('actions: ' + (($counts.GetEnumerator() | Sort-Object Name | ForEach-Object { "$($_.Name)=$($_.Value)" }) -join ' '))
$crash = @()
foreach ($name in 'crash.log', 'hang.log') {
    $path = Join-Path $data $name
    if (Test-Path $path) {
        $lines = @(Get-Content -LiteralPath $path)
        if ($name -eq 'crash.log') { $crash = $lines }
        Write-Host "${name}:"
        $lines | Select-Object -Last 40 | ForEach-Object { Write-Host "  $_" }
        if ($name -eq 'hang.log' -and $FailOnStall -and @($lines | Where-Object { $_ -match 'not responding|stopped' }).Count) {
            $failures.Add('hang.log has a stop of 2 s or more')
        }
    }
}
foreach ($line in $crash) {
    if ($line -match ' panic v| exception v') { $failures.Add("crash.log: $line") }
}
if (-not $KeepData) { Start-Sleep -Seconds 2; Remove-Item -Recurse -Force -LiteralPath $data -ErrorAction SilentlyContinue } else { Write-Host "Data: $data" }
if ($failures.Count) {
    Write-Host "== FAILED (seed $Seed):"
    $failures | ForEach-Object { Write-Host "  $_" }
    exit 1
}
Write-Host "== OK (seed $Seed)"
