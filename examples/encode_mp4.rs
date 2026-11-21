//! Turns a recorded timeline of PNG frames into an H.264 MP4 (Media
//! Foundation's sink writer) or an animated GIF (WIC's GIF encoder). Both
//! ship with Windows: nothing to install.
//!
//! The timeline is a text file, one frame per line: `<ms>\t<png>`, where
//! `<ms>` is when the frame was captured on the video's clock and `<png>` is
//! relative to the timeline's folder. Captures come at whatever rate
//! PrintWindow manages, so the output resamples them to a fixed rate: each
//! output frame shows the last capture taken at or before its time. A slow
//! stretch repeats frames and the motion keeps its real speed.
//!
//! ```text
//! cargo run --release --example encode_mp4 -- frames.txt out.mp4 [--fps 30] [--quality 70]
//! cargo run --release --example encode_mp4 -- frames.txt out.gif --gif [--fps 12] [--width 640]
//!     [--from <ms>] [--to <ms>]
//! ```

use std::{
    mem::ManuallyDrop,
    path::{Path, PathBuf},
};

use windows::{
    Win32::{
        Graphics::Imaging::{
            CLSID_WICImagingFactory, GUID_ContainerFormatGif, GUID_WICPixelFormat8bppIndexed,
            GUID_WICPixelFormat32bppBGRA, IWICBitmapEncoder, IWICBitmapFrameEncode,
            IWICBitmapSource, IWICImagingFactory, IWICMetadataQueryWriter, WICBitmapDitherTypeNone,
            WICBitmapEncoderNoCache, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
            WICDecodeMetadataCacheOnDemand, WICRect,
        },
        Media::MediaFoundation::{
            CODECAPI_AVEncCommonQuality, CODECAPI_AVEncCommonRateControlMode,
            CODECAPI_AVEncMPVGOPSize, IMFAttributes, IMFSinkWriter, MF_MT_AVG_BITRATE,
            MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
            MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
            MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_YUV_MATRIX, MF_SINK_WRITER_DISABLE_THROTTLING,
            MF_VERSION, MFCreateAttributes, MFCreateMediaType, MFCreateMemoryBuffer,
            MFCreateSample, MFCreateSinkWriterFromURL, MFMediaType_Video, MFNominalRange_16_235,
            MFSTARTUP_FULL, MFShutdown, MFStartup, MFVideoFormat_H264, MFVideoFormat_NV12,
            MFVideoInterlace_Progressive, MFVideoTransferMatrix_BT709,
            eAVEncCommonRateControlMode_Quality, eAVEncH264VProfile_High,
        },
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                StructuredStorage::{
                    CAUB, PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
                },
            },
            Variant::{VARENUM, VT_UI1, VT_VECTOR},
        },
    },
    core::{GUID, HSTRING, Interface, Result},
};

struct Options {
    timeline: PathBuf,
    out: PathBuf,
    gif: bool,
    fps: u32,
    quality: u32,
    width: Option<u32>,
    from: u64,
    to: Option<u64>,
}

fn parse_args() -> std::result::Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let mut positional = Vec::new();
    let (mut gif, mut fps, mut quality, mut width, mut from, mut to) =
        (false, None, 70, None, 0, None);
    while let Some(arg) = args.next() {
        let mut number = |name: &str| -> std::result::Result<u64, String> {
            args.next()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| format!("{name} necesita un número"))
        };
        match arg.as_str() {
            "--gif" => gif = true,
            "--fps" => fps = Some(number("--fps")? as u32),
            "--quality" => quality = number("--quality")? as u32,
            "--width" => width = Some(number("--width")? as u32),
            "--from" => from = number("--from")?,
            "--to" => to = Some(number("--to")?),
            _ => positional.push(PathBuf::from(arg)),
        }
    }
    let [timeline, out] = <[PathBuf; 2]>::try_from(positional).map_err(|_| {
        "uso: encode_mp4 <timeline.txt> <salida.mp4|salida.gif> [--gif] [--fps N] ...".to_string()
    })?;
    let fps = fps.unwrap_or(if gif { 12 } else { 30 }).clamp(1, 60);
    Ok(Options {
        timeline,
        out,
        gif,
        fps,
        quality: quality.min(100),
        width,
        from,
        to,
    })
}

/// (capture time in ms, PNG path), sorted by time.
fn read_timeline(path: &Path) -> std::result::Result<Vec<(u64, PathBuf)>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("no se pudo leer {}: {e}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut frames: Vec<(u64, PathBuf)> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let (ms, file) = l.split_once('\t')?;
            Some((ms.trim().parse().ok()?, dir.join(file.trim())))
        })
        .collect();
    frames.sort_by_key(|(ms, _)| *ms);
    if frames.is_empty() {
        return Err(format!("{} no tiene cuadros", path.display()));
    }
    Ok(frames)
}

/// For each output frame of a clip [from, to) at `fps`, the index of the
/// capture it shows: the last one taken at or before its time.
fn resample(times: &[u64], from: u64, to: u64, fps: u32) -> Vec<usize> {
    let count = ((to.saturating_sub(from)) * fps as u64)
        .div_ceil(1000)
        .max(1);
    let mut picks = Vec::with_capacity(count as usize);
    let mut i = 0;
    for n in 0..count {
        let t = from + n * 1000 / fps as u64;
        while i + 1 < times.len() && times[i + 1] <= t {
            i += 1;
        }
        picks.push(i);
    }
    picks
}

struct Image {
    width: u32,
    height: u32,
    bgra: Vec<u8>,
}

struct Wic {
    factory: IWICImagingFactory,
}

impl Wic {
    fn new() -> Result<Self> {
        Ok(Self {
            factory: unsafe {
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?
            },
        })
    }

    /// A PNG as straight BGRA, optionally scaled to `width` (aspect kept).
    fn load(&self, path: &Path, width: Option<u32>) -> Result<Image> {
        unsafe {
            let decoder = self.factory.CreateDecoderFromFilename(
                &HSTRING::from(path.as_os_str()),
                None,
                windows::Win32::Foundation::GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )?;
            let frame = decoder.GetFrame(0)?;
            let converter = self.factory.CreateFormatConverter()?;
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat32bppBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )?;
            let (mut w, mut h) = (0, 0);
            converter.GetSize(&mut w, &mut h)?;
            let mut source: IWICBitmapSource = converter.cast()?;
            if let Some(target) = width.filter(|t| *t != w) {
                let th = ((h as u64 * target as u64) / w as u64).max(1) as u32;
                let scaler = self.factory.CreateBitmapScaler()?;
                scaler.Initialize(&source, target, th, WICBitmapInterpolationModeFant)?;
                source = scaler.cast()?;
                (w, h) = (target, th);
            }
            let mut bgra = vec![0u8; (w * h * 4) as usize];
            source.CopyPixels(std::ptr::null(), w * 4, &mut bgra)?;
            Ok(Image {
                width: w,
                height: h,
                bgra,
            })
        }
    }
}

/// Alpha-composites over black (captures are opaque but for keyed-out
/// corners) and converts to NV12, BT.709 limited range, at `w`×`h`
/// (cropped or padded from the top-left).
fn to_nv12(image: &Image, w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let mut out = vec![0u8; w * h * 3 / 2];
    let pixel = |x: usize, y: usize| -> (f32, f32, f32) {
        if x >= image.width as usize || y >= image.height as usize {
            return (0.0, 0.0, 0.0);
        }
        let i = (y * image.width as usize + x) * 4;
        let a = image.bgra[i + 3] as f32 / 255.0;
        (
            image.bgra[i + 2] as f32 * a,
            image.bgra[i + 1] as f32 * a,
            image.bgra[i] as f32 * a,
        )
    };
    for y in 0..h {
        for x in 0..w {
            let (r, g, b) = pixel(x, y);
            out[y * w + x] = (16.0 + 0.1826 * r + 0.6142 * g + 0.0620 * b)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    }
    let uv = w * h;
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let (mut r, mut g, mut b) = (0.0, 0.0, 0.0);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = pixel(x + dx, y + dy);
                r += p.0 / 4.0;
                g += p.1 / 4.0;
                b += p.2 / 4.0;
            }
            let i = uv + (y / 2) * w + x;
            out[i] = (128.0 - 0.1006 * r - 0.3386 * g + 0.4392 * b)
                .round()
                .clamp(0.0, 255.0) as u8;
            out[i + 1] = (128.0 + 0.4392 * r - 0.3989 * g - 0.0403 * b)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    }
    out
}

fn pack(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

fn encode_mp4(options: &Options, frames: &[(u64, PathBuf)], picks: &[usize]) -> Result<()> {
    let wic = Wic::new()?;
    let first = wic.load(&frames[picks[0]].1, options.width)?;
    // H.264 wants even dimensions.
    let (w, h) = (first.width & !1, first.height & !1);
    let fps = options.fps;
    let duration = 10_000_000 / fps as i64;
    unsafe {
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
        let mut attributes: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attributes, 1)?;
        let attributes = attributes.expect("MFCreateAttributes returned no attributes");
        attributes.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
        let _ = std::fs::remove_file(&options.out);
        let writer: IMFSinkWriter =
            MFCreateSinkWriterFromURL(&HSTRING::from(options.out.as_os_str()), None, &attributes)?;

        let output = MFCreateMediaType()?;
        output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        // A ceiling for the container; the encoder runs on quality below.
        output.SetUINT32(&MF_MT_AVG_BITRATE, 4_000_000)?;
        output.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
        output.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))?;
        output.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
        output.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        output.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        output.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        let stream = writer.AddStream(&output)?;

        let input = MFCreateMediaType()?;
        input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        input.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))?;
        input.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
        input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        input.SetUINT32(&MF_MT_DEFAULT_STRIDE, w)?;
        input.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        input.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        // Quality-based VBR: a still screen costs next to nothing and a
        // moving one gets what it needs, instead of a constant bitrate.
        let mut params: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut params, 3)?;
        let params = params.expect("MFCreateAttributes returned no attributes");
        params.SetUINT32(
            &CODECAPI_AVEncCommonRateControlMode,
            eAVEncCommonRateControlMode_Quality.0 as u32,
        )?;
        params.SetUINT32(&CODECAPI_AVEncCommonQuality, options.quality)?;
        // A keyframe every two seconds: without it the encoder writes almost
        // none, and a player seeking into the clip shows a stale picture.
        params.SetUINT32(&CODECAPI_AVEncMPVGOPSize, options.fps * 2)?;
        writer.SetInputMediaType(stream, &input, &params)?;
        writer.BeginWriting()?;

        let size = (w * h * 3 / 2) as usize;
        let mut cached: Option<(usize, Vec<u8>)> = None;
        for (n, &pick) in picks.iter().enumerate() {
            if cached.as_ref().is_none_or(|(i, _)| *i != pick) {
                let image = wic.load(&frames[pick].1, options.width)?;
                cached = Some((pick, to_nv12(&image, w, h)));
            }
            let nv12 = &cached.as_ref().expect("frame cached above").1;
            let buffer = MFCreateMemoryBuffer(size as u32)?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            std::ptr::copy_nonoverlapping(nv12.as_ptr(), data, size);
            buffer.Unlock()?;
            buffer.SetCurrentLength(size as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(n as i64 * duration)?;
            sample.SetSampleDuration(duration)?;
            writer.WriteSample(stream, &sample)?;
            if n % 300 == 0 {
                eprintln!("  {n}/{} cuadros", picks.len());
            }
        }
        writer.Finalize()?;
        MFShutdown()?;
    }
    Ok(())
}

fn propvariant(vt: VARENUM, value: PROPVARIANT_0_0_0) -> PROPVARIANT {
    PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: value,
            }),
        },
    }
}

/// A byte vector property borrowing `bytes`, which must outlive the call
/// that reads it. Never dropped: PROPVARIANT's drop would hand the borrowed
/// bytes to CoTaskMemFree.
fn bytes_property(bytes: &mut [u8]) -> ManuallyDrop<PROPVARIANT> {
    ManuallyDrop::new(propvariant(
        VARENUM(VT_VECTOR.0 | VT_UI1.0),
        PROPVARIANT_0_0_0 {
            caub: CAUB {
                cElems: bytes.len() as u32,
                pElems: bytes.as_mut_ptr(),
            },
        },
    ))
}

/// Animated GIF: one frame per run of identical output frames, its delay
/// stretched to cover the run, each with its own 256-colour palette.
fn encode_gif(options: &Options, frames: &[(u64, PathBuf)], picks: &[usize]) -> Result<()> {
    let wic = Wic::new()?;
    // Runs of the same capture become one GIF frame with a longer delay.
    let mut runs: Vec<(usize, u32)> = Vec::new();
    for &pick in picks {
        match runs.last_mut() {
            Some((p, n)) if *p == pick => *n += 1,
            _ => runs.push((pick, 1)),
        }
    }
    unsafe {
        let _ = std::fs::remove_file(&options.out);
        let stream = wic.factory.CreateStream()?;
        stream.InitializeFromFilename(
            &HSTRING::from(options.out.as_os_str()),
            windows::Win32::Foundation::GENERIC_WRITE.0,
        )?;
        let encoder: IWICBitmapEncoder = wic
            .factory
            .CreateEncoder(&GUID_ContainerFormatGif, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        // Loop forever (the NETSCAPE2.0 application extension).
        let meta: IWICMetadataQueryWriter = encoder.GetMetadataQueryWriter()?;
        let mut app = *b"NETSCAPE2.0";
        meta.SetMetadataByName(
            &HSTRING::from("/appext/application"),
            &*bytes_property(&mut app),
        )?;
        let mut data = [3u8, 1, 0, 0, 0];
        meta.SetMetadataByName(&HSTRING::from("/appext/data"), &*bytes_property(&mut data))?;

        // Rounding each delay to centiseconds would drift: the error is
        // carried to the next frame.
        let mut owed = 0.0f64;
        for (i, (pick, count)) in runs.iter().enumerate() {
            let image = wic.load(&frames[*pick].1, options.width)?;
            let bitmap = wic.factory.CreateBitmapFromMemory(
                image.width,
                image.height,
                &GUID_WICPixelFormat32bppBGRA,
                image.width * 4,
                &image.bgra,
            )?;
            let palette = wic.factory.CreatePalette()?;
            palette.InitializeFromBitmap(&bitmap, 256, false)?;
            let indexed = wic.factory.CreateFormatConverter()?;
            indexed.Initialize(
                &bitmap,
                &GUID_WICPixelFormat8bppIndexed,
                WICBitmapDitherTypeNone,
                &palette,
                0.0,
                WICBitmapPaletteTypeCustom,
            )?;

            let mut frame: Option<IWICBitmapFrameEncode> = None;
            encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
            let frame = frame.expect("CreateNewFrame returned no frame");
            frame.Initialize(None)?;
            frame.SetSize(image.width, image.height)?;
            let mut format: GUID = GUID_WICPixelFormat8bppIndexed;
            frame.SetPixelFormat(&mut format)?;
            frame.SetPalette(&palette)?;
            let exact = *count as f64 * 100.0 / options.fps as f64 + owed;
            let delay = exact.round().max(2.0);
            owed = exact - delay;
            let frame_meta = frame.GetMetadataQueryWriter()?;
            frame_meta.SetMetadataByName(
                &HSTRING::from("/grctlext/Delay"),
                &PROPVARIANT::from(delay as u16),
            )?;
            let rect = WICRect {
                X: 0,
                Y: 0,
                Width: image.width as i32,
                Height: image.height as i32,
            };
            frame.WriteSource(&indexed, &rect)?;
            frame.Commit()?;
            if i % 50 == 0 {
                eprintln!("  {i}/{} cuadros GIF", runs.len());
            }
        }
        encoder.Commit()?;
        stream.Commit(windows::Win32::System::Com::STGC_DEFAULT)?;
    }
    Ok(())
}

fn main() {
    let options = match parse_args() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let frames = match read_timeline(&options.timeline) {
        Ok(frames) => frames,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };
    // The clip, clamped to what was captured; the last frame holds for one
    // frame's time.
    let start = frames
        .iter()
        .rposition(|(ms, _)| *ms <= options.from)
        .unwrap_or(0);
    let last_ms = frames.last().map_or(0, |(ms, _)| *ms) + 1000 / options.fps as u64;
    let to = options.to.unwrap_or(last_ms).min(last_ms);
    let clip = &frames[start..];
    let times: Vec<u64> = clip.iter().map(|(ms, _)| *ms).collect();
    let picks = resample(&times, options.from.max(times[0]), to, options.fps);
    eprintln!(
        "{} capturas -> {} cuadros a {} fps ({:.1} s)",
        clip.len(),
        picks.len(),
        options.fps,
        picks.len() as f64 / options.fps as f64
    );
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let result = if options.gif {
        encode_gif(&options, clip, &picks)
    } else {
        encode_mp4(&options, clip, &picks)
    };
    match result {
        Ok(()) => {
            let bytes = std::fs::metadata(&options.out).map_or(0, |m| m.len());
            println!(
                "{} ({:.1} MB)",
                options.out.display(),
                bytes as f64 / 1_048_576.0
            );
        }
        Err(e) => {
            eprintln!("no se pudo codificar: {e}");
            std::process::exit(1);
        }
    }
}

