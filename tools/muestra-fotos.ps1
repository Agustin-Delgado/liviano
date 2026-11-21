<#
.SYNOPSIS
  Saca las fotos de docs/muestra con un Nea propio (tools/muestra.ps1).

.DESCRIPTION
  -Part elige qué tanda: 'claro' (todas las funciones en tema claro),
  'oscuro' (las que cambian con el tema) o 'todo'. Deja el navegador
  abierto si se pide -Keep; si no, lo cierra y borra sus temporales.

.EXAMPLE
  tools\muestra-fotos.ps1 -Part todo
#>
[CmdletBinding()]
param([ValidateSet('claro', 'oscuro', 'todo')][string]$Part = 'todo', [switch]$Keep)

. (Join-Path $PSScriptRoot 'muestra.ps1')

$Sites = [ordered]@{
    wiki    = 'https://es.wikipedia.org/wiki/Buenos_Aires'
    github  = 'https://github.com/microsoft/windows-rs'
    mdn     = 'https://developer.mozilla.org/es/docs/Web/JavaScript'
    rust    = 'https://doc.rust-lang.org/book/'
    example = 'https://example.com/'
    hn      = 'https://news.ycombinator.com/'
}

function Wait-Quiet([int]$Ms = 600) { Start-Sleep -Milliseconds $Ms }

# The six sample tabs, each shown once (so each gets its hover picture),
# Wikipedia in front.
function Open-SampleTabs {
    $ids = [ordered]@{}
    foreach ($key in $Sites.Keys) { $ids[$key] = Open-Page $Sites[$key] -Wait 25000 }
    foreach ($key in $Sites.Keys) { Run next_tab; Wait-Quiet 700 }
    Select-Tab $ids.wiki
    Wait-Quiet 800
    $ids
}

function Get-Strip { Get-HitMap -Y 20 -Step 4 }

function Shoot-Layouts([string]$t) {
    Save-Shot "01-pestanas-arriba-$t" | Out-Null
    Run toggle_tab_layout; Wait-Quiet 900
    Save-Shot "02-pestanas-al-costado-$t" | Out-Null
    Run toggle_sidebar; Wait-Quiet 900
    Save-Shot "03-pestanas-colapsadas-$t" | Out-Null
    Run toggle_sidebar; Wait-Quiet 900
    Run toggle_tab_layout; Wait-Quiet 900
}

function Shoot-Buttons([string]$t) {
    $strip = Get-Strip
    $gear = $strip['Settings']
    Set-Hover $gear[0] $gear[1] | Out-Null; Wait-Quiet 1300
    Save-Shot "04-engranaje-y-botones-de-ventana-$t" | Out-Null
    $close = $strip['Window(Close)']
    Set-Hover $close[0] $close[1] | Out-Null; Wait-Quiet 1300
    Save-Shot "05-boton-cerrar-$t" | Out-Null
    Clear-Hover; Wait-Quiet 400
    $strip
}

function Shoot-Card([string]$t, $strip) {
    $tab = $strip['Tab(2)']
    Set-Hover $tab[0] $tab[1] | Out-Null; Wait-Quiet 1600
    Save-Shot "06-tarjeta-de-pestana-$t" | Out-Null
    Clear-Hover; Wait-Quiet 400
}

function Shoot-Field([string]$t) {
    Run new_tab; Wait-Quiet
    Save-Shot "08-campo-vacio-$t" | Out-Null
    Close-Field; Wait-Quiet 300
    Run focus_address; Wait-Quiet 300
    Type-Field 'git'; Wait-Quiet
    Save-Shot "09-campo-autocompletado-$t" | Out-Null
    Close-Field; Wait-Quiet 300
    Run switch_tab; Wait-Quiet 300
    Type-Field 'rust'; Wait-Quiet
    Save-Shot "10-cambiar-de-pestana-$t" | Out-Null
    Close-Field; Wait-Quiet 300
    Run find; Wait-Quiet 300
    Type-Field 'Argentina'; Wait-Quiet 900
    Save-Shot "11-buscar-en-la-pagina-$t" | Out-Null
    Close-Field; Wait-Quiet 300
}

function Shoot-Panels([string]$t) {
    foreach ($p in @(@('bookmarks', '12-panel-marcadores'), @('history', '13-panel-historial'), @('downloads', '14-panel-descargas'), @('passwords', '15-panel-contrasenas'))) {
        Run $p[0]; Wait-Quiet 700
        Save-Shot "$($p[1])-$t" | Out-Null
        Close-Field; Wait-Quiet 300
    }
}

function Shoot-Settings([string]$t) {
    Run settings; Wait-Quiet 900
    Save-Shot "16-ajustes-$t" -Class NeaSettings | Out-Null
    Close-Settings; Wait-Quiet 500
}

function Shoot-Split([string]$t, $ids) {
    Select-Tab $ids.wiki; Wait-Quiet
    Run split_page; Wait-Quiet 300
    # The open tab, from the field's list: Down picks it, Enter pairs it.
    Type-Field 'Hacker'; Wait-Quiet 400
    Send-FieldKey 0x28; Wait-Quiet 300
    Send-FieldKey 0x0D; Wait-Quiet 1500
    Run focus_right_page; Wait-Quiet 700
    Save-Shot "17-vista-dividida-$t" | Out-Null
    Run unsplit; Wait-Quiet 700
}

function Shoot-Private([string]$t) {
    Run new_private_tab; Wait-Quiet 300
    Save-Shot "18-campo-pestana-privada-$t" | Out-Null
    Type-Field 'https://es.wikipedia.org/wiki/Mate_(infusi%C3%B3n)' -Submit
    try { Nea wait @{ timeout_ms = 20000 } | Out-Null } catch { }
    Wait-Quiet 1200
    Save-Shot "19-pestana-privada-$t" | Out-Null
    Run close_tab; Wait-Quiet 600
}

function Shoot-Reader([string]$t, $ids) {
    Select-Tab $ids.wiki; Wait-Quiet
    Run reader_mode; Wait-Quiet 1500
    Save-Shot "20-modo-lectura-$t" | Out-Null
    Run reader_mode; Wait-Quiet 800
}

# Ad blocking: the same front page in two fresh windows, one as Nea
# comes (ad blocking and tracking prevention on) and one with both off.
function Shoot-AdBlock {
    $url = 'https://www.infobae.com/'
    Start-Muestra -Theme light | Out-Null
    Open-Page $url -Wait 30000 | Out-Null
    Wait-Quiet 8000
    $blocked = (Nea shield).blocked
    $with = Save-Shot 'infobae-con'
    Stop-Muestra
    Start-Muestra -Theme light -Settings @{ block_trackers = $false; shield_disabled = @('infobae.com') } | Out-Null
    Open-Page $url -Wait 30000 | Out-Null
    Wait-Quiet 8000
    $without = Save-Shot 'infobae-sin'
    Stop-Muestra
    Save-SideBySide $with "Nea como viene: bloqueo de anuncios y de rastreadores ($blocked solicitudes bloqueadas)" $without 'Sin bloqueo de anuncios ni de rastreadores' '26-bloqueo-de-anuncios-infobae' | Out-Null
    Remove-Item $with, $without
}

Disconnect-Nea
if ($Part -in 'claro', 'todo') {
    Start-Muestra -Theme light
    $ids = Open-SampleTabs
    Shoot-Layouts claro
    $strip = Shoot-Buttons claro
    Shoot-Card claro $strip
    $first = Save-Shot '00-tira'
    Save-Crop $first '07-iconos-de-sitios-claro' 0 0 1000 40 2 | Out-Null
    Remove-Item $first
    Shoot-Field claro
    Shoot-Panels claro
    Shoot-Settings claro
    Shoot-Split claro $ids
    Shoot-Private claro
    Shoot-Reader claro $ids

    # Hide an element: the picker on, the banner under the (synthetic) pointer.
    $local = Open-Page (Get-SiteUrl 'ocultar.html')
    Run hide_element; Wait-Quiet 1000
    Save-Shot '21-ocultar-elemento-claro' | Out-Null
    Nea click @{ selector = '#promo' } | Out-Null
    Wait-Quiet 800
    Save-Shot '22-elemento-oculto-claro' | Out-Null
    Nea close @{ tab = $local } | Out-Null

    # A window the page opens without a click.
    $popup = Open-Page (Get-SiteUrl 'emergente.html')
    Wait-Quiet 1500
    Save-Shot '23-emergente-bloqueada-claro' | Out-Null
    Nea close @{ tab = $popup } | Out-Null
    Select-Tab $ids.wiki; Wait-Quiet

    # The tab's context menu.
    $strip = Get-Strip
    $tab = $strip['Tab(3)']
    Open-ContextMenu $tab[0] $tab[1]; Wait-Quiet 900
    Save-Shot '24-menu-de-pestana-claro' | Out-Null
    Close-Menus; Wait-Quiet 500

    # The extensions button and its menu (a local, disabled extension of
    # the repo: nothing from a third party).
    $ext = $strip['Extensions']
    Send-Mouse $script:WM_LBUTTONDOWN $ext[0] $ext[1]; Send-Mouse $script:WM_LBUTTONUP $ext[0] $ext[1]
    Wait-Quiet 900
    Save-Shot '25-boton-de-extensiones-claro' | Out-Null
    Close-Menus; Wait-Quiet 500

    # A sleeping tab: background views dropped, the card says so.
    Nea discard | Out-Null; Wait-Quiet 800
    $strip = Get-Strip
    $tab = $strip['Tab(4)']
    Set-Hover $tab[0] $tab[1] | Out-Null; Wait-Quiet 1600
    Save-Shot '27-pestana-dormida-claro' | Out-Null
    Clear-Hover

    if ($Part -eq 'todo') {
        Run settings; Wait-Quiet 700
        Set-ThemeInSettings dark; Wait-Quiet 700
        Close-Settings; Wait-Quiet 700
        Write-JsonFile $script:StateFile ((Get-MuestraState) | Select-Object pid, server, port, data, @{ n = 'theme'; e = { 'dark' } })
    }
}
if ($Part -eq 'oscuro') {
    Start-Muestra -Theme dark
    $ids = Open-SampleTabs
}
if ($Part -in 'oscuro', 'todo') {
    if ($Part -eq 'todo') { foreach ($key in $Sites.Keys) { Select-Tab $ids[$key]; Wait-Quiet 500 }; Select-Tab $ids.wiki; Wait-Quiet }
    Shoot-Layouts oscuro
    $strip = Shoot-Buttons oscuro
    Shoot-Card oscuro $strip
    Shoot-Field oscuro
    Shoot-Panels oscuro
    Shoot-Settings oscuro
    Shoot-Split oscuro $ids
    Shoot-Private oscuro
    Shoot-Reader oscuro $ids
}
if (-not $Keep) { Stop-Muestra }
if ($Part -in 'claro', 'todo') { Shoot-AdBlock }
Get-ChildItem $script:Out -Filter *.png | Select-Object Name, Length
