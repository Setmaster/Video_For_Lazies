//! Native Windows recording host. No media API is linked by the editor executable.
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Display {
    pub id: String,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub kind: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Region {
    pub display_id: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub region: Region,
    pub fps: u32,
    pub show_cursor: bool,
    pub system_audio_device_id: Option<String>,
    pub microphone_device_id: Option<String>,
    pub quality: String,
    pub spool_path: PathBuf,
    #[serde(default)]
    pub ffmpeg_path: Option<PathBuf>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub ready: bool,
    pub elapsed_ms: u64,
    pub bytes_written: u64,
    pub encoder: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub elapsed_ms: u64,
    pub bytes_written: u64,
    pub encoder: String,
    pub video_frames: u64,
    pub distinct_frames: u64,
    pub max_video_lag_ms: u64,
    pub max_steady_video_lag_ms: u64,
}

pub struct RunError {
    pub message: String,
    /// True only while no media sample has been submitted to the sink writer.
    pub no_media: bool,
}

#[path = "windows_audio.rs"]
mod audio;
#[path = "windows_ffmpeg.rs"]
mod ffmpeg;
#[path = "windows_gpu.rs"]
mod gpu;
#[path = "windows_ui.rs"]
mod ui;
pub use ui::{enumerate_displays, select_region};

pub fn supports_capture() -> bool {
    let Ok(_com) = ComApartment::new() else {
        return false;
    };
    ::windows::Graphics::Capture::GraphicsCaptureSession::IsSupported().unwrap_or(false)
}
pub fn audio_devices() -> Result<Vec<AudioDevice>, String> {
    let _com = ComApartment::new()?;
    audio::audio_devices()
}

use ::windows::{
    Foundation::TypedEventHandler,
    Graphics::{
        Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession},
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
    },
    Win32::{
        Devices::Display::*,
        Foundation::{HANDLE, HMODULE, POINT},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            Direct3D11::*,
            Dxgi::{Common::*, CreateDXGIFactory1, IDXGIDevice, IDXGIFactory1, IDXGIOutput6},
            Gdi::{HMONITOR, MONITOR_DEFAULTTONULL, MonitorFromPoint},
        },
        Media::MediaFoundation::*,
        System::{
            Com::CoTaskMemFree,
            Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
            StationsAndDesktops::{
                CloseDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS,
                GetUserObjectInformationW, OpenInputDesktop, UOI_NAME,
            },
            WinRT::{
                Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
                Graphics::Capture::IGraphicsCaptureItemInterop,
                RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize,
            },
        },
        UI::HiDpi::{
            DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            SetThreadDpiAwarenessContext,
        },
    },
    core::{HSTRING, Interface, PCWSTR, PWSTR},
};
use std::{
    ptr,
    time::{Duration, Instant},
};

struct ComApartment;
impl ComApartment {
    fn new() -> Result<Self, String> {
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED)
                .map_err(|e| format!("Windows runtime initialization: {e}"))?
        };
        Ok(Self)
    }
}
impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() }
    }
}
struct MediaRuntime;
impl MediaRuntime {
    fn new() -> ::windows::core::Result<Self> {
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL)? };
        Ok(Self)
    }
}
impl Drop for MediaRuntime {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
        }
    }
}
struct PhysicalDpi(DPI_AWARENESS_CONTEXT);
impl PhysicalDpi {
    fn new() -> Self {
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}
impl Drop for PhysicalDpi {
    fn drop(&mut self) {
        unsafe {
            if !self.0.0.is_null() {
                SetThreadDpiAwarenessContext(self.0);
            }
        }
    }
}

fn qpc100ns() -> ::windows::core::Result<i64> {
    let (mut tick, mut freq) = (0, 0);
    unsafe {
        QueryPerformanceCounter(&mut tick)?;
        QueryPerformanceFrequency(&mut freq)?
    };
    Ok(((tick as i128) * 10_000_000 / (freq as i128)) as i64)
}

fn capture_buffer_bytes(display: &Display, region: &Region, hdr: bool) -> u64 {
    // Two WGC textures, crop SRV, GPU/staging NV12 and up to three CPU
    // NV12 frames (latest/pending/pipe worker), plus padding/LUT allowance.
    // Driver and codec internals are separate from these controlled buffers.
    let display_pixels = u64::from(display.width).saturating_mul(u64::from(display.height));
    let crop_pixels =
        u64::from((region.width + 1) & !1).saturating_mul(u64::from((region.height + 1) & !1));
    display_pixels
        .saturating_mul(if hdr { 16 } else { 8 })
        .saturating_add(crop_pixels.saturating_mul(if hdr { 16 } else { 12 }))
}
fn interactive_desktop() -> Result<(), String> {
    unsafe {
        let desk = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS).map_err(
            |_| "Recording stopped: the interactive desktop is unavailable or locked".to_string(),
        )?;
        let mut name = [0u16; 128];
        let mut needed = 0;
        let read = GetUserObjectInformationW(
            HANDLE(desk.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            (name.len() * 2) as u32,
            Some(&mut needed),
        );
        let _ = CloseDesktop(desk);
        read.map_err(|e| format!("Desktop state: {e}"))?;
        let text = String::from_utf16_lossy(
            &name[..name.iter().position(|x| *x == 0).unwrap_or(name.len())],
        );
        if !text.eq_ignore_ascii_case("default") {
            return Err("Recording stopped: secure or nondefault desktop".into());
        };
        Ok(())
    }
}

fn same_color_state(
    before: DXGI_COLOR_SPACE_TYPE,
    before_white: f32,
    after: DXGI_COLOR_SPACE_TYPE,
    after_white: f32,
) -> bool {
    before == after && (before_white - after_white).abs() < 0.0001
}

fn monitor_color(
    monitor: HMONITOR,
    display_id: &str,
) -> ::windows::core::Result<(DXGI_COLOR_SPACE_TYPE, f32)> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut ai = 0;
        while let Ok(adapter) = factory.EnumAdapters1(ai) {
            let mut oi = 0;
            while let Ok(output) = adapter.EnumOutputs(oi) {
                if output.GetDesc()?.Monitor == monitor {
                    let hdr: IDXGIOutput6 = output.cast()?;
                    let space = hdr.GetDesc1()?.ColorSpace;
                    return match space {
                        DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709 => Ok((space, 1.0)),
                        DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020
                        | DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709 => {
                            Ok((space, sdr_white_scale(display_id)?))
                        }
                        _ => Err(::windows::core::Error::new(
                            ::windows::core::HRESULT(0x80004001u32 as i32),
                            "This display color space is unsupported for recording",
                        )),
                    };
                }
                oi += 1;
            }
            ai += 1;
        }
        Err(::windows::core::Error::new(
            ::windows::core::HRESULT(0x80004005u32 as i32),
            "Cannot verify selected display color space",
        ))
    }
}

fn video_bitrate(width: u32, height: u32, fps: u32, high: bool) -> u32 {
    let raw =
        u64::from(width) * u64::from(height) * u64::from(fps) * if high { 15 } else { 8 } / 100;
    raw.clamp(
        if high { 2_000_000 } else { 1_000_000 },
        if high { 160_000_000 } else { 80_000_000 },
    ) as u32
}

struct Capture {
    device: ID3D11Device,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    item: GraphicsCaptureItem,
    closed: Arc<AtomicBool>,
    closed_token: i64,
    converter: gpu::Converter,
    crop: D3D11_BOX,
    monitor_width: i32,
    monitor_height: i32,
    white: f32,
    monitor: HMONITOR,
    display_id: String,
    color_space: DXGI_COLOR_SPACE_TYPE,
}
impl Capture {
    fn new(display: &Display, region: &Region, cursor: bool) -> ::windows::core::Result<Self> {
        unsafe {
            let monitor = MonitorFromPoint(
                POINT {
                    x: display.x + 1,
                    y: display.y + 1,
                },
                MONITOR_DEFAULTTONULL,
            );
            if monitor.0.is_null() {
                return Err(::windows::core::Error::new(
                    ::windows::core::HRESULT(0x80070057u32 as i32),
                    "Selected display is no longer available",
                ));
            }
            // FP16 scRGB retains HDR values until the explicit SDR tone map below.
            let (color_space, white) = monitor_color(monitor, &display.id)?;
            let hdr = color_space != DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709;
            if capture_buffer_bytes(display, region, hdr) > 512 * 1024 * 1024 {
                return Err(::windows::core::Error::new(
                    ::windows::core::HRESULT(0x8007000eu32 as i32),
                    "This recording area requires more than 512 MiB of capture buffers; choose a smaller area or display",
                ));
            }
            let interop: IGraphicsCaptureItemInterop =
                ::windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
            let item: GraphicsCaptureItem = interop.CreateForMonitor(monitor)?;
            let size = item.Size()?;
            if size.Width != display.width as i32 || size.Height != display.height as i32 {
                return Err(::windows::core::Error::new(
                    ::windows::core::HRESULT(0x80070057u32 as i32),
                    "Display geometry changed; select it again",
                ));
            }
            let (mut device, mut context) = (None, None);
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device = device.unwrap();
            let context = context.unwrap();
            let multithread: ID3D11Multithread = context.cast()?;
            let _ = multithread.SetMultithreadProtected(true);
            let dxgi: IDXGIDevice = device.cast()?;
            let direct: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;
            let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
                &direct,
                if hdr {
                    DirectXPixelFormat::R16G16B16A16Float
                } else {
                    DirectXPixelFormat::B8G8R8A8UIntNormalized
                },
                2,
                size,
            )?;
            let session = pool.CreateCaptureSession(&item)?;
            session.SetIsCursorCaptureEnabled(cursor)?;
            let closed = Arc::new(AtomicBool::new(false));
            let signal = closed.clone();
            let closed_token = item.Closed(&TypedEventHandler::new(move |_, _| {
                signal.store(true, Ordering::Release);
                Ok(())
            }))?;
            let converter =
                gpu::Converter::new(&device, &context, region.width, region.height, hdr, white)
                    .map_err(|message| {
                        ::windows::core::Error::new(
                            ::windows::core::HRESULT(0x80004005u32 as i32),
                            message,
                        )
                    })?;
            let left = (region.x - display.x) as u32;
            let top = (region.y - display.y) as u32;
            Ok(Self {
                device,
                pool,
                session,
                item,
                closed,
                closed_token,
                converter,
                crop: D3D11_BOX {
                    left,
                    top,
                    front: 0,
                    right: left + region.width,
                    bottom: top + region.height,
                    back: 1,
                },
                monitor_width: size.Width,
                monitor_height: size.Height,
                white,
                monitor,
                display_id: display.id.clone(),
                color_space,
            })
        }
    }
    fn verify_color(&self) -> Result<(), String> {
        let (space, white) = monitor_color(self.monitor, &self.display_id)
            .map_err(|e| format!("Display color state: {e}"))?;
        let initial_white = self.white;
        if !same_color_state(self.color_space, initial_white, space, white) {
            return Err(
                "Display HDR mode or SDR white level changed; the partial recording was retained"
                    .into(),
            );
        }
        Ok(())
    }
    fn frame(&self) -> Result<Option<(i64, Vec<u8>)>, String> {
        if self.closed.load(Ordering::Acquire) {
            return Err("Recording display was closed or disconnected".into());
        }
        unsafe {
            self.device
                .GetDeviceRemovedReason()
                .map_err(|e| format!("Graphics device lost: {e}"))?;
        }
        // The pool owns at most two frames. All immediate-context work stays on this thread.
        let frame = unsafe {
            let mut raw = ptr::null_mut();
            (::windows::core::Interface::vtable(&self.pool).TryGetNextFrame)(
                self.pool.as_raw(),
                &mut raw,
            )
            .ok()
            .map_err(|e| format!("Capture frame pool: {e}"))?;
            if raw.is_null() {
                return Ok(None);
            };
            ::windows::Graphics::Capture::Direct3D11CaptureFrame::from_raw(raw)
        };
        let result = (|| -> ::windows::core::Result<(i64, Vec<u8>)> {
            let size = frame.ContentSize()?;
            if size.Width != self.monitor_width || size.Height != self.monitor_height {
                return Err(::windows::core::Error::new(
                    ::windows::core::HRESULT(0x80070057u32 as i32),
                    "Display size changed during recording",
                ));
            }
            let timestamp = frame.SystemRelativeTime()?.Duration;
            let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
            unsafe {
                let texture: ID3D11Texture2D = access.GetInterface()?;
                let nv12 = self
                    .converter
                    .convert(&texture, &self.crop)
                    .map_err(|message| {
                        ::windows::core::Error::new(
                            ::windows::core::HRESULT(0x80004005u32 as i32),
                            message,
                        )
                    })?;
                frame.Close()?;
                Ok((timestamp, nv12))
            }
        })();
        let _ = frame.Close();
        let value = result.map_err(|e| format!("Capture frame: {e}"))?;
        Ok(Some(value))
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
        let _ = self.item.RemoveClosed(self.closed_token);
    }
}

/// BT.709 limited-range NV12. Odd right/bottom edges are black-padded, never sampled outside the selected area.
#[cfg(test)]
fn bgra_to_nv12(src: &[u8], width: u32, height: u32) -> Vec<u8> {
    rgb_to_nv12(width, height, |x, y| {
        let i = ((y * width + x) * 4) as usize;
        [src[i + 2], src[i + 1], src[i]]
    })
}

#[cfg(test)]
fn rgb_to_nv12(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
    let w = (width + 1) & !1;
    let h = (height + 1) & !1;
    let mut out = vec![0u8; (w * h * 3 / 2) as usize];
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let (mut r, mut g, mut b) = (0, 0, 0);
            for dy in 0..2 {
                for dx in 0..2 {
                    let [p, q, t] = if x + dx < width && y + dy < height {
                        pixel(x + dx, y + dy).map(i32::from)
                    } else {
                        [0; 3]
                    };
                    out[((y + dy) * w + x + dx) as usize] =
                        (((47 * p + 157 * q + 16 * t + 128) >> 8) + 16).clamp(16, 235) as u8;
                    r += p;
                    g += q;
                    b += t;
                }
            }
            r /= 4;
            g /= 4;
            b /= 4;
            let uv = (w * h + (y / 2) * w + x) as usize;
            out[uv] = (((-26 * r - 86 * g + 112 * b + 128) >> 8) + 128).clamp(16, 240) as u8;
            out[uv + 1] = (((112 * r - 102 * g - 10 * b + 128) >> 8) + 128).clamp(16, 240) as u8;
        }
    }
    out
}

// scRGB uses linear BT.709 primaries and 1.0 = 80 nits. Windows scales
// ordinary SDR windows by SDRWhiteLevel/1000 before HDR composition.
fn sdr_white_scale(display_id: &str) -> ::windows::core::Result<f32> {
    unsafe {
        for _ in 0..3 {
            let (mut path_count, mut mode_count) = (0, 0);
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
                .ok()?;
            if path_count > 1024 || mode_count > 4096 {
                break;
            }
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
            if QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            )
            .is_err()
            {
                continue;
            }
            for path in &paths[..path_count as usize] {
                let mut name = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
                name.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: std::mem::size_of_val(&name) as u32,
                    adapterId: path.sourceInfo.adapterId,
                    id: path.sourceInfo.id,
                };
                if DisplayConfigGetDeviceInfo(&mut name.header) != 0 {
                    continue;
                }
                let length = name
                    .viewGdiDeviceName
                    .iter()
                    .position(|x| *x == 0)
                    .unwrap_or(32);
                if !String::from_utf16_lossy(&name.viewGdiDeviceName[..length])
                    .eq_ignore_ascii_case(display_id)
                {
                    continue;
                }
                let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL::default();
                white.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
                    size: std::mem::size_of_val(&white) as u32,
                    adapterId: path.targetInfo.adapterId,
                    id: path.targetInfo.id,
                };
                if DisplayConfigGetDeviceInfo(&mut white.header) == 0 && white.SDRWhiteLevel > 0 {
                    return Ok(white.SDRWhiteLevel as f32 / 1000.0);
                }
            }
        }
        Err(::windows::core::Error::new(
            ::windows::core::HRESULT(0x80004005u32 as i32),
            "Cannot read HDR display SDR white level; recording was not started",
        ))
    }
}

#[cfg(test)]
fn half_to_float(bits: u16) -> f32 {
    let sign = u32::from(bits & 0x8000) << 16;
    let exponent = (bits >> 10) & 31;
    let mantissa = u32::from(bits & 1023);
    match exponent {
        0 => (if sign == 0 { 1.0 } else { -1.0 }) * mantissa as f32 / 16_777_216.0,
        31 => 0.0, // Unreliable nonfinite source components never enter the encoder.
        _ => f32::from_bits(sign | ((u32::from(exponent) + 112) << 23) | (mantissa << 13)),
    }
}

#[cfg(test)]
fn bt709_transfer(linear: f32) -> u8 {
    let encoded = if linear < 0.018 {
        4.5 * linear
    } else {
        1.099 * linear.powf(0.45) - 0.099
    };
    (encoded * 255.0).round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
struct ToneMapper {
    white: f32,
    transfer: [u8; 4097],
}
#[cfg(test)]
impl ToneMapper {
    fn new(white: f32) -> Self {
        let mut transfer = [0; 4097];
        for (i, value) in transfer.iter_mut().enumerate() {
            *value = bt709_transfer(i as f32 / 4096.0);
        }
        Self { white, transfer }
    }

    fn convert(&self, rgb: [f32; 3]) -> [u8; 3] {
        let mut rgb = rgb.map(|v| if v.is_finite() { v / self.white } else { 0.0 });
        let luma = (rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722).max(0.0);
        // A luminance-only Reinhard shoulder begins at 75% reference white.
        // Shadows/midtones retain their linear values; normalized SDR white
        // stays neutral, while HDR highlights retain detail instead of clipping.
        let mapped = if luma > 0.75 {
            let highlight = luma - 0.75;
            0.75 + 0.25 * highlight / (highlight + 0.25)
        } else {
            luma
        };
        if luma > 0.0 {
            rgb = rgb.map(|v| v * (mapped / luma));
        }
        // Compress out-of-gamut chroma toward the mapped neutral axis with a
        // common factor, retaining hue direction instead of clipping channels.
        let mut chroma = 1.0f32;
        for value in rgb {
            if value > 1.0 {
                chroma = chroma.min((1.0 - mapped) / (value - mapped));
            }
            if value < 0.0 {
                chroma = chroma.min(mapped / (mapped - value));
            }
        }
        rgb.map(|v| {
            let linear = (mapped + (v - mapped) * chroma).clamp(0.0, 1.0);
            self.transfer[(linear * 4096.0).round() as usize]
        })
    }
}

struct MfWriter {
    writer: Option<IMFSinkWriter>,
    sink: IMFMediaSink,
    stream: IMFByteStream,
    video: u32,
    audio: Option<u32>,
    encoder: String,
    spool_path: PathBuf,
}
impl MfWriter {
    fn new(config: &Config) -> ::windows::core::Result<Self> {
        unsafe {
            let width = (config.region.width + 1) & !1;
            let height = (config.region.height + 1) & !1;
            let video = MFCreateMediaType()?;
            video.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            video.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            let rate = video_bitrate(width, height, config.fps, config.quality == "high");
            video.SetUINT32(&MF_MT_AVG_BITRATE, rate)?;
            video.SetUINT32(&MF_MT_INTERLACE_MODE, 2)?;
            video.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | height as u64)?;
            video.SetUINT64(&MF_MT_FRAME_RATE, ((config.fps as u64) << 32) | 1)?;
            video.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
            video.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
            video.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
            video.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
            video.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
            video.SetUINT32(&MF_MT_MPEG2_PROFILE, 77)?;
            video.SetUINT32(&MF_MT_MAX_KEYFRAME_SPACING, config.fps * 2)?;
            let has_audio =
                config.system_audio_device_id.is_some() || config.microphone_device_id.is_some();
            let audio = if has_audio {
                let a = MFCreateMediaType()?;
                a.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                a.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
                a.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
                a.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, 48_000)?;
                a.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                a.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 24_000)?;
                Some(a)
            } else {
                None
            };
            let pathname = HSTRING::from(config.spool_path.as_os_str());
            let stream = MFCreateFile(
                MF_ACCESSMODE_READWRITE,
                MF_OPENMODE_FAIL_IF_EXIST,
                MF_FILEFLAGS_NONE,
                PCWSTR(pathname.as_ptr()),
            )?;
            let sink = MFCreateFMPEG4MediaSink(&stream, &video, audio.as_ref())?;
            let mut writer = Self {
                writer: None,
                sink,
                stream,
                video: 0,
                audio: None,
                encoder: String::new(),
                spool_path: config.spool_path.clone(),
            };
            let sink_attrs: IMFAttributes = writer.sink.cast()?;
            sink_attrs.SetUINT64(&MF_MPEG4SINK_MIN_FRAGMENT_DURATION, 20_000_000)?;
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 4)?;
            let attrs = attrs.unwrap();
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            attrs.SetUINT32(&MF_LOW_LATENCY, 1)?;
            let sw = MFCreateSinkWriterFromMediaSink(&writer.sink, &attrs)?;
            // Sink-writer indices are zero based and differ from the MP4
            // media sink's track identifiers (which start at one).
            writer.video = 0;
            let input = MFCreateMediaType()?;
            video.CopyAllItems(&input)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            input.SetUINT32(&MF_MT_DEFAULT_STRIDE, width)?;
            input.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
            input.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
            input.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
            sw.SetInputMediaType(writer.video, &input, None)?;
            if has_audio {
                let index = 1;
                let a = MFCreateMediaType()?;
                a.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                a.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
                a.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
                a.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, 48_000)?;
                a.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                a.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
                a.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 192_000)?;
                a.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
                sw.SetInputMediaType(index, &a, None)?;
                writer.audio = Some(index)
            }
            sw.BeginWriting()?;
            writer.encoder = encoder_description(&sw, writer.video);
            writer.writer = Some(sw);
            Ok(writer)
        }
    }
    fn sample(
        &self,
        index: u32,
        bytes: &[u8],
        pts: i64,
        duration: i64,
    ) -> ::windows::core::Result<()> {
        unsafe {
            let buffer = MFCreateMemoryBuffer(bytes.len() as u32)?;
            let mut target = ptr::null_mut();
            buffer.Lock(&mut target, None, None)?;
            ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(bytes.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts)?;
            sample.SetSampleDuration(duration)?;
            self.writer.as_ref().unwrap().WriteSample(index, &sample)
        }
    }
    fn finish(&mut self) -> Result<(), String> {
        unsafe {
            self.writer
                .as_ref()
                .unwrap()
                .Finalize()
                .map_err(|e| format!("Finalize fragmented MP4: {e}"))?;
            drop(self.writer.take());
            // The sink writer can close its sink/byte stream when released.
            // Repeated MF close/flush calls can then report E_INVALIDARG even
            // after a successful Finalize. Establish durability through our
            // writable file handle after relinquishing the MF writer.
            let _ = self.sink.Shutdown();
            let _ = self.stream.Close();
        }
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.spool_path)
            .and_then(|file| file.sync_all())
            .map_err(|e| format!("Synchronize recording file: {e}"))
    }
}
impl Drop for MfWriter {
    fn drop(&mut self) {
        drop(self.writer.take());
        unsafe {
            let _ = self.sink.Shutdown();
            let _ = self.stream.Flush();
            let _ = self.stream.Close();
        }
    }
}
enum WriterBackend {
    MediaFoundation(MfWriter),
    Ffmpeg(ffmpeg::Writer),
}
struct Writer {
    backend: WriterBackend,
    video: u32,
    audio: Option<u32>,
    encoder: String,
    spool_path: PathBuf,
}
impl Writer {
    fn new(config: &Config) -> Result<Self, String> {
        match MfWriter::new(config) {
            Ok(writer) => Ok(Self {
                video: writer.video,
                audio: writer.audio,
                encoder: writer.encoder.clone(),
                spool_path: config.spool_path.clone(),
                backend: WriterBackend::MediaFoundation(writer),
            }),
            Err(error)
                if error.code() == MF_E_INVALIDMEDIATYPE
                    || error.code() == MF_E_TOPO_CODEC_NOT_FOUND =>
            {
                // No sample has been submitted during negotiation. Remove only
                // this freshly created, owned spool before invoking FFmpeg -n.
                if config.spool_path.exists() {
                    std::fs::remove_file(&config.spool_path)
                        .map_err(|e| format!("Prepare fallback recording spool: {e}"))?;
                }
                let writer = ffmpeg::Writer::new(config)?;
                Ok(Self {
                    backend: WriterBackend::Ffmpeg(writer),
                    video: 0,
                    audio: (config.system_audio_device_id.is_some()
                        || config.microphone_device_id.is_some())
                    .then_some(1),
                    encoder: "FFmpeg libx264 (software)".into(),
                    spool_path: config.spool_path.clone(),
                })
            }
            Err(error) => Err(error.to_string()),
        }
    }
    fn sample(&mut self, index: u32, bytes: &[u8], pts: i64, duration: i64) -> Result<(), String> {
        match &mut self.backend {
            WriterBackend::MediaFoundation(writer) => writer
                .sample(index, bytes, pts, duration)
                .map_err(|e| e.to_string()),
            WriterBackend::Ffmpeg(writer) => writer.sample(index, bytes, pts),
        }
    }
    fn video_sample(&mut self, frame: Arc<Vec<u8>>, pts: i64, duration: i64) -> Result<(), String> {
        match &mut self.backend {
            WriterBackend::MediaFoundation(writer) => writer
                .sample(self.video, &frame, pts, duration)
                .map_err(|e| e.to_string()),
            WriterBackend::Ffmpeg(writer) => writer.video_sample(frame, pts),
        }
    }
    fn finish(&mut self) -> Result<(), String> {
        match &mut self.backend {
            WriterBackend::MediaFoundation(writer) => writer.finish(),
            WriterBackend::Ffmpeg(writer) => {
                writer.finish()?;
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&self.spool_path)
                    .and_then(|file| file.sync_all())
                    .map_err(|e| format!("Synchronize recording file: {e}"))
            }
        }
    }
    fn bytes_written(&self) -> u64 {
        file_size(&self.spool_path)
            + if matches!(self.backend, WriterBackend::Ffmpeg(_)) && self.audio.is_some() {
                file_size(&self.spool_path.with_file_name("capture.pcm"))
            } else {
                0
            }
    }
}

fn encoder_description(writer: &IMFSinkWriter, index: u32) -> String {
    unsafe {
        let found = (|| -> ::windows::core::Result<String> {
            let ex: IMFSinkWriterEx = writer.cast()?;
            for i in 0..8 {
                let mut transform = None;
                let mut category = ::windows::core::GUID::zeroed();
                if ex
                    .GetTransformForStream(index, i, Some(&mut category), &mut transform)
                    .is_err()
                {
                    break;
                };
                if category != MFT_CATEGORY_VIDEO_ENCODER {
                    continue;
                };
                let attrs = transform.unwrap().GetAttributes()?;
                let hw = attrs
                    .GetStringLength(&MFT_ENUM_HARDWARE_URL_Attribute)
                    .is_ok();
                let name = if let Ok(n) = attrs.GetStringLength(&MFT_FRIENDLY_NAME_Attribute) {
                    let mut buf = vec![0u16; n as usize + 1];
                    attrs.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut buf, None)?;
                    String::from_utf16_lossy(&buf[..n as usize])
                } else if let Ok(clsid) = attrs.GetGUID(&MFT_TRANSFORM_CLSID_Attribute) {
                    let mut text = PWSTR::null();
                    let result = MFTGetInfo(clsid, Some(&mut text), None, None, None, None, None);
                    let value = if result.is_ok() && !text.is_null() {
                        text.to_string()
                            .unwrap_or_else(|_| format!("H.264 transform {clsid:?}"))
                    } else {
                        format!("H.264 transform {clsid:?}")
                    };
                    if !text.is_null() {
                        CoTaskMemFree(Some(text.0.cast()));
                    }
                    value
                } else {
                    "H.264 transform".to_string()
                };
                return Ok(format!(
                    "{} ({})",
                    name,
                    if hw {
                        "hardware"
                    } else {
                        "hardware identity unreported"
                    }
                ));
            }
            Ok("Media Foundation H.264 (transform identity unavailable)".into())
        })();
        found.unwrap_or_else(|_| "Media Foundation H.264 (transform identity unavailable)".into())
    }
}

/// Runs only inside the supervised native recorder host, never on the Tauri thread.
/// Stop fixes a single QPC endpoint; errors retain every byte of the owned spool.
pub fn run(
    config: Config,
    stop: Arc<AtomicBool>,
    progress: impl Fn(Progress) + Send + 'static,
) -> Result<Outcome, RunError> {
    let mut no_media = true;
    run_inner(config, stop, progress, &mut no_media)
        .map_err(|message| RunError { message, no_media })
}

fn run_inner(
    config: Config,
    stop: Arc<AtomicBool>,
    progress: impl Fn(Progress) + Send + 'static,
    no_media: &mut bool,
) -> Result<Outcome, String> {
    let _dpi = PhysicalDpi::new();
    let _com = ComApartment::new()?;
    if !supports_capture() {
        return Err("Windows Graphics Capture is unavailable on this Windows session".into());
    }
    interactive_desktop()?;
    if ![24, 25, 30, 50, 60].contains(&config.fps)
        || !matches!(config.quality.as_str(), "standard" | "high")
    {
        return Err("Unsupported recording frame rate or quality".into());
    }
    let display = enumerate_displays()?
        .into_iter()
        .find(|d| d.id == config.region.display_id)
        .ok_or("Selected display is no longer connected")?;
    let r = &config.region;
    if r.width < 16
        || r.height < 16
        || r.width > 7680
        || r.height > 4320
        || r.x < display.x
        || r.y < display.y
        || r.x as i64 + r.width as i64 > display.x as i64 + display.width as i64
        || r.y as i64 + r.height as i64 > display.y as i64 + display.height as i64
    {
        return Err(
            "Recording area must fit one display and be between 16×16 and 7680×4320".into(),
        );
    }
    if !config.spool_path.is_absolute() || config.spool_path.exists() {
        return Err(
            "A fresh absolute spool path is required; existing files are never replaced".into(),
        );
    }
    if stop.load(Ordering::Acquire) {
        return Err("Recording canceled before preparation".into());
    }
    let _mf =
        MediaRuntime::new().map_err(|e| format!("Windows Media Foundation unavailable: {e}"))?;
    let capture = Capture::new(&display, r, config.show_cursor)
        .map_err(|e| format!("Screen capture initialization: {e}"))?;
    let mut writer = Writer::new(&config)
        .map_err(|e| format!("H.264/AAC fragmented MP4 initialization: {e}"))?;
    let mut audio = audio::AudioCapture::start(
        config.system_audio_device_id.as_deref(),
        config.microphone_device_id.as_deref(),
    )?;
    let _hud = ui::StopHud::start(stop.clone())?;
    capture
        .session
        .StartCapture()
        .map_err(|e| format!("Start screen capture: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let (zero, first) = loop {
        if stop.load(Ordering::Acquire) {
            return Err("Recording canceled before its first frame".into());
        };
        audio.poll()?;
        if let Some(frame) = capture.frame()? {
            break frame;
        };
        if Instant::now() > deadline {
            return Err("Windows did not provide a capture frame; no recording was started".into());
        };
        std::thread::sleep(Duration::from_millis(3))
    };
    let mut frames = VideoFrames {
        latest: Arc::new(first),
        latest_time: zero,
        pending: None,
    };
    let mut next_video = 0u64;
    let mut distinct_frames = 0u64;
    let mut last_written_frame = None;
    let mut max_video_lag = 0i64;
    let mut max_steady_video_lag = 0i64;
    let mut cadence = CadenceGuard::new(config.fps);
    let mut audio_frames = 0u64;
    let mut last_report = Instant::now();
    let mut last_health = Instant::now();
    let cutoff = loop {
        let now = qpc100ns().map_err(|e| format!("Recording clock: {e}"))?;
        let elapsed = (now - zero).max(0);
        let pts = (next_video as i128 * 10_000_000 / config.fps as i128) as i64;
        let lag = (elapsed - pts).max(0);
        max_video_lag = max_video_lag.max(lag);
        if elapsed >= 10_000_000 {
            max_steady_video_lag = max_steady_video_lag.max(lag);
        }
        if stop.load(Ordering::Acquire) {
            validate_stop(*no_media, elapsed, next_video, config.fps)?;
            break now;
        }
        // A short scheduler/driver stall can repeat past images without moving
        // their source timestamps. Bound both its size and recovery time;
        // sustained encoder deficit must still stop the recording.
        cadence.check(elapsed, lag)?;
        let slot = zero + pts;
        frames.advance(slot);
        // Keep at most one future frame, and do not allocate another while it
        // is pending. Together with Arc sharing to the pipe worker, this caps
        // retained CPU buffers at three. Never substitute a future image into
        // an earlier CFR slot when a capture/encoder loop is late.
        if frames.pending.is_none()
            && let Some((time, image)) = capture.frame()?
            && time >= zero
            && time <= now + 100_000
        {
            frames.push(time, image, slot);
        }
        let due = (elapsed as u64 * config.fps as u64) / 10_000_000;
        if next_video <= due {
            let pts = (next_video as i128 * 10_000_000 / config.fps as i128) as i64;
            // A failed WriteSample may still have reached the media sink. Preserve its
            // output conservatively from the first attempted submission onward.
            *no_media = false;
            writer
                .video_sample(
                    frames.latest.clone(),
                    pts,
                    ((next_video + 1) as i128 * 10_000_000 / config.fps as i128) as i64 - pts,
                )
                .map_err(|e| format!("Encode video: {e}"))?;
            next_video += 1;
            if last_written_frame != Some(frames.latest_time) {
                distinct_frames += 1;
                last_written_frame = Some(frames.latest_time);
            }
            if next_video == 1 {
                progress(Progress {
                    ready: true,
                    elapsed_ms: 0,
                    bytes_written: writer.bytes_written(),
                    encoder: writer.encoder.clone(),
                });
            }
        }
        audio.poll()?;
        if let Some(index) = writer.audio {
            let available = ((elapsed - 1_000_000).max(0) as u64 * 48_000) / 10_000_000;
            while audio_frames + 480 <= available {
                write_audio(&mut writer, &mut audio, index, zero, &mut audio_frames, 480)?;
            }
        }
        if last_report.elapsed() >= Duration::from_millis(250) {
            progress(Progress {
                ready: true,
                elapsed_ms: (elapsed / 10_000) as u64,
                bytes_written: writer.bytes_written(),
                encoder: writer.encoder.clone(),
            });
            last_report = Instant::now()
        }
        if last_health.elapsed() >= Duration::from_millis(500) {
            interactive_desktop()?;
            capture.verify_color()?;
            let current = enumerate_displays()?
                .into_iter()
                .find(|d| d.id == display.id);
            if !current.is_some_and(|d| {
                d.x == display.x
                    && d.y == display.y
                    && d.width == display.width
                    && d.height == display.height
            }) {
                return Err("Display topology changed; the partial recording was retained".into());
            };
            last_health = Instant::now()
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    capture
        .session
        .Close()
        .map_err(|e| format!("Stop capture: {e}"))?;
    // Shared-mode packets are delivered after their sampling interval. Drain the bounded tail.
    std::thread::sleep(Duration::from_millis(100));
    audio.poll()?;
    audio.stop()?;
    // Stop fixes one common cutoff. Do not acquire any post-cutoff image.
    // Fill still-missing CFR slots with the last known image at or before
    // each slot, including a previously queued future image only when due.
    // The final CFR frame ends less than one frame period after the cutoff;
    // PCM below ends at the cutoff within one 48 kHz sample.
    while video_time(next_video, config.fps) < cutoff - zero {
        let pts = video_time(next_video, config.fps);
        frames.advance(zero + pts);
        writer
            .video_sample(
                frames.latest.clone(),
                pts,
                video_time(next_video + 1, config.fps) - pts,
            )
            .map_err(|e| format!("Encode video Stop tail: {e}"))?;
        next_video += 1;
        if last_written_frame != Some(frames.latest_time) {
            distinct_frames += 1;
            last_written_frame = Some(frames.latest_time);
        }
    }
    if let Some(index) = writer.audio {
        let total = ((cutoff - zero).max(0) as u64 * 48_000) / 10_000_000;
        while audio_frames < total {
            let chunk = (total - audio_frames).min(480) as u32;
            write_audio(
                &mut writer,
                &mut audio,
                index,
                zero,
                &mut audio_frames,
                chunk,
            )?;
        }
    }
    writer
        .finish()
        .map_err(|e| format!("Finalize recording; partial file retained: {e}"))?;
    Ok(Outcome {
        elapsed_ms: ((cutoff - zero).max(0) / 10_000) as u64,
        bytes_written: writer.bytes_written(),
        encoder: writer.encoder.clone(),
        video_frames: next_video,
        distinct_frames,
        max_video_lag_ms: (max_video_lag / 10_000) as u64,
        max_steady_video_lag_ms: (max_steady_video_lag / 10_000) as u64,
    })
}

fn video_time(frame: u64, fps: u32) -> i64 {
    (i128::from(frame) * 10_000_000 / i128::from(fps)) as i64
}

fn validate_stop(no_media: bool, elapsed: i64, next_video: u64, fps: u32) -> Result<(), String> {
    if no_media {
        return Err("Recording canceled before its first media sample".into());
    }
    if elapsed - video_time(next_video, fps) > 2_500_000 {
        return Err(
            "Recording stopped more than 250 ms behind; the partial recording was retained".into(),
        );
    }
    Ok(())
}

struct CadenceGuard {
    soft: i64,
    late_since: Option<i64>,
}
impl CadenceGuard {
    fn new(fps: u32) -> Self {
        Self {
            soft: 500_000i64.max(20_000_000 / i64::from(fps)),
            late_since: None,
        }
    }
    fn check(&mut self, elapsed: i64, lag: i64) -> Result<(), String> {
        let failure = || {
            format!(
                "The encoder cannot keep up with recording ({} ms behind); the partial recording was retained",
                lag / 10_000
            )
        };
        if lag > 2_500_000 {
            return Err(failure());
        }
        if elapsed < 10_000_000 {
            return Ok(());
        }
        if lag <= self.soft / 2 {
            self.late_since = None;
        } else if lag > self.soft && self.late_since.is_none() {
            self.late_since = Some(elapsed);
        }
        if self
            .late_since
            .is_some_and(|since| elapsed - since >= 10_000_000)
        {
            return Err(failure());
        }
        Ok(())
    }
}
struct VideoFrames {
    latest: Arc<Vec<u8>>,
    latest_time: i64,
    pending: Option<(i64, Arc<Vec<u8>>)>,
}
impl VideoFrames {
    fn advance(&mut self, slot: i64) {
        if self.pending.as_ref().is_some_and(|(time, _)| *time <= slot) {
            let (time, image) = self.pending.take().unwrap();
            self.latest = image;
            self.latest_time = time;
        }
    }
    fn push(&mut self, time: i64, image: Vec<u8>, slot: i64) {
        if time < self.latest_time {
            return;
        }
        if time <= slot {
            self.latest = Arc::new(image);
            self.latest_time = time;
        } else {
            self.pending = Some((time, Arc::new(image)));
        }
    }
}

fn file_size(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}
fn write_audio(
    writer: &mut Writer,
    audio: &mut audio::AudioCapture,
    index: u32,
    zero: i64,
    frames: &mut u64,
    count: u32,
) -> Result<(), String> {
    let pts = (*frames as i128 * 10_000_000 / 48_000) as i64;
    let pcm = audio.mix_into(zero + pts, count)?;
    let bytes = unsafe { std::slice::from_raw_parts(pcm.as_ptr().cast::<u8>(), pcm.len() * 2) };
    writer
        .sample(index, bytes, pts, count as i64 * 10_000_000 / 48_000)
        .map_err(|e| format!("Encode audio: {e}"))?;
    *frames += count as u64;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scheduler_stalls_of_100_and_200_ms_recover_without_changing_the_epoch() {
        for stall in [1_000_000, 2_000_000] {
            let mut cadence = CadenceGuard::new(60);
            cadence.check(20_000_000, stall).unwrap();
            for step in 1..=12 {
                let lag = (stall - step * 166_667).max(0);
                cadence.check(20_000_000 + step * 20_000, lag).unwrap();
            }
            assert!(cadence.late_since.is_none());
        }
    }

    #[test]
    fn excessive_or_persistent_cadence_deficits_are_terminal() {
        let mut cadence = CadenceGuard::new(60);
        assert!(cadence.check(20_000_000, 2_500_001).is_err());
        cadence.check(20_000_000, 600_000).unwrap();
        cadence.check(29_999_999, 600_000).unwrap();
        assert!(cadence.check(30_000_000, 600_000).is_err());
    }

    #[test]
    fn cadence_recovery_requires_hysteresis_and_does_not_reset_the_deadline() {
        let mut cadence = CadenceGuard::new(60);
        cadence.check(20_000_000, 600_000).unwrap();
        cadence.check(25_000_000, 490_000).unwrap();
        cadence.check(29_000_000, 510_000).unwrap();
        assert!(cadence.check(30_000_000, 490_000).is_err());
        // Full recovery permits a later, independent bounded transient.
        let mut cadence = CadenceGuard::new(60);
        cadence.check(20_000_000, 2_000_000).unwrap();
        cadence.check(22_000_000, 200_000).unwrap();
        cadence.check(50_000_000, 1_000_000).unwrap();
        cadence.check(59_999_999, 300_000).unwrap();
        assert!(cadence.check(60_000_000, 300_000).is_err());
    }

    #[test]
    fn stop_drains_200_ms_backlog_to_the_common_audio_cutoff_without_future_images() {
        let fps = 60;
        for cutoff in [20_000_000, 20_123_456] {
            let mut frames = VideoFrames {
                latest: Arc::new(vec![1]),
                latest_time: 17_500_000,
                pending: Some((19_500_000, Arc::new(vec![2]))),
            };
            let mut next = 108; // The encoder has emitted only 1.8 seconds.
            validate_stop(false, cutoff, next, fps).unwrap();
            while video_time(next, fps) < cutoff {
                let pts = video_time(next, fps);
                frames.advance(pts);
                assert!(frames.latest_time <= pts);
                assert_eq!(frames.latest[0], if pts < 19_500_000 { 1 } else { 2 });
                next += 1;
            }
            let audio_samples = (cutoff as i128 * 48_000 / 10_000_000) as i64;
            let audio_end = audio_samples * 10_000_000 / 48_000;
            assert!((0..=209).contains(&(cutoff - audio_end)));
            assert!(video_time(next, fps) >= cutoff);
            assert!(video_time(next, fps) - audio_end <= video_time(1, fps) + 209);
            assert_eq!(next, if cutoff == 20_000_000 { 120 } else { 121 });
        }
    }

    #[test]
    fn stop_before_the_first_media_sample_remains_a_known_empty_cancellation() {
        assert!(
            validate_stop(true, 100_000, 0, 60)
                .unwrap_err()
                .contains("canceled before its first media sample")
        );
    }

    #[test]
    fn stop_rejects_unbounded_cfr_tail_after_a_long_stall() {
        assert!(validate_stop(false, 20_500_000, 108, 60).is_ok());
        assert!(validate_stop(false, 20_500_001, 108, 60).is_err());
        assert!(validate_stop(false, 600_000_000, 108, 60).is_err());
    }

    #[test]
    fn stop_tail_reuses_at_most_three_frame_buffers() {
        let mut frames = VideoFrames {
            latest: Arc::new(vec![1]),
            latest_time: 0,
            pending: None,
        };
        let mut worker = frames.latest.clone();
        frames.push(1_000_000, vec![2], 1_000_000);
        frames.push(2_000_000, vec![3], 1_000_000);
        let unique = |frames: &VideoFrames, worker: &Arc<Vec<u8>>| {
            let mut pointers =
                std::collections::HashSet::from([Arc::as_ptr(&frames.latest), Arc::as_ptr(worker)]);
            if let Some((_, pending)) = &frames.pending {
                pointers.insert(Arc::as_ptr(pending));
            }
            pointers.len()
        };
        assert_eq!(unique(&frames, &worker), 3);
        for frame in 6..18 {
            frames.advance(video_time(frame, 60));
            worker = frames.latest.clone();
            assert!(unique(&frames, &worker) <= 3);
        }
        assert_eq!(frames.latest[0], 3);
    }

    #[test]
    fn neutral_pixels_have_zero_chroma_at_every_reference_level() {
        for level in [0, 64, 128, 255] {
            let nv12 = rgb_to_nv12(2, 2, |_, _| [level; 3]);
            assert_eq!(&nv12[4..], &[128, 128]);
        }
    }
    #[test]
    fn delayed_new_frames_are_never_assigned_to_earlier_video_slots() {
        let mut frames = VideoFrames {
            latest: Arc::new(vec![1]),
            latest_time: 0,
            pending: None,
        };
        // A new image captured 500 ms later must not be used for an overdue
        // 100 ms slot, even if the writer has temporarily fallen behind.
        frames.push(5_000_000, vec![2], 1_000_000);
        assert_eq!(*frames.latest, vec![1]);
        frames.advance(4_999_999);
        assert_eq!(*frames.latest, vec![1]);
        frames.advance(5_000_000);
        assert_eq!(*frames.latest, vec![2]);
        assert_eq!(frames.latest_time, 5_000_000);
        frames.push(4_000_000, vec![3], 6_000_000);
        assert_eq!(*frames.latest, vec![2]);
        assert!(frames.pending.is_none());
    }

    #[test]
    fn color_health_change_requires_a_controlled_stop() {
        let hdr = DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
        let sdr = DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709;
        assert!(same_color_state(hdr, 2.5, hdr, 2.5));
        assert!(!same_color_state(hdr, 2.5, sdr, 1.0));
        assert!(!same_color_state(hdr, 2.5, hdr, 3.0));
        assert!(!same_color_state(hdr, 2.5, hdr, f32::NAN));
    }

    #[test]
    fn quality_presets_stay_distinct_at_small_and_ultrawide_sizes() {
        for (width, height) in [(642, 482), (1920, 1080), (7680, 2160), (7680, 4320)] {
            for fps in [24, 30, 60] {
                assert!(
                    video_bitrate(width, height, fps, true)
                        > video_bitrate(width, height, fps, false)
                );
            }
        }
    }

    #[test]
    fn fp16_decoding_handles_linear_hdr_range_and_invalid_components() {
        assert_eq!(half_to_float(0x3c00), 1.0);
        assert_eq!(half_to_float(0x4a40), 12.5);
        assert_eq!(half_to_float(0xbc00), -1.0);
        assert_eq!(half_to_float(1), 1.0 / 16_777_216.0);
        assert_eq!(half_to_float(0x7c00), 0.0);
        assert_eq!(half_to_float(0x7e00), 0.0);
    }

    #[test]
    fn hdr_tone_mapping_normalizes_os_white_and_retains_highlight_detail() {
        let mapper = ToneMapper::new(2.5);
        assert_eq!(mapper.convert([0.0; 3]), [0; 3]);
        let white = mapper.convert([2.5; 3]);
        assert_eq!(white[0], white[1]);
        assert_eq!(white[1], white[2]);
        assert!((237..=240).contains(&white[0]));
        let gray = mapper.convert([0.45; 3]); // 18% of configured SDR white.
        assert!((103..=105).contains(&gray[0]));
        let highlight = mapper.convert([10.0; 3]);
        assert!(highlight[0] > white[0] && highlight[0] < 255);
        let red = mapper.convert([2.5, 0.0, 0.0]);
        assert_eq!(red, [255, 0, 0]);
        let wide = mapper.convert([5.0, -0.5, 0.2]);
        assert!(wide[0] > wide[2] && wide[2] > wide[1]);
        assert_eq!(ToneMapper::new(1.0).convert([0.18; 3]), gray);
    }

    #[test]
    fn full_ultrawide_hdr_and_cropped_capture_have_bounded_buffer_estimates() {
        let display = Display {
            id: String::new(),
            name: String::new(),
            x: 0,
            y: 0,
            width: 7680,
            height: 2160,
        };
        let mut region = Region {
            display_id: String::new(),
            x: 0,
            y: 0,
            width: 7680,
            height: 2160,
        };
        assert_eq!(capture_buffer_bytes(&display, &region, true), 530_841_600);
        assert!(capture_buffer_bytes(&display, &region, true) < 512 * 1024 * 1024);
        region.width = 641;
        region.height = 481;
        assert!(capture_buffer_bytes(&display, &region, true) < 272_000_000);
        let large = Display {
            height: 4320,
            ..display
        };
        region.width = 7680;
        region.height = 4320;
        assert!(capture_buffer_bytes(&large, &region, true) > 512 * 1024 * 1024);
    }

    #[test]
    fn nv12_padding_keeps_selected_pixels_and_black_border() {
        let data = [0, 0, 255, 255].repeat(9);
        let n = bgra_to_nv12(&data, 3, 3);
        assert_eq!(n.len(), 24);
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(n[y * 4 + x], if x < 3 && y < 3 { 63 } else { 16 });
            }
        }
        assert_eq!(&n[16..18], &[102, 240]);
    }
}
