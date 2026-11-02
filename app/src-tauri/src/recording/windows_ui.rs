//! Helper-owned Win32 UI. Coordinates crossing this module are physical pixels.
use super::{Display, Region};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
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
    fn finish_drag(&mut self) {
        if self.dragging {
            self.dragging = false;
            self.confirmed = self.error.is_none() && self.bounds.is_some_and(Bounds::valid);
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
                state.finish_drag();
                release_capture = true;
                close = state.confirmed || state.error.is_some();
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
                    "Drag and release to select · Esc: cancel\nArrows: move · Shift+Arrows: resize · Ctrl: 10 px · Enter: confirm{}",
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

#[cfg(test)]
mod tests {
    use super::{Bounds, Display, Selector};
    use std::sync::{Arc, atomic::AtomicBool};

    fn selector(bounds: Option<Bounds>) -> Selector {
        Selector {
            display: Display {
                id: "test-display".into(),
                name: "Test display".into(),
                x: -1920,
                y: 0,
                width: 1920,
                height: 1080,
            },
            stop: Arc::new(AtomicBool::new(false)),
            start: Some((100, 80)),
            bounds,
            dragging: true,
            confirmed: false,
            error: None,
        }
    }

    #[test]
    fn left_release_confirms_valid_drag_without_keyboard_input() {
        let bounds = Bounds::drag((742, 564), (101, 83), 1920, 1080);
        let mut state = selector(Some(bounds));
        state.finish_drag();
        assert!(state.confirmed);
        assert!(!state.dragging);
        assert_eq!(state.bounds, Some(bounds));
    }

    #[test]
    fn left_release_keeps_empty_tiny_and_oversized_areas_uncommitted() {
        for bounds in [
            None,
            Some(Bounds::drag((100, 80), (100, 80), 1920, 1080)),
            Some(Bounds::drag((100, 80), (115, 180), 1920, 1080)),
            Some(Bounds::drag((0, 0), (7681, 4320), 15360, 8640)),
        ] {
            let mut state = selector(bounds);
            state.finish_drag();
            assert!(!state.confirmed);
            assert!(!state.dragging);
            assert_eq!(state.bounds, bounds);
        }
    }

    #[test]
    fn release_does_not_confirm_after_cursor_error_or_lost_capture() {
        let bounds = Bounds::drag((100, 80), (300, 180), 1920, 1080);
        let mut failed = selector(Some(bounds));
        failed.error = Some("Cursor position unavailable".into());
        failed.finish_drag();
        assert!(!failed.confirmed);
        assert!(!failed.dragging);

        let mut lost_capture = selector(Some(bounds));
        lost_capture.dragging = false;
        lost_capture.finish_drag();
        assert!(!lost_capture.confirmed);
    }

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
