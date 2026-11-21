<#
.SYNOPSIS
  Measures the cold start of Nea: from the creation of the process to the
  first paint of the window, and to the field that accepts typing.

.DESCRIPTION
  Each run starts a new Nea with a temporary NEA_DATA_DIR and NEA_TRACE=1,
  waits -WaitSec seconds and closes it through the bench pipe. Nea writes
  to trace.log how long before `main` the process was created, and a mark
  for the first paint, the field shown and the first page. The script adds
  them up, so the time that the loader takes is in the result.

  Cases:
    empty     no saved session: the field opens at startup
    session   a saved session of -Tabs tabs (the first one is shown)

  The script sends no keys or clicks to the system.

.EXAMPLE
  tools\startup.ps1 -Exe C:\Temp\neafps\release\nea.exe -Repeat 5
#>
param(
    [string]$Exe = '',
    [int]$Repeat = 5,
    [int]$Tabs = 40,
    [int]$WaitSec = 4
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'bench.ps1')
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Exe) { $Exe = Join-Path $root 'target\release\nea.exe' }
$Exe = (Resolve-Path $Exe).Path

function Stats([double[]]$values) {
    if (-not $values -or $values.Count -eq 0) { return $null }
    $sorted = $values | Sort-Object
    [pscustomobject]@{ p50 = $sorted[[Math]::Floor(($sorted.Count - 1) / 2)]; max = $sorted[-1]; count = $values.Count }
}

function Start-Run([string]$case) {
    $data = Join-Path ([IO.Path]::GetTempPath()) ('neastart-data-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
    New-Item -ItemType Directory -Path $data | Out-Null
    $settings = @{ bench_enabled = $true; field_position = 'middle' }
    [IO.File]::WriteAllText((Join-Path $data 'settings.json'), ($settings | ConvertTo-Json -Compress))
    if ($case -eq 'session') {
        $saved = @()
        for ($i = 0; $i -lt $Tabs; $i++) { $saved += @{ url = "https://example.com/?tab=$i"; title = "Example tab $i"; pinned = $false } }
        $session = @{ tabs = $saved; active = 0; vertical_tabs = $true; sidebar_collapsed = $false; window = @(80, 80, 1480, 980); maximized = $false }
        [IO.File]::WriteAllText((Join-Path $data 'session.json'), ($session | ConvertTo-Json -Compress -Depth 6))
    }
    $env:NEA_DATA_DIR = $data
    $env:NEA_BENCH = '1'
    $env:NEA_TRACE = '1'
    $env:NEA_SCALE = '100'
    $env:NEA_CREDENTIAL_PREFIX = 'nea-startup-test'
    $env:NEA_BENCH_PIPE = 'start-' + [Guid]::NewGuid().ToString('N').Substring(0, 6)
    $process = Start-Process -FilePath $Exe -PassThru
    try {
        Start-Sleep -Seconds $WaitSec
        Connect-Nea -TimeoutSec 10
        try { Invoke-Nea quit | Out-Null } catch { }
        Disconnect-Nea
    } finally {
        if (-not $process.WaitForExit(15000)) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
        Start-Sleep -Milliseconds 500
        foreach ($p in @(Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($data) })) {
            Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue
        }
        Remove-Item Env:NEA_DATA_DIR, Env:NEA_BENCH, Env:NEA_BENCH_PIPE, Env:NEA_TRACE, Env:NEA_SCALE, Env:NEA_CREDENTIAL_PREFIX -ErrorAction SilentlyContinue
    }
    $trace = @(Get-Content -LiteralPath (Join-Path $data 'trace.log'))
    Remove-Item -Recurse -Force -LiteralPath $data -ErrorAction SilentlyContinue
    $at = { param($pattern) $line = $trace | Where-Object { $_ -match $pattern } | Select-Object -First 1; if ($line) { [double](($line.Trim() -split '\s+')[0]) } else { $null } }
    $startLine = $trace | Where-Object { $_ -match 'process started ([\d.]+) ms before' } | Select-Object -First 1
    if (-not $startLine) { return $null }
    $null = $startLine -match 'process started ([\d.]+) ms before'
    $created = [double](($startLine.Trim() -split '\s+')[0]) - [double]$Matches[1]
    $since = { param($t) if ($null -eq $t) { $null } else { $t - $created } }
    [pscustomobject]@{
        paint = & $since (& $at 'first paint$')
        field = & $since (& $at 'field shown$')
        page  = & $since (& $at 'navigation completed tab=')
    }
}

foreach ($case in @('empty', 'session')) {
    $runs = @()
    for ($r = 0; $r -lt $Repeat; $r++) { $run = Start-Run $case; if ($run) { $runs += $run } }
    Write-Host "$case ($($runs.Count) runs, ms from process creation):"
    foreach ($what in @('paint', 'field', 'page')) {
        $s = Stats @($runs | ForEach-Object { $_.$what } | Where-Object { $null -ne $_ })
        if ($s) { Write-Host ("  {0,-6} p50 {1,7:N1}  max {2,7:N1}" -f $what, $s.p50, $s.max) }
    }
}
