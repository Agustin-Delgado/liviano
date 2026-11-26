<#
.SYNOPSIS
  Measures memory use: fixed scenarios, a report and a regression gate.

.DESCRIPTION
  Each scenario starts a new Nea with a temporary NEA_DATA_DIR and script
  control (NEA_BENCH=1, on a separate pipe). The script puts Nea in the
  state of the scenario and waits until the memory is stable. Then it
  measures the private working set of nea.exe and of all the engine
  processes. The result is the median of some samples. With -Edge and
  -Chrome, the script opens the same pages in Edge and in Chrome. Each
  browser uses a separate temporary profile, and the script measures its
  process tree in the same way.

  Blocking: before the scenarios, a preparation run lets Nea download
  EasyList and EasyPrivacy one time. The script copies this cache into each
  scenario. Thus all the scenarios compile the same rules, and no scenario
  downloads lists during a measurement. The scenarios with views wait until
  the filter engine is ready, and then open the page. For the heavy page,
  the script records the number of requests that the filter engine saw and
  blocked (the `shield` command of the pipe).

  Scenarios:
    1 app-only              the app with no views (no engine)
    2 about-blank           one tab with about:blank
    3 light                 a light page (-LightUrl)
    4 heavy-<variant>       a heavy page (-HeavyUrl) in each variant of
                            -HeavyVariants (with and without blocking)
    5 20-tabs               20 loaded tabs, 19 of them asleep
    6 restore-50            startup with a session of 50 tabs
    7 20-tabs-open          20 loaded tabs, and the script puts no tab to
                            sleep: each browser does what it does alone
    8 20-tabs-idle          the same, after -IdleSec with no input

  Gate: with -Baseline, the script compares the results with the file. It
  exits with code 1 if a scenario is more than its tolerance. -UpdateBaseline
  writes the figures of this run to the file. The script also reads a
  baseline with the old Spanish scenario names (solo-programa, liviana,
  pesada-*, 20-pestanas, restaurar-50).

.EXAMPLE
  tools\measure.ps1 -Build -Edge -Chrome -Report docs\benchmarks.md -Csv docs\benchmarks.csv -UpdateBaseline
.EXAMPLE
  tools\measure.ps1 -Baseline docs\benchmarks-baseline.json   # fails if there is a regression
#>
[CmdletBinding()]
param(
    [string]$Exe = '',
    [string]$BenchExe = '',
    [switch]$Build,
    [string]$LightUrl = 'https://example.com/',
    [string]$HeavyUrl = 'https://www.infobae.com/',
    # Variant name -> settings merged into settings.json. Blocking is
    # on everywhere unless a host is in shield_disabled; without that module
    # both variants are the same build and measure the same.
    [System.Collections.IDictionary]$HeavyVariants = $null,
    [string[]]$ManyUrls = @('https://example.com/', 'https://example.org/', 'https://example.net/', 'https://www.iana.org/', 'https://info.cern.ch/'),
    [int]$SettleSec = 10,
    [int]$IdleSec = 300,
    [int]$Samples = 3,
    [int]$LoadTimeoutSec = 45,
    [string[]]$Only = @(),
    [switch]$Edge,
    [string]$EdgeExe = '',
    [switch]$Chrome,
    [string]$ChromeExe = '',
    # Extra engine switches for every Nea run (WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS),
    # to try a memory flag before building it in.
    [string]$EngineArgs = '',
    [string]$Report = '',
    [string]$Csv = '',
    [string]$Baseline = '',
    [switch]$UpdateBaseline,
    [double]$Tolerance = 15,
    [double]$HeavyTolerance = 35
)

$ErrorActionPreference = 'Stop'
# With -File a list arrives as one "a,b" string.
$Only = @($Only | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
$ManyUrls = @($ManyUrls | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
. (Join-Path $PSScriptRoot 'bench.ps1')

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Exe) { $Exe = Join-Path $root 'target\release\nea.exe' }
if (-not $BenchExe) { $BenchExe = Join-Path $root 'target\release\examples\bench.exe' }
if ($Build) {
    Push-Location $root
    try {
        cargo build --release --bin nea --example bench
        if ($LASTEXITCODE -ne 0) { throw 'cargo build failed.' }
    } finally { Pop-Location }
}
$Exe = (Resolve-Path $Exe).Path
$BenchExe = (Resolve-Path $BenchExe).Path

$heavyHost = ([Uri]$HeavyUrl).Host -replace '^www\.', ''
if (-not $HeavyVariants) {
    $HeavyVariants = [ordered]@{
        'blocking'    = @{}
        'no-blocking' = @{ shield_disabled = @($heavyHost) }
    }
}

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

# Processes whose command line mentions $marker (an engine or Edge profile dir).
function Get-ProcessesMentioning([string]$name, [string]$marker) {
    @(Get-CimInstance Win32_Process -Filter "Name='$name'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($marker) })
}

function Get-Median([double[]]$values) {
    $sorted = @($values | Sort-Object)
    $sorted[[int][Math]::Floor(($sorted.Count - 1) / 2)]
}

function Measure-Samples([scriptblock]$probe) {
    Start-Sleep -Seconds $SettleSec
    $runs = @()
    for ($i = 0; $i -lt $Samples; $i++) {
        if ($i -gt 0) { Start-Sleep -Seconds 2 }
        $runs += , (& $probe)
    }
    $median = Get-Median ($runs | ForEach-Object { $_.total })
    $runs | Where-Object { $_.total -eq $median } | Select-Object -First 1
}

$script:Rows = New-Object System.Collections.Generic.List[object]
$script:Shield = [ordered]@{}
$script:ShieldStats = $null
$script:ShieldCache = $null
$script:ShieldFiles = @()
$script:Notes = New-Object System.Collections.Generic.List[string]
$script:EngineVersion = ''

function Add-Row($scenario, $browser, $m, [string]$note = '') {
    $mb = { param($b) [Math]::Round($b / 1MB, 1) }
    $row = [pscustomobject]@{
        scenario  = $scenario
        browser   = $browser
        total_mb  = & $mb $m.total
        app_mb    = & $mb $m.app
        engine_mb = & $mb $m.engine
        processes = $m.processes
        note      = $note
    }
    $script:Rows.Add($row)
    Write-Host ("  {0,-24} {1,-8} {2,8:N1} MB  ({3} processes){4}" -f $scenario, $browser, $row.total_mb, $row.processes, $(if ($note) { "  - $note" } else { '' }))
}

# ---------- Nea ----------

function Start-Nea([hashtable]$settings, $session) {
    $dir = New-TempDir 'nea-measure'
    $all = @{ bench_enabled = $true }
    foreach ($k in $settings.Keys) { $all[$k] = $settings[$k] }
    [IO.File]::WriteAllText((Join-Path $dir 'settings.json'), ($all | ConvertTo-Json -Compress -Depth 5))
    if ($session) {
        [IO.File]::WriteAllText((Join-Path $dir 'session.json'), ($session | ConvertTo-Json -Compress -Depth 5))
    }
    if ($script:ShieldCache) {
        # Copy-Item keeps the files' dates, so the lists look fresh and nothing is downloaded.
        Copy-Item -Recurse -LiteralPath $script:ShieldCache -Destination (Join-Path $dir 'shield')
    }
    $env:NEA_DATA_DIR = $dir
    $env:NEA_BENCH = '1'
    $env:NEA_BENCH_PIPE = 'measure-' + [Guid]::NewGuid().ToString('N').Substring(0, 6)
    if ($EngineArgs) { $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = $EngineArgs }
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
    Start-Sleep -Milliseconds 500
    # The engine normally leaves with the app; anything still holding the profile goes.
    foreach ($p in (Get-ProcessesMentioning 'msedgewebview2.exe' $run.Dir)) {
        Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue
    }
    Remove-Item Env:NEA_DATA_DIR, Env:NEA_BENCH, Env:NEA_BENCH_PIPE, Env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS -ErrorAction SilentlyContinue
    Remove-TempDir $run.Dir
}

function Get-NeaMemory {
    $m = Invoke-Nea memory
    if (-not $script:EngineVersion) {
        $engine = $m.processes | Where-Object { $_.exe -eq 'msedgewebview2.exe' } | Select-Object -First 1
        if ($engine) {
            $path = (Get-Process -Id $engine.pid -ErrorAction SilentlyContinue).Path
            if ($path) { $script:EngineVersion = (Get-Item $path).VersionInfo.ProductVersion }
        }
    }
    [pscustomobject]@{ total = [double]$m.total; app = [double]$m.app; engine = [double]$m.engine; processes = @($m.processes).Count }
}

# The filter engine compiles on its own thread once the first view needs the
# web engine; a page opened before that loads unfiltered.
function Wait-Shield {
    $deadline = (Get-Date).AddSeconds(20)
    while ((Get-Date) -lt $deadline) {
        $s = Invoke-Nea shield
        if ($null -ne $s.rules) { return $s }
        Start-Sleep -Milliseconds 200
    }
    throw 'The filter engine did not compile in 20 s.'
}

# Opens about:blank first so the engine (and with it the filters) start,
# then the page itself in a new tab, and closes the blank one.
function Open-Filtered([string]$url) {
    $blank = (Invoke-Nea open @{ url = 'about:blank' }).tab
    Wait-Tab $blank | Out-Null
    $script:ShieldStats = Wait-Shield
    $tab = (Invoke-Nea open @{ url = $url }).tab
    Invoke-Nea close @{ tab = $blank } | Out-Null
    $tab
}

function Get-ShieldNote {
    $now = Invoke-Nea shield
    $requests = $now.requests - $script:ShieldStats.requests
    $blocked = $now.blocked - $script:ShieldStats.blocked
    $script:LastShield = [pscustomobject]@{ requests = $requests; blocked = $blocked; rules = $now.rules; heap_bytes = $now.heap_bytes }
    "$blocked of $requests requests blocked"
}

function Wait-Tab($tab) {
    $arguments = @{ timeout_ms = $LoadTimeoutSec * 1000 }
    if ($null -ne $tab) { $arguments.tab = $tab }
    $reply = Invoke-Nea wait $arguments -NoThrow
    if ($reply.ok) { return '' }
    "did not finish loading in $LoadTimeoutSec s"
}

function Invoke-NeaScenario([string]$name, [hashtable]$settings, $session, [scriptblock]$steps) {
    if (-not (Test-Wanted $name)) { return }
    $run = Start-Nea $settings $session
    $script:ShieldStats = $null
    try {
        $note = [string](& $steps)
        $m = Measure-Samples { Get-NeaMemory }
        # Counted after the samples: ads keep arriving while the page settles.
        if ($script:ShieldStats) {
            $shieldNote = Get-ShieldNote
            $script:Shield[$name] = $script:LastShield
            $note = (@($note, $shieldNote) | Where-Object { $_ }) -join ', '
        }
        Add-Row $name 'Nea' $m $note
    } finally {
        Stop-Nea $run
    }
}

# ---------- Edge and Chrome ----------

$script:Foreign = [ordered]@{
    Edge   = @{ Wanted = [bool]$Edge; Exe = $EdgeExe; Process = 'msedge.exe'; Path = 'Microsoft\Edge\Application\msedge.exe'; Version = '' }
    Chrome = @{ Wanted = [bool]$Chrome; Exe = $ChromeExe; Process = 'chrome.exe'; Path = 'Google\Chrome\Application\chrome.exe'; Version = '' }
}

function Find-Browser([string]$browser) {
    $b = $script:Foreign[$browser]
    if ($b.Exe) { return $b.Exe }
    foreach ($base in @(${env:ProgramFiles(x86)}, $env:ProgramFiles, $env:LOCALAPPDATA)) {
        if (-not $base) { continue }
        $candidate = Join-Path $base $b.Path
        if (Test-Path $candidate) { return $candidate }
    }
    $null
}

# Only the processes started with our own profile folder: the person's own
# Edge or Chrome, if open, never shows up here.
function Get-ForeignMemory([string]$processName, [string]$profileDir) {
    $pids = @(Get-ProcessesMentioning $processName $profileDir | ForEach-Object { $_.ProcessId })
    if ($pids.Count -eq 0) { return [pscustomobject]@{ total = 0; app = 0; engine = 0; processes = 0 } }
    $m = (& $BenchExe memtree @pids) | ConvertFrom-Json
    [pscustomobject]@{ total = [double]$m.total; app = 0; engine = [double]$m.total; processes = @($m.processes).Count }
}

function Invoke-ForeignScenario([string]$browser, [string]$name, [string[]]$urls, [int]$idleSec = 0) {
    $b = $script:Foreign[$browser]
    if (-not $b.Wanted -or -not (Test-Wanted $name)) { return }
    $exe = Find-Browser $browser
    if (-not $exe) {
        $note = "$browser is not installed. The report has no comparison with it."
        if (-not $script:Notes.Contains($note)) { $script:Notes.Add($note) }
        return
    }
    $b.Version = (Get-Item $exe).VersionInfo.ProductVersion
    $profileDir = New-TempDir "nea-measure-$($browser.ToLower())"
    $browserArgs = @("--user-data-dir=`"$profileDir`"", '--no-first-run', '--no-default-browser-check', '--disable-sync', '--new-window') + $urls
    Start-Process -FilePath $exe -ArgumentList $browserArgs | Out-Null
    try {
        # Neither can be asked when a page is done: they get the load timeout's share.
        Start-Sleep -Seconds ([Math]::Min($LoadTimeoutSec, 10 + 2 * $urls.Count))
        if ($idleSec) { Start-Sleep -Seconds $idleSec }
        $m = Measure-Samples { Get-ForeignMemory $b.Process $profileDir }
        Add-Row $name $browser $m
    } finally {
        for ($i = 0; $i -lt 3; $i++) {
            foreach ($p in (Get-ProcessesMentioning $b.Process $profileDir)) { Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue }
            Start-Sleep -Milliseconds 700
        }
        Remove-TempDir $profileDir
    }
}

function Invoke-OthersScenario([string]$name, [string[]]$urls, [int]$idleSec = 0) {
    foreach ($browser in $script:Foreign.Keys) { Invoke-ForeignScenario $browser $name $urls $idleSec }
}

# ---------- filter lists ----------

# One run lets Nea fetch EasyList and EasyPrivacy the way it always does;
# every scenario then starts from a copy of that cache.
function Initialize-ShieldCache {
    $run = Start-Nea @{} $null
    try {
        $tab = (Invoke-Nea open @{ url = 'about:blank' }).tab
        Wait-Tab $tab | Out-Null
        $stamp = Join-Path $run.Dir 'shield\updated'
        $deadline = (Get-Date).AddSeconds(90)
        while (-not (Test-Path $stamp) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 500 }
        if (-not (Test-Path $stamp)) {
            $script:Notes.Add('The download of EasyList and EasyPrivacy failed. Blocking used only the built-in minimal list.')
            return
        }
        $cache = New-TempDir 'nea-measure-shield'
        Get-ChildItem -LiteralPath (Join-Path $run.Dir 'shield') -File | Where-Object { $_.Extension -ne '.tmp' } |
            Copy-Item -Destination $cache
        $script:ShieldCache = $cache
        $script:ShieldFiles = @(Get-ChildItem -LiteralPath $cache -Filter '*.txt' | ForEach-Object { "$($_.Name) ($([Math]::Round($_.Length / 1KB)) KB)" })
    } finally {
        Stop-Nea $run
    }
}

# ---------- scenarios ----------

$manyUrls = for ($i = 0; $i -lt 20; $i++) { $ManyUrls[$i % $ManyUrls.Count] }
$restoreUrls = for ($i = 0; $i -lt 50; $i++) { $ManyUrls[$i % $ManyUrls.Count] }
$restoreSession = @{
    tabs   = @($restoreUrls | ForEach-Object { @{ url = $_; title = ''; pinned = $false } })
    active = 0
}

Write-Host "Nea: $Exe"
Write-Host "Settling $SettleSec s, $Samples samples per scenario (median)."
if ($Only.Count -eq 0 -or @($Only | Where-Object { $_ -ne 'app-only' }).Count -gt 0) {
    Write-Host 'Preparing the block list cache...'
    Initialize-ShieldCache
}

Invoke-NeaScenario 'app-only' @{} $null { '' }

Invoke-NeaScenario 'about-blank' @{} $null {
    $tab = (Invoke-Nea open @{ url = 'about:blank' }).tab
    Wait-Tab $tab
}
Invoke-OthersScenario 'about-blank' @('about:blank')

Invoke-NeaScenario 'light' @{} $null {
    $tab = (Invoke-Nea open @{ url = $LightUrl }).tab
    Wait-Tab $tab
}
Invoke-OthersScenario 'light' @($LightUrl)

foreach ($variant in $HeavyVariants.Keys) {
    Invoke-NeaScenario "heavy-$variant" $HeavyVariants[$variant] $null {
        $tab = Open-Filtered $HeavyUrl
        Wait-Tab $tab
    }
}
Invoke-OthersScenario 'heavy' @($HeavyUrl)

Invoke-NeaScenario '20-tabs' @{} $null {
    $late = 0
    foreach ($url in $manyUrls) {
        $tab = (Invoke-Nea open @{ url = $url }).tab
        if (Wait-Tab $tab) { $late++ }
    }
    $slept = (Invoke-Nea discard).discarded
    $note = "$slept asleep"
    if ($late) { $note += ", $late not done loading" }
    $note
}
Invoke-OthersScenario '20-tabs' $manyUrls

foreach ($natural in @(@{ Name = '20-tabs-open'; Idle = 0 }, @{ Name = '20-tabs-idle'; Idle = $IdleSec })) {
    Invoke-NeaScenario $natural.Name @{} $null {
        $late = 0
        foreach ($url in $manyUrls) {
            $tab = (Invoke-Nea open @{ url = $url }).tab
            if (Wait-Tab $tab) { $late++ }
        }
        if ($natural.Idle) { Start-Sleep -Seconds $natural.Idle }
        $note = if ($natural.Idle) { "$($natural.Idle) s with no input" } else { 'no tab put to sleep by the script' }
        if ($late) { $note += ", $late not done loading" }
        $note
    }
    Invoke-OthersScenario $natural.Name $manyUrls $natural.Idle
}

Invoke-NeaScenario 'restore-50' @{} $restoreSession {
    $state = Invoke-Nea state
    $note = "$(@($state.tabs).Count) tabs restored"
    $late = Wait-Tab $null
    if ($late) { $note += ", the active one $late" }
    $note
}

# ---------- report ----------

$commit = (git -C $root rev-parse --short HEAD 2>$null)
$dirty = (git -C $root status --porcelain 2>$null)
if ($dirty) { $commit = "$commit (with uncommitted changes)" }
$os = Get-CimInstance Win32_OperatingSystem
$ramGb = [Math]::Round($os.TotalVisibleMemorySize / 1MB, 1)
$others = @($script:Foreign.Keys | Where-Object { $script:Foreign[$_].Version })

function Get-Row([string]$scenario, [string]$browser = 'Nea') {
    $script:Rows | Where-Object { $_.browser -eq $browser -and $_.scenario -eq $scenario } | Select-Object -First 1
}

function Format-Mb([double]$mb) { ('{0:N1}' -f $mb) }

# The memory budgets, checked against this run.
function Get-BudgetLines {
    $out = New-Object System.Collections.Generic.List[string]
    $yes = { param($ok) if ($ok) { 'Yes' } else { '**No**' } }
    $out.Add('| Budget or criterion | Target | Measured | Met? |')
    $out.Add('|---|---|---|---|')
    $neaRows = @($script:Rows | Where-Object { $_.browser -eq 'Nea' })
    $solo = Get-Row 'app-only'
    if ($neaRows.Count -gt 0) {
        $maxApp = ($neaRows | Measure-Object -Property app_mb -Maximum).Maximum
        $soloText = if ($solo) { "$(Format-Mb $solo.app_mb) MB with no views; " } else { '' }
        $out.Add("| Own process (UI + logic) | < 20 MB idle (a few dozen at most) | $($soloText)at most $(Format-Mb $maxApp) MB across all scenarios | $(& $yes ($maxApp -lt 20)) |")
    }
    $restore = Get-Row 'restore-50'
    $light = Get-Row 'light'
    if ($restore -and $light) {
        $perApp = ($restore.app_mb - $light.app_mb) / 49
        $perTotal = ($restore.total_mb - $light.total_mb) / 49
        # Rows are rounded to 0.1 MB: spread over 49 tabs that is ~0.002 MB.
        $perText = if ($perApp -le 0.002) { 'Under 0.002 MB (below the resolution)' } else { "$('{0:N3}' -f $perApp) MB" }
        $out.Add("| Sleeping tab (metadata only) | < 0.5 MB each | $perText of the app per tab (restore-50 against light, 49 asleep); counting the engine, $('{0:N2}' -f $perTotal) MB | $(& $yes ($perApp -lt 0.5)) |")
    }
    $heavy = @($script:Shield.Values | Where-Object { $_.heap_bytes }) | Select-Object -First 1
    if ($heavy) {
        $engineMb = $heavy.heap_bytes / 1MB
        $out.Add("| Filter engine | A few MB | $('{0:N1}' -f $engineMb) MB of compiled rules ($('{0:N0}' -f $heavy.rules) network rules) | $(& $yes ($engineMb -lt 10)) |")
    }
    if ($light) {
        $out.Add("| Active tab with a light page | 80 to 150 MB across all engine processes | $(Format-Mb $light.engine_mb) MB of engine, $(Format-Mb $light.total_mb) MB in total | $(& $yes ($light.engine_mb -le 150)) |")
    }
    $twenty = Get-Row '20-tabs'
    if ($twenty) {
        foreach ($browser in $others) {
            $other = Get-Row '20-tabs' $browser
            if (-not $other -or $other.total_mb -le 0) { continue }
            $less = 100 * (1 - $twenty.total_mb / $other.total_mb)
            $out.Add("| 20 tabs with 19 asleep, clearly less than $browser | At least 40 % less | $(Format-Mb $twenty.total_mb) MB against $(Format-Mb $other.total_mb) MB: $('{0:N0}' -f $less) % less | $(& $yes ($less -ge 40)) |")
        }
    }
    $out.Add('| Suspended tab | Much less than an active tab | No scenario: "20-tabs" discards the sleeping tabs (no view), which is the state with the lowest cost | Not measured |')
    $out.Add('| History index | A few MB with tens of thousands of entries | The scenarios start with an empty history | Not measured |')
    $out
}

if ($Csv) {
    $script:Rows | Select-Object scenario, browser, total_mb, app_mb, engine_mb, processes, note |
        ConvertTo-Csv -NoTypeInformation | ForEach-Object { $_ -replace '"', '' } |
        Set-Content -Path $Csv -Encoding UTF8
}

if ($Report) {
    $lines = New-Object System.Collections.Generic.List[string]
    $lines.Add('# Memory benchmarks')
    $lines.Add('')
    $lines.Add('`tools/measure.ps1` generated this report. The figures are the private working set of `nea.exe` and of all the engine processes (WebView2). Each figure is the median of ' + $Samples + ' samples after ' + $SettleSec + ' s of settling. Each scenario starts a new Nea with temporary data.')
    $lines.Add('')
    $lines.Add("- Date: $(Get-Date -Format 'yyyy-MM-dd HH:mm')")
    $lines.Add("- Commit: $commit")
    $lines.Add("- Windows: $($os.Caption) $($os.Version), $ramGb GB of RAM")
    if ($script:EngineVersion) { $lines.Add("- WebView2 engine: $($script:EngineVersion)") }
    foreach ($browser in $others) { $lines.Add("- $($browser): $($script:Foreign[$browser].Version) (own temporary profile, same pages)") }
    if ($EngineArgs) { $lines.Add('- Extra engine arguments: `' + $EngineArgs + '`') }
    if ($script:ShieldFiles.Count -gt 0) { $lines.Add("- Block lists: $($script:ShieldFiles -join ', ') plus the built-in minimal one") }
    $lines.Add("- Light page: $LightUrl · heavy page: $HeavyUrl")
    $lines.Add('')
    $header = '| Scenario | Nea (MB) | app | engine | processes |'
    $rule = '|---|---:|---:|---:|---:|'
    foreach ($browser in $others) { $header += " $browser (MB) | Savings |"; $rule += '---:|---:|' }
    $lines.Add($header + ' Note |')
    $lines.Add($rule + '---|')
    foreach ($row in ($script:Rows | Where-Object { $_.browser -eq 'Nea' })) {
        $key = $row.scenario -replace '^heavy-.*$', 'heavy'
        $line = '| {0} | {1:N1} | {2:N1} | {3:N1} | {4} |' -f $row.scenario, $row.total_mb, $row.app_mb, $row.engine_mb, $row.processes
        foreach ($browser in $others) {
            $other = Get-Row $key $browser
            if ($other -and $other.total_mb -gt 0) {
                $line += ' {0:N1} | {1:N0} % |' -f $other.total_mb, (100 * (1 - $row.total_mb / $other.total_mb))
            } else {
                $line += '  |  |'
            }
        }
        $lines.Add($line + " $($row.note) |")
    }
    $lines.Add('')
    $lines.Add('## Memory budgets')
    $lines.Add('')
    foreach ($l in (Get-BudgetLines)) { $lines.Add($l) }
    $lines.Add('')
    $lines.Add('Notes:')
    $lines.Add('')
    $lines.Add('- "app" is `nea.exe`. "engine" is the total of all its child processes (browser, GPU, network, storage, renderers and the WebView2 crashpad).')
    $lines.Add('- The variants of the heavy page have different settings.json values only (`' + (($HeavyVariants.Keys | ForEach-Object { "$($_): " + ($HeavyVariants[$_] | ConvertTo-Json -Compress) }) -join '`, `') + '`). The `shield_disabled` key is the value of `shield::site_key` (the host without "www."). The note gives the number of requests that the filter engine saw and blocked, from the page load to the last sample.')
    $lines.Add('- The script cannot make Edge and Chrome put tabs to sleep. Thus in "20-tabs" all 20 tabs are live in these browsers. These browsers also do not tell when a page load is complete, so the script waits a fixed time before settling. Their figure includes their own interface (the browser process). In Nea, this is "app".')
    $lines.Add('- "restore-50" starts with a session of 50 tabs. Only the active tab creates its view.')
    $lines.Add('- Regression gate: `tools/measure.ps1 -Baseline docs/benchmarks-baseline.json` exits with code 1 if a scenario is more than its base figure plus the tolerance in the file. `-UpdateBaseline` writes new figures to the file.')
    foreach ($note in $script:Notes) { $lines.Add(''); $lines.Add("_$($note)_") }
    [IO.File]::WriteAllLines((Join-Path (Get-Location) $Report), $lines, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "Report: $Report"
}
if ($script:ShieldCache) { Remove-TempDir $script:ShieldCache }

# ---------- regression gate ----------

$current = [ordered]@{}
foreach ($row in ($script:Rows | Where-Object { $_.browser -eq 'Nea' })) { $current[$row.scenario] = $row.total_mb }

$failed = @()
if ($Baseline -and (Test-Path $Baseline) -and -not $UpdateBaseline) {
    $base = Get-Content -Raw -Path $Baseline | ConvertFrom-Json
    # Baselines from before the scenarios had English names.
    $legacy = @{ 'solo-programa' = 'app-only'; 'liviana' = 'light'; 'pesada-con-bloqueo' = 'heavy-blocking'; 'pesada-sin-bloqueo' = 'heavy-no-blocking'; '20-pestanas' = '20-tabs'; 'restaurar-50' = 'restore-50' }
    foreach ($entry in $base.scenarios.PSObject.Properties) {
        $name = if ($legacy.ContainsKey($entry.Name)) { $legacy[$entry.Name] } else { $entry.Name }
        if (-not $current.Contains($name)) { continue }
        # A percentage of almost nothing is noise: never less than 2 MB of room.
        $limit = [Math]::Max($entry.Value.total_mb * (1 + $entry.Value.tolerance_percent / 100), $entry.Value.total_mb + 2)
        $now = $current[$name]
        $status = if ($now -gt $limit) { 'REGRESSION' } else { 'ok' }
        Write-Host ("  {0,-24} {1,8:N1} MB  base {2:N1} MB  limit {3:N1} MB  {4}" -f $name, $now, $entry.Value.total_mb, $limit, $status)
        if ($now -gt $limit) { $failed += $name }
    }
}
if ($Baseline -and $UpdateBaseline) {
    $scenarios = [ordered]@{}
    foreach ($name in $current.Keys) {
        $tol = if ($name -like 'heavy-*') { $HeavyTolerance } else { $Tolerance }
        $scenarios[$name] = [ordered]@{ total_mb = $current[$name]; tolerance_percent = $tol }
    }
    $doc = [ordered]@{ commit = $commit; date = (Get-Date -Format 'yyyy-MM-dd'); unit = 'MB, private working set'; scenarios = $scenarios }
    [IO.File]::WriteAllText((Join-Path (Get-Location) $Baseline), ($doc | ConvertTo-Json -Depth 5), (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "Baseline updated: $Baseline"
}
if ($failed.Count -gt 0) {
    Write-Host ("Memory regression in: " + ($failed -join ', ')) -ForegroundColor Red
    exit 1
}
