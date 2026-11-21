//! The single field: a small popup over the page with a native edit control
//! and, beside it, the list of suggestions. It serves every mode — address,
//! tab switcher, find in page and the bookmarks, history, downloads and
//! passwords panels — so there is one field, not six.
//!
//! The edit stays the system's (IME, selection, clipboard, undo all come
//! with it); everything around it is drawn with Direct2D: the panel's
//! ground and its search glass, and the list, an owner-drawn list box whose
//! rows read as Search's (a site mark, the title, the address dimmed, a
//! soft rounded ground for the picked row).
//!
//! The field is a pill: its ends are half circles, radius half its height.
//! With the list open the panel keeps the pill's round ends on the field's
//! side and closes on the list's side with the dialog's corner, as a
//! pill that grew a list. Windows only rounds a window by a few fixed
//! pixels, so the panel is two windows:
//!
//! - the **backdrop**, per-pixel alpha (`UpdateLayeredWindow`), drawn once
//!   per size: the antialiased shape, its 1 px ring and `shadow-pop`. It
//!   never takes the pointer, and its bitmap is let go once handed over.
//! - the **popup** over it, holding the edit and the list, clipped by a
//!   window region to the shape pulled in by a pixel. The region's steps
//!   fall where the backdrop is already the same solid ground, so the edge
//!   that shows is the backdrop's smooth one; the children paint as in any
//!   window, which they would not inside an `UpdateLayeredWindow` one.
//!
//! The popup fades as before (`fade`) and the backdrop follows each step.
//!
//! Where it floats is a setting (`FieldPosition`): under the top of the
//! page, in the middle as a command palette, or on the bottom edge with
//! the list opening upwards.

use std::cell::{Cell, RefCell};

use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM},
        Graphics::Gdi::{
            AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, BeginPaint, BitBlt, ClientToScreen,
            CreateCompatibleDC, CreateDIBSection, CreateSolidBrush, DIB_RGB_COLORS, DeleteDC, DeleteObject, EndPaint,
            ExtCreateRegion, GetDC, GetTextMetricsW, HBITMAP, HBRUSH, HDC, HFONT, HGDIOBJ, InvalidateRect, PAINTSTRUCT, RDH_RECTANGLES,
            RGNDATA, RGNDATAHEADER, ReleaseDC, SRCCOPY, SelectObject, SetBkColor, SetTextColor, SetWindowRgn, TEXTMETRICW,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::{
                DRAWITEMSTRUCT, EM_GETSEL, EM_SETCUEBANNER, EM_SETMARGINS, EM_SETSEL, ODS_SELECTED, SetWindowTheme,
                WM_MOUSELEAVE,
            },
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, GetKeyState, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DELETE,
                VK_DOWN, VK_ESCAPE, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON, VK_RETURN, VK_SHIFT, VK_UP,
            },
            Shell::{DefSubclassProc, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
    core::{HSTRING, PCWSTR, w},
};

use crate::{
    anim::{self, Levels, specs},
    design::{Palette, Rgb, dwm, lift, size},
    fade,
    gfx::{Align, Color, Font, Painter, RectF, Surface},
    settings::FieldPosition,
};

/// Whether the foreground window is one of this process's. Taking the focus
/// otherwise would pull the keyboard away from the program the person is
/// typing in.
pub fn owns_foreground() -> bool {
    unsafe {
        let foreground = GetForegroundWindow();
        if foreground.is_invalid() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(foreground, Some(&mut pid));
        pid == windows::Win32::System::Threading::GetCurrentProcessId()
    }
}

/// Where the field goes for a mode. Find in page never sits in the middle:
/// the page scrolls each match to its middle, right under the palette, so
/// there it opens on top. Every other mode goes where the setting says
/// (find included on the bottom edge, as Firefox's find bar).
pub fn position_for(setting: FieldPosition, find: bool) -> FieldPosition {
    match setting {
        FieldPosition::Middle if find => FieldPosition::Top,
        other => other,
    }
}

const LIST_ID: usize = 2;
const LBN_SELCHANGE_CODE: u32 = 1;
const EN_CHANGE_CODE: u32 = 0x0300;

/// What the popup asks of the app. The app decides; the popup only draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Changed,
    Submit { row: Option<usize>, shift: bool },
    /// Supr on a selected row of a panel.
    Delete { row: usize },
    Cancel,
}

thread_local! {
    static HANDLER: Cell<Option<fn(Signal)>> = const { Cell::new(None) };
    /// The edit's ground brush (WM_CTLCOLOREDIT), its colour and the ink.
    static BRUSHES: Cell<(isize, u32, u32)> = const { Cell::new((0, 0, 0)) };
    static LOOK: Cell<Option<(Palette, f32)>> = const { Cell::new(None) };
    /// One surface for the panel's ground and one for the list's rows.
    static SURFACES: RefCell<(Surface, Surface)> = RefCell::new(Default::default());
    /// The row under the pointer, drawn with a quieter ground than the picked one.
    static HOVERED: Cell<Option<usize>> = const { Cell::new(None) };
    /// How far each row's grounds have come in: (picked, hovered) by row.
    static GROUNDS: RefCell<(Levels<usize>, Levels<usize>)> = RefCell::new(Default::default());
    /// The backdrop and where it stands from the popup's corner (it follows
    /// the popup from `fade`'s steps, which only know the popup).
    static BACKDROP: Cell<Option<(HWND, i32, i32)>> = const { Cell::new(None) };
}

/// The field's height: the pill's ends are half of it.
const FIELD_HEIGHT: f32 = 48.0;
const ROW_HEIGHT: f32 = 36.0;
/// Space around the list inside the panel.
const LIST_PAD: f32 = 6.0;
/// The glass's column at the field's left.
const GLASS_COLUMN: f32 = 44.0;
/// The list's end of the panel: the dialog's corner (brand.css
/// `--radius-dialog`). Concentric enough with the rows' 8 px grounds 6 px
/// in, and the list box's square corner stays inside it.
const LIST_RADIUS: f32 = size::RADIUS_DIALOG;
/// How far the region stands inside the drawn edge, in device pixels: the
/// popup only covers what the backdrop paints solid.
const REGION_INSET: f32 = 1.0;
/// From the page's edge to the field (top and bottom), before scaling.
pub const EDGE_GAP: f32 = 10.0;

/// Posted to the panel when a click outside it took activation away.
const WM_CLICKED_OUTSIDE: u32 = WM_APP + 60;

/// Whether a mouse button is down with the pointer outside the panel: what
/// a deactivation looks like when a click caused it.
fn clicked_outside(panel: HWND) -> bool {
    let down = [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON].iter().any(|k| unsafe { GetAsyncKeyState(k.0 as i32) } as u16 & 0x8000 != 0);
    let mut cursor = POINT::default();
    let mut rect = RECT::default();
    unsafe {
        let _ = GetCursorPos(&mut cursor);
        let _ = GetWindowRect(panel, &mut rect);
    }
    is_outside_click(down, cursor, rect)
}

fn is_outside_click(button_down: bool, cursor: POINT, panel: RECT) -> bool {
    let inside = cursor.x >= panel.left && cursor.x < panel.right && cursor.y >= panel.top && cursor.y < panel.bottom;
    button_down && !inside
}

fn signal(s: Signal) {
    if let Some(handler) = HANDLER.with(|h| h.get()) {
        handler(s);
    }
}

// ---------- geometry ----------

/// Where the field may go, in screen pixels: a column of the focused page
/// (its half in split view, beside the tab column when that is shown), its
/// top and bottom already kept `EDGE_GAP` off the page's edges.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub bottom: i32,
}

/// The measures the layout needs, in device pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    field: i32,
    row: i32,
    pad: i32,
    list_radius: f32,
}

impl Metrics {
    fn at(scale: f32) -> Self {
        let px = |v: f32| (v * scale).round() as i32;
        Self { field: px(FIELD_HEIGHT), row: px(ROW_HEIGHT), pad: px(LIST_PAD), list_radius: LIST_RADIUS * scale }
    }
}

/// Where everything goes for one position, frame and number of rows.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Layout {
    /// The panel, in screen pixels.
    panel: RECT,
    /// The field's top, inside the panel.
    field_top: i32,
    /// The list box, inside the panel (zero height without rows).
    list: RECT,
    /// Rows on screen at once; the list scrolls past them.
    shown: usize,
    /// The hairline between field and list (y inside the panel).
    rule: Option<i32>,
    /// Corner radii of the panel: (top, bottom).
    radii: (f32, f32),
    /// The list reads upwards: the first row sits next to the field.
    upward: bool,
}

impl Layout {
    fn width(&self) -> i32 {
        self.panel.right - self.panel.left
    }
    fn height(&self) -> i32 {
        self.panel.bottom - self.panel.top
    }
}

/// How far below the frame's top the field sits in the middle: its centre
/// at 40 % of the page, the optical middle. With the list open the whole
/// panel then sits about the centre, and the field never moves as rows come
/// and go.
const MIDDLE_AT: f32 = 0.4;

fn layout(position: FieldPosition, frame: Frame, m: Metrics, rows: usize, max_rows: usize) -> Layout {
    let height = frame.bottom - frame.top;
    let list_extra = 1 + 2 * m.pad;
    // The middle keeps its field still: the list only gets what fits under it.
    let middle_top = (frame.top + (height as f32 * MIDDLE_AT) as i32 - m.field / 2).max(frame.top);
    let room = match position {
        FieldPosition::Middle => frame.bottom - middle_top - m.field - list_extra,
        _ => height - m.field - list_extra,
    };
    let fit = (room / m.row.max(1)).max(1) as usize;
    let shown = rows.min(max_rows).min(fit);
    let list_h = shown as i32 * m.row;
    let panel_h = m.field + if shown > 0 { list_extra + list_h } else { 0 };
    let top = match position {
        FieldPosition::Top => frame.top,
        FieldPosition::Middle => middle_top,
        FieldPosition::Bottom => frame.bottom - panel_h,
    };
    let panel = RECT { left: frame.left, top, right: frame.left + frame.width, bottom: top + panel_h };
    let upward = position == FieldPosition::Bottom;
    let width = frame.width;
    let end = m.field as f32 / 2.0;
    if shown == 0 {
        let list = RECT { left: m.pad, top: m.field, right: width - m.pad, bottom: m.field };
        return Layout { panel, field_top: 0, list, shown, rule: None, radii: (end, end), upward };
    }
    if upward {
        let list = RECT { left: m.pad, top: m.pad, right: width - m.pad, bottom: m.pad + list_h };
        let rule = list.bottom + m.pad;
        Layout { panel, field_top: rule + 1, list, shown, rule: Some(rule), radii: (m.list_radius, end), upward }
    } else {
        let list = RECT { left: m.pad, top: m.field + 1 + m.pad, right: width - m.pad, bottom: m.field + 1 + m.pad + list_h };
        Layout { panel, field_top: 0, list, shown, rule: Some(m.field), radii: (end, m.list_radius), upward }
    }
}

/// The px a popover travels as it comes and goes, toward the edge it hangs
/// from (celagem's popovers grow from the side that faces their trigger):
/// down from the top, up from the bottom, and only the fade in the middle.
fn slide(position: FieldPosition, scale: f32) -> i32 {
    let px = (anim::POPOVER_SLIDE * scale).round() as i32;
    match position {
        FieldPosition::Top => -px,
        FieldPosition::Middle => 0,
        FieldPosition::Bottom => px,
    }
}

/// A list row as the app numbers it (0 = best) and as the list box holds it:
/// read upwards, the first row is the last one in the box, next to the field.
fn list_index(row: usize, rows: usize, upward: bool) -> usize {
    if upward { rows - 1 - row } else { row }
}

/// The arrow keys move on screen: Up goes up, also when the list reads
/// upwards, where up is toward the rows after the first. From the field
/// the first arrow toward the list lands on the first row; past the list's
/// end the selection goes back to the field (None).
fn next_selection(selected: Option<usize>, rows: usize, key_down: bool, upward: bool) -> Option<usize> {
    if rows == 0 {
        return None;
    }
    // +1: toward the rows after the first.
    let toward_more = key_down != upward;
    match selected {
        None if toward_more => Some(0),
        None => Some(rows - 1),
        Some(r) if toward_more => (r + 1 < rows).then_some(r + 1),
        Some(r) => r.checked_sub(1),
    }
}

// ---------- the backdrop's shape ----------

/// A rounded rectangle whose top corners and bottom corners may differ.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Shape {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    radii: (f32, f32),
}

impl Shape {
    /// Signed distance from (x, y) to the edge: negative inside.
    fn distance(&self, x: f32, y: f32) -> f32 {
        let (hw, hh) = (self.width / 2.0, self.height / 2.0);
        let (cx, cy) = (self.left + hw, self.top + hh);
        let r = if y < cy { self.radii.0 } else { self.radii.1 }.min(hw).min(hh).max(0.0);
        let qx = (x - cx).abs() - (hw - r);
        let qy = (y - cy).abs() - (hh - r);
        let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
        outside + qx.max(qy).min(0.0) - r
    }

    /// The shape moved and grown (`grow` < 0 shrinks it, corners too).
    fn shifted(&self, dy: f32, grow: f32) -> Self {
        Self {
            left: self.left - grow,
            top: self.top + dy - grow,
            width: (self.width + 2.0 * grow).max(0.0),
            height: (self.height + 2.0 * grow).max(0.0),
            radii: ((self.radii.0 + grow).max(0.0), (self.radii.1 + grow).max(0.0)),
        }
    }
}

/// `shadow-pop` (0 12px 32px -8px black/0.18) and its 1 px ring, in px
/// before scaling.
const SHADOW_Y: f32 = 12.0;
const SHADOW_BLUR: f32 = 32.0;
const SHADOW_SPREAD: f32 = 8.0;

/// How far the backdrop reaches past the panel on each side for the shadow:
/// (left, top, right, bottom), device pixels.
fn shadow_margins(scale: f32) -> (i32, i32, i32, i32) {
    let reach = (SHADOW_BLUR - SHADOW_SPREAD) * scale;
    let side = reach.ceil() as i32 + 2;
    let top = (reach - SHADOW_Y * scale).max(0.0).ceil() as i32 + 2;
    let bottom = (reach + SHADOW_Y * scale).ceil() as i32 + 2;
    (side, top, side, bottom)
}

/// The colours the backdrop is drawn with.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BackdropLook {
    fill: Rgb,
    /// The ring's colour and opacity.
    ring: (Rgb, f32),
    /// The shadow's opacity at its heart.
    shadow: f32,
}

impl BackdropLook {
    fn of(palette: &Palette) -> Self {
        // A black ring and shadow vanish on a dark page: the ring turns
        // light and the shadow doubles, as the cards do.
        let (ring, shadow) = if palette.dark {
            ((Rgb(255, 255, 255), lift::POP_RING), lift::POP_DROP * 2.0)
        } else {
            ((Rgb(0, 0, 0), lift::POP_RING), lift::POP_DROP)
        };
        Self { fill: palette.field, ring, shadow }
    }
}

/// erfc(x), Abramowitz and Stegun 7.1.26 (error under 1.5e-7).
fn erfc(x: f32) -> f32 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * z);
    let poly = t * (0.254_829_6 + t * (-0.284_496_74 + t * (1.421_413_7 + t * (-1.453_152 + t * 1.061_405_4))));
    let y = poly * (-z * z).exp();
    if x >= 0.0 { y } else { 2.0 - y }
}

/// Draws the backdrop into `pixels` (premultiplied BGRA, `width` per row):
/// the shadow, the ring 1 px outside the shape, and the shape's ground with
/// an antialiased edge. Inside, past the edge, there is nothing to work
/// out: the ground is solid there.
fn render_backdrop(pixels: &mut [u32], width: usize, shape: Shape, look: BackdropLook, scale: f32) {
    let shadow = shape.shifted(SHADOW_Y * scale, -SHADOW_SPREAD * scale);
    // CSS blur radius = 2 sigma.
    let k = 1.0 / (SHADOW_BLUR * scale / 2.0 * std::f32::consts::SQRT_2);
    let pack = |a: f32, r: f32, g: f32, b: f32| -> u32 {
        let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        c(a) << 24 | c(r) << 16 | c(g) << 8 | c(b)
    };
    let f = |v: u8| v as f32 / 255.0;
    let (fr, fg, fb) = (f(look.fill.0), f(look.fill.1), f(look.fill.2));
    let solid = pack(1.0, fr, fg, fb);
    let (rr, rg, rb) = (f(look.ring.0.0), f(look.ring.0.1), f(look.ring.0.2));
    for (i, pixel) in pixels.iter_mut().enumerate() {
        let (x, y) = ((i % width) as f32 + 0.5, (i / width) as f32 + 0.5);
        let d = shape.distance(x, y);
        if d < -1.0 {
            *pixel = solid;
            continue;
        }
        let cover = (0.5 - d).clamp(0.0, 1.0);
        let ring = ((1.5 - d).clamp(0.0, 1.0) - cover) * look.ring.1;
        let drop = look.shadow * 0.5 * erfc(shadow.distance(x, y) * k);
        // Shadow, then ring over it, then the ground over both.
        let mut a = drop;
        let (mut r, mut g, mut b) = (0.0, 0.0, 0.0);
        a = ring + a * (1.0 - ring);
        r = rr * ring + r * (1.0 - ring);
        g = rg * ring + g * (1.0 - ring);
        b = rb * ring + b * (1.0 - ring);
        a = cover + a * (1.0 - cover);
        r = fr * cover + r * (1.0 - cover);
        g = fg * cover + g * (1.0 - cover);
        b = fb * cover + b * (1.0 - cover);
        *pixel = pack(a, r, g, b);
    }
}

/// The popup's region as runs of rows: (top, bottom, left, right) of each
/// band of rows that share a span, in the popup's pixels. Only pixels at
/// least `REGION_INSET` inside the edge are in.
fn region_bands(width: i32, height: i32, radii: (f32, f32)) -> Vec<(i32, i32, i32, i32)> {
    let shape = Shape { left: 0.0, top: 0.0, width: width as f32, height: height as f32, radii };
    let mut bands: Vec<(i32, i32, i32, i32)> = Vec::new();
    for y in 0..height {
        let cy = y as f32 + 0.5;
        // The shape is the same on both sides: find the left end only.
        let Some(left) = (0..width / 2 + 1).find(|x| shape.distance(*x as f32 + 0.5, cy) < -REGION_INSET) else { continue };
        let right = width - left;
        match bands.last_mut() {
            Some(band) if band.1 == y && band.2 == left && band.3 == right => band.1 = y + 1,
            _ => bands.push((y, y + 1, left, right)),
        }
    }
    bands
}

fn apply_region(hwnd: HWND, width: i32, height: i32, radii: (f32, f32)) {
    let bands = region_bands(width, height, radii);
    let rects: Vec<RECT> = bands.iter().map(|(t, b, l, r)| RECT { left: *l, top: *t, right: *r, bottom: *b }).collect();
    let header = std::mem::size_of::<RGNDATAHEADER>();
    let bytes = header + rects.len() * std::mem::size_of::<RECT>();
    // RGNDATA is a header followed by the rectangles: built in a buffer of
    // u32s, which RECT and the header are aligned to.
    let mut buffer = vec![0u32; bytes.div_ceil(4)];
    let head = RGNDATAHEADER {
        dwSize: header as u32,
        iType: RDH_RECTANGLES,
        nCount: rects.len() as u32,
        nRgnSize: (rects.len() * std::mem::size_of::<RECT>()) as u32,
        rcBound: RECT { left: 0, top: 0, right: width, bottom: height },
    };
    unsafe {
        let base = buffer.as_mut_ptr() as *mut u8;
        std::ptr::copy_nonoverlapping(&head as *const RGNDATAHEADER as *const u8, base, header);
        std::ptr::copy_nonoverlapping(rects.as_ptr() as *const u8, base.add(header), rects.len() * std::mem::size_of::<RECT>());
        let region = ExtCreateRegion(None, bytes as u32, buffer.as_ptr() as *const RGNDATA);
        if !region.is_invalid() {
            // The window owns the region from here on.
            SetWindowRgn(hwnd, Some(region), true);
        }
    }
}

/// The opacity a layered window has now (255 when it says nothing).
fn alpha_of(hwnd: HWND) -> u8 {
    let mut alpha = 255u8;
    let mut flags = LAYERED_WINDOW_ATTRIBUTES_FLAGS(0);
    unsafe {
        if GetLayeredWindowAttributes(hwnd, None, Some(&mut alpha), Some(&mut flags)).is_err() || flags.0 & LWA_ALPHA.0 == 0 {
            return 255;
        }
    }
    alpha
}

fn blend(alpha: u8) -> BLENDFUNCTION {
    BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: alpha, AlphaFormat: AC_SRC_ALPHA as u8 }
}

/// Keeps the backdrop under the popup: same opacity, same place (a slide
/// included), shown and hidden with it. `fade` calls it on every step.
fn mirror(popup: HWND) {
    let Some((backdrop, dx, dy)) = BACKDROP.with(|b| b.get()) else { return };
    unsafe {
        if !IsWindowVisible(popup).as_bool() {
            let _ = ShowWindow(backdrop, SW_HIDE);
            return;
        }
        let mut rect = RECT::default();
        let _ = GetWindowRect(popup, &mut rect);
        let _ = SetWindowPos(
            backdrop,
            Some(popup),
            rect.left + dx,
            rect.top + dy,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        let blend = blend(alpha_of(popup));
        let _ = UpdateLayeredWindow(backdrop, None, None, None, None, None, COLORREF(0), Some(&blend), ULW_ALPHA);
    }
}

/// A 32-bit bitmap selected into a memory DC, freed when dropped.
struct Canvas {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    len: usize,
}

impl Canvas {
    fn new(width: i32, height: i32) -> Option<Self> {
        unsafe {
            let dc = CreateCompatibleDC(None);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let Ok(bitmap) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
                let _ = DeleteDC(dc);
                return None;
            };
            let old = SelectObject(dc, bitmap.into());
            Some(Self { dc, bitmap, old, bits: bits as *mut u32, len: (width * height) as usize })
        }
    }

    fn pixels(&self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.len) }
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// What the backdrop on screen was drawn from, to draw it again on request.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Painted {
    size: (i32, i32),
    shape: Shape,
    look: BackdropLook,
    scale: f32,
}

thread_local! {
    static PAINTED: Cell<Option<Painted>> = const { Cell::new(None) };
}

unsafe extern "system" fn backdrop_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // What is on screen came from UpdateLayeredWindow, not from here. A
        // paint only comes from PrintWindow (the capture tools), which
        // points it at its own bitmap: it gets the picture again, drawn
        // from the numbers it was drawn from, premultiplied alpha included.
        WM_PAINT | WM_PRINT | WM_PRINTCLIENT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = if msg == WM_PAINT { unsafe { BeginPaint(hwnd, &mut ps) } } else { HDC(wparam.0 as _) };
            if let Some(p) = PAINTED.with(|p| p.get()) {
                if let Some(canvas) = Canvas::new(p.size.0, p.size.1) {
                    render_backdrop(canvas.pixels(), p.size.0 as usize, p.shape, p.look, p.scale);
                    unsafe {
                        let _ = BitBlt(hdc, 0, 0, p.size.0, p.size.1, Some(canvas.dc), 0, 0, SRCCOPY);
                    }
                }
            }
            if msg == WM_PAINT {
                unsafe {
                    let _ = EndPaint(hwnd, &ps);
                }
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// What the backdrop was last drawn for: drawn again only when it changes.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Drawn {
    size: (i32, i32),
    radii: (f32, f32),
    look: BackdropLook,
    /// The part of the backdrop the owner's client area leaves in sight,
    /// relative to the panel.
    clip: RECT,
}

pub struct Omnibar {
    pub popup: HWND,
    pub edit: HWND,
    pub list: HWND,
    backdrop: HWND,
    owner: HWND,
    rows: usize,
    metrics: Metrics,
    scale: f32,
    /// The edit's text line, for centring it in the pill.
    line_height: i32,
    /// Rows shown at once; the list scrolls past them.
    visible_rows: usize,
    /// Supr deletes the selected row instead of a character.
    rows_deletable: bool,
    /// Where it opens this time (the setting, as `position_for` reads it).
    position: FieldPosition,
    /// The layout in force, read by the paint and the keys.
    layout: Cell<Option<Layout>>,
    drawn: Cell<Option<Drawn>>,
    /// New rows read upwards: the list scrolls to its end once placed.
    scroll_to_end: Cell<bool>,
}

impl Omnibar {
    pub fn create(owner: HWND, handler: fn(Signal)) -> windows::core::Result<Self> {
        HANDLER.with(|h| h.set(Some(handler)));
        unsafe {
            let instance = GetModuleHandleW(None)?.into();
            let class = WNDCLASSW {
                lpfnWndProc: Some(popup_proc),
                hInstance: instance,
                lpszClassName: w!("LivianoOmnibar"),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                ..Default::default()
            };
            RegisterClassW(&class);
            let shadow_class = WNDCLASSW {
                lpfnWndProc: Some(backdrop_proc),
                hInstance: instance,
                lpszClassName: w!("LivianoOmnibarBackdrop"),
                ..Default::default()
            };
            RegisterClassW(&shadow_class);
            let backdrop = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                w!("LivianoOmnibarBackdrop"),
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                Some(owner),
                None,
                Some(instance),
                None,
            )?;
            let popup = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("LivianoOmnibar"),
                PCWSTR::null(),
                WS_POPUP | WS_CLIPCHILDREN,
                0,
                0,
                100,
                40,
                Some(owner),
                None,
                Some(instance),
                None,
            )?;
            let edit = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("EDIT"),
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
                0,
                0,
                100,
                30,
                Some(popup),
                Some(HMENU(1 as _)),
                Some(instance),
                None,
            )?;
            let list = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("LISTBOX"),
                PCWSTR::null(),
                WS_CHILD
                    | WS_VSCROLL
                    | WINDOW_STYLE((LBS_NOTIFY | LBS_NOINTEGRALHEIGHT | LBS_OWNERDRAWFIXED | LBS_HASSTRINGS) as u32),
                0,
                30,
                100,
                10,
                Some(popup),
                Some(HMENU(LIST_ID as _)),
                Some(instance),
                None,
            )?;
            let _ = SetWindowSubclass(edit, Some(edit_proc), 1, 0);
            let _ = SetWindowSubclass(list, Some(list_proc), 2, 0);
            // The shape and the shadow are the backdrop's: Windows draws
            // neither corners, nor border, nor shadow of its own.
            dwm::set(popup, dwm::CORNERS, dwm::DO_NOT_ROUND);
            dwm::set(popup, dwm::BORDER_COLOR, dwm::COLOR_NONE);
            // Layered for good, so it can fade in and out as a popover.
            fade::prepare(popup, 255);
            fade::follow(popup, mirror);
            BACKDROP.with(|b| b.set(Some((backdrop, 0, 0))));
            Ok(Self {
                popup,
                edit,
                list,
                backdrop,
                owner,
                rows: 0,
                metrics: Metrics::at(1.0),
                scale: 1.0,
                line_height: 19,
                visible_rows: usize::MAX,
                rows_deletable: false,
                position: FieldPosition::Top,
                layout: Cell::new(None),
                drawn: Cell::new(None),
                scroll_to_end: Cell::new(false),
            })
        }
    }

    /// The edit is set in a larger cut of the interface's face (`font` is
    /// the one other controls use; this one is the field's own).
    pub fn set_font(&mut self, font: HFONT, scale: f32) {
        let edit_font = crate::app::make_font_sized(size::TEXT_FIELD, scale);
        unsafe {
            let old = SendMessageW(self.edit, WM_GETFONT, None, None).0;
            SendMessageW(self.edit, WM_SETFONT, Some(WPARAM(edit_font.0 as usize)), Some(LPARAM(1)));
            if old != 0 && old != font.0 as isize {
                let _ = DeleteObject(HFONT(old as _).into());
            }
            // The text line the edit draws, to centre it in the pill.
            let dc = GetDC(Some(self.edit));
            let before = SelectObject(dc, edit_font.into());
            let mut metrics = TEXTMETRICW::default();
            if GetTextMetricsW(dc, &mut metrics).as_bool() {
                self.line_height = metrics.tmHeight;
            }
            SelectObject(dc, before);
            ReleaseDC(Some(self.edit), dc);
        }
        self.metrics = Metrics::at(scale);
        unsafe {
            // Owner-drawn rows: their height is ours, not the font's.
            SendMessageW(self.list, LB_SETITEMHEIGHT, Some(WPARAM(0)), Some(LPARAM(self.metrics.row as isize)));
        }
        self.scale = scale;
        LOOK.with(|l| l.set(l.get().map(|(p, _)| (p, scale))));
    }

    pub fn set_palette(&self, palette: &Palette) {
        BRUSHES.with(|b| {
            let (old, _, _) = b.get();
            unsafe {
                if old != 0 {
                    let _ = DeleteObject(HBRUSH(old as _).into());
                }
                let field = CreateSolidBrush(palette.field.colorref());
                b.set((field.0 as isize, palette.field.colorref().0, palette.ink.colorref().0));
            }
        });
        LOOK.with(|l| l.set(Some((*palette, self.scale))));
        dwm::set(self.popup, dwm::DARK_MODE, palette.dark as u32);
        unsafe {
            let theme = if palette.dark { w!("DarkMode_Explorer") } else { w!("Explorer") };
            let _ = SetWindowTheme(self.list, theme, PCWSTR::null());
            let _ = InvalidateRect(Some(self.popup), None, true);
            let _ = InvalidateRect(Some(self.list), None, true);
        }
        if let Some(layout) = self.layout.get() {
            self.draw_backdrop(&layout);
        }
    }

    /// How many rows show at once and whether Supr deletes one: a panel is
    /// a taller list whose rows can be removed.
    pub fn set_list_mode(&mut self, visible_rows: usize, rows_deletable: bool) {
        self.visible_rows = visible_rows.max(1);
        self.rows_deletable = rows_deletable;
    }

    /// Opens in `frame` at `position` (see `position_for`).
    pub fn open(&mut self, frame: Frame, position: FieldPosition, cue: &str, text: &str, select_all: bool) {
        unsafe {
            let cue = HSTRING::from(cue);
            SendMessageW(self.edit, EM_SETCUEBANNER, Some(WPARAM(1)), Some(LPARAM(cue.as_ptr() as isize)));
            let _ = SetWindowTextW(self.edit, &HSTRING::from(text));
            let len = text.encode_utf16().count() as isize;
            let start = if select_all { 0 } else { len };
            SendMessageW(self.edit, EM_SETSEL, Some(WPARAM(start as usize)), Some(LPARAM(len)));
        }
        self.position = position;
        self.set_rows(&[]);
        self.place(frame);
        // A popover opening: 100 ms of fade, coming a few px from the edge
        // it hangs from (the list grows with what it finds).
        fade::fade_in(self.popup, specs::POPOVER_IN, 255, slide(position, self.scale));
        unsafe {
            if owns_foreground() {
                let _ = ShowWindow(self.popup, SW_SHOW);
                let _ = SetFocus(Some(self.edit));
            } else {
                // Another program has the keyboard (a scripted run, or the
                // person switched away): the field shows without taking it,
                // so what is typed there never lands here. A click on the
                // window gives the field the focus as usual.
                let _ = ShowWindow(self.popup, SW_SHOWNOACTIVATE);
            }
        }
        mirror(self.popup);
    }

    pub fn place(&self, frame: Frame) {
        let layout = layout(self.position, frame, self.metrics, self.rows, self.visible_rows);
        let before = self.layout.replace(Some(layout));
        let m = self.metrics;
        let glass = (GLASS_COLUMN * self.scale).round() as i32;
        let (width, height) = (layout.width(), layout.height());
        let line = self.line_height.min(m.field);
        unsafe {
            let _ = SetWindowPos(
                self.popup,
                Some(HWND_TOP),
                layout.panel.left,
                layout.panel.top,
                width,
                height,
                SWP_NOACTIVATE,
            );
            // The text line in the middle of the pill; the edit ends where
            // the pill's right end starts to curve in.
            let _ = SetWindowPos(
                self.edit,
                None,
                glass,
                layout.field_top + (m.field - line) / 2,
                width - glass - m.field / 2,
                line,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            // EC_LEFTMARGIN | EC_RIGHTMARGIN, both zero: the glass column is the margin.
            SendMessageW(self.edit, EM_SETMARGINS, Some(WPARAM(3)), Some(LPARAM(0)));
            let list = layout.list;
            let _ = SetWindowPos(
                self.list,
                None,
                list.left,
                list.top,
                list.right - list.left,
                list.bottom - list.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let _ = ShowWindow(self.list, if layout.shown > 0 { SW_SHOWNA } else { SW_HIDE });
            if self.scroll_to_end.replace(false) && layout.upward && self.rows > 0 {
                SendMessageW(self.list, LB_SETTOPINDEX, Some(WPARAM(self.rows - 1)), None);
            }
            let reshaped = before.is_none_or(|b| (b.width(), b.height(), b.radii) != (width, height, layout.radii));
            if reshaped {
                apply_region(self.popup, width, height, layout.radii);
            }
            let _ = InvalidateRect(Some(self.popup), None, false);
        }
        // A fade sliding in goes on from the new place.
        fade::rest_at(self.popup, layout.panel.left, layout.panel.top);
        self.draw_backdrop(&layout);
        mirror(self.popup);
    }

    /// Draws the backdrop for `layout` when its size, corners, colours or
    /// the part in sight changed; otherwise `mirror` only moves it.
    fn draw_backdrop(&self, layout: &Layout) {
        let Some((palette, _)) = LOOK.with(|l| l.get()) else { return };
        let (ml, mt, mr, mb) = shadow_margins(self.scale);
        let (width, height) = (layout.width(), layout.height());
        // The shadow ends with the owner's window, as a popover's ends with
        // the page: nothing is drawn over the desktop.
        let mut clip = RECT { left: -ml, top: -mt, right: width + mr, bottom: height + mb };
        unsafe {
            let mut client = RECT::default();
            if GetClientRect(self.owner, &mut client).is_ok() {
                let mut origin = POINT::default();
                let _ = ClientToScreen(self.owner, &mut origin);
                let (px, py) = (layout.panel.left - origin.x, layout.panel.top - origin.y);
                clip.left = clip.left.max(-px);
                clip.top = clip.top.max(-py);
                clip.right = clip.right.min(client.right - px);
                clip.bottom = clip.bottom.min(client.bottom - py);
            }
        }
        if clip.right <= clip.left || clip.bottom <= clip.top {
            return;
        }
        let drawn = Drawn { size: (width, height), radii: layout.radii, look: BackdropLook::of(&palette), clip };
        if self.drawn.get() == Some(drawn) {
            return;
        }
        let (bw, bh) = (clip.right - clip.left, clip.bottom - clip.top);
        let shape = Shape {
            left: -clip.left as f32,
            top: -clip.top as f32,
            width: width as f32,
            height: height as f32,
            radii: layout.radii,
        };
        let painted = Painted { size: (bw, bh), shape, look: drawn.look, scale: self.scale };
        let Some(canvas) = Canvas::new(bw, bh) else { return };
        render_backdrop(canvas.pixels(), bw as usize, shape, painted.look, painted.scale);
        let at = POINT { x: layout.panel.left + clip.left, y: layout.panel.top + clip.top };
        let size = SIZE { cx: bw, cy: bh };
        let zero = POINT::default();
        unsafe {
            let alpha = if IsWindowVisible(self.popup).as_bool() { alpha_of(self.popup) } else { 0 };
            let blend = blend(alpha);
            let _ = UpdateLayeredWindow(
                self.backdrop,
                None,
                Some(&at),
                Some(&size),
                Some(canvas.dc),
                Some(&zero),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
        // The system keeps its own copy: ours goes at once (the canvas
        // drops here). What it was drawn from is kept, a few numbers.
        PAINTED.with(|p| p.set(Some(painted)));
        BACKDROP.with(|b| b.set(Some((self.backdrop, clip.left, clip.top))));
        self.drawn.set(Some(drawn));
    }

    pub fn close(&mut self) {
        fade::fade_out(self.popup, specs::POPOVER_OUT, slide(self.position, self.scale), None);
    }

    /// Where a menu that belongs to the field opens: under the field's left
    /// end (screen pixels).
    pub fn menu_anchor(&self) -> POINT {
        let mut rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.popup, &mut rect);
        }
        let field_top = self.layout.get().map_or(0, |l| l.field_top);
        POINT { x: rect.left, y: rect.top + field_top + self.metrics.field }
    }

    pub fn text(&self) -> String {
        unsafe {
            let len = GetWindowTextLengthW(self.edit) as usize;
            let mut buffer = vec![0u16; len + 1];
            let copied = GetWindowTextW(self.edit, &mut buffer) as usize;
            String::from_utf16_lossy(&buffer[..copied])
        }
    }

    /// Caret position and whether text is selected after it.
    pub fn selection(&self) -> (usize, usize) {
        let (mut start, mut end) = (0u32, 0u32);
        unsafe {
            SendMessageW(
                self.edit,
                EM_GETSEL,
                Some(WPARAM(&mut start as *mut u32 as usize)),
                Some(LPARAM(&mut end as *mut u32 as isize)),
            );
        }
        (start as usize, end as usize)
    }

    /// Puts `completion` after the typed text, selected, so typing on
    /// replaces it and Enter accepts it.
    pub fn complete_inline(&self, typed: &str, completion: &str) {
        let full = format!("{typed}{completion}");
        let start = typed.encode_utf16().count();
        let end = full.encode_utf16().count();
        unsafe {
            let _ = SetWindowTextW(self.edit, &HSTRING::from(full));
            SendMessageW(self.edit, EM_SETSEL, Some(WPARAM(start)), Some(LPARAM(end as isize)));
        }
    }

    fn upward(&self) -> bool {
        self.position == FieldPosition::Bottom
    }

    pub fn set_rows(&mut self, rows: &[String]) {
        HOVERED.with(|h| h.set(None));
        // New rows: whatever was picked or hovered before is another row now.
        GROUNDS.with(|g| *g.borrow_mut() = Default::default());
        let upward = self.upward();
        unsafe {
            // Each add repaints a shown list otherwise: two hundred rows of
            // the history panel were a third of a second per keystroke.
            SendMessageW(self.list, WM_SETREDRAW, Some(WPARAM(0)), None);
            SendMessageW(self.list, LB_RESETCONTENT, None, None);
            let mut add = |row: &String| {
                let text = HSTRING::from(row.as_str());
                SendMessageW(self.list, LB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize)));
            };
            if upward {
                rows.iter().rev().for_each(&mut add);
            } else {
                rows.iter().for_each(&mut add);
            }
            SendMessageW(self.list, WM_SETREDRAW, Some(WPARAM(1)), None);
            let _ = InvalidateRect(Some(self.list), None, false);
        }
        self.rows = rows.len();
        self.scroll_to_end.set(upward);
    }

    /// The rows in the app's order (the first is the best), whichever way
    /// the list reads on screen.
    pub fn row_labels(&self) -> Vec<String> {
        let mut labels: Vec<String> = (0..self.rows).map(|i| item_text(self.list, i)).collect();
        if self.upward() {
            labels.reverse();
        }
        labels
    }

    pub fn selected_row(&self) -> Option<usize> {
        let index = unsafe { SendMessageW(self.list, LB_GETCURSEL, None, None).0 };
        (index >= 0 && (index as usize) < self.rows).then(|| list_index(index as usize, self.rows, self.upward()))
    }

    pub fn select_row(&self, row: Option<usize>) {
        let index = row.filter(|r| *r < self.rows).map_or(usize::MAX, |r| list_index(r, self.rows, self.upward()));
        unsafe {
            SendMessageW(self.list, LB_SETCURSEL, Some(WPARAM(index)), None);
        }
    }

    fn move_selection(&self, key_down: bool) {
        if self.rows == 0 {
            return;
        }
        self.select_row(next_selection(self.selected_row(), self.rows, key_down, self.upward()));
    }
}

thread_local! {
    static INSTANCE: Cell<Option<*const Omnibar>> = const { Cell::new(None) };
}

/// The edit's key handling needs to move the list selection; the app keeps
/// the `Omnibar` alive for the whole run and registers it here.
pub fn register(omnibar: &Omnibar) {
    INSTANCE.with(|i| i.set(Some(omnibar as *const Omnibar)));
}

unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _: usize, _: usize) -> LRESULT {
    match msg {
        WM_KEYDOWN => {
            let key = wparam.0 as u16;
            let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
            if key == VK_DOWN.0 || key == VK_UP.0 {
                if let Some(omnibar) = INSTANCE.with(|i| i.get()) {
                    unsafe { (*omnibar).move_selection(key == VK_DOWN.0) };
                }
                return LRESULT(0);
            }
            if key == VK_RETURN.0 {
                let row = INSTANCE.with(|i| i.get()).and_then(|o| unsafe { (*o).selected_row() });
                signal(Signal::Submit { row, shift });
                return LRESULT(0);
            }
            if key == VK_ESCAPE.0 {
                signal(Signal::Cancel);
                return LRESULT(0);
            }
            if key == VK_DELETE.0 {
                let row = INSTANCE
                    .with(|i| i.get())
                    .and_then(|o| unsafe { if (*o).rows_deletable { (*o).selected_row() } else { None } });
                if let Some(row) = row {
                    signal(Signal::Delete { row });
                    return LRESULT(0);
                }
            }
        }
        // Swallow the characters of Enter and Esc so the edit doesn't beep.
        WM_CHAR if wparam.0 == 0x0D || wparam.0 == 0x1B => return LRESULT(0),
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

unsafe extern "system" fn popup_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let code = (wparam.0 >> 16) as u32 & 0xFFFF;
            let id = wparam.0 & 0xFFFF;
            if id == LIST_ID && code == LBN_SELCHANGE_CODE {
                let row = INSTANCE.with(|i| i.get()).and_then(|o| unsafe { (*o).selected_row() });
                signal(Signal::Submit { row, shift: false });
            } else if id == 1 && code == EN_CHANGE_CODE {
                signal(Signal::Changed);
            }
            LRESULT(0)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let (field, field_color, ink) = BRUSHES.with(|b| b.get());
            if field == 0 {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            let hdc = HDC(wparam.0 as _);
            unsafe {
                SetTextColor(hdc, COLORREF(ink));
                SetBkColor(hdc, COLORREF(field_color));
            }
            LRESULT(field)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                let placed = INSTANCE.with(|i| i.get()).and_then(|o| (*o).layout.get().map(|l| (l, (*o).metrics)));
                if let (Some((palette, scale)), Some((layout, metrics))) = (LOOK.with(|l| l.get()), placed) {
                    SURFACES.with(|s| {
                        s.borrow_mut().0.paint(hdc, rect, |g| paint_panel(g, rect, &layout, metrics, &palette, scale))
                    });
                }
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_DRAWITEM => {
            let item = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            if item.itemID == u32::MAX {
                return LRESULT(1);
            }
            let text = item_text(item.hwndItem, item.itemID as usize);
            let row = item.itemID as usize;
            let picked = item.itemState.0 & ODS_SELECTED.0 != 0;
            let hovered = HOVERED.with(|h| h.get()) == Some(row);
            // The picked row's ground and the hover's fade in and out as a
            // menu item's colours do (150 ms ease-out).
            let now = std::time::Instant::now();
            let (picked, hovered, moving) = GROUNDS.with(|g| {
                let (p, h) = &mut *g.borrow_mut();
                p.set(row, if picked { 1.0 } else { 0.0 }, now, specs::HOVER);
                h.set(row, if hovered { 1.0 } else { 0.0 }, now, specs::HOVER);
                (p.get(row, now), h.get(row, now), p.moving(now) || h.moving(now))
            });
            if let Some((palette, scale)) = LOOK.with(|l| l.get()) {
                SURFACES.with(|s| {
                    s.borrow_mut().1.paint(item.hDC, item.rcItem, |g| {
                        paint_row(g, item.rcItem.into(), &text, picked, hovered, &palette, scale)
                    })
                });
            }
            if moving {
                anim::repaint(item.hwndItem, Some(item.rcItem));
            }
            LRESULT(1)
        }
        // Losing activation alone does not close the field: the main window
        // taking focus back (first launch, Alt+Tab) would otherwise close it.
        // A click is what closes it: on the page or the strip (the app sees
        // those), and also any other click outside it that takes activation
        // away — the page's own focus event does not always arrive, and a
        // click on another program left the field floating.
        WM_ACTIVATE => {
            if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE && clicked_outside(hwnd) {
                // Posted: activation moves while the app may be mid-call.
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_CLICKED_OUTSIDE, WPARAM(0), LPARAM(0));
                }
            }
            LRESULT(0)
        }
        WM_CLICKED_OUTSIDE => {
            if unsafe { IsWindowVisible(hwnd) }.as_bool() {
                signal(Signal::Cancel);
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_ACTIVATE as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// The panel's ground: the field's colour, the search glass in the middle
/// of the pill's left end, and a hairline between the field and the list,
/// in from the edges as far as the rows are.
fn paint_panel(g: &Painter, rect: RECT, layout: &Layout, m: Metrics, pal: &Palette, scale: f32) {
    g.fill(rect.into(), pal.field);
    let column = GLASS_COLUMN * scale;
    let (cx, cy) = (column * 0.55 - scale, layout.field_top as f32 + m.field as f32 / 2.0 - scale);
    let r = 5.5 * scale;
    let ink = Color::from(pal.ink_muted);
    g.arc(cx, cy, r, 0.0, std::f32::consts::TAU - 0.01, 1.5 * scale, ink);
    let d = r * std::f32::consts::FRAC_1_SQRT_2;
    let from = (cx + d, cy + d);
    g.line(from, (from.0 + 4.0 * scale, from.1 + 4.0 * scale), 1.6 * scale, ink);
    if let Some(y) = layout.rule {
        let y = y as f32;
        g.fill(RectF::new(m.pad as f32, y, rect.right as f32 - m.pad as f32, y + 1.0), pal.line);
    }
}

/// One row: a soft ground when picked (or, quieter, under the pointer), the
/// site's mark, the title in ink and what follows it dimmed.
fn paint_row(g: &Painter, rect: RectF, text: &str, picked: f32, hovered: f32, pal: &Palette, scale: f32) {
    g.fill(rect, pal.field);
    let ground = rect.inset(0.0, 1.0);
    if hovered > 0.0 {
        g.round(ground, size::RADIUS * scale, Color::from(pal.row_hover).with_alpha(hovered * (1.0 - picked)));
    }
    if picked > 0.0 {
        g.round(ground, size::RADIUS * scale, Color::from(pal.selection).with_alpha(picked));
    }
    let parts = RowParts::of(text);
    let x = rect.left + 10.0 * scale;
    let (_, cy) = rect.center();
    if let Some(initial) = parts.mark {
        let m = size::MARK * scale;
        let mark = RectF::new(x, cy - m / 2.0, x + m, cy + m / 2.0);
        crate::tabbar::draw_mark(g, mark, initial, false, false, pal, scale);
    }
    let x = x + (size::MARK + 10.0) * scale;
    let right = rect.right - 12.0 * scale;
    let title_font = Font::new(size::TEXT * scale, size::REGULAR, Align::Leading);
    let detail_font = Font::new(size::TEXT_SMALL * scale, size::REGULAR, Align::Leading);
    if parts.detail.is_empty() {
        g.text(parts.title, RectF::new(x, rect.top, right, rect.bottom), title_font, pal.ink);
        return;
    }
    // The title takes what it needs, up to two thirds of the row; the
    // detail what is left.
    let room = right - x;
    let title_w = g.measure(parts.title, title_font).min(room * 0.66);
    g.text(parts.title, RectF::new(x, rect.top, x + title_w + 1.0, rect.bottom), title_font, pal.ink);
    let detail_x = x + title_w + 10.0 * scale;
    if detail_x < right {
        g.text(&parts.detail, RectF::new(detail_x, rect.top, right, rect.bottom), detail_font, pal.ink_muted);
    }
}

/// How a row's label splits for drawing. The app writes labels as
/// "title    address    more" (four spaces between parts), the shape the
/// script control and the tests read back unchanged.
#[derive(Debug, PartialEq)]
struct RowParts<'a> {
    title: &'a str,
    detail: String,
    mark: Option<char>,
}

impl<'a> RowParts<'a> {
    fn of(label: &'a str) -> Self {
        let mut parts = label.split("    ").map(str::trim).filter(|p| !p.is_empty());
        let title = parts.next().unwrap_or("");
        let rest: Vec<&str> = parts.collect();
        let detail = rest.join(" · ");
        // The site's letter, from the address when the row has one.
        let mark = rest.first().filter(|a| a.contains('.') && !a.contains(' ')).and_then(|address| {
            let host = address.trim_start_matches("www.");
            host.chars().find(|c| c.is_alphanumeric())
        });
        Self { title, detail, mark }
    }
}

fn item_text(list: HWND, index: usize) -> String {
    unsafe {
        let len = SendMessageW(list, LB_GETTEXTLEN, Some(WPARAM(index)), None).0;
        if len <= 0 {
            return String::new();
        }
        let mut buffer = vec![0u16; len as usize + 1];
        SendMessageW(list, LB_GETTEXT, Some(WPARAM(index)), Some(LPARAM(buffer.as_mut_ptr() as isize)));
        String::from_utf16_lossy(&buffer[..len as usize])
    }
}

/// Tracks the row under the pointer for its quieter ground.
unsafe extern "system" fn list_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _: usize, _: usize) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let found = unsafe { SendMessageW(hwnd, LB_ITEMFROMPOINT, None, Some(lparam)).0 as u32 };
            // The high word says the point is past every row.
            let row = ((found >> 16) == 0).then_some((found & 0xFFFF) as usize);
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            set_hovered(hwnd, row);
        }
        WM_MOUSELEAVE => set_hovered(hwnd, None),
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

fn set_hovered(list: HWND, row: Option<usize>) {
    let before = HOVERED.with(|h| h.replace(row));
    if before == row {
        return;
    }
    for index in [before, row].into_iter().flatten() {
        let mut rect = RECT::default();
        unsafe {
            SendMessageW(list, LB_GETITEMRECT, Some(WPARAM(index)), Some(LPARAM(&mut rect as *mut RECT as isize)));
            let _ = InvalidateRect(Some(list), Some(&rect), false);
        }
    }
}

