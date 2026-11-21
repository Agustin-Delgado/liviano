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

use std::{cell::RefCell, collections::HashMap, rc::Rc};

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
                DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
                DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
                DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS, DWRITE_TRIMMING,
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

impl Shared {
    fn create() -> windows::core::Result<Self> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
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
            let family = HSTRING::from(pick_family(&dwrite));
            Ok(Self { d2d, dwrite, round, family, formats: RefCell::new(HashMap::new()) })
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
            let painter = Painter { target: &target, brush: &brush, shared: &shared, bitmaps: &self.bitmaps };
            draw(&painter);
            target.EndDraw(None, None)
        };
        // Keep what this frame drew; a partial repaint that drew none of the
        // icons keeps them all (they are still on screen).
        let mut bitmaps = self.bitmaps.borrow_mut();
        if bitmaps.values().any(|(_, used)| *used) {
            bitmaps.retain(|_, (_, used)| std::mem::take(used));
        }
        drop(bitmaps);
        if let Err(error) = result {
            // The target is gone with whatever device stood behind it; the
            // next paint makes a new one.
            if error.code() == D2DERR_RECREATE_TARGET {
                self.target = None;
                self.bitmaps.borrow_mut().clear();
            }
        }
        true
    }
}

fn create_target(shared: &Shared) -> windows::core::Result<(ID2D1DCRenderTarget, ID2D1SolidColorBrush)> {
    let props = D2D1_RENDER_TARGET_PROPERTIES {
        r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
        usage: D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
        minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
    };
    unsafe {
        let target = shared.d2d.CreateDCRenderTarget(&props)?;
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
}

impl Painter<'_> {
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
            draw(self);
            self.rt().SetTransform(&before);
        }
    }

    /// Draws `draw` cut to `rect`: a tab folding into its group's label.
    pub fn clip(&self, rect: RectF, draw: impl FnOnce(&Painter)) {
        unsafe { self.rt().PushAxisAlignedClip(&rect.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
        draw(self);
        unsafe { self.rt().PopAxisAlignedClip() };
    }

    /// A card on the ground (brand.css `shadow-card`): a soft drop under
    /// it, the 1 px ring around it, then its fill. `lift` 0..1 scales the
    /// shadow, so a card can fade in or out of the ground.
    pub fn card(&self, rect: RectF, radius: f32, fill: impl Into<Color>, dark: bool, lift: f32, scale: f32) {
        use crate::design::lift as l;
        let shade = Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
        if lift > 0.0 {
            // 0 2px 6px -2px: a few widening layers below, each fainter.
            for (grow, alpha) in [(0.0, 0.5), (1.0, 0.3), (2.0, 0.2)] {
                let g = grow * scale;
                let drop = RectF::new(
                    rect.left - g + 2.0 * scale,
                    rect.top + 2.0 * scale,
                    rect.right + g - 2.0 * scale,
                    rect.bottom + (1.0 + grow) * scale,
                );
                self.round(
                    drop,
                    radius + g,
                    shade.with_alpha(l::CARD_DROP * alpha * lift * if dark { 2.0 } else { 1.0 }),
                );
            }
        }
        self.round(rect, radius, fill);
        let ring = if dark {
            Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }.with_alpha(0.06)
        } else {
            shade.with_alpha(l::CARD_RING)
        };
        self.outline(rect, radius, 1.0, ring.with_alpha(lift.clamp(0.0, 1.0)));
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
        if text.is_empty() || rect.width() <= 0.0 {
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

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/gfx.rs"]
#[rustfmt::skip]
mod tests;
