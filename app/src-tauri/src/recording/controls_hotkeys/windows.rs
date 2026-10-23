//! All registrations and preparation windows belong to this one native thread.
//! The main-window subclass only fences a small state object and forwards.
use super::*;
use ::windows::{
    Win32::{
        Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::{
            LibraryLoader::GetModuleHandleW,
            StationsAndDesktops::{
                CloseDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, GetThreadDesktop,
                GetUserObjectInformationW, OpenInputDesktop, UOI_NAME,
            },
            Threading::GetCurrentThreadId,
        },
        UI::{
            Input::KeyboardAndMouse::*,
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
    core::{PCWSTR, w},
};
use registry::{RegisterError, Registrar, Registry, TargetKey};
use std::{
    sync::Weak,
    time::{Duration, Instant},
};
const SUBCLASS: usize = 0x56464c43;
fn hwnd(value: usize) -> HWND {
    HWND(value as *mut _)
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
pub(super) fn classify(value: usize) -> NativeWindow {
    let h = hwnd(value);
    unsafe {
        if value == 0 || !IsWindow(Some(h)).as_bool() {
            NativeWindow::Unavailable
        } else if IsIconic(h).as_bool() {
            NativeWindow::Minimized
        } else if !IsWindowVisible(h).as_bool() {
            NativeWindow::Hidden
        } else {
            NativeWindow::Visible
        }
    }
}
struct Observer {
    shared: Weak<Shared>,
}
unsafe extern "system" fn observer(
    h: HWND,
    message: u32,
    wp: WPARAM,
    lp: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    // The stable allocation belongs to this exact subclass until NCDESTROY.
    let reference = unsafe { &*(data as *const Observer) };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Some(shared) = reference.shared.upgrade()
            && let Ok(mut g) = shared.gate.lock()
        {
            if g.hwnd != h.0 as usize {
                return;
            }
            if message == WM_SYSCOMMAND
                && (wp.0 & 0xfff0) == SC_RESTORE as usize
                && g.window == NativeWindow::Minimized
            {
                // Fence before Tao/DefSubclassProc can forward a restore transition.
                g.observe(NativeWindow::Visible, Instant::now());
            } else if message == WM_SIZE {
                let next = if wp.0 as u32 == SIZE_MINIMIZED {
                    NativeWindow::Minimized
                } else if unsafe { IsWindowVisible(h) }.as_bool() {
                    NativeWindow::Visible
                } else {
                    NativeWindow::Hidden
                };
                g.observe(next, Instant::now());
            } else if message == WM_SHOWWINDOW {
                let next = if wp.0 == 0 {
                    NativeWindow::Hidden
                } else {
                    classify(h.0 as usize)
                };
                g.observe(next, Instant::now());
            } else if message == WM_CLOSE {
                g.invalidate("Window close requested.");
            } else if message == WM_NCDESTROY {
                g.observer_live = false;
                g.hwnd = 0;
                g.observe(NativeWindow::Unavailable, Instant::now());
            }
        }
    }));
    if message == WM_NCDESTROY {
        let removed = unsafe { RemoveWindowSubclass(h, Some(observer), id) }.as_bool();
        let result = unsafe { DefSubclassProc(h, message, wp, lp) };
        // No callback can use this reference once this subclass is removed.
        if removed {
            drop(unsafe { Box::from_raw(data as *mut Observer) });
        } else if let Some(shared) = reference.shared.upgrade()
            && let Ok(mut error) = shared.cleanup_error.lock()
        {
            *error = Some(
                "Main-window subclass removal was not confirmed; callback allocation retained."
                    .into(),
            );
        }
        result
    } else {
        unsafe { DefSubclassProc(h, message, wp, lp) }
    }
}
pub(super) fn initialize(
    manager: ControlsManager,
    main: &WebviewWindow,
    bindings: Bindings,
    store: prefs::FileStore,
) -> Result<(), String> {
    let native = main.hwnd().map_err(|e| e.to_string())?;
    let h = HWND(native.0);
    let thread = unsafe { GetCurrentThreadId() };
    if unsafe { GetWindowThreadProcessId(h, None) } != thread {
        return Err("Main-window observer must initialize on its UI thread.".into());
    }
    let observer_data = Box::into_raw(Box::new(Observer {
        shared: Arc::downgrade(&manager.shared),
    }));
    if !unsafe { SetWindowSubclass(h, Some(observer), SUBCLASS, observer_data as usize) }.as_bool()
    {
        drop(unsafe { Box::from_raw(observer_data) });
        return Err("Main-window shortcut observer could not be installed.".into());
    }
    {
        let mut g = manager
            .shared
            .gate
            .lock()
            .map_err(|_| "Shortcut state unavailable.")?;
        g.hwnd = h.0 as usize;
        g.ui_thread = thread;
        g.observer_live = true;
        g.window = classify(g.hwnd);
    }
    let (tx, rx) = mpsc::sync_channel(32);
    let (ready_tx, ready_rx) = mpsc::channel();
    *manager
        .shared
        .commands
        .lock()
        .map_err(|_| "Shortcut channel unavailable.")? = Some(tx);
    let cloned = manager.clone();
    let worker = std::thread::Builder::new()
        .name("recording-shortcuts".into())
        .spawn(move || owner(cloned, rx, ready_tx, bindings, store))
        .map_err(|e| e.to_string());
    let result = match worker {
        Ok(worker) => {
            *manager
                .shared
                .worker
                .lock()
                .map_err(|_| "Shortcut worker unavailable.")? = Some(worker);
            ready_rx
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| "Shortcut owner thread did not initialize.".to_string())
                .and_then(|r| r)
        }
        Err(e) => Err(e),
    };
    if result.is_err() {
        manager
            .shared
            .worker_stopping
            .store(true, Ordering::Release);
        let removed = unsafe { RemoveWindowSubclass(h, Some(observer), SUBCLASS) }.as_bool();
        if removed {
            drop(unsafe { Box::from_raw(observer_data) });
            if let Ok(mut g) = manager.shared.gate.lock() {
                g.observer_live = false;
                g.hwnd = 0;
            }
        }
        if let Ok(mut c) = manager.shared.commands.lock() {
            c.take();
        }
    }
    result
}
fn input_desktop() -> Result<(), String> {
    unsafe {
        let desktop = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS)
            .map_err(|_| "Key-release proof requires the interactive desktop.".to_string())?;
        let mut buffer = [0u16; 128];
        let mut needed = 0;
        let read = GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(buffer.as_mut_ptr().cast()),
            256,
            Some(&mut needed),
        );
        let _ = CloseDesktop(desktop);
        read.map_err(|e| e.to_string())?;
        let end = buffer
            .iter()
            .position(|x| *x == 0)
            .ok_or("Desktop name is not bounded.")?;
        if needed > 256 || String::from_utf16_lossy(&buffer[..end]) != "Default" {
            return Err("Key-release proof is unavailable on this desktop.".into());
        }
        let thread_desktop = GetThreadDesktop(GetCurrentThreadId()).map_err(|e| e.to_string())?;
        let mut own = [0u16; 128];
        let mut own_needed = 0;
        GetUserObjectInformationW(
            HANDLE(thread_desktop.0),
            UOI_NAME,
            Some(own.as_mut_ptr().cast()),
            256,
            Some(&mut own_needed),
        )
        .map_err(|e| e.to_string())?;
        if own_needed > 256 || own != buffer {
            return Err("Shortcut owner is not on the interactive desktop.".into());
        }
        Ok(())
    }
}
pub(super) fn normalize(ui_thread: u32, event: KeyCapture) -> Result<Chord, String> {
    let layout = unsafe { GetKeyboardLayout(ui_thread) };
    if layout.0.is_null() {
        return Err("The main-window keyboard layout is unavailable.".into());
    }
    super::keymap::normalize(event, |character| {
        Ok(unsafe { VkKeyScanExW(character, layout) })
    })
}

struct WinRegistrar {
    window: HWND,
}
impl Registrar for WinRegistrar {
    fn register(&mut self, id: i32, chord: &Chord) -> Result<(), RegisterError> {
        unsafe {
            RegisterHotKey(
                Some(self.window),
                id,
                HOT_KEY_MODIFIERS(chord.modifier_bits()) | MOD_NOREPEAT,
                chord.virtual_key().map_err(|e| RegisterError {
                    conflict: false,
                    message: e,
                })?,
            )
        }
        .map_err(|e| RegisterError {
            conflict: e.code().0 as u32 == 0x80070581,
            message: format!("Shortcut could not be registered: {e}"),
        })
    }
    fn unregister(&mut self, id: i32) -> Result<(), String> {
        unsafe { UnregisterHotKey(Some(self.window), id) }.map_err(|e| e.to_string())
    }
    fn primary_released(&self, chord: &Chord) -> Result<bool, String> {
        input_desktop()?;
        Ok(unsafe { GetAsyncKeyState(chord.virtual_key()? as i32) } >= 0)
    }
    fn all_released(&self) -> Result<bool, String> {
        input_desktop()?;
        let keys = (0x30..=0x5a).chain(0x70..=0x87).chain([
            0x10, 0x11, 0x12, 0x5b, 0x5c, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5,
        ]);
        Ok(keys
            .into_iter()
            .all(|key| unsafe { GetAsyncKeyState(key) } >= 0))
    }
}
struct WindowState {
    shared: Weak<Shared>,
    cancel: Option<Arc<AtomicBool>>,
}
unsafe extern "system" fn window_proc(h: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if message == WM_NCCREATE {
        let create = unsafe { &*(lp.0 as *const CREATESTRUCTW) };
        unsafe { SetWindowLongPtrW(h, GWLP_USERDATA, create.lpCreateParams as isize) };
    }
    let pointer = unsafe { GetWindowLongPtrW(h, GWLP_USERDATA) } as *const WindowState;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if !pointer.is_null() {
            let state = unsafe { &*pointer };
            if message == WM_HOTKEY {
                if let Some(shared) = state.shared.upgrade() {
                    shared.key(
                        wp.0 as i32,
                        (lp.0 as u32) & 0xffff,
                        ((lp.0 as u32) >> 16) & 0xffff,
                    );
                }
                return Some(LRESULT(0));
            }
            if message == WM_MOUSEACTIVATE && state.cancel.is_some() {
                return Some(LRESULT(MA_NOACTIVATE as isize));
            }
            if (message == WM_CLOSE || (message == WM_COMMAND && (wp.0 & 0xffff) == 100))
                && let Some(cancel) = &state.cancel
            {
                cancel.store(true, Ordering::Release);
                return Some(LRESULT(0));
            }
        }
        None
    }));
    if message == WM_NCDESTROY {
        unsafe { SetWindowLongPtrW(h, GWLP_USERDATA, 0) };
    }
    match result {
        Ok(Some(value)) => value,
        _ => unsafe { DefWindowProcW(h, message, wp, lp) },
    }
}
struct OwnedWindow {
    h: HWND,
    state: Option<Box<WindowState>>,
}
impl Drop for OwnedWindow {
    fn drop(&mut self) {
        if unsafe { IsWindow(Some(self.h)) }.as_bool() {
            let destroyed = unsafe { DestroyWindow(self.h) };
            if (destroyed.is_err() || unsafe { IsWindow(Some(self.h)) }.as_bool())
                && let Some(state) = self.state.take()
            {
                if let Some(shared) = state.shared.upgrade()
                    && let Ok(mut error) = shared.cleanup_error.lock()
                {
                    *error=Some("A shortcut window did not confirm destruction; callback state retained safely.".into());
                }
                let _ = Box::leak(state);
            }
        }
    }
}
struct Class {
    name: Vec<u16>,
    instance: HINSTANCE,
}
impl Class {
    fn new() -> Result<Self, String> {
        let instance = HINSTANCE(
            unsafe { GetModuleHandleW(None) }
                .map_err(|e| e.to_string())?
                .0,
        );
        let name = wide(&format!(
            "VFLRecordingControls-{}-{}",
            std::process::id(),
            unsafe { GetCurrentThreadId() }
        ));
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: PCWSTR(name.as_ptr()),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&wc) } == 0 {
            return Err("Shortcut native window class could not be registered.".into());
        }
        Ok(Self { name, instance })
    }
    fn window(
        &self,
        shared: Weak<Shared>,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<OwnedWindow, String> {
        let hud = cancel.is_some();
        let mut state = Box::new(WindowState { shared, cancel });
        let h = unsafe {
            CreateWindowExW(
                if hud {
                    WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST
                } else {
                    WINDOW_EX_STYLE(0)
                },
                PCWSTR(self.name.as_ptr()),
                w!("Recording preparation - Cancel to stop"),
                if hud {
                    WS_CAPTION | WS_SYSMENU
                } else {
                    WINDOW_STYLE(0)
                },
                24,
                24,
                380,
                110,
                if hud { None } else { Some(HWND_MESSAGE) },
                None,
                Some(self.instance),
                Some((&mut *state as *mut WindowState).cast()),
            )
        }
        .map_err(|e| e.to_string())?;
        let owned = OwnedWindow {
            h,
            state: Some(state),
        };
        if hud {
            unsafe { SetWindowDisplayAffinity(h, WINDOW_DISPLAY_AFFINITY(0x11)) }
                .map_err(|e| format!("Preparation HUD capture exclusion failed: {e}"))?;
            unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("BUTTON"),
                    w!("Cancel recording"),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                    16,
                    16,
                    330,
                    34,
                    Some(h),
                    Some(HMENU(100 as *mut _)),
                    Some(self.instance),
                    None,
                )
            }
            .map_err(|e| e.to_string())?;
            let _ = unsafe { ShowWindow(h, SW_SHOWNOACTIVATE) };
            if !unsafe { IsWindowVisible(h) }.as_bool() {
                return Err("Recording preparation Cancel HUD could not be shown.".into());
            }
        }
        Ok(owned)
    }
}
impl Drop for Class {
    fn drop(&mut self) {
        let _ = unsafe { UnregisterClassW(PCWSTR(self.name.as_ptr()), Some(self.instance)) };
    }
}
struct Hud {
    token: String,
    _window: OwnedWindow,
}
fn owner(
    manager: ControlsManager,
    rx: mpsc::Receiver<WorkerCommand>,
    ready: mpsc::Sender<Result<(), String>>,
    bindings: Bindings,
    mut store: prefs::FileStore,
) -> Result<(), String> {
    let init = (|| {
        let class = Class::new()?;
        let window = class.window(Arc::downgrade(&manager.shared), None)?;
        Ok::<_, String>((class, window))
    })();
    let (class, window) = match init {
        Ok(parts) => {
            let _ = ready.send(Ok(()));
            parts
        }
        Err(e) => {
            let _ = ready.send(Err(e.clone()));
            return Err(e);
        }
    };
    let mut registry = Registry::new(WinRegistrar { window: window.h }, bindings);
    let mut mutations = service::Mutations::new();
    let mut hud: Option<Hud> = None;
    let mut message = MSG::default();
    let mut last_emit = Instant::now();
    let mut emitted_revision = u64::MAX;
    while !manager.shared.shutting_down.load(Ordering::Acquire)
        && !manager.shared.worker_stopping.load(Ordering::Acquire)
    {
        // Bound each queue so command floods cannot starve native hotkeys/Stop.
        for _ in 0..64 {
            if !unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
                break;
            }
            let _ = unsafe { TranslateMessage(&message) };
            unsafe { DispatchMessageW(&message) };
        }
        for _ in 0..16 {
            match rx.try_recv() {
                Ok(WorkerCommand::Request(request, reply)) => {
                    let result = mutations.execute(&manager, &mut registry, &mut store, *request);
                    let _ = reply.send(result);
                }
                Ok(WorkerCommand::HudVisible { attempt, session }) => {
                    let matching = manager
                        .shared
                        .gate
                        .lock()
                        .map(|g| {
                            g.attempt.as_ref().is_some_and(|a| {
                                a.token == attempt && a.session_id.as_deref() == Some(&session)
                            }) && g.session.as_ref().is_some_and(|s| s.session_id == session)
                        })
                        .unwrap_or(false);
                    if matching && hud.as_ref().is_some_and(|h| h.token == attempt) {
                        hud.take();
                    }
                }
                Ok(WorkerCommand::Finish { attempt }) => {
                    if hud.as_ref().is_some_and(|h| h.token == attempt) {
                        hud.take();
                    }
                }
                Ok(WorkerCommand::Refresh) => {}
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    manager.signal_shutdown();
                    break;
                }
            }
        }
        mutations.settle_assignment(&manager, &registry);
        let (targets, suppressed) = manager
            .shared
            .gate
            .lock()
            .map(|mut g| {
                g.expire(Instant::now());
                (
                    Shared::targets(&g),
                    g.stopped || (g.assignment_suppressed && g.session.is_none()),
                )
            })
            .unwrap_or((BTreeMap::new(), true));
        registry.refresh(&targets, suppressed);
        manager.shared.publish(&registry);
        for _ in 0..4 {
            let next = manager
                .shared
                .pending
                .lock()
                .ok()
                .and_then(|mut q| q.pop_front());
            let Some(entry) = next else { break };
            dispatch(&manager, &class, &mut hud, entry);
        }
        // A failed validation/terminal retires only its exact HUD. An admitted attempt
        // retains Cancel until the helper's explicit Stop-HUD-visible acknowledgement.
        let keep = manager
            .shared
            .gate
            .lock()
            .ok()
            .and_then(|g| g.attempt.as_ref().map(|a| a.token.clone()));
        if hud
            .as_ref()
            .is_some_and(|h| Some(&h.token) != keep.as_ref())
        {
            hud.take();
        }
        let revision = manager.shared.gate.lock().map(|g| g.revision).unwrap_or(0);
        if revision != emitted_revision && last_emit.elapsed() >= Duration::from_millis(50) {
            manager.shared.emit();
            emitted_revision = revision;
            last_emit = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    let cleanup = registry.disable_all();
    manager.shared.publish(&registry);
    hud.take();
    drop(window);
    drop(class);
    cleanup
}
fn dispatch(
    manager: &ControlsManager,
    class: &Class,
    hud: &mut Option<Hud>,
    entry: registry::Entry,
) {
    let backend = manager.shared.backend.lock().ok().and_then(|b| b.clone());
    let Some(backend) = backend else { return };
    let prepared = (|| -> Result<ControlIntent, String> {
        let mut g = manager
            .shared
            .gate
            .lock()
            .map_err(|_| "Shortcut state unavailable.")?;
        let current = classify(g.hwnd);
        g.observe(current, Instant::now());
        g.expire(Instant::now());
        if !Shared::current_entry(&g, &entry) {
            return Err("Queued shortcut context changed.".into());
        }
        match entry.action {
            Action::Start => {
                let TargetKey::Start {
                    token: arm_token, ..
                } = &entry.target.key
                else {
                    return Err("Start context unavailable.".into());
                };
                g.advance_start()?;
                let token = g.token()?;
                let mut config = g
                    .arm
                    .as_ref()
                    .ok_or("Start draft unavailable.")?
                    .config
                    .clone();
                config.request_id = token.clone();
                let cancel = Arc::new(AtomicBool::new(false));
                let permit = gate::StartPermit {
                    shared: Arc::downgrade(&manager.shared),
                    token: token.clone(),
                    arm_token: arm_token.clone(),
                    epoch: g.invalidation_epoch,
                    cycle: g.minimize_cycle,
                    owner: g.owner.clone(),
                    ui: false,
                    consumed: Arc::new(AtomicBool::new(false)),
                    cancel: cancel.clone(),
                };
                g.attempt = Some(gate::AttemptState {
                    token: token.clone(),
                    session_id: None,
                    cancel: cancel.clone(),
                    admitted: false,
                    hud_visible: false,
                });
                g.bump();
                Ok(ControlIntent::Start(Box::new(StartAttempt {
                    token,
                    config,
                    permit,
                    cancel,
                })))
            }
            Action::Pause | Action::Resume => {
                let TargetKey::Transition {
                    owner,
                    session,
                    revision,
                } = &entry.target.key
                else {
                    return Err("Session context unavailable.".into());
                };
                let (owner_token, session_id, control_revision) =
                    (owner.clone(), session.clone(), *revision);
                let operation_id = g.token()?;
                if entry.action == Action::Pause {
                    Ok(ControlIntent::Pause {
                        owner_token,
                        operation_id,
                        session_id,
                        control_revision,
                    })
                } else {
                    Ok(ControlIntent::Resume {
                        owner_token,
                        operation_id,
                        session_id,
                        control_revision,
                    })
                }
            }
            Action::Stop => Err("Stop is dispatched directly to its exact-session latch.".into()),
        }
    })();
    let intent = match prepared {
        Ok(intent) => intent,
        Err(_) => return,
    };
    if let ControlIntent::Start(attempt) = &intent {
        // Show a real independent Cancel path before any backend resolution/work.
        let window = match class.window(
            Arc::downgrade(&manager.shared),
            Some(attempt.cancel.clone()),
        ) {
            Ok(window) => window,
            Err(e) => {
                manager.fail_attempt(&attempt.token, &e);
                return;
            }
        };
        let eligible = manager
            .shared
            .gate
            .lock()
            .map(|mut g| {
                let current = classify(g.hwnd);
                g.observe(current, Instant::now());
                if let Some(a) = g
                    .attempt
                    .as_mut()
                    .filter(|a| a.token == attempt.token && !a.cancel.load(Ordering::Acquire))
                {
                    a.hud_visible = true;
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
        if !eligible {
            drop(window);
            manager.finish_attempt(&attempt.token);
            return;
        }
        *hud = Some(Hud {
            token: attempt.token.clone(),
            _window: window,
        });
    }
    let token = match &intent {
        ControlIntent::Start(a) => Some(a.token.clone()),
        _ => None,
    };
    if let Err(e) = backend.dispatch(intent) {
        if let Some(token) = token {
            manager.fail_attempt(&token, &e);
        } else if let Ok(mut g) = manager.shared.gate.lock()
            && Shared::targets(&g)
                .get(&entry.action)
                .is_some_and(|t| t.key == entry.target.key)
        {
            g.message = Some(bounded_message(e));
            g.bump();
        }
    }
}
