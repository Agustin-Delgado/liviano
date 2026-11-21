<#
.SYNOPSIS
  Takes the screenshots for docs/showcase with a separate Nea
  (tools/showcase.ps1).

.DESCRIPTION
  -Part selects the set of screenshots:
    light   all the features in the light theme
    dark    the features that change with the theme
    all     the two sets
  -Keep keeps the browser open. Without -Keep, the script closes the
  browser and deletes its temporary files.

.EXAMPLE
  tools\showcase-shots.ps1 -Part all
#>
[CmdletBinding()]
param([ValidateSet('light', 'dark', 'all')][string]$Part = 'all', [switch]$Keep)

. (Join-Path $PSScriptRoot 'showcase.ps1')

$Sites = [ordered]@{
    wiki    = 'https://en.wikipedia.org/wiki/Buenos_Aires'
    github  = 'https://github.com/microsoft/windows-rs'
    mdn     = 'https://developer.mozilla.org/en-US/docs/Web/JavaScript'
    rust    = 'https://doc.rust-lang.org/book/'
    example = 'https://example.com/'
    hn      = 'https://news.ycombinator.com/'
}

function Wait-Quiet([int]$Ms = 600) { Start-Sleep -Milliseconds $Ms }

# Opens the six sample tabs and shows each tab one time, so that each tab
# card has a picture. Then it selects the Wikipedia tab.
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
    Save-Shot "01-tabs-top-$t" | Out-Null
    Run toggle_tab_layout; Wait-Quiet 900
    Save-Shot "02-tabs-side-$t" | Out-Null
    Run toggle_sidebar; Wait-Quiet 900
    Save-Shot "03-tabs-collapsed-$t" | Out-Null
    Run toggle_sidebar; Wait-Quiet 900
    Run toggle_tab_layout; Wait-Quiet 900
}

function Shoot-Buttons([string]$t) {
    $strip = Get-Strip
    $gear = $strip['Settings']
    Set-Hover $gear[0] $gear[1] | Out-Null; Wait-Quiet 1300
    Save-Shot "04-settings-button-and-window-buttons-$t" | Out-Null
    $close = $strip['Window(Close)']
    Set-Hover $close[0] $close[1] | Out-Null; Wait-Quiet 1300
    Save-Shot "05-close-button-$t" | Out-Null
    Clear-Hover; Wait-Quiet 400
    $strip
}

function Shoot-Card([string]$t, $strip) {
    $tab = $strip['Tab(2)']
    Set-Hover $tab[0] $tab[1] | Out-Null; Wait-Quiet 1600
    Save-Shot "06-tab-card-$t" | Out-Null
    Clear-Hover; Wait-Quiet 400
}

function Shoot-Field([string]$t) {
    Run new_tab; Wait-Quiet
    Save-Shot "08-field-empty-$t" | Out-Null
    Close-Field; Wait-Quiet 300
    Run focus_address; Wait-Quiet 300
    Type-Field 'git'; Wait-Quiet
    Save-Shot "09-field-autocomplete-$t" | Out-Null
    Close-Field; Wait-Quiet 300
    Run switch_tab; Wait-Quiet 300
    Type-Field 'rust'; Wait-Quiet
    Save-Shot "10-switch-tab-$t" | Out-Null
    Close-Field; Wait-Quiet 300
    Run find; Wait-Quiet 300
    Type-Field 'Argentina'; Wait-Quiet 900
    Save-Shot "11-find-in-page-$t" | Out-Null
    Close-Field; Wait-Quiet 300
}

function Shoot-Panels([string]$t) {
    foreach ($p in @(@('bookmarks', '12-bookmarks-panel'), @('history', '13-history-panel'), @('downloads', '14-downloads-panel'), @('passwords', '15-passwords-panel'))) {
        Run $p[0]; Wait-Quiet 700
        Save-Shot "$($p[1])-$t" | Out-Null
        Close-Field; Wait-Quiet 300
    }
}

function Shoot-Settings([string]$t) {
    Run settings; Wait-Quiet 900
    Save-Shot "16-settings-$t" -Class NeaSettings | Out-Null
    Close-Settings; Wait-Quiet 500
}

function Shoot-Split([string]$t, $ids) {
    Select-Tab $ids.wiki; Wait-Quiet
    Run split_page; Wait-Quiet 300
    # Select the open tab in the list of the field: Down selects it, Enter opens it beside the page.
    Type-Field 'Hacker'; Wait-Quiet 400
    Send-FieldKey 0x28; Wait-Quiet 300
    Send-FieldKey 0x0D; Wait-Quiet 1500
    Run focus_right_page; Wait-Quiet 700
    Save-Shot "17-split-view-$t" | Out-Null
    Run unsplit; Wait-Quiet 700
}

function Shoot-Private([string]$t) {
    Run new_private_tab; Wait-Quiet 300
    Save-Shot "18-private-tab-field-$t" | Out-Null
    Type-Field 'https://en.wikipedia.org/wiki/Mate_(drink)' -Submit
    try { Nea wait @{ timeout_ms = 20000 } | Out-Null } catch { }
    Wait-Quiet 1200
    Save-Shot "19-private-tab-$t" | Out-Null
    Run close_tab; Wait-Quiet 600
}

function Shoot-Reader([string]$t, $ids) {
    Select-Tab $ids.wiki; Wait-Quiet
    Run reader_mode; Wait-Quiet 1500
    Save-Shot "20-reader-mode-$t" | Out-Null
    Run reader_mode; Wait-Quiet 800
}

# Ad blocking: the same home page in two new Nea instances. The first has the
# default settings (ad blocking and tracker blocking on). The second has both off.
function Shoot-AdBlock {
    $url = 'https://www.infobae.com/'
    Start-Showcase -Theme light | Out-Null
    Open-Page $url -Wait 30000 | Out-Null
    Wait-Quiet 8000
    $blocked = (Nea shield).blocked
    $with = Save-Shot 'infobae-with'
    Stop-Showcase
    Start-Showcase -Theme light -Settings @{ block_trackers = $false; shield_disabled = @('infobae.com') } | Out-Null
    Open-Page $url -Wait 30000 | Out-Null
    Wait-Quiet 8000
    $without = Save-Shot 'infobae-without'
    Stop-Showcase
    Save-SideBySide $with "Nea out of the box: ad and tracker blocking ($blocked requests blocked)" $without 'Without ad or tracker blocking' '26-ad-blocking-infobae' | Out-Null
    Remove-Item $with, $without
}

Disconnect-Nea
if ($Part -in 'light', 'all') {
    Start-Showcase -Theme light
    $ids = Open-SampleTabs
    Shoot-Layouts light
    $strip = Shoot-Buttons light
    Shoot-Card light $strip
    $first = Save-Shot '00-strip'
    Save-Crop $first '07-site-icons-light' 0 0 1000 40 2 | Out-Null
    Remove-Item $first
    Shoot-Field light
    Shoot-Panels light
    Shoot-Settings light
    Shoot-Split light $ids
    Shoot-Private light
    Shoot-Reader light $ids

    # Hide an element: the picker is on, and the synthetic pointer is on the banner.
    $local = Open-Page (Get-SiteUrl 'hide.html')
    Run hide_element; Wait-Quiet 1000
    Save-Shot '21-hide-element-light' | Out-Null
    Nea click @{ selector = '#promo' } | Out-Null
    Wait-Quiet 800
    Save-Shot '22-element-hidden-light' | Out-Null
    Nea close @{ tab = $local } | Out-Null

    # A window that the page opens without a click.
    $popup = Open-Page (Get-SiteUrl 'popup.html')
    Wait-Quiet 1500
    Save-Shot '23-popup-blocked-light' | Out-Null
    Nea close @{ tab = $popup } | Out-Null
    Select-Tab $ids.wiki; Wait-Quiet

    # The tab's context menu.
    $strip = Get-Strip
    $tab = $strip['Tab(3)']
    Open-ContextMenu $tab[0] $tab[1]; Wait-Quiet 900
    Save-Shot '24-tab-menu-light' | Out-Null
    Close-Menus; Wait-Quiet 500

    # The extensions button and its menu. The extension is a local, disabled test
    # extension. It does not come from a third party.
    $ext = $strip['Extensions']
    Send-Mouse $script:WM_LBUTTONDOWN $ext[0] $ext[1]; Send-Mouse $script:WM_LBUTTONUP $ext[0] $ext[1]
    Wait-Quiet 900
    Save-Shot '25-extensions-button-light' | Out-Null
    Close-Menus; Wait-Quiet 500

    # A sleeping tab: Nea discards the background views, and the tab card shows this.
    Nea discard | Out-Null; Wait-Quiet 800
    $strip = Get-Strip
    $tab = $strip['Tab(4)']
    Set-Hover $tab[0] $tab[1] | Out-Null; Wait-Quiet 1600
    Save-Shot '27-sleeping-tab-light' | Out-Null
    Clear-Hover; Wait-Quiet 400

    # A tab group with its menu open. The group has two adjacent tabs, so no tab moves.
    # A right click on the group label in the tab strip opens the menu.
    Select-Tab $ids.github; Wait-Quiet
    Select-Tab $ids.mdn; Wait-Quiet
    Nea group @{ tabs = @($ids.github, $ids.mdn); name = 'Research'; color = 'blue' } | Out-Null
    Wait-Quiet 900
    $strip = Get-Strip
    $label = @($strip.Keys | Where-Object { $_ -like 'Group(*' })[0]
    if ($label) {
        $at = $strip[$label]
        Open-ContextMenu $at[0] $at[1]; Wait-Quiet 900
        Save-Shot '28-tab-groups-light' | Out-Null
        Close-Menus; Wait-Quiet 500
    } else { Write-Warning 'The tab strip has no group label. The script did not take 28-tab-groups-light.' }
    # Remove the group, so that the dark set has the six tabs again.
    foreach ($t in @($ids.github, $ids.mdn)) { Select-Tab $t; Wait-Quiet 300; Run ungroup_tab; Wait-Quiet 400 }
    Select-Tab $ids.wiki; Wait-Quiet

    if ($Part -eq 'all') {
        Run settings; Wait-Quiet 700
        Set-ThemeInSettings dark; Wait-Quiet 700
        Close-Settings; Wait-Quiet 700
        Write-JsonFile $script:StateFile ((Get-ShowcaseState) | Select-Object pid, server, port, data, @{ n = 'theme'; e = { 'dark' } })
    }
}
if ($Part -eq 'dark') {
    Start-Showcase -Theme dark
    $ids = Open-SampleTabs
}
if ($Part -in 'dark', 'all') {
    if ($Part -eq 'all') { foreach ($key in $Sites.Keys) { Select-Tab $ids[$key]; Wait-Quiet 500 }; Select-Tab $ids.wiki; Wait-Quiet }
    Shoot-Layouts dark
    $strip = Shoot-Buttons dark
    Shoot-Card dark $strip
    Shoot-Field dark
    Shoot-Panels dark
    Shoot-Settings dark
    Shoot-Split dark $ids
    Shoot-Private dark
    Shoot-Reader dark $ids
}
if (-not $Keep) { Stop-Showcase }
if ($Part -in 'light', 'all') { Shoot-AdBlock }
Get-ChildItem $script:Out -Filter *.png | Select-Object Name, Length
