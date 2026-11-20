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
                    D2D_RECT_F, D2D_SIZE_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_HOLLOW,
                    D2D1_FIGURE_END_OPEN, D2D1_PIXEL_FORMAT,
                },
                D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL,
                D2D1_CAP_STYLE_ROUND, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED,
                D2D1_FEATURE_LEVEL_DEFAULT, D2D1_LINE_JOIN_ROUND, D2D1_RENDER_TARGET_PROPERTIES,
                D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE, D2D1_ROUNDED_RECT,
                D2D1_STROKE_STYLE_PROPERTIES, D2D1_SWEEP_DIRECTION_CLOCKWISE,
                D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory,
                ID2D1RenderTarget, ID2D1SolidColorBrush, ID2D1StrokeStyle,
            },
            DirectWrite::{
                DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
                DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
                DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_METRICS, DWRITE_TRIMMING,
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
        Self::new(mix(self.left, to.left), mix(self.top, to.top), mix(self.right, to.right), mix(self.bottom, to.bottom))
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

struct Shared {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    round: ID2D1StrokeStyle,
    family: HSTRING,
    formats: RefCell<HashMap<(u32, u16, Align), (IDWriteTextFormat, IDWriteInlineObject)>>,
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
                    &HSTRING::from("es-ar"),
                )
                .ok()?;
            let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            let _ = format.SetTextAlignment(match font.align {
                Align::Leading => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
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
        if dwrite.GetSystemFontCollection(&mut collection, false).is_ok() {
            if let Some(collection) = collection {
                let (mut index, mut exists) = (0u32, windows::core::BOOL(0));
                if collection.FindFamilyName(&HSTRING::from(PREFERRED), &mut index, &mut exists).is_ok()
                    && exists.as_bool()
                {
                    return PREFERRED;
                }
            }
        }
    }
    "Segoe UI"
}

/// One window's (or one owner-drawn control's) way onto the screen.
#[derive(Default)]
pub struct Surface {
    target: Option<(ID2D1DCRenderTarget, ID2D1SolidColorBrush)>,
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
                return false;
            }
            target.BeginDraw();
            let offset = windows_numerics::Matrix3x2::translation(-rect.left as f32, -rect.top as f32);
            target.SetTransform(&offset);
            let painter = Painter { target: &target, brush: &brush, shared: &shared };
            draw(&painter);
            target.EndDraw(None, None)
        };
        if let Err(error) = result {
            // The target is gone with whatever device stood behind it; the
            // next paint makes a new one.
            if error.code() == D2DERR_RECREATE_TARGET {
                self.target = None;
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
}

impl Painter<'_> {
    fn rt(&self) -> &ID2D1RenderTarget {
        self.target
    }

    fn ink(&self, color: impl Into<Color>) -> &ID2D1SolidColorBrush {
        unsafe { self.brush.SetColor(&color.into().d2d()) };
        self.brush
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

    /// Part of a circle's edge, from `start` turning `sweep` (radians,
    /// clockwise, 0 at the top): the loading ring.
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
            let Ok(layout) = self.shared.dwrite.CreateTextLayout(&wide, &format, f32::MAX / 4.0, 1000.0) else {
                return 0.0;
            };
            let mut metrics = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut metrics).is_err() {
                return 0.0;
            }
            metrics.widthIncludingTrailingWhitespace
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

/// Fills `rect` with plain GDI: what is drawn when Direct2D is missing.
pub fn fill_gdi(hdc: HDC, rect: RECT, color: Rgb) {
    use windows::Win32::Graphics::Gdi::{CreateSolidBrush, DeleteObject, FillRect};
    unsafe {
        let brush = CreateSolidBrush(color.colorref());
        FillRect(hdc, &rect, brush);
        let _ = DeleteObject(brush.into());
    }
}

