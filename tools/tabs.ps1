<#
.SYNOPSIS
  Measures the UI thread of Nea with many tabs (200 by default).

.DESCRIPTION
  The script starts a new Nea with a temporary NEA_DATA_DIR, script
  control on a separate pipe and NEA_TRACE=1. The session has -Tabs saved
  tabs (asleep) in vertical layout. The pages come from a small page
  server on the loopback interface only (127.0.0.1).

  Phases:
    restore     start with -Tabs saved tabs, then wait
    panels      the history panel and the address field, 10 times each,
                with -HistoryEntries entries in the history
    open        open -Tabs test tabs, one after the other, without a wait
    switch      the next_tab command -Tabs times, 30 ms apart
    background  open -Tabs / 2 test tabs in the background
    idle        no requests for -IdleSec (the sleep timer discards views)
    close       close all test tabs

  For each phase, the script reads from trace.log the handlers that took
  4 ms or more, and shows, for each handler name, the number, the number
  over 16 ms and the maximum. At the end, it shows hang.log. Handlers nest:
  "message" contains all the others.

  The script sends no keys or clicks to the system. It does not move a
  window to the front.

.EXAMPLE
  tools\tabs.ps1 -Exe C:\Temp\neaperf\release\nea.exe -Report C:\Temp\tabs.json
#>
param(
    [string]$Exe = '',
    [int]$Tabs = 200,
    [int]$IdleSec = 40,
    [int]$HistoryEntries = 20000,
    [string]$Report = '',
    [switch]$KeepData,
    # Exit with code 1 when hang.log has a handler of 50 ms or more.
    [switch]$FailOnStall
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'bench.ps1')
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Exe) { $Exe = Join-Path $root 'target\release\nea.exe' }
$Exe = (Resolve-Path $Exe).Path

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
            $n = ($path -replace '[^0-9]', ''); if (-not $n) { $n = '0' }
            $html = "<!doctype html><html><head><meta charset=`"utf-8`"><title>Page $n</title></head>" +
                "<body style=`"font:14px system-ui;margin:24px`"><h1>Page $n</h1>" +
                ((1..200 | ForEach-Object { "<p>Line $_ of page $n.</p>" }) -join '') + '</body></html>'
            $body = if ($path -eq '/favicon.ico') { [byte[]]@() } else { $utf8.GetBytes($html) }
            $head = $utf8.GetBytes("HTTP/1.1 200 OK`r`nContent-Type: text/html; charset=utf-8`r`nContent-Length: $($body.Length)`r`nCache-Control: max-age=3600`r`nConnection: close`r`n`r`n")
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

# Trace lines: "<ms since start> <text>". The watchdog writes them once a second.
function Read-Trace([string]$dir) {
    $path = Join-Path $dir 'trace.log'
    if (-not (Test-Path $path)) { return @() }
    @(Get-Content -LiteralPath $path | ForEach-Object {
        if ($_ -match '^\s*([\d.]+)\s+slow (\S+(?: \S+)*?) (\d+) ([\d.]+) ms$') {
            [pscustomobject]@{ at = [double]$Matches[1]; name = $Matches[2]; ms = [double]$Matches[4] }
        }
    })
}

function Get-LastMs([string]$dir) {
    Start-Sleep -Milliseconds 1300
    $path = Join-Path $dir 'trace.log'
    if (-not (Test-Path $path)) { return 0 }
    $last = Get-Content -LiteralPath $path -Tail 1
    if ($last -match '^\s*([\d.]+)') { [double]$Matches[1] } else { 0 }
}

$port = Get-FreePort
$job = Start-Job -ScriptBlock $server -ArgumentList $port
$base = "http://127.0.0.1:$port/p/"
$data = Join-Path ([IO.Path]::GetTempPath()) ('neatabs-data-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $data | Out-Null
$process = $null
$phases = New-Object System.Collections.Generic.List[object]
try {
    $deadline = (Get-Date).AddSeconds(15)
    while ($true) {
        try { Invoke-WebRequest -UseBasicParsing -TimeoutSec 2 "${base}0" | Out-Null; break } catch {
            if ((Get-Date) -gt $deadline) { throw "The local page server did not start: $(Receive-Job $job 2>&1)" }
            Start-Sleep -Milliseconds 300
        }
    }
    $settings = @{ bench_enabled = $true }
    [IO.File]::WriteAllText((Join-Path $data 'settings.json'), ($settings | ConvertTo-Json -Compress))
    $saved = @(for ($i = 0; $i -lt $Tabs; $i++) { @{ url = "$base$i"; title = "Page $i"; pinned = $false } })
    $session = @{ tabs = $saved; active = 0; vertical_tabs = $true; sidebar_collapsed = $false; window = @(80, 80, 1480, 980); maximized = $false }
    [IO.File]::WriteAllText((Join-Path $data 'session.json'), ($session | ConvertTo-Json -Compress -Depth 6))
    $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    $words = 'news', 'sports', 'recipe', 'weather', 'map', 'video', 'docs', 'forum', 'shop', 'bank', 'travel', 'music'
    $sb = New-Object System.Text.StringBuilder
    [void]$sb.Append('[')
    for ($i = 0; $i -lt $HistoryEntries; $i++) {
        if ($i) { [void]$sb.Append(',') }
        $w = $words[$i % $words.Count]
        [void]$sb.Append("{`"url`":`"https://www.site$($i % 997)-$w.com/$w/article-$i`",`"title`":`"$w $i - Site $($i % 997)`",`"visits`":$(1 + $i % 13),`"last`":$($now - $i * 37)}")
    }
    [void]$sb.Append(']')
    [IO.File]::WriteAllText((Join-Path $data 'history.json'), $sb.ToString())

    $env:NEA_DATA_DIR = $data
    $env:NEA_BENCH = '1'
    $env:NEA_TRACE = '1'
    $env:NEA_SCALE = '100'
    $env:NEA_CREDENTIAL_PREFIX = 'nea-tabs-test'
    $env:NEA_BENCH_PIPE = 'tabs-' + [Guid]::NewGuid().ToString('N').Substring(0, 6)
    $process = Start-Process -FilePath $Exe -PassThru
    Connect-Nea -TimeoutSec 20
    $ping = Invoke-Nea ping
    if ($ping.pid -ne $process.Id) { throw "A different process ($($ping.pid)) answers on the pipe." }

    $mark = 0.0
    function End-Phase([string]$name, [double[]]$requests) {
        $now = Get-LastMs $data
        $rows = @(Read-Trace $data | Where-Object { $_.at -gt $script:mark -and $_.at -le $now })
        $script:mark = $now
        $byName = @($rows | Group-Object name | ForEach-Object {
            $ms = @($_.Group | ForEach-Object { $_.ms })
            [pscustomobject]@{
                name = $_.Name
                count = $ms.Count
                over16 = @($ms | Where-Object { $_ -gt 16 }).Count
                max = [Math]::Round(($ms | Measure-Object -Maximum).Maximum, 1)
                total = [Math]::Round(($ms | Measure-Object -Sum).Sum, 0)
            }
        } | Sort-Object max -Descending)
        $req = if ($requests.Count) { [Math]::Round(($requests | Measure-Object -Maximum).Maximum, 0) } else { 0 }
        $phases.Add([pscustomobject]@{ phase = $name; request_max_ms = $req; handlers = $byName })
        Write-Host ("{0} (slowest pipe request {1} ms)" -f $name, $req)
        foreach ($h in ($byName | Select-Object -First 8)) {
            Write-Host ("  {0,-28} n={1,-5} >16ms={2,-4} max={3,7} ms total={4,6} ms" -f $h.name, $h.count, $h.over16, $h.max, $h.total)
        }
    }
    function Invoke-Timed([string]$cmd, [hashtable]$arguments) {
        $watch = [Diagnostics.Stopwatch]::StartNew()
        Invoke-Nea $cmd $arguments -NoThrow | Out-Null
        $watch.Elapsed.TotalMilliseconds
    }

    Start-Sleep -Seconds 5
    End-Phase 'restore' @()

    $times = @(for ($i = 0; $i -lt 10; $i++) {
        Invoke-Timed 'run' @{ name = 'history' }; Start-Sleep -Milliseconds 400
        Invoke-Timed 'run' @{ name = 'focus_address' }; Start-Sleep -Milliseconds 400
    })
    End-Phase 'panels' $times

    $times = @(for ($i = 0; $i -lt $Tabs; $i++) { Invoke-Timed 'open' @{ url = "$base$($Tabs + $i)" } })
    Start-Sleep -Seconds 3
    End-Phase 'open' $times

    $times = @(for ($i = 0; $i -lt $Tabs; $i++) { Invoke-Timed 'run' @{ name = 'next_tab' }; Start-Sleep -Milliseconds 30 })
    Start-Sleep -Seconds 3
    End-Phase 'switch' $times

    $times = @(for ($i = 0; $i -lt [Math]::Floor($Tabs / 2); $i++) { Invoke-Timed 'open' @{ url = "$base$(2 * $Tabs + $i)"; background = $true } })
    Start-Sleep -Seconds 3
    End-Phase 'background' $times

    Start-Sleep -Seconds $IdleSec
    End-Phase 'idle' @()

    $times = @(Invoke-Timed 'close' @{})
    Start-Sleep -Seconds 3
    End-Phase 'close' $times
} finally {
    try { Invoke-Nea quit -NoThrow | Out-Null } catch { }
    Disconnect-Nea
    if ($process -and -not $process.WaitForExit(20000)) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
    Stop-Job $job -ErrorAction SilentlyContinue; Remove-Job $job -Force -ErrorAction SilentlyContinue
    Remove-Item Env:NEA_DATA_DIR, Env:NEA_BENCH, Env:NEA_BENCH_PIPE, Env:NEA_TRACE, Env:NEA_SCALE, Env:NEA_CREDENTIAL_PREFIX -ErrorAction SilentlyContinue
}

$hang = Join-Path $data 'hang.log'
$stalls = @()
if (Test-Path $hang) {
    $stalls = @(Get-Content -LiteralPath $hang)
    Write-Host 'hang.log:'
    $stalls | Select-Object -Last 30 | ForEach-Object { Write-Host "  $_" }
}
if ($Report) {
    $commit = (git -C $root rev-parse --short HEAD 2>$null)
    [IO.File]::WriteAllText($Report, ([pscustomobject]@{ commit = $commit; tabs = $Tabs; phases = $phases; hang = $stalls } | ConvertTo-Json -Depth 6))
}
if (-not $KeepData) { Start-Sleep -Seconds 2; Remove-Item -Recurse -Force -LiteralPath $data -ErrorAction SilentlyContinue } else { Write-Host "Data: $data" }
if ($FailOnStall -and @($stalls | Where-Object { $_ -match ' slow: ' }).Count) { exit 1 }
