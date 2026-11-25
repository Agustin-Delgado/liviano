<#
.SYNOPSIS
  Measures the frames of the space switch under fast key presses.

.DESCRIPTION
  The script starts a new Nea with a temporary NEA_DATA_DIR, script
  control on a separate pipe and NEA_TRACE=1. The session has 3 spaces of
  -TabsPerSpace tabs each, in the vertical layout. The first tab of each
  space is a real page.

  A warm-up visits each space once, so that each space's live page is
  made and loaded. Then the script runs one pattern of next_space and
  prev_space commands through the bench pipe:

    slow     one switch every 600 ms: each slide ends before the next.
    rapid    next_space every 30 to 80 ms (random) for -Seconds.
    reverse  next_space, then prev_space 40 to 90 ms later, mid-slide,
             for -Seconds.

  It reports, for the pattern: the frame gaps of the strip (the "strip"
  frame marks; a gap of 150 ms or more is a pause, not a frame), the
  frame clock's stats from "state" (worst frame, frames over 16 ms, gaps)
  and the time of each command from trace.log.

  The script sends no keys or clicks to the system. It does not move a
  window to the front.

.EXAMPLE
  tools\spaces.ps1 -Exe target\release\nea.exe -Pattern rapid
#>
param(
    [string]$Exe = '',
    [ValidateSet('slow', 'rapid', 'reverse')][string]$Pattern = 'rapid',
    [int]$Seconds = 4,
    [int]$TabsPerSpace = 5,
    # The display's frame time: 8.333 on a 120 Hz screen.
    [double]$TargetMs = 16.667,
    [int]$Seed = 7,
    [switch]$ShowGaps,
    [switch]$KeepData
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'bench.ps1')
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Exe) { $Exe = Join-Path $root 'target\release\nea.exe' }
$Exe = (Resolve-Path $Exe).Path

function Stats([double[]]$values) {
    if ($values.Count -eq 0) { return $null }
    $sorted = $values | Sort-Object
    $at = { param($p) $sorted[[Math]::Min($sorted.Count - 1, [Math]::Floor($p * ($sorted.Count - 1) + 0.5))] }
    [pscustomobject]@{ p50 = (& $at 0.5); p95 = (& $at 0.95); max = $sorted[-1]; count = $values.Count }
}

$data = Join-Path ([IO.Path]::GetTempPath()) ('neaspaces-data-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $data | Out-Null
$settings = @{ bench_enabled = $true; hover_preview = $true; check_updates = $false }
[IO.File]::WriteAllText((Join-Path $data 'settings.json'), ($settings | ConvertTo-Json -Compress))
$pages = @('https://en.wikipedia.org/wiki/Rust_(programming_language)', 'https://github.com/rust-lang/rust', 'https://developer.mozilla.org/en-US/docs/Web/CSS')
$saved = @()
$spaces = @()
for ($s = 0; $s -lt 3; $s++) {
    $spaces += @{ name = "Space $s"; color = @('blue', 'green', 'red')[$s]; active = $saved.Count }
    for ($i = 0; $i -lt $TabsPerSpace; $i++) {
        $url = if ($i -eq 0) { $pages[$s] } else { "https://example.com/?space=$s&tab=$i" }
        $saved += @{ url = $url; title = "Tab $s.$i"; pinned = $false; space = $s }
    }
}
$session = @{
    tabs = $saved; active = 0; vertical_tabs = $true; sidebar_collapsed = $false
    window = @(80, 80, 1480, 980); maximized = $false; spaces = $spaces; active_space = 0
}
[IO.File]::WriteAllText((Join-Path $data 'session.json'), ($session | ConvertTo-Json -Compress -Depth 6))

$env:NEA_DATA_DIR = $data
$env:NEA_BENCH = '1'
$env:NEA_TRACE = '1'
$env:NEA_SCALE = '100'
$env:NEA_CREDENTIAL_PREFIX = 'nea-spaces-test'
$env:NEA_BENCH_PIPE = 'spaces-' + [Guid]::NewGuid().ToString('N').Substring(0, 6)
$process = Start-Process -FilePath $Exe -PassThru
try {
    Connect-Nea -TimeoutSec 20
    $ping = Invoke-Nea ping
    if ($ping.pid -ne $process.Id) { throw "A different process ($($ping.pid)) answers on the pipe." }
    Start-Sleep -Seconds 4
    # Warm-up: each space's live page is made and loads.
    for ($k = 0; $k -lt 3; $k++) {
        Invoke-Nea run @{ name = 'next_space' } | Out-Null
        Start-Sleep -Milliseconds 600
        Invoke-Nea wait @{ timeout_ms = 15000 } -NoThrow | Out-Null
    }
    Start-Sleep -Seconds 2
    $state = Invoke-Nea state
    Write-Host ("Spaces {0}, tabs in the active space {1}" -f @($state.spaces.list).Count, @($state.tabs).Count)
    $before = $state.anim
    $tracePath = Join-Path $data 'trace.log'
    $traceBefore = if (Test-Path $tracePath) { @(Get-Content -LiteralPath $tracePath).Count } else { 0 }
    Invoke-Nea frames | Out-Null

    $random = New-Object System.Random($Seed)
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $sent = 0
    $wait = { param([int]$ms) $due = $clock.ElapsedMilliseconds + $ms; while ($clock.ElapsedMilliseconds -lt $due) { Start-Sleep -Milliseconds 1 } }
    switch ($Pattern) {
        'slow' {
            for ($k = 0; $k -lt 8; $k++) {
                $name = if ($k % 2) { 'prev_space' } else { 'next_space' }
                Invoke-Nea run @{ name = $name } | Out-Null; $sent++
                & $wait 600
            }
        }
        'rapid' {
            while ($clock.ElapsedMilliseconds -lt $Seconds * 1000) {
                Invoke-Nea run @{ name = 'next_space' } | Out-Null; $sent++
                & $wait $random.Next(30, 81)
            }
        }
        'reverse' {
            while ($clock.ElapsedMilliseconds -lt $Seconds * 1000) {
                Invoke-Nea run @{ name = 'next_space' } | Out-Null; $sent++
                & $wait $random.Next(40, 91)
                Invoke-Nea run @{ name = 'prev_space' } | Out-Null; $sent++
                & $wait $random.Next(40, 91)
            }
        }
    }
    Start-Sleep -Milliseconds 800
    $all = @((Invoke-Nea frames).marks)
    $marks = @($all | Where-Object { $_[0] -eq 'strip' })
    $card = @($all | Where-Object { $_[0] -eq 'space card' })
    $end = Invoke-Nea state
    $after = $end.anim
    # The list landed: the page on screen is the live tab of the space it landed on.
    $shown = @($end.shown)
    $landed = if ($shown.Count -eq 1 -and $shown[0] -eq $end.active) { 'ok' } else { 'WRONG' }
    Write-Host ("  page after landing: {0} (shown {1}, active {2})" -f $landed, ($shown -join ','), $end.active)

    $intervals = New-Object System.Collections.Generic.List[double]
    for ($k = 1; $k -lt $marks.Count; $k++) {
        $gap = ([double]$marks[$k][1] - [double]$marks[$k - 1][1]) / 1000.0
        if ($gap -lt 150) {
            $intervals.Add($gap)
            if ($ShowGaps -and $gap -gt 1.5 * $TargetMs) { Write-Host ("    gap {0:N1} ms before {1:N1}" -f $gap, ([double]$marks[$k][1] / 1000.0)) }
        }
    }
    $s = Stats $intervals.ToArray()
    $dropped = @($intervals | Where-Object { $_ -gt 1.5 * $TargetMs }).Count
    Write-Host ("Pattern {0}: {1} commands in {2:N1} s" -f $Pattern, $sent, ($clock.ElapsedMilliseconds / 1000.0))
    if ($s) {
        $mean = ($intervals | Measure-Object -Average).Average
        Write-Host ("  strip frames: {0:N1} fps  p50 {1:N2} ms  p95 {2:N2} ms  max {3:N2} ms  dropped {4}/{5}" -f (1000.0 / $mean), $s.p50, $s.p95, $s.max, $dropped, $s.count)
    }
    $cardGaps = New-Object System.Collections.Generic.List[double]
    for ($k = 1; $k -lt $card.Count; $k++) {
        $gap = ([double]$card[$k][1] - [double]$card[$k - 1][1]) / 1000.0
        if ($gap -lt 150) { $cardGaps.Add($gap) }
    }
    $cs = Stats $cardGaps.ToArray()
    if ($cs) {
        $cardDropped = @($cardGaps | Where-Object { $_ -gt 1.5 * $TargetMs }).Count
        Write-Host ("  name card frames: p50 {0:N2} ms  p95 {1:N2} ms  max {2:N2} ms  dropped {3}/{4}" -f $cs.p50, $cs.p95, $cs.max, $cardDropped, $cs.count)
    }
    $frames = $after.frames - $before.frames
    $gaps = $after.gaps - $before.gaps
    Write-Host ("  clock: worst frame {0:N2} ms (whole run), frames over 16 ms {1}/{2}, mean frame {3:N2} ms, worst gap {4:N2} ms, mean gap {5:N2} ms over {6}" -f `
        ($after.worst_frame_us / 1000.0), ($after.frames_over_16ms - $before.frames_over_16ms), $frames,
        $(if ($frames) { ($after.total_frame_us - $before.total_frame_us) / 1000.0 / $frames } else { 0 }),
        ($after.worst_gap_us / 1000.0), $(if ($gaps) { ($after.total_gap_us - $before.total_gap_us) / 1000.0 / $gaps } else { 0 }), $gaps)
    $lines = @(Get-Content -LiteralPath $tracePath | Select-Object -Skip $traceBefore)
    $commands = @($lines | Where-Object { $_ -match 'command (next|prev)_space done in ([\d.]+) ms' } | ForEach-Object { [double]$Matches[2] })
    $c = Stats $commands
    if ($c) { Write-Host ("  command time: p50 {0:N2} ms  p95 {1:N2} ms  max {2:N2} ms over {3}" -f $c.p50, $c.p95, $c.max, $c.count) }
    $slow = @($lines | Where-Object { $_ -match ' slow ' })
    Write-Host ("  slow messages: {0}" -f $slow.Count)
    # The longest handlers: what held the thread between frames.
    $slow | Sort-Object { if ($_ -match ' ([\d.]+) ms$') { -[double]$Matches[1] } else { 0 } } | Select-Object -First 6 | ForEach-Object { Write-Host "    $_" }
} finally {
    try { Invoke-Nea quit | Out-Null } catch { }
    Disconnect-Nea
    if (-not $process.WaitForExit(15000)) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 500
    foreach ($p in @(Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($data) })) {
        Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue
    }
    Remove-Item Env:NEA_DATA_DIR, Env:NEA_BENCH, Env:NEA_BENCH_PIPE, Env:NEA_TRACE, Env:NEA_SCALE, Env:NEA_CREDENTIAL_PREFIX -ErrorAction SilentlyContinue
    if (-not $KeepData) { Remove-Item -Recurse -Force -LiteralPath $data -ErrorAction SilentlyContinue } else { Write-Host "Data: $data" }
}
