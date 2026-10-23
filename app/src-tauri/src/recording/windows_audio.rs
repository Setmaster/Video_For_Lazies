//! WASAPI endpoints owned exclusively by the recorder's COM-initialized thread.
//! Construction/enumeration never opens a stream. Only `AudioCapture::start`
//! starts capture. `None` disables an endpoint; `Some("default")` resolves once.

use std::{marker::PhantomData, rc::Rc};
use windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Media::Audio::{
            AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
            AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_LOOPBACK,
            AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, DEVICE_STATE_ACTIVE, EDataFlow,
            IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator, IMMEndpoint,
            MMDeviceEnumerator, WAVEFORMATEX, eCapture, eMultimedia, eRender,
        },
        System::Com::{
            CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ,
            StructuredStorage::{PropVariantClear, PropVariantToStringAlloc},
        },
    },
    core::{Interface, PCWSTR, PWSTR},
};

const SAMPLE_RATE: u32 = 48_000;
const HNS_PER_SECOND: i64 = 10_000_000;

fn audio_error(operation: &str, error: windows::core::Error) -> String {
    format!("{operation}: {error}")
}

fn enumerator() -> Result<IMMDeviceEnumerator, String> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .map_err(|error| audio_error("Open Windows audio devices", error))
}

/// Resolve defaults once without activating a stream. Reopening uses only IDs.
pub fn pin_config(config: &mut super::audio_types::NativeAudioConfig) -> Result<(), String> {
    use super::audio_types::NativeRender;
    fn pin(id: &mut String, flow: EDataFlow) -> Result<(), String> {
        let device = if id == "default" {
            unsafe { enumerator()?.GetDefaultAudioEndpoint(flow, eMultimedia) }
        } else {
            let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
            unsafe { enumerator()?.GetDevice(PCWSTR(wide.as_ptr())) }
        }
        .map_err(|e| audio_error("Validate pinned audio endpoint", e))?;
        let state = unsafe { device.GetState() }
            .map_err(|e| audio_error("Inspect pinned audio endpoint", e))?;
        if state != DEVICE_STATE_ACTIVE {
            return Err("The pinned audio endpoint is unavailable.".into());
        }
        *id = owned_string(
            unsafe { device.GetId() }.map_err(|e| audio_error("Read pinned audio endpoint", e))?,
        )?;
        Ok(())
    }
    if let NativeRender::System { device_id } = &mut config.render {
        pin(device_id, eRender)?;
    }
    if let Some(microphone) = &mut config.microphone {
        pin(&mut microphone.device_id, eCapture)?;
    }
    if let NativeRender::Application { identity } = &config.render {
        super::audio_apps::validate_identity(identity)?;
    }
    Ok(())
}

fn owned_string(value: PWSTR) -> Result<String, String> {
    let result = unsafe { value.to_string() }.map_err(|error| error.to_string());
    unsafe { CoTaskMemFree(Some(value.0.cast())) };
    result
}

fn device_name(device: &IMMDevice) -> Option<String> {
    // Property access can fail independently from endpoint enumeration.
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ).ok()?;
        let mut value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
        let result = PropVariantToStringAlloc(&value)
            .ok()
            .and_then(|name| owned_string(name).ok());
        let _ = PropVariantClear(&mut value);
        result.filter(|name| !name.is_empty())
    }
}

/// The caller owns COM initialization; this function never activates audio clients.
pub fn audio_devices() -> Result<Vec<super::AudioDevice>, String> {
    let enumerator = enumerator()?;
    let mut devices = Vec::new();
    for (flow, kind, default_name) in [
        (eRender, "system", "Default system output"),
        (eCapture, "microphone", "Default microphone"),
    ] {
        if let Ok(default) = unsafe { enumerator.GetDefaultAudioEndpoint(flow, eMultimedia) } {
            let suffix = device_name(&default)
                .map(|name| format!(" ({name})"))
                .unwrap_or_default();
            devices.push(super::AudioDevice {
                id: "default".into(),
                name: format!("{default_name}{suffix}"),
                kind: kind.into(),
            });
        }
        let endpoints = unsafe { enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) }
            .map_err(|error| audio_error("List Windows audio endpoints", error))?;
        let count = unsafe { endpoints.GetCount() }
            .map_err(|error| audio_error("Count Windows audio endpoints", error))?;
        for index in 0..count {
            let device = unsafe { endpoints.Item(index) }
                .map_err(|error| audio_error("Read Windows audio endpoint", error))?;
            let id = owned_string(
                unsafe { device.GetId() }
                    .map_err(|error| audio_error("Read Windows audio endpoint ID", error))?,
            )?;
            devices.push(super::AudioDevice {
                name: device_name(&device).unwrap_or_else(|| id.clone()),
                id,
                kind: kind.into(),
            });
        }
    }
    Ok(devices)
}

struct Endpoint {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    timeline: timeline::Timeline,
    running: bool,
    label: &'static str,
    role: usize,
    process: Option<super::audio_apps::ProcessGuard>,
}

impl Endpoint {
    fn open(enumerator: &IMMDeviceEnumerator, id: &str, flow: EDataFlow) -> Result<Self, String> {
        let label = if flow == eRender {
            "System audio"
        } else {
            "Microphone"
        };
        let device = if id == "default" {
            unsafe { enumerator.GetDefaultAudioEndpoint(flow, eMultimedia) }
        } else {
            let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
            if id.is_empty() || id.contains('\0') {
                return Err(format!("{label}: invalid audio endpoint ID"));
            }
            unsafe { enumerator.GetDevice(PCWSTR(wide.as_ptr())) }
        }
        .map_err(|error| {
            audio_error(&format!("{label}: selected endpoint is unavailable"), error)
        })?;
        let endpoint: IMMEndpoint = device
            .cast()
            .map_err(|error| audio_error("Inspect Windows audio endpoint", error))?;
        let actual_flow = unsafe { endpoint.GetDataFlow() }
            .map_err(|error| audio_error("Inspect Windows audio endpoint direction", error))?;
        if actual_flow != flow {
            return Err(format!(
                "{label}: selected endpoint has the wrong direction"
            ));
        }
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
            .map_err(|error| audio_error(&format!("{label}: activate endpoint"), error))?;
        Self::initialize(client, flow, label, None)
    }
    fn initialize(
        client: IAudioClient,
        flow: EDataFlow,
        label: &'static str,
        process: Option<super::audio_apps::ProcessGuard>,
    ) -> Result<Self, String> {
        // The system audio engine handles native channel layout/sample-rate conversion.
        // The timeline below then corrects device-clock drift against shared QPC.
        let format = WAVEFORMATEX {
            wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
            nChannels: 2,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * 8,
            nBlockAlign: 8,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        let flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY
            | if flow == eRender {
                AUDCLNT_STREAMFLAGS_LOOPBACK
            } else {
                0
            };
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags,
                HNS_PER_SECOND,
                0,
                &format,
                None,
            )
        }
        .map_err(|error| audio_error(&format!("{label}: initialize shared capture"), error))?;
        let capture = unsafe { client.GetService() }
            .map_err(|error| audio_error(&format!("{label}: open capture service"), error))?;
        Ok(Self {
            client,
            capture,
            timeline: timeline::Timeline::new(flow == eRender),
            running: false,
            label,
            role: if flow == eRender { 0 } else { 1 },
            process,
        })
    }

    fn start(&mut self) -> Result<(), String> {
        if let Some(process) = &self.process {
            process.verify()?;
        }
        unsafe { self.client.Start() }
            .map_err(|error| audio_error(&format!("{}: start capture", self.label), error))?;
        self.running = true;
        Ok(())
    }

    fn poll(&mut self) -> Result<(), String> {
        if let Some(process) = &self.process {
            process.verify()?;
        }
        // One second of endpoint buffering, with a finite drain bound even if a
        // broken device keeps producing packets. All failures terminate capture.
        for _ in 0..256 {
            let size = unsafe { self.capture.GetNextPacketSize() }
                .map_err(|error| audio_error(&format!("{}: poll capture", self.label), error))?;
            if size == 0 {
                return Ok(());
            }
            let (mut data, mut frames, mut flags, mut position, mut qpc) =
                (std::ptr::null_mut(), 0, 0, 0, 0);
            unsafe {
                self.capture.GetBuffer(
                    &mut data,
                    &mut frames,
                    &mut flags,
                    Some(&mut position),
                    Some(&mut qpc),
                )
            }
            .map_err(|error| audio_error(&format!("{}: read capture packet", self.label), error))?;
            if frames == 0 {
                return Ok(());
            } // AUDCLNT_S_BUFFER_EMPTY acquired no packet.
            // Always release a successfully acquired packet, including malformed
            // or timestamp-error packets. Copy before ReleaseBuffer invalidates it.
            let packet = if frames > SAMPLE_RATE * 2 {
                Err(format!(
                    "{}: audio packet exceeded buffer limit",
                    self.label
                ))
            } else if flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 != 0
                || qpc > i64::MAX as u64
            {
                Err(format!(
                    "{}: audio packet has an unreliable QPC timestamp; recording stopped",
                    self.label
                ))
            } else if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                Ok(vec![[0.0; 2]; frames as usize])
            } else if data.is_null() {
                Err(format!("{}: audio packet has no sample data", self.label))
            } else {
                let mut samples = Vec::with_capacity(frames as usize);
                for frame in 0..frames as usize {
                    let pointer = unsafe { data.add(frame * 8).cast::<f32>() };
                    let left = unsafe { pointer.read_unaligned() };
                    let right = unsafe { pointer.add(1).read_unaligned() };
                    samples.push([finite(left), finite(right)]);
                }
                Ok(samples)
            };
            let released = unsafe { self.capture.ReleaseBuffer(frames) }.map_err(|error| {
                audio_error(&format!("{}: release capture packet", self.label), error)
            });
            let samples = packet?;
            released?;
            let discontinuity = flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0;
            self.timeline
                .push(qpc as i64, position, samples, discontinuity)
                .map_err(|error| format!("{}: {error}", self.label))?;
        }
        Err(format!(
            "{}: audio device exceeded packet drain limit",
            self.label
        ))
    }

    fn stop(&mut self) -> Result<(), String> {
        if self.running {
            unsafe { self.client.Stop() }
                .map_err(|error| audio_error(&format!("{}: stop capture", self.label), error))?;
            self.running = false;
        }
        Ok(())
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn finite(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

pub struct AudioCapture {
    endpoints: Vec<Endpoint>,
    stopped: bool,
    // Capture GetBuffer/ReleaseBuffer and COM releases stay on their owner thread.
    _thread: PhantomData<Rc<()>>,
}

impl AudioCapture {
    pub fn start(system_id: Option<&str>, mic_id: Option<&str>) -> Result<Self, String> {
        let mut capture = Self {
            endpoints: Vec::new(),
            stopped: false,
            _thread: PhantomData,
        };
        if system_id.is_none() && mic_id.is_none() {
            return Ok(capture);
        }
        let enumerator = enumerator()?;
        for (id, flow) in [(system_id, eRender), (mic_id, eCapture)] {
            if let Some(id) = id {
                capture
                    .endpoints
                    .push(Endpoint::open(&enumerator, id, flow)?);
            }
        }
        // Initialize every requested endpoint before starting any capture.
        for endpoint in &mut capture.endpoints {
            endpoint.start()?;
        }
        Ok(capture)
    }

    pub fn start_native(
        render: &super::audio_types::NativeRender,
        mic_id: Option<&str>,
        stop: &std::sync::atomic::AtomicBool,
    ) -> Result<Self, String> {
        use super::audio_types::NativeRender;
        let cancelled = || {
            if stop.load(std::sync::atomic::Ordering::Acquire) {
                Err("Audio preparation was cancelled.".to_string())
            } else {
                Ok(())
            }
        };
        cancelled()?;
        let mut capture = Self {
            endpoints: Vec::new(),
            stopped: false,
            _thread: PhantomData,
        };
        match render {
            NativeRender::Off => {}
            NativeRender::System { device_id } => {
                capture
                    .endpoints
                    .push(Endpoint::open(&enumerator()?, device_id, eRender)?)
            }
            NativeRender::Application { identity } => {
                let activated = super::audio_apps::activate(identity, stop)?;
                cancelled()?;
                capture.endpoints.push(Endpoint::initialize(
                    activated.client,
                    eRender,
                    "Application audio",
                    Some(activated.guard),
                )?);
            }
        }
        cancelled()?;
        if let Some(id) = mic_id {
            capture
                .endpoints
                .push(Endpoint::open(&enumerator()?, id, eCapture)?);
        }
        cancelled()?;
        for endpoint in &mut capture.endpoints {
            if stop.load(std::sync::atomic::Ordering::Acquire) {
                return Err("Audio preparation was cancelled before starting an endpoint.".into());
            }
            endpoint.start()?;
            cancelled()?;
        }
        Ok(capture)
    }
    /// Render each source exactly once, without irreversible pre-mix clipping.
    pub fn render_sources(
        &mut self,
        start: i64,
        frames: u32,
    ) -> Result<[Vec<[f32; 2]>; 2], String> {
        if frames > 480 {
            return Err("Audio source block exceeds10ms.".into());
        }
        let mut blocks = [
            vec![[0.; 2]; frames as usize],
            vec![[0.; 2]; frames as usize],
        ];
        for endpoint in &mut self.endpoints {
            endpoint.timeline.mix(start, &mut blocks[endpoint.role]);
        }
        Ok(blocks)
    }

    pub fn poll(&mut self) -> Result<(), String> {
        let mut failure = None;
        if !self.stopped {
            for endpoint in &mut self.endpoints {
                if let Err(error) = endpoint.poll() {
                    failure.get_or_insert(error);
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }

    /// Exact interleaved stereo 48 kHz PCM. Render at least 100 ms behind
    /// current QPC to allow delivery of the endpoint's most recent packets.
    /// Calls advance forward; late packets are discarded, missing audio is zero.
    pub fn mix_into(&mut self, start_qpc100ns: i64, frames: u32) -> Result<Vec<i16>, String> {
        if frames > SAMPLE_RATE {
            return Err("Audio mix request exceeds one second".into());
        }
        self.poll()?;
        let mut mixed = vec![[0.0; 2]; frames as usize];
        for endpoint in &mut self.endpoints {
            endpoint.timeline.mix(start_qpc100ns, &mut mixed);
        }
        Ok(mixed
            .into_iter()
            .flat_map(|frame| frame.map(timeline::pcm))
            .collect())
    }

    /// Release all COM clients after the final copied timeline samples are written.
    pub fn release(&mut self) {
        self.endpoints.clear();
        self.stopped = true;
    }

    /// Stop clients before final timeline rendering; keeps already copied audio.
    pub fn stop(&mut self) -> Result<(), String> {
        if self.stopped {
            return Ok(());
        }
        // Stop all endpoints even if one errors; never retain microphone capture
        // because an unrelated loopback endpoint was invalidated.
        let mut failure = None;
        for endpoint in &mut self.endpoints {
            if let Err(error) = endpoint.stop() {
                failure.get_or_insert(error);
            }
        }
        for endpoint in &mut self.endpoints {
            if let Err(error) = endpoint.poll() {
                failure.get_or_insert(error);
            }
        }
        self.stopped = true;
        failure.map_or(Ok(()), Err)
    }
}

// This module is independent from COM so clock, buffering and mixing behavior
// can also be tested without opening devices or recording desktop audio.
mod timeline {
    use std::collections::VecDeque;
    const STEP: f64 = 10_000_000.0 / 48_000.0;
    const MAX_FRAMES: usize = 96_000;
    const MAX_PACKETS: usize = 256;
    const MAX_GAP_HNS: f64 = 2_500_000.0;

    struct Packet {
        qpc: i64,
        position: u64,
        step: f64,
        samples: Vec<[f32; 2]>,
        joins_previous: bool,
    }
    impl Packet {
        fn end(&self) -> f64 {
            self.qpc as f64 + self.samples.len() as f64 * self.step
        }
    }
    #[derive(Default)]
    pub(super) struct Timeline {
        packets: VecDeque<Packet>,
        frames: usize,
        continuous: bool,
        // Survives consumption of buffered packets, so a blocked capture loop
        // cannot hide a long discontinuity by first rendering its old packets.
        last_packet: Option<(i64, f64)>,
        // WASAPI loopback can stop delivering packets while the render endpoint
        // is idle. Its next QPC timestamp resumes at the actual sound time.
        // Capture endpoints instead promise a continuous microphone stream.
        loopback: bool,
    }
    impl Timeline {
        pub(super) fn new(loopback: bool) -> Self {
            Self {
                loopback,
                ..Self::default()
            }
        }

        #[cfg(test)]
        fn break_continuity(&mut self) {
            self.continuous = false;
        }

        pub(super) fn push(
            &mut self,
            qpc: i64,
            position: u64,
            samples: Vec<[f32; 2]>,
            discontinuity: bool,
        ) -> Result<(), String> {
            if samples.is_empty() {
                return Ok(());
            }
            if samples.len() > MAX_FRAMES - self.frames || self.packets.len() >= MAX_PACKETS {
                return Err(
                    "audio buffering exceeded its limit; recording stopped to prevent audio loss"
                        .into(),
                );
            }
            if let Some((previous_qpc, previous_end)) = self.last_packet {
                if qpc <= previous_qpc {
                    return Err("audio QPC timestamp moved backwards; recording stopped".into());
                }
                if !self.loopback && qpc as f64 - previous_end > MAX_GAP_HNS {
                    return Err(
                        "audio packet gap exceeded 250 ms; recording stopped to prevent audio loss"
                            .into(),
                    );
                }
            }
            let mut joins_previous = false;
            if let Some(previous) = self.packets.back_mut()
                && self.continuous
                && !discontinuity
                && previous.position.checked_add(previous.samples.len() as u64) == Some(position)
            {
                let measured = (qpc - previous.qpc) as f64 / previous.samples.len() as f64;
                // Only small clock-rate differences are drift. Larger gaps
                // remain silence instead of stretching old samples over them.
                if (STEP * 0.99..=STEP * 1.01).contains(&measured) {
                    previous.step = measured;
                    joins_previous = true;
                }
            }
            self.frames += samples.len();
            self.last_packet = Some((qpc, qpc as f64 + samples.len() as f64 * STEP));
            self.packets.push_back(Packet {
                qpc,
                position,
                step: STEP,
                samples,
                joins_previous,
            });
            self.continuous = true;
            Ok(())
        }

        fn pop_front(&mut self) {
            if let Some(packet) = self.packets.pop_front() {
                self.frames -= packet.samples.len();
            }
        }

        pub(super) fn mix(&mut self, start: i64, output: &mut [[f32; 2]]) {
            for (frame, destination) in output.iter_mut().enumerate() {
                let time = start as f64 + frame as f64 * STEP;
                while self
                    .packets
                    .front()
                    .is_some_and(|packet| packet.end() <= time)
                {
                    self.pop_front();
                }
                let Some(packet) = self.packets.front() else {
                    continue;
                };
                if time < packet.qpc as f64 {
                    continue;
                }
                let source = (time - packet.qpc as f64) / packet.step;
                let index = source.floor() as usize;
                let Some(left) = packet.samples.get(index) else {
                    continue;
                };
                // Adjacent packet interpolation is allowed only when both QPC
                // and device frame positions agree. Never interpolate a gap.
                let right = packet
                    .samples
                    .get(index + 1)
                    .or_else(|| {
                        self.packets
                            .get(1)
                            .filter(|next| {
                                next.joins_previous
                                    && packet.position.checked_add(packet.samples.len() as u64)
                                        == Some(next.position)
                                    && (next.qpc as f64 - packet.end()).abs() < 2.0
                            })
                            .and_then(|next| next.samples.first())
                    })
                    .unwrap_or(left);
                let fraction = (source - index as f64) as f32;
                for channel in 0..2 {
                    destination[channel] +=
                        left[channel] + (right[channel] - left[channel]) * fraction;
                }
            }
            // Retain at most the packet containing the next output timestamp.
            let end = start as f64 + output.len() as f64 * STEP;
            while self
                .packets
                .front()
                .is_some_and(|packet| packet.end() <= end)
            {
                self.pop_front();
            }
        }
    }

    pub(super) fn pcm(value: f32) -> i16 {
        if !value.is_finite() {
            return 0;
        }
        (value.clamp(-1.0, 1.0) * 32768.0)
            .round()
            .clamp(-32768.0, 32767.0) as i16
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn small_gaps_are_silent_and_consumed_audio_is_discarded() {
            let mut timeline = Timeline::default();
            timeline
                .push(10_000, 0, vec![[0.5, -0.5]; 48], false)
                .unwrap();
            timeline
                .push(30_000, 96, vec![[0.25; 2]; 48], true)
                .unwrap();
            let mut output = vec![[0.0; 2]; 192];
            timeline.mix(0, &mut output);
            assert_eq!(output[0], [0.0; 2]);
            assert_eq!(output[48], [0.5, -0.5]);
            assert_eq!(output[100], [0.0; 2]);
            assert_eq!(output[144], [0.25; 2]);
            assert_eq!(timeline.frames, 0);
        }
        #[test]
        fn qpc_rate_corrects_linear_resampling() {
            let mut timeline = Timeline::default();
            let samples = (0..480).map(|index| [index as f32 / 480.0; 2]).collect();
            timeline.push(0, 0, samples, false).unwrap();
            timeline
                .push(100_500, 480, vec![[1.0; 2]; 480], false)
                .unwrap();
            let mut output = vec![[0.0; 2]; 480];
            timeline.mix(0, &mut output);
            assert!((output[240][0] - 0.5 / 1.005).abs() < 0.00001);
        }
        #[test]
        fn mixing_clamps_and_never_wraps() {
            let mut system = Timeline::default();
            let mut microphone = Timeline::default();
            system.push(0, 0, vec![[0.8, -0.8]; 48], false).unwrap();
            microphone.push(0, 0, vec![[0.8, -0.8]; 48], false).unwrap();
            let mut output = vec![[0.0; 2]; 48];
            system.mix(0, &mut output);
            microphone.mix(0, &mut output);
            assert_eq!(output[0].map(pcm), [i16::MAX, i16::MIN]);
            assert_eq!(pcm(f32::NAN), 0);
        }
        #[test]
        fn frame_overload_is_an_error_without_discarding_buffered_audio() {
            let mut timeline = Timeline::default();
            for index in 0..200 {
                timeline
                    .push(
                        index * 100_000,
                        index as u64 * 480,
                        vec![[1.0; 2]; 480],
                        false,
                    )
                    .unwrap();
            }
            assert_eq!(timeline.frames, MAX_FRAMES);
            assert!(
                timeline
                    .push(20_000_000, 96_000, vec![[1.0; 2]; 480], false)
                    .unwrap_err()
                    .contains("buffering exceeded")
            );
            assert_eq!(timeline.frames, MAX_FRAMES);
            assert_eq!(timeline.packets.front().unwrap().qpc, 0);
        }
        #[test]
        fn packet_overload_is_an_error_even_for_tiny_packets() {
            let mut timeline = Timeline::default();
            for index in 0..MAX_PACKETS {
                timeline
                    .push(index as i64 * 209, index as u64, vec![[1.0; 2]], false)
                    .unwrap();
            }
            assert!(
                timeline
                    .push(
                        MAX_PACKETS as i64 * 209,
                        MAX_PACKETS as u64,
                        vec![[1.0; 2]],
                        false
                    )
                    .is_err()
            );
            assert_eq!(timeline.packets.len(), MAX_PACKETS);
        }
        #[test]
        fn reset_timestamps_fail_after_buffered_audio_has_been_consumed() {
            let mut timeline = Timeline::default();
            timeline
                .push(100_000, 0, vec![[1.0; 2]; 480], false)
                .unwrap();
            timeline.mix(100_000, &mut vec![[0.0; 2]; 480]);
            assert_eq!(timeline.frames, 0);
            assert!(
                timeline
                    .push(1, 0, vec![[1.0; 2]; 480], false)
                    .unwrap_err()
                    .contains("backwards")
            );
            assert_eq!(timeline.frames, 0);
        }
        #[test]
        fn long_gap_is_terminal_but_first_packet_has_no_deadline() {
            let mut timeline = Timeline::default();
            let start = 600_000_000;
            timeline.push(start, 0, vec![[1.0; 2]; 480], false).unwrap();
            timeline.mix(start, &mut vec![[0.0; 2]; 480]);
            assert_eq!(timeline.frames, 0);
            // The first packet ended at start + 100_000. Exactly 250 ms is allowed.
            timeline
                .push(start + 2_600_000, 480, vec![[1.0; 2]; 480], true)
                .unwrap();
            assert!(
                timeline
                    .push(start + 5_200_001, 960, vec![[1.0; 2]; 480], true)
                    .unwrap_err()
                    .contains("gap exceeded 250 ms")
            );
        }
        #[test]
        fn idle_loopback_resumes_at_qpc_without_stretching_sound_over_silence() {
            let mut timeline = Timeline::new(true);
            timeline.push(0, 0, vec![[0.75; 2]; 480], false).unwrap();
            let mut first = vec![[0.0; 2]; 480];
            timeline.mix(0, &mut first);
            assert_eq!(first[0], [0.75; 2]);
            assert_eq!(timeline.frames, 0);
            // Silence may produce no loopback packets for seconds. The device
            // position can resume contiguous even though wall QPC advanced.
            timeline
                .push(30_000_000, 480, vec![[0.25; 2]; 480], true)
                .unwrap();
            let mut silent = vec![[0.0; 2]; 480];
            timeline.mix(20_000_000, &mut silent);
            assert!(silent.iter().all(|frame| *frame == [0.0; 2]));
            let mut resumed = vec![[0.0; 2]; 480];
            timeline.mix(30_000_000, &mut resumed);
            assert!(resumed.iter().all(|frame| *frame == [0.25; 2]));
            assert!(timeline.push(1, 0, vec![[1.0; 2]; 480], true).is_err());
        }
        #[test]
        fn discontinuity_does_not_stretch_or_interpolate_old_samples() {
            let mut timeline = Timeline::default();
            timeline.push(0, 0, vec![[1.0; 2]; 480], false).unwrap();
            timeline
                .push(100_000, 480, vec![[0.5; 2]; 480], true)
                .unwrap();
            assert_eq!(timeline.packets[0].step, STEP);
            let mut output = [[0.0; 2]];
            timeline.mix(99_900, &mut output);
            assert_eq!(output[0], [1.0; 2]);
            timeline.break_continuity();
            timeline
                .push(200_500, 960, vec![[0.5; 2]; 480], false)
                .unwrap();
            assert_eq!(timeline.packets[0].step, STEP);
        }
    }
}
