<#
.SYNOPSIS
  Medición de memoria: escenarios fijos, reporte y umbral de regresión.

.DESCRIPTION
  Cada escenario arranca Nea de cero con un NEA_DATA_DIR temporal y el
  control por script (NEA_BENCH=1, en un pipe propio), lo lleva al estado
  del escenario, espera a que se asiente y toma la memoria de trabajo privada
  de nea.exe más todo el árbol de procesos del motor (mediana de varias
  muestras). Con -Edge y -Chrome abre las mismas páginas en Edge y en Chrome
  (cada uno con un perfil temporal propio) y suma su árbol del mismo modo.

  Bloqueo: antes de los escenarios una corrida de preparación deja
  que Nea baje EasyList y EasyPrivacy una vez; esa caché se copia a cada
  escenario, así todos compilan las mismas reglas y ninguno descarga listas
  mientras se mide. Los escenarios con vistas esperan a que el motor de
  filtros esté compilado antes de abrir la página, y la página pesada anota
  cuántas solicitudes vio y bloqueó (comando `shield` del pipe).

  Escenarios:
    1 solo-programa         el programa sin vistas (sin motor)
    2 about-blank           una pestaña con about:blank
    3 liviana               una página liviana (-LightUrl)
    4 pesada-<variante>     una página pesada (-HeavyUrl) en cada variante de
                            -HeavyVariants (con y sin bloqueo)
    5 20-pestanas           20 pestañas cargadas, 19 dormidas
    6 restaurar-50          arranque con una sesión de 50 pestañas

  Umbral: con -Baseline compara contra el archivo y termina con código 1 si
  algún escenario sube más que su tolerancia; -UpdateBaseline lo reescribe
  con las cifras de esta corrida.

.EXAMPLE
  tools\measure.ps1 -Build -Edge -Chrome -Report docs\medicion.md -Csv docs\medicion.csv -UpdateBaseline
.EXAMPLE
  tools\measure.ps1 -Baseline docs\medicion-baseline.json   # falla si hay regresión
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
        if ($LASTEXITCODE -ne 0) { throw 'cargo build falló' }
    } finally { Pop-Location }
}
$Exe = (Resolve-Path $Exe).Path
$BenchExe = (Resolve-Path $BenchExe).Path

$heavyHost = ([Uri]$HeavyUrl).Host -replace '^www\.', ''
if (-not $HeavyVariants) {
    $HeavyVariants = [ordered]@{
        'con-bloqueo' = @{}
        'sin-bloqueo' = @{ shield_disabled = @($heavyHost) }
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
    Write-Warning "No se pudo borrar $dir"
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
    Write-Host ("  {0,-24} {1,-8} {2,8:N1} MB  ({3} procesos){4}" -f $scenario, $browser, $row.total_mb, $row.processes, $(if ($note) { "  - $note" } else { '' }))
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
    if ($ping.pid -ne $process.Id) { throw "El pipe lo atiende otro proceso ($($ping.pid))" }
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
    throw 'El motor de filtros no terminó de compilar en 20 s'
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
    "$blocked de $requests solicitudes bloqueadas"
}

function Wait-Tab($tab) {
    $arguments = @{ timeout_ms = $LoadTimeoutSec * 1000 }
    if ($null -ne $tab) { $arguments.tab = $tab }
    $reply = Invoke-Nea wait $arguments -NoThrow
    if ($reply.ok) { return '' }
    "no terminó de cargar en $LoadTimeoutSec s"
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

function Invoke-ForeignScenario([string]$browser, [string]$name, [string[]]$urls) {
    $b = $script:Foreign[$browser]
    if (-not $b.Wanted -or -not (Test-Wanted $name)) { return }
    $exe = Find-Browser $browser
    if (-not $exe) {
        $note = "$browser no está instalado: sin comparación."
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

function Invoke-OthersScenario([string]$name, [string[]]$urls) {
    foreach ($browser in $script:Foreign.Keys) { Invoke-ForeignScenario $browser $name $urls }
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
            $script:Notes.Add('No se pudieron bajar EasyList/EasyPrivacy: el bloqueo usó sólo la lista mínima incorporada.')
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
Write-Host "Asentamiento $SettleSec s, $Samples muestras por escenario (mediana)."
if ($Only.Count -eq 0 -or @($Only | Where-Object { $_ -ne 'solo-programa' }).Count -gt 0) {
    Write-Host 'Preparando la caché de listas de bloqueo...'
    Initialize-ShieldCache
}

Invoke-NeaScenario 'solo-programa' @{} $null { '' }

Invoke-NeaScenario 'about-blank' @{} $null {
    $tab = (Invoke-Nea open @{ url = 'about:blank' }).tab
    Wait-Tab $tab
}
Invoke-OthersScenario 'about-blank' @('about:blank')

Invoke-NeaScenario 'liviana' @{} $null {
    $tab = (Invoke-Nea open @{ url = $LightUrl }).tab
    Wait-Tab $tab
}
Invoke-OthersScenario 'liviana' @($LightUrl)

foreach ($variant in $HeavyVariants.Keys) {
    Invoke-NeaScenario "pesada-$variant" $HeavyVariants[$variant] $null {
        $tab = Open-Filtered $HeavyUrl
        Wait-Tab $tab
    }
}
Invoke-OthersScenario 'pesada' @($HeavyUrl)

Invoke-NeaScenario '20-pestanas' @{} $null {
    $late = 0
    foreach ($url in $manyUrls) {
        $tab = (Invoke-Nea open @{ url = $url }).tab
        if (Wait-Tab $tab) { $late++ }
    }
    $slept = (Invoke-Nea discard).discarded
    $note = "$slept dormidas"
    if ($late) { $note += ", $late sin terminar de cargar" }
    $note
}
Invoke-OthersScenario '20-pestanas' $manyUrls

Invoke-NeaScenario 'restaurar-50' @{} $restoreSession {
    $state = Invoke-Nea state
    $note = "$(@($state.tabs).Count) pestañas restauradas"
    $late = Wait-Tab $null
    if ($late) { $note += ", la activa $late" }
    $note
}

# ---------- report ----------

$commit = (git -C $root rev-parse --short HEAD 2>$null)
$dirty = (git -C $root status --porcelain 2>$null)
if ($dirty) { $commit = "$commit (con cambios sin commitear)" }
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
    $yes = { param($ok) if ($ok) { 'Sí' } else { '**No**' } }
    $out.Add('| Presupuesto o criterio | Objetivo | Medido | ¿Se cumple? |')
    $out.Add('|---|---|---|---|')
    $neaRows = @($script:Rows | Where-Object { $_.browser -eq 'Nea' })
    $solo = Get-Row 'solo-programa'
    if ($neaRows.Count -gt 0) {
        $maxApp = ($neaRows | Measure-Object -Property app_mb -Maximum).Maximum
        $soloText = if ($solo) { "$(Format-Mb $solo.app_mb) MB sin vistas; " } else { '' }
        $out.Add("| Proceso propio (interfaz + lógica) | < 20 MB en reposo (máx. unas pocas decenas) | $($soloText)máximo $(Format-Mb $maxApp) MB en todos los escenarios | $(& $yes ($maxApp -lt 20)) |")
    }
    $restore = Get-Row 'restaurar-50'
    $light = Get-Row 'liviana'
    if ($restore -and $light) {
        $perApp = ($restore.app_mb - $light.app_mb) / 49
        $perTotal = ($restore.total_mb - $light.total_mb) / 49
        # Rows are rounded to 0.1 MB: spread over 49 tabs that is ~0.002 MB.
        $perText = if ($perApp -le 0.002) { 'Menos de 0.002 MB (bajo la resolución)' } else { "$('{0:N3}' -f $perApp) MB" }
        $out.Add("| Pestaña dormida (sólo metadatos) | < 0,5 MB cada una | $perText del programa por pestaña (restaurar-50 contra liviana, 49 dormidas); contando el motor, $('{0:N2}' -f $perTotal) MB | $(& $yes ($perApp -lt 0.5)) |")
    }
    $heavy = @($script:Shield.Values | Where-Object { $_.heap_bytes }) | Select-Object -First 1
    if ($heavy) {
        $engineMb = $heavy.heap_bytes / 1MB
        $out.Add("| Motor de filtros | Pocos MB | $('{0:N1}' -f $engineMb) MB de reglas compiladas ($('{0:N0}' -f $heavy.rules) reglas de red) | $(& $yes ($engineMb -lt 10)) |")
    }
    if ($light) {
        $out.Add("| Pestaña activa con página liviana | 80 a 150 MB entre todos los procesos del motor | $(Format-Mb $light.engine_mb) MB de motor, $(Format-Mb $light.total_mb) MB en total | $(& $yes ($light.engine_mb -le 150)) |")
    }
    $twenty = Get-Row '20-pestanas'
    if ($twenty) {
        foreach ($browser in $others) {
            $other = Get-Row '20-pestanas' $browser
            if (-not $other -or $other.total_mb -le 0) { continue }
            $less = 100 * (1 - $twenty.total_mb / $other.total_mb)
            $out.Add("| 20 pestañas con 19 dormidas, claramente menos que $browser | Al menos 40 % menos | $(Format-Mb $twenty.total_mb) MB contra $(Format-Mb $other.total_mb) MB: $('{0:N0}' -f $less) % menos | $(& $yes ($less -ge 40)) |")
        }
    }
    $out.Add('| Pestaña suspendida | Reducción fuerte respecto de la activa | Sin escenario propio: "20-pestanas" duerme con descarte (vista destruida), el estado de menor costo | Sin medir |')
    $out.Add('| Índice de historial | Pocos MB con decenas de miles de entradas | Los escenarios arrancan con historial vacío | Sin medir |')
    $out
}

if ($Csv) {
    $script:Rows | Select-Object scenario, browser, total_mb, app_mb, engine_mb, processes, note |
        ConvertTo-Csv -NoTypeInformation | ForEach-Object { $_ -replace '"', '' } |
        Set-Content -Path $Csv -Encoding UTF8
}

if ($Report) {
    $lines = New-Object System.Collections.Generic.List[string]
    $lines.Add('# Medición de memoria')
    $lines.Add('')
    $lines.Add('Generado por `tools/measure.ps1`. Memoria de trabajo privada: `nea.exe` más todo el árbol de procesos del motor (WebView2), mediana de ' + $Samples + ' muestras tras ' + $SettleSec + ' s de asentamiento. Cada escenario arranca de cero con datos temporales.')
    $lines.Add('')
    $lines.Add("- Fecha: $(Get-Date -Format 'yyyy-MM-dd HH:mm')")
    $lines.Add("- Commit: $commit")
    $lines.Add("- Windows: $($os.Caption) $($os.Version), $ramGb GB de RAM")
    if ($script:EngineVersion) { $lines.Add("- Motor WebView2: $($script:EngineVersion)") }
    foreach ($browser in $others) { $lines.Add("- $($browser): $($script:Foreign[$browser].Version) (perfil temporal propio, mismas páginas)") }
    if ($EngineArgs) { $lines.Add('- Argumentos extra del motor: `' + $EngineArgs + '`') }
    if ($script:ShieldFiles.Count -gt 0) { $lines.Add("- Listas de bloqueo: $($script:ShieldFiles -join ', ') más la mínima incorporada") }
    $lines.Add("- Página liviana: $LightUrl · página pesada: $HeavyUrl")
    $lines.Add('')
    $header = '| Escenario | Nea (MB) | programa | motor | procesos |'
    $rule = '|---|---:|---:|---:|---:|'
    foreach ($browser in $others) { $header += " $browser (MB) | Ahorro |"; $rule += '---:|---:|' }
    $lines.Add($header + ' Nota |')
    $lines.Add($rule + '---|')
    foreach ($row in ($script:Rows | Where-Object { $_.browser -eq 'Nea' })) {
        $key = $row.scenario -replace '^pesada-.*$', 'pesada'
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
    $lines.Add('## Presupuestos de memoria')
    $lines.Add('')
    foreach ($l in (Get-BudgetLines)) { $lines.Add($l) }
    $lines.Add('')
    $lines.Add('Notas:')
    $lines.Add('')
    $lines.Add('- "programa" es `nea.exe`; "motor" suma todos sus procesos hijos (navegador, GPU, red, almacenamiento, renderizadores, crashpad de WebView2).')
    $lines.Add('- Las variantes de la página pesada difieren sólo en settings.json (`' + (($HeavyVariants.Keys | ForEach-Object { "$($_): " + ($HeavyVariants[$_] | ConvertTo-Json -Compress) }) -join '`, `') + '`); la clave de `shield_disabled` es la de `shield::site_key` (el host sin "www."). La nota cuenta las solicitudes que vio y bloqueó el motor de filtros desde que se abrió la página hasta la última muestra.')
    $lines.Add('- En Edge y Chrome no se puede forzar el sueño de pestañas: en "20-pestanas" las 20 están vivas, que es lo que cuesta abrirlas ahí. Tampoco avisan cuándo terminó de cargar: se les da una espera fija antes del asentamiento. Su número incluye su propia interfaz (proceso del navegador), que en Nea es "programa".')
    $lines.Add('- "restaurar-50" arranca con una sesión de 50 pestañas: sólo la activa crea su vista.')
    $lines.Add('- Umbral de regresión: `tools/measure.ps1 -Baseline docs/medicion-baseline.json` sale con código 1 si un escenario supera su cifra base más la tolerancia del archivo; `-UpdateBaseline` la reescribe.')
    foreach ($note in $script:Notes) { $lines.Add(''); $lines.Add("_$($note)_") }
    [IO.File]::WriteAllLines((Join-Path (Get-Location) $Report), $lines, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "Reporte: $Report"
}
if ($script:ShieldCache) { Remove-TempDir $script:ShieldCache }

# ---------- regression gate ----------

$current = [ordered]@{}
foreach ($row in ($script:Rows | Where-Object { $_.browser -eq 'Nea' })) { $current[$row.scenario] = $row.total_mb }

$failed = @()
if ($Baseline -and (Test-Path $Baseline) -and -not $UpdateBaseline) {
    $base = Get-Content -Raw -Path $Baseline | ConvertFrom-Json
    foreach ($entry in $base.scenarios.PSObject.Properties) {
        if (-not $current.Contains($entry.Name)) { continue }
        # A percentage of almost nothing is noise: never less than 2 MB of room.
        $limit = [Math]::Max($entry.Value.total_mb * (1 + $entry.Value.tolerance_percent / 100), $entry.Value.total_mb + 2)
        $now = $current[$entry.Name]
        $status = if ($now -gt $limit) { 'REGRESIÓN' } else { 'ok' }
        Write-Host ("  {0,-24} {1,8:N1} MB  base {2:N1} MB  límite {3:N1} MB  {4}" -f $entry.Name, $now, $entry.Value.total_mb, $limit, $status)
        if ($now -gt $limit) { $failed += $entry.Name }
    }
}
if ($Baseline -and $UpdateBaseline) {
    $scenarios = [ordered]@{}
    foreach ($name in $current.Keys) {
        $tol = if ($name -like 'pesada-*') { $HeavyTolerance } else { $Tolerance }
        $scenarios[$name] = [ordered]@{ total_mb = $current[$name]; tolerance_percent = $tol }
    }
    $doc = [ordered]@{ commit = $commit; date = (Get-Date -Format 'yyyy-MM-dd'); unit = 'MB, private working set'; scenarios = $scenarios }
    [IO.File]::WriteAllText((Join-Path (Get-Location) $Baseline), ($doc | ConvertTo-Json -Depth 5), (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "Baseline actualizado: $Baseline"
}
if ($failed.Count -gt 0) {
    Write-Host ("Regresión de memoria en: " + ($failed -join ', ')) -ForegroundColor Red
    exit 1
}
