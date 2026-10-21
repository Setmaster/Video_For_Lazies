//! Window discovery and health checks only. No capture session or thumbnail is opened.
//! HWND/process/class identities remain inside the helper; public IDs are parent-owned.
use super::source::{Display, WindowIdentity, WindowInfo, WindowListing};
use std::{ffi::c_void, mem::size_of, panic::AssertUnwindSafe};
use windows::{
    Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE, HWND, LPARAM, RECT},
        Graphics::{
            Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
            Gdi::{
                GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW,
                MonitorFromWindow,
            },
        },
        System::Threading::{
            GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
        },
        UI::{
            HiDpi::{
                DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                SetThreadDpiAwarenessContext,
            },
            WindowsAndMessaging::{
                EnumWindows, GA_ROOT, GWL_EXSTYLE, GWL_STYLE, GetAncestor, GetClassNameW,
                GetWindowDisplayAffinity, GetWindowLongPtrW, GetWindowRect, GetWindowTextW,
                GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, WS_CHILD,
                WS_EX_TOOLWINDOW,
            },
        },
    },
    core::{BOOL, PWSTR},
};

const MAX_WINDOWS: usize = 128;
// Reserve space for the helper's control envelope within its 65,536-byte lane.
const MAX_LISTING_BYTES: usize = 60 * 1024;
const CLASS_BUFFER_CHARS: usize = 512;
const STILL_ACTIVE: u32 = 259;

pub struct WindowState {
    pub hwnd: HWND,
    pub monitor: HMONITOR,
    pub display: Display,
}

struct PhysicalDpi(DPI_AWARENESS_CONTEXT);
impl PhysicalDpi {
    fn enter() -> Result<Self, String> {
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if previous.0.is_null() {
            Err("Windows could not enable physical window coordinates (per-monitor DPI v2).".into())
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

struct ProcessHandle(HANDLE);
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn filetime(value: FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

fn process_started(handle: &ProcessHandle) -> Result<u64, String> {
    let (mut creation, mut exit, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe { GetProcessTimes(handle.0, &mut creation, &mut exit, &mut kernel, &mut user) }
        .map_err(|_| {
            "Windows could not verify the selected application's process identity.".to_string()
        })?;
    let mut exit_code = 0;
    unsafe { GetExitCodeProcess(handle.0, &mut exit_code) }.map_err(|_| {
        "Windows could not verify whether the selected application is running.".to_string()
    })?;
    // Checking exit FILETIME also handles a terminated process whose exit code is 259.
    if exit_code != STILL_ACTIVE || filetime(exit) != 0 || filetime(creation) == 0 {
        return Err(
            "The selected application has exited. Select its new window explicitly.".into(),
        );
    }
    Ok(filetime(creation))
}

fn open_process(pid: u32) -> Result<ProcessHandle, String> {
    if pid == 0 {
        return Err("Windows did not identify the selected application's process.".into());
    }
    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map(ProcessHandle)
        .map_err(|_| "Windows denied access to the selected application's process identity.".into())
}

fn process_label(handle: &ProcessHandle) -> String {
    let mut buffer = vec![0_u16; 32_768];
    let mut length = buffer.len() as u32;
    if unsafe {
        QueryFullProcessImageNameW(
            handle.0,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    }
    .is_err()
        || length == 0
        || length as usize >= buffer.len()
    {
        return "Application".into();
    }
    let full = String::from_utf16_lossy(&buffer[..length as usize]);
    let basename = full.rsplit(['\\', '/']).next().unwrap_or_default();
    let label = bounded_label(basename, 64);
    if label.is_empty() {
        "Application".into()
    } else {
        label
    }
}

fn bounded_label(value: &str, maximum: usize) -> String {
    let mut result = String::new();
    for character in value.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if result.len() + character.len_utf8() > maximum {
            break;
        }
        result.push(character);
    }
    result.trim().to_owned()
}

fn hwnd_text(hwnd: HWND) -> String {
    format!(
        "{:0width$x}",
        hwnd.0 as usize,
        width = size_of::<usize>() * 2
    )
}

fn parse_hwnd(value: &str) -> Result<HWND, String> {
    let number = usize::from_str_radix(value, 16)
        .map_err(|_| "The selected window identity is invalid.".to_string())?;
    let hwnd = HWND(number as *mut c_void);
    if number == 0 || hwnd_text(hwnd) != value {
        return Err("The selected window identity is not canonical.".into());
    }
    Ok(hwnd)
}

fn class_name(hwnd: HWND) -> Result<String, String> {
    let mut buffer = [0_u16; CLASS_BUFFER_CHARS];
    let length = unsafe { GetClassNameW(hwnd, &mut buffer) };
    if length <= 0 || length as usize >= buffer.len() - 1 {
        return Err(
            "Windows could not verify the selected window's complete class identity.".into(),
        );
    }
    String::from_utf16(&buffer[..length as usize])
        .map_err(|_| "Windows reported an invalid window class identity.".into())
}

fn ordinary_visible_window(hwnd: HWND) -> bool {
    unsafe {
        IsWindow(Some(hwnd)).as_bool()
            && GetAncestor(hwnd, GA_ROOT) == hwnd
            && IsWindowVisible(hwnd).as_bool()
            && GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & WS_CHILD.0 == 0
            && GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 == 0
    }
}

fn window_pid(hwnd: HWND) -> Result<u32, String> {
    let mut pid = 0;
    let thread = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if thread == 0 || pid == 0 {
        return Err("The selected window has closed or changed identity.".into());
    }
    Ok(pid)
}

fn cloaked(hwnd: HWND) -> Result<bool, String> {
    let mut value = 0_u32;
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut value as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    }
    .map_err(|_| {
        "Windows could not verify the selected window's desktop visibility.".to_string()
    })?;
    Ok(value != 0)
}

fn protection(hwnd: HWND) -> &'static str {
    let mut affinity = 0;
    match unsafe { GetWindowDisplayAffinity(hwnd, &mut affinity) } {
        Ok(()) if affinity == 0 => "allowed",
        Ok(()) => "excluded",
        // This query commonly fails for ordinary non-layered foreign windows.
        Err(_) => "unknown",
    }
}

fn monitor_display(hwnd: HWND) -> Result<(HMONITOR, Display), String> {
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.0.is_null() {
        return Err("Windows could not locate the selected window's display.".into());
    }
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    if !unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool() {
        return Err("Windows could not read the selected window's physical display bounds.".into());
    }
    let rect = info.monitorInfo.rcMonitor;
    let width = i64::from(rect.right) - i64::from(rect.left);
    let height = i64::from(rect.bottom) - i64::from(rect.top);
    if !(1..=i64::from(i32::MAX)).contains(&width) || !(1..=i64::from(i32::MAX)).contains(&height) {
        return Err("Windows reported invalid physical display dimensions.".into());
    }
    let length = info
        .szDevice
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(info.szDevice.len());
    let id = String::from_utf16_lossy(&info.szDevice[..length]);
    if id.is_empty() {
        return Err("Windows did not identify the selected window's display.".into());
    }
    Ok((
        monitor,
        Display {
            name: format!(
                "{}{} ({} × {})",
                id,
                if info.monitorInfo.dwFlags & 1 != 0 {
                    " (Primary)"
                } else {
                    ""
                },
                width,
                height
            ),
            id,
            x: rect.left,
            y: rect.top,
            width: width as u32,
            height: height as u32,
        },
    ))
}

fn window_size(hwnd: HWND) -> Result<(u32, u32), String> {
    let mut rect = RECT::default();
    if unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut rect as *mut RECT).cast(),
            size_of::<RECT>() as u32,
        )
    }
    .is_err()
    {
        unsafe { GetWindowRect(hwnd, &mut rect) }.map_err(|_| {
            "Windows could not read the selected window's physical bounds.".to_string()
        })?;
    }
    let width = i64::from(rect.right) - i64::from(rect.left);
    let height = i64::from(rect.bottom) - i64::from(rect.top);
    if !(1..=i64::from(i32::MAX)).contains(&width) || !(1..=i64::from(i32::MAX)).contains(&height) {
        return Err("The selected window has no usable capture bounds.".into());
    }
    Ok((width as u32, height as u32))
}

fn matches_identity(
    hwnd: HWND,
    identity: &WindowIdentity,
    process: &ProcessHandle,
) -> Result<(), String> {
    if !ordinary_visible_window(hwnd)
        || window_pid(hwnd)? != identity.process_id
        || class_name(hwnd)? != identity.class_name
        || format!("{:016x}", process_started(process)?) != identity.process_started
    {
        return Err("The selected window has closed or changed identity. Select it again.".into());
    }
    Ok(())
}

pub fn validate_window(identity: &WindowIdentity) -> Result<WindowState, String> {
    let _dpi = PhysicalDpi::enter()?;
    let hwnd = parse_hwnd(&identity.hwnd)?;
    if identity.process_id == std::process::id()
        || identity.process_started.len() != 16
        || u64::from_str_radix(&identity.process_started, 16)
            .ok()
            .filter(|value| *value != 0 && format!("{value:016x}") == identity.process_started)
            .is_none()
        || identity.class_name.is_empty()
        || identity.class_name.contains('\0')
        || identity.class_name.len() > (CLASS_BUFFER_CHARS - 1) * 3
    {
        return Err("The selected application's window identity is invalid.".into());
    }
    let process = open_process(identity.process_id)?;
    matches_identity(hwnd, identity, &process)?;
    if unsafe { IsIconic(hwnd) }.as_bool() {
        return Err("The selected window is minimized. Restore it before capturing.".into());
    }
    if cloaked(hwnd)? {
        return Err("The selected window is hidden on another desktop.".into());
    }
    if protection(hwnd) == "excluded" {
        return Err("The selected window excludes screen capture.".into());
    }
    let (monitor, display) = monitor_display(hwnd)?;
    // Check usable physical bounds; WGC ContentSize owns the capture canvas.
    window_size(hwnd)?;
    // Reject replacement between geometry/health queries; the caller rechecks
    // again around capture-item construction and during capture.
    matches_identity(hwnd, identity, &process)?;
    if unsafe { IsIconic(hwnd) }.as_bool() || cloaked(hwnd)? || protection(hwnd) == "excluded" {
        return Err("The selected window became unavailable during validation.".into());
    }
    Ok(WindowState {
        hwnd,
        monitor,
        display,
    })
}

fn inspect_window(hwnd: HWND, excluded_pid: u32) -> Option<WindowInfo> {
    if !ordinary_visible_window(hwnd) {
        return None;
    }
    let pid = window_pid(hwnd).ok()?;
    if pid == excluded_pid || pid == std::process::id() {
        return None;
    }
    let mut title = [0_u16; 256];
    let title_length = unsafe { GetWindowTextW(hwnd, &mut title) };
    if title_length <= 0 {
        return None;
    }
    let name = bounded_label(
        &String::from_utf16_lossy(&title[..title_length as usize]),
        128,
    );
    if name.is_empty() {
        return None;
    }
    let process = open_process(pid).ok()?;
    let identity = WindowIdentity {
        hwnd: hwnd_text(hwnd),
        process_id: pid,
        process_started: format!("{:016x}", process_started(&process).ok()?),
        class_name: class_name(hwnd).ok()?,
    };
    let process_name = process_label(&process);
    if process_name.eq_ignore_ascii_case("vfl_recording_host.exe") {
        return None;
    }
    let mut reason = match cloaked(hwnd) {
        Ok(true) => return None,
        Ok(false) => None,
        Err(error) => Some(error),
    };
    if unsafe { IsIconic(hwnd) }.as_bool() {
        reason = Some("Restore this minimized window before capturing.".into());
    }
    let protection = protection(hwnd).to_string();
    if protection == "excluded" {
        reason = Some("This window excludes screen capture.".into());
    }
    let (width, height) = window_size(hwnd).unwrap_or_else(|error| {
        reason.get_or_insert(error);
        (0, 0)
    });
    let display_id = match monitor_display(hwnd) {
        Ok((_, display)) => display.id,
        Err(error) => {
            reason.get_or_insert(error);
            String::new()
        }
    };
    matches_identity(hwnd, &identity, &process).ok()?;
    Some(WindowInfo {
        identity,
        name,
        process_name,
        width,
        height,
        display_id,
        available: reason.is_none(),
        reason,
        protection,
    })
}

fn push_bounded(listing: &mut WindowListing, window: WindowInfo) -> Result<bool, String> {
    if listing.windows.len() >= MAX_WINDOWS {
        listing.truncated = true;
        return Ok(false);
    }
    listing.windows.push(window);
    let size = serde_json::to_vec(&*listing)
        .map_err(|_| "Windows could not serialize the source listing.".to_string())?
        .len();
    if size >= MAX_LISTING_BYTES {
        listing.windows.pop();
        listing.truncated = true;
        return Ok(false);
    }
    Ok(true)
}

struct Enumeration {
    excluded_pid: u32,
    listing: WindowListing,
    error: Option<String>,
}
unsafe extern "system" fn window_callback(hwnd: HWND, parameter: LPARAM) -> BOOL {
    let state = unsafe { &mut *(parameter.0 as *mut Enumeration) };
    // A Rust panic must never unwind through EnumWindows' foreign stack.
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        if let Some(window) = inspect_window(hwnd, state.excluded_pid) {
            push_bounded(&mut state.listing, window)
        } else {
            Ok(true)
        }
    }));
    match result {
        Ok(Ok(more)) => BOOL(i32::from(more)),
        Ok(Err(error)) => {
            state.error = Some(error);
            BOOL(0)
        }
        Err(_) => {
            state.error = Some("Windows source enumeration failed safely.".into());
            BOOL(0)
        }
    }
}

pub fn enumerate_windows(excluded_pid: u32) -> Result<WindowListing, String> {
    let _dpi = PhysicalDpi::enter()?;
    let mut state = Enumeration {
        excluded_pid,
        listing: WindowListing {
            windows: Vec::new(),
            truncated: false,
        },
        error: None,
    };
    let result = unsafe {
        EnumWindows(
            Some(window_callback),
            LPARAM((&mut state as *mut Enumeration) as isize),
        )
    };
    if let Some(error) = state.error {
        return Err(error);
    }
    if result.is_err() && !state.listing.truncated {
        return Err("Windows could not enumerate application windows.".into());
    }
    Ok(state.listing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_bound_utf8_and_remove_control_characters() {
        assert_eq!(bounded_label("ééé", 5), "éé");
        assert_eq!(bounded_label("  app\n\0name  ", 128), "app  name");
        assert_eq!(bounded_label("🪟".repeat(40).as_str(), 128).len(), 128);
    }

    #[test]
    fn hwnd_identity_rejects_aliases_and_null() {
        let canonical = hwnd_text(HWND(std::ptr::dangling_mut::<c_void>()));
        assert_eq!(parse_hwnd(&canonical).unwrap().0 as usize, 1);
        for invalid in ["1", "0x1", "-1", "0000000000000000", "FFFFFFFFFFFFFFFF"] {
            assert!(parse_hwnd(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn listing_bounds_actual_json_escaping_and_entries() {
        let mut listing = WindowListing {
            windows: Vec::new(),
            truncated: false,
        };
        for index in 0..256 {
            let window = WindowInfo {
                identity: WindowIdentity {
                    hwnd: hwnd_text(HWND((index + 1) as *mut c_void)),
                    process_id: 1,
                    process_started: "0000000000000001".into(),
                    class_name: "\"".repeat(500),
                },
                name: "\"".repeat(128),
                process_name: "\\".repeat(64),
                width: 1,
                height: 1,
                display_id: "\\\\.\\DISPLAY1".into(),
                available: true,
                reason: None,
                protection: "unknown".into(),
            };
            if !push_bounded(&mut listing, window).unwrap() {
                break;
            }
        }
        assert!(listing.truncated);
        assert!(!listing.windows.is_empty() && listing.windows.len() < MAX_WINDOWS);
        assert!(serde_json::to_vec(&listing).unwrap().len() < MAX_LISTING_BYTES);
    }

    #[test]
    fn compact_windows_still_stop_at_the_entry_limit() {
        let mut listing = WindowListing {
            windows: Vec::new(),
            truncated: false,
        };
        for index in 0..=MAX_WINDOWS {
            let accepted = push_bounded(
                &mut listing,
                WindowInfo {
                    identity: WindowIdentity {
                        hwnd: hwnd_text(HWND((index + 1) as *mut c_void)),
                        process_id: 1,
                        process_started: "0000000000000001".into(),
                        class_name: "c".into(),
                    },
                    name: "w".into(),
                    process_name: "p".into(),
                    width: 1,
                    height: 1,
                    display_id: "d".into(),
                    available: true,
                    reason: None,
                    protection: "unknown".into(),
                },
            )
            .unwrap();
            assert_eq!(accepted, index < MAX_WINDOWS);
        }
        assert_eq!(listing.windows.len(), MAX_WINDOWS);
        assert!(listing.truncated);
        assert!(serde_json::to_vec(&listing).unwrap().len() < MAX_LISTING_BYTES);
    }
}
