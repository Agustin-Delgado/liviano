# Memory benchmarks

`tools/measure.ps1` made this page. The values are the private working set of `nea.exe` and of the full engine process tree (WebView2). Each value is the median of 3 samples after 10 s of settling. Each scenario starts with new temporary data.

- Date: 2026-11-20 15:49
- Commit: 1a410d7
- Windows: Microsoft Windows 11 Pro 10.0.26200, 32 GB of RAM
- WebView2 engine: 157.0.4341.60
- Edge: 157.0.4341.60 (a separate temporary profile, the same pages)
- Chrome: 156.0.8078.80 (a separate temporary profile, the same pages)
- Block lists: easylist.txt (2023 KB), easyprivacy.txt (1485 KB) and the built-in minimal list
- Light page: https://example.com/ · heavy page: https://www.infobae.com/

| Scenario | Nea (MB) | app | engine | processes | Edge (MB) | Saving | Chrome (MB) | Saving | Note |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| app-only | 1.6 | 1.6 | 0.0 | 1 |  |  |  |  |  |
| about-blank | 86.8 | 4.1 | 82.7 | 7 | 184.1 | 53% | 239.7 | 64% |  |
| light | 109.8 | 3.9 | 105.9 | 7 | 201.2 | 45% | 246.8 | 56% |  |
| heavy-blocking | 394.9 | 4.3 | 390.6 | 9 | 641.4 | 38% | 783.9 | 50% | 44 of 197 requests blocked |
| heavy-no-blocking | 559.3 | 4.3 | 555.0 | 16 | 641.4 | 13% | 783.9 | 29% | 0 of 306 requests blocked |
| 20-tabs | 182.7 | 5.0 | 177.7 | 7 | 451.3 | 60% | 491.2 | 63% | 19 asleep |
| restore-50 | 109.8 | 3.9 | 105.8 | 7 |  |  |  |  | 50 tabs restored |

## Memory budgets

| Budget or criterion | Target | Measured | Result |
|---|---|---|---|
| Own process (interface and logic) | < 20 MB at rest (a few tens at most) | 1.6 MB with no views; 5.0 MB maximum in all scenarios | Met |
| Sleeping tab (metadata only) | < 0.5 MB each | Less than 0.002 MB (below the resolution) of app memory for each tab (restore-50 against light, 49 asleep); with the engine, 0.00 MB | Met |
| Filter engine | A few MB | 1.4 MB of compiled rules (106,608 network rules) | Met |
| Active tab with a light page | 80 to 150 MB for all engine processes | 105.9 MB for the engine, 109.8 MB in total | Met |
| 20 tabs with 19 asleep, much less than Edge | At least 40% less | 182.7 MB against 451.3 MB: 60% less | Met |
| 20 tabs with 19 asleep, much less than Chrome | At least 40% less | 182.7 MB against 491.2 MB: 63% less | Met |
| Suspended tab | A large decrease from the active tab | No separate scenario: "20-tabs" uses sleep with discard (the view is destroyed), which is the cheapest state | Not measured |
| History index | A few MB with tens of thousands of entries | The scenarios start with an empty history | Not measured |

Notes:

- "app" is `nea.exe`. "engine" is the sum of all its child processes (browser, GPU, network, storage, renderers, WebView2 crashpad).
- The two heavy page variants have different settings.json files only (`blocking: {}`, `no-blocking: {"shield_disabled":["infobae.com"]}`). The `shield_disabled` key comes from `shield::site_key` (the host without "www."). The note counts the requests that the filter engine saw and blocked. The count starts when the page opens and stops at the last sample.
- Edge and Chrome cannot put tabs to sleep on request. In "20-tabs", all 20 tabs are alive, which is the cost of 20 open tabs there. These browsers also do not report the end of a page load, so they get a fixed wait before settling. Their value includes their own interface (the browser process). In Nea, the interface is "app".
- "restore-50" starts with a session of 50 tabs. Only the active tab creates its view.
- Regression threshold: `tools/measure.ps1 -Baseline docs/benchmarks-baseline.json` exits with code 1 when a scenario is too high. The limit is the base value plus the tolerance in the file. `-UpdateBaseline` writes the file again.
