//! Drawing with Direct2D and DirectWrite: antialiased shapes, round corners
//! and good type, for every surface the app draws itself.
//!
//! The factories are made once per thread (the UI has one) and shared. Each
//! window keeps a `Surface`: a DC render target bound to the HDC it is
//! handed in WM_PAINT. The render target is the software one on purpose: a
//! hardware target brings a Direct3D device and its driver into the
//! process, several megabytes for a strip of tabs, and these surfaces are
//! small enough that the CPU draws them in well under a millisecond.
//! The target draws into its own buffer and copies it out at `EndDraw`, so
//! nothing flickers.
//!
//! Coordinates are physical pixels (the target is told 96 DPI): the layout
//! code already scales everything by the window's DPI.

use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{Condvar, Mutex},
    time::Duration,
};

use windows::{
    Win32::{
        Foundation::{D2DERR_RECREATE_TARGET, RECT},
        Graphics::{
            Direct2D::{
                Common::{
                    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F,
                    D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN,
                    D2D1_PIXEL_FORMAT,
                },
                D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES, D2D1_CAP_STYLE_ROUND,
                D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
                D2D1_LINE_JOIN_ROUND, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE, D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES,
                D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1CreateFactory, ID2D1Bitmap, ID2D1DCRenderTarget, ID2D1Factory,
                ID2D1RenderTarget, ID2D1SolidColorBrush, ID2D1StrokeStyle,
            },
            DirectWrite::{
                DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_METRICS, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_WEIGHT, DWRITE_LINE_METRICS, DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
                DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS, DWRITE_TRIMMING,
                DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory,
                IDWriteFactory, IDWriteInlineObject, IDWriteTextFormat,
            },
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Gdi::HDC,
        },
    },
    core::HSTRING,
};
use windows_numerics::Vector2;

use crate::design::Rgb;

/// A colour with its opacity, as Direct2D takes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl From<Rgb> for Color {
    fn from(c: Rgb) -> Self {
        Self { r: c.0 as f32 / 255.0, g: c.1 as f32 / 255.0, b: c.2 as f32 / 255.0, a: 1.0 }
    }
}

impl Color {
    pub fn with_alpha(self, a: f32) -> Self {
        Self { a: self.a * a.clamp(0.0, 1.0), ..self }
    }
    fn d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F { r: self.r, g: self.g, b: self.b, a: self.a }
    }
}

/// A rectangle in pixels, with fractional edges for what moves.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct RectF {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl RectF {
    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self { left, top, right, bottom }
    }
    pub fn width(&self) -> f32 {
        self.right - self.left
    }
    pub fn height(&self) -> f32 {
        self.bottom - self.top
    }
    pub fn inset(&self, dx: f32, dy: f32) -> Self {
        Self::new(self.left + dx, self.top + dy, self.right - dx, self.bottom - dy)
    }
    pub fn center(&self) -> (f32, f32) {
        ((self.left + self.right) / 2.0, (self.top + self.bottom) / 2.0)
    }
    /// The rectangle `t` of the way from `self` to `to`.
    pub fn lerp(&self, to: &RectF, t: f32) -> Self {
        let mix = |a: f32, b: f32| a + (b - a) * t;
        Self::new(
            mix(self.left, to.left),
            mix(self.top, to.top),
            mix(self.right, to.right),
            mix(self.bottom, to.bottom),
        )
    }
    fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.left, top: self.top, right: self.right, bottom: self.bottom }
    }
}

impl From<RECT> for RectF {
    fn from(r: RECT) -> Self {
        Self::new(r.left as f32, r.top as f32, r.right as f32, r.bottom as f32)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Align {
    Leading,
    Center,
    Trailing,
}

/// A size in pixels (already scaled), a weight and an alignment. The
/// format behind it is made once and kept.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Font {
    pub size: f32,
    pub weight: u16,
    pub align: Align,
}

impl Font {
    pub const fn new(size: f32, weight: u16, align: Align) -> Self {
        Self { size, weight, align }
    }
}

/// Text formats by size, weight and alignment, each with its ellipsis sign.
type Formats = HashMap<(u32, u16, Align), (IDWriteTextFormat, IDWriteInlineObject)>;

struct Shared {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    round: ID2D1StrokeStyle,
    family: HSTRING,
    formats: RefCell<Formats>,
    marks: RefCell<HashMap<(u32, u16, Align), LineMarks>>,
}

thread_local! {
    static SHARED: Option<Rc<Shared>> = Shared::create().ok().map(Rc::new);
}

fn shared() -> Option<Rc<Shared>> {
    SHARED.with(|s| s.clone())
}

/// The face the interface is set in: Segoe UI Variable where Windows has it
/// (11), Segoe UI otherwise. The GDI edit controls use the same name.
pub fn family() -> String {
    shared().map_or_else(|| "Segoe UI".to_string(), |s| s.family.to_string())
}

/// The box text is laid out in to be measured. Finite and modest: in a
/// centred format DirectWrite places the line at half of it, and at
/// `f32::MAX / 4` the line's own width is lost to rounding (it measured 0).
const MEASURE_WIDTH: f32 = 100_000.0;

/// How wide `text` is in `font`, in pixels, without a target to draw on
/// (to size a window before drawing it).
pub fn measure(text: &str, font: Font) -> f32 {
    let Some(shared) = shared() else { return 0.0 };
    let Some((format, _)) = shared.format(font) else { return 0.0 };
    let wide: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        let Ok(layout) = shared.dwrite.CreateTextLayout(&wide, &format, MEASURE_WIDTH, 1000.0) else {
            return 0.0;
        };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        if layout.GetMetrics(&mut metrics).is_err() {
            return 0.0;
        }
        metrics.widthIncludingTrailingWhitespace
    }
}

/// erfc(x), Abramowitz and Stegun 7.1.26 (error under 1.5e-7).
pub fn erfc(x: f32) -> f32 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * z);
    let poly = t * (0.254_829_6 + t * (-0.284_496_74 + t * (1.421_413_7 + t * (-1.453_152 + t * 1.061_405_4))));
    let y = poly * (-z * z).exp();
    if x >= 0.0 { y } else { 2.0 - y }
}

/// A CSS blur of `blur` device pixels (two sigmas) as bands of a shape
/// grown by `outset`, each with its share of the shadow's opacity. Stacked,
/// the bands give the blurred edge: at a distance `d` past the shape, the
/// shares of the bands that reach it add up to the Gaussian's tail there.
pub fn drop_bands(blur: f32) -> Vec<(f32, f32)> {
    let sigma = (blur / 2.0).max(0.25);
    let tail = |d: f32| 0.5 * erfc(d / (sigma * std::f32::consts::SQRT_2));
    // Half a pixel apart, from 2.5 sigmas out to the edge (the card covers
    // what is inside it).
    let steps = (2.5 * sigma / 0.5).ceil() as i32;
    let mut bands = Vec::new();
    let mut reached = 0.0;
    for k in (0..=steps).rev() {
        let d = k as f32 * 0.5;
        let share = tail(d - 0.25) - reached;
        reached += share;
        bands.push((d, share));
    }
    bands
}

/// Where a line of text sits in the box `Painter::text` centres it in,
/// from the font's own metrics: to set a mark on the letters, not on the box.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LineMarks {
    /// The line's height (ascent, descent and gap, as DirectWrite lays it).
    pub height: f32,
    /// From the line's top down to its baseline.
    pub baseline: f32,
    /// The height of a lowercase letter over the baseline.
    pub x_height: f32,
    /// The height of a capital over the baseline.
    pub cap_height: f32,
}

impl LineMarks {
    /// The baseline's y when the line is centred in a box from `top` to `bottom`.
    pub fn baseline_in(&self, top: f32, bottom: f32) -> f32 {
        top + (bottom - top - self.height) / 2.0 + self.baseline
    }

    /// The y of the middle of the lowercase letters, in that box.
    pub fn x_middle_in(&self, top: f32, bottom: f32) -> f32 {
        self.baseline_in(top, bottom) - self.x_height / 2.0
    }
}

/// The line metrics of `font` (see `LineMarks`); zero when DirectWrite fails.
pub fn line_marks(font: Font) -> LineMarks {
    let Some(shared) = shared() else { return LineMarks::default() };
    let key = ((font.size * 4.0).round() as u32, font.weight, font.align);
    if let Some(found) = shared.marks.borrow().get(&key) {
        return *found;
    }
    let Some((format, _)) = shared.format(font) else { return LineMarks::default() };
    let marks = unsafe {
        let wide: Vec<u16> = "x".encode_utf16().collect();
        let mut line = [DWRITE_LINE_METRICS::default()];
        let mut count = 0u32;
        let laid = shared
            .dwrite
            .CreateTextLayout(&wide, &format, MEASURE_WIDTH, 1000.0)
            .ok()
            .and_then(|layout| layout.GetLineMetrics(Some(&mut line), &mut count).ok());
        let mut face = DWRITE_FONT_METRICS::default();
        let mut collection = None;
        let found = shared.dwrite.GetSystemFontCollection(&mut collection, false).is_ok()
            && collection.is_some_and(|c| {
                let (mut index, mut exists) = (0u32, windows::core::BOOL(0));
                c.FindFamilyName(&shared.family, &mut index, &mut exists).is_ok()
                    && exists.as_bool()
                    && c.GetFontFamily(index)
                        .and_then(|f| {
                            f.GetFirstMatchingFont(
                                DWRITE_FONT_WEIGHT(font.weight as i32),
                                DWRITE_FONT_STRETCH_NORMAL,
                                DWRITE_FONT_STYLE_NORMAL,
                            )
                        })
                        .map(|f| f.GetMetrics(&mut face))
                        .is_ok()
            });
        if laid.is_none() || count == 0 || !found || face.designUnitsPerEm == 0 {
            return LineMarks::default();
        }
        let unit = font.size / face.designUnitsPerEm as f32;
        LineMarks {
            height: line[0].height,
            baseline: line[0].baseline,
            x_height: face.xHeight as f32 * unit,
            cap_height: face.capHeight as f32 * unit,
        }
    };
    shared.marks.borrow_mut().insert(key, marks);
    marks
}

impl Shared {
    fn create() -> windows::core::Result<Self> {
        let warmed = take_warmed();
        unsafe {
            let (d2d, dwrite, family) = match warmed {
                Some(w) => (w.d2d, w.dwrite, w.family),
                None => {
                    let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
                    let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
                    let family = pick_family(&dwrite);
                    (d2d, dwrite, family)
                }
            };
            let round = d2d.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    miterLimit: 10.0,
                    ..Default::default()
                },
                None,
            )?;
            let family = HSTRING::from(family);
            Ok(Self {
                d2d,
                dwrite,
                round,
                family,
                formats: RefCell::new(HashMap::new()),
                marks: RefCell::new(HashMap::new()),
            })
        }
    }

    fn format(&self, font: Font) -> Option<(IDWriteTextFormat, IDWriteInlineObject)> {
        let key = ((font.size * 4.0).round() as u32, font.weight, font.align);
        if let Some(found) = self.formats.borrow().get(&key) {
            return Some(found.clone());
        }
        let made = unsafe {
            let format = self
                .dwrite
                .CreateTextFormat(
                    &self.family,
                    None,
                    DWRITE_FONT_WEIGHT(font.weight as i32),
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    font.size,
                    &HSTRING::from("en-us"),
                )
                .ok()?;
            let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            let _ = format.SetTextAlignment(match font.align {
                Align::Leading => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Trailing => DWRITE_TEXT_ALIGNMENT_TRAILING,
            });
            let ellipsis = self.dwrite.CreateEllipsisTrimmingSign(&format).ok()?;
            let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
            let _ = format.SetTrimming(&trimming, &ellipsis);
            (format, ellipsis)
        };
        self.formats.borrow_mut().insert(key, made.clone());
        Some(made)
    }
}

fn pick_family(dwrite: &IDWriteFactory) -> &'static str {
    const PREFERRED: &str = "Segoe UI Variable Text";
    unsafe {
        let mut collection = None;
        if dwrite.GetSystemFontCollection(&mut collection, false).is_ok()
            && let Some(collection) = collection
        {
            let (mut index, mut exists) = (0u32, windows::core::BOOL(0));
            if collection.FindFamilyName(&HSTRING::from(PREFERRED), &mut index, &mut exists).is_ok() && exists.as_bool()
            {
                return PREFERRED;
            }
        }
    }
    "Segoe UI"
}

/// Pictures up to this many pixels (64 × 64) are kept between frames.
const CACHED_IMAGE_PIXELS: u32 = 64 * 64;

/// A picture's identity: its size and an FNV-1a hash of its pixels (a
/// favicon is 1 KB; hashing it is far cheaper than uploading it).
fn image_key(pixels: &[u8], width: u32, height: u32) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ ((width as u64) << 32 | height as u64);
    for chunk in pixels.as_chunks::<8>().0 {
        h ^= u64::from_le_bytes(*chunk);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// One window's (or one owner-drawn control's) way onto the screen.
#[derive(Default)]
pub struct Surface {
    target: Option<(ID2D1DCRenderTarget, ID2D1SolidColorBrush)>,
    /// Small pictures (site icons) as device bitmaps, by content: made once
    /// instead of on every frame, dropped when a frame no longer draws them
    /// or the target is recreated.
    bitmaps: RefCell<HashMap<u64, (ID2D1Bitmap, bool)>>,
}

impl Surface {
    /// Draws `rect` of `hdc` with `draw`. The coordinates `draw` sees are
    /// those of `hdc` (the target is offset to `rect`). Returns false when
    /// Direct2D is not there, so the caller can fall back to plain GDI.
    pub fn paint(&mut self, hdc: HDC, rect: RECT, draw: impl FnOnce(&Painter)) -> bool {
        let Some(shared) = shared() else { return false };
        if self.target.is_none() {
            self.target = create_target(&shared).ok();
        }
        let Some((target, brush)) = self.target.clone() else { return false };
        let result = unsafe {
            if target.BindDC(hdc, &rect).is_err() {
                self.target = None;
                self.bitmaps.borrow_mut().clear();
                return false;
            }
            target.BeginDraw();
            let offset = windows_numerics::Matrix3x2::translation(-rect.left as f32, -rect.top as f32);
            target.SetTransform(&offset);
            let bounds = RectF::new(rect.left as f32, rect.top as f32, rect.right as f32, rect.bottom as f32);
            let painter = Painter {
                target: &target,
                brush: &brush,
                shared: &shared,
                bitmaps: &self.bitmaps,
                bounds: std::cell::Cell::new(bounds),
            };
            draw(&painter);
            target.EndDraw(None, None)
        };
        self.frame_ended(result);
        true
    }

    /// What a frame leaves behind: the pictures it drew, and the target
    /// only if `EndDraw` succeeded.
    fn frame_ended(&mut self, result: windows::core::Result<()>) {
        // Keep what this frame drew; a partial repaint that drew none of the
        // icons keeps them all (they are still on screen).
        let mut bitmaps = self.bitmaps.borrow_mut();
        if bitmaps.values().any(|(_, used)| *used) {
            bitmaps.retain(|_, (_, used)| std::mem::take(used));
        }
        drop(bitmaps);
        if let Err(error) = result {
            // D2DERR_RECREATE_TARGET: the target is gone with whatever stood
            // behind it. Any other error leaves a target in a state nobody
            // knows. Both: the next paint makes a new one, a few ms.
            crate::diag::trace(|| {
                let lost = error.code() == D2DERR_RECREATE_TARGET;
                format!("draw target dropped: 0x{:08X} (lost {lost})", error.code().0)
            });
            self.target = None;
            self.bitmaps.borrow_mut().clear();
        }
    }
}

/// What the warm-up thread made for the UI thread: the factories, which
/// already drew once. A single-threaded factory only asks that one thread
/// at a time uses it: the thread lets go of it before the UI thread takes
/// it, through the lock.
struct Warmed {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    family: &'static str,
}

// Refer to `Warmed`: never used by two threads at once.
unsafe impl Send for Warmed {}

/// The warm-up's result, and whether the thread is done.
static WARMED: Mutex<(Option<Warmed>, bool)> = Mutex::new((None, false));
static WARMED_DONE: Condvar = Condvar::new();
/// The UI thread waits this long at most for the warm-up to end.
const WARM_WAIT: Duration = Duration::from_millis(150);

/// Starts Direct2D and DirectWrite on a thread of its own, while the window
/// is made. A factory's first target, its first binding and its first text
/// set up its software renderer: 30-45 ms measured inside the first paint,
/// once for each factory (a factory made on another thread warmed nothing
/// of this one's). So the thread makes the UI thread's own factories,
/// draws with them once, and hands them over. Not its target: one made on
/// this thread drew each paint of the UI thread in 20-60 ms.
pub fn warm_up() {
    let spawned = std::thread::Builder::new().name("gfx-warm".into()).stack_size(256 * 1024).spawn(|| {
        let warmed = warm();
        let mut slot = WARMED.lock().unwrap_or_else(|e| e.into_inner());
        *slot = (warmed, true);
        WARMED_DONE.notify_all();
    });
    if spawned.is_err() {
        WARMED.lock().unwrap_or_else(|e| e.into_inner()).1 = true;
    }
}

fn warm() -> Option<Warmed> {
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    };
    unsafe {
        let d2d = D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()?;
        let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
        let family = pick_family(&dwrite);
        let target = d2d.CreateDCRenderTarget(&target_properties()).ok().and_then(|target| {
            let brush = target.CreateSolidColorBrush(&Color::from(Rgb(0, 0, 0)).d2d(), None).ok()?;
            Some((target, brush))
        });
        if let Some((target, brush)) = &target {
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            let bitmap = CreateCompatibleBitmap(screen, 64, 32);
            let old = SelectObject(dc, bitmap.into());
            let rect = RECT { left: 0, top: 0, right: 64, bottom: 32 };
            if target.BindDC(dc, &rect).is_ok() {
                target.BeginDraw();
                target.Clear(Some(&Color::from(Rgb(255, 255, 255)).d2d()));
                let round =
                    D2D1_ROUNDED_RECT { rect: RectF::new(2.0, 2.0, 60.0, 30.0).d2d(), radiusX: 6.0, radiusY: 6.0 };
                target.FillRoundedRectangle(&round, brush);
                if let Ok(format) = dwrite.CreateTextFormat(
                    &HSTRING::from(family),
                    None,
                    DWRITE_FONT_WEIGHT(400),
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    13.0,
                    &HSTRING::from("en-us"),
                ) {
                    let text: Vec<u16> = "Nea".encode_utf16().collect();
                    target.DrawText(
                        &text,
                        &format,
                        &rect_f(rect),
                        brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
                let _ = target.EndDraw(None, None);
            }
            SelectObject(dc, old);
            let _ = DeleteObject(bitmap.into());
            let _ = DeleteDC(dc);
            ReleaseDC(None, screen);
        }
        drop(target);
        Some(Warmed { d2d, dwrite, family })
    }
}

fn rect_f(r: RECT) -> D2D_RECT_F {
    D2D_RECT_F { left: r.left as f32, top: r.top as f32, right: r.right as f32, bottom: r.bottom as f32 }
}

/// The warm-up's factories, once it is done (waiting for it a little).
fn take_warmed() -> Option<Warmed> {
    let slot = WARMED.lock().unwrap_or_else(|e| e.into_inner());
    let (mut slot, _) =
        WARMED_DONE.wait_timeout_while(slot, WARM_WAIT, |(_, done)| !*done).unwrap_or_else(|e| e.into_inner());
    slot.0.take()
}

fn target_properties() -> D2D1_RENDER_TARGET_PROPERTIES {
    D2D1_RENDER_TARGET_PROPERTIES {
        r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
        usage: D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
        minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
    }
}

fn create_target(shared: &Shared) -> windows::core::Result<(ID2D1DCRenderTarget, ID2D1SolidColorBrush)> {
    unsafe {
        let target = shared.d2d.CreateDCRenderTarget(&target_properties())?;
        let brush = target.CreateSolidColorBrush(&Color::from(Rgb(0, 0, 0)).d2d(), None)?;
        Ok((target, brush))
    }
}

/// What a draw callback gets: shapes, lines and text in pixels.
pub struct Painter<'a> {
    target: &'a ID2D1DCRenderTarget,
    brush: &'a ID2D1SolidColorBrush,
    shared: &'a Shared,
    bitmaps: &'a RefCell<HashMap<u64, (ID2D1Bitmap, bool)>>,
    /// The part of the target this paint draws, in the coordinates the
    /// drawing code sees (an `offset` moves it the other way).
    bounds: std::cell::Cell<RectF>,
}

impl Painter<'_> {
    /// Whether anything drawn in `r` shows in this paint: what does not is
    /// skipped (a frame of a hover redraws two tabs, not the whole strip).
    pub fn visible(&self, r: RectF) -> bool {
        let b = self.bounds.get();
        r.right > b.left && r.left < b.right && r.bottom > b.top && r.top < b.bottom
    }

    fn rt(&self) -> &ID2D1RenderTarget {
        self.target
    }

    fn ink(&self, color: impl Into<Color>) -> &ID2D1SolidColorBrush {
        unsafe { self.brush.SetColor(&color.into().d2d()) };
        self.brush
    }

    /// Everything to `color`, alpha included (a layered window's
    /// transparent ground).
    pub fn clear(&self, color: impl Into<Color>) {
        unsafe { self.rt().Clear(Some(&color.into().d2d())) };
    }

    /// Draws `draw` moved by (dx, dy): what slides in.
    pub fn offset(&self, dx: f32, dy: f32, draw: impl FnOnce(&Painter)) {
        if dx == 0.0 && dy == 0.0 {
            draw(self);
            return;
        }
        unsafe {
            let mut before = windows_numerics::Matrix3x2::default();
            self.rt().GetTransform(&mut before);
            let moved = windows_numerics::Matrix3x2 { M31: before.M31 + dx, M32: before.M32 + dy, ..before };
            self.rt().SetTransform(&moved);
            let bounds = self.bounds.get();
            self.bounds.set(RectF::new(bounds.left - dx, bounds.top - dy, bounds.right - dx, bounds.bottom - dy));
            draw(self);
            self.bounds.set(bounds);
            self.rt().SetTransform(&before);
        }
    }

    /// Draws `draw` cut to `rect`: a tab folding into its group's label.
    pub fn clip(&self, rect: RectF, draw: impl FnOnce(&Painter)) {
        unsafe { self.rt().PushAxisAlignedClip(&rect.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
        draw(self);
        unsafe { self.rt().PopAxisAlignedClip() };
    }

    /// The live tab's card (brand.css `shadow-ring`, as the active entry of
    /// a sidebar): a small drop under it (0 1px 2px), a 1 px ring just
    /// outside its edge, then its fill. On a dark ground the ring turns
    /// light and the drop deepens, so it reads the same. `opacity` fades
    /// all of it.
    pub fn ring_card(&self, rect: RectF, radius: f32, fill: impl Into<Color>, dark: bool, opacity: f32, scale: f32) {
        use crate::design::lift;
        let black = Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
        let (ring, drop) = if dark {
            (Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }.with_alpha(lift::RING_DARK), lift::RING_DROP_DARK)
        } else {
            (black.with_alpha(lift::RING), lift::RING_DROP)
        };
        let down = (lift::RING_DROP_Y * scale).round();
        for (outset, share) in drop_bands(lift::RING_DROP_BLUR * scale) {
            let band = RectF::new(
                rect.left - outset,
                rect.top + down - outset,
                rect.right + outset,
                rect.bottom + down + outset,
            );
            self.round(band, (radius + outset).max(0.0), black.with_alpha(drop * share * opacity));
        }
        // The ring: a whole device pixel per 100 %, outside the edge.
        let w = scale.round().max(1.0);
        let around = rect.inset(-w / 2.0, -w / 2.0);
        let rounded = D2D1_ROUNDED_RECT { rect: around.d2d(), radiusX: radius + w / 2.0, radiusY: radius + w / 2.0 };
        unsafe { self.rt().DrawRoundedRectangle(&rounded, self.ink(ring.with_alpha(opacity)), w, None) };
        self.round(rect, radius, fill);
    }

    pub fn fill(&self, rect: RectF, color: impl Into<Color>) {
        unsafe { self.rt().FillRectangle(&rect.d2d(), self.ink(color)) };
    }

    pub fn round(&self, rect: RectF, radius: f32, color: impl Into<Color>) {
        let rounded = D2D1_ROUNDED_RECT { rect: rect.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.rt().FillRoundedRectangle(&rounded, self.ink(color)) };
    }

    /// A hairline (or thicker) outline just inside `rect`.
    pub fn outline(&self, rect: RectF, radius: f32, width: f32, color: impl Into<Color>) {
        let half = width / 2.0;
        let rounded = D2D1_ROUNDED_RECT { rect: rect.inset(half, half).d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.rt().DrawRoundedRectangle(&rounded, self.ink(color), width, None) };
    }

    pub fn line(&self, from: (f32, f32), to: (f32, f32), width: f32, color: impl Into<Color>) {
        unsafe {
            self.rt().DrawLine(
                Vector2::new(from.0, from.1),
                Vector2::new(to.0, to.1),
                self.ink(color),
                width,
                &self.shared.round,
            )
        };
    }

    /// An open polyline with round joins (chevrons, ticks).
    pub fn polyline(&self, points: &[(f32, f32)], width: f32, color: impl Into<Color>) {
        let Some(&first) = points.first() else { return };
        unsafe {
            let Ok(path) = self.shared.d2d.CreatePathGeometry() else { return };
            let Ok(sink) = path.Open() else { return };
            sink.BeginFigure(Vector2::new(first.0, first.1), D2D1_FIGURE_BEGIN_HOLLOW);
            let rest: Vec<Vector2> = points[1..].iter().map(|p| Vector2::new(p.0, p.1)).collect();
            sink.AddLines(&rest);
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            if sink.Close().is_ok() {
                self.rt().DrawGeometry(&path, self.ink(color), width, &self.shared.round);
            }
        }
    }

    /// A filled polygon (the window buttons' small arrows).
    pub fn polygon(&self, points: &[(f32, f32)], color: impl Into<Color>) {
        let Some(&first) = points.first() else { return };
        unsafe {
            let Ok(path) = self.shared.d2d.CreatePathGeometry() else { return };
            let Ok(sink) = path.Open() else { return };
            sink.BeginFigure(Vector2::new(first.0, first.1), D2D1_FIGURE_BEGIN_FILLED);
            let rest: Vec<Vector2> = points[1..].iter().map(|p| Vector2::new(p.0, p.1)).collect();
            sink.AddLines(&rest);
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            if sink.Close().is_ok() {
                self.rt().FillGeometry(&path, self.ink(color), None);
            }
        }
    }

    /// Part of a circle's edge, from `start` turning `sweep` (radians,
    /// clockwise, 0 at the top): the loading ring.
    #[allow(clippy::too_many_arguments, reason = "the arc's geometry and look, as Direct2D's own calls take them")]
    pub fn arc(&self, cx: f32, cy: f32, radius: f32, start: f32, sweep: f32, width: f32, color: impl Into<Color>) {
        let at = |angle: f32| Vector2::new(cx + radius * angle.sin(), cy - radius * angle.cos());
        unsafe {
            let Ok(path) = self.shared.d2d.CreatePathGeometry() else { return };
            let Ok(sink) = path.Open() else { return };
            sink.BeginFigure(at(start), D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: at(start + sweep),
                size: D2D_SIZE_F { width: radius, height: radius },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: if sweep > std::f32::consts::PI { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            if sink.Close().is_ok() {
                self.rt().DrawGeometry(&path, self.ink(color), width, &self.shared.round);
            }
        }
    }

    /// How wide `text` is in `font`, in pixels.
    pub fn measure(&self, text: &str, font: Font) -> f32 {
        let Some((format, _)) = self.shared.format(font) else { return 0.0 };
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let Ok(layout) = self.shared.dwrite.CreateTextLayout(&wide, &format, MEASURE_WIDTH, 1000.0) else {
                return 0.0;
            };
            let mut metrics = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut metrics).is_err() {
                return 0.0;
            }
            metrics.widthIncludingTrailingWhitespace
        }
    }

    /// A picture (premultiplied BGRA, `width` × `height`) drawn into `rect`.
    /// A small one (a site's icon) is kept as a device bitmap keyed by its
    /// content, so a strip of thirty tabs doesn't upload thirty icons per
    /// frame; a large one (a page preview) is made for this one draw.
    pub fn image(&self, pixels: &[u8], width: u32, height: u32, rect: RectF, opacity: f32) {
        if width == 0 || height == 0 || pixels.len() < (width * height * 4) as usize {
            return;
        }
        let pixels = &pixels[..(width * height * 4) as usize];
        let cache = (width * height <= CACHED_IMAGE_PIXELS).then_some(self.bitmaps);
        let key = cache.map(|_| image_key(pixels, width, height));
        let cached = cache.zip(key).and_then(|(c, k)| {
            let mut c = c.borrow_mut();
            let entry = c.get_mut(&k)?;
            entry.1 = true;
            Some(entry.0.clone())
        });
        let bitmap = match cached {
            Some(bitmap) => bitmap,
            None => {
                let Some(bitmap) = self.upload(pixels, width, height) else { return };
                if let (Some(c), Some(k)) = (cache, key) {
                    c.borrow_mut().insert(k, (bitmap.clone(), true));
                }
                bitmap
            }
        };
        unsafe {
            self.rt().DrawBitmap(&bitmap, Some(&rect.d2d()), opacity, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
        }
    }

    fn upload(&self, pixels: &[u8], width: u32, height: u32) -> Option<ID2D1Bitmap> {
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        unsafe {
            self.rt()
                .CreateBitmap(D2D_SIZE_U { width, height }, Some(pixels.as_ptr() as *const _), width * 4, &props)
                .ok()
        }
    }

    /// One line of text in `rect`, centred vertically, cut with an
    /// ellipsis when it does not fit.
    pub fn text(&self, text: &str, rect: RectF, font: Font, color: impl Into<Color>) {
        if text.is_empty() || rect.width() <= 0.0 || !self.visible(rect) {
            return;
        }
        let Some((format, _)) = self.shared.format(font) else { return };
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            self.rt().DrawText(
                &wide,
                &format,
                &rect.d2d(),
                self.ink(color),
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            )
        };
    }
}

/// The rectangles of `dirty` that `hdc` can really draw on, in client
/// coordinates: a window with `WS_CLIPCHILDREN` asked to repaint whole
/// only shows the chrome around its pages, and a software Direct2D target
/// renders every pixel it is bound to, seen or not. Too many pieces (or no
/// region to read): `dirty` as one.
pub fn visible_rects(hdc: HDC, hwnd: windows::Win32::Foundation::HWND, dirty: RECT) -> Vec<RECT> {
    use windows::Win32::{
        Foundation::POINT,
        Graphics::Gdi::{
            ClientToScreen, CreateRectRgn, DeleteObject, GetRandomRgn, GetRegionData, RGNDATA, RGNDATAHEADER,
        },
    };
    const SYSRGN: i32 = 4;
    const MAX_PIECES: usize = 8;
    let mut pieces = Vec::new();
    unsafe {
        let rgn = CreateRectRgn(0, 0, 0, 0);
        if GetRandomRgn(hdc, rgn, SYSRGN) == 1 {
            let size = GetRegionData(rgn, 0, None) as usize;
            // u64 storage keeps the header and the RECTs after it aligned.
            let mut buffer = vec![0u64; size.div_ceil(8).max(1)];
            let data = buffer.as_mut_ptr() as *mut RGNDATA;
            if size >= std::mem::size_of::<RGNDATAHEADER>()
                && GetRegionData(rgn, size as u32, Some(data)) as usize == size
            {
                let header = (*data).rdh;
                let count = header.nCount as usize;
                if (1..=MAX_PIECES).contains(&count) {
                    // The system region is in screen coordinates.
                    let mut origin = POINT::default();
                    let _ = ClientToScreen(hwnd, &mut origin);
                    let first = (data as *const u8).add(header.dwSize as usize) as *const RECT;
                    for i in 0..count {
                        let r = *first.add(i);
                        let piece = RECT {
                            left: (r.left - origin.x).max(dirty.left),
                            top: (r.top - origin.y).max(dirty.top),
                            right: (r.right - origin.x).min(dirty.right),
                            bottom: (r.bottom - origin.y).min(dirty.bottom),
                        };
                        if piece.right > piece.left && piece.bottom > piece.top {
                            pieces.push(piece);
                        }
                    }
                }
            }
        }
        let _ = DeleteObject(rgn.into());
    }
    if pieces.is_empty() {
        pieces.push(dirty);
    }
    pieces
}

/// Fills `rect` with plain GDI: what is drawn when Direct2D is missing.
pub fn fill_gdi(hdc: HDC, rect: RECT, color: Rgb) {
    use windows::Win32::Graphics::Gdi::{CreateSolidBrush, DeleteObject, FillRect};
    unsafe {
        let brush = CreateSolidBrush(color.colorref());
        FillRect(hdc, &rect, brush);
        let _ = DeleteObject(brush.into());
    }
}

/// Draws a picture (BGRA with no transparent part, `width` x `height`)
/// into `rect` with plain GDI: a large picture drawn one time, with no
/// Direct2D target for all of its area.
pub fn picture_gdi(hdc: HDC, rect: RECT, pixels: &[u8], width: u32, height: u32) {
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HALFTONE, SRCCOPY, SetBrushOrgEx, SetStretchBltMode,
        StretchDIBits,
    };
    if width == 0 || height == 0 || pixels.len() < (width * height * 4) as usize {
        return;
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            // Negative: the first row is the top one.
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    unsafe {
        SetStretchBltMode(hdc, HALFTONE);
        let _ = SetBrushOrgEx(hdc, 0, 0, None);
        StretchDIBits(
            hdc,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            0,
            0,
            width as i32,
            height as i32,
            Some(pixels.as_ptr() as *const _),
            &info,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/gfx.rs"]
#[rustfmt::skip]
mod tests;
