# Design and motion

The native interface follows the language of a web design system. It uses
neutral grays, the ink as the primary for active items (black or white, no hue) and seven type steps. No text is
heavier than medium. The tokens are in `src/design.rs`. The motion is in
`src/anim.rs` (`curves` and `specs`).

## Tokens

| Token | Value in Nea |
|---|---|
| Page canvas / field canvas | `canvas` / `field` #FAFAFA |
| Window background (shell) | `chrome` #F5F5F5 |
| Hover / row | #F0F0F0 / #F3F3F3 |
| Ink | ink #171717, quiet #525252, muted #696969, subtle #929292 |
| Line | #E8E8E8, strong #D7D7D7 |
| Primary | The ink: #171717 (hover #404040) on the light shell, #EDEDED (hover #FFFFFF) on the dark one. Text on it is the ground: #FCFCFC / #141414. No hue |
| Tooltip | ink background #171717, text #FAFAFA, caption 11 px medium, padding 8×2, offset 8 px |
| Type | caption 11, small 12, body 13, emphasis 14; maximum weight 500 |
| Corners | mark 2, chip 4, control 6, field 8, panel 12 |
| Shadows | Active tab: `shadow-ring`. A 1 px ring outside the edge, black/0.09, and a drop 0 1 2 black/0.05. In the dark theme: ring white/0.08, drop black/0.3. The ring is one device pixel per 100 % on whole pixels. The same card is on the side strip and on the top strip. Focus halo brand/0.18 |
| Button | height 26 px, field corner, brand primary, outlined secondary |
| Checkbox | corner 4, `line-strong` border, hover brand/40, checked state in brand with a white check mark |
| Tab groups | Nine hues: gray, blue, red, yellow, green, pink, purple, cyan and orange. Light theme: the `ink` step of the hue. Dark theme: the `solid` step (the fill, ≥ 3:1 on the tab strip). The name uses the group color, moved 20% toward the ink (25% in dark). The name sits on a 10% tint of the hue (14% in dark). The contrast is ≥ 4.5:1 on the tab strip, on the tint and on hover. A test checks it. Tab strip on top: label height 22, control corner, small (12) medium text, as a tab title. A group without a name shows a 6 px dot. The line is 1.5 px. Tab strip on the side: the header row is 22 px, shorter than a tab row (32). It has a 5 px dot in the group color, the name (or the site of the first tab) in caption (11) regular (400) `ink_muted`, and a chevron. The header has no background, at rest or on hover. On hover the name goes to `ink`. The dot, the count and the chevron are centered on the middle of the lowercase letters. This position comes from the DirectWrite font metrics, not from the middle of the row. The chevron center is on the same x as the close button center of the tabs below it. A test checks this at 100, 125 and 150 %. The guide is one device pixel per 100 % wide (1 px at 100 % and 125 %, 2 px at 150 %), on whole pixels, in the group color without transparency. The dot is centered on the guide. The tab indent is 12 px. |
| Space | Shown only with two spaces or more. The ground of the strip does not change with the space. Top strip: a 2 px line in the `fill` at 80% at the bottom of the strip. Column: the mark on the row of the gear, from the column pad (8) to the first control less 2 px: 8 px pad, a 6 px dot in the `fill`, 6 px, the name in small (12) in `ink_muted` (`ink` on hover), at most 96 px, 8 px pad; no ground, at rest or on hover, as a group header; it does not show in less than 24 px. Name card: a popup of its own over the page (layered, no focus, no clicks), centered on the page area, 32 px above its bottom edge. 32 px high, round ends, `tab_active` with the card ring and a soft drop, 14 px pad, an 8 px dot in the `fill`, 8 px, the name in text (13) medium in `ink`, at most 240 px, 14 px pad. |
| Profile button | Shown only with two profiles or more. The gear's twin: 30 × 28 px on the gear's row, control corner (6), the ghost ground of the gear (nothing at rest, `hover` under the pointer, a step deeper on a press). Column: at the column pad (8), left of the space mark, which starts 2 px after it. Top strip: left of the update chip, the zoom chip or the extensions (what is leftmost), with the same 2 px gap; the tabs make room for it. In it: an 18 px disc in the `fill` of the profile's hue (profile 0 is gray), with the initial in the tab mark type (medium), white on the light theme and #171717 on the dark one. The tooltip is "Profile". |
| Lock screen | A locked window draws it over the whole client area, on `chrome`; only the window buttons stay, on their caption row (as the hidden column's row). Centered, one under the other: a 64 px disc in the `fill` of the profile's hue with its initial in display (28) medium (white on the light theme, #171717 on the dark one, as the profile button); 16 px; the name in emphasis (14) medium, `ink`; 4 px; why it is locked and "Its tabs stay hidden until it opens." in body (13) regular, `ink_muted`, at most 480 px wide and 16 px from the edges; 24 px; the primary button "Unlock with Windows Hello" (the ink: near-black on light, near-white on dark), 32 px high, at least 200 px wide, 16 px pad, the field corner; 8 px; with two profiles or more, the link "Use another profile" in small (12) regular, `ink_muted`, `ink` on hover, the hand cursor; a note from the last try under it in small `ink_muted`. The block is centered in the window and never above the caption row. While a prompt is up, the button is at 45 %. No fades on this screen. |
| Window buttons | The Windows glyphs (dash, square, cross) at 8 px, with a 1 px stroke on whole pixels. A capsule with three 28 × 28 segments, radius 7. All controls of a caption row have one margin on each side: left of the capsule, right of the last control, above and below. On the top strip and in the collapsed row, this margin is 6 px (the strip is 6 + 28 + 6 = 40 px). On the side strip, it is the column pad, 8 px (the row is 44 px, and the first row starts below it). The capsule and Back, Forward and Reload are on one center line. Tests check this at 100, 125 and 150 %. The background is `chrome` moved 5% toward `ink`. The gap to Back is 12 px. On hover, the segment becomes 6% darker (11% when pressed). Close becomes red #C42B1C with white ink. The end segments keep the capsule curve. Glyphs use `ink_muted` at rest and `ink_asleep` when the window is inactive. |
| Zoom chip | 40 × 28 px, control corner (6). The percent is in caption (11) medium, `ink_muted`. The ground is `tab_tile`, as a pinned tab at rest. On hover, the ground goes to `hover` and the text to `ink`. A press, and each change of the level, flashes the ground to `line` for 150 ms. The gap to the next control is 2 px. |
| Update chip | 52 × 28 px, left of the zoom chip (or where the zoom chip goes), with the same 2 px gap. It shows only while a verified update waits for a restart. The zoom chip's corner, ground, hover and press. The text is "Update" in caption (11) medium, `accent`. On hover, the text goes to `accent_hover`. The tooltip is "Restart to update to x.y.z". |
| Bookmarks bar | Top strip: a 28 px band under the strip, with one line under the band. Chips 24 px high, control corner (6), a ghost ground: nothing at rest, `hover` under the pointer, `line` for 150 ms on a press. The icon is 16 px, 8 px from the left edge, then 6 px to the title. The title is small (12) regular, `ink_muted`, `ink` on hover. A chip is 160 px at most, with a 2 px gap. The **»** button is 28 px wide at the right end. Column: tiles 36 px high and at least 40 px wide, stretched to fill the row, with a 2 px gap and the field corner (8). The ground is `tab_tile`, as a pinned tab, `hover` on hover. The icon (or the initial) is centered. Up to three rows; the last cell is the **…** tile. 6 px of air under the tiles. |
| Back, forward, reload | 28×28 buttons with a ghost background and hover. A disabled button uses `subtle` and has no hover. Stroke 1.5 px. |

The dark theme is specific to Nea, because the design system has no dark
theme. It keeps the same steps and roles, in reverse. The primary is the ink, so
it is white on a dark background.

Two items from Search agree with this language and did not change:

- Tabs are soft backgrounds on the tab strip.
- Private tabs use purple.

## Motion

| Item | Duration and curve |
|---|---|
| Hover and press on buttons, tabs and rows | 150 ms, `ease-out` (0, 0, 0.2, 1) |
| The field and its list | It opens as a macOS panel (Spotlight). It grows from 0.96 with the spring in 320 ms, with most of the growth in the first 100 ms. The fade takes 140 ms, `ease-out`. It closes in 150 ms, `ease`: it shrinks to 0.96 and fades out. In the middle position, it grows from its center. In the top and bottom positions, it grows from its edge, with 4 px of travel. |
| Menu | 120 ms to open, 100 ms to close, `ease` |
| Settings (dialog) | 100 ms, `ease` |
| Tooltip | 600 ms delay, 100 ms to open, 80 ms to close. It closes after 100 ms. If a different tooltip closed less than 300 ms before, it opens with no delay. |
| Toast and password bar | 396 ms to open with a `linear()` spring (it goes to 1.038 and settles). It moves up 6 px. 400 ms to close. |
| Split view focus line | 200 ms, `ease-out` |
| Active tab background on a tab switch | 140 ms, `--ease-out-strong`. At 200 ms, a move across the full tab strip felt slow. |
| Neighbor tabs that make space (move, open, close) | 160 ms and 200 ms, `--ease-out-strong` (0.23, 1, 0.32, 1) |
| Tab strip column that comes back | 250 ms, `--ease-move` (0.77, 0, 0.175, 1) |
| Group collapse or expand | 220 ms, `--ease-out-strong`. The tabs fold into the label (or come out of it) and fade. The arrow turns and the group line fades. The adjacent items move with the same curve. If the active tab is in the group, its background does not move. It fades in on the tab that replaces it. |
| Label of a new group | 200 ms, `--ease-out-strong`. It opens and fades in, as a new tab does. |
| Space switch | The whole tab list slides sideways; the other space's list comes in beside it, one tab-area width away. A key: 200 ms, `--ease-out-strong`, from the side of the space. A swipe: the list follows the fingers (600 wheel units are one width) and, when the wheel stops for 120 ms, goes on to the other space (past a third of the width, or a flick of 3 widths a second) or back, in 200 ms, `--ease-out-strong`. Past the first space: a rubber band, at most a quarter of the width. Nothing in the list moves on its own during a switch. The line and the mark change at once. The name card comes in when the switch is done, as a toast (396 ms spring, up 6 px), stays 1 s, and goes as a toast (400 ms, down 6 px). It does not show while the fingers pull. Reduced motion: no slide; the card shows and goes at once. |
| Checkbox check mark | 75 ms |
| Loading ring | 1 turn/s, at 20 frames/s, as Tailwind's `animate-spin` |

Nea calculates the CSS curves as the browser does:

- `cubic-bezier` with Newton's method and, if necessary, bisection.
- The `linear()` spring with the same points.

The tests compare the results with the Chromium values.

An interrupted animation continues from its current position. It does not
jump. When a hover reverses halfway, the return time is proportional to the
distance that the item moved.

If the Windows interface animations are off (`SPI_GETCLIENTAREAANIMATION`),
all items move to their final position immediately.

## Intentional differences

- **The field scales with an image of itself.** A window with a native
  `Edit` control cannot scale. During the open and close animations, the
  `Edit` window is invisible (opacity 0) and already has the keyboard focus.
  The background window draws the full field from a snapshot. The snapshot
  contains the background, the shadow and the `WM_PRINT` output of the field
  and its child windows. Nea resamples the snapshot in each frame at the
  current scale and opacity. The last frame is the snapshot without changes.
  Thus the change to the real windows does not change a pixel. Nea releases
  the snapshot and the frame bitmap at the end. Settings does not scale. It
  only fades.
- **Nea draws the placeholder of the field.** The cue banner of the `Edit`
  control uses the system gray, and you cannot change its color. When the
  field is empty, Nea draws the placeholder in `ink-muted`. The contrast is
  more than 4.5:1 in light and dark. Nea uses the font of the `Edit` control
  and the position of its first character.
- **The field is a pill in two windows.** Windows rounds only a few fixed
  pixels. The lower window uses per-pixel alpha (`UpdateLayeredWindow`). It
  has the smooth shape, the ring and a shadow in three layers: contact, key
  and ambient. The ambient layer is wide and diffuse, as in macOS panels. The
  upper window contains the `Edit` control and the list. A region one pixel
  inside the edge clips it. The steps of the region fall on the plain
  background below. Thus the visible edge is the smooth edge. When the list is
  open, the field side stays round. The list side uses the dialog corner
  (16 px).
- **At the bottom position, the list reads upward.** The first suggestion is
  next to the field. The Up key moves into the list, as in fzf. Find in page
  never uses the middle position, because the page centers each match there.
- **The tab strip column animates only when it comes back.** When you hide
  it, it disappears immediately. Thus the page gets the space with no wait.
  It slides in only when you show it, or when you leave full screen. A
  window that comes back from the taskbar shows the column at once.
- **Menus and combo boxes.** Nea has no custom context menu. It uses the
  Windows menus (`TrackPopupMenu`) and the Windows combo box lists. They keep
  the system transitions.
- **No `DwmFlush`.** `DwmFlush` blocks the interface thread until the next
  compositor frame. The clock is one 15 ms timer. The timer exists only while
  an item moves.
- **You rename a group in the field.** Chrome opens a bubble with the name
  and the palette. Nea uses the field for the name and the label menu for the
  color. Thus Nea has no extra window to maintain.
- **The gear has no accessible name.** No drawn button has UIA support yet.
  This is separate work for the full tab strip.

## How to see the motion

- `tools/frames.ps1` records the intermediate frames of an animation. It
  starts the trigger and captures with PrintWindow every N ms. It composes
  each layered window with its current opacity and makes a strip of frames.
- Each frame takes about 300 ms to capture, because PrintWindow captures the
  full window with its WebView. Thus a short animation gives no intermediate
  frames. To record the field animations, make a build with the field
  durations multiplied by 8.
- `NEA_BENCH=1` adds the `anim` key to the pipe's `state`. It contains the
  frame count, the worst frame and the number of frames over 16 ms.

## Logo

The logo is a white feather on a graphite square with round corners. The
graphite goes from #51515C at the top to #27272F at the bottom. The feather
shows lightness. It points up, which shows forward movement. The graphite is
neutral on purpose, like the rest of the window.

A script makes the feather, not a person. A few numbers in `tools/logo.py`
set its shape. They give the curved rachis, the width of each vane, the
angle of the barbs and the positions of the splits.

- `python -I tools/logo.py` is the source. It writes these files:
  - `assets/logo/nea.svg` (light).
  - `nea-dark.svg` (dark: a white and silver feather on near black).
  - `nea-mono.svg` (the feather only, in `currentColor`).
  - `nea.ico` (16 to 64 in BMP, 256 in PNG).
  - The previews in `docs/logo/`.

  Do not edit the SVG files.
- Below 48 px, the barbs and the thin splits become unclear. Thus the small
  sizes use a simpler feather (the `DETAIL` table). It is larger in the
  square, has no barbs, has one deep split and has a thicker rachis.
- `build.rs` embeds the `.ico` as resource 1 with `nea.rc`. The exe contains
  only the light icon, because Windows does not change the icon of an exe
  with the theme.
