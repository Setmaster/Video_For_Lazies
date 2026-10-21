//! Small bounded BMP frames shared by the host pipe and binary frontend IPC.
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

pub const MAX_WIDTH: u32 = 480;
pub const MAX_HEIGHT: u32 = 270;
pub const MAX_BMP: usize = 388_854;
pub const MAX_HEADER: usize = 1024;
pub const MAX_FRAME: usize = 12 + MAX_HEADER + MAX_BMP;
pub const LEASE_MS: u64 = 3000;
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
        let json = serde_json::to_vec(&header).map_err(|_| invalid())?;
        if json.len() > MAX_HEADER {
            return Err(invalid());
        }
        let mut bytes = Vec::with_capacity(12 + json.len() + bmp.len());
        bytes.extend_from_slice(b"VFLP");
        bytes.extend_from_slice(&(json.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(bmp.len() as u32).to_le_bytes());
        bytes.extend(json);
        bytes.extend_from_slice(bmp);
        Ok(Self { header, bytes })
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
    Frame::new(header, &bmp).map(Some)
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
pub fn thumbnail(nv12: &[u8], width: u32, height: u32) -> Result<(u32, u32, Vec<u8>), String> {
    if width == 0 || height == 0 || width > 7680 || height > 4320 {
        return Err(invalid());
    }
    let ew = (width + 1) & !1;
    let eh = (height + 1) & !1;
    if nv12.len() != (ew * eh * 3 / 2) as usize {
        return Err(invalid());
    }
    let (w, h) = if width <= MAX_WIDTH && height <= MAX_HEIGHT {
        (width, height)
    } else if u64::from(width) * u64::from(MAX_HEIGHT) > u64::from(height) * u64::from(MAX_WIDTH) {
        (MAX_WIDTH, (height * MAX_WIDTH / width).max(1))
    } else {
        ((width * MAX_HEIGHT / height).max(1), MAX_HEIGHT)
    };
    let stride = (w * 3 + 3) & !3;
    let len = 54 + stride * h;
    let mut bmp = vec![0_u8; len as usize];
    bmp[..2].copy_from_slice(b"BM");
    bmp[2..6].copy_from_slice(&len.to_le_bytes());
    bmp[10..14].copy_from_slice(&54_u32.to_le_bytes());
    bmp[14..18].copy_from_slice(&40_u32.to_le_bytes());
    bmp[18..22].copy_from_slice(&(w as i32).to_le_bytes());
    bmp[22..26].copy_from_slice(&(-(h as i32)).to_le_bytes());
    bmp[26..28].copy_from_slice(&1_u16.to_le_bytes());
    bmp[28..30].copy_from_slice(&24_u16.to_le_bytes());
    bmp[34..38].copy_from_slice(&(stride * h).to_le_bytes());
    let transfer: [u8; 256] = std::array::from_fn(|value| {
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
    });
    for y in 0..h {
        for x in 0..w {
            let sx = (u64::from(x) * u64::from(width) / u64::from(w)) as u32;
            let sy = (u64::from(y) * u64::from(height) / u64::from(h)) as u32;
            let luma = i32::from(nv12[(sy * ew + sx) as usize]) - 16;
            let chroma = (ew * eh + (sy / 2) * ew + (sx & !1)) as usize;
            let u = i32::from(nv12[chroma]) - 128;
            let v = i32::from(nv12[chroma + 1]) - 128;
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
        let frame = Frame::new(
            FrameHeader {
                preview_id: "preview-1".into(),
                config_epoch: 2,
                sequence: 3,
                width: w,
                height: h,
                captured_at_qpc100ns: "100".into(),
            },
            &bmp,
        )
        .unwrap();
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
        assert_eq!((w, h), (151, 270));
        assert!(bmp.len() <= MAX_BMP);
    }
}
