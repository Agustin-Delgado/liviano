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
| Discarded | Only when the system has almost no commit left, or after the "discard" setting (off by default) | The view and its renderer close. The address, the title and the scroll position stay. | The page loads again, at the same scroll position |

By default, a tab is not discarded for its time out of sight: it stays
suspended, and it comes back as it was, with no load. A suspended tab uses
almost no memory of the computer (see "Trim of the suspended renderers").

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

### Trim of the suspended renderers

After a suspend, the renderer of the page still keeps some megabytes in
memory (4 to 9 MB for each tab in the test). The renderer does not run, so
2 s after a suspend Nea empties the working set of each renderer process
that has only suspended pages (`EmptyWorkingSet`). The engine tells which
main frames each renderer has (`GetProcessExtendedInfos`). A frame of a
different site is in its own renderer: Nea goes up through the parent
frames to find the page that the frame is in. A renderer that also has a
live page, or no frame of a page, stays as it is. When the tab comes back,
the system gives the pages back from its standby list or from the page
file.

The engine works again in a suspended renderer from time to time and
brings memory back, for minutes with a heavy page. Thus the trims come
again: 2, 20, 25, 30, 35, 45 and 80 s after the last suspend, and then
each 30 s while a suspended page exists.

The engine can refuse a suspend. Then the view stays awake for Nea, and
Nea tries again after 30 s.

### Screen off, session locked

While the screen is off or the session is locked, nobody can see the
window. Nea hides its views then, as it does for a minimized window, and
shows them again when the screen comes on or the session is unlocked. The
engine does not draw a hidden view. Sound continues. Without this, a page
with an animation that never stops (a small dot that pulses is sufficient)
used 17 s of processor time each minute on a 120 Hz screen that was off;
with it, 0.7 s. Edge and Chrome also stop then.

A test run (`NEA_BENCH`) does not do this: it measures the pages as a
person sees them, also at night.

### Window covered by a different window

The engine does not know that a window of a different program is on top
of the view: it draws the page below a maximized editor as if a person
looked at it. While the window of Nea is not the active one, it asks each
second if one window covers all of it (`cover::out_of_sight`), or if it is
on a different virtual desktop. Only one window that contains all of the
window of Nea counts. A transparent window (a layered window, or one with
a region) does not count, and the round corners of Windows 11 are part of
the test.

After two yes answers (a window that only moves across is not a cover),
Nea takes a picture of each page on screen, hides the views and draws the
pictures in their place. Thus the small pictures that Windows shows of
the window (Alt+Tab, the taskbar) continue to show the page. A picture is
at most 1920 pixels wide, and Nea drops it when the views come back. The
views come back in 250 ms at most after the cover goes, and immediately
when the window becomes the active one. Sound continues. The page is not
suspended: its scripts run, as in a minimized window.

Measured with a page that draws an animation that never stops, processor
time of Nea and its engine in 20 s: 8.3 s in sight, 0.4 s covered, 5.2 s
in sight again (a development build, 120 Hz screen). Edge and Chrome do
the same thing for their own windows.

A test run does not do this, unless `NEA_BENCH_SIGHT` is set.

### One view awake

Only one view out of sight stays awake: the tab that you saw last. Nea
suspends the others at once, before the "suspend" time. A view that is
awake costs 30 to 40 MB in its renderer, and a suspended tab comes back in
tens of milliseconds.

### Commit, and the limit on live views

A suspended view gives back its working set, but it keeps most of its
commit (approximately 50 MB for each page in the test). Commit is space
that the system holds in the page file. It is not memory in use. Thus Nea
discards a suspended tab only when the commit of the system is almost at
its limit (see "Low memory").

Two settings in `settings.json` bring back a discard by time and by
number. Both are 0 (off) by default. `discard_after_mins` discards a tab
after that time out of sight. `live_tabs_limit` is a maximum number of
live views out of sight: Nea discards the views that were seen least
recently first, and the views that are out of sight for 5 min or more keep
only half of the limit. These tabs are never discarded, and are not
counted: pinned tabs, private tabs, tabs that play sound and sites that
you keep awake.

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

At the same time, all the engine processes give back their working set:
the browser, GPU and utility processes, and each renderer, also that of
the page on screen. This comes again each 60 s for as long as nobody uses
the window (`memory::REST_TRIM_EVERY`). It also comes when the window is
in the background for 2 min: a window of a different program is the active
one, and the person works there. Before this, the engine trim came one
time for each pause and left the page on screen as it was. That page, the
browser process and the GPU process got back their memory in the next
minutes, and they were most of what stayed: 20 heavy sites after 5 min
with no input had approximately 296 MB, and have 42 to 65 MB now (3 runs).

Nea does not do this while a page plays sound: a person can watch a video
with no input, and a trim of a process that runs can drop a frame. A
script request counts as use, so a test that drives Nea measures it as in
use.

The cost was measured (`tools/return.ps1`, 9 tabs of heavy sites, 300 s
with no input, 2 runs before and 4 after). The processor time did not
change: 71.5 and 74.1 s, against 76.4 s before. The time to come back to a
suspended tab did not change: 160 to 190 ms. The first script call on the
page on screen after the trims took 72 and 103 ms, less than the same call
just after the load (189 and 328 ms): the pages that the trim took out
come back from memory, not from the disk, while the system has free memory.

## Low memory

There are two levels.

Low: the memory load is at the "memory pressure" setting (85%) or more, or
the commit of the system is at 90% of its limit or more. Nea does these
steps immediately:

- It suspends each tab out of sight, with no wait and no park.
- It keeps at most 8 live views out of sight (4 of the views that were out
  of sight for 5 min). It discards the others.
- It gives back its own working set (at most each 30 s).
- It does not make a spare view.

High: the commit of the system is at 97% of its limit or more, or Windows
signals low memory (`CreateMemoryResourceNotification`). Nea also discards
each tab out of sight that it can discard. A discard of a suspended tab
gives back commit, not memory in use, so Nea does it only here.

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

### Pass 4

2026-11-26, same engine and computer, offline, 30 tabs, one run of each
build, memory pressure off (`memory_pressure_percent` 100). "Before" is
release 0.1.9. The change: no discard by time or by number, only one view
out of sight stays awake (the others are suspended at once), the trim of
the suspended renderers, and the trim of the engine when nobody uses the
window.

| Phase | Before | After |
|---|---:|---:|
| idle 5 min | 190 / 534 | 121 / 1834 |
| minimized | 6 / 514 | 14 / 1815 |
| Tabs that load again when they come back | 25 | 0 |

The time after a look at each of the 30 tabs, private working set in MB,
one sample each 10 s (`tools/memory.ps1 -SeriesSec 70`):

| Seconds | 5 | 16 | 26 | 37 | 47 | 57 | 68 |
|---|---:|---:|---:|---:|---:|---:|---:|
| Before | 645 | 634 | 458 | 459 | 458 | 456 | 465 |
| After | 407 | 458 | 263 | 251 | 254 | 271 | 223 |

The engine works again in the renderer of a page approximately 20 s after
its suspend, and brings back the memory that a trim gave to the system
(717 MB at 26 s when the next trim came only at 40 s). Thus the trims
come close together in that time: 2, 20, 25, 30, 35, 45 and 80 s after
the last suspend. The commit is more than two times as much: it is space
in the page file, not memory in use, and Nea gives it back when the system
is near its limit (see "Low memory").

20 tabs of light pages, nothing forced (`tools/measure.ps1`, scenarios
"20-tabs-open" and "20-tabs-idle"), total private working set in MB:

| | Nea | Nea 0.1.9 | Edge | Chrome |
|---|---:|---:|---:|---:|
| Open | 208 | 291 | 411 | 469 |
| 5 min with no input | 74 | 156 | 401 | 503 |

20 tabs of heavy sites (10 sites, each two times: Wikipedia, GitHub, MDN,
YouTube, Reddit, Infobae, La Nación, Amazon, Stack Overflow, NYTimes),
nothing forced, a new profile for each browser, median of 3 runs:

| | Nea | Nea 0.1.9 | Edge | Chrome |
|---|---:|---:|---:|---:|
| Open | 610 | 1633 | 2665 | 3138 |
| 5 min with no input | 296 | 521 | 2832 | 3270 |

The figure after 5 min changes much from run to run. For the two builds
of Nea it is the median of 31 samples in 60 s (`-Samples 31`), 6 runs of
Nea (344, 513, 316, 276, 167, 111) and 4 of release 0.1.9 (466, 856, 524,
519). The page on screen (NYTimes) and the browser and GPU processes make
the difference: one run had 104 MB in the page on screen, a different one
37 MB. Each suspended tab has 0 to 2 MB. With 9 heavy sites and a
Wikipedia article on screen, 5 runs gave 125 to 135 MB: the page on screen
84, the browser process 16 to 20, the GPU process 14 to 18.

With the blocker off, the 20 tabs open at 735 MB (median of 3 runs).
`--js-flags=--optimize-for-size` and `--disable-features=BackForwardCache`
made no difference larger than the noise.

The time to come back to a suspended tab did not change (`tools/return.ps1`,
real pages, 2 runs of each build): approximately 40 ms more than a script
call on the tab on screen, with and without the trims. The computer had
free memory, so the system did not write the trimmed pages to the disk.

With low memory (`tools/pressure.ps1` held 6.6 GB: commit of the system
93 to 95 %, memory load 83 %), the same test gave the same times for Nea
(151 to 177 ms) and no tab was discarded. Release 0.1.9 discarded a tab in
the first 90 s. The page file of the test computer has a fixed size, so
the test did not go as far as a return from the disk.

The trim each 30 s uses no processor time that the test can measure
(`tools/return.ps1`, 9 tabs of heavy sites, 300 s with no input): 76.4 s
for Nea and 77.1 s for release 0.1.9, of which the app used 0.2 s. Almost
all of that time is the page on screen: a Wikipedia article used 17 s of
processor time each minute (GPU process 11, renderer 6), and example.com
or an MDN page used 0.7 to 1.3 s. The article has an animation that
never stops, and the screen was off: see "Screen off, session locked".

One tab costs 152 MB: renderer 56, GPU process 55, browser process 25,
utility 9, app 6. No engine switch changed this by more than 3 MB (GPU
rasterization off, shader cache off, one raster thread, background
networking and component update off). `--disable-gpu` gave 108 MB, but
then the engine draws in software.

Tried and not used: `--in-process-gpu` (one process less, but 207 MB after
5 min: the trim does not hold). `--process-per-site` gave 190 MB open and
54 MB after 5 min (11 processes, not 24); it is not on, because a page
that hangs stops the other pages of its site.

The text below is about the cap of the passes before (the setting is now
`live_tabs_limit`, off by default).

A smaller cap is a choice, not a default. With `live_tabs_out_of_sight`
set to 4 (2 after 5 min), the same run gave, with 10 tabs: idle
329 / 535 (default 440 / 776), idle 5 min 55 / 371 (201 / 486); with 40
tabs: idle 387 / 637 (528 / 846), idle 5 min 239 / 481 (240 / 553). The
cost: more tabs are discarded, and a discarded tab loads its page again
when it comes back (approximately 160 ms for the local test page, more
for a real page; a suspended view shows in 15 to 46 ms). It also loses its
back and forward history, its form fields and the state of its scripts.
Its scroll position comes back.
