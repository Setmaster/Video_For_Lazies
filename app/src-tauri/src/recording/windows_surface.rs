//! Helper-owned native preview child. All HWND/D3D work stays on the capture owner thread.
//! The surface is ordinary capturable app content, like the former browser canvas.
use super::{
    gpu,
    preview::{self, Viewport},
    source::Geometry,
};
use std::{
    marker::PhantomData,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, DXGI_STATUS_OCCLUDED, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, RECT,
            WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
        },
        Graphics::{
            Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
            Direct3D11::*,
            Dxgi::{Common::*, *},
        },
        System::{LibraryLoader::GetModuleHandleW, Threading::WaitForSingleObject},
        UI::{
            HiDpi::{
                AreDpiAwarenessContextsEqual, GetThreadDpiAwarenessContext,
                GetWindowDpiAwarenessContext,
            },
            Input::KeyboardAndMouse::IsWindowEnabled,
            WindowsAndMessaging::*,
        },
    },
    core::{Interface, PCWSTR, w},
};

pub const MAX_SURFACE_WIDTH: u32 = 1920;
pub const MAX_SURFACE_HEIGHT: u32 = 1080;
pub const MAX_SURFACE_BYTES: u64 = MAX_SURFACE_WIDTH as u64 * MAX_SURFACE_HEIGHT as u64 * 4 * 2;

pub use super::preview::ContentRect;
pub fn content_rect(source: Geometry, width: u32, height: u32) -> ContentRect {
    let (w, h) = preview::dimensions_with_bounds(source.width, source.height, width, height);
    // Viewports may upscale a small source; aspect containment must still fill the canvas.
    let (w, h) = if source.width <= width && source.height <= height {
        if u64::from(source.width) * u64::from(height) > u64::from(source.height) * u64::from(width)
        {
            (
                width,
                (u64::from(source.height) * u64::from(width) / u64::from(source.width)).max(1)
                    as u32,
            )
        } else {
            (
                (u64::from(source.width) * u64::from(height) / u64::from(source.height)).max(1)
                    as u32,
                height,
            )
        }
    } else {
        (w, h)
    };
    ContentRect {
        left: (width - w) / 2,
        top: (height - h) / 2,
        width: w,
        height: h,
    }
}
fn image_target(canvas: &Viewport, source: Geometry) -> Viewport {
    let content = content_rect(source, canvas.width, canvas.height);
    Viewport {
        x: canvas.x + content.left as i32,
        y: canvas.y + content.top as i32,
        width: content.width,
        height: content.height,
        ..canvas.clone()
    }
}
unsafe extern "system" fn procedure(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match message {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        _ => unsafe { DefWindowProcW(hwnd, message, wp, lp) },
    }
}
struct NativeWindow {
    hwnd: HWND,
    class: Vec<u16>,
    instance: HINSTANCE,
}
impl NativeWindow {
    fn new(target: &Viewport) -> Result<Self, String> {
        let parent = owner(target)?;
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let class: Vec<u16> = format!(
            "VflRecordingPreview_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
        .encode_utf16()
        .chain(Some(0))
        .collect();
        let instance = HINSTANCE(unsafe { GetModuleHandleW(None) }.map_err(err)?.0);
        let description = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: instance,
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&description) } == 0 {
            return Err("Could not register native preview surface.".into());
        }
        let hwnd = match unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_NOPARENTNOTIFY,
                PCWSTR(class.as_ptr()),
                w!("Recording preview"),
                WS_CHILD | WS_DISABLED | WS_CLIPSIBLINGS,
                target.x,
                target.y,
                target.width as i32,
                target.height as i32,
                Some(parent),
                None,
                Some(instance),
                None,
            )
        } {
            Ok(hwnd) => hwnd,
            Err(error) => {
                let _ = unsafe { UnregisterClassW(PCWSTR(class.as_ptr()), Some(instance)) };
                return Err(err(error));
            }
        };
        Ok(Self {
            hwnd,
            class,
            instance,
        })
    }
}
impl Drop for NativeWindow {
    fn drop(&mut self) {
        unsafe {
            if IsWindow(Some(self.hwnd)).as_bool() {
                let _ = DestroyWindow(self.hwnd);
            }
            let _ = UnregisterClassW(PCWSTR(self.class.as_ptr()), Some(self.instance));
        }
    }
}
struct Latency(HANDLE);
impl Drop for Latency {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}
// Drop GPU references/latency handle before destroying the associated HWND.
struct Surface {
    target: Viewport,
    render_width: u32,
    render_height: u32,
    has_presented: bool,
    created: Instant,
    view: ID3D11RenderTargetView,
    latency: Latency,
    swap: IDXGISwapChain1,
    window: NativeWindow,
}
fn owner(target: &Viewport) -> Result<HWND, String> {
    target.validate()?;
    let parent = HWND(
        target
            .parent_hwnd
            .parse::<usize>()
            .map_err(|_| "Invalid preview owner")? as *mut _,
    );
    let mut pid = 0;
    unsafe {
        if !IsWindow(Some(parent)).as_bool()
            || GetWindowThreadProcessId(parent, Some(&mut pid)) == 0
            || pid != target.parent_pid
        {
            return Err("Native preview owner changed.".into());
        }
        if !AreDpiAwarenessContextsEqual(
            GetWindowDpiAwarenessContext(parent),
            GetThreadDpiAwarenessContext(),
        )
        .as_bool()
        {
            return Err("Native preview owner DPI context differs from capture.".into());
        }
    }
    Ok(parent)
}
pub fn available(target: &Viewport) -> Result<bool, String> {
    let parent = owner(target)?;
    Ok(unsafe { IsWindowVisible(parent) }.as_bool()
        && !unsafe { IsIconic(parent) }.as_bool()
        && unsafe { IsWindowEnabled(parent) }.as_bool())
}
fn buffer_dimensions(width: u32, height: u32, budget: u64) -> Result<(u32, u32), String> {
    let desired =
        preview::dimensions_with_bounds(width, height, MAX_SURFACE_WIDTH, MAX_SURFACE_HEIGHT);
    let minimum = preview::dimensions_with_bounds(width, height, 480, 270);
    if u64::from(minimum.0) * u64::from(minimum.1) * 8 > budget {
        return Err(
            "Native preview surface exceeds the capture buffer budget. Recording is unaffected."
                .into(),
        );
    }
    if u64::from(desired.0) * u64::from(desired.1) * 8 <= budget {
        return Ok(desired);
    }
    let scale = (budget as f64 / (u64::from(desired.0) * u64::from(desired.1) * 8) as f64).sqrt();
    let (mut w, mut h) = (
        (desired.0 as f64 * scale).floor().max(1.) as u32,
        (desired.1 as f64 * scale).floor().max(1.) as u32,
    );
    while u64::from(w) * u64::from(h) * 8 > budget {
        if w > h {
            w -= 1;
        } else {
            h -= 1;
        }
    }
    Ok((w, h))
}
fn pending_presentation(presented: bool, elapsed: Duration) -> Result<(), String> {
    if !presented && elapsed >= Duration::from_secs(2) {
        Err("Windows did not present a native preview frame within2seconds.".into())
    } else {
        Ok(())
    }
}
impl Surface {
    fn new(device: &ID3D11Device, target: &Viewport, budget: u64) -> Result<Self, String> {
        let window = NativeWindow::new(target)?;
        let dxgi: IDXGIDevice = device.cast().map_err(err)?;
        let adapter = unsafe { dxgi.GetAdapter() }.map_err(err)?;
        let factory: IDXGIFactory2 = unsafe { adapter.GetParent() }.map_err(err)?;
        let (render_width, render_height) = buffer_dimensions(target.width, target.height, budget)?;
        let swap = unsafe {
            factory.CreateSwapChainForHwnd(
                device,
                window.hwnd,
                &DXGI_SWAP_CHAIN_DESC1 {
                    Width: render_width,
                    Height: render_height,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                    BufferCount: 2,
                    Scaling: DXGI_SCALING_STRETCH,
                    SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                    AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                    Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32,
                    ..Default::default()
                },
                None,
                None,
            )
        }
        .map_err(err)?;
        unsafe { factory.MakeWindowAssociation(window.hwnd, DXGI_MWA_NO_ALT_ENTER) }
            .map_err(err)?;
        let latency_swap: IDXGISwapChain2 = swap.cast().map_err(err)?;
        unsafe { latency_swap.SetMaximumFrameLatency(1) }.map_err(err)?;
        let handle = unsafe { latency_swap.GetFrameLatencyWaitableObject() };
        if handle.is_invalid() {
            return Err("Native preview frame-latency handle is unavailable.".into());
        }
        let latency = Latency(handle);
        let buffer: ID3D11Texture2D = unsafe { swap.GetBuffer(0) }.map_err(err)?;
        let mut view = None;
        unsafe { device.CreateRenderTargetView(&buffer, None, Some(&mut view)) }.map_err(err)?;
        let view = view.ok_or("Native preview has no render target")?;
        Ok(Self {
            target: target.clone(),
            render_width,
            render_height,
            has_presented: false,
            created: Instant::now(),
            view,
            latency,
            swap,
            window,
        })
    }
}
fn err(error: windows::core::Error) -> String {
    format!("Native GPU preview: {error}")
}

pub struct Presenter {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    vertex: ID3D11VertexShader,
    pixel: ID3D11PixelShader,
    constants: ID3D11Buffer,
    surface: Option<Surface>,
    budget: u64,
    generation: Option<(String, u64)>,
    _owner: PhantomData<Rc<()>>,
}
pub struct Presented {
    pub hwnd: String,
    pub content: ContentRect,
    pub width: u32,
    pub height: u32,
}
impl Presenter {
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        budget: u64,
    ) -> Result<Self, String> {
        let vs = gpu::compile_display_shader(false)?;
        let ps = gpu::compile_display_shader(true)?;
        let mut vertex = None;
        let mut pixel = None;
        let mut constants = None;
        unsafe {
            let bytes = |code: &windows::Win32::Graphics::Direct3D::ID3DBlob| {
                std::slice::from_raw_parts(code.GetBufferPointer().cast(), code.GetBufferSize())
            };
            device
                .CreateVertexShader(bytes(&vs), None, Some(&mut vertex))
                .map_err(err)?;
            device
                .CreatePixelShader(bytes(&ps), None, Some(&mut pixel))
                .map_err(err)?;
            device
                .CreateBuffer(
                    &D3D11_BUFFER_DESC {
                        ByteWidth: 32,
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                        ..Default::default()
                    },
                    None,
                    Some(&mut constants),
                )
                .map_err(err)?;
        }
        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            vertex: vertex.ok_or("No preview vertex shader")?,
            pixel: pixel.ok_or("No preview pixel shader")?,
            constants: constants.ok_or("No preview constants")?,
            surface: None,
            budget,
            generation: None,
            _owner: PhantomData,
        })
    }
    pub fn visible(&self) -> bool {
        self.surface.as_ref().is_some_and(|surface| {
            surface.has_presented && unsafe { IsWindowVisible(surface.window.hwnd) }.as_bool()
        })
    }
    pub fn hide(&mut self) {
        self.surface = None;
    }
    pub fn draw(
        &mut self,
        target: &Viewport,
        source: &gpu::Converter,
        geometry: Geometry,
        color: (bool, f32),
        generation: (&str, u64),
        current: impl Fn() -> bool,
    ) -> Result<Option<Presented>, String> {
        if !current() {
            self.hide();
            return Ok(None);
        }
        if self
            .generation
            .as_ref()
            .is_none_or(|old| old.0 != generation.0 || old.1 != generation.1)
        {
            self.hide();
            self.generation = Some((generation.0.into(), generation.1));
        }
        let (hdr, white) = color;
        let parent = owner(target)?;
        if !unsafe { IsWindowVisible(parent) }.as_bool()
            || unsafe { IsIconic(parent) }.as_bool()
            || !unsafe { IsWindowEnabled(parent) }.as_bool()
        {
            self.hide();
            return Ok(None);
        }
        let mut client = RECT::default();
        unsafe { GetClientRect(parent, &mut client) }.map_err(err)?;
        if i64::from(target.x) + i64::from(target.width) > i64::from(client.right)
            || i64::from(target.y) + i64::from(target.height) > i64::from(client.bottom)
        {
            // A resize can precede the next browser viewport update. Do not
            // draw outside Main or fail a still-valid preview generation.
            self.hide();
            return Ok(None);
        }
        let image = image_target(target, geometry);
        let target = &image;
        if self.surface.as_ref().is_none_or(|surface| {
            surface.target.parent_hwnd != target.parent_hwnd
                || surface.target.parent_pid != target.parent_pid
                || surface.target.width != target.width
                || surface.target.height != target.height
        }) {
            self.surface = None;
            self.surface = Some(Surface::new(&self.device, target, self.budget)?);
        }
        let surface = self.surface.as_mut().unwrap();
        let mut message = MSG::default();
        while unsafe { PeekMessageW(&mut message, Some(surface.window.hwnd), 0, 0, PM_REMOVE) }
            .as_bool()
        {
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if !unsafe { IsWindow(Some(surface.window.hwnd)) }.as_bool() {
            return Err("Native preview child was destroyed.".into());
        }
        unsafe {
            SetWindowPos(
                surface.window.hwnd,
                Some(HWND_TOP),
                target.x,
                target.y,
                target.width as i32,
                target.height as i32,
                SWP_NOACTIVATE
                    | if surface.has_presented {
                        SWP_SHOWWINDOW
                    } else {
                        SET_WINDOW_POS_FLAGS(0)
                    },
            )
        }
        .map_err(err)?;
        surface.target = target.clone();
        let wait = unsafe { WaitForSingleObject(surface.latency.0, 0) };
        if wait == WAIT_TIMEOUT {
            pending_presentation(surface.has_presented, surface.created.elapsed())?;
            return Ok(None);
        }
        if wait != WAIT_OBJECT_0 {
            return Err(format!("Native preview frame wait failed:0x{:08X}", wait.0));
        }
        let content = ContentRect {
            left: 0,
            top: 0,
            width: target.width,
            height: target.height,
        };
        let sx = surface.render_width as f32 / target.width as f32;
        let sy = surface.render_height as f32 / target.height as f32;
        let draw_width = (content.width as f32 * sx).round().max(1.0) as u32;
        let draw_height = (content.height as f32 * sy).round().max(1.0) as u32;
        let constants = [
            geometry.width,
            geometry.height,
            draw_width,
            draw_height,
            0,
            u32::from(hdr),
            white.to_bits(),
            draw_width | (draw_height << 16),
        ];
        unsafe {
            self.context.UpdateSubresource(
                &self.constants,
                0,
                None,
                constants.as_ptr().cast(),
                0,
                0,
            );
            self.context
                .ClearRenderTargetView(&surface.view, &[0., 0., 0., 1.]);
            self.context
                .OMSetRenderTargets(Some(&[Some(surface.view.clone())]), None);
            self.context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: content.left as f32 * sx,
                TopLeftY: content.top as f32 * sy,
                Width: content.width as f32 * sx,
                Height: content.height as f32 * sy,
                MinDepth: 0.,
                MaxDepth: 1.,
            }]));
            self.context.IASetInputLayout(None);
            self.context
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.context.VSSetShader(&self.vertex, None);
            self.context.PSSetShader(&self.pixel, None);
            self.context.PSSetShaderResources(
                0,
                Some(&[
                    Some(source.source_view().clone()),
                    Some(source.transfer_view().clone()),
                ]),
            );
            self.context
                .PSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
            self.context.Draw(3, 0);
            self.context.PSSetShaderResources(0, Some(&[None, None]));
            self.context.PSSetConstantBuffers(0, Some(&[None]));
            self.context.OMSetRenderTargets(None, None);
            self.context.VSSetShader(None, None);
            self.context.PSSetShader(None, None);
        }
        // Never let UI presentation wait or backpressure the recording loop.
        if !current() {
            self.hide();
            return Ok(None);
        }
        let surface = self.surface.as_mut().unwrap();
        let result = unsafe { surface.swap.Present(0, DXGI_PRESENT_DO_NOT_WAIT) };
        if result == DXGI_ERROR_WAS_STILL_DRAWING || result == DXGI_STATUS_OCCLUDED {
            pending_presentation(surface.has_presented, surface.created.elapsed())?;
            return Ok(None);
        }
        result.ok().map_err(err)?;
        if !current() {
            self.hide();
            return Ok(None);
        }
        let surface = self.surface.as_mut().unwrap();
        // FLIP_DISCARD can render while hidden: show only a successfully submitted frame.
        unsafe {
            SetWindowPos(
                surface.window.hwnd,
                Some(HWND_TOP),
                target.x,
                target.y,
                target.width as i32,
                target.height as i32,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
        }
        .map_err(err)?;
        surface.has_presented = true;
        Ok(Some(Presented {
            hwnd: (surface.window.hwnd.0 as usize).to_string(),
            content,
            width: target.width,
            height: target.height,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ultrawide_and_tall_canvases_do_not_spend_resolution_on_bars() {
        let canvas = Viewport {
            parent_hwnd: "1".into(),
            parent_pid: 1,
            x: 10,
            y: 20,
            width: 7000,
            height: 1200,
            revision: 1,
        };
        let image = image_target(
            &canvas,
            Geometry {
                width: 1920,
                height: 1080,
            },
        );
        assert_eq!(
            (image.x, image.y, image.width, image.height),
            (2443, 20, 2133, 1200)
        );
        let (w, h) = buffer_dimensions(image.width, image.height, MAX_SURFACE_BYTES).unwrap();
        assert!(w >= 1918 && h == 1080);
        assert!(u64::from(w) * u64::from(h) * 8 <= MAX_SURFACE_BYTES);
        let tall = image_target(
            &Viewport {
                width: 1200,
                height: 7000,
                ..canvas
            },
            Geometry {
                width: 1080,
                height: 1920,
            },
        );
        assert_eq!((tall.width, tall.height), (1200, 2133));
        let (w, h) = buffer_dimensions(tall.width, tall.height, MAX_SURFACE_BYTES).unwrap();
        assert!(w >= 607 && h == 1080);
    }
    #[test]
    fn surface_budget_scales_without_exceeding_two_buffers_or_old_clarity() {
        assert_eq!(
            buffer_dimensions(3840, 2160, MAX_SURFACE_BYTES).unwrap(),
            (1920, 1080)
        );
        let (w, h) = buffer_dimensions(1920, 1080, 8 * 1280 * 720).unwrap();
        assert!(u64::from(w) * u64::from(h) * 8 <= 8 * 1280 * 720);
        assert!(w >= 480 && h >= 270);
        assert!(buffer_dimensions(1920, 1080, 8 * 480 * 270 - 1).is_err());
    }
    #[test]
    fn captured_content_alone_never_satisfies_first_presentation_deadline() {
        assert!(pending_presentation(false, Duration::from_millis(1999)).is_ok());
        assert!(pending_presentation(false, Duration::from_secs(2)).is_err());
        assert!(pending_presentation(false, Duration::from_secs(3)).is_err());
        assert!(pending_presentation(true, Duration::from_secs(30)).is_ok());
    }
    #[test]
    fn aspect_containment_preserves_full_source_and_centers_bars() {
        assert_eq!(
            content_rect(
                Geometry {
                    width: 1920,
                    height: 1080
                },
                1000,
                1000
            ),
            ContentRect {
                left: 0,
                top: 219,
                width: 1000,
                height: 562
            }
        );
        assert_eq!(
            content_rect(
                Geometry {
                    width: 100,
                    height: 200
                },
                1920,
                1080
            ),
            ContentRect {
                left: 690,
                top: 0,
                width: 540,
                height: 1080
            }
        );
    }
}
