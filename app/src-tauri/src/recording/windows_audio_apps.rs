//! Application discovery and process-loopback activation. All COM clients remain
//! on the caller's MTA. Enumeration never initializes or starts an audio stream.
use super::audio_types::{AudioApplicationInfo, AudioApplicationListing, ProcessIdentity};
use std::{
    collections::BTreeMap,
    ffi::c_void,
    marker::PhantomData,
    mem::{ManuallyDrop, size_of},
    panic::AssertUnwindSafe,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use windows::{
    Wdk::System::SystemServices::RtlGetVersion,
    Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE, HWND, LPARAM, WAIT_TIMEOUT},
        Media::Audio::{
            AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
            AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
            ActivateAudioInterfaceAsync, AudioSessionStateExpired, DEVICE_STATE_ACTIVE,
            IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
            IActivateAudioInterfaceCompletionHandler_Vtbl, IAudioClient, IAudioSessionControl2,
            IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator,
            PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, eRender,
        },
        System::{
            Com::BLOB,
            Com::{
                APTTYPE, APTTYPE_MTA, APTTYPEQUALIFIER, CLSCTX_ALL, CoCreateInstance,
                CoGetApartmentType,
                StructuredStorage::{
                    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
                },
            },
            SystemInformation::OSVERSIONINFOW,
            Threading::{
                GetCurrentProcessId, GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW,
                WaitForSingleObject,
            },
            Variant::VT_BLOB,
        },
        UI::WindowsAndMessaging::{
            EnumWindows, GA_ROOT, GWL_EXSTYLE, GWL_STYLE, GetAncestor, GetWindowLongPtrW,
            GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, WS_CHILD, WS_EX_TOOLWINDOW,
        },
    },
    core::{BOOL, GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface, PWSTR},
};

const MAX_APPLICATIONS: usize = 128;
const MAX_LISTING_BYTES: usize = 60 * 1024;
const MIN_PROCESS_LOOPBACK_BUILD: u32 = 20_348;
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(5);
// A timed-out activation cannot be cancelled. Keep its single bundle until host
// exit, and refuse retries in this host. The supervised recording host treats
// this error as fatal; its callback can only notify, never Initialize/Start.
static ACTIVATION_RESERVED: AtomicBool = AtomicBool::new(false);

fn support_reason(major: u32, build: u32) -> Option<String> {
    (major < 10 || build < MIN_PROCESS_LOOPBACK_BUILD).then(|| {
        "Application audio requires Windows build 20348 or newer. Choose System audio explicitly on this Windows version.".into()
    })
}

/// This coarse OS check is not a promise that activation will succeed. Windows
/// can still deny activation or report that this API is unavailable at runtime.
pub fn capability() -> Result<Option<String>, String> {
    let mut version = OSVERSIONINFOW {
        dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    unsafe { RtlGetVersion(&mut version) }
        .ok()
        .map_err(|_| "Windows could not determine application-audio support.".to_string())?;
    Ok(support_reason(
        version.dwMajorVersion,
        version.dwBuildNumber,
    ))
}

fn filetime(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

fn require_mta() -> Result<(), String> {
    let mut apartment = APTTYPE::default();
    let mut qualifier = APTTYPEQUALIFIER::default();
    unsafe { CoGetApartmentType(&mut apartment, &mut qualifier) }
        .map_err(|_| "Application audio requires an initialized recording MTA.".to_string())?;
    if apartment != APTTYPE_MTA {
        return Err("Application audio must run on the recording MTA.".into());
    }
    Ok(())
}

fn parse_creation(value: &str) -> Result<u64, String> {
    if value.len() != 16 {
        return Err("The selected application identity is invalid.".into());
    }
    let parsed = u64::from_str_radix(value, 16)
        .ok()
        .filter(|v| *v != 0 && format!("{v:016x}") == value);
    parsed.ok_or_else(|| "The selected application identity is invalid.".into())
}

/// The retained handle prevents PID reuse from silently selecting another app.
/// Rc marker deliberately prevents moving either the guard or its client owner
/// across apartments/threads, including when Windows bindings permit Send.
pub struct ProcessGuard {
    handle: HANDLE,
    creation: u64,
    _owner_thread: PhantomData<Rc<()>>,
}
impl Drop for ProcessGuard {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.handle) };
    }
}
impl ProcessGuard {
    fn open(pid: u32) -> Result<Self, String> {
        if pid == 0 {
            return Err("The selected application has no valid process.".into());
        }
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                false,
                pid,
            )
        }
        .map_err(|_| "Windows denied access to the selected application's identity.".to_string())?;
        let mut guard = Self {
            handle,
            creation: 0,
            _owner_thread: PhantomData,
        };
        guard.creation = guard.read_creation()?;
        guard.verify()?;
        Ok(guard)
    }
    fn read_creation(&self) -> Result<u64, String> {
        let (mut creation, mut exit, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        unsafe {
            GetProcessTimes(
                self.handle,
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        }
        .map_err(|_| "Windows could not verify the selected application's identity.".to_string())?;
        if filetime(exit) != 0 || filetime(creation) == 0 {
            return Err(
                "The selected application has exited. Select its new process explicitly.".into(),
            );
        }
        Ok(filetime(creation))
    }
    pub fn verify(&self) -> Result<(), String> {
        if unsafe { WaitForSingleObject(self.handle, 0) } != WAIT_TIMEOUT
            || self.read_creation()? != self.creation
        {
            return Err("The selected application has exited or changed identity.".into());
        }
        Ok(())
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

fn process_label(guard: &ProcessGuard) -> String {
    let mut buffer = vec![0_u16; 32_768];
    let mut length = buffer.len() as u32;
    if unsafe {
        QueryFullProcessImageNameW(
            guard.handle,
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
    // Full image path is local only; never serialize it or command-line data.
    let full = String::from_utf16_lossy(&buffer[..length as usize]);
    let label = bounded_label(full.rsplit(['\\', '/']).next().unwrap_or_default(), 96);
    if label.is_empty() {
        "Application".into()
    } else {
        label
    }
}

struct Candidates {
    names: BTreeMap<u32, (String, u64)>,
    excluded: [u32; 2],
    visited: usize,
    truncated: bool,
}
impl Candidates {
    fn insert(&mut self, pid: u32, name: String, creation: u64) {
        if pid == 0 || self.excluded.contains(&pid) || self.names.contains_key(&pid) {
            return;
        }
        if self.names.len() == MAX_APPLICATIONS {
            self.truncated = true;
            return;
        }
        self.names.insert(pid, (name, creation));
    }
}

unsafe extern "system" fn collect_window(hwnd: HWND, parameter: LPARAM) -> BOOL {
    // Never unwind through EnumWindows. No pointers or window identities escape.
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| unsafe {
        let state = &mut *(parameter.0 as *mut Candidates);
        state.visited += 1;
        if state.visited > 4096 {
            state.truncated = true;
            return false;
        }
        if !IsWindowVisible(hwnd).as_bool()
            || GetAncestor(hwnd, GA_ROOT) != hwnd
            || GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & WS_CHILD.0 != 0
            || GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0
        {
            return true;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 || state.excluded.contains(&pid) {
            return true;
        }
        let Ok(guard) = ProcessGuard::open(pid) else {
            state.truncated = true;
            return true;
        };
        let mut title = [0_u16; 257];
        let length = GetWindowTextW(hwnd, &mut title).max(0) as usize;
        let mut fresh_pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut fresh_pid));
        if pid != fresh_pid || guard.verify().is_err() {
            state.truncated = true;
            return true;
        }
        if length > 0 {
            state.insert(
                pid,
                bounded_label(&String::from_utf16_lossy(&title[..length]), 256),
                guard.creation,
            );
        }
        true
    }));
    BOOL::from(result.unwrap_or(false))
}

fn collect_sessions(state: &mut Candidates) -> Result<(), String> {
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(|_| "Windows audio-session discovery is unavailable.".to_string())?;
    let endpoints = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }
        .map_err(|_| "Windows could not enumerate audio applications.".to_string())?;
    let count = unsafe { endpoints.GetCount() }
        .map_err(|_| "Windows could not count audio endpoints.".to_string())?;
    if count > 64 {
        state.truncated = true;
    }
    for index in 0..count.min(64) {
        let result = (|| -> windows::core::Result<()> {
            let endpoint = unsafe { endpoints.Item(index)? };
            let manager: IAudioSessionManager2 = unsafe { endpoint.Activate(CLSCTX_ALL, None)? };
            let sessions = unsafe { manager.GetSessionEnumerator()? };
            let count = unsafe { sessions.GetCount()? };
            if !(0..=1024).contains(&count) {
                state.truncated = true;
            }
            for index in 0..count.clamp(0, 1024) {
                let session = unsafe { sessions.GetSession(index)? };
                if unsafe { session.GetState()? } == AudioSessionStateExpired {
                    continue;
                }
                let session: IAudioSessionControl2 = session.cast()?;
                let pid = unsafe { session.GetProcessId()? };
                if pid == 0 || state.excluded.contains(&pid) || state.names.contains_key(&pid) {
                    continue;
                }
                let Ok(guard) = ProcessGuard::open(pid) else {
                    state.truncated = true;
                    continue;
                };
                state.insert(pid, String::new(), guard.creation);
            }
            Ok(())
        })();
        // Keep silent window applications discoverable if a single audio device
        // disappears concurrently. Explicit truncation discloses an incomplete list.
        if result.is_err() {
            state.truncated = true;
        }
    }
    Ok(())
}

fn fit_listing(mut listing: AudioApplicationListing) -> Result<AudioApplicationListing, String> {
    while serde_json::to_vec(&listing)
        .map_err(|_| "Could not describe audio applications.".to_string())?
        .len()
        > MAX_LISTING_BYTES
    {
        listing.truncated = true;
        if listing.applications.pop().is_none() {
            return Err("Audio application description exceeds its limit.".into());
        }
    }
    Ok(listing)
}

/// Requires COM initialized on the caller's MTA, but never activates IAudioClient.
pub fn enumerate_applications(excluded_pid: u32) -> Result<AudioApplicationListing, String> {
    require_mta()?;
    let unsupported = capability()?;
    let mut candidates = Candidates {
        names: BTreeMap::new(),
        excluded: [unsafe { GetCurrentProcessId() }, excluded_pid],
        visited: 0,
        truncated: false,
    };
    let result = unsafe {
        EnumWindows(
            Some(collect_window),
            LPARAM(&mut candidates as *mut _ as isize),
        )
    };
    if result.is_err() && !candidates.truncated {
        return Err("Windows could not enumerate application windows.".into());
    }
    collect_sessions(&mut candidates)?;
    let mut applications = Vec::new();
    for (pid, (title, creation)) in candidates.names {
        let Ok(guard) = ProcessGuard::open(pid) else {
            candidates.truncated = true;
            continue;
        };
        if guard.creation != creation {
            candidates.truncated = true;
            continue;
        }
        let process_name = process_label(&guard);
        applications.push(AudioApplicationInfo {
            identity: ProcessIdentity {
                process_id: pid,
                process_started: format!("{:016x}", guard.creation),
            },
            name: if title.is_empty() {
                process_name.clone()
            } else {
                title
            },
            process_name,
            available: unsupported.is_none(),
            reason: unsupported.clone(),
        });
    }
    applications.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.identity.process_id.cmp(&b.identity.process_id))
    });
    fit_listing(AudioApplicationListing {
        applications,
        truncated: candidates.truncated,
    })
}

// Minimal agile COM callback: no apartment-bound object is accessed on the RPC
// callback thread. FTM support uses the same pinned windows-core marshaler as its
// implement macro, with the outer IUnknown retained for the marshaler's lifetime.
#[repr(C)]
struct Completion {
    vtable: &'static IActivateAudioInterfaceCompletionHandler_Vtbl,
    completed: Arc<AtomicBool>,
}
impl Completion {
    const VTABLE: IActivateAudioInterfaceCompletionHandler_Vtbl =
        IActivateAudioInterfaceCompletionHandler_Vtbl {
            base__: IUnknown_Vtbl {
                QueryInterface: Self::query,
                AddRef: Self::add_ref,
                Release: Self::release,
            },
            ActivateCompleted: Self::completed,
        };
    fn create(completed: Arc<AtomicBool>) -> IActivateAudioInterfaceCompletionHandler {
        let raw = Arc::into_raw(Arc::new(Self {
            vtable: &Self::VTABLE,
            completed,
        }));
        unsafe { IActivateAudioInterfaceCompletionHandler::from_raw(raw.cast_mut().cast()) }
    }
    unsafe extern "system" fn query(
        this: *mut c_void,
        iid: *const GUID,
        result: *mut *mut c_void,
    ) -> HRESULT {
        const E_POINTER: HRESULT = HRESULT(0x80004003_u32 as i32);
        if iid.is_null() || result.is_null() {
            return E_POINTER;
        }
        unsafe {
            *result = std::ptr::null_mut();
        }
        let iid = unsafe { *iid };
        if iid == IUnknown::IID
            || iid == IActivateAudioInterfaceCompletionHandler::IID
            || iid == windows::core::imp::IAgileObject::IID
        {
            unsafe {
                Self::add_ref(this);
                *result = this;
            }
            return HRESULT(0);
        }
        if iid == windows::core::imp::IMarshal::IID {
            return std::panic::catch_unwind(AssertUnwindSafe(|| unsafe {
                Self::add_ref(this);
                windows::core::imp::marshaler(IUnknown::from_raw(this), result)
            }))
            .unwrap_or(HRESULT(0x8007000E_u32 as i32));
        }
        HRESULT(0x80004002_u32 as i32)
    }
    unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
        unsafe {
            Arc::increment_strong_count(this.cast::<Self>());
        }
        let arc = ManuallyDrop::new(unsafe { Arc::from_raw(this.cast::<Self>()) });
        Arc::strong_count(&arc).min(u32::MAX as usize) as u32
    }
    unsafe extern "system" fn release(this: *mut c_void) -> u32 {
        let arc = unsafe { Arc::from_raw(this.cast::<Self>()) };
        let count = Arc::strong_count(&arc)
            .saturating_sub(1)
            .min(u32::MAX as usize) as u32;
        drop(arc);
        count
    }
    unsafe extern "system" fn completed(this: *mut c_void, _operation: *mut c_void) -> HRESULT {
        unsafe { &*this.cast::<Self>() }
            .completed
            .store(true, Ordering::Release);
        HRESULT(0)
    }
}

pub struct ActivatedProcess {
    pub client: IAudioClient,
    pub guard: ProcessGuard,
}

// Both addresses remain stable for the entire asynchronous operation, including
// the deliberately retained timeout/Stop path. PROPVARIANT itself has a Drop
// extension in windows0.61.3 that calls PropVariantClear, so its OUTER value must
// be ManuallyDrop: the blob borrows a Rust Box, not COM-allocated owned memory.
struct ActivationParameters {
    variant: Box<ManuallyDrop<PROPVARIANT>>,
    _parameters: Box<AUDIOCLIENT_ACTIVATION_PARAMS>,
}
impl ActivationParameters {
    fn new(process_id: u32) -> Self {
        let mut parameters = Box::new(AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: process_id,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        });
        let variant = Box::new(ManuallyDrop::new(PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_BLOB,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: PROPVARIANT_0_0_0 {
                        blob: BLOB {
                            cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                            pBlobData: (&mut *parameters as *mut AUDIOCLIENT_ACTIVATION_PARAMS)
                                .cast(),
                        },
                    },
                }),
            },
        }));
        Self {
            variant,
            _parameters: parameters,
        }
    }
    fn as_variant(&self) -> &PROPVARIANT {
        &self.variant
    }
}

struct PendingActivation {
    operation: IActivateAudioInterfaceAsyncOperation,
    _callback: IActivateAudioInterfaceCompletionHandler,
    _parameters: ActivationParameters,
    guard: ProcessGuard,
}

#[derive(Debug, PartialEq, Eq)]
enum WaitState {
    Pending,
    Completed,
    Stopped,
    TimedOut,
}
fn wait_state(completed: bool, stopped: bool, elapsed: Duration) -> WaitState {
    if completed {
        WaitState::Completed
    } else if stopped {
        WaitState::Stopped
    } else if elapsed >= ACTIVATION_TIMEOUT {
        WaitState::TimedOut
    } else {
        WaitState::Pending
    }
}

pub fn activate(identity: &ProcessIdentity, stop: &AtomicBool) -> Result<ActivatedProcess, String> {
    if let Some(reason) = capability()? {
        return Err(reason);
    }
    if stop.load(Ordering::Acquire) {
        return Err("Application audio activation was stopped.".into());
    }
    require_mta()?;
    if identity.process_id == unsafe { GetCurrentProcessId() } {
        return Err("The recording host cannot be its own application audio source.".into());
    }
    let expected = parse_creation(&identity.process_started)?;
    let guard = ProcessGuard::open(identity.process_id)?;
    if guard.creation != expected {
        return Err("The selected application has changed. Select it again.".into());
    }
    if ACTIVATION_RESERVED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(
            "Application audio activation is already pending. Start a new recording host.".into(),
        );
    }
    // On ordinary return reset the reservation; only an incomplete asynchronous
    // operation deliberately keeps it reserved until process exit.
    struct Reservation(bool);
    impl Drop for Reservation {
        fn drop(&mut self) {
            if self.0 {
                ACTIVATION_RESERVED.store(false, Ordering::Release);
            }
        }
    }
    let mut reservation = Reservation(true);
    let completed = Arc::new(AtomicBool::new(false));
    let callback = Completion::create(completed.clone());
    let parameters = ActivationParameters::new(identity.process_id);
    let operation = unsafe { ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(parameters.as_variant()), &callback) }
        .map_err(|error| format!("Windows could not activate the selected application's audio ({}). No System audio fallback was used.", error.code()))?;
    let pending = PendingActivation {
        operation,
        _callback: callback,
        _parameters: parameters,
        guard,
    };
    let started = Instant::now();
    loop {
        match wait_state(
            completed.load(Ordering::Acquire),
            stop.load(Ordering::Acquire),
            started.elapsed(),
        ) {
            WaitState::Completed => break,
            state @ (WaitState::Stopped | WaitState::TimedOut) => {
                // No cancellation API exists. One bounded bundle remains valid
                // for a late notification until this fatal-error host exits.
                std::mem::forget(pending);
                reservation.0 = false;
                return Err(if state == WaitState::Stopped {
                    "Application audio activation was stopped. The recording host must exit before another attempt."
                } else {
                    "Application audio activation timed out. The recording host must exit before another attempt."
                }.into());
            }
            WaitState::Pending => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    if stop.load(Ordering::Acquire) {
        return Err("Application audio activation was stopped.".into());
    }
    let mut result = HRESULT(0);
    let mut activated = None;
    unsafe {
        pending
            .operation
            .GetActivateResult(&mut result, &mut activated)
    }
    .map_err(|error| {
        format!(
            "Windows could not retrieve application audio activation ({}).",
            error.code()
        )
    })?;
    result.ok().map_err(|error| format!("Windows rejected application audio activation ({}). No System audio fallback was used.", error.code()))?;
    let client: IAudioClient = activated
        .ok_or_else(|| "Windows returned no application audio client.".to_string())?
        .cast()
        .map_err(|_| "Windows returned an unsupported application audio client.".to_string())?;
    pending.guard.verify()?;
    if stop.load(Ordering::Acquire) {
        return Err("Application audio activation was stopped.".into());
    }
    Ok(ActivatedProcess {
        client,
        guard: pending.guard,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activation_blob_borrows_stable_rust_parameters_across_owner_moves() {
        let owner = ActivationParameters::new(1234);
        let pointer = (&*owner._parameters as *const AUDIOCLIENT_ACTIVATION_PARAMS).cast::<u8>();
        let moved = Box::new(owner);
        let blob = unsafe { moved.as_variant().Anonymous.Anonymous.Anonymous.blob };
        assert_eq!(moved.as_variant().vt(), VT_BLOB);
        assert_eq!(
            blob.cbSize as usize,
            size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>()
        );
        assert_eq!(blob.pBlobData.cast_const(), pointer);
    }
    #[test]
    fn dropping_borrowed_variant_never_clears_the_rust_parameter_allocation() {
        // The pinned Windows extension DOES own/clear an ordinary PROPVARIANT.
        assert!(std::mem::needs_drop::<PROPVARIANT>());
        assert!(!std::mem::needs_drop::<ManuallyDrop<PROPVARIANT>>());
        for process_id in 1..=128 {
            let ActivationParameters {
                variant,
                _parameters: parameters,
            } = ActivationParameters::new(process_id);
            // Regressing variant to Box<PROPVARIANT> would COM-free this borrowed
            // Rust allocation here, then double-free it when parameters drops.
            drop(variant);
            assert_eq!(
                parameters.ActivationType,
                AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK
            );
            assert_eq!(
                unsafe { parameters.Anonymous.ProcessLoopbackParams.TargetProcessId },
                process_id
            );
            drop(parameters);
            // Also exercise the normal/error-return owner destruction order.
            drop(ActivationParameters::new(process_id));
        }
    }
    #[test]
    fn process_identity_is_exact_and_nonzero() {
        assert_eq!(parse_creation("00000000000000ff"), Ok(255));
        for invalid in [
            "0",
            "0000000000000000",
            "00000000000000FF",
            "000000000000000ff",
            "invalid",
        ] {
            assert!(parse_creation(invalid).is_err());
        }
    }
    #[test]
    fn labels_are_utf8_bounded_and_strip_controls() {
        assert_eq!(bounded_label(" \n音声\tapp ", 14), "音声 app");
        assert_eq!(bounded_label(" \n音声\tapp ", 10), "音声 a");
        assert_eq!(bounded_label("🎵🎵", 7), "🎵");
    }
    #[test]
    fn discovery_excludes_self_and_preserves_silent_windows() {
        let mut candidates = Candidates {
            names: BTreeMap::new(),
            excluded: [7, 8],
            visited: 0,
            truncated: false,
        };
        for pid in 0..200 {
            candidates.insert(pid, "silent window".into(), 1);
        }
        assert_eq!(candidates.names.len(), MAX_APPLICATIONS);
        assert!(!candidates.names.contains_key(&7));
        assert!(!candidates.names.contains_key(&8));
        assert!(candidates.truncated);
        candidates.insert(1, "duplicate session".into(), 2);
        assert_eq!(
            candidates.names.get(&1).unwrap(),
            &("silent window".into(), 1)
        );
    }
    #[test]
    fn payload_budget_is_measured_after_json_escaping() {
        let applications = (1..=128)
            .map(|pid| AudioApplicationInfo {
                identity: ProcessIdentity {
                    process_id: pid,
                    process_started: "0000000000000001".into(),
                },
                name: "\\\"".repeat(128),
                process_name: "a".repeat(96),
                available: false,
                reason: Some("r".repeat(200)),
            })
            .collect();
        let fitted = fit_listing(AudioApplicationListing {
            applications,
            truncated: false,
        })
        .unwrap();
        assert!(fitted.truncated);
        assert!(!fitted.applications.is_empty());
        assert!(serde_json::to_vec(&fitted).unwrap().len() <= MAX_LISTING_BYTES);
    }
    #[test]
    fn os_floor_is_coarse_and_activation_wait_is_stop_aware() {
        assert!(support_reason(10, 20347).is_some());
        assert!(support_reason(10, 20348).is_none());
        assert_eq!(wait_state(false, false, Duration::ZERO), WaitState::Pending);
        assert_eq!(wait_state(false, true, Duration::ZERO), WaitState::Stopped);
        assert_eq!(
            wait_state(false, false, ACTIVATION_TIMEOUT),
            WaitState::TimedOut
        );
        assert_eq!(
            wait_state(true, true, ACTIVATION_TIMEOUT),
            WaitState::Completed
        );
    }
    #[test]
    fn callback_is_thread_safe_notification_only() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<Completion>();
        let done = Arc::new(AtomicBool::new(false));
        let handler = Completion::create(done.clone());
        let agile = handler.cast::<windows::core::imp::IAgileObject>().unwrap();
        drop(agile);
        unsafe { (handler.vtable().ActivateCompleted)(handler.as_raw(), std::ptr::null_mut()) }
            .ok()
            .unwrap();
        assert!(done.load(Ordering::Acquire));
        drop(handler);
        assert_eq!(Arc::strong_count(&done), 1);
    }
    #[test]
    fn retained_current_process_identity_is_verified() {
        let guard = ProcessGuard::open(unsafe { GetCurrentProcessId() }).unwrap();
        guard.verify().unwrap();
        assert_ne!(guard.creation, 0);
    }

    #[test]
    fn callback_free_threaded_marshaler_retains_outer_until_release() {
        use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .unwrap();
        struct Apartment;
        impl Drop for Apartment {
            fn drop(&mut self) {
                unsafe { CoUninitialize() };
            }
        }
        let _apartment = Apartment;
        require_mta().unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let handler = Completion::create(done.clone());
        let marshaler = handler.cast::<windows::core::imp::IMarshal>().unwrap();
        drop(handler);
        assert_eq!(Arc::strong_count(&done), 2);
        let handler = marshaler
            .cast::<IActivateAudioInterfaceCompletionHandler>()
            .unwrap();
        unsafe { (handler.vtable().ActivateCompleted)(handler.as_raw(), std::ptr::null_mut()) }
            .ok()
            .unwrap();
        assert!(done.load(Ordering::Acquire));
        drop(handler);
        drop(marshaler);
        assert_eq!(Arc::strong_count(&done), 1);
    }
}
