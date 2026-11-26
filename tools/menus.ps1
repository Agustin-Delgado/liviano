<#
.SYNOPSIS
  Measures the menus that Nea draws: the memory of the app process at
  rest, before and after many open and close cycles, and the time from
  the click to the first frame of a menu.

.DESCRIPTION
  The script starts a new Nea with a temporary NEA_DATA_DIR and script
  control on a separate pipe. It sends no keys or clicks to the system and
  takes no focus: it uses the `menu` request of the pipe, which posts the
  click to the window of the Nea that it started.

  One cycle opens the menu of a tab, moves the highlight, opens a submenu
  with the keyboard and closes the menu with Escape. After the cycles, no
  window and no bitmap of a menu can be alive.

  With -Shots, the script also saves captures of some menus to that
  folder, in the light and in the dark theme (tools\capture.ps1).

  With -Check, the script checks how the menus work: their items, the
  highlight under the pointer, the delay of a submenu, the keys, a click
  on an item and the command that it runs. The pointer is not the real
  one: the script posts mouse messages to the menu windows of the Nea
  that it started. The script exits with code 1 if a check fails.

  An executable without the `menu` request (a build before the menus)
  gives only the memory at rest.

.EXAMPLE
  tools\menus.ps1 -Exe C:\Temp\nea-menus\release\nea.exe -Cycles 200
.EXAMPLE
  tools\menus.ps1 -Exe C:\Temp\nea-menus\release\nea.exe -Cycles 0 -Shots C:\Temp\nea-menus-shots
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Exe,
    [int]$Cycles = 200,
    [int]$SettleSec = 6,
    [string]$Shots = '',
    [string]$Pipe = 'menus',
    # 125 or 150: the scale of the interface (NEA_SCALE).
    [int]$Scale = 0,
    # A file for the trace of the measured run (NEA_TRACE=1): each menu
    # writes how long each step of its first frame took.
    [string]$Trace = '',
    # Checks how the menus work: the items, the pointer (mouse messages to
    # the menu windows of this Nea), the keyboard and the commands.
    [switch]$Check,
    [string]$Out = ''
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'bench.ps1')
$Exe = (Resolve-Path $Exe).Path

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class NeaMenuProbe {
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
    [DllImport("kernel32.dll")] static extern bool GetProcessHandleCount(IntPtr process, out uint count);
    [DllImport("psapi.dll")] static extern bool GetProcessMemoryInfo(IntPtr process, ref Counters counters, uint cb);
    [DllImport("user32.dll")] static extern uint GetGuiResources(IntPtr process, uint flags);
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);
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
}
'@

function Read-App([uint32]$id, [int]$samples = 5) {
    # The median of some samples: the working set moves a little.
    $rows = @(1..$samples | ForEach-Object { Start-Sleep -Milliseconds 400; , ([NeaMenuProbe]::Read($id)) })
    $median = { param($i) (@($rows | ForEach-Object { $_[$i] }) | Sort-Object)[[int]($samples / 2)] }
    [pscustomobject]@{
        private_ws_kb = [int]((& $median 0) / 1KB)
        commit_kb     = [int]((& $median 1) / 1KB)
        gdi           = [int](& $median 2)
        user          = [int](& $median 3)
        handles       = [int](& $median 4)
    }
}

function Wait-Menu([scriptblock]$until, [int]$timeoutMs = 3000) {
    $deadline = (Get-Date).AddMilliseconds($timeoutMs)
    while ($true) {
        $state = Invoke-Nea menu @{ op = 'state' }
        if (& $until $state) { return $state }
        if ((Get-Date) -gt $deadline) { throw "The menu did not reach the state in $timeoutMs ms: $($state | ConvertTo-Json -Compress -Depth 3)" }
        Start-Sleep -Milliseconds 5
    }
}

function Open-Menu([int]$x, [int]$y, [switch]$Left) {
    Invoke-Nea menu @{ op = 'open'; x = $x; y = $y; left = [bool]$Left } | Out-Null
    Wait-Menu { param($s) $s.open }
}

function Close-Menu {
    Invoke-Nea menu @{ op = 'close' } | Out-Null
    Wait-Menu { param($s) -not $s.open -and -not $s.fading -and $s.windows -eq 0 -and $s.bitmaps -eq 0 } | Out-Null
}

function Start-Nea([string]$theme) {
    $data = Join-Path ([IO.Path]::GetTempPath()) ('nea-menus-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
    New-Item -ItemType Directory -Path $data | Out-Null
    $settings = @{ theme = $theme }
    [IO.File]::WriteAllText((Join-Path $data 'settings.json'), ($settings | ConvertTo-Json), (New-Object System.Text.UTF8Encoding($false)))
    $env:NEA_DATA_DIR = $data
    $env:NEA_BENCH = '1'
    $env:NEA_BENCH_PIPE = $Pipe
    if ($Scale -gt 0) { $env:NEA_SCALE = "$Scale" } else { Remove-Item Env:NEA_SCALE -ErrorAction SilentlyContinue }
    if ($Trace) { $env:NEA_TRACE = '1' } else { Remove-Item Env:NEA_TRACE -ErrorAction SilentlyContinue }
    $process = Start-Process -FilePath $Exe -PassThru
    Connect-Nea -Pipe (Get-NeaPipeName -Suffix $Pipe)
    [pscustomobject]@{ Process = $process; Data = $data }
}

function Stop-Nea($nea) {
    try { Invoke-Nea quit | Out-Null } catch { }
    Disconnect-Nea
    if (-not $nea.Process.WaitForExit(8000)) { Stop-Process -Id $nea.Process.Id -Force -ErrorAction SilentlyContinue }
    $log = Join-Path $nea.Data 'trace.log'
    if ($Trace -and (Test-Path $log)) { Copy-Item -Force $log $Trace }
    for ($i = 0; $i -lt 10; $i++) {
        try { Remove-Item -Recurse -Force -LiteralPath $nea.Data -ErrorAction Stop; break } catch { Start-Sleep -Milliseconds 500 }
    }
}

function Test-MenuRequest {
    $reply = Invoke-Nea menu @{ op = 'state' } -NoThrow
    [bool]$reply.ok
}

function Get-Center($rect) { @([int](($rect[0] + $rect[2]) / 2), [int](($rect[1] + $rect[3]) / 2)) }

# ---------- the measurement ----------

$result = [ordered]@{ exe = $Exe; cycles = $Cycles }
$nea = Start-Nea 'light'
try {
    $tab = (Invoke-Nea open @{ url = 'about:blank' }).tab
    Invoke-Nea wait @{ tab = $tab; timeout_ms = 20000 } | Out-Null
    Start-Sleep -Seconds $SettleSec
    $id = [uint32]$nea.Process.Id
    $result.at_rest = Read-App $id
    Write-Host ("at rest:      private WS {0} KB, commit {1} KB, GDI {2}, USER {3}, handles {4}" -f $result.at_rest.private_ws_kb, $result.at_rest.commit_kb, $result.at_rest.gdi, $result.at_rest.user, $result.at_rest.handles)
    if (-not (Test-MenuRequest)) {
        Write-Host 'This executable has no menu request: only the memory at rest is measured.'
    } elseif ($Cycles -gt 0) {
        $rect = (Invoke-Nea state).tabs[0].rect
        $x, $y = Get-Center $rect
        $first = New-Object System.Collections.Generic.List[double]
        $click = New-Object System.Collections.Generic.List[double]
        $trend = New-Object System.Collections.Generic.List[object]
        for ($i = 1; $i -le $Cycles; $i++) {
            $state = Open-Menu $x $y
            $first.Add($state.first_frame_us / 1000.0)
            $click.Add($state.click_to_frame_us / 1000.0)
            # The highlight goes down, a submenu opens with the keyboard.
            Invoke-Nea menu @{ op = 'key'; key = 'down' } | Out-Null
            Invoke-Nea menu @{ op = 'key'; key = 'end' } | Out-Null
            $items = $state.levels[0].items
            $sub = 0..($items.Count - 1) | Where-Object { $items[$_].submenu -and $items[$_].enabled } | Select-Object -First 1
            if ($null -ne $sub) {
                Invoke-Nea menu @{ op = 'hover'; level = 0; item = $sub } | Out-Null
                Invoke-Nea menu @{ op = 'key'; key = 'right' } | Out-Null
            }
            Close-Menu
            if ($i -eq 1) {
                Start-Sleep -Seconds 2
                $result.after_first = Read-App $id 3
            }
            # A line for each quarter of the run: memory that goes up from
            # one line to the next, all the time, is a leak.
            if ($i % [Math]::Max(1, [int]($Cycles / 4)) -eq 0) {
                $now = Read-App $id 3
                $trend.Add([ordered]@{ cycle = $i; private_ws_kb = $now.private_ws_kb; commit_kb = $now.commit_kb; gdi = $now.gdi; user = $now.user; handles = $now.handles })
                Write-Host ("cycle {0,4}:   private WS {1} KB, commit {2} KB, GDI {3}, USER {4}, handles {5}" -f $i, $now.private_ws_kb, $now.commit_kb, $now.gdi, $now.user, $now.handles)
            }
        }
        Start-Sleep -Seconds $SettleSec
        $result.after_cycles = Read-App $id
        $final = Invoke-Nea menu @{ op = 'state' }
        $sorted = @($first | Sort-Object)
        $sortedClick = @($click | Sort-Object)
        $pick = { param($list, $q) $list[[Math]::Min($list.Count - 1, [int]($list.Count * $q))] }
        $result.first_frame_ms = [ordered]@{
            median = [Math]::Round((& $pick $sorted 0.5), 2)
            p95    = [Math]::Round((& $pick $sorted 0.95), 2)
            worst  = [Math]::Round($sorted[-1], 2)
            first  = [Math]::Round($first[0], 2)
        }
        $result.click_to_frame_ms = [ordered]@{
            median = [Math]::Round((& $pick $sortedClick 0.5), 2)
            p95    = [Math]::Round((& $pick $sortedClick 0.95), 2)
            worst  = [Math]::Round($sortedClick[-1], 2)
            first  = [Math]::Round($click[0], 2)
        }
        $result.trend = $trend.ToArray()
        $result.left_alive = [ordered]@{ windows = $final.windows; bitmaps = $final.bitmaps; opens = $final.opens }
        Write-Host ("after 1:      private WS {0} KB, commit {1} KB" -f $result.after_first.private_ws_kb, $result.after_first.commit_kb)
        Write-Host ("after {0}:    private WS {1} KB, commit {2} KB, GDI {3}, USER {4}, handles {5}" -f $Cycles, $result.after_cycles.private_ws_kb, $result.after_cycles.commit_kb, $result.after_cycles.gdi, $result.after_cycles.user, $result.after_cycles.handles)
        Write-Host ("first frame after the call:  median {0} ms, p95 {1} ms, worst {2} ms (the first open: {3} ms)" -f $result.first_frame_ms.median, $result.first_frame_ms.p95, $result.first_frame_ms.worst, $result.first_frame_ms.first)
        Write-Host ("first frame after the click: median {0} ms, p95 {1} ms, worst {2} ms (the first open: {3} ms)" -f $result.click_to_frame_ms.median, $result.click_to_frame_ms.p95, $result.click_to_frame_ms.worst, $result.click_to_frame_ms.first)
        Write-Host ("left alive: {0} windows, {1} bitmaps after {2} menus" -f $final.windows, $final.bitmaps, $final.opens)
    }
} finally {
    Stop-Nea $nea
}

# ---------- the checks ----------

if ($Check) {
    $failed = New-Object System.Collections.Generic.List[string]
    $passed = 0
    function Assert-That([bool]$condition, [string]$what) {
        if ($condition) { $script:passed++ } else { $script:failed.Add($what); Write-Host "FAILED: $what" }
    }
    $WM_MOUSEMOVE = 0x0200; $WM_LBUTTONDOWN = 0x0201; $WM_LBUTTONUP = 0x0202; $WM_RBUTTONUP = 0x0205
    # A mouse message to the window of a level, at the middle of an item:
    # the path of a real pointer in the menu, without the pointer.
    function Send-Mouse($level, [int]$item, [uint32]$message) {
        $x, $y = Get-Center $level.items[$item].rect
        $x -= $level.window[0]; $y -= $level.window[1]
        $at = [IntPtr](($y -shl 16) -bor ($x -band 0xFFFF))
        [NeaMenuProbe]::PostMessageW([IntPtr][int64]$level.hwnd, $message, [IntPtr]::Zero, $at) | Out-Null
    }
    function Find-Item($level, [string]$label) {
        0..($level.items.Count - 1) | Where-Object { $level.items[$_].label -eq $label } | Select-Object -First 1
    }
    function Get-TabPoint { Get-Center (((Invoke-Nea state).tabs | Where-Object { $_.rect } | Select-Object -Last 1).rect) }

    $nea = Start-Nea 'light'
    try {
        $one = (Invoke-Nea open @{ url = 'about:blank' }).tab
        $two = (Invoke-Nea open @{ url = 'about:blank' }).tab
        Invoke-Nea wait @{ tab = $two; timeout_ms = 20000 } | Out-Null
        if (-not (Test-MenuRequest)) { throw 'This executable has no menu request.' }
        $group = (Invoke-Nea group @{ tabs = @($one); name = 'Research'; color = 'green' }).group
        if ((Invoke-Nea state).layout -ne 'vertical') { Invoke-Nea run @{ name = 'toggle_tab_layout' } | Out-Null }
        Invoke-Nea run @{ name = 'new_space' } | Out-Null
        Start-Sleep -Milliseconds 500
        Invoke-Nea type @{ text = 'Work'; submit = $true } | Out-Null
        Start-Sleep -Milliseconds 500
        Invoke-Nea run @{ name = 'prev_space' } | Out-Null
        Start-Sleep -Milliseconds 1200

        # The menu of a tab opens with its items and its hooks.
        $x, $y = Get-TabPoint
        $menu = Open-Menu $x $y
        $root = $menu.levels[0]
        $labels = @($root.items | ForEach-Object { $_.label })
        Assert-That ($menu.levels.Count -eq 1) 'one level opens'
        Assert-That (($labels -contains 'Pin tab') -and ($labels -contains 'Close tab') -and ($labels -contains 'Move to space')) 'the tab menu has its items'
        Assert-That ($menu.hooks[0] -and $menu.hooks[1] -and $menu.hooks[2]) 'the three hooks are set while the menu is open'
        Assert-That ($menu.windows -eq 1 -and $menu.bitmaps -eq 1) 'one window and one bitmap for one level'
        Assert-That ($null -eq $root.hot) 'nothing has the highlight at first'
        $awake = Find-Item $root 'Keep site awake (uses more memory)'
        Assert-That ($root.items[$awake].checked -eq $false) 'an item that is off says so'
        $dup = Find-Item $root 'Duplicate tab'
        Assert-That ($root.items[$dup].shortcut.Length -gt 0) 'a command shows its keys'

        # The release of the click that opened the menu selects nothing.
        $pin = Find-Item $root 'Pin tab'
        Send-Mouse $root $pin $WM_RBUTTONUP
        Start-Sleep -Milliseconds 200
        $menu = Invoke-Nea menu @{ op = 'state' }
        Assert-That $menu.open 'a release with no move leaves the menu open'

        # The pointer on an item moves the highlight; at rest, its submenu opens.
        $move = Find-Item $root 'Move to space'
        Send-Mouse $root $move $WM_MOUSEMOVE
        $menu = Wait-Menu { param($s) $s.levels[0].hot -eq $move }
        Assert-That ($menu.levels.Count -eq 1) 'the submenu does not open at once'
        $menu = Wait-Menu { param($s) $s.levels.Count -eq 2 }
        Assert-That ($menu.levels[0].child -eq $move) 'the submenu opens after the pointer rests'
        $sub = $menu.levels[1]
        Assert-That ($sub.panel[0] -ge $menu.levels[0].panel[2] - 8) 'the submenu is beside its parent'
        Assert-That (($sub.items | Where-Object { $_.label -eq 'Work' }).swatch.Count -eq 3) 'a space shows its colour'
        Send-Mouse $sub 0 $WM_MOUSEMOVE
        $menu = Wait-Menu { param($s) $s.levels.Count -eq 2 -and $s.levels[1].hot -eq 0 }
        Assert-That ($menu.levels[0].hot -eq $move) 'the parent keeps the highlight on the item that leads to the submenu'

        # The pointer back on a different item closes the submenu after the delay.
        Send-Mouse $menu.levels[0] $pin $WM_MOUSEMOVE
        $menu = Wait-Menu { param($s) $s.levels[0].hot -eq $pin }
        $menu = Wait-Menu { param($s) $s.levels.Count -eq 1 }
        Assert-That ($menu.windows -eq 1 -and $menu.bitmaps -eq 1) 'the closed submenu leaves no window and no bitmap'

        # The keyboard.
        $menu = Invoke-Nea menu @{ op = 'key'; key = 'end' }
        Assert-That ($menu.levels[0].items[$menu.levels[0].hot].label -eq 'Close other tabs') 'End goes to the last item'
        $menu = Invoke-Nea menu @{ op = 'key'; key = 'down' }
        Assert-That ($menu.levels[0].hot -eq 0) 'Down goes around to the first item'
        $menu = Invoke-Nea menu @{ op = 'key'; key = 'up' }
        Assert-That ($menu.levels[0].items[$menu.levels[0].hot].label -eq 'Close other tabs') 'Up goes around to the last item'
        $menu = Invoke-Nea menu @{ op = 'key'; key = 'home' }
        $menu = Invoke-Nea menu @{ op = 'key'; key = 'd' }
        Assert-That ($menu.levels[0].hot -eq $dup) 'a letter goes to the item that starts with it'
        $menu = Invoke-Nea menu @{ op = 'key'; key = 'm' }
        $menu = Invoke-Nea menu @{ op = 'key'; key = 'right' }
        Assert-That ($menu.levels.Count -eq 2 -and $menu.levels[1].hot -eq 0) 'Right opens the submenu with its first item highlighted'
        $menu = Invoke-Nea menu @{ op = 'key'; key = 'left' }
        Assert-That ($menu.levels.Count -eq 1 -and $menu.levels[0].hot -eq $move) 'Left closes the submenu'
        $menu = Invoke-Nea menu @{ op = 'key'; key = 'enter' }
        Assert-That ($menu.levels.Count -eq 2) 'Enter opens a submenu'
        $menu = Invoke-Nea menu @{ op = 'key'; key = 'escape' }
        Assert-That ($menu.open -and $menu.levels.Count -eq 1) 'Escape closes one submenu'
        Invoke-Nea menu @{ op = 'key'; key = 'escape' } | Out-Null
        $menu = Wait-Menu { param($s) -not $s.open -and -not $s.fading -and $s.windows -eq 0 }
        Assert-That (-not ($menu.hooks[0] -or $menu.hooks[1] -or $menu.hooks[2])) 'no hook stays after the menu'
        $tab = (Invoke-Nea state).tabs | Where-Object { $_.id -eq $two }
        Assert-That (-not $tab.pinned) 'Escape selects nothing'

        # A click on an item runs its command.
        $x, $y = Get-TabPoint
        $menu = Open-Menu $x $y
        $root = $menu.levels[0]
        Send-Mouse $root (Find-Item $root 'Pin tab') $WM_MOUSEMOVE
        Send-Mouse $root (Find-Item $root 'Pin tab') $WM_LBUTTONDOWN
        Send-Mouse $root (Find-Item $root 'Pin tab') $WM_LBUTTONUP
        Wait-Menu { param($s) -not $s.open } | Out-Null
        Start-Sleep -Milliseconds 300
        $tab = (Invoke-Nea state).tabs | Where-Object { $_.id -eq $two }
        Assert-That ([bool]$tab.pinned) 'a click on "Pin tab" pins the tab'

        # Enter on an item runs its command, and the label follows the state.
        Wait-Menu { param($s) -not $s.fading } | Out-Null
        $x, $y = Get-Center $tab.rect
        $menu = Open-Menu $x $y
        Assert-That ($menu.levels[0].items[0].label -eq 'Unpin tab') 'the menu of a pinned tab says "Unpin tab"'
        Invoke-Nea menu @{ op = 'key'; key = 'home' } | Out-Null
        Invoke-Nea menu @{ op = 'key'; key = 'enter' } | Out-Null
        Wait-Menu { param($s) -not $s.open } | Out-Null
        Start-Sleep -Milliseconds 300
        $tab = (Invoke-Nea state).tabs | Where-Object { $_.id -eq $two }
        Assert-That (-not $tab.pinned) 'Enter on "Unpin tab" unpins the tab'

        # A colour list: nine dots, one check mark, and a click sets the colour.
        Wait-Menu { param($s) -not $s.fading } | Out-Null
        $label = ((Invoke-Nea state).groups | Where-Object { $_.id -eq $group }).label
        $x, $y = Get-Center $label
        $menu = Open-Menu $x $y
        $color = Find-Item $menu.levels[0] 'Color'
        Invoke-Nea menu @{ op = 'hover'; level = 0; item = $color } | Out-Null
        $menu = Invoke-Nea menu @{ op = 'key'; key = 'right' }
        $colors = $menu.levels[1]
        Assert-That ($colors.items.Count -eq 9 -and @($colors.items | Where-Object { $_.swatch }).Count -eq 9) 'the colour list has nine colours with their dots'
        $checked = @($colors.items | Where-Object { $_.checked })
        Assert-That ($checked.Count -eq 1 -and $checked[0].label -eq 'Green') 'the colour of the group has the check mark'
        $red = Find-Item $colors 'Red'
        Send-Mouse $colors $red $WM_MOUSEMOVE
        Send-Mouse $colors $red $WM_LBUTTONDOWN
        Send-Mouse $colors $red $WM_LBUTTONUP
        Wait-Menu { param($s) -not $s.open } | Out-Null
        Start-Sleep -Milliseconds 300
        Assert-That (((Invoke-Nea state).groups | Where-Object { $_.id -eq $group }).color -eq 'red') 'a click on a colour sets it'

        # A disabled item: a click does nothing and the menu stays.
        Wait-Menu { param($s) -not $s.fading } | Out-Null
        $x, $y = Get-TabPoint
        $menu = Open-Menu $x 520
        $root = $menu.levels[0]
        $labels = @($root.items | ForEach-Object { $_.label })
        Assert-That (($labels -contains 'New tab') -and ($labels -contains 'This space')) 'the empty space of the column has its menu'
        $menu = Invoke-Nea menu @{ op = 'close' }
        $final = Wait-Menu { param($s) -not $s.open -and -not $s.fading }
        Assert-That ($final.windows -eq 0 -and $final.bitmaps -eq 0) 'after the menus: no window and no bitmap'
    } finally {
        Stop-Nea $nea
    }
    Write-Host ("checks: {0} passed, {1} failed" -f $passed, $failed.Count)
    $result.checks = [ordered]@{ passed = $passed; failed = @($failed) }
}

# ---------- the captures ----------

if ($Shots) {
    if (-not (Test-Path $Shots)) { New-Item -ItemType Directory -Path $Shots | Out-Null }
    $capture = Join-Path $PSScriptRoot 'capture.ps1'
    foreach ($theme in 'light', 'dark') {
        $nea = Start-Nea $theme
        try {
            $one = (Invoke-Nea open @{ url = 'about:blank' }).tab
            $two = (Invoke-Nea open @{ url = 'about:blank' }).tab
            Invoke-Nea wait @{ tab = $two; timeout_ms = 20000 } | Out-Null
            if (-not (Test-MenuRequest)) { throw 'This executable has no menu request.' }
            $group = (Invoke-Nea group @{ tabs = @($one); name = 'Research'; color = 'green' }).group
            # The tabs at the side: the mark of the space is in the column.
            if ((Invoke-Nea state).layout -ne 'vertical') { Invoke-Nea run @{ name = 'toggle_tab_layout' } | Out-Null }
            # A second space, named in the field, then back to the first.
            Invoke-Nea run @{ name = 'new_space' } | Out-Null
            Start-Sleep -Milliseconds 500
            Invoke-Nea type @{ text = 'Work'; submit = $true } | Out-Null
            Start-Sleep -Milliseconds 500
            Invoke-Nea run @{ name = 'prev_space' } | Out-Null
            # The name of the space stays over the tabs for a moment.
            Start-Sleep -Milliseconds 1800
            $state = Invoke-Nea state
            $id = $nea.Process.Id
            $shot = { param($name) & $capture -ProcessId $id -Out (Join-Path $Shots "$name-$theme.png") | Out-Null }

            # 1. The menu of a tab, with the highlight on an item.
            $tabRect = ($state.tabs | Where-Object { $_.rect } | Select-Object -Last 1).rect
            if ($tabRect) {
                $x, $y = Get-Center $tabRect
                $menu = Open-Menu $x $y
                Invoke-Nea menu @{ op = 'key'; key = 'down' } | Out-Null
                Invoke-Nea menu @{ op = 'key'; key = 'down' } | Out-Null
                & $shot 'tab-menu'
                # 2. The same menu with "Move to space" open.
                $items = $menu.levels[0].items
                $sub = 0..($items.Count - 1) | Where-Object { $items[$_].label -eq 'Move to space' } | Select-Object -First 1
                if ($null -ne $sub) {
                    Invoke-Nea menu @{ op = 'hover'; level = 0; item = $sub } | Out-Null
                    Invoke-Nea menu @{ op = 'key'; key = 'right' } | Out-Null
                    & $shot 'tab-menu-submenu'
                }
                Close-Menu
            }
            # 3. The menu of the column's empty space, with "This space" and
            # its colours open: three levels.
            $x = [int](($tabRect[0] + $tabRect[2]) / 2)
            $menu = Open-Menu $x 420
            $items = $menu.levels[0].items
            $sub = 0..($items.Count - 1) | Where-Object { $items[$_].label -eq 'This space' } | Select-Object -First 1
            if ($null -ne $sub) {
                Invoke-Nea menu @{ op = 'hover'; level = 0; item = $sub } | Out-Null
                $menu = Invoke-Nea menu @{ op = 'key'; key = 'right' }
                $items = $menu.levels[1].items
                $sub = 0..($items.Count - 1) | Where-Object { $items[$_].label -eq 'Color' } | Select-Object -First 1
                Invoke-Nea menu @{ op = 'hover'; level = 1; item = $sub } | Out-Null
                Invoke-Nea menu @{ op = 'key'; key = 'right' } | Out-Null
                Invoke-Nea menu @{ op = 'key'; key = 'down' } | Out-Null
                & $shot 'space-colors'
            }
            Close-Menu
            # 4. The menu of a group label.
            $label = ($state.groups | Where-Object { $_.id -eq $group }).label
            if ($label) {
                $x, $y = Get-Center $label
                Open-Menu $x $y | Out-Null
                Invoke-Nea menu @{ op = 'key'; key = 'down' } | Out-Null
                & $shot 'group-menu'
                Close-Menu
            }
        } finally {
            Stop-Nea $nea
        }
    }
    Write-Host "captures: $Shots"
}

if ($Out) {
    [IO.File]::WriteAllText($Out, ($result | ConvertTo-Json -Depth 6), (New-Object System.Text.UTF8Encoding($false)))
}
$result | ConvertTo-Json -Depth 6
if ($Check -and $result.checks.failed.Count -gt 0) { exit 1 }
