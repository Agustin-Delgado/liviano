# Memory policy

This page tells how Nea keeps the memory of its full process tree low. The
tree is `nea.exe` and all its WebView2 processes (browser, GPU, utility,
renderers and crashpad). The code is in `src/memory.rs` and
`src/app/memory_ui.rs`.

## Tabs out of sight

A tab that is not on screen goes through these stages:

| Stage | When | What the engine does | Cost to come back |
|---|---|---|---|
| Lowered | 20 s after the tab left the screen | Memory target level "low": it drops caches and decoded images. Scripts and timers continue. | A few milliseconds |
| Suspended | After the "suspend" setting (60 s) | `TrySuspend`: scripts and timers stop, and the working set is trimmed | A resume |
| Discarded | After the "discard" setting (15 min) | The view and its renderer close. Only the address and the title stay. | The page loads again |

A suspended view gives back its working set, but it keeps most of its
commit (approximately 50 MB for each page in the test). Thus Nea also keeps
a maximum number of live views out of sight: `live_tabs_out_of_sight` in
`settings.json`, 8 by default. Nea discards the views that were seen least
recently first. These tabs are never discarded, and are not counted:
pinned tabs, private tabs, tabs that play sound and sites that you keep
awake.

The time that the computer sleeps does not count as idle time: after the
resume, each tab is as idle as it was before the sleep.

Nea closes at most 3 views in one sleep tick, and it does not close a view
while the strip or the field moves. Closing a view stops the window for
tens of milliseconds.

## Minimized window

The window keeps its layout when it is minimized. After 3 s, Nea hides the
views, releases its own drawing target and gives back its working set. The
pages on screen then go through the stages "lowered" and "suspended" from
the time of the minimize. They are never discarded. When the window comes
back, Nea only shows the views again. The first paint takes approximately
8 ms more than before, because Nea makes the drawing target again.

## Low memory

When the memory load is more than the "memory pressure" setting (85%), or
when Windows signals low memory (`CreateMemoryResourceNotification`), Nea
does these steps immediately:

- It discards each tab out of sight that it can discard.
- It suspends the other tabs out of sight.
- It gives back its own working set (at most each 30 s).
- It does not make a spare view.

## What Nea does not do, and why

- No `--renderer-process-limit` and no `--process-per-site`. Each one puts
  many pages in one renderer. Then one slow page makes the other pages slow,
  and one crash closes all of them.
- No `--disable-features=SpareRendererForSitePerProcess`. The measured
  difference was in the noise (less than 2%).
- No working set trim of the engine processes. The engine manages them, and
  a trim only moves pages to the page file. It does not decrease the commit.

## Measure

`tools/memory.ps1` measures the private working set and the commit of the
process tree of the instance that it starts, by process type. It serves
local pages on the loopback interface only. With `-Heavy`, it also opens
some real pages if they are reachable.

```powershell
tools\memory.ps1 -Exe C:\Temp\nearam\release\nea.exe -Heavy -Out C:\Temp\memory.json
```

The phases are: idle (30 s after all tabs were selected), idle for 5 min,
minimized for 60 s, and after tab churn (3 rounds of 10 tabs opened and
closed).

## Results

2026-11-21, WebView2 157.0.4341.60, Windows 11, 32 GB of RAM. Each tab has
a local page (large DOM, four images, 10 MB of script objects, an animation
and a timer). Tabs 2 to 5 have real pages (Wikipedia, GitHub, MDN,
Infobae). Values are the total of the process tree in MB: private working
set / commit. "Before" is commit f208939.

| Tabs | Phase | Before | After |
|---:|---|---:|---:|
| 1 | idle | 151 / 230 | 151 / 228 |
| 1 | idle 5 min | 141 / 216 | 140 / 215 |
| 1 | minimized | 140 / 204 | 91 / 203 |
| 1 | after churn | 184 / 273 | 129 / 275 |
| 10 | idle | 723 / 1025 | 453 / 939 |
| 10 | idle 5 min | 563 / 986 | 592 / 958 |
| 10 | minimized | 561 / 972 | 538 / 946 |
| 10 | after churn | 715 / 1101 | 552 / 1005 |
| 40 | idle | 1608 / 2936 | 571 / 897 |
| 40 | idle 5 min | 1665 / 2928 | 552 / 868 |
| 40 | minimized | 1666 / 2910 | 28 / 855 |
| 40 | after churn | 1819 / 3064 | 378 / 967 |

The cap gives most of the decrease: with 40 tabs, 31 tabs are discarded,
and the browser and GPU processes also become smaller (each view costs
approximately 5 MB in the browser process and 11 MB of GPU commit). With 1
tab, only the minimized window changes. The working set of real pages
changes from run to run by approximately 10%. The commit is more stable.
