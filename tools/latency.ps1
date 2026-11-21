<#
.SYNOPSIS
  Latencia del campo: desde Enter hasta que la página empieza a navegar, en
  frío y en caliente, más una pasada de uso pesado que junta los cuelgues.

.DESCRIPTION
  Cada corrida arranca Nea de cero con un NEA_DATA_DIR temporal, el
  control por script en un pipe propio y NEA_TRACE=1, que deja en
  trace.log una marca con hora para cada paso (Enter, vista pedida, vista
  lista, NavigationStarting, ContentLoading, NavigationCompleted). El script
  lee esas marcas: no mide desde afuera, así que no cuenta lo que tarda el
  pipe en ir y volver.

  El historial se siembra con -HistoryEntries entradas falsas (como uno de
  años de uso) y las listas de bloqueo se bajan una vez y se copian a cada
  corrida, como en measure.ps1.

  Casos:
    frio            sin pestañas: el campo abierto al arrancar, Enter con el motor apagado
    frio-sesion     con una sesión guardada: Ctrl+T y Enter apenas arranca
    misma-pestana   Ctrl+L y Enter sobre una pestaña viva
    pestana-nueva   Ctrl+T y Enter (con -Tabs pestañas abiertas detrás)
    dormida         elegir una pestaña descartada (hasta que navega)
  Con -Usage: -Tabs pestañas reales, cambios de pestaña, el campo, paneles y
  el temporizador de sueño acelerado; al final, hang.log y los manejadores
  lentos del trace.

  Nada de esto toca el teclado ni el mouse del sistema: todo va por el pipe.
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
    # Prefijo para guardar el trace de cada corrida (<prefijo>-<n>.log).
    [string]$KeepTrace = '',
    # Lo que tarda una persona en escribir la dirección, entre abrir el campo
    # y Enter. Con 0 el pipe escribe al instante: el peor caso, nadie llega
    # a Enter antes de que el motor prepare la vista.
    [int]$TypingMs = 0
)

$ErrorActionPreference = 'Stop'
$Only = @($Only | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
. (Join-Path $PSScriptRoot 'bench.ps1')
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Exe) { $Exe = Join-Path $root 'target\release\nea.exe' }
$Exe = (Resolve-Path $Exe).Path

$HeavyUrls = @(
    'https://www.infobae.com/', 'https://www.lanacion.com.ar/', 'https://www.clarin.com/', 'https://www.youtube.com/',
    'https://es.wikipedia.org/wiki/Argentina', 'https://github.com/rust-lang/rust', 'https://www.reddit.com/',
    'https://www.amazon.com/', 'https://www.mercadolibre.com.ar/', 'https://www.nytimes.com/', 'https://www.bbc.com/',
    'https://edition.cnn.com/', 'https://www.theguardian.com/international', 'https://elpais.com/', 'https://stackoverflow.com/questions',
    'https://news.ycombinator.com/', 'https://www.imdb.com/', 'https://www.espn.com/', 'https://www.booking.com/',
    'https://www.pagina12.com.ar/', 'https://www.perfil.com/', 'https://www.ambito.com/', 'https://www.tn.com.ar/',
    'https://developer.mozilla.org/es/docs/Web/JavaScript', 'https://www.lemonde.fr/', 'https://www.spiegel.de/',
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
    Write-Warning "No se pudo borrar $dir"
}

function Get-ProcessesMentioning([string]$name, [string]$marker) {
    @(Get-CimInstance Win32_Process -Filter "Name='$name'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($marker) })
}

function Write-History([string]$path, [int]$count) {
    $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    $words = 'noticias', 'deportes', 'receta', 'clima', 'mapa', 'video', 'docs', 'foro', 'tienda', 'banco', 'viaje', 'musica'
    $sb = New-Object System.Text.StringBuilder
    [void]$sb.Append('[')
    for ($i = 0; $i -lt $count; $i++) {
        if ($i) { [void]$sb.Append(',') }
        $w = $words[$i % $words.Count]
        $url = "https://www.sitio$($i % 997)-$w.com/$w/articulo-$i?ref=$($i * 7 % 1000)"
        [void]$sb.Append("{`"url`":`"$url`",`"title`":`"$w $i - Sitio $($i % 997)`",`"visits`":$(1 + $i % 13),`"last`":$($now - $i * 37)}")
    }
    [void]$sb.Append(']')
    [IO.File]::WriteAllText($path, $sb.ToString())
}

$script:ShieldCache = $null
$script:HistoryFile = $null
$script:Runs = @()

function Start-Run([hashtable]$settings, $session) {
    $dir = New-TempDir 'nea-latency'
    $all = @{ bench_enabled = $true }
    foreach ($k in $settings.Keys) { $all[$k] = $settings[$k] }
    [IO.File]::WriteAllText((Join-Path $dir 'settings.json'), ($all | ConvertTo-Json -Compress -Depth 5))
    if ($session) { [IO.File]::WriteAllText((Join-Path $dir 'session.json'), ($session | ConvertTo-Json -Compress -Depth 5)) }
    if ($script:HistoryFile) { Copy-Item -LiteralPath $script:HistoryFile -Destination (Join-Path $dir 'history.json') }
    if ($script:ShieldCache) { Copy-Item -Recurse -LiteralPath $script:ShieldCache -Destination (Join-Path $dir 'shield') }
    $env:NEA_DATA_DIR = $dir
    $env:NEA_BENCH = '1'
    $env:NEA_TRACE = '1'
    $env:NEA_BENCH_PIPE = 'latency-' + [Guid]::NewGuid().ToString('N').Substring(0, 6)
    $process = Start-Process -FilePath $Exe -PassThru
    $script:Runs += $process.Id
    Connect-Nea -TimeoutSec 20
    $ping = Invoke-Nea ping
    if ($ping.pid -ne $process.Id) { throw "El pipe lo atiende otro proceso ($($ping.pid))" }
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
    $result = [ordered]@{ navegacion = $null; contenido = $null; completa = $null }
    if ($nav) {
        $null = $nav.text -match 'tab=(\d+)'
        $tab = $Matches[1]
        $result.navegacion = [Math]::Round($nav.ms - $start.ms, 1)
        foreach ($pair in @(@('contenido', "content loading tab=$tab$"), @('completa', "navigation completed tab=$tab "))) {
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
        if ($m -and $null -ne $m.completa) { return $m }
    }
    Measure-Marks (Read-Trace $run) $from $after
}

function Last-Ms($run) {
    $t = Read-Trace $run
    if ($t.Count) { $t[-1].ms } else { 0 }
}

$script:Results = New-Object System.Collections.Generic.List[object]

function Add-Result([string]$case, $m) {
    if (-not $m) { Write-Host "  $case  sin marcas"; return }
    $row = [pscustomobject]@{ caso = $case; navegacion_ms = $m.navegacion; contenido_ms = $m.contenido; completa_ms = $m.completa }
    $script:Results.Add($row)
    Write-Host ("  {0,-16} Enter->navegación {1,8} ms   ->contenido {2,8} ms   ->completa {3,8} ms" -f $case, $m.navegacion, $m.contenido, $m.completa)
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
            Write-Warning 'No se bajaron las listas: sólo la mínima incorporada.'
        }
    } finally { Stop-Run $run }
    $script:HistoryFile = Join-Path ([IO.Path]::GetTempPath()) ("nea-latency-history-" + [Guid]::NewGuid().ToString('N').Substring(0, 8) + '.json')
    Write-History $script:HistoryFile $HistoryEntries
}

Write-Host "Nea: $Exe"
Initialize-Cache
try {
    if (Test-Wanted 'frio') {
        for ($r = 0; $r -lt $Repeat; $r++) {
            $run = Start-Run @{} $null
            try {
                Start-Sleep -Seconds 2
                # The field is already open (no tabs): type and Enter, engine off.
                if ($TypingMs -gt 0) {
                    # The first keystroke, then the rest at a person's pace.
                    Invoke-Nea type @{ text = $TargetUrl.Substring(0, 1) } | Out-Null
                    Start-Sleep -Milliseconds $TypingMs
                    Invoke-Nea type @{ text = $TargetUrl.Substring(1); submit = $true } | Out-Null
                } else {
                    Invoke-Nea type @{ text = $TargetUrl; submit = $true } | Out-Null
                }
                Add-Result 'frio' (Wait-Navigation $run '^submit' 0)
            } finally { Stop-Run $run }
        }
    }

    if (Test-Wanted 'frio-sesion') {
        $session = @{ tabs = @(@{ url = 'https://example.org/'; title = 'Example'; pinned = $false }); active = 0 }
        for ($r = 0; $r -lt $Repeat; $r++) {
            $run = Start-Run @{} $session
            try {
                Submit-Field 'new_tab' $TargetUrl
                Add-Result 'frio-sesion' (Wait-Navigation $run '^submit' 0)
            } finally { Stop-Run $run }
        }
    }

    if ((Test-Wanted 'misma-pestana') -or (Test-Wanted 'pestana-nueva') -or (Test-Wanted 'dormida')) {
        $run = Start-Run @{} $null
        try {
            $first = (Invoke-Nea open @{ url = 'https://example.org/' }).tab
            Invoke-Nea wait @{ tab = $first; timeout_ms = 30000 } | Out-Null
            if ($Tabs -gt 0) { Open-Background $Tabs | Out-Null; Start-Sleep -Seconds 15 }
            Invoke-Nea run @{ name = 'go_to_tab_1' } | Out-Null
            Start-Sleep -Seconds 1
            if (Test-Wanted 'misma-pestana') {
                for ($r = 0; $r -lt $Repeat; $r++) {
                    $after = Last-Ms $run
                    $url = $TargetUrl + "?m=$r"
                    Submit-Field 'focus_address' $url
                    Add-Result 'misma-pestana' (Wait-Navigation $run '^submit' $after)
                }
            }
            if (Test-Wanted 'pestana-nueva') {
                for ($r = 0; $r -lt $Repeat; $r++) {
                    $after = Last-Ms $run
                    Submit-Field 'new_tab' ($TargetUrl + "?n=$r")
                    Add-Result 'pestana-nueva' (Wait-Navigation $run '^submit' $after)
                    Invoke-Nea run @{ name = 'go_to_tab_1' } | Out-Null
                    Start-Sleep -Seconds 2
                }
            }
            if (Test-Wanted 'dormida') {
                for ($r = 0; $r -lt $Repeat; $r++) {
                    Invoke-Nea run @{ name = 'go_to_tab_1' } | Out-Null
                    Start-Sleep -Milliseconds 500
                    Invoke-Nea discard | Out-Null
                    Start-Sleep -Seconds 2
                    $after = Last-Ms $run
                    # The last tab is one opened in the background (discard only
                    # takes those): now asleep, the switch wakes it.
                    Invoke-Nea run @{ name = 'go_to_tab_9' } | Out-Null
                    Add-Result 'dormida' (Wait-Navigation $run '^command go_to_tab_9' $after)
                }
            }
        } finally { Stop-Run $run }
    }

    if ($Usage) {
        Write-Host "Uso pesado: $Tabs pestañas, $UsageSec s"
        $run = Start-Run @{ suspend_after_secs = 10; discard_after_secs = 30 } $null
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
                    Invoke-Nea type @{ text = 'noticias deportes' } | Out-Null
                    Invoke-Nea type @{ text = 'https://www.wikipedia.org/'; submit = $true } | Out-Null
                }
                if ($i % 7 -eq 0) {
                    Invoke-Nea run @{ name = 'history' } | Out-Null
                    Invoke-Nea type @{ text = 'receta' } | Out-Null
                    Invoke-Nea run @{ name = 'focus_address' } | Out-Null
                    Invoke-Nea type @{ text = 'sitio12' } | Out-Null
                }
                if ($i % 9 -eq 0) { Invoke-Nea run @{ name = 'close_tab' } | Out-Null }
                if ($i % 11 -eq 0) { Invoke-Nea run @{ name = 'reopen_tab' } | Out-Null }
            }
            $memory = Invoke-Nea memory
            Write-Host ("  memoria: app {0:N1} MB, total {1:N1} MB, {2} procesos" -f ($memory.app / 1MB), ($memory.total / 1MB), @($memory.processes).Count)
            $trace = Read-Trace $run
            $slow = $trace | Where-Object { $_.text -match '^slow (\S+) (\d+) ([\d.]+) ms' } | ForEach-Object {
                $null = $_.text -match '^slow (\S+) (\d+) ([\d.]+) ms'
                [pscustomobject]@{ que = $Matches[1]; ms = [double]$Matches[3] }
            }
            Write-Host '  manejadores lentos (>4 ms), por tipo:'
            $slow | Group-Object que | Sort-Object { ($_.Group | Measure-Object ms -Maximum).Maximum } -Descending | ForEach-Object {
                $stats = $_.Group | Measure-Object ms -Maximum -Sum -Average
                Write-Host ("    {0,-32} {1,5} veces  máx {2,8:N1} ms  prom {3,7:N1} ms  total {4,9:N1} ms" -f $_.Name, $_.Count, $stats.Maximum, $stats.Average, $stats.Sum)
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
Write-Host 'Mediana por caso (ms desde Enter):'
$script:Results | Group-Object caso | ForEach-Object {
    $median = { param($xs) $s = @($xs | Where-Object { $null -ne $_ } | Sort-Object); if ($s.Count) { $s[[int][Math]::Floor(($s.Count - 1) / 2)] } else { $null } }
    $nav = & $median ($_.Group.navegacion_ms)
    $content = & $median ($_.Group.contenido_ms)
    $done = & $median ($_.Group.completa_ms)
    Write-Host ("  {0,-16} navegación {1,8}   contenido {2,8}   completa {3,8}   ({4} corridas)" -f $_.Name, $nav, $content, $done, $_.Count)
}
if ($Report) { $script:Results | ConvertTo-Json | Set-Content -LiteralPath $Report -Encoding utf8 }
