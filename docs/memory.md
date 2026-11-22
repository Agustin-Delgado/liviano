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

The views that are out of sight for 5 min or more keep only half of the
cap (4 by default). Thus eight tabs that you look at one after the other
stay live while you go back and forth, but not for the rest of the day.
With 10 tabs, this decreases the commit after 5 min from 740 MB to 512 MB.
Each live view out of sight costs approximately 64 MB of commit: 50 MB in
its renderer, 8.5 MB in the GPU process and 5.5 MB in the browser process
(1400 × 900 window).

The time that the computer sleeps does not count as idle time: after the
resume, each tab is as idle as it was before the sleep.

Each stage is a call to the engine, and each call stops the window: tens of
milliseconds, and hundreds when the engine is busy. Thus Nea closes at most
one view in one sleep tick, or else lowers or suspends at most 4 views. The
next tick comes after 250 ms, or after three times the length of the last
tick if that is more. Nea does not change a stage while the strip or the
field moves, and it closes a view only after 1 s without a key, a click or
a pointer move (except when memory is low).

Nea also makes at most 2 views at the same time. A tab on screen goes
before the tabs out of sight. When a tab leaves the screen before its view
is started, it stays asleep.

## Minimized window

The window keeps its layout when it is minimized. After 3 s, Nea hides the
views, releases its own drawing target and gives back its working set. The
pages on screen then go through the stages "lowered" and "suspended" from
the time of the minimize. They are never discarded. When the window comes
back, Nea only shows the views again. The first paint takes approximately
8 ms more than before, because Nea makes the drawing target again.

## Window on screen, but nobody uses it

When the session has no input (and no script request) for 2 min, Nea gives
back its own memory one time for each such pause. First the heaps release
the free pages that they keep for later allocations
(`HeapOptimizeResources`), then the working set goes. After 150 rounds of
tab churn, this changed the app process from 25.2 MB working set and
32.1 MB commit to 0.3 MB and 21.3 MB. The minimized window and low memory
use the same step. The first paint after the pause takes the pages back
from memory.

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
- No other engine switches. Each one was measured with 10 tabs (idle,
  total commit; 765 MB without a switch), and none saves memory without a
  cost:

  | Switch | Commit | Why not |
  |---|---:|---|
  | `--js-flags=--optimize-for-size` | 758 MB | In the noise |
  | `--force-gpu-mem-available-mb=128` | 775 MB | No change |
  | `--gpu-program-cache-size-kb=1024 --force-gpu-mem-discardable-limit-mb=32` | 768 MB | No change |
  | `--num-raster-threads=1` | 772 MB | No change, and slower raster |
  | `--disable-gpu-rasterization` | 766 MB | 20 MB less GPU commit with 1 tab, but the CPU draws the pages |
  | `--enable-low-end-device-mode` | 741 MB | 16-bit textures and smaller caches: the pages look worse |
  | `--enable-features=InvisibleWindowSwapchainRecreate` | 767 MB | No change |

- No smaller bounds for a hidden view. The GPU commit of a live view
  increases with the size of the window (8.5 MB for each view at
  1400 × 900, 4 MB at 700 × 500), but the engine keeps it while the view is
  hidden, also when the view is made 1 pixel high. Only a discard frees it.

## Leaks

`tools/leaks.ps1` opens, selects and closes tabs, opens and closes Settings,
and opens the field with its list and closes it, for many rounds. It writes
the commit, the working set, the GDI and USER objects and the handles of
the app process, and the commit of the engine processes.

2026-11-22, 500 rounds (1500 views made and closed, Settings and the field
500 times each, 19 min): the app commit increases from 9 MB to 37 MB in the
first 250 rounds and then stays flat (9 KB for each round in the second
half). GDI objects (59), USER objects (99) and handles (443) stay flat.
Settings alone and the field alone stay flat from the first round. The
engine processes stay at approximately 350 MB with 3 tabs. Thus there is
no leak: the app's heaps keep their largest size, and the idle trim gives
it back.

A discarded tab costs approximately 80 KB in the app process (address,
title, its place on the strip). Icons (300 sites at most), previews (3 MB
at most) and the bitmaps of the strip (only what the last frame drew)
have limits.

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

### Pass 2

2026-11-22, same engine, computer and pages. "Before" is commit 5a5e921.
The 40-tab rows come from a second run of both builds at the same time
(the first run of "before" had low memory during a build and discarded
more tabs). The change is in the phases after 5 min: the smaller cap for
views out of sight for long, and the idle trim of the app.

| Tabs | Phase | Before | After |
|---:|---|---:|---:|
| 1 | idle | 153 / 231 | 159 / 236 |
| 1 | idle 5 min | 134 / 218 | 144 / 223 |
| 1 | minimized | 92 / 207 | 5 / 211 |
| 1 | after churn | 129 / 276 | 125 / 282 |
| 10 | idle | 462 / 762 | 471 / 766 |
| 10 | idle 5 min | 272 / 740 | 198 / 512 |
| 10 | minimized | 14 / 728 | 8 / 498 |
| 10 | after churn | 222 / 870 | 170 / 825 |
| 40 | idle | 429 / 813 | 368 / 808 |
| 40 | idle 5 min | 301 / 795 | 232 / 559 |
| 40 | minimized | 249 / 782 | 5 / 544 |
| 40 | after churn | 263 / 815 | 258 / 808 |

With 10 tabs, the live views out of sight cost 455 MB of the 766 MB (9
renderers), the GPU process 182 MB and the browser process 88 MB. The app
process is 17 MB. The working set while minimized changes much from run to
run, because the engine trims its processes when it wants to.
