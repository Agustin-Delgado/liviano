<#
.SYNOPSIS
  Measures the time to come back to a tab that was out of sight.

.DESCRIPTION
  Starts a new Nea with a temporary NEA_DATA_DIR and a session of 9 tabs,
  looks at each tab one time and goes back to the first one. After
  -RestSec, it selects tabs 9 to 6, one after the other. After -IdleSec
  more with no request (the idle trim comes in this time), it selects tabs
  5 to 2. For each tab, the result is the time from the select command to
  the answer of a script in the page: the page is in use again. The time
  of the pipe is in the result, and it is the same for each build.

.EXAMPLE
  tools\return.ps1 -Exe target\release\nea.exe
#>
param(
    [string]$Exe = '',
    [string[]]$Urls = @('https://en.wikipedia.org/wiki/Web_browser', 'https://developer.mozilla.org/en-US/docs/Web/JavaScript', 'https://github.com/rust-lang/rust'),
    [int]$RestSec = 90,
    [int]$IdleSec = 150,
    [System.Collections.IDictionary]$Settings = @{}
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'bench.ps1')
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Exe) { $Exe = Join-Path $root 'target\release\nea.exe' }
$Exe = (Resolve-Path $Exe).Path

$dir = Join-Path ([IO.Path]::GetTempPath()) ('nea-return-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $dir | Out-Null
$all = @{ bench_enabled = $true; check_updates = $false }
foreach ($k in $Settings.Keys) { $all[$k] = $Settings[$k] }
[IO.File]::WriteAllText((Join-Path $dir 'settings.json'), ($all | ConvertTo-Json -Compress -Depth 5))
$tabs = for ($i = 0; $i -lt 9; $i++) { @{ url = $Urls[$i % $Urls.Count]; title = "Tab $i"; pinned = $false } }
$session = @{ tabs = @($tabs); active = 0; vertical_tabs = $false; sidebar_collapsed = $false; window = @(60, 60, 1460, 960); maximized = $false }
[IO.File]::WriteAllText((Join-Path $dir 'session.json'), ($session | ConvertTo-Json -Compress -Depth 6))
New-Item -ItemType Directory -Path (Join-Path $dir 'shield') | Out-Null
[IO.File]::WriteAllText((Join-Path $dir 'shield\updated'), '')
$env:NEA_DATA_DIR = $dir
$env:NEA_BENCH = '1'
$env:NEA_BENCH_PIPE = 'return-' + [Guid]::NewGuid().ToString('N').Substring(0, 6)
$env:NEA_CREDENTIAL_PREFIX = 'nea-return-test'
$process = Start-Process -FilePath $Exe -PassThru

function Select-Tab([int]$n) {
    $watch = [Diagnostics.Stopwatch]::StartNew()
    Invoke-Nea run @{ name = "go_to_tab_$n" } | Out-Null
    Invoke-Nea text | Out-Null
    $watch.Stop()
    [Math]::Round($watch.Elapsed.TotalMilliseconds, 1)
}

try {
    Connect-Nea -TimeoutSec 20
    Invoke-Nea wait @{ timeout_ms = 45000 } -NoThrow | Out-Null
    for ($n = 2; $n -le 9; $n++) {
        Invoke-Nea run @{ name = "go_to_tab_$n" } | Out-Null
        Invoke-Nea wait @{ timeout_ms = 45000 } -NoThrow | Out-Null
    }
    Invoke-Nea run @{ name = 'go_to_tab_1' } | Out-Null
    Write-Host "Nea $Exe"
    $awake = Select-Tab 1
    Write-Host "  tab on screen (pipe and script only): $awake ms"
    Start-Sleep -Seconds $RestSec
    $state = @((Invoke-Nea state).tabs | ForEach-Object { $_.live }) -join ' '
    Write-Host "  after $RestSec s: $state"
    $first = foreach ($n in 9, 8, 7, 6) { Select-Tab $n; Start-Sleep -Milliseconds 800 }
    Write-Host "  back to a tab after $RestSec s: $($first -join ', ') ms"
    Invoke-Nea run @{ name = 'go_to_tab_1' } | Out-Null
    Start-Sleep -Seconds $IdleSec
    $second = foreach ($n in 5, 4, 3, 2) { Select-Tab $n; Start-Sleep -Milliseconds 800 }
    Write-Host "  back to a tab after $IdleSec s more with no input: $($second -join ', ') ms"
} finally {
    try { Invoke-Nea quit | Out-Null } catch { }
    Disconnect-Nea
    if (-not $process.WaitForExit(15000)) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 800
    foreach ($p in @(Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($dir) })) {
        Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue
    }
    Remove-Item Env:NEA_DATA_DIR, Env:NEA_BENCH, Env:NEA_BENCH_PIPE, Env:NEA_CREDENTIAL_PREFIX -ErrorAction SilentlyContinue
    for ($i = 0; $i -lt 10; $i++) {
        try { Remove-Item -Recurse -Force -LiteralPath $dir -ErrorAction Stop; break } catch { Start-Sleep -Milliseconds 500 }
    }
}
