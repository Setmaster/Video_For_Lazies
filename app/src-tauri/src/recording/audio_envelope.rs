//! Fixed-owned mute history. A durable edge always precedes affected PCM/ACK.
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
};
pub const FILE_NAME: &str = "mute-envelope.bin";
pub const MAX_BYTES: u64 = 4 * 1024 * 1024;
pub const HEADER_BYTES: usize = 64;
pub const RECORD_BYTES: usize = 64;
pub const MAX_RECORDS: u64 = (MAX_BYTES - HEADER_BYTES as u64) / RECORD_BYTES as u64;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    pub ordinal: u64,
    pub sample: u64,
    pub command: u64,
    pub mask: u8,
}
#[derive(Clone, Debug)]
pub struct History {
    pub initial_mask: u8,
    pub roles: u8,
    pub edges: Vec<Edge>,
}
fn bad() -> String {
    "The recording mute history is invalid. Its recovery files have been kept.".into()
}
fn header(session: &str, roles: u8, initial: u8) -> Result<[u8; 64], String> {
    if !super::audio_types::bounded_id(session) || roles == 0 || roles > 3 || initial & !roles != 0
    {
        return Err(bad());
    }
    let mut h = [0; 64];
    h[..8].copy_from_slice(b"VFLMUTE1");
    h[8..10].copy_from_slice(&1u16.to_le_bytes());
    h[10] = roles;
    h[11] = initial;
    h[12..16].copy_from_slice(&48_000u32.to_le_bytes());
    h[16..18].copy_from_slice(&2u16.to_le_bytes());
    h[18..20].copy_from_slice(&64u16.to_le_bytes());
    h[20..52].copy_from_slice(&Sha256::digest(session.as_bytes()));
    Ok(h)
}
fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
fn digest(previous: &[u8; 32], payload: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(previous);
    hash.update(payload);
    hash.finalize().into()
}
pub struct Envelope {
    file: File,
    previous: [u8; 32],
    roles: u8,
    mask: u8,
    count: u64,
    last_sample: u64,
    last_command: u64,
}
impl Envelope {
    pub fn create(directory: &Path, session: &str, roles: u8, initial: u8) -> Result<Self, String> {
        let h = header(session, roles, initial)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(FILE_NAME))
            .map_err(|e| format!("Prepare mute history: {e}"))?;
        file.write_all(&h)
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("Commit mute history header: {e}"))?;
        Ok(Self {
            file,
            previous: Sha256::digest(h).into(),
            roles,
            mask: initial,
            count: 0,
            last_sample: 0,
            last_command: 0,
        })
    }
    pub fn remaining(&self) -> u64 {
        MAX_RECORDS - self.count
    }
    pub fn bytes(&self) -> u64 {
        HEADER_BYTES as u64 + self.count * RECORD_BYTES as u64
    }
    pub fn append(&mut self, sample: u64, command: u64, mask: u8) -> Result<(), String> {
        if mask & !self.roles != 0 || sample < self.last_sample || command <= self.last_command {
            return Err(bad());
        }
        if mask == self.mask {
            return Ok(());
        }
        if self.remaining() == 0 {
            return Err("Mute history reached its safe capacity; recording stopped before applying the change.".into());
        }
        let mut record = [0; 64];
        record[..8].copy_from_slice(&(self.count + 1).to_le_bytes());
        record[8..16].copy_from_slice(&sample.to_le_bytes());
        record[16..24].copy_from_slice(&command.to_le_bytes());
        record[24] = mask;
        let next = digest(&self.previous, &record[..32]);
        record[32..].copy_from_slice(&next);
        // Do not write any PCM at this boundary if either operation fails.
        self.file
            .write_all(&record)
            .and_then(|_| self.file.sync_all())
            .map_err(|e| format!("Commit mute change before audio: {e}"))?;
        self.count += 1;
        self.previous = next;
        self.mask = mask;
        self.last_sample = sample;
        self.last_command = command;
        Ok(())
    }
    pub fn finish(&self) -> Result<(), String> {
        self.file
            .sync_all()
            .map_err(|e| format!("Flush mute history: {e}"))
    }
}
pub fn read(directory: &Path, session: &str, roles: u8, initial: u8) -> Result<History, String> {
    let mut file =
        File::open(directory.join(FILE_NAME)).map_err(|e| format!("Open mute history: {e}"))?;
    let meta = file.metadata().map_err(|_| bad())?;
    if !meta.is_file() || meta.len() < 64 || meta.len() > MAX_BYTES {
        return Err(bad());
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| bad())?;
    decode(&bytes, session, roles, initial)
}
fn decode(bytes: &[u8], session: &str, roles: u8, initial: u8) -> Result<History, String> {
    let h = header(session, roles, initial)?;
    if bytes.len() < 64 || bytes.len() as u64 > MAX_BYTES || bytes[..64] != h {
        return Err(bad());
    }
    let mut previous: [u8; 32] = Sha256::digest(h).into();
    let mut edges = Vec::new();
    let (mut sample, mut command, mut mask) = (0, 0, initial);
    // A single incomplete terminal append is safe ONLY because the writer
    // syncs the complete edge before any affected PCM or applied ACK.
    for record in bytes[64..].chunks_exact(64) {
        let edge = Edge {
            ordinal: u64_at(record, 0),
            sample: u64_at(record, 8),
            command: u64_at(record, 16),
            mask: record[24],
        };
        let next = digest(&previous, &record[..32]);
        if edge.ordinal != edges.len() as u64 + 1
            || edge.sample < sample
            || edge.command <= command
            || edge.mask & !roles != 0
            || edge.mask == mask
            || record[25..32] != [0; 7]
            || record[32..] != next
        {
            return Err(bad());
        }
        sample = edge.sample;
        command = edge.command;
        mask = edge.mask;
        previous = next;
        edges.push(edge);
    }
    Ok(History {
        initial_mask: initial,
        roles,
        edges,
    })
}
impl History {
    pub fn muted(&self, role: u8, sample: u64) -> bool {
        let index = self.edges.partition_point(|edge| edge.sample <= sample);
        let mask = if index == 0 {
            self.initial_mask
        } else {
            self.edges[index - 1].mask
        };
        mask & (1 << role) != 0
    }
    pub fn apply(&self, role: u8, start: u64, samples: &mut [[f32; 2]]) {
        for (index, sample) in samples.iter_mut().enumerate() {
            if self.muted(role, start + index as u64) {
                *sample = [0.; 2];
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn record(bytes: &mut Vec<u8>, sample: u64, command: u64, mask: u8) {
        let ordinal = ((bytes.len() - 64) / 64) as u64 + 1;
        let previous: [u8; 32] = if ordinal == 1 {
            Sha256::digest(&bytes[..64]).into()
        } else {
            bytes[bytes.len() - 32..].try_into().unwrap()
        };
        let mut r = [0; 64];
        r[..8].copy_from_slice(&ordinal.to_le_bytes());
        r[8..16].copy_from_slice(&sample.to_le_bytes());
        r[16..24].copy_from_slice(&command.to_le_bytes());
        r[24] = mask;
        let hash = digest(&previous, &r[..32]);
        r[32..].copy_from_slice(&hash);
        bytes.extend(r);
    }
    #[test]
    fn checked_prefix_rejects_corruption_and_replays_initial_and_same_sample_edges() {
        let mut b = header("session-1", 3, 2).unwrap().to_vec();
        record(&mut b, 480, 1, 0);
        record(&mut b, 480, 2, 2);
        record(&mut b, 960, 3, 0);
        let h = decode(&b, "session-1", 3, 2).unwrap();
        assert!(h.muted(1, 479));
        assert!(h.muted(1, 480));
        assert!(!h.muted(1, 960));
        let mut torn = b.clone();
        torn.extend([1; 31]);
        assert_eq!(decode(&torn, "session-1", 3, 2).unwrap().edges.len(), 3);
        b[100] ^= 1;
        assert!(decode(&b, "session-1", 3, 2).is_err());
        assert!(decode(&torn, "another", 3, 2).is_err());
    }
    #[test]
    fn maximal_history_is_byte_bounded_independent_of_journal() {
        let mut b = header("session-1", 1, 0).unwrap().to_vec();
        for n in 1..=MAX_RECORDS {
            record(&mut b, n, n, (n & 1) as u8);
        }
        assert_eq!(b.len() as u64, MAX_BYTES);
        assert_eq!(
            decode(&b, "session-1", 1, 0).unwrap().edges.len() as u64,
            MAX_RECORDS
        );
        b.push(0);
        assert!(decode(&b, "session-1", 1, 0).is_err());
        let journal = serde_json::json!({"version":2,"audio":{"envelope":FILE_NAME,"format":"f32le","maximumBytes":MAX_BYTES}});
        assert!(serde_json::to_vec(&journal).unwrap().len() < 65536);
    }
}
