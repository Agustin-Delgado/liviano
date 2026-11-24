<#
.SYNOPSIS
  Measures the frame rate and the frame pacing of each Nea animation.

.DESCRIPTION
  The script starts a new Nea with a temporary NEA_DATA_DIR, script
  control on a separate pipe and NEA_TRACE=1. The session has -Tabs tabs
  in vertical layout and two tab groups, as a heavy daily session.

  For each animation, the script starts the animation through the bench
  pipe (a command, a hover, a group change) or through a PostMessage to a
  window of this Nea. Then it reads the frame marks (the "frames"
  request). Each animated window puts one mark for each frame that it
  draws. The script calculates, for each animation, the frames per
  second, the interval between frames (p50, p95, maximum) and the dropped
  frames (an interval longer than 1.5 times the -TargetMs).

  The script also sends WM_MOUSEMOVE messages over the tabs and the group
  headers, and reads from trace.log how long each mouse move took.

  The script sends no keys or clicks to the system. It does not move a
  window to the front.

.EXAMPLE
  tools\fps.ps1 -Exe C:\Temp\neafps\release\nea.exe -Repeat 5
#>
param(
    [string]$Exe = '',
    [int]$Repeat = 5,
    [int]$Tabs = 40,
    [double]$TargetMs = 16.667,
    [string]$Report = '',
    # Only the animations whose names match (wildcards); the mouse and
    # click passes run only without it.
    [string[]]$Only = @(),
    [switch]$KeepData,
    # Exit with code 1 when hang.log has a handler of 50 ms or more.
    [switch]$FailOnStall,
    # Print each dropped frame with its time (ms from start, as in trace.log).
    [switch]$ShowGaps,
    # No network outside this computer: the engine resolves only 127.0.0.1,
    # the block lists stay the built-in ones and the updater does not check.
    # The pages of other hosts become error pages.
    [switch]$Offline,
    # Bookmarks in bookmarks.json: with some, the bookmarks bar shows.
    [int]$Bookmarks = 0
)

$ErrorActionPreference = 'Stop'
$script:exitCode = 0
. (Join-Path $PSScriptRoot 'bench.ps1')
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Exe) { $Exe = Join-Path $root 'target\release\nea.exe' }
$Exe = (Resolve-Path $Exe).Path

if (-not ('NeaWin' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class NeaWin {
    delegate bool EnumProc(IntPtr hwnd, IntPtr lparam);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc proc, IntPtr lparam);
    [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr parent, EnumProc proc, IntPtr lparam);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, StringBuilder name, int max);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hwnd, uint msg, IntPtr wparam, IntPtr lparam);
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr hwnd, uint msg, IntPtr wparam, IntPtr lparam);

    static string ClassOf(IntPtr hwnd) {
        var sb = new StringBuilder(256);
        GetClassName(hwnd, sb, sb.Capacity);
        return sb.ToString();
    }

    // The top-level windows of the process with the class, visible or not.
    public static IntPtr[] TopLevel(uint pid, string cls) {
        var found = new List<IntPtr>();
        EnumWindows((h, _) => {
            uint owner;
            GetWindowThreadProcessId(h, out owner);
            if (owner == pid && ClassOf(h) == cls) found.Add(h);
            return true;
        }, IntPtr.Zero);
        return found.ToArray();
    }

    public static IntPtr Child(IntPtr parent, string cls) {
        IntPtr hit = IntPtr.Zero;
        EnumChildWindows(parent, (h, _) => {
            if (ClassOf(h) == cls) { hit = h; return false; }
            return true;
        }, IntPtr.Zero);
        return hit;
    }

    public static bool Visible(IntPtr hwnd) { return IsWindowVisible(hwnd); }

    [DllImport("user32.dll")] static extern uint GetGuiResources(IntPtr process, uint flags);
    // GDI (0) and USER (1) objects of a process.
    public static uint Gdi(IntPtr process) { return GetGuiResources(process, 0); }
    public static uint User(IntPtr process) { return GetGuiResources(process, 1); }

    public static IntPtr Point(int x, int y) { return (IntPtr)(((y & 0xFFFF) << 16) | (x & 0xFFFF)); }
}
'@
}

$WM_MOUSEMOVE = 0x200
$WM_KEYDOWN = 0x100
$VK_ESCAPE = 0x1B

function Stats([double[]]$values) {
    if ($values.Count -eq 0) { return $null }
    $sorted = $values | Sort-Object
    $at = { param($p) $sorted[[Math]::Min($sorted.Count - 1, [Math]::Floor($p * ($sorted.Count - 1) + 0.5))] }
    [pscustomobject]@{ p50 = (& $at 0.5); p95 = (& $at 0.95); max = $sorted[-1]; count = $values.Count }
}

# --- start a run ---

$data = Join-Path ([IO.Path]::GetTempPath()) ('neafps-data-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $data | Out-Null
$settings = @{ bench_enabled = $true; field_position = 'middle'; hover_preview = $true; check_updates = $false }
[IO.File]::WriteAllText((Join-Path $data 'settings.json'), ($settings | ConvertTo-Json -Compress))
$oldArgs = $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS
if ($Offline) {
    New-Item -ItemType Directory -Path (Join-Path $data 'shield') | Out-Null
    [IO.File]::WriteAllText((Join-Path $data 'shield\updated'), '')
    $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = '--host-resolver-rules="MAP * ~NOTFOUND, EXCLUDE 127.0.0.1"'
}
$saved = @()
for ($i = 0; $i -lt $Tabs; $i++) {
    $tab = @{ url = "https://example.com/?tab=$i"; title = "Example tab $i"; pinned = $false }
    if ($i -ge 4 -and $i -lt 10) { $tab.group = 0 }
    if ($i -ge 12 -and $i -lt 20) { $tab.group = 1 }
    $saved += $tab
}
$session = @{
    tabs = $saved; active = 0; vertical_tabs = $true; sidebar_collapsed = $false
    window = @(80, 80, 1480, 980); maximized = $false
    groups = @(@{ name = 'Work'; color = 'blue'; collapsed = $false }, @{ name = 'Read later'; color = 'green'; collapsed = $false })
}
[IO.File]::WriteAllText((Join-Path $data 'session.json'), ($session | ConvertTo-Json -Compress -Depth 6))

if ($Bookmarks -gt 0) {
    $marks = @(); for ($i = 0; $i -lt $Bookmarks; $i++) { $marks += @{ url = "https://example.com/mark-$i"; title = "Bookmark $i"; added = $i } }
    [IO.File]::WriteAllText((Join-Path $data 'bookmarks.json'), (ConvertTo-Json -InputObject $marks -Compress))
}
$env:NEA_DATA_DIR = $data
$env:NEA_BENCH = '1'
$env:NEA_TRACE = '1'
$env:NEA_SCALE = '100'
$env:NEA_CREDENTIAL_PREFIX = 'nea-fps-test'
$env:NEA_BENCH_PIPE = 'fps-' + [Guid]::NewGuid().ToString('N').Substring(0, 6)
$process = Start-Process -FilePath $Exe -PassThru
try {
    Connect-Nea -TimeoutSec 20
    $ping = Invoke-Nea ping
    if ($ping.pid -ne $process.Id) { throw "A different process ($($ping.pid)) answers on the pipe." }
    $main = $null
    for ($i = 0; $i -lt 50 -and -not $main; $i++) {
        $main = [NeaWin]::TopLevel([uint32]$process.Id, 'NeaWindow') | Select-Object -First 1
        if (-not $main) { Start-Sleep -Milliseconds 200 }
    }
    if (-not $main) { throw 'The main window of Nea is not there.' }
    Start-Sleep -Seconds 4
    $gdiBefore = [NeaWin]::Gdi($process.Handle)
    $userBefore = [NeaWin]::User($process.Handle)

    $results = New-Object System.Collections.Generic.List[object]
    $moves = New-Object System.Collections.Generic.List[double]

    function Measure-Animation([string]$name, [scriptblock]$trigger, [int]$settleMs = 700, [scriptblock]$after = $null) {
        if ($Only.Count -and -not ($Only | Where-Object { $name -like $_ })) { return }
        $intervals = New-Object System.Collections.Generic.List[double]
        $byLabel = @{}
        for ($r = 0; $r -lt $Repeat; $r++) {
            Invoke-Nea frames | Out-Null
            & $trigger
            Start-Sleep -Milliseconds $settleMs
            $marks = (Invoke-Nea frames).marks
            if ($after) { & $after; Start-Sleep -Milliseconds 500; Invoke-Nea frames | Out-Null }
            $groups = @{}
            foreach ($m in $marks) {
                $label = [string]$m[0]
                # The loading ring turns at its own, slower rate (SPIN_FRAME_MS).
                if ($label -eq 'ring') { continue }
                if (-not $groups.ContainsKey($label)) { $groups[$label] = New-Object System.Collections.Generic.List[double] }
                $groups[$label].Add([double]$m[1] / 1000.0)
            }
            foreach ($label in $groups.Keys) {
                $t = $groups[$label]
                if (-not $byLabel.ContainsKey($label)) { $byLabel[$label] = 0 }
                $byLabel[$label] += $t.Count
                for ($k = 1; $k -lt $t.Count; $k++) {
                    $gap = $t[$k] - $t[$k - 1]
                    # A pause this long is a second motion, not a slow frame.
                    if ($gap -lt 150) {
                        $intervals.Add($gap)
                        if ($ShowGaps -and $gap -gt 1.5 * $TargetMs) { Write-Host ("    gap {0:N1} ms before {1:N1} [{2}]" -f $gap, $t[$k], $label) }
                    }
                }
            }
        }
        $s = Stats $intervals.ToArray()
        if (-not $s) {
            Write-Host ("  {0,-26} no frames" -f $name)
            return
        }
        $dropped = @($intervals | Where-Object { $_ -gt 1.5 * $TargetMs }).Count
        $mean = ($intervals | Measure-Object -Average).Average
        $row = [pscustomobject]@{
            animation = $name
            labels = (($byLabel.Keys | Sort-Object) -join ',')
            fps = [Math]::Round(1000.0 / $mean, 1)
            p50_ms = [Math]::Round($s.p50, 2)
            p95_ms = [Math]::Round($s.p95, 2)
            max_ms = [Math]::Round($s.max, 2)
            intervals = $s.count
            dropped = $dropped
        }
        $results.Add($row)
        Write-Host ("  {0,-26} {1,6} fps  p50 {2,6} ms  p95 {3,6} ms  max {4,7} ms  dropped {5,3}/{6}  [{7}]" -f $name, $row.fps, $row.p50_ms, $row.p95_ms, $row.max_ms, $dropped, $s.count, $row.labels)
    }

    function Tab-Rects { @((Invoke-Nea state).tabs | Where-Object { $_.rect }) }
    function Field-Edit {
        foreach ($popup in [NeaWin]::TopLevel([uint32]$process.Id, 'NeaOmnibar')) {
            $edit = [NeaWin]::Child($popup, 'Edit')
            if ($edit -ne [IntPtr]::Zero) { return $edit }
        }
        [IntPtr]::Zero
    }
    function Close-Field {
        $edit = Field-Edit
        if ($edit -ne [IntPtr]::Zero) { [void][NeaWin]::PostMessage($edit, $WM_KEYDOWN, [IntPtr]$VK_ESCAPE, [IntPtr]1) }
    }
    function Move-Mouse([int]$x, [int]$y) { [void][NeaWin]::PostMessage($main, $WM_MOUSEMOVE, [IntPtr]0, [NeaWin]::Point($x, $y)) }
    function Center($rect) { @([int](($rect[0] + $rect[2]) / 2), [int](($rect[1] + $rect[3]) / 2)) }

    $state = Invoke-Nea state
    $rects = Tab-Rects
    Write-Host ("Layout {0}, {1} tabs, {2} groups, window {3}" -f $state.layout, $state.tabs.Count, @($state.groups).Count, $main)
    Write-Host 'Animations:'

    # The live tab's ground and the indicator move to the next tab.
    Measure-Animation 'live card (next tab)' { Invoke-Nea run @{ name = 'next_tab' } | Out-Null } 500
    Measure-Animation 'live card (prev tab)' { Invoke-Nea run @{ name = 'prev_tab' } | Out-Null } 500
    # The hover fades of the tabs (150 ms each way).
    $t1 = Center $rects[2].rect
    $t2 = Center $rects[3].rect
    Measure-Animation 'tab hover fade' { Invoke-Nea hover @{ x = $t1[0]; y = $t1[1] } | Out-Null; Start-Sleep -Milliseconds 60; Invoke-Nea hover @{ x = $t2[0]; y = $t2[1] } | Out-Null } 400 { Invoke-Nea hover @{ x = -1; y = -1 } | Out-Null }
    # A group folds and unfolds.
    $group = @($state.groups)[0].id
    Measure-Animation 'group collapse' { Invoke-Nea group @{ group = $group; collapsed = $true } | Out-Null } 500
    Measure-Animation 'group expand' { Invoke-Nea group @{ group = $group; collapsed = $false } | Out-Null } 500
    # The sidebar folds and comes back.
    Measure-Animation 'sidebar hide' { Invoke-Nea run @{ name = 'toggle_sidebar' } | Out-Null } 600 { Invoke-Nea run @{ name = 'toggle_sidebar' } | Out-Null }
    Measure-Animation 'sidebar show' { Invoke-Nea run @{ name = 'toggle_sidebar' } | Out-Null; Start-Sleep -Milliseconds 600; Invoke-Nea frames | Out-Null; Invoke-Nea run @{ name = 'toggle_sidebar' } | Out-Null } 600
    # Neighbours make room for a new tab, and close up after it.
    Measure-Animation 'tab open (shift)' { $script:opened = (Invoke-Nea open @{ url = 'about:blank'; background = $true }).tab } 500 { Invoke-Nea close @{ tab = $script:opened } | Out-Null }
    # The field grows in from its snapshot and shrinks out.
    Measure-Animation 'field open' { Invoke-Nea run @{ name = 'focus_address' } | Out-Null } 600 { Close-Field }
    Measure-Animation 'field close' { Invoke-Nea run @{ name = 'focus_address' } | Out-Null; Start-Sleep -Milliseconds 600; Invoke-Nea frames | Out-Null; Close-Field } 500
    Measure-Animation 'history panel open' { Invoke-Nea run @{ name = 'history' } | Out-Null } 600 { Close-Field }
    # The hover card fades in after its delay, moves, and fades out.
    Measure-Animation 'hover card fade' { Invoke-Nea hover @{ x = $t1[0]; y = $t1[1] } | Out-Null; Start-Sleep -Milliseconds 700; Invoke-Nea hover @{ x = -1; y = -1 } | Out-Null } 400
    Measure-Animation 'settings fade' { Invoke-Nea run @{ name = 'settings' } | Out-Null } 500 {
        foreach ($w in [NeaWin]::TopLevel([uint32]$process.Id, 'NeaSettings')) { [void][NeaWin]::PostMessage($w, 0x10, [IntPtr]0, [IntPtr]0) }
    }

    if ($Only.Count) {
        $results | Format-Table -AutoSize | Out-String | Write-Host
        Write-Host ("Handles: GDI {0} -> {1}, USER {2} -> {3}" -f $gdiBefore, [NeaWin]::Gdi($process.Handle), $userBefore, [NeaWin]::User($process.Handle))
        return
    }
    # --- mouse moves, as the real WM_MOUSEMOVE path ---
    Write-Host 'Mouse moves over the strip:'
    $traceBefore = 0
    $tracePath = Join-Path $data 'trace.log'
    Start-Sleep -Milliseconds 1300
    if (Test-Path $tracePath) { $traceBefore = @(Get-Content -LiteralPath $tracePath).Count }
    $rects = Tab-Rects
    $sent = 0
    for ($pass = 0; $pass -lt 3; $pass++) {
        foreach ($t in $rects) {
            $c = Center $t.rect
            for ($dy = -6; $dy -le 6; $dy += 3) {
                Move-Mouse $c[0] ($c[1] + $dy)
                $sent++
                Start-Sleep -Milliseconds 8
            }
        }
    }
    Invoke-Nea state | Out-Null
    Start-Sleep -Milliseconds 1500
    $slow = @()
    if (Test-Path $tracePath) {
        $slow = @(Get-Content -LiteralPath $tracePath | Select-Object -Skip $traceBefore | Where-Object { $_ -match 'slow message 512 ([\d.]+) ms' } | ForEach-Object { [double]$Matches[1] })
    }
    $slowest = if ($slow.Count) { ($slow | Measure-Object -Maximum).Maximum } else { 0 }
    Write-Host ("  {0} moves sent, {1} took 4 ms or more, slowest {2} ms" -f $sent, $slow.Count, $slowest)

    # --- clicks on sleeping tabs (WM_LBUTTONDOWN = 513, as in hang.log) ---
    Write-Host 'Clicks on sleeping tabs:'
    if (Test-Path $tracePath) { $traceBefore = @(Get-Content -LiteralPath $tracePath).Count }
    $asleep = @((Invoke-Nea state).tabs | Where-Object { $_.rect -and $_.live -eq 'asleep' } | Select-Object -First 8)
    Invoke-Nea frames | Out-Null
    $clickIntervals = New-Object System.Collections.Generic.List[double]
    foreach ($t in $asleep) {
        $c = Center $t.rect
        Move-Mouse $c[0] $c[1]
        [void][NeaWin]::PostMessage($main, 0x201, [IntPtr]1, [NeaWin]::Point($c[0], $c[1]))
        [void][NeaWin]::PostMessage($main, 0x202, [IntPtr]0, [NeaWin]::Point($c[0], $c[1]))
        Start-Sleep -Milliseconds 900
        $marks = @((Invoke-Nea frames).marks | Where-Object { $_[0] -eq 'strip' })
        for ($k = 1; $k -lt $marks.Count; $k++) {
            $gap = ([double]$marks[$k][1] - [double]$marks[$k - 1][1]) / 1000.0
            if ($gap -lt 150) { $clickIntervals.Add($gap) }
        }
    }
    Start-Sleep -Milliseconds 1500
    $clicks = @()
    if (Test-Path $tracePath) {
        $clicks = @(Get-Content -LiteralPath $tracePath | Select-Object -Skip $traceBefore | Where-Object { $_ -match 'slow message 513 ([\d.]+) ms' } | ForEach-Object { [double]$Matches[1] })
    }
    $s = Stats $clicks
    if ($s) { Write-Host ("  {0} clicks, button down took p50 {1} ms, max {2} ms" -f $asleep.Count, $s.p50, $s.max) } else { Write-Host ("  {0} clicks, none took 4 ms or more" -f $asleep.Count) }
    $s = Stats $clickIntervals.ToArray()
    if ($s) { Write-Host ("  live card after a click: p50 {0:N2} ms, p95 {1:N2} ms, max {2:N2} ms over {3} intervals" -f $s.p50, $s.p95, $s.max, $s.count) }
    $hang = Join-Path $data 'hang.log'
    $stalls = @()
    if (Test-Path $hang) {
        $stalls = @(Get-Content -LiteralPath $hang | Where-Object { $_ -match ' slow: ' })
        Write-Host 'hang.log:'; Get-Content -LiteralPath $hang | Select-Object -Last 20 | ForEach-Object { Write-Host "  $_" }
    }

    # --- the window at rest: CPU, memory and handles after the whole run ---
    $engine = { @(Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($data) } | ForEach-Object { Get-Process -Id $_.ProcessId -ErrorAction SilentlyContinue }) }
    Start-Sleep -Seconds 5
    $process.Refresh()
    $appCpu = $process.TotalProcessorTime.TotalMilliseconds
    $engineCpu = (& $engine | ForEach-Object { $_.TotalProcessorTime.TotalMilliseconds } | Measure-Object -Sum).Sum
    $idleSec = 10
    Start-Sleep -Seconds $idleSec
    $process.Refresh()
    $appIdle = ($process.TotalProcessorTime.TotalMilliseconds - $appCpu) / ($idleSec * 10.0)
    $engineIdle = ((& $engine | ForEach-Object { $_.TotalProcessorTime.TotalMilliseconds } | Measure-Object -Sum).Sum - $engineCpu) / ($idleSec * 10.0)
    $engineMb = ((& $engine | Measure-Object -Property PrivateMemorySize64 -Sum).Sum) / 1MB
    $live = @((Invoke-Nea state).tabs | Where-Object { $_.live -ne 'asleep' }).Count
    Write-Host 'At rest:'
    Write-Host ("  CPU over {0} s: app {1:N2} %, engine {2:N2} % (of one core)" -f $idleSec, $appIdle, $engineIdle)
    Write-Host ("  memory: app {0:N1} MB private, engine {1:N1} MB private, {2} tabs awake" -f ($process.PrivateMemorySize64 / 1MB), $engineMb, $live)
    Write-Host ("  handles: GDI {0} -> {1}, USER {2} -> {3} (after startup -> after the run)" -f $gdiBefore, [NeaWin]::Gdi($process.Handle), $userBefore, [NeaWin]::User($process.Handle))

    if ($Report) { $results | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $Report -Encoding utf8 }
    $results | Format-Table -AutoSize | Out-String | Write-Host
    if ($FailOnStall -and $stalls.Count) { $script:exitCode = 1; Write-Host "$($stalls.Count) handlers took 50 ms or more." }
} finally {
    try { Invoke-Nea quit | Out-Null } catch { }
    Disconnect-Nea
    if (-not $process.WaitForExit(15000)) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 500
    foreach ($p in @(Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($data) })) {
        Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue
    }
    Remove-Item Env:NEA_DATA_DIR, Env:NEA_BENCH, Env:NEA_BENCH_PIPE, Env:NEA_TRACE, Env:NEA_SCALE, Env:NEA_CREDENTIAL_PREFIX -ErrorAction SilentlyContinue
    if ($Offline) {
        if ($null -eq $oldArgs) { Remove-Item Env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS -ErrorAction SilentlyContinue }
        else { $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = $oldArgs }
    }
    if (-not $KeepData) { Remove-Item -Recurse -Force -LiteralPath $data -ErrorAction SilentlyContinue } else { Write-Host "Data: $data" }
}
if ($script:exitCode) { exit $script:exitCode }
