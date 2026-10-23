//! Helper-owned Win32 UI. Coordinates crossing this module are physical pixels.
use super::{Display, Region};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
    core::{BOOL, PCWSTR, w},
};

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

struct PhysicalDpi(DPI_AWARENESS_CONTEXT);
impl PhysicalDpi {
    fn enter() -> Result<Self, String> {
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if previous.0.is_null() {
            Err(
                "Windows could not enable physical display coordinates (per-monitor DPI v2)."
                    .into(),
            )
        } else {
            Ok(Self(previous))
        }
    }
}
impl Drop for PhysicalDpi {
    fn drop(&mut self) {
        unsafe { SetThreadDpiAwarenessContext(self.0) };
    }
}

#[derive(Default)]
struct MonitorList {
    displays: Vec<(bool, Display)>,
    error: Option<String>,
}

unsafe extern "system" fn monitor_callback(
    monitor: HMONITOR,
    _: HDC,
    _: *mut RECT,
    parameter: LPARAM,
) -> BOOL {
    let list = unsafe { &mut *(parameter.0 as *mut MonitorList) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if !unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool() {
        list.error = Some("Windows could not read a display's physical bounds.".into());
        return BOOL(0);
    }
    let rect = info.monitorInfo.rcMonitor;
    let width = i64::from(rect.right) - i64::from(rect.left);
    let height = i64::from(rect.bottom) - i64::from(rect.top);
    if !(1..=i64::from(i32::MAX)).contains(&width) || !(1..=i64::from(i32::MAX)).contains(&height) {
        list.error = Some("Windows reported invalid display dimensions.".into());
        return BOOL(0);
    }
    let length = info
        .szDevice
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(info.szDevice.len());
    let id = String::from_utf16_lossy(&info.szDevice[..length]);
    if id.is_empty() {
        return BOOL(1);
    }
    let primary = info.monitorInfo.dwFlags & 1 != 0;
    list.displays.push((
        primary,
        Display {
            name: format!(
                "{}{} ({} × {})",
                id,
                if primary { " (Primary)" } else { "" },
                width,
                height
            ),
            id,
            x: rect.left,
            y: rect.top,
            width: width as u32,
            height: height as u32,
        },
    ));
    BOOL(1)
}

pub fn enumerate_displays() -> Result<Vec<Display>, String> {
    let _dpi = PhysicalDpi::enter()?;
    let mut list = MonitorList::default();
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(monitor_callback),
            LPARAM(&mut list as *mut _ as isize),
        )
    };
    if let Some(error) = list.error {
        return Err(error);
    }
    if !ok.as_bool() || list.displays.is_empty() {
        return Err("Windows did not report an available display.".into());
    }
    list.displays
        .sort_by(|a, b| b.0.cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
    Ok(list
        .displays
        .into_iter()
        .map(|(_, display)| display)
        .collect())
}

// Each invocation owns its class. No class or window is borrowed from Tauri.
struct WindowClass {
    name: Vec<u16>,
    instance: HINSTANCE,
}
impl WindowClass {
    fn register(procedure: WNDPROC) -> Result<Self, String> {
        static NEXT_CLASS: AtomicU64 = AtomicU64::new(1);
        let name = wide(&format!(
            "VflRecordingUi_{}_{}",
            std::process::id(),
            NEXT_CLASS.fetch_add(1, Ordering::Relaxed)
        ));
        let instance = HINSTANCE(
            unsafe { GetModuleHandleW(None) }
                .map_err(|e| e.to_string())?
                .0,
        );
        let class = WNDCLASSW {
            lpfnWndProc: procedure,
            hInstance: instance,
            lpszClassName: PCWSTR(name.as_ptr()),
            hCursor: unsafe { LoadCursorW(None, IDC_CROSS) }.map_err(|e| e.to_string())?,
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err("Windows could not register recording UI.".into());
        }
        Ok(Self { name, instance })
    }
}
impl Drop for WindowClass {
    fn drop(&mut self) {
        let _ = unsafe { UnregisterClassW(PCWSTR(self.name.as_ptr()), Some(self.instance)) };
    }
}

struct OwnedWindow(HWND);
impl Drop for OwnedWindow {
    fn drop(&mut self) {
        if unsafe { IsWindow(Some(self.0)) }.as_bool() {
            let _ = unsafe { DestroyWindow(self.0) };
        }
    }
}

fn exclude_from_capture(hwnd: HWND) -> Result<(), String> {
    unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) }
        .map_err(|e| format!("Could not exclude recording UI from capture: {e}"))?;
    let mut affinity = 0;
    unsafe { GetWindowDisplayAffinity(hwnd, &mut affinity) }
        .map_err(|e| format!("Could not verify recording UI capture exclusion: {e}"))?;
    if affinity != WDA_EXCLUDEFROMCAPTURE.0 {
        return Err("Windows did not enable recording UI capture exclusion.".into());
    }
    Ok(())
}

fn message_loop(hwnd: HWND) -> Result<(), String> {
    let mut message = MSG::default();
    loop {
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            return Ok(());
        }
        let status = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
        if status == -1 {
            return Err("Windows recording UI message processing failed.".into());
        }
        if status == 0 {
            return Ok(());
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bounds {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}
impl Bounds {
    fn drag(start: (i32, i32), end: (i32, i32), width: i32, height: i32) -> Self {
        let a = (start.0.clamp(0, width), start.1.clamp(0, height));
        let b = (end.0.clamp(0, width), end.1.clamp(0, height));
        Self {
            left: a.0.min(b.0),
            top: a.1.min(b.1),
            right: a.0.max(b.0),
            bottom: a.1.max(b.1),
        }
    }
    fn valid(self) -> bool {
        (16..=7680).contains(&(self.right - self.left))
            && (16..=4320).contains(&(self.bottom - self.top))
    }
    fn adjust(&mut self, dx: i32, dy: i32, resize: bool, width: i32, height: i32) {
        if resize {
            self.right = self
                .right
                .saturating_add(dx)
                .clamp((self.left + 16).min(width), width);
            self.bottom = self
                .bottom
                .saturating_add(dy)
                .clamp((self.top + 16).min(height), height);
        } else {
            let dx = dx.clamp(-self.left, width - self.right);
            let dy = dy.clamp(-self.top, height - self.bottom);
            self.left += dx;
            self.right += dx;
            self.top += dy;
            self.bottom += dy;
        }
    }
    fn rect(self) -> RECT {
        RECT {
            left: self.left,
            top: self.top,
            right: self.right,
            bottom: self.bottom,
        }
    }
}

struct Selector {
    display: Display,
    stop: Arc<AtomicBool>,
    start: Option<(i32, i32)>,
    bounds: Option<Bounds>,
    dragging: bool,
    confirmed: bool,
    error: Option<String>,
}
impl Selector {
    fn cursor(&self) -> Result<(i32, i32), String> {
        let mut point = POINT::default();
        unsafe { GetCursorPos(&mut point) }.map_err(|e| e.to_string())?;
        Ok((
            (i64::from(point.x) - i64::from(self.display.x)).clamp(0, i64::from(self.display.width))
                as i32,
            (i64::from(point.y) - i64::from(self.display.y))
                .clamp(0, i64::from(self.display.height)) as i32,
        ))
    }
    fn update_drag(&mut self) {
        match (self.start, self.cursor()) {
            (Some(start), Ok(end)) => {
                self.bounds = Some(Bounds::drag(
                    start,
                    end,
                    self.display.width as i32,
                    self.display.height as i32,
                ))
            }
            (_, Err(error)) => self.error = Some(error),
            _ => {}
        }
    }
}

unsafe extern "system" fn selector_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // BeginPaint can synchronously send this message. Avoid borrowing state
    // again while the outer paint callback is still reading it.
    if message == WM_ERASEBKGND {
        return LRESULT(1);
    }
    if message == WM_NCCREATE {
        let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize) };
    }
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Selector;
    if pointer.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let mut close = false;
    let mut release_capture = false;
    let handled = {
        let state = unsafe { &mut *pointer };
        match message {
            WM_LBUTTONDOWN => {
                match state.cursor() {
                    Ok(point) => {
                        state.start = Some(point);
                        state.bounds = None;
                        state.dragging = true;
                        unsafe { SetCapture(hwnd) };
                    }
                    Err(error) => {
                        state.error = Some(error);
                        close = true;
                    }
                }
                true
            }
            WM_MOUSEMOVE if state.dragging => {
                state.update_drag();
                close = state.error.is_some();
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                true
            }
            WM_LBUTTONUP if state.dragging => {
                state.update_drag();
                state.dragging = false;
                release_capture = true;
                close = state.error.is_some();
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                true
            }
            WM_CAPTURECHANGED => {
                state.dragging = false;
                true
            }
            WM_KEYDOWN => {
                match wparam.0 as u16 {
                    key if key == VK_ESCAPE.0 => close = true,
                    key if key == VK_RETURN.0 => {
                        if state.bounds.is_some_and(Bounds::valid) && !state.dragging {
                            state.confirmed = true;
                            close = true;
                        }
                    }
                    key => {
                        let step = if unsafe { GetKeyState(i32::from(VK_CONTROL.0)) } < 0 {
                            10
                        } else {
                            1
                        };
                        let delta = match key {
                            key if key == VK_LEFT.0 => (-step, 0),
                            key if key == VK_RIGHT.0 => (step, 0),
                            key if key == VK_UP.0 => (0, -step),
                            key if key == VK_DOWN.0 => (0, step),
                            _ => (0, 0),
                        };
                        if let Some(bounds) = &mut state.bounds {
                            bounds.adjust(
                                delta.0,
                                delta.1,
                                unsafe { GetKeyState(i32::from(VK_SHIFT.0)) } < 0,
                                state.display.width as i32,
                                state.display.height as i32,
                            );
                            let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                        }
                    }
                }
                true
            }
            WM_TIMER => {
                close = state.stop.load(Ordering::Acquire);
                true
            }
            WM_DISPLAYCHANGE => {
                state.error =
                    Some("Display configuration changed. Choose the recording area again.".into());
                close = true;
                true
            }
            WM_CLOSE => {
                close = true;
                true
            }
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = unsafe { BeginPaint(hwnd, &mut paint) };
                let area = RECT {
                    left: 0,
                    top: 0,
                    right: state.display.width as i32,
                    bottom: state.display.height as i32,
                };
                unsafe {
                    FillRect(dc, &area, HBRUSH(GetStockObject(BLACK_BRUSH).0));
                    SetTextColor(dc, COLORREF(0x00ffffff));
                    SetBkMode(dc, TRANSPARENT);
                }
                let dimensions = state
                    .bounds
                    .map(|b| format!("   {} × {} px", b.right - b.left, b.bottom - b.top))
                    .unwrap_or_default();
                let mut text = wide(&format!(
                    "Drag an area · Enter: confirm · Esc: cancel\nArrows: move · Shift+Arrows: resize · Ctrl: 10 px{}",
                    dimensions
                ));
                text.pop();
                let mut label = RECT {
                    left: 20,
                    top: 20,
                    right: area.right - 20,
                    bottom: 100,
                };
                unsafe { DrawTextW(dc, &mut text, &mut label, DT_LEFT | DT_WORDBREAK) };
                if let Some(bounds) = state.bounds {
                    unsafe { FrameRect(dc, &bounds.rect(), HBRUSH(GetStockObject(WHITE_BRUSH).0)) };
                }
                let _ = unsafe { EndPaint(hwnd, &paint) };
                true
            }
            WM_DESTROY => true,
            WM_NCDESTROY => {
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
                false
            }
            _ => false,
        }
    };
    // ReleaseCapture synchronously sends WM_CAPTURECHANGED to this window.
    if release_capture {
        let _ = unsafe { ReleaseCapture() };
    }
    if close {
        let _ = unsafe { DestroyWindow(hwnd) };
    }
    if handled {
        LRESULT(0)
    } else {
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }
}

/// Returns global physical coordinates restricted to the selected display.
pub fn select_region(display_id: &str, stop: Arc<AtomicBool>) -> Result<Option<Region>, String> {
    if stop.load(Ordering::Acquire) {
        return Ok(None);
    }
    let _dpi = PhysicalDpi::enter()?;
    let display = enumerate_displays()?
        .into_iter()
        .find(|d| d.id == display_id)
        .ok_or("The selected display is no longer available.")?;
    let class = WindowClass::register(Some(selector_proc))?;
    let mut state = Box::new(Selector {
        display,
        stop,
        start: None,
        bounds: None,
        dragging: false,
        confirmed: false,
        error: None,
    });
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            PCWSTR(class.name.as_ptr()),
            w!("Choose recording area"),
            WS_POPUP,
            state.display.x,
            state.display.y,
            state.display.width as i32,
            state.display.height as i32,
            None,
            None,
            Some(class.instance),
            Some((&mut *state as *mut Selector).cast()),
        )
    }
    .map_err(|e| format!("Could not open recording area selector: {e}"))?;
    let window = OwnedWindow(hwnd);
    unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), 135, LWA_ALPHA) }
        .map_err(|e| e.to_string())?;
    // Never leave a recording overlay visible in an accidentally concurrent capture.
    exclude_from_capture(hwnd)?;
    if unsafe { SetTimer(Some(hwnd), 1, 100, None) } == 0 {
        return Err("Could not start recording selector cancellation timer.".into());
    }
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
    }
    message_loop(hwnd)?;
    drop(window);
    if let Some(error) = state.error.take() {
        return Err(error);
    }
    if !state.confirmed || state.stop.load(Ordering::Acquire) {
        return Ok(None);
    }
    let bounds = state
        .bounds
        .filter(|b| b.valid())
        .ok_or("Choose an area between 16 × 16 and 7680 × 4320 pixels.")?;
    let current = enumerate_displays()?
        .into_iter()
        .find(|d| d.id == state.display.id)
        .ok_or("The selected display was disconnected.")?;
    if (current.x, current.y, current.width, current.height)
        != (
            state.display.x,
            state.display.y,
            state.display.width,
            state.display.height,
        )
    {
        return Err("Display configuration changed. Choose the recording area again.".into());
    }
    Ok(Some(Region {
        display_id: state.display.id.clone(),
        x: state
            .display
            .x
            .checked_add(bounds.left)
            .ok_or("Recording area coordinate overflow.")?,
        y: state
            .display
            .y
            .checked_add(bounds.top)
            .ok_or("Recording area coordinate overflow.")?,
        width: (bounds.right - bounds.left) as u32,
        height: (bounds.bottom - bounds.top) as u32,
    }))
}

const HUD_PAUSE: usize = 101;
const HUD_STOP: usize = 100;
const HUD_SHUTDOWN: u32 = WM_APP + 1;
struct HudState {
    stop: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    controls: Arc<super::native_controls::Controls>,
    handle: Arc<Mutex<Option<isize>>>,
}

unsafe extern "system" fn hud_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_ERASEBKGND {
        return LRESULT(1);
    }
    if message == WM_NCCREATE {
        let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize) };
    }
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut HudState;
    if pointer.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let mut close = false;
    let handled = {
        let state = unsafe { &mut *pointer };
        match message {
            WM_CLOSE => {
                state.stop.store(true, Ordering::Release);
                close = true;
                true
            }
            WM_COMMAND if wparam.0 & 0xffff == HUD_STOP => {
                state.stop.store(true, Ordering::Release);
                close = true;
                true
            }
            WM_COMMAND if wparam.0 & 0xffff == HUD_PAUSE => {
                state.controls.hud_intent();
                true
            }
            HUD_SHUTDOWN => {
                close = true;
                true
            }
            WM_TIMER => {
                close =
                    state.stop.load(Ordering::Acquire) || state.shutdown.load(Ordering::Acquire);
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                true
            }
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = unsafe { BeginPaint(hwnd, &mut paint) };
                let area = RECT {
                    left: 0,
                    top: 0,
                    right: 430,
                    bottom: 118,
                };
                unsafe {
                    FillRect(dc, &area, HBRUSH(GetStockObject(BLACK_BRUSH).0));
                    SetTextColor(dc, COLORREF(0x00ffffff));
                    SetBkMode(dc, TRANSPARENT);
                }
                let (elapsed_ms, phase) = state.controls.hud_state();
                let elapsed = elapsed_ms / 1000;
                let label_phase = match phase {
                    1 => "Pausing",
                    2 => "Paused · inputs released",
                    3 => "Resuming",
                    _ => "Recording",
                };
                if let Ok(button) = unsafe { GetDlgItem(Some(hwnd), HUD_PAUSE as i32) } {
                    let _ = unsafe {
                        SetWindowTextW(
                            button,
                            if phase == 2 {
                                w!("Resume")
                            } else {
                                w!("Pause")
                            },
                        )
                    };
                    let _ = unsafe { EnableWindow(button, phase == 0 || phase == 2) };
                }
                let mut label = wide(&format!(
                    "{}  {:02}:{:02}:{:02}",
                    label_phase,
                    elapsed / 3600,
                    (elapsed / 60) % 60,
                    elapsed % 60
                ));
                label.pop();
                let mut rect = RECT {
                    left: 12,
                    top: 12,
                    right: 410,
                    bottom: 68,
                };
                unsafe { DrawTextW(dc, &mut label, &mut rect, DT_LEFT | DT_WORDBREAK) };
                let _ = unsafe { EndPaint(hwnd, &paint) };
                true
            }
            WM_DESTROY => true,
            WM_NCDESTROY => {
                if let Ok(mut handle) = state.handle.lock() {
                    *handle = None;
                }
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
                false
            }
            _ => false,
        }
    };
    if close {
        let _ = unsafe { DestroyWindow(hwnd) };
    }
    if handled {
        LRESULT(0)
    } else {
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }
}

pub struct StopHud {
    handle: Arc<Mutex<Option<isize>>>,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl StopHud {
    pub fn start(
        stop: Arc<AtomicBool>,
        controls: Arc<super::native_controls::Controls>,
    ) -> Result<Self, String> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_thread = shutdown.clone();
        let handle = Arc::new(Mutex::new(None));
        let handle_thread = handle.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("recording-stop-ui".into())
            .spawn(move || {
                let result = run_hud(
                    stop.clone(),
                    shutdown_thread,
                    handle_thread,
                    controls,
                    &ready_tx,
                );
                if let Err(error) = result {
                    stop.store(true, Ordering::Release);
                    let _ = ready_tx.send(Err(error));
                }
            })
            .map_err(|e| format!("Could not start recording Stop UI: {e}"))?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self {
                handle,
                shutdown,
                thread: Some(thread),
            }),
            result => {
                shutdown.store(true, Ordering::Release);
                bounded_join(thread);
                Err(match result {
                    Ok(Err(error)) => error,
                    _ => "Recording Stop UI did not become ready.".into(),
                })
            }
        }
    }
}
impl Drop for StopHud {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        // NCDESTROY clears the handle under the same lock before Windows can
        // reuse it. Never post a shutdown message to a recycled foreign HWND.
        if let Ok(handle) = self.handle.lock()
            && let Some(hwnd) = *handle
        {
            let _ = unsafe {
                PostMessageW(
                    Some(HWND(hwnd as *mut _)),
                    HUD_SHUTDOWN,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
        }
        if let Some(thread) = self.thread.take() {
            bounded_join(thread);
        }
    }
}

fn bounded_join(thread: JoinHandle<()>) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !thread.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if thread.is_finished() {
        let _ = thread.join();
    }
    // A stalled Win32 thread must not stall recording finalization or process shutdown.
    // Its cancellation flag remains set; the helper process owns its final lifetime.
}

fn run_hud(
    stop: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    handle: Arc<Mutex<Option<isize>>>,
    controls: Arc<super::native_controls::Controls>,
    ready: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let _dpi = PhysicalDpi::enter()?;
    let class = WindowClass::register(Some(hud_proc))?;
    let display = enumerate_displays()?
        .into_iter()
        .next()
        .ok_or("No display for recording Stop UI.")?;
    let mut state = Box::new(HudState {
        stop,
        shutdown,
        controls,
        handle: handle.clone(),
    });
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(class.name.as_ptr()),
            w!("Video For Lazies recording"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            display.x.saturating_add(24),
            display.y.saturating_add(24),
            440,
            160,
            None,
            None,
            Some(class.instance),
            Some((&mut *state as *mut HudState).cast()),
        )
    }
    .map_err(|e| format!("Could not open recording Stop UI: {e}"))?;
    let window = OwnedWindow(hwnd);
    *handle
        .lock()
        .map_err(|_| "Recording Stop UI ownership state failed.")? = Some(hwnd.0 as isize);
    exclude_from_capture(hwnd)?;
    // PMv2 scales the native caption. Preserve sufficient physical client
    // space for the Stop button at every display scaling factor.
    let mut outer = RECT {
        left: 0,
        top: 0,
        right: 430,
        bottom: 118,
    };
    unsafe {
        AdjustWindowRectExForDpi(
            &mut outer,
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            false,
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            GetDpiForWindow(hwnd),
        )
    }
    .map_err(|e| format!("Could not size recording Stop UI: {e}"))?;
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            outer.right - outer.left,
            outer.bottom - outer.top,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }
    .map_err(|e| format!("Could not size recording Stop UI: {e}"))?;
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!("Stop recording"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            12,
            74,
            170,
            30,
            Some(hwnd),
            Some(HMENU(HUD_STOP as *mut _)),
            Some(class.instance),
            None,
        )
    }
    .map_err(|e| format!("Could not create recording Stop button: {e}"))?;
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!("Pause"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
            196,
            74,
            170,
            30,
            Some(hwnd),
            Some(HMENU(HUD_PAUSE as *mut _)),
            Some(class.instance),
            None,
        )
    }
    .map_err(|e| format!("Could not create Pause/Resume button: {e}"))?;
    if unsafe { SetTimer(Some(hwnd), 1, 100, None) } == 0 {
        return Err("Could not start recording Stop UI timer.".into());
    }
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return Err("The recording Stop window could not become visible.".into());
    }
    ready
        .send(Ok(()))
        .map_err(|_| "Recording host stopped before Stop UI was ready.")?;
    let result = message_loop(hwnd);
    drop(window);
    result
}

#[cfg(test)]
mod tests {
    use super::Bounds;

    #[test]
    fn reverse_drag_clamps_to_one_display() {
        assert_eq!(
            Bounds::drag((1800, 1000), (-300, -90), 1920, 1080),
            Bounds {
                left: 0,
                top: 0,
                right: 1800,
                bottom: 1000
            }
        );
        assert_eq!(
            Bounds::drag((-900, 50), (4000, 2000), 1920, 1080),
            Bounds {
                left: 0,
                top: 50,
                right: 1920,
                bottom: 1080
            }
        );
    }

    #[test]
    fn movement_preserves_size_and_clamps_edges() {
        let mut bounds = Bounds::drag((100, 80), (300, 180), 800, 600);
        bounds.adjust(-1000, 2000, false, 800, 600);
        assert_eq!(
            bounds,
            Bounds {
                left: 0,
                top: 500,
                right: 200,
                bottom: 600
            }
        );
        bounds.adjust(2000, -2000, false, 800, 600);
        assert_eq!(
            bounds,
            Bounds {
                left: 600,
                top: 0,
                right: 800,
                bottom: 100
            }
        );
    }

    #[test]
    fn resize_enforces_minimum_and_monitor_bounds() {
        let mut bounds = Bounds::drag((100, 80), (300, 180), 800, 600);
        bounds.adjust(i32::MIN, i32::MAX, true, 800, 600);
        assert_eq!(
            bounds,
            Bounds {
                left: 100,
                top: 80,
                right: 116,
                bottom: 600
            }
        );
        assert!(bounds.valid());
        assert!(!Bounds::drag((0, 0), (1, 100), 800, 600).valid());
    }

    #[test]
    fn translated_negative_origin_stays_inside_physical_display() {
        for start in [(-500, -500), (0, 0), (400, 300), (2000, 2000)] {
            for end in [(-500, -500), (0, 0), (1000, 800), (3000, 3000)] {
                let bounds = Bounds::drag(start, end, 1920, 1080);
                let (x, y) = (-1920 + bounds.left, -1080 + bounds.top);
                assert!((-1920..=0).contains(&x));
                assert!((-1080..=0).contains(&y));
                assert!(x + (bounds.right - bounds.left) <= 0);
                assert!(y + (bounds.bottom - bounds.top) <= 0);
            }
        }
    }

    #[test]
    fn odd_selection_preserves_requested_pixels_and_respects_size_limits() {
        let chosen = Bounds::drag((101, 83), (742, 564), 1920, 1080);
        assert_eq!(
            (chosen.right - chosen.left, chosen.bottom - chosen.top),
            (641, 481)
        );
        assert!(chosen.valid());
        assert!(!Bounds::drag((0, 0), (15, 100), 1920, 1080).valid());
        assert!(!Bounds::drag((0, 0), (7682, 4320), 15360, 8640).valid());
    }
}
