<#
.SYNOPSIS
  Graba el recorrido de docs/muestra: cuadros con PrintWindow mientras un
  Liviano propio (tools/muestra.ps1) hace cada cosa, y arma el MP4 y el GIF
  con examples/encode_mp4.rs.

.DESCRIPTION
  La grabación se detiene mientras una página carga de la red: el video
  junta los tramos grabados, cada uno a su velocidad real. Cada escena
  anota en escenas.txt el momento en que empieza, sobre el reloj del video.
#>
[CmdletBinding()]
param(
    [string]$Video = (Join-Path (Split-Path -Parent $PSScriptRoot) 'docs\muestra\liviano.mp4'),
    [string]$Gif = (Join-Path (Split-Path -Parent $PSScriptRoot) 'docs\muestra\animaciones.gif'),
    [int]$Quality = 70
)

. (Join-Path $PSScriptRoot 'muestra.ps1')

$dir = Join-Path $script:Scratch 'video'
Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$scenes = Join-Path $dir 'escenas.txt'

function Pause-For([int]$Ms) { Start-Sleep -Milliseconds $Ms }
function Scene([string]$Name) { Add-Content -Path $scenes -Value "$([LivianoMuestra]::Elapsed)`t$Name" -Encoding UTF8 }

# Loads off the record: the network's time is not the browser's.
function Wait-Load([int64]$Tab = 0) {
    Suspend-Recording
    $args_ = @{ timeout_ms = 25000 }
    if ($Tab) { $args_.tab = $Tab }
    try { Lv wait $args_ | Out-Null } catch { }
    Start-Sleep -Milliseconds 700
    Resume-Recording
}

# Opens the field with a command and checks it is up: a command that lands
# while focus is still moving to a page can be undone by it.
function Open-Field([string]$Command, [int]$Ms = 400) {
    for ($try = 0; $try -lt 3; $try++) {
        Run $Command; Pause-For $Ms
        if ((Get-State).field.open) { return }
    }
    throw "el campo no abrió con $Command"
}

function Hover-Tab($Map, [string]$Key, [int]$Ms) {
    $p = $Map[$Key]
    if ($p) { Set-Hover $p[0] $p[1] | Out-Null }
    Pause-For $Ms
}

Disconnect-Liviano
Start-Muestra -Theme light | Out-Null
# Two tabs already there when the video starts, each seen once.
$wiki = Open-Page 'https://es.wikipedia.org/wiki/Buenos_Aires' -Wait 25000
$github = Open-Page 'https://github.com/microsoft/windows-rs' -Wait 25000
Run next_tab; Pause-For 700
Select-Tab $wiki; Pause-For 800

Start-Recording $dir 33
Pause-For 900

Scene 'Abrir una pestaña desde el campo'
Open-Field new_tab 500
Write-Slowly 'doc.rust-lang.org/book' -DelayMs 75; Pause-For 400
Type-Field 'doc.rust-lang.org/book' -Submit
Pause-For 300
Wait-Load
Pause-For 600

Scene 'Pestañas nuevas en segundo plano'
$opened = @()
foreach ($u in 'https://developer.mozilla.org/es/docs/Web/JavaScript', 'https://example.com/', 'https://news.ycombinator.com/') {
    $opened += (Lv open @{ url = $u; background = $true }).tab
    Pause-For 700
}
foreach ($t in $opened) { Wait-Load $t }
Pause-For 400

# Each tab seen once (so its card has a picture), off the record.
Suspend-Recording
foreach ($t in $opened) { Select-Tab $t; Start-Sleep -Milliseconds 700 }
Select-Tab $wiki; Start-Sleep -Milliseconds 900
$strip = Get-HitMap -Y 20 -Step 4
Resume-Recording
Pause-For 500

Scene 'Tarjeta al pasar el mouse'
Hover-Tab $strip 'Tab(2)' 1500
Hover-Tab $strip 'Tab(3)' 900
Hover-Tab $strip 'Tab(4)' 900
Hover-Tab $strip 'Tab(5)' 900
Clear-Hover; Pause-For 700

Scene 'Cambiar de pestaña'
foreach ($i in 1..4) { Run next_tab; Pause-For 650 }
foreach ($i in 1..2) { Run prev_tab; Pause-For 650 }
Pause-For 300

Scene 'Reordenar'
$from = $strip['Tab(6)']; $to = $strip['Tab(2)']
Invoke-Drag $from[0] $from[1] $to[0] $to[1] -Steps 40 -StepMs 18
Pause-For 1000

Scene 'Pestañas al costado'
Run toggle_tab_layout; Pause-For 1300
Suspend-Recording
$column = Get-HitMap -Column -X 90 -Step 4 -Max 800
Resume-Recording
$tabsInColumn = @($column.Keys | Where-Object { $_ -like 'Tab(*' })
foreach ($k in $tabsInColumn[1..3]) { Hover-Tab $column $k 1000 }
Clear-Hover; Pause-For 600

Scene 'Colapsar y expandir la columna'
Run toggle_sidebar; Pause-For 1300
Run toggle_sidebar; Pause-For 1300
Run toggle_tab_layout; Pause-For 1200
Suspend-Recording
$strip = Get-HitMap -Y 20 -Step 4
Select-Tab $wiki; Start-Sleep -Milliseconds 700
Resume-Recording
Pause-For 400

Scene 'El campo con historial'
Open-Field focus_address 500
Write-Slowly 'wiki' -DelayMs 140; Pause-For 1400
Close-Field; Pause-For 600

Scene 'Cambiar de pestaña por nombre'
Open-Field switch_tab 500
Write-Slowly 'hack' -DelayMs 140; Pause-For 900
Type-Field 'hack' -Submit; Pause-For 1000
Select-Tab $wiki; Pause-For 600

Scene 'Buscar en la página'
Open-Field find 400
# Whole: the bench's type empties the field first, and an emptied search
# closes itself.
Type-Field 'Argentina'; Pause-For 1600
Close-Field; Pause-For 600

Scene 'Paneles'
Run bookmarks; Pause-For 1500
Run history; Pause-For 1500
Run downloads; Pause-For 1500
Close-Field; Pause-For 600

Scene 'Vista dividida'
Open-Field split_page 500
Write-Slowly 'Hacker' -DelayMs 110; Pause-For 500
Send-FieldKey 0x28; Pause-For 400
Send-FieldKey 0x0D; Pause-For 1500
Run focus_left_page; Pause-For 1000
Run focus_right_page; Pause-For 1000
Run focus_left_page; Pause-For 1000
Run unsplit; Pause-For 900

Scene 'Ajustes y tema oscuro'
Run settings; Pause-For 1600
Set-ThemeInSettings dark; Pause-For 1600
Close-Settings; Pause-For 900

Scene 'En oscuro'
foreach ($i in 1..2) { Run next_tab; Pause-For 650 }
Hover-Tab $strip 'Tab(3)' 1400
Hover-Tab $strip 'Tab(4)' 900
Clear-Hover; Pause-For 500
Run toggle_tab_layout; Pause-For 1300
Run toggle_sidebar; Pause-For 1100
Run toggle_sidebar; Pause-For 1300
Run toggle_tab_layout; Pause-For 1000
Open-Field new_tab 900
Close-Field; Pause-For 900

Stop-Recording
Stop-Muestra

# The encoder reports progress on stderr, which PowerShell 5.1 would take
# for a failure under 'Stop'.
$ErrorActionPreference = 'Continue'
$encoder = Join-Path $script:Root 'target\release\examples\encode_mp4.exe'
& $encoder (Join-Path $dir 'timeline.txt') $Video --fps 30 --quality $Quality
# The GIF: hover cards, switching and reordering, smaller (under 5 MB).
$marks = Get-Content $scenes -Encoding UTF8 | ForEach-Object { $p = $_ -split "`t"; [pscustomobject]@{ At = [int64]$p[0]; Name = $p[1] } }
$from = ($marks | Where-Object Name -eq 'Tarjeta al pasar el mouse').At
$to = ($marks | Where-Object Name -eq 'Pestañas al costado').At
& $encoder (Join-Path $dir 'timeline.txt') $Gif --gif --fps 12 --width 560 --from $from --to $to
# Each scene at the minute and second it starts in the video.
$lines = $marks | ForEach-Object { "{0}:{1:00.0}`t{2}" -f [math]::Floor($_.At / 60000), (($_.At % 60000) / 1000), $_.Name }
[IO.File]::WriteAllLines((Join-Path $script:Root 'docs\muestra\escenas-video.txt'), [string[]]$lines, (New-Object System.Text.UTF8Encoding($false)))
