//! Small bounded BMP frames shared by the host pipe and binary frontend IPC.
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    sync::OnceLock,
    time::{Duration, Instant},
};

pub const MAX_WIDTH: u32 = 960;
pub const MAX_HEIGHT: u32 = 540;
pub const MAX_BMP: usize = 1_555_254;
pub const MAX_HEADER: usize = 1024;
pub const MAX_FRAME: usize = 12 + MAX_HEADER + MAX_BMP;
pub const LEASE_MS: u64 = 3000;
pub const FRAME_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 30);
const EARLY_TOLERANCE: Duration = Duration::from_millis(2);
/// Absolute phase avoids halving 30 fps delivery when captures/wakeups jitter.
/// A small early tolerance absorbs scheduler jitter without renewing the clock.
#[derive(Default)]
pub struct Cadence {
    next: Option<Instant>,
}
impl Cadence {
    pub fn due(&self, now: Instant) -> bool {
        self.next.is_none_or(|next| now + EARLY_TOLERANCE >= next)
    }
    pub fn poll(&mut self, now: Instant) -> bool {
        if !self.due(now) {
            return false;
        }
        let next = self.next.unwrap_or(now);
        // Preserve phase for ordinary scheduler jitter, but reset after a
        // substantial stall so the next phase cannot cause a short catch-up pair.
        self.next = Some(
            if now.saturating_duration_since(next) >= FRAME_INTERVAL / 2 {
                now + FRAME_INTERVAL
            } else {
                next + FRAME_INTERVAL
            },
        );
        true
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrameHeader {
    pub preview_id: String,
    pub config_epoch: u64,
    pub sequence: u64,
    pub width: u32,
    pub height: u32,
    pub captured_at_qpc100ns: String,
}
#[derive(Clone)]
pub struct Frame {
    pub header: FrameHeader,
    pub bytes: Vec<u8>,
}
fn invalid() -> String {
    "Invalid bounded recording preview frame.".into()
}
impl Frame {
    pub fn new(header: FrameHeader, bmp: &[u8]) -> Result<Self, String> {
        validate_bmp(&header, bmp)?;
        Self::from_bmp(header, bmp.to_vec())
    }
    pub fn from_bmp(header: FrameHeader, mut bmp: Vec<u8>) -> Result<Self, String> {
        validate_bmp(&header, &bmp)?;
        let json = serde_json::to_vec(&header).map_err(|_| invalid())?;
        if json.len() > MAX_HEADER {
            return Err(invalid());
        }
        let bmp_len = bmp.len();
        let prefix = 12 + json.len();
        bmp.reserve_exact(prefix);
        bmp.resize(bmp_len + prefix, 0);
        bmp.copy_within(0..bmp_len, prefix);
        bmp[..4].copy_from_slice(b"VFLP");
        bmp[4..8].copy_from_slice(&(json.len() as u32).to_le_bytes());
        bmp[8..12].copy_from_slice(&(bmp_len as u32).to_le_bytes());
        bmp[12..prefix].copy_from_slice(&json);
        Ok(Self { header, bytes: bmp })
    }
}
pub fn read_frame(reader: &mut impl Read) -> Result<Option<Frame>, String> {
    let mut prefix = [0; 12];
    match reader.read(&mut prefix[..1]) {
        Ok(0) => return Ok(None),
        Ok(_) => {}
        Err(_) => return Err(invalid()),
    }
    reader.read_exact(&mut prefix[1..]).map_err(|_| invalid())?;
    let header_len = u32::from_le_bytes(prefix[4..8].try_into().unwrap()) as usize;
    let bmp_len = u32::from_le_bytes(prefix[8..12].try_into().unwrap()) as usize;
    if &prefix[..4] != b"VFLP"
        || header_len == 0
        || header_len > MAX_HEADER
        || !(54..=MAX_BMP).contains(&bmp_len)
    {
        return Err(invalid());
    }
    let mut json = vec![0; header_len];
    let mut bmp = vec![0; bmp_len];
    reader.read_exact(&mut json).map_err(|_| invalid())?;
    reader.read_exact(&mut bmp).map_err(|_| invalid())?;
    let header = serde_json::from_slice(&json).map_err(|_| invalid())?;
    Frame::from_bmp(header, bmp).map(Some)
}
pub fn write_frame(writer: &mut impl Write, frame: &Frame) -> std::io::Result<()> {
    writer.write_all(&frame.bytes)?;
    writer.flush()
}
fn validate_bmp(header: &FrameHeader, bmp: &[u8]) -> Result<(), String> {
    let w = header.width;
    let h = header.height;
    if w == 0
        || h == 0
        || w > MAX_WIDTH
        || h > MAX_HEIGHT
        || header.preview_id.is_empty()
        || header.preview_id.len() > 100
        || !header
            .preview_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || header.config_epoch == 0
        || header.config_epoch > 9_007_199_254_740_991
        || header.sequence == 0
        || header.sequence > 9_007_199_254_740_991
        || header.captured_at_qpc100ns.parse::<u64>().is_err()
        || bmp.len() < 54
        || bmp.len() > MAX_BMP
    {
        return Err(invalid());
    }
    let stride = (w * 3 + 3) & !3;
    if bmp.len() != 54 + (stride * h) as usize
        || &bmp[..2] != b"BM"
        || u32::from_le_bytes(bmp[2..6].try_into().unwrap()) as usize != bmp.len()
        || u32::from_le_bytes(bmp[10..14].try_into().unwrap()) != 54
        || u32::from_le_bytes(bmp[14..18].try_into().unwrap()) != 40
        || i32::from_le_bytes(bmp[18..22].try_into().unwrap()) != w as i32
        || i32::from_le_bytes(bmp[22..26].try_into().unwrap()) != -(h as i32)
        || bmp[26..30] != [1, 0, 24, 0]
        || bmp[30..34] != [0, 0, 0, 0]
    {
        return Err(invalid());
    }
    Ok(())
}
/// Aspect-contained display dimensions, without upscaling the source.
pub fn dimensions(width: u32, height: u32) -> (u32, u32) {
    dimensions_with_bounds(width, height, MAX_WIDTH, MAX_HEIGHT)
}
pub fn dimensions_with_bounds(
    width: u32,
    height: u32,
    max_width: u32,
    max_height: u32,
) -> (u32, u32) {
    if width <= max_width && height <= max_height {
        (width, height)
    } else if u64::from(width) * u64::from(max_height) > u64::from(height) * u64::from(max_width) {
        (
            max_width,
            (u64::from(height) * u64::from(max_width) / u64::from(width)).max(1) as u32,
        )
    } else {
        (
            (u64::from(width) * u64::from(max_height) / u64::from(height)).max(1) as u32,
            max_height,
        )
    }
}

fn transfer_table() -> &'static [u8; 256] {
    static TRANSFER: OnceLock<[u8; 256]> = OnceLock::new();
    TRANSFER.get_or_init(|| {
        std::array::from_fn(|value| {
            let encoded = value as f32 / 255.;
            let linear = if encoded < 0.081 {
                encoded / 4.5
            } else {
                ((encoded + 0.099) / 1.099).powf(1. / 0.45)
            };
            let srgb = if linear <= 0.0031308 {
                linear * 12.92
            } else {
                1.055 * linear.powf(1. / 2.4) - 0.055
            };
            (srgb * 255.).round().clamp(0., 255.) as u8
        })
    })
}

pub fn thumbnail(nv12: &[u8], width: u32, height: u32) -> Result<(u32, u32, Vec<u8>), String> {
    thumbnail_with_bounds(nv12, width, height, (MAX_WIDTH, MAX_HEIGHT))
}
pub fn thumbnail_with_bounds(
    nv12: &[u8],
    width: u32,
    height: u32,
    bounds: (u32, u32),
) -> Result<(u32, u32, Vec<u8>), String> {
    if width == 0 || height == 0 || width > 7680 || height > 4320 {
        return Err(invalid());
    }
    let ew = (width + 1) & !1;
    let eh = (height + 1) & !1;
    if nv12.len() != (ew * eh * 3 / 2) as usize {
        return Err(invalid());
    }
    let (w, h) = dimensions_with_bounds(width, height, bounds.0, bounds.1);
    let stride = (w * 3 + 3) & !3;
    let len = 54 + stride * h;
    // Reserve envelope headroom so the delivery worker can prepend it in place.
    let mut bmp = Vec::with_capacity(len as usize + 12 + MAX_HEADER);
    bmp.resize(len as usize, 0);
    bmp[..2].copy_from_slice(b"BM");
    bmp[2..6].copy_from_slice(&len.to_le_bytes());
    bmp[10..14].copy_from_slice(&54_u32.to_le_bytes());
    bmp[14..18].copy_from_slice(&40_u32.to_le_bytes());
    bmp[18..22].copy_from_slice(&(w as i32).to_le_bytes());
    bmp[22..26].copy_from_slice(&(-(h as i32)).to_le_bytes());
    bmp[26..28].copy_from_slice(&1_u16.to_le_bytes());
    bmp[28..30].copy_from_slice(&24_u16.to_le_bytes());
    bmp[34..38].copy_from_slice(&(stride * h).to_le_bytes());
    let transfer = transfer_table();
    for y in 0..h {
        for x in 0..w {
            // Bilinear sampling avoids nearest-neighbour shimmer on moving scaled content.
            let sx =
                ((x as f32 + 0.5) * width as f32 / w as f32 - 0.5).clamp(0.0, (width - 1) as f32);
            let sy =
                ((y as f32 + 0.5) * height as f32 / h as f32 - 0.5).clamp(0.0, (height - 1) as f32);
            let (x0, y0) = (sx as u32, sy as u32);
            let (x1, y1) = ((x0 + 1).min(width - 1), (y0 + 1).min(height - 1));
            let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
            let sample = |px: u32, py: u32| {
                let chroma = (ew * eh + (py / 2) * ew + (px & !1)) as usize;
                [
                    nv12[(py * ew + px) as usize],
                    nv12[chroma],
                    nv12[chroma + 1],
                ]
            };
            let a = sample(x0, y0);
            let b = sample(x1, y0);
            let c = sample(x0, y1);
            let d = sample(x1, y1);
            let filtered: [i32; 3] = std::array::from_fn(|channel| {
                let top = a[channel] as f32 * (1.0 - fx) + b[channel] as f32 * fx;
                let bottom = c[channel] as f32 * (1.0 - fx) + d[channel] as f32 * fx;
                (top * (1.0 - fy) + bottom * fy).round() as i32
            });
            let [luma, u, v] = [filtered[0] - 16, filtered[1] - 128, filtered[2] - 128];
            let channels = [
                (298 * luma + 541 * u + 128) >> 8,
                (298 * luma - 55 * u - 136 * v + 128) >> 8,
                (298 * luma + 459 * v + 128) >> 8,
            ];
            let offset = (54 + y * stride + x * 3) as usize;
            for (index, channel) in channels.into_iter().enumerate() {
                bmp[offset + index] = transfer[channel.clamp(0, 255) as usize];
            }
        }
    }
    Ok((w, h, bmp))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn off_phase_stall_does_not_allow_an_immediate_tolerance_catch_up() {
        for late_by in [
            Duration::from_millis(1063),
            Duration::from_millis(1066),
            FRAME_INTERVAL * 32 - Duration::from_millis(1),
        ] {
            let start = Instant::now();
            let mut cadence = Cadence::default();
            assert!(cadence.poll(start));
            let late = start + late_by;
            assert!(cadence.poll(late));
            assert!(!cadence.poll(late));
            assert!(!cadence.poll(late + Duration::from_millis(5)));
            assert!(cadence.poll(late + FRAME_INTERVAL));
        }
    }
    #[test]
    fn accepted_polls_preserve_minimum_spacing_across_late_wake_phases() {
        let half = FRAME_INTERVAL / 2;
        let phases = (0..=70_000).step_by(500).map(Duration::from_micros).chain([
            half - Duration::from_nanos(1),
            half,
            half + Duration::from_nanos(1),
            FRAME_INTERVAL - Duration::from_nanos(1),
            FRAME_INTERVAL,
        ]);
        for phase in phases {
            let start = Instant::now();
            let mut cadence = Cadence::default();
            assert!(cadence.poll(start));
            let late = start + FRAME_INTERVAL + phase;
            assert!(cadence.poll(late));
            let mut previous = late;
            for offset_ms in 0..=100 {
                let now = late + Duration::from_millis(offset_ms);
                if cadence.poll(now) {
                    assert!(
                        now.duration_since(previous) >= half - EARLY_TOLERANCE,
                        "phase {phase:?} produced a short pair: {:?}",
                        now.duration_since(previous)
                    );
                    previous = now;
                }
            }
        }
    }
    #[test]
    fn legacy_thumbnail_keeps_previous_payload_bound() {
        let mut nv12 = vec![16; 1920 * 1080 * 3 / 2];
        nv12[1920 * 1080..].fill(128);
        let (width, height, bmp) = thumbnail_with_bounds(&nv12, 1920, 1080, (480, 270)).unwrap();
        assert_eq!((width, height), (480, 270));
        assert_eq!(bmp.len(), 388_854);
        assert_eq!(bmp.capacity(), 388_854 + 12 + MAX_HEADER);
    }
    #[test]
    fn jittered_thirty_hz_arrivals_keep_delivery_rate_without_catch_up_bursts() {
        let start = Instant::now();
        let mut cadence = Cadence::default();
        for index in 0..120 {
            let jitter = [0, -1_000_000, 1_000_000, -500_000, 500_000][index % 5];
            let time = start + Duration::from_nanos((index as i64 * 33_333_333 + jitter) as u64);
            assert!(cadence.poll(time), "frame {index} was incorrectly dropped");
            assert!(!cadence.poll(time));
        }
        let late = start + Duration::from_secs(10);
        assert!(cadence.poll(late));
        assert!(!cadence.poll(late + Duration::from_millis(1)));
        // Coarse 15.6 ms Windows wakeups preserve the absolute phase on average.
        let mut cadence = Cadence::default();
        let delivered = (0..640)
            .filter(|tick| cadence.poll(start + Duration::from_micros(tick * 15_625)))
            .count();
        assert!(
            (298..=301).contains(&delivered),
            "{delivered} frames in 10 seconds"
        );
    }
    #[test]
    fn landscape_output_fills_new_bound_and_filters_scaled_edges() {
        let (width, height) = (1920, 1080);
        let mut nv12 = vec![128; width * height * 3 / 2];
        for row in nv12[..width * height].chunks_exact_mut(width) {
            for pair in row.chunks_exact_mut(2) {
                pair.copy_from_slice(&[16, 235]);
            }
        }
        let (w, h, bmp) = thumbnail(&nv12, width as u32, height as u32).unwrap();
        assert_eq!((w, h), (MAX_WIDTH, MAX_HEIGHT));
        assert_eq!(bmp.len(), MAX_BMP);
        // Every output pixel covers both dark and light source pixels, rather
        // than selecting only one stripe and shimmering as it moves.
        assert!(
            bmp[54..]
                .iter()
                .all(|channel| (120..=200).contains(channel))
        );
        assert_eq!(MAX_FRAME, 1_556_290);
    }
    #[test]
    fn odd_source_ignores_encoder_padding_and_wire_is_bounded() {
        let mut nv12 = vec![16; 18 * 18 * 3 / 2];
        nv12[18 * 18..].fill(128);
        for y in 0..17 {
            nv12[y * 18..y * 18 + 17].fill(235);
        }
        let (w, h, bmp) = thumbnail(&nv12, 17, 17).unwrap();
        assert_eq!((w, h), (17, 17));
        let stride = (w * 3 + 3) & !3;
        for y in 0..h {
            assert!(
                bmp[(54 + y * stride) as usize..(54 + y * stride + w * 3) as usize]
                    .iter()
                    .all(|v| *v >= 254)
            );
        }
        let pixels = bmp.as_ptr();
        let frame = Frame::from_bmp(
            FrameHeader {
                preview_id: "preview-1".into(),
                config_epoch: 2,
                sequence: 3,
                width: w,
                height: h,
                captured_at_qpc100ns: "100".into(),
            },
            bmp,
        )
        .unwrap();
        assert_eq!(frame.bytes.as_ptr(), pixels);
        assert!(frame.bytes.len() <= MAX_FRAME);
        let decoded = read_frame(&mut frame.bytes.as_slice()).unwrap().unwrap();
        assert_eq!(decoded.header.config_epoch, 2);
        assert_eq!(decoded.bytes, frame.bytes);
        let mut bad = frame.bytes;
        bad[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_frame(&mut bad.as_slice()).is_err());
    }
    #[test]
    fn large_portrait_preview_preserves_aspect_inside_fixed_budget() {
        let mut nv12 = vec![16; 1080 * 1920 * 3 / 2];
        nv12[1080 * 1920..].fill(128);
        let (w, h, bmp) = thumbnail(&nv12, 1080, 1920).unwrap();
        assert_eq!((w, h), (303, 540));
        assert!(bmp.len() <= MAX_BMP);
    }
}
