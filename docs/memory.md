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
| Suspended | After the "suspend" setting (60 s) | First the view is parked (see below). Then `TrySuspend`: scripts and timers stop, and the working set is trimmed | A resume |
| Discarded | After the "discard" setting (15 min) | The view and its renderer close. The address, the title and the scroll position stay. | The page loads again, at the same scroll position |

### Parked before the suspend

The engine keeps the surfaces of a hidden view at the size that it last
drew (DirectComposition surfaces in the GPU process, 4 to 11 MB for
each view at 1400 × 900, as the page uses more or fewer layers). A smaller size or another parent window
does not free them while the view is hidden: the engine lets them go only
after it draws the view at the new size, and it draws only a visible view.
Thus, when a view is due for its suspend, Nea first parks it: the view is
visible again, but 1000 pixels above the client area of the window (the
system clips it away, so nobody sees it), 1 pixel high and as wide as
before. On the next sleep tick (200 ms or more later) Nea hides and
suspends it. A longer park frees no more (10 tabs, GPU process after
90 s: 145 MB with 200 ms, 600 ms and 1500 ms; 182 to 192 MB without a
park). The width stays the same, so the text keeps its lines and
the page keeps its scroll position.

The page sees this: one `visibilitychange` to "visible" and back to
"hidden" in less than a second, and one `resize` (height 1). When the tab
comes back, it gets one more `resize`. The first frame after the return
took 15 to 46 ms (before: 15 to 174 ms; the views without a park were
slower to come back, not faster). Under memory pressure, and for a page
on screen in a minimized window, there is no park.

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
(1400 × 900 window). After the park, the GPU part is approximately 0.

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

## Back to a discarded tab

A discarded tab loads its page again when it comes back. Its back and
forward history is lost (WebView2 cannot keep it), and so are the form
fields and the state of the page's scripts. Nea keeps the scroll
position: when a tab leaves the screen, Nea reads `scrollX` and `scrollY`
of the page. When the view is made again after a discard and the page
loads at the same address, Nea scrolls it there, if the page is still at
the top. Only the page itself scrolls back, not a box that scrolls inside
it. The hover card keeps its last picture of the page.

## Same site, one environment

All the tabs share one WebView2 environment and one profile (private tabs
use a second profile in the same browser process). Thus there is one
browser process, one GPU process and one network service. Each tab that
the person opens gets its own renderer, also for the same site: 4 tabs of
one site had 4 renderers (approximately 64 MB each with the test page).
`--process-per-site` puts them in one renderer, with the costs above. The
engine's feature `ProcessPerSiteUpToMainFrameThreshold` is in the engine,
but it changed nothing (4 renderers with a threshold of 4).

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

- No smaller bounds while hidden. The GPU commit of a live view
  increases with the size of the window (8.5 MB for each view at
  1400 × 900, 4 MB at 700 × 500), but the engine keeps it while the view is
  hidden, also when the view is made 1 pixel high or 0 × 0, or moved to
  another parent window (pass 3, 10 tabs: GPU 190 and 192 MB, before 179
  to 192 MB). Only a view that draws at the new size frees it: refer to
  "Parked before the suspend".
- No `--disable-direct-composition` and no `--in-process-gpu`. Both save
  much memory (10 tabs: 618 MB and 609 MB of commit, before 745 MB; 1 tab:
  199 MB and 173 MB, before 247 MB): without DirectComposition, the GPU
  process does not keep surfaces for hidden views, and `--in-process-gpu`
  also turns DirectComposition off. But the pages lose frames: a page that
  scrolls at 120 Hz dropped 2 to 19 frames out of 960 (before: 0 or 1),
  and 3 to 104 with `--in-process-gpu`. `--in-process-gpu` also puts the
  GPU driver in the browser process: a GPU crash then ends the engine and
  reloads all the tabs. And without DirectComposition there are no
  hardware overlays for video.
- No other engine switches of pass 3 (1 tab, total commit, 247 to 250 MB
  without a switch): `--enable-features=NetworkServiceInProcess` 248,
  `--gpu-rasterization-msaa-sample-count=0` 248,
  `--disable-accelerated-2d-canvas` 249, `--enable-zero-copy` 247,
  `--disable-gpu-shader-disk-cache` 247, `--disable-features=SkiaGraphite`
  246, `--js-flags=--max-semi-space-size=2` 247. With 10 tabs,
  `--disable-features=DCompPresenter` 746 (before 745). The browser
  process loads only the engine and its telemetry DLLs (`telclient.dll`,
  `oneds.dll`, 7.5 MB of image); there is nothing else to cut there.
- No memory target "low" for the tab on screen. Set after 1 min on a
  visible page, it decreased the working set of the tree from 165 MB to
  100 MB, but it increased the commit from 281 MB to 312 MB, and the page
  lost a few more frames. The working set comes back when the page draws.
- No forced purge through DevTools (`Memory.simulatePressureNotification`
  "critical" and `HeapProfiler.collectGarbage` at the "lowered" stage):
  with 10 tabs, the renderers kept 447 MB (before 446 to 450 MB), and the
  GPU process increased by 8 MB.

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
local pages on the loopback interface only. Without `-Heavy`, it sends
nothing outside the computer: the blocker lists are marked fresh, and the
engine resolves only the loopback addresses. With `-Heavy`, it also opens
some real pages if they are reachable.

The idle phases depend on the person: Nea's idle trim waits for 2 min with
no input in the whole session (`GetLastInputInfo`). A run while somebody
uses the computer does not get it.

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

### Pass 3

2026-11-22, same engine, computer and pages, but no real pages: the run
is offline (tabs 2 to 5 are local pages too). "Before" is commit be0afe8
(release 0.1.5). Each value is the mean of two runs of each build, one
after the other (before, after, before, after). The change is the park
before the suspend: it shows in the phases after the suspend (idle 5 min,
minimized), in the GPU process.

| Tabs | Phase | Before | After |
|---:|---|---:|---:|
| 1 | idle | 154 / 235 | 153 / 234 |
| 1 | idle 5 min | 143 / 222 | 142 / 221 |
| 1 | minimized | 5 / 211 | 6 / 209 |
| 1 | after churn | 131 / 279 | 92 / 270 |
| 10 | idle | 408 / 771 | 440 / 776 |
| 10 | idle 5 min | 318 / 510 | 201 / 486 |
| 10 | minimized | 16 / 497 | 81 / 472 |
| 10 | after churn | 212 / 827 | 223 / 812 |
| 40 | idle | 527 / 858 | 528 / 846 |
| 40 | idle 5 min | 363 / 591 | 240 / 553 |
| 40 | minimized | 21 / 576 | 14 / 538 |
| 40 | after churn | 239 / 821 | 278 / 837 |

GPU process commit with 10 tabs after 5 min: 150 MB before, 127 to 136 MB
after; with 40 tabs, 168 to 210 MB before, 144 to 176 MB after. The
working set after 5 min is also smaller (by approximately 120 MB, in the
renderers), but it changes from run to run more than the commit. The
"minimized" and "after churn" working sets change much from run to run
(the engine trims when it wants to).

A smaller cap is a choice, not a default. With `live_tabs_out_of_sight`
set to 4 (2 after 5 min), the same run gave, with 10 tabs: idle
329 / 535 (default 440 / 776), idle 5 min 55 / 371 (201 / 486); with 40
tabs: idle 387 / 637 (528 / 846), idle 5 min 239 / 481 (240 / 553). The
cost: more tabs are discarded, and a discarded tab loads its page again
when it comes back (approximately 160 ms for the local test page, more
for a real page; a suspended view shows in 15 to 46 ms). It also loses its
back and forward history, its form fields and the state of its scripts.
Its scroll position comes back.
