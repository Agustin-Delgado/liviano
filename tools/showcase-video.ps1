<#
.SYNOPSIS
  Records the showcase video for docs/showcase. A separate Nea
  (tools/showcase.ps1) does each step, and PrintWindow captures the frames.
  Then examples/encode_mp4.rs makes the MP4 and the GIF.

.DESCRIPTION
  The recording stops while a page loads from the network. The video joins
  the recorded parts, and each part plays at its real speed. Each scene
  writes its start time on the video clock to scenes.txt. At the end, the
  script writes the scene list to docs/showcase/video-scenes.txt.
#>
[CmdletBinding()]
param(
    [string]$Video = (Join-Path (Split-Path -Parent $PSScriptRoot) 'docs\showcase\nea.mp4'),
    [string]$Gif = (Join-Path (Split-Path -Parent $PSScriptRoot) 'docs\showcase\animations.gif'),
    [int]$Quality = 70
)

. (Join-Path $PSScriptRoot 'showcase.ps1')

$dir = Join-Path $script:Scratch 'video'
Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$scenes = Join-Path $dir 'scenes.txt'

function Pause-For([int]$Ms) { Start-Sleep -Milliseconds $Ms }
function Scene([string]$Name) { Add-Content -Path $scenes -Value "$([NeaShowcase]::Elapsed)`t$Name" -Encoding UTF8 }

# Stops the recording during a page load. The network time is not browser time.
function Wait-Load([int64]$Tab = 0) {
    Suspend-Recording
    $args_ = @{ timeout_ms = 25000 }
    if ($Tab) { $args_.tab = $Tab }
    try { Nea wait $args_ | Out-Null } catch { }
    Start-Sleep -Milliseconds 700
    Resume-Recording
}

# Opens the field with a command and makes sure that the field is open.
# A page that gets the focus at the same time can close the field again.
function Open-Field([string]$Command, [int]$Ms = 400) {
    for ($try = 0; $try -lt 3; $try++) {
        Run $Command; Pause-For $Ms
        if ((Get-State).field.open) { return }
    }
    throw "the field did not open with $Command"
}

function Hover-Tab($Map, [string]$Key, [int]$Ms) {
    $p = $Map[$Key]
    if ($p) { Set-Hover $p[0] $p[1] | Out-Null }
    Pause-For $Ms
}

Disconnect-Nea
Start-Showcase -Theme light | Out-Null
# Two tabs are open when the video starts. Each tab was shown one time.
$wiki = Open-Page 'https://en.wikipedia.org/wiki/Buenos_Aires' -Wait 25000
$github = Open-Page 'https://github.com/microsoft/windows-rs' -Wait 25000
Run next_tab; Pause-For 700
Select-Tab $wiki; Pause-For 800

Start-Recording $dir 33
Pause-For 900

Scene 'Open a tab from the field'
Open-Field new_tab 500
Write-Slowly 'doc.rust-lang.org/book' -DelayMs 75; Pause-For 400
Type-Field 'doc.rust-lang.org/book' -Submit
Pause-For 300
Wait-Load
Pause-For 600

Scene 'Open new tabs in the background'
$opened = @()
foreach ($u in 'https://developer.mozilla.org/en-US/docs/Web/JavaScript', 'https://example.com/', 'https://news.ycombinator.com/') {
    $opened += (Nea open @{ url = $u; background = $true }).tab
    Pause-For 700
}
foreach ($t in $opened) { Wait-Load $t }
Pause-For 400

# Show each tab one time, so that its card has a picture. This part is not recorded.
Suspend-Recording
foreach ($t in $opened) { Select-Tab $t; Start-Sleep -Milliseconds 700 }
Select-Tab $wiki; Start-Sleep -Milliseconds 900
$strip = Get-HitMap -Y 20 -Step 4
Resume-Recording
Pause-For 500

Scene 'The tab card'
Hover-Tab $strip 'Tab(2)' 1500
Hover-Tab $strip 'Tab(3)' 900
Hover-Tab $strip 'Tab(4)' 900
Hover-Tab $strip 'Tab(5)' 900
Clear-Hover; Pause-For 700

Scene 'Change tabs'
foreach ($i in 1..4) { Run next_tab; Pause-For 650 }
foreach ($i in 1..2) { Run prev_tab; Pause-For 650 }
Pause-For 300

Scene 'Move a tab'
$from = $strip['Tab(6)']; $to = $strip['Tab(2)']
Invoke-Drag $from[0] $from[1] $to[0] $to[1] -Steps 40 -StepMs 18
Pause-For 1000

Scene 'Tabs on the side'
Run toggle_tab_layout; Pause-For 1300
Suspend-Recording
$column = Get-HitMap -Column -X 90 -Step 4 -Max 800
Resume-Recording
$tabsInColumn = @($column.Keys | Where-Object { $_ -like 'Tab(*' })
foreach ($k in $tabsInColumn[1..3]) { Hover-Tab $column $k 1000 }
Clear-Hover; Pause-For 600

Scene 'Collapse and expand the column'
Run toggle_sidebar; Pause-For 1300
Run toggle_sidebar; Pause-For 1300
Run toggle_tab_layout; Pause-For 1200
Suspend-Recording
$strip = Get-HitMap -Y 20 -Step 4
Select-Tab $wiki; Start-Sleep -Milliseconds 700
Resume-Recording
Pause-For 400

Scene 'The field with history'
Open-Field focus_address 500
Write-Slowly 'wiki' -DelayMs 140; Pause-For 1400
Close-Field; Pause-For 600

Scene 'Find a tab by name'
Open-Field switch_tab 500
Write-Slowly 'hack' -DelayMs 140; Pause-For 900
Type-Field 'hack' -Submit; Pause-For 1000
Select-Tab $wiki; Pause-For 600

Scene 'Find in page'
Open-Field find 400
# Type the full text in one step. The bench type command clears the field first,
# and an empty search closes the field.
Type-Field 'Argentina'; Pause-For 1600
Close-Field; Pause-For 600

Scene 'Panels'
Run bookmarks; Pause-For 1500
Run history; Pause-For 1500
Run downloads; Pause-For 1500
Close-Field; Pause-For 600

Scene 'Split view'
Open-Field split_page 500
Write-Slowly 'Hacker' -DelayMs 110; Pause-For 500
Send-FieldKey 0x28; Pause-For 400
Send-FieldKey 0x0D; Pause-For 1500
Run focus_left_page; Pause-For 1000
Run focus_right_page; Pause-For 1000
Run focus_left_page; Pause-For 1000
Run unsplit; Pause-For 900

Scene 'Settings and the dark theme'
Run settings; Pause-For 1600
Set-ThemeInSettings dark; Pause-For 1600
Close-Settings; Pause-For 900

Scene 'The dark theme'
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
Stop-Showcase

# The encoder writes its progress to stderr. With 'Stop', PowerShell 5.1
# reads that output as a failure.
$ErrorActionPreference = 'Continue'
# If SHOWCASE_EXE is set, the encoder is in the same build folder.
$encoder = if ($script:ShowcaseExe) { Join-Path (Split-Path -Parent $script:ShowcaseExe) 'examples\encode_mp4.exe' } else { Join-Path $script:Root 'target\release\examples\encode_mp4.exe' }
& $encoder (Join-Path $dir 'timeline.txt') $Video --fps 30 --quality $Quality
# The GIF shows the tab cards, tab changes and a tab move. It is smaller (less than 5 MB).
$marks = Get-Content $scenes -Encoding UTF8 | ForEach-Object { $p = $_ -split "`t"; [pscustomobject]@{ At = [int64]$p[0]; Name = $p[1] } }
$from = ($marks | Where-Object Name -eq 'The tab card').At
$to = ($marks | Where-Object Name -eq 'Tabs on the side').At
& $encoder (Join-Path $dir 'timeline.txt') $Gif --gif --fps 12 --width 560 --from $from --to $to
# Write each scene with its start time in the video (minutes and seconds).
$lines = $marks | ForEach-Object { "{0}:{1:00.0}`t{2}" -f [math]::Floor($_.At / 60000), (($_.At % 60000) / 1000), $_.Name }
[IO.File]::WriteAllLines((Join-Path $script:Root 'docs\showcase\video-scenes.txt'), [string[]]$lines, (New-Object System.Text.UTF8Encoding($false)))
