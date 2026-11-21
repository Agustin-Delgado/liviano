//! Small images through WIC: a site icon or a page preview decoded and
//! scaled to the pixels it is drawn at (premultiplied BGRA, what Direct2D
//! takes), and a preview re-encoded as a small JPEG to be kept compressed.

use windows::{
    Win32::{
        Graphics::Imaging::*,
        System::Com::{
            CLSCTX_INPROC_SERVER, CoCreateInstance, IStream, STATFLAG_NONAME, STREAM_SEEK_SET,
            StructuredStorage::CreateStreamOnHGlobal,
        },
    },
    core::Result,
};

/// Pixels ready to draw: premultiplied BGRA, `width` × `height`.
#[derive(Debug, PartialEq)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

thread_local! {
    static FACTORY: Option<IWICImagingFactory> =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok() };
}

fn factory() -> Option<IWICImagingFactory> {
    FACTORY.with(|f| f.clone())
}

/// The size an image of `w` × `h` takes to fit `max_w` wide (never larger).
pub fn fit_width(w: u32, h: u32, max_w: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (0, 0);
    }
    if w <= max_w {
        return (w, h);
    }
    let height = ((h as u64 * max_w as u64 + w as u64 / 2) / w as u64).max(1) as u32;
    (max_w, height)
}

fn source(f: &IWICImagingFactory, encoded: &[u8]) -> Result<(IWICBitmapFrameDecode, u32, u32)> {
    unsafe {
        let stream = f.CreateStream()?;
        stream.InitializeFromMemory(encoded)?;
        let decoder = f.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnLoad)?;
        let frame = decoder.GetFrame(0)?;
        let (mut w, mut h) = (0, 0);
        frame.GetSize(&mut w, &mut h)?;
        Ok((frame, w, h))
    }
}

fn read(f: &IWICImagingFactory, from: &IWICBitmapSource, width: u32, height: u32) -> Result<Pixels> {
    unsafe {
        let scaler = f.CreateBitmapScaler()?;
        scaler.Initialize(from, width, height, WICBitmapInterpolationModeHighQualityCubic)?;
        let converter = f.CreateFormatConverter()?;
        converter.Initialize(
            &scaler,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let stride = width * 4;
        let mut data = vec![0u8; (stride * height) as usize];
        converter.CopyPixels(std::ptr::null(), stride, &mut data)?;
        Ok(Pixels { width, height, data })
    }
}

/// Decodes a PNG/JPEG/ICO and scales it to a `size` × `size` square.
pub fn decode_square(encoded: &[u8], size: u32) -> Option<Pixels> {
    let f = factory()?;
    let (frame, w, h) = source(&f, encoded).ok()?;
    if w == 0 || h == 0 || size == 0 {
        return None;
    }
    read(&f, &frame.into(), size, size).ok()
}

/// Decodes an image scaled to fit `max_w` wide, keeping its proportions.
pub fn decode_fit(encoded: &[u8], max_w: u32) -> Option<Pixels> {
    let f = factory()?;
    let (frame, w, h) = source(&f, encoded).ok()?;
    let (width, height) = fit_width(w, h, max_w);
    if width == 0 {
        return None;
    }
    read(&f, &frame.into(), width, height).ok()
}

/// Re-encodes an image as a JPEG at most `max_w` wide: a page preview kept
/// small and compressed until it is shown.
pub fn shrink_to_jpeg(encoded: &[u8], max_w: u32, quality: f32) -> Option<Vec<u8>> {
    let f = factory()?;
    let (frame, w, h) = source(&f, encoded).ok()?;
    let (width, height) = fit_width(w, h, max_w);
    if width == 0 {
        return None;
    }
    unsafe {
        let scaler = f.CreateBitmapScaler().ok()?;
        scaler.Initialize(&frame, width, height, WICBitmapInterpolationModeHighQualityCubic).ok()?;
        let stream = CreateStreamOnHGlobal(Default::default(), true).ok()?;
        let encoder = f.CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null()).ok()?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache).ok()?;
        let mut target = None;
        let mut options = None;
        encoder.CreateNewFrame(&mut target, &mut options).ok()?;
        let target = target?;
        if let Some(options) = &options {
            set_quality(options, quality);
        }
        target.Initialize(options.as_ref()).ok()?;
        target.SetSize(width, height).ok()?;
        let mut format = GUID_WICPixelFormat24bppBGR;
        target.SetPixelFormat(&mut format).ok()?;
        let converter = f.CreateFormatConverter().ok()?;
        converter.Initialize(&scaler, &format, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        target.WriteSource(&converter, std::ptr::null()).ok()?;
        target.Commit().ok()?;
        encoder.Commit().ok()?;
        stream_bytes(&stream)
    }
}

fn set_quality(options: &windows::Win32::System::Com::StructuredStorage::IPropertyBag2, quality: f32) {
    use windows::Win32::System::{
        Com::StructuredStorage::PROPBAG2,
        Variant::{VARIANT, VT_R4},
    };
    let mut name: Vec<u16> = "ImageQuality".encode_utf16().chain([0]).collect();
    let bag = PROPBAG2 { pstrName: windows::core::PWSTR(name.as_mut_ptr()), ..Default::default() };
    let mut value = VARIANT::default();
    unsafe {
        let inner = &mut *value.Anonymous.Anonymous;
        inner.vt = VT_R4;
        inner.Anonymous.fltVal = quality.clamp(0.0, 1.0);
        let _ = options.Write(1, &bag, &value);
    }
}

/// Everything written to a stream, from its start.
pub fn stream_bytes(stream: &IStream) -> Option<Vec<u8>> {
    unsafe {
        let mut stat = Default::default();
        stream.Stat(&mut stat, STATFLAG_NONAME).ok()?;
        let len = stat.cbSize as usize;
        stream.Seek(0, STREAM_SEEK_SET, None).ok()?;
        let mut data = vec![0u8; len];
        let mut read = 0u32;
        let hr = stream.Read(data.as_mut_ptr() as *mut _, len as u32, Some(&mut read));
        if hr.is_err() {
            return None;
        }
        data.truncate(read as usize);
        Some(data)
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/image.rs"]
#[rustfmt::skip]
mod tests;
