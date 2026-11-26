<#
.SYNOPSIS
  Measures the latency of the field: the time from Enter to the start of
  the page navigation. It measures cold and warm starts. A heavy-use pass
  also collects the hangs.

.DESCRIPTION
  Each run starts a new Nea with a temporary NEA_DATA_DIR. The run uses
  script control on a separate pipe and NEA_TRACE=1. With NEA_TRACE=1, Nea
  writes a timestamped mark for each step to trace.log. The steps are
  Enter, view requested, view ready, NavigationStarting, ContentLoading and
  NavigationCompleted. The script reads these marks. Thus the time of the
  pipe round trip is not in the result.

  The script fills the history with -HistoryEntries fake entries, as after
  years of use. It downloads the block lists one time and copies them into
  each run, as measure.ps1 does.

  Cases:
    cold            no tabs: the field is open at startup, Enter with the engine off
    cold-session    a saved session: Ctrl+T and Enter immediately after startup
    same-tab        Ctrl+L and Enter on a live tab
    new-tab         Ctrl+T and Enter (with -Tabs tabs open behind it)
    sleeping        selection of a discarded tab (until it navigates)
  -Usage opens -Tabs real tabs and does tab changes, field searches and
  panel searches. It also uses a faster sleep timer. At the end, it shows
  hang.log and the slow handlers from the trace.

  The script does not use the system keyboard or mouse. All commands go
  through the pipe.
#>
param(
    [string]$Exe = '',
    [int]$Repeat = 5,
    [int]$Tabs = 30,
    [int]$HistoryEntries = 20000,
    [string]$TargetUrl = 'https://example.com/',
    [string[]]$Only = @(),
    [switch]$Usage,
    [int]$UsageSec = 120,
    [string]$Report = '',
    # The prefix for the saved trace of each run (<prefix>-<n>.log).
    [string]$KeepTrace = '',
    # The time that a person needs to type the address, from the open field to Enter.
    # With 0, the pipe types immediately. This is the worst case: a person cannot
    # press Enter before the engine has the view ready.
    [int]$TypingMs = 0,
    # No network outside this computer: the engine resolves only 127.0.0.1,
    # the block lists stay the built-in ones and the updater does not check.
    # The pages of other hosts become error pages.
    [switch]$Offline
)

$ErrorActionPreference = 'Stop'
$Only = @($Only | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
. (Join-Path $PSScriptRoot 'bench.ps1')
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Exe) { $Exe = Join-Path $root 'target\release\nea.exe' }
$Exe = (Resolve-Path $Exe).Path

$HeavyUrls = @(
    'https://www.infobae.com/', 'https://www.lanacion.com.ar/', 'https://www.clarin.com/', 'https://www.youtube.com/',
    'https://en.wikipedia.org/wiki/Argentina', 'https://github.com/rust-lang/rust', 'https://www.reddit.com/',
    'https://www.amazon.com/', 'https://www.mercadolibre.com.ar/', 'https://www.nytimes.com/', 'https://www.bbc.com/',
    'https://edition.cnn.com/', 'https://www.theguardian.com/international', 'https://elpais.com/', 'https://stackoverflow.com/questions',
    'https://news.ycombinator.com/', 'https://www.imdb.com/', 'https://www.espn.com/', 'https://www.booking.com/',
    'https://www.pagina12.com.ar/', 'https://www.perfil.com/', 'https://www.ambito.com/', 'https://www.tn.com.ar/',
    'https://developer.mozilla.org/en-US/docs/Web/JavaScript', 'https://www.lemonde.fr/', 'https://www.spiegel.de/',
    'https://www.cnet.com/', 'https://www.theverge.com/', 'https://www.wired.com/', 'https://www.weather.com/'
)

function Test-Wanted([string]$name) {
    if ($Only.Count -eq 0) { return $true }
    foreach ($o in $Only) { if ($name -like $o) { return $true } }
    $false
}

function New-TempDir([string]$prefix) {
    $dir = Join-Path ([IO.Path]::GetTempPath()) ("$prefix-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
    New-Item -ItemType Directory -Path $dir | Out-Null
    $dir
}

function Remove-TempDir([string]$dir) {
    for ($i = 0; $i -lt 10; $i++) {
        try { Remove-Item -Recurse -Force -LiteralPath $dir -ErrorAction Stop; return } catch { Start-Sleep -Milliseconds 500 }
    }
    Write-Warning "Cannot delete $dir."
}

function Get-ProcessesMentioning([string]$name, [string]$marker) {
    @(Get-CimInstance Win32_Process -Filter "Name='$name'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($marker) })
}

function Write-History([string]$path, [int]$count) {
    $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    $words = 'news', 'sports', 'recipe', 'weather', 'map', 'video', 'docs', 'forum', 'shop', 'bank', 'travel', 'music'
    $sb = New-Object System.Text.StringBuilder
    [void]$sb.Append('[')
    for ($i = 0; $i -lt $count; $i++) {
        if ($i) { [void]$sb.Append(',') }
        $w = $words[$i % $words.Count]
        $url = "https://www.site$($i % 997)-$w.com/$w/article-$i?ref=$($i * 7 % 1000)"
        [void]$sb.Append("{`"url`":`"$url`",`"title`":`"$w $i - Site $($i % 997)`",`"visits`":$(1 + $i % 13),`"last`":$($now - $i * 37)}")
    }
    [void]$sb.Append(']')
    [IO.File]::WriteAllText($path, $sb.ToString())
}

$script:ShieldCache = $null
$script:HistoryFile = $null
$script:Runs = @()

function Start-Run([hashtable]$settings, $session) {
    $dir = New-TempDir 'nea-latency'
    $all = @{ bench_enabled = $true; check_updates = $false }
    foreach ($k in $settings.Keys) { $all[$k] = $settings[$k] }
    [IO.File]::WriteAllText((Join-Path $dir 'settings.json'), ($all | ConvertTo-Json -Compress -Depth 5))
    if ($session) { [IO.File]::WriteAllText((Join-Path $dir 'session.json'), ($session | ConvertTo-Json -Compress -Depth 5)) }
    if ($script:HistoryFile) { Copy-Item -LiteralPath $script:HistoryFile -Destination (Join-Path $dir 'history.json') }
    if ($script:ShieldCache) { Copy-Item -Recurse -LiteralPath $script:ShieldCache -Destination (Join-Path $dir 'shield') }
    if ($Offline) {
        New-Item -ItemType Directory -Force -Path (Join-Path $dir 'shield') | Out-Null
        [IO.File]::WriteAllText((Join-Path $dir 'shield\updated'), '')
        $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = '--host-resolver-rules="MAP * ~NOTFOUND, EXCLUDE 127.0.0.1"'
    }
    $env:NEA_DATA_DIR = $dir
    $env:NEA_BENCH = '1'
    $env:NEA_TRACE = '1'
    $env:NEA_BENCH_PIPE = 'latency-' + [Guid]::NewGuid().ToString('N').Substring(0, 6)
    $process = Start-Process -FilePath $Exe -PassThru
    $script:Runs += $process.Id
    Connect-Nea -TimeoutSec 20
    $ping = Invoke-Nea ping
    if ($ping.pid -ne $process.Id) { throw "A different process ($($ping.pid)) answers on the pipe." }
    [pscustomobject]@{ Process = $process; Dir = $dir }
}

$script:RunCount = 0
function Stop-Run($run, [switch]$Keep) {
    try { Invoke-Nea quit | Out-Null } catch { }
    $script:RunCount++
    $trace = Join-Path $run.Dir 'trace.log'
    if ($KeepTrace -and (Test-Path $trace)) { Copy-Item -LiteralPath $trace -Destination "$KeepTrace-$($script:RunCount).log" -Force }
    Disconnect-Nea
    if (-not $run.Process.WaitForExit(15000)) { Stop-Process -Id $run.Process.Id -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 500
    foreach ($p in (Get-ProcessesMentioning 'msedgewebview2.exe' $run.Dir)) { Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue }
    Remove-Item Env:NEA_DATA_DIR, Env:NEA_BENCH, Env:NEA_BENCH_PIPE, Env:NEA_TRACE -ErrorAction SilentlyContinue
    if ($Offline) { Remove-Item Env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS -ErrorAction SilentlyContinue }
    if (-not $Keep) { Remove-TempDir $run.Dir }
}

# Trace lines: "<ms since start> <text>". The watchdog writes them once a second.
function Read-Trace($run) {
    Start-Sleep -Milliseconds 1300
    $path = Join-Path $run.Dir 'trace.log'
    if (-not (Test-Path $path)) { return @() }
    @(Get-Content -LiteralPath $path | ForEach-Object {
        if ($_ -match '^\s*([\d.]+)\s+(.*)$') { [pscustomobject]@{ ms = [double]$Matches[1]; text = $Matches[2] } }
    })
}

# From the mark matching $from (the first after $after) to the first
# NavigationStarting of a web address after it, and from there on the same
# tab: its ContentLoading and NavigationCompleted (other tabs keep loading
# in the background, their marks don't count).
function Measure-Marks($trace, [string]$from, [double]$after) {
    $start = $trace | Where-Object { $_.ms -gt $after -and $_.text -match $from } | Select-Object -First 1
    if (-not $start) { return $null }
    $nav = $trace | Where-Object { $_.ms -ge $start.ms -and $_.text -match 'navigation starting tab=(\d+) https?://' } | Select-Object -First 1
    $result = [ordered]@{ navigation = $null; content = $null; complete = $null }
    if ($nav) {
        $null = $nav.text -match 'tab=(\d+)'
        $tab = $Matches[1]
        $result.navigation = [Math]::Round($nav.ms - $start.ms, 1)
        foreach ($pair in @(@('content', "content loading tab=$tab$"), @('complete', "navigation completed tab=$tab "))) {
            $hit = $trace | Where-Object { $_.ms -ge $nav.ms -and $_.text -match $pair[1] } | Select-Object -First 1
            if ($hit) { $result[$pair[0]] = [Math]::Round($hit.ms - $start.ms, 1) }
        }
    }
    [pscustomobject]$result
}

function Wait-Navigation($run, [string]$from, [double]$after, [int]$timeoutSec = 30) {
    $deadline = (Get-Date).AddSeconds($timeoutSec)
    while ((Get-Date) -lt $deadline) {
        $m = Measure-Marks (Read-Trace $run) $from $after
        if ($m -and $null -ne $m.complete) { return $m }
    }
    Measure-Marks (Read-Trace $run) $from $after
}

function Last-Ms($run) {
    $t = Read-Trace $run
    if ($t.Count) { $t[-1].ms } else { 0 }
}

$script:Results = New-Object System.Collections.Generic.List[object]

function Add-Result([string]$case, $m) {
    if (-not $m) { Write-Host "  $case  no marks"; return }
    $row = [pscustomobject]@{ case = $case; navigation_ms = $m.navigation; content_ms = $m.content; complete_ms = $m.complete }
    $script:Results.Add($row)
    Write-Host ("  {0,-16} Enter->navigation {1,8} ms   ->content {2,8} ms   ->complete {3,8} ms" -f $case, $m.navigation, $m.content, $m.complete)
}

function Submit-Field([string]$command, [string]$text) {
    Invoke-Nea run @{ name = $command } | Out-Null
    if ($TypingMs -gt 0) { Start-Sleep -Milliseconds $TypingMs }
    Invoke-Nea type @{ text = $text; submit = $true } | Out-Null
}

function Open-Background([int]$count) {
    $ids = @()
    for ($i = 0; $i -lt $count; $i++) {
        $ids += (Invoke-Nea open @{ url = $HeavyUrls[$i % $HeavyUrls.Count]; background = $true }).tab
        Start-Sleep -Milliseconds 150
    }
    $ids
}

function Initialize-Cache {
    if ($Offline) {
        $script:HistoryFile = Join-Path ([IO.Path]::GetTempPath()) ("nea-latency-history-" + [Guid]::NewGuid().ToString('N').Substring(0, 8) + '.json')
        Write-History $script:HistoryFile $HistoryEntries
        return
    }
    $run = Start-Run @{} $null
    try {
        Invoke-Nea open @{ url = 'about:blank' } | Out-Null
        $stamp = Join-Path $run.Dir 'shield\updated'
        $deadline = (Get-Date).AddSeconds(90)
        while (-not (Test-Path $stamp) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 500 }
        if (Test-Path $stamp) {
            $cache = New-TempDir 'nea-latency-shield'
            Get-ChildItem -LiteralPath (Join-Path $run.Dir 'shield') -File | Where-Object { $_.Extension -ne '.tmp' } | Copy-Item -Destination $cache
            $script:ShieldCache = $cache
        } else {
            Write-Warning 'The download of the block lists failed. The runs use only the built-in minimal list.'
        }
    } finally { Stop-Run $run }
    $script:HistoryFile = Join-Path ([IO.Path]::GetTempPath()) ("nea-latency-history-" + [Guid]::NewGuid().ToString('N').Substring(0, 8) + '.json')
    Write-History $script:HistoryFile $HistoryEntries
}

Write-Host "Nea: $Exe"
Initialize-Cache
try {
    if (Test-Wanted 'cold') {
        for ($r = 0; $r -lt $Repeat; $r++) {
            $run = Start-Run @{} $null
            try {
                Start-Sleep -Seconds 2
                # The field is open because there are no tabs. Type and press Enter with the engine off.
                if ($TypingMs -gt 0) {
                    # Type the first character, then the rest at the speed of a person.
                    Invoke-Nea type @{ text = $TargetUrl.Substring(0, 1) } | Out-Null
                    Start-Sleep -Milliseconds $TypingMs
                    Invoke-Nea type @{ text = $TargetUrl.Substring(1); submit = $true } | Out-Null
                } else {
                    Invoke-Nea type @{ text = $TargetUrl; submit = $true } | Out-Null
                }
                Add-Result 'cold' (Wait-Navigation $run '^submit' 0)
            } finally { Stop-Run $run }
        }
    }

    if (Test-Wanted 'cold-session') {
        $session = @{ tabs = @(@{ url = 'https://example.org/'; title = 'Example'; pinned = $false }); active = 0 }
        for ($r = 0; $r -lt $Repeat; $r++) {
            $run = Start-Run @{} $session
            try {
                Submit-Field 'new_tab' $TargetUrl
                Add-Result 'cold-session' (Wait-Navigation $run '^submit' 0)
            } finally { Stop-Run $run }
        }
    }

    if ((Test-Wanted 'same-tab') -or (Test-Wanted 'new-tab') -or (Test-Wanted 'sleeping')) {
        $run = Start-Run @{} $null
        try {
            $first = (Invoke-Nea open @{ url = 'https://example.org/' }).tab
            Invoke-Nea wait @{ tab = $first; timeout_ms = 30000 } | Out-Null
            if ($Tabs -gt 0) { Open-Background $Tabs | Out-Null; Start-Sleep -Seconds 15 }
            Invoke-Nea run @{ name = 'go_to_tab_1' } | Out-Null
            Start-Sleep -Seconds 1
            if (Test-Wanted 'same-tab') {
                for ($r = 0; $r -lt $Repeat; $r++) {
                    $after = Last-Ms $run
                    $url = $TargetUrl + "?m=$r"
                    Submit-Field 'focus_address' $url
                    Add-Result 'same-tab' (Wait-Navigation $run '^submit' $after)
                }
            }
            if (Test-Wanted 'new-tab') {
                for ($r = 0; $r -lt $Repeat; $r++) {
                    $after = Last-Ms $run
                    Submit-Field 'new_tab' ($TargetUrl + "?n=$r")
                    Add-Result 'new-tab' (Wait-Navigation $run '^submit' $after)
                    Invoke-Nea run @{ name = 'go_to_tab_1' } | Out-Null
                    Start-Sleep -Seconds 2
                }
            }
            if (Test-Wanted 'sleeping') {
                for ($r = 0; $r -lt $Repeat; $r++) {
                    Invoke-Nea run @{ name = 'go_to_tab_1' } | Out-Null
                    Start-Sleep -Milliseconds 500
                    Invoke-Nea discard | Out-Null
                    Start-Sleep -Seconds 2
                    $after = Last-Ms $run
                    # The last tab opened in the background (discard affects only those tabs).
                    # The tab is asleep, and the tab change wakes it.
                    Invoke-Nea run @{ name = 'go_to_tab_9' } | Out-Null
                    Add-Result 'sleeping' (Wait-Navigation $run '^command go_to_tab_9' $after)
                }
            }
        } finally { Stop-Run $run }
    }

    if ($Usage) {
        Write-Host "Heavy use: $Tabs tabs, $UsageSec s"
        $run = Start-Run @{ suspend_after_secs = 10; discard_after_mins = 1; live_tabs_limit = 4 } $null
        try {
            $first = (Invoke-Nea open @{ url = 'https://example.org/' }).tab
            Invoke-Nea wait @{ tab = $first; timeout_ms = 30000 } | Out-Null
            Open-Background $Tabs | Out-Null
            $deadline = (Get-Date).AddSeconds($UsageSec)
            $i = 0
            while ((Get-Date) -lt $deadline) {
                $i++
                Invoke-Nea run @{ name = 'next_tab' } | Out-Null
                Start-Sleep -Milliseconds 400
                if ($i % 5 -eq 0) {
                    Invoke-Nea run @{ name = 'new_tab' } | Out-Null
                    Invoke-Nea type @{ text = 'news sports' } | Out-Null
                    Invoke-Nea type @{ text = 'https://www.wikipedia.org/'; submit = $true } | Out-Null
                }
                if ($i % 7 -eq 0) {
                    Invoke-Nea run @{ name = 'history' } | Out-Null
                    Invoke-Nea type @{ text = 'recipe' } | Out-Null
                    Invoke-Nea run @{ name = 'focus_address' } | Out-Null
                    Invoke-Nea type @{ text = 'site12' } | Out-Null
                }
                if ($i % 9 -eq 0) { Invoke-Nea run @{ name = 'close_tab' } | Out-Null }
                if ($i % 11 -eq 0) { Invoke-Nea run @{ name = 'reopen_tab' } | Out-Null }
            }
            $memory = Invoke-Nea memory
            Write-Host ("  memory: app {0:N1} MB, total {1:N1} MB, {2} processes" -f ($memory.app / 1MB), ($memory.total / 1MB), @($memory.processes).Count)
            $trace = Read-Trace $run
            $slow = $trace | Where-Object { $_.text -match '^slow (\S+) (\d+) ([\d.]+) ms' } | ForEach-Object {
                $null = $_.text -match '^slow (\S+) (\d+) ([\d.]+) ms'
                [pscustomobject]@{ what = $Matches[1]; ms = [double]$Matches[3] }
            }
            Write-Host '  slow handlers (>4 ms), by kind:'
            $slow | Group-Object what | Sort-Object { ($_.Group | Measure-Object ms -Maximum).Maximum } -Descending | ForEach-Object {
                $stats = $_.Group | Measure-Object ms -Maximum -Sum -Average
                Write-Host ("    {0,-32} {1,5} times  max {2,8:N1} ms  avg {3,7:N1} ms  total {4,9:N1} ms" -f $_.Name, $_.Count, $stats.Maximum, $stats.Average, $stats.Sum)
            }
            $hang = Join-Path $run.Dir 'hang.log'
            if (Test-Path $hang) { Write-Host '  hang.log:'; Get-Content $hang | ForEach-Object { Write-Host "    $_" } }
            $crash = Join-Path $run.Dir 'crash.log'
            if (Test-Path $crash) { Write-Host '  crash.log:'; Get-Content $crash | ForEach-Object { Write-Host "    $_" } }
        } finally { Stop-Run $run }
    }
} finally {
    if ($script:ShieldCache) { Remove-TempDir $script:ShieldCache }
    if ($script:HistoryFile) { Remove-Item -LiteralPath $script:HistoryFile -ErrorAction SilentlyContinue }
}

Write-Host ''
Write-Host 'Median per case (ms since Enter):'
$script:Results | Group-Object case | ForEach-Object {
    $median = { param($xs) $s = @($xs | Where-Object { $null -ne $_ } | Sort-Object); if ($s.Count) { $s[[int][Math]::Floor(($s.Count - 1) / 2)] } else { $null } }
    $nav = & $median ($_.Group.navigation_ms)
    $content = & $median ($_.Group.content_ms)
    $done = & $median ($_.Group.complete_ms)
    Write-Host ("  {0,-16} navigation {1,8}   content {2,8}   complete {3,8}   ({4} runs)" -f $_.Name, $nav, $content, $done, $_.Count)
}
if ($Report) { $script:Results | ConvertTo-Json | Set-Content -LiteralPath $Report -Encoding utf8 }
