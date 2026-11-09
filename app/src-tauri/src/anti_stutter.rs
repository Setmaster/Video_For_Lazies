//! Conservative, opt-in source-anchor selection. This module never owns a clock.
use crate::interpolation_timeline::is_scene_cut;
use crate::video::AntiStutterMode;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub(super) const MAX_PICTURES: usize = 32;
const MAX_BUFFER_BYTES: usize = 256 * 1024 * 1024;
const MAX_REPEATED: usize = 24;
const MAX_SPAN_NS: u64 = 750_000_000;
const DETAIL_LIMIT: usize = 32;
const TILES: usize = 48;

// Strength changes confidence only. Similarity, source segmentation and all
// discontinuity/resource guards remain identical to the conservative policy.
#[derive(Clone, Copy, Debug, Default)]
struct Policy {
    strength: u8,
}
impl Policy {
    fn minimum_matches(self) -> usize {
        12 - usize::from(self.strength.min(100)) / 25
    }
    fn residual_factor(self) -> u128 {
        400 - 2 * u128::from(self.strength.min(100))
    }
    fn accepts_residual(self, residual: u64, context: Evidence) -> bool {
        u128::from(residual) * self.residual_factor() <= u128::from(context.coarse_mean) * 100
    }
    fn accepts_matches(self, textured: usize, matched: usize, coherent: usize) -> bool {
        textured >= 12 && coherent >= self.minimum_matches() && coherent * 3 >= matched * 2
    }
}

fn is_zero(value: &u8) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Interval {
    pub start_s: f64,
    pub end_s: f64,
    pub source_repeated_frames: u64,
    pub repaired_frames: u64,
    pub reason: String,
    /// Total surviving source A-to-B time, present only for an extended bridge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_duration_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AntiStutterDiagnostics {
    pub version: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub strength: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<AntiStutterMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_duration_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Repeated source picture timestamps inside deleted suffixes. A partial
    /// interval deletion can be positive even when no picture timestamp falls in it.
    pub removed_frames: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_runs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridged_runs: Option<u64>,
    /// Extra retained time beyond the first actual source display interval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_bridge_duration_s: Option<f64>,
    pub analyzed_frames: u64,
    pub candidate_runs: u64,
    pub repaired_runs: u64,
    pub repaired_frames: u64,
    pub unchanged_runs: u64,
    pub intervals: Vec<Interval>,
}

#[derive(Clone, Copy, Debug)]
struct Evidence {
    exact: bool,
    near: bool,
    cut: bool,
    lighting: bool,
    ambiguous: bool,
    mean: u64,
    coarse_mean: u64,
    tiles: [u64; TILES],
    delta_ns: u64,
}
impl Evidence {
    fn plateau(self) -> bool {
        self.exact || self.near
    }
    fn discontinuity(self) -> bool {
        self.cut || self.lighting
    }
    fn moving(self) -> bool {
        !self.discontinuity()
            && !self.ambiguous
            && !self.plateau()
            && self.mean >= 500
            && self.tiles.iter().filter(|mean| **mean >= 500).count() >= 12
    }
}

fn compare(a: &[u8], b: &[u8], width: usize, height: usize) -> Result<Evidence, String> {
    let mut total = 0u64;
    let mut near_tiles = true;
    let mut direction = [[0u64; 2]; 3];
    let mut tiles = [0u64; TILES];
    for ty in 0..6 {
        for tx in 0..8 {
            let mut sum = 0u64;
            let mut count = 0u64;
            for y in height * ty / 6..height * (ty + 1) / 6 {
                crate::process::check_cancelled().map_err(|_| "Canceled.".to_string())?;
                let first = (y * width + width * tx / 8) * 3;
                let last = (y * width + width * (tx + 1) / 8) * 3;
                for (index, (&left, &right)) in
                    a[first..last].iter().zip(&b[first..last]).enumerate()
                {
                    let difference = left.abs_diff(right);
                    let channel = index % 3;
                    direction[channel][0] += u64::from(right > left);
                    direction[channel][1] += u64::from(right < left);
                    sum += u64::from(difference);
                    count += 1;
                }
            }
            tiles[ty * 8 + tx] = sum * 1_000 / count.max(1);
            near_tiles &= sum * 2 <= count * 3;
            total += sum;
        }
    }
    let mean = total * 1_000 / a.len().max(1) as u64;
    let pixels = (a.len() / 3) as u64;
    // A broad one-direction color/brightness step is uncertain lighting, not
    // evidence of camera displacement. Do not bridge a held flash or fade.
    let lighting =
        pixels > 0 && (0..3).any(|c| direction[c][0].max(direction[c][1]) * 100 >= pixels * 95);
    let cut = is_scene_cut(a, b);
    let mut near = total != 0 && total * 2 <= a.len() as u64 && near_tiles;
    let mut ambiguous = false;
    let mut coarse_mean = 0;
    if total > 0 && !cut && !lighting {
        let left = Luma::new(a, width, height)?;
        let right = Luma::new(b, width, height)?;
        let mut coarse_total = 0u64;
        let mut coarse_tiles = true;
        for ty in 0..6 {
            for tx in 0..8 {
                let mut sum = 0u64;
                let mut count = 0u64;
                for y in left.height * ty / 6..left.height * (ty + 1) / 6 {
                    crate::process::check_cancelled().map_err(|_| "Canceled.".to_string())?;
                    for x in left.width * tx / 8..left.width * (tx + 1) / 8 {
                        let index = y * left.width + x;
                        sum += u64::from(left.pixels[index].abs_diff(right.pixels[index]));
                        count += 1;
                    }
                }
                coarse_total += sum;
                coarse_tiles &= sum <= count * 6;
            }
        }
        coarse_mean = coarse_total * 1_000 / left.pixels.len().max(1) as u64;
        if !near
            && total <= a.len() as u64 * 8
            && coarse_total <= left.pixels.len() as u64 * 2
            && coarse_tiles
        {
            let matches = correspondence(&left, &right)?;
            near = matches.held();
            // Weak, low-frequency residuals alone never become evidence of
            // distinct flanking motion for another tentative plateau.
            ambiguous = !near && !matches.continuous(Policy::default());
        }
    }
    Ok(Evidence {
        exact: total == 0,
        // Compression may change a few channels greatly while the picture
        // remains held. Compare sums before rounding; maximum-channel noise
        // is not a semantic veto. This opt-in policy may alter small details.
        near,
        cut,
        lighting,
        ambiguous,
        mean,
        coarse_mean,
        tiles,
        delta_ns: 0,
    })
}

// A consistent residual growing at the same locations is observable drift,
// even when its individual steps fit the compression-near envelope. Exact or
// oscillating residuals do not establish this directional trend.
fn drifting(anchor: &[u8], previous: &[u8], current: &[u8]) -> Result<bool, String> {
    let mut changed = 0u64;
    let mut overlap = 0u64;
    let mut aligned = 0u64;
    let mut before = 0u64;
    let mut step = 0u64;
    let mut after = 0u64;
    for (index, ((a, b), c)) in anchor.iter().zip(previous).zip(current).enumerate() {
        if index % 4096 == 0 {
            crate::process::check_cancelled().map_err(|_| "Canceled.".to_string())?;
        }
        let left = i16::from(*b) - i16::from(*a);
        let right = i16::from(*c) - i16::from(*b);
        changed += u64::from(left != 0 || right != 0);
        if left != 0 && right != 0 {
            overlap += 1;
            aligned += u64::from(left.signum() == right.signum());
        }
        before += u64::from(left.unsigned_abs());
        step += u64::from(right.unsigned_abs());
        after += u64::from(a.abs_diff(*c));
    }
    Ok(changed > 0
        && overlap * 4 >= changed * 3
        && aligned * 10 >= overlap * 9
        && before > 0
        && step * 2 >= before
        && before * 2 >= step
        && after * 4 >= before * 5)
}

struct Luma {
    pixels: Vec<u8>,
    width: usize,
    height: usize,
}
impl Luma {
    fn new(rgb: &[u8], width: usize, height: usize) -> Result<Self, String> {
        let scale = width.div_ceil(160).max(height.div_ceil(90)).max(1);
        let (w, h) = (width.div_ceil(scale), height.div_ceil(scale));
        let mut pixels = Vec::with_capacity(w * h);
        for y in 0..h {
            crate::process::check_cancelled().map_err(|_| "Canceled.".to_string())?;
            for x in 0..w {
                let mut sum = 0u64;
                let mut count = 0u64;
                for sy in y * scale..((y + 1) * scale).min(height) {
                    for sx in x * scale..((x + 1) * scale).min(width) {
                        let offset = (sy * width + sx) * 3;
                        sum += u64::from(rgb[offset]) * 54
                            + u64::from(rgb[offset + 1]) * 183
                            + u64::from(rgb[offset + 2]) * 19;
                        count += 1;
                    }
                }
                pixels.push(((sum + count * 128) / (count * 256)) as u8);
            }
        }
        Ok(Self {
            pixels,
            width: w,
            height: h,
        })
    }
    fn half(&self) -> Result<Self, String> {
        let (width, height) = (self.width.div_ceil(2), self.height.div_ceil(2));
        let mut pixels = Vec::with_capacity(width * height);
        for y in 0..height {
            crate::process::check_cancelled().map_err(|_| "Canceled.".to_string())?;
            for x in 0..width {
                let mut total = 0u32;
                let mut count = 0u32;
                for sy in y * 2..((y + 1) * 2).min(self.height) {
                    for sx in x * 2..((x + 1) * 2).min(self.width) {
                        total += u32::from(self.pixels[sy * self.width + sx]);
                        count += 1;
                    }
                }
                pixels.push(((total + count / 2) / count) as u8);
            }
        }
        Ok(Self {
            pixels,
            width,
            height,
        })
    }
    fn patch(&self, x: usize, y: usize) -> [u8; 25] {
        let mut patch = [0; 25];
        for py in 0..5 {
            patch[py * 5..py * 5 + 5].copy_from_slice(
                &self.pixels[(y + py - 2) * self.width + x - 2..(y + py - 2) * self.width + x + 3],
            );
        }
        patch
    }
    fn patch_half(&self, x: i32, y: i32) -> [u8; 25] {
        let mut patch = [0; 25];
        for py in 0..5 {
            for px in 0..5 {
                let sx = (x + (px as i32 - 2) * 2).clamp(0, (self.width as i32 - 1) * 2) as usize;
                let sy = (y + (py as i32 - 2) * 2).clamp(0, (self.height as i32 - 1) * 2) as usize;
                let (ix, iy, fx, fy) = (sx / 2, sy / 2, (sx % 2) as u32, (sy % 2) as u32);
                let right = (ix + 1).min(self.width - 1);
                let bottom = (iy + 1).min(self.height - 1);
                let sum = u32::from(self.pixels[iy * self.width + ix]) * (2 - fx) * (2 - fy)
                    + u32::from(self.pixels[iy * self.width + right]) * fx * (2 - fy)
                    + u32::from(self.pixels[bottom * self.width + ix]) * (2 - fx) * fy
                    + u32::from(self.pixels[bottom * self.width + right]) * fx * fy;
                patch[py * 5 + px] = ((sum + 2) / 4) as u8;
            }
        }
        patch
    }
}

// A bounded correspondence check, not an estimate of the missing true motion.
// Textured patches must have a distinct match and agree spatially. Repetitive
// texture, flat areas and unrelated same-histogram images may be unqualified.
fn continuous(
    a: &[u8],
    b: &[u8],
    width: usize,
    height: usize,
    policy: Policy,
) -> Result<bool, String> {
    let a = Luma::new(a, width, height)?;
    let b = Luma::new(b, width, height)?;
    Ok(correspondence(&a, &b)?.continuous(policy))
}

// Removing an already-qualified hold needs continuity across the deletion
// seam, but need not reconstruct its long missing trajectory. One fixed larger
// spatial support can retain scene correspondence despite fine occlusion or
// parallax. Descriptor-space gates stay the same; their source-space support
// and tolerance double. This evidence never enables retained-time Fill.
fn removable_continuous(
    a: &[u8],
    b: &[u8],
    width: usize,
    height: usize,
    policy: Policy,
) -> Result<bool, String> {
    let a = Luma::new(a, width, height)?.half()?;
    let b = Luma::new(b, width, height)?.half()?;
    Ok(correspondence(&a, &b)?.continuous(policy))
}

#[derive(Default)]
struct Correspondence {
    textured: usize,
    matches: Vec<(usize, usize, i32, i32)>,
    coherent: usize,
}
impl Correspondence {
    fn continuous(&self, policy: Policy) -> bool {
        policy.accepts_matches(self.textured, self.matches.len(), self.coherent)
    }
    fn stable_vector(&self, policy: Policy) -> Option<MotionVector> {
        if !self.continuous(policy) {
            return None;
        }
        let moving: Vec<_> = self
            .matches
            .iter()
            .filter(|m| m.2 != 0 || m.3 != 0)
            .collect();
        let cluster = moving
            .iter()
            .map(|center| {
                moving
                    .iter()
                    .copied()
                    .filter(|m| (center.2 - m.2).abs() <= 6 && (center.3 - m.3).abs() <= 6)
                    .collect::<Vec<_>>()
            })
            .filter(|c| spread(c))
            .max_by_key(Vec::len)?;
        let vector = MotionVector {
            x: median(cluster.iter().map(|m| m.2 as f64).collect()),
            y: median(cluster.iter().map(|m| m.3 as f64).collect()),
        };
        let magnitude = vector.magnitude();
        let deviation = median(
            cluster
                .iter()
                .map(|m| (m.2 as f64 - vector.x).hypot(m.3 as f64 - vector.y))
                .collect(),
        );
        let aligned = cluster
            .iter()
            .filter(|m| {
                vector.agrees(MotionVector {
                    x: m.2 as f64,
                    y: m.3 as f64,
                })
            })
            .count();
        (magnitude >= 2.0
            && deviation <= 1.0f64.max(magnitude * 0.5)
            && aligned * 3 >= cluster.len() * 2)
            .then_some(vector)
    }
    fn held(&self) -> bool {
        let zero: Vec<_> = self
            .matches
            .iter()
            .filter(|m| m.2 == 0 && m.3 == 0)
            .collect();
        zero.len() >= 12
            && zero.len() * 4 >= self.matches.len() * 3
            && zero.len() * 2 >= self.textured
            && spread(&zero)
    }
}
fn spread(matches: &[&(usize, usize, i32, i32)]) -> bool {
    (0..8).filter(|x| matches.iter().any(|m| m.0 == *x)).count() >= 4
        && (0..6).filter(|y| matches.iter().any(|m| m.1 == *y)).count() >= 3
}
fn correspondence(a: &Luma, b: &Luma) -> Result<Correspondence, String> {
    if a.width < 16 || a.height < 12 {
        return Ok(Correspondence::default());
    }
    let mut matches = Vec::new();
    let mut textured = 0;
    #[cfg(test)]
    let mut match_scores = Vec::new();
    for ty in 0..6 {
        for tx in 0..8 {
            crate::process::check_cancelled().map_err(|_| "Canceled.".to_string())?;
            let x = ((2 * tx + 1) * a.width / 16).clamp(2, a.width - 3);
            let y = ((2 * ty + 1) * a.height / 12).clamp(2, a.height - 3);
            let patch = a.patch(x, y);
            let mean = patch.iter().map(|p| u32::from(*p)).sum::<u32>() / 25;
            if patch
                .iter()
                .map(|p| u32::from(*p).abs_diff(mean))
                .sum::<u32>()
                < 25 * 4
            {
                continue;
            }
            textured += 1;
            let mut scores = Vec::with_capacity(33 * 33);
            for dy in -16i32..=16 {
                for dx in -16i32..=16 {
                    let (sx, sy) = (x as i32 + dx, y as i32 + dy);
                    if sx < 2 || sy < 2 || sx + 2 >= b.width as i32 || sy + 2 >= b.height as i32 {
                        continue;
                    }
                    let other = b.patch(sx as usize, sy as usize);
                    let score = patch
                        .iter()
                        .zip(other)
                        .map(|(l, r)| u32::from(l.abs_diff(r)))
                        .sum::<u32>();
                    scores.push((score, dx, dy));
                }
            }
            // Integer-only matching penalizes ordinary fractional camera
            // displacement. Refine up to eight separated hypotheses equally,
            // including alternatives, rather than improving only the winner.
            scores.sort_unstable_by_key(|s| s.0);
            let mut seeds: Vec<(u32, i32, i32)> = Vec::with_capacity(8);
            for candidate in &scores {
                if seeds
                    .iter()
                    .all(|s| (s.1 - candidate.1).abs() > 2 || (s.2 - candidate.2).abs() > 2)
                {
                    seeds.push(*candidate);
                    if seeds.len() == 8 {
                        break;
                    }
                }
            }
            let refined: Vec<_> = seeds
                .iter()
                .map(|(_, dx, dy)| {
                    let mut best = (u32::MAX, 0, 0);
                    for oy in -1..=1 {
                        for ox in -1..=1 {
                            let (dx, dy) = (dx * 2 + ox, dy * 2 + oy);
                            let other = b.patch_half(x as i32 * 2 + dx, y as i32 * 2 + dy);
                            let score = patch
                                .iter()
                                .zip(other)
                                .map(|(l, r)| u32::from(l.abs_diff(r)))
                                .sum();
                            if score < best.0 {
                                best = (score, dx, dy);
                            }
                        }
                    }
                    best
                })
                .collect();
            let Some(&(best, dx, dy)) = refined.iter().min_by_key(|s| s.0) else {
                continue;
            };
            let alternative = refined
                .iter()
                .filter(|s| (s.1 - dx).abs() > 4 || (s.2 - dy).abs() > 4)
                .map(|s| s.0)
                .min()
                .unwrap_or(0);
            #[cfg(test)]
            match_scores.push((best, alternative));
            if best <= 25 * 10 && alternative >= best + 25 && best * 4 <= alternative * 3 {
                matches.push((tx, ty, dx, dy));
            }
        }
    }
    let coherent = coherent_matches(&matches);

    #[cfg(test)]
    if std::env::var_os("VFL_ANTI_MATCH_TRACE").is_some() {
        eprintln!(
            "correspondence textured={textured} matched={} spread_nonzero_cluster={coherent} quality={} separated={} scores={match_scores:?}",
            matches.len(),
            match_scores.iter().filter(|s| s.0 <= 250).count(),
            match_scores
                .iter()
                .filter(|s| s.1 >= s.0 + 25 && s.0 * 4 <= s.1 * 3)
                .count()
        );
    }
    Ok(Correspondence {
        textured,
        matches,
        coherent,
    })
}

fn coherent_matches(matches: &[(usize, usize, i32, i32)]) -> usize {
    // Static HUD/background matches cannot establish continuity for unrelated
    // moving content. Require spatially distributed matched displacement.
    let moving: Vec<_> = matches.iter().filter(|m| m.2 != 0 || m.3 != 0).collect();
    moving
        .iter()
        .map(|center| {
            let cluster: Vec<_> = moving
                .iter()
                .filter(|n| (center.2 - n.2).abs() <= 6 && (center.3 - n.3).abs() <= 6)
                .collect();
            let columns = (0..8).filter(|x| cluster.iter().any(|m| m.0 == *x)).count();
            let rows = (0..6).filter(|y| cluster.iter().any(|m| m.1 == *y)).count();
            if columns >= 4 && rows >= 3 {
                cluster.len()
            } else {
                0
            }
        })
        .max()
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug)]
struct MotionVector {
    x: f64,
    y: f64,
}
impl MotionVector {
    fn magnitude(self) -> f64 {
        self.x.hypot(self.y)
    }
    fn agrees(self, other: Self) -> bool {
        self.x * other.x + self.y * other.y >= 0.5 * self.magnitude() * other.magnitude()
    }
}
#[derive(Clone, Copy)]
struct MotionSample {
    vector: MotionVector,
    duration_ns: u64,
}
struct MotionAnchor {
    image: Luma,
    start_ns: u64,
    end_ns: u64,
    first_ns: u64,
    source_steps: usize,
}
fn reference_interval(duration: u64, first: u64, steps: usize) -> bool {
    duration > 0
        && duration <= 100_000_002
        && first > 0
        && first <= duration
        && first.saturating_mul(2).saturating_add(2) >= duration
        && (1..=2).contains(&steps)
}
fn median(mut values: Vec<f64>) -> f64 {
    values.sort_unstable_by(f64::total_cmp);
    (values[(values.len() - 1) / 2] + values[values.len() / 2]) * 0.5
}
fn bridge_duration(
    first: u64,
    span: u64,
    bridge: Option<MotionVector>,
    before: &[MotionSample],
    after: &[MotionSample],
) -> (u64, &'static str) {
    let Some(bridge) = bridge else {
        return (first, "insufficientMotionContext");
    };
    let both = !before.is_empty() && !after.is_empty();
    if !both && before.len().max(after.len()) < 2 {
        return (first, "insufficientMotionContext");
    }
    let samples: Vec<_> = before.iter().chain(after).collect();
    let mut fastest = 0.0f64;
    let mut slowest = f64::INFINITY;
    for (i, sample) in samples.iter().enumerate() {
        if sample.duration_ns == 0
            || !sample.vector.agrees(bridge)
            || samples[..i]
                .iter()
                .any(|other| !sample.vector.agrees(other.vector))
        {
            return (first, "conflictingMotion");
        }
        let speed = sample.vector.magnitude() / sample.duration_ns as f64;
        if !speed.is_finite() || speed <= 0.0 {
            return (first, "insufficientMotionContext");
        }
        fastest = fastest.max(speed);
        slowest = slowest.min(speed);
    }
    if fastest > slowest * 2.0 {
        return (first, "conflictingMotion");
    }
    let mut maximum = 250_000_000.min(span / 2).max(first);
    if !both {
        maximum = maximum.min(first.saturating_mul(2));
    }
    let estimated = (bridge.magnitude() / fastest).round();
    if !estimated.is_finite() || estimated <= 0.0 {
        return (first, "insufficientMotionContext");
    }
    let retained = (estimated as u64).clamp(first, maximum);
    (
        retained,
        if retained > first {
            "motionEstimate"
        } else {
            "boundedMinimum"
        },
    )
}

struct Picture {
    pixels: Vec<u8>,
    pts: u64,
    incoming: Option<Evidence>,
}

struct Skipped {
    start: u64,
    repeated: u64,
    reason: &'static str,
}

#[derive(Debug)]
pub(super) struct Span {
    pub right: Option<usize>,
    pub repaired: bool,
    pub cut: bool,
    pub remove_start: Option<u64>,
    detail: Option<usize>,
    motion: Option<Evidence>,
}

#[derive(Clone, Copy)]
struct Cadence {
    duration_ns: u64,
    source_steps: usize,
}
impl Cadence {
    fn matches(self, other: Self) -> bool {
        self.source_steps == other.source_steps
            && (self.duration_ns.abs_diff(other.duration_ns) <= 2
                || u128::from(self.duration_ns.abs_diff(other.duration_ns)) * 100
                    <= u128::from(self.duration_ns.max(other.duration_ns)) * 5)
    }
}

enum CadenceVerdict {
    Regular,
    Irregular,
    Unknown,
}

fn regular_cadence(values: &[Cadence], current: usize) -> bool {
    (1..=4).any(|period| {
        let needed = period * 2 + 1;
        values.len() >= needed
            && (0..=values.len() - needed).any(|start| {
                start <= current
                    && current < start + needed
                    && (start + period..start + needed)
                        .all(|i| values[i].matches(values[i - period]))
            })
    })
}

pub(super) struct Window {
    policy: Policy,
    mode: AntiStutterMode,
    cadence_history: VecDeque<Cadence>,
    cadence_open_steps: usize,
    removed_ns: u64,
    retained_bridge_ns: u64,
    motion_history: VecDeque<MotionAnchor>,
    current_descriptor: Option<Luma>,
    motion_first_ns: Option<u64>,
    pictures: VecDeque<Picture>,
    pool: Vec<Vec<u8>>,
    width: usize,
    height: usize,
    frame_bytes: usize,
    duration_ns: u64,
    eof: bool,
    skipped: Option<Skipped>,
    previous_motion: Option<(Evidence, u64)>,
    hold_started: Option<u64>,
    at_source_beginning: bool,
    pub cut_pairs: u64,
    pub high_water: usize,
    pub allocated_high_water: usize,
    pub diagnostics: AntiStutterDiagnostics,
}

impl Window {
    pub fn new(
        pixels: Vec<u8>,
        pts: u64,
        width: usize,
        height: usize,
        duration_ns: u64,
        real_first: bool,
        strength: u8,
    ) -> Self {
        let frame_bytes = pixels.len();
        Self {
            policy: Policy { strength },
            mode: AntiStutterMode::Repair,
            cadence_history: VecDeque::new(),
            cadence_open_steps: 0,
            removed_ns: 0,
            retained_bridge_ns: 0,
            motion_history: VecDeque::new(),
            current_descriptor: None,
            motion_first_ns: None,
            pictures: VecDeque::from([Picture {
                pixels,
                pts,
                incoming: None,
            }]),
            pool: Vec::new(),
            width,
            height,
            frame_bytes,
            duration_ns,
            eof: false,
            skipped: None,
            previous_motion: None,
            hold_started: None,
            at_source_beginning: false,
            cut_pairs: 0,
            high_water: 1,
            allocated_high_water: 1,
            diagnostics: AntiStutterDiagnostics {
                version: 3,
                strength,
                mode: None,
                removed_duration_s: None,
                removed_frames: None,
                removed_runs: None,
                bridged_runs: None,
                retained_bridge_duration_s: None,
                analyzed_frames: u64::from(real_first),
                candidate_runs: 0,
                repaired_runs: 0,
                repaired_frames: 0,
                unchanged_runs: 0,
                intervals: Vec::new(),
            },
        }
    }

    pub fn at_source_beginning(mut self, verified: bool) -> Self {
        self.at_source_beginning = verified;
        self
    }

    pub fn with_mode(mut self, mode: AntiStutterMode) -> Self {
        self.mode = mode;
        if mode == AntiStutterMode::Remove {
            self.diagnostics.version = 5;
            self.diagnostics.mode = Some(mode);
            self.diagnostics.removed_duration_s = Some(0.0);
            self.diagnostics.removed_frames = Some(0);
            self.diagnostics.removed_runs = Some(0);
            self.diagnostics.bridged_runs = Some(0);
            self.diagnostics.retained_bridge_duration_s = Some(0.0);
        }
        self
    }

    fn ensure(
        &mut self,
        count: usize,
        read: &mut impl FnMut(&mut [u8]) -> Result<Option<u64>, String>,
    ) -> Result<bool, String> {
        if count > MAX_PICTURES
            || self
                .frame_bytes
                .checked_mul(count)
                .is_none_or(|bytes| bytes > MAX_BUFFER_BYTES)
        {
            return Ok(false);
        }
        while self.pictures.len() < count && !self.eof {
            crate::process::check_cancelled().map_err(|_| "Canceled.".to_string())?;
            let mut pixels = if let Some(pixels) = self.pool.pop() {
                pixels
            } else {
                let mut pixels = Vec::new();
                pixels.try_reserve_exact(self.frame_bytes).map_err(|_| {
                    "Not enough memory for bounded Anti-stutter lookahead.".to_string()
                })?;
                pixels.resize(self.frame_bytes, 0);
                pixels
            };
            self.allocated_high_water = self
                .allocated_high_water
                .max(self.pictures.len() + self.pool.len() + 1);
            let Some(pts) = read(&mut pixels)? else {
                self.pool.push(pixels);
                self.eof = true;
                break;
            };
            let previous = self
                .pictures
                .back()
                .ok_or("Missing Anti-stutter source anchor.")?;
            if pts <= previous.pts {
                return Err(
                    "Anti-stutter requires strictly increasing presentation timestamps.".into(),
                );
            }
            let mut evidence = compare(&previous.pixels, &pixels, self.width, self.height)?;
            evidence.delta_ns = pts - previous.pts;
            self.cut_pairs += u64::from(evidence.cut);
            self.pictures.push_back(Picture {
                pixels,
                pts,
                incoming: Some(evidence),
            });
            self.diagnostics.analyzed_frames += 1;
            self.high_water = self.high_water.max(self.pictures.len());
        }
        Ok(self.pictures.len() >= count)
    }

    pub fn pixels(&self, index: usize) -> &[u8] {
        &self.pictures[index].pixels
    }
    pub fn pts(&self, index: usize) -> u64 {
        self.pictures[index].pts
    }

    fn detail(&mut self, start: u64, end: u64, repeated: u64, reason: &str) -> Option<usize> {
        #[cfg(test)]
        if std::env::var_os("VFL_ANTI_FULL_TRACE").is_some() {
            eprintln!("decision {start} {end} repeats={repeated} reason={reason}");
        }
        if self.diagnostics.intervals.len() == DETAIL_LIMIT {
            return None;
        }
        let index = self.diagnostics.intervals.len();
        self.diagnostics.intervals.push(Interval {
            start_s: start as f64 / 1e9,
            end_s: end as f64 / 1e9,
            source_repeated_frames: repeated,
            repaired_frames: 0,
            reason: reason.into(),
            bridge_duration_s: None,
            bridge_reason: None,
        });
        Some(index)
    }

    fn finish_skip(&mut self, end: u64) {
        if let Some(skipped) = self.skipped.take() {
            self.detail(skipped.start, end, skipped.repeated, skipped.reason);
        }
    }

    fn unchanged(&mut self, reason: &'static str) -> Span {
        if self.mode == AntiStutterMode::Remove
            && matches!(
                reason,
                "sceneCut" | "gradualDrift" | "resourceLimit" | "excessiveHold" | "uncertainMotion"
            )
        {
            self.motion_history.clear();
            self.current_descriptor = None;
            self.motion_first_ns = None;
        }
        self.diagnostics.unchanged_runs += 1;
        self.skipped = Some(Skipped {
            start: self.pts(0),
            repeated: 0,
            reason,
        });
        self.ordinary()
    }

    fn ordinary(&mut self) -> Span {
        if self.pictures.len() < 2 {
            self.finish_skip(self.duration_ns);
            return Span {
                right: None,
                repaired: false,
                cut: false,
                remove_start: None,
                detail: None,
                motion: None,
            };
        }
        let incoming = self.pictures[1]
            .incoming
            .expect("every noninitial picture has evidence");
        if incoming.plateau() {
            if let Some(skipped) = &mut self.skipped {
                skipped.repeated += 1;
            }
        } else {
            self.finish_skip(self.pts(1));
        }
        Span {
            right: Some(1),
            repaired: false,
            cut: incoming.cut,
            remove_start: None,
            detail: None,
            motion: None,
        }
    }

    fn qualify(
        &mut self,
        right: usize,
        residual: u64,
        read: &mut impl FnMut(&mut [u8]) -> Result<Option<u64>, String>,
    ) -> Result<Option<&'static str>, String> {
        let before = match self.previous_motion {
            Some((before, ended)) if ended == self.pts(0) && before.delta_ns <= MAX_SPAN_NS => {
                Some(before)
            }
            None if self.at_source_beginning => None,
            _ => return Ok(Some("missingMotionContext")),
        };
        let bridge = compare(self.pixels(0), self.pixels(right), self.width, self.height)?;
        if bridge.discontinuity() {
            return Ok(Some("sceneCut"));
        }
        if !bridge.moving() {
            return Ok(Some("missingMotionContext"));
        }
        // Recurring held frames are an explicit opt-in target. Find the next
        // distinct picture without pretending every decoded edge is motion.
        let mut post = right + 1;
        let after = loop {
            if !self.ensure(post + 1, read)? {
                return Ok(Some(if self.eof {
                    "missingMotionContext"
                } else {
                    "resourceLimit"
                }));
            }
            let delta = self.pts(post) - self.pts(right);
            if delta > MAX_SPAN_NS {
                return Ok(Some("missingMotionContext"));
            }
            let mut evidence = compare(
                self.pixels(right),
                self.pixels(post),
                self.width,
                self.height,
            )?;
            evidence.delta_ns = delta;
            if evidence.discontinuity()
                || self.pictures[post]
                    .incoming
                    .is_some_and(Evidence::discontinuity)
            {
                return Ok(Some("sceneCut"));
            }
            if evidence.moving() {
                break evidence;
            }
            post += 1;
        };
        // Compression residual is amplitude, not velocity. A long recurring
        // hold in the next context must not turn stable codec noise into fast
        // motion. Both distinct flanks still share the bounded source clock.
        for context in before.into_iter().chain([after, bridge]) {
            if !self.policy.accepts_residual(residual, context) {
                return Ok(Some("uncertainMotion"));
            }
        }
        if before.is_none()
            && !continuous(
                self.pixels(right),
                self.pixels(post),
                self.width,
                self.height,
                self.policy,
            )?
        {
            return Ok(Some("missingMotionContext"));
        }
        #[cfg(test)]
        if std::env::var_os("VFL_ANTI_MATCH_TRACE").is_some() {
            eprintln!(
                "candidate {} -> {} post {}",
                self.pts(0),
                self.pts(right),
                self.pts(post)
            );
        }
        if !continuous(
            self.pixels(0),
            self.pixels(right),
            self.width,
            self.height,
            self.policy,
        )? {
            return Ok(Some("unreliableCorrespondence"));
        }
        Ok(None)
    }

    fn cadence_verdict(
        &mut self,
        right: usize,
        read: &mut impl FnMut(&mut [u8]) -> Result<Option<u64>, String>,
    ) -> Result<CadenceVerdict, String> {
        let mut signatures: Vec<_> = self.cadence_history.iter().copied().collect();
        let current = signatures.len();
        signatures.push(Cadence {
            duration_ns: self.pts(right) - self.pts(0),
            source_steps: right,
        });
        let mut start = right;
        loop {
            if regular_cadence(&signatures, current) {
                return Ok(CadenceVerdict::Regular);
            }
            let forward = &signatures[current..];
            if (1..=4).all(|period| {
                forward.len() > period
                    && (period..forward.len()).any(|i| !forward[i].matches(forward[i - period]))
            }) {
                return Ok(CadenceVerdict::Irregular);
            }
            if signatures.len() - current > 8 {
                return Ok(CadenceVerdict::Unknown);
            }
            let mut next = start + 1;
            loop {
                if !self.ensure(next + 1, read)? || self.pts(next) - self.pts(right) > MAX_SPAN_NS {
                    return Ok(CadenceVerdict::Unknown);
                }
                let incoming = self.pictures[next].incoming.expect("cadence source edge");
                let first = if next == start + 1 {
                    incoming
                } else {
                    compare(
                        self.pixels(start),
                        self.pixels(next),
                        self.width,
                        self.height,
                    )?
                };
                if incoming.discontinuity() || first.discontinuity() {
                    return Ok(CadenceVerdict::Unknown);
                }
                if !incoming.plateau() {
                    if !first.moving() {
                        return Ok(CadenceVerdict::Unknown);
                    }
                    signatures.push(Cadence {
                        duration_ns: self.pts(next) - self.pts(start),
                        source_steps: next - start,
                    });
                    start = next;
                    break;
                }
                if !first.plateau()
                    || (next > start + 1
                        && drifting(self.pixels(start), self.pixels(next - 1), self.pixels(next))?)
                {
                    return Ok(CadenceVerdict::Unknown);
                }
                next += 1;
            }
        }
    }

    fn adaptive_bridge(
        &mut self,
        right: usize,
        read: &mut impl FnMut(&mut [u8]) -> Result<Option<u64>, String>,
    ) -> Result<(u64, &'static str), String> {
        let first = self.pts(1) - self.pts(0);
        let span = self.pts(right) - self.pts(0);
        if 250_000_000.min(span / 2).max(first) == first {
            return Ok((first, "boundedMinimum"));
        }
        let left = self
            .current_descriptor
            .as_ref()
            .expect("Remove descriptor prepared");
        let right_image = Luma::new(self.pixels(right), self.width, self.height)?.half()?;
        let bridge = correspondence(left, &right_image)?.stable_vector(self.policy);
        let mut before = Vec::new();
        for (i, anchor) in self.motion_history.iter().enumerate() {
            let endpoint = if let Some(next) = self.motion_history.get(i + 1) {
                if anchor.end_ns != next.start_ns {
                    continue;
                }
                &next.image
            } else {
                if anchor.end_ns != self.pts(0) {
                    continue;
                }
                left
            };
            if reference_interval(
                anchor.end_ns - anchor.start_ns,
                anchor.first_ns,
                anchor.source_steps,
            ) && let Some(vector) =
                correspondence(&anchor.image, endpoint)?.stable_vector(self.policy)
            {
                before.push(MotionSample {
                    vector,
                    duration_ns: anchor.end_ns - anchor.start_ns,
                });
            }
        }
        let after = self.future_motion(right, read)?;
        let result = bridge_duration(
            first,
            self.pts(right) - self.pts(0),
            bridge,
            &before,
            &after,
        );
        #[cfg(test)]
        if std::env::var_os("VFL_ANTI_FULL_TRACE").is_some() {
            eprintln!(
                "bridge {} {} first={first} retained={} reason={} before={} after={} displacement={bridge:?}",
                self.pts(0),
                self.pts(right),
                result.0,
                result.1,
                before.len(),
                after.len()
            );
        }
        Ok(result)
    }

    fn future_motion(
        &mut self,
        right: usize,
        read: &mut impl FnMut(&mut [u8]) -> Result<Option<u64>, String>,
    ) -> Result<Vec<MotionSample>, String> {
        let mut after = Vec::new();
        let mut start = right;
        for _ in 0..2 {
            let mut next = start + 1;
            let endpoint = loop {
                if !self.ensure(next + 1, read)? || self.pts(next) - self.pts(right) > MAX_SPAN_NS {
                    break None;
                }
                let evidence = compare(
                    self.pixels(start),
                    self.pixels(next),
                    self.width,
                    self.height,
                )?;
                if evidence.discontinuity()
                    || evidence.ambiguous
                    || self.pictures[next]
                        .incoming
                        .is_some_and(|e| e.discontinuity() || e.ambiguous)
                {
                    break None;
                }
                if evidence.moving() {
                    break Some(next);
                }
                if !evidence.plateau()
                    || (next > start + 1
                        && drifting(self.pixels(start), self.pixels(next - 1), self.pixels(next))?)
                {
                    break None;
                }
                next += 1;
            };
            let Some(end) = endpoint else {
                break;
            };
            let duration = self.pts(end) - self.pts(start);
            if reference_interval(duration, self.pts(start + 1) - self.pts(start), end - start) {
                let a = Luma::new(self.pixels(start), self.width, self.height)?.half()?;
                let b = Luma::new(self.pixels(end), self.width, self.height)?.half()?;
                if let Some(vector) = correspondence(&a, &b)?.stable_vector(self.policy) {
                    after.push(MotionSample {
                        vector,
                        duration_ns: duration,
                    });
                }
            } else {
                break;
            }
            start = end;
        }
        Ok(after)
    }

    pub fn next(
        &mut self,
        read: &mut impl FnMut(&mut [u8]) -> Result<Option<u64>, String>,
    ) -> Result<Option<Span>, String> {
        if self.pictures.is_empty() {
            return Ok(None);
        }
        if self.mode == AntiStutterMode::Remove
            && self.current_descriptor.is_none()
            && self.skipped.is_none()
        {
            self.current_descriptor =
                Some(Luma::new(self.pixels(0), self.width, self.height)?.half()?);
        }
        if !self.ensure(2, read)? {
            return Ok(Some(self.ordinary()));
        }
        if self.skipped.is_some()
            || !self.pictures[1]
                .incoming
                .expect("incoming evidence")
                .plateau()
        {
            return Ok(Some(self.ordinary()));
        }
        self.diagnostics.candidate_runs += 1;
        let mut right = 1;
        let mut residual = 0;
        loop {
            if !self.ensure(right + 1, read)? {
                return Ok(Some(self.unchanged(if self.eof {
                    "missingMotionContext"
                } else {
                    "resourceLimit"
                })));
            }
            if self.pts(right) - self.pts(0) > MAX_SPAN_NS {
                return Ok(Some(self.unchanged("excessiveHold")));
            }
            if self.pictures[right]
                .incoming
                .is_some_and(Evidence::discontinuity)
            {
                return Ok(Some(self.unchanged("sceneCut")));
            }
            if !self.pictures[right]
                .incoming
                .expect("incoming evidence")
                .plateau()
            {
                break;
            }
            // Comparing only adjacent pictures would turn a slow drift into
            // a sequence of artificial holds. Retire the entire ambiguous run.
            let first = if right == 1 {
                self.pictures[right].incoming.expect("incoming evidence")
            } else {
                compare(self.pixels(0), self.pixels(right), self.width, self.height)?
            };
            residual = residual.max(first.coarse_mean);
            if first.discontinuity() {
                return Ok(Some(self.unchanged("sceneCut")));
            }
            if !first.plateau() {
                return Ok(Some(self.unchanged("gradualDrift")));
            }
            if right >= 2 && drifting(self.pixels(0), self.pixels(right - 1), self.pixels(right))? {
                return Ok(Some(self.unchanged("gradualDrift")));
            }
            if right > MAX_REPEATED {
                return Ok(Some(self.unchanged("excessiveHold")));
            }
            right += 1;
        }
        let coarse_removal = match self.qualify(right, residual, read)? {
            None => false,
            Some("unreliableCorrespondence")
                if self.mode == AntiStutterMode::Remove
                    && removable_continuous(
                        self.pixels(0),
                        self.pixels(right),
                        self.width,
                        self.height,
                        self.policy,
                    )? =>
            {
                true
            }
            Some(reason) => return Ok(Some(self.unchanged(reason))),
        };
        // Cadence protects source time, not the unwanted held-picture motion.
        // A fully qualified hold can still use the existing Fill interpolation
        // when its time must be retained or cadence evidence is incomplete.
        let (mut remove_start, reason) = if self.mode == AntiStutterMode::Remove {
            match self.cadence_verdict(right, read)? {
                CadenceVerdict::Regular | CadenceVerdict::Unknown if coarse_removal => {
                    return Ok(Some(self.unchanged("unreliableCorrespondence")));
                }
                CadenceVerdict::Regular => (None, "regularCadence"),
                CadenceVerdict::Unknown => (None, "insufficientCadence"),
                CadenceVerdict::Irregular => (Some(self.pts(1)), "removed"),
            }
        } else {
            (None, "repaired")
        };
        let mut bridge_info = None;
        if remove_start.is_some() {
            let (duration, why) = self.adaptive_bridge(right, read)?;
            remove_start = Some(self.pts(0) + duration);
            bridge_info = Some((duration, why));
        }
        let detail = self.detail(
            remove_start.unwrap_or(self.pts(0)),
            self.pts(right),
            (right - 1) as u64,
            reason,
        );
        if let (Some(detail), Some((duration, why))) = (detail, bridge_info) {
            self.diagnostics.intervals[detail].bridge_reason = Some(why.into());
            if duration > self.pts(1) - self.pts(0) {
                self.diagnostics.intervals[detail].bridge_duration_s = Some(duration as f64 / 1e9);
            }
        }
        Ok(Some(Span {
            right: Some(right),
            repaired: true,
            cut: false,
            remove_start,
            detail,
            motion: Some(compare(
                self.pixels(0),
                self.pixels(right),
                self.width,
                self.height,
            )?),
        }))
    }

    pub fn advance(&mut self, span: Span, repaired_frames: u64) {
        if let Some(start) = span.remove_start {
            let right = span.right.expect("removed hold has changed endpoint");
            self.removed_ns += self.pts(right) - start;
            self.diagnostics.removed_duration_s = Some(self.removed_ns as f64 / 1e9);
            *self
                .diagnostics
                .removed_runs
                .as_mut()
                .expect("remove mode counters") += 1;
            *self
                .diagnostics
                .removed_frames
                .as_mut()
                .expect("remove mode counters") +=
                (1..right).filter(|i| self.pts(*i) >= start).count() as u64;
            let extra = start - self.pts(1);
            if extra > 0 {
                self.retained_bridge_ns += extra;
                *self
                    .diagnostics
                    .bridged_runs
                    .as_mut()
                    .expect("remove mode counters") += 1;
                self.diagnostics.retained_bridge_duration_s =
                    Some(self.retained_bridge_ns as f64 / 1e9);
            }
        } else if span.repaired {
            if repaired_frames > 0 {
                self.diagnostics.repaired_runs += 1;
            } else {
                self.diagnostics.unchanged_runs += 1;
                if let Some(detail) = span.detail {
                    self.diagnostics.intervals[detail].reason = "noOutputSamples".into();
                }
            }
        }
        self.diagnostics.repaired_frames += repaired_frames;
        if let Some(detail) = span.detail {
            self.diagnostics.intervals[detail].repaired_frames = repaired_frames;
        }
        if let Some(right) = span.right {
            self.at_source_beginning = false;
            let left_pts = self.pts(0);
            let right_pts = self.pts(right);
            let mut evidence = span
                .motion
                .unwrap_or_else(|| self.pictures[right].incoming.expect("source edge"));
            if self.mode == AntiStutterMode::Remove {
                if evidence.discontinuity() || evidence.ambiguous || span.remove_start.is_some() {
                    self.motion_history.clear();
                    self.current_descriptor = None;
                    self.motion_first_ns = None;
                } else if !span.repaired && evidence.plateau() {
                    self.motion_first_ns.get_or_insert(right_pts - left_pts);
                    if right_pts - self.hold_started.unwrap_or(left_pts) > MAX_SPAN_NS {
                        self.motion_history.clear();
                        self.current_descriptor = None;
                        self.motion_first_ns = None;
                    }
                } else {
                    let start = self.hold_started.unwrap_or(left_pts);
                    let first = self
                        .motion_first_ns
                        .take()
                        .unwrap_or_else(|| self.pts(1) - left_pts);
                    let steps = self.cadence_open_steps + right;
                    if evidence.moving() && reference_interval(right_pts - start, first, steps) {
                        if let Some(image) = self.current_descriptor.take() {
                            self.motion_history.push_back(MotionAnchor {
                                image,
                                start_ns: start,
                                end_ns: right_pts,
                                first_ns: first,
                                source_steps: steps,
                            });
                            if self.motion_history.len() > 2 {
                                self.motion_history.pop_front();
                            }
                        }
                    } else {
                        self.motion_history.clear();
                    }
                    self.current_descriptor = None;
                }
            }
            if evidence.discontinuity() {
                // Near similarity and a lighting/cut flag can coexist. A cut
                // always retires history before any new plateau is timed.
                self.previous_motion = None;
                self.hold_started = None;
                self.cadence_history.clear();
                self.cadence_open_steps = 0;
            } else if !span.repaired && evidence.plateau() {
                self.cadence_open_steps += right;
                let start = *self.hold_started.get_or_insert(left_pts);
                if right_pts - start > MAX_SPAN_NS {
                    self.previous_motion = None;
                    self.cadence_history.clear();
                }
            } else {
                let start = self.hold_started.take().unwrap_or(left_pts);
                evidence.delta_ns = right_pts - start;
                if self.mode == AntiStutterMode::Remove
                    && evidence.moving()
                    && evidence.delta_ns <= MAX_SPAN_NS
                {
                    self.cadence_history.push_back(Cadence {
                        duration_ns: evidence.delta_ns,
                        source_steps: self.cadence_open_steps + right,
                    });
                    if self.cadence_history.len() > 8 {
                        self.cadence_history.pop_front();
                    }
                } else {
                    self.cadence_history.clear();
                }
                self.cadence_open_steps = 0;
                self.previous_motion = if evidence.moving() && evidence.delta_ns <= MAX_SPAN_NS {
                    Some((evidence, right_pts))
                } else {
                    None
                };
            }
        } else {
            self.motion_history.clear();
            self.current_descriptor = None;
            self.motion_first_ns = None;
            self.previous_motion = None;
            self.hold_started = None;
            self.cadence_history.clear();
            self.cadence_open_steps = 0;
        }
        let count = span.right.unwrap_or(self.pictures.len());
        for _ in 0..count {
            if let Some(picture) = self.pictures.pop_front() {
                self.pool.push(picture.pixels);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpolation_timeline::Rate;

    const WIDTH: usize = 96;
    const HEIGHT: usize = 54;
    type TestSpan = (u64, Option<u64>, bool);
    type RunResult = (AntiStutterDiagnostics, usize, Vec<TestSpan>);
    fn picture(position: usize) -> Vec<u8> {
        picture_sized(WIDTH, HEIGHT, position as f64)
    }
    fn picture_sized(width: usize, height: usize, position: f64) -> Vec<u8> {
        let mut result = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            for x in 0..width {
                let x = x as f64 + position * 2.0;
                let y = y as f64;
                let value = (112.0
                    + 36.0 * (0.19 * x + 0.035 * y).sin()
                    + 24.0 * (0.071 * x - 0.139 * y).cos()
                    + 20.0 * (0.041 * x + 0.273 * y).sin())
                .round()
                .clamp(0.0, 255.0) as u8;
                result.extend([value; 3]);
            }
        }
        result
    }
    fn frames(positions: &[usize], step: u64) -> Vec<(Vec<u8>, u64)> {
        positions
            .iter()
            .enumerate()
            .map(|(i, p)| (picture(*p), i as u64 * step))
            .collect()
    }
    fn run(input: &[(Vec<u8>, u64)], fps: u32, speed: f64) -> RunResult {
        let result = run_at_strength(input, fps, speed, 0);
        // Every existing full-window negative also remains a negative at the
        // strongest setting, including first-anchor/cut/resource boundaries.
        if result.0.repaired_runs == 0 {
            assert_eq!(run_at_strength(input, fps, speed, 100).0.repaired_runs, 0);
        }
        result
    }
    fn run_at_strength(input: &[(Vec<u8>, u64)], fps: u32, speed: f64, strength: u8) -> RunResult {
        run_sized(input, fps, speed, strength, (WIDTH, HEIGHT), false)
    }

    fn remove(input: &[(Vec<u8>, u64)]) -> (AntiStutterDiagnostics, super::super::RemovalMap) {
        remove_sized(input, WIDTH, HEIGHT, 75)
    }
    fn remove_sized(
        input: &[(Vec<u8>, u64)],
        width: usize,
        height: usize,
        strength: u8,
    ) -> (AntiStutterDiagnostics, super::super::RemovalMap) {
        let duration = input.last().unwrap().1 + 33_333_333;
        let mut window = Window::new(
            input[0].0.clone(),
            input[0].1,
            width,
            height,
            duration,
            true,
            strength,
        )
        .at_source_beginning(input[0].1 == 0)
        .with_mode(AntiStutterMode::Remove);
        let mut map = super::super::RemovalMap::new(duration);
        let mut next = 1;
        let mut read = |pixels: &mut [u8]| {
            if next == input.len() {
                return Ok(None);
            }
            pixels.copy_from_slice(&input[next].0);
            let pts = input[next].1;
            next += 1;
            Ok(Some(pts))
        };
        let rate = Rate::for_playback(120, 1.0).unwrap();
        let mut output = 0;
        while let Some(span) = window.next(&mut read).unwrap() {
            if let Some(start) = span.remove_start {
                map.push(start, window.pts(span.right.unwrap())).unwrap();
            }
            let left = map.map_ns(window.pts(0));
            let right = span.right.map(|i| map.map_ns(window.pts(i)));
            let count = rate.frame_count(map.output_duration_ns()).unwrap();
            let mut filled = 0;
            while output < count && right.is_none_or(|r| rate.sample_before(output, r)) {
                let alpha = right
                    .map(|r| rate.alpha(output, left, r).unwrap())
                    .unwrap_or(0.0);
                filled += u64::from(
                    span.repaired && span.remove_start.is_none() && alpha > 0.0 && alpha < 1.0,
                );
                output += 1;
            }
            window.advance(span, filled);
        }
        assert!(window.allocated_high_water <= MAX_PICTURES);
        assert_eq!(
            window.diagnostics.removed_duration_s,
            Some(map.removed_duration_ns() as f64 / 1e9)
        );
        (window.diagnostics, map)
    }

    fn motion(x: f64, y: f64, duration_ns: u64) -> MotionSample {
        MotionSample {
            vector: MotionVector { x, y },
            duration_ns,
        }
    }
    #[test]
    fn adaptive_bridge_uses_real_motion_time_and_bounded_consistent_context() {
        let first = 66_666_667;
        let bridge = Some(MotionVector { x: 8., y: 0. });
        let before = [motion(3.5, 1., 100_000_000), motion(4., 0., 66_666_667)];
        assert_eq!(
            bridge_duration(first, 600_000_000, bridge, &before, &[]),
            (133_333_334, "motionEstimate")
        );
        assert_eq!(
            bridge_duration(first, 600_000_000, bridge, &before[..1], &[]),
            (first, "insufficientMotionContext")
        );
        assert_eq!(
            bridge_duration(
                first,
                600_000_000,
                bridge,
                &[motion(4., 0., 50_000_000)],
                &[motion(4., 0., 50_000_000)]
            ),
            (100_000_000, "motionEstimate")
        );
        assert_eq!(
            bridge_duration(
                first,
                600_000_000,
                bridge,
                &[motion(4., 0., 50_000_000)],
                &[motion(-4., 0., 50_000_000)]
            ),
            (first, "conflictingMotion")
        );
        assert_eq!(
            bridge_duration(
                first,
                600_000_000,
                bridge,
                &[motion(4., 0., 50_000_000)],
                &[motion(4., 0., 150_000_000)]
            ),
            (first, "conflictingMotion")
        );
        let both = [motion(4., 0., 50_000_000)];
        assert_eq!(
            bridge_duration(
                20_000_000,
                750_000_000,
                Some(MotionVector { x: 40., y: 0. }),
                &both,
                &both
            ),
            (250_000_000, "motionEstimate")
        );
        assert_eq!(
            bridge_duration(
                20_000_000,
                200_000_000,
                Some(MotionVector { x: 40., y: 0. }),
                &both,
                &both
            ),
            (100_000_000, "motionEstimate")
        );
        assert_eq!(
            bridge_duration(120_000_000, 180_000_000, bridge, &both, &both),
            (120_000_000, "boundedMinimum")
        );
        assert_eq!(
            bridge_duration(
                40_000_000,
                400_000_000,
                Some(MotionVector { x: 12., y: 0. }),
                &[motion(4., 0., 50_000_000)],
                &[motion(4., 0., 50_000_000)]
            ),
            (150_000_000, "motionEstimate")
        );
        assert!(reference_interval(66_666_667, 33_333_333, 2));
        assert!(!reference_interval(100_000_000, 30_000_000, 2));
        assert!(!reference_interval(166_666_667, 133_333_333, 2));
        assert!(!reference_interval(90_000_000, 45_000_000, 3));
    }
    #[test]
    fn adaptive_motion_requires_directional_support_not_boolean_continuity() {
        let field = |kind| {
            let matches: Vec<_> = (0..6)
                .flat_map(|y| {
                    (0..8).map(move |x| match kind {
                        0 => (x, y, 4, 0),
                        1 => (x, y, 4 + y as i32 / 3, x as i32 / 3 - 1),
                        2 => (x, y, 1, 0),
                        _ => (
                            x,
                            y,
                            if (x + y) % 3 == 0 { -2 } else { 4 },
                            if (x + y) % 2 == 0 { 4 } else { -4 },
                        ),
                    })
                })
                .collect();
            let coherent = coherent_matches(&matches);
            Correspondence {
                textured: 48,
                matches,
                coherent,
            }
        };
        for kind in [0, 1] {
            assert!(field(kind).stable_vector(Policy { strength: 75 }).is_some());
        }
        assert!(field(2).continuous(Policy { strength: 75 }));
        assert!(
            field(2).stable_vector(Policy { strength: 75 }).is_none(),
            "one half-pixel is quantization weak despite continuity"
        );
        assert!(
            field(3).stable_vector(Policy { strength: 75 }).is_none(),
            "mixed directions cannot silently become speed"
        );
    }
    #[test]
    fn adaptive_future_context_stops_at_contaminated_or_ambiguous_intervals() {
        for kind in 0..4 {
            let mut input = if kind == 2 {
                frames(&[0, 0, 0, 3, 5, 7], 33_333_333)
            } else {
                frames(&[0, 2, 4, 6], 33_333_333)
            };
            if kind == 1 {
                for (_, pts) in input.iter_mut().skip(1) {
                    *pts += 100_000_000;
                }
            }
            let mut window =
                Window::new(input[0].0.clone(), 0, WIDTH, HEIGHT, 500_000_000, true, 75)
                    .with_mode(AntiStutterMode::Remove);
            let mut i = 1;
            let mut read = |pixels: &mut [u8]| {
                if i == input.len() {
                    return Ok(None);
                }
                pixels.copy_from_slice(&input[i].0);
                let t = input[i].1;
                i += 1;
                Ok(Some(t))
            };
            if kind == 3 {
                window.ensure(2, &mut read).unwrap();
                window.pictures[1].incoming.as_mut().unwrap().ambiguous = true;
            }
            let samples = window.future_motion(0, &mut read).unwrap();
            if kind == 0 {
                assert_eq!(samples.len(), 2);
            } else {
                assert_eq!(samples.len(), 0, "kind={kind}");
            }
        }
        let input = frames(&[0, 0, 2, 4], 33_333_333);
        let mut window = Window::new(input[0].0.clone(), 0, WIDTH, HEIGHT, 200_000_000, true, 75)
            .with_mode(AntiStutterMode::Remove);
        let mut i = 1;
        window
            .ensure(2, &mut |pixels| {
                pixels.copy_from_slice(&input[i].0);
                let t = input[i].1;
                i += 1;
                Ok(Some(t))
            })
            .unwrap();
        window.current_descriptor = Some(
            Luma::new(&input[0].0, WIDTH, HEIGHT)
                .unwrap()
                .half()
                .unwrap(),
        );
        window.motion_history.push_back(MotionAnchor {
            image: Luma::new(&input[0].0, WIDTH, HEIGHT)
                .unwrap()
                .half()
                .unwrap(),
            start_ns: 0,
            end_ns: 33_333_333,
            first_ns: 33_333_333,
            source_steps: 1,
        });
        let span = window.unchanged("gradualDrift");
        assert!(window.current_descriptor.is_none() && window.motion_history.is_empty());
        window.advance(span, 0);
        assert!(window.current_descriptor.is_none() && window.motion_history.is_empty());
    }

    #[test]
    fn adaptive_partial_repeat_cut_counts_deleted_timestamps_and_legacy_serde() {
        let input = frames(&[0, 0, 0, 4], 20_000_000);
        let mut window = Window::new(input[0].0.clone(), 0, WIDTH, HEIGHT, 120_000_000, true, 75)
            .with_mode(AntiStutterMode::Remove);
        let mut i = 1;
        window
            .ensure(4, &mut |pixels| {
                pixels.copy_from_slice(&input[i].0);
                let t = if i == 3 { 100_000_000 } else { input[i].1 };
                i += 1;
                Ok(Some(t))
            })
            .unwrap();
        window.advance(
            Span {
                right: Some(3),
                repaired: true,
                cut: false,
                remove_start: Some(50_000_000),
                detail: None,
                motion: None,
            },
            0,
        );
        assert_eq!(window.diagnostics.removed_runs, Some(1));
        assert_eq!(window.diagnostics.removed_frames, Some(0));
        assert_eq!(window.diagnostics.bridged_runs, Some(1));
        assert_eq!(window.diagnostics.retained_bridge_duration_s, Some(0.03));
        let old = serde_json::json!({"version":3,"analyzedFrames":0,"candidateRuns":0,"repairedRuns":0,"repairedFrames":0,"unchangedRuns":0,"intervals":[{"startS":0.,"endS":1.,"sourceRepeatedFrames":1,"repairedFrames":0,"reason":"repaired"}]});
        let decoded: AntiStutterDiagnostics = serde_json::from_value(old).unwrap();
        assert_eq!(decoded.bridged_runs, None);
        assert_eq!(decoded.intervals[0].bridge_duration_s, None);
    }

    fn broad_texture(
        width: usize,
        height: usize,
        position: f64,
        zoom: f64,
        parallax: bool,
    ) -> Vec<u8> {
        let node = |x: i32, y: i32| {
            let mut v = (x as u32).wrapping_mul(747796405)
                ^ (y as u32).wrapping_mul(2891336453)
                ^ 0x1542_9361;
            v ^= v >> 16;
            v = v.wrapping_mul(2246822519);
            (32 + (v % 192)) as f64
        };
        let mut pixels = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            for x in 0..width {
                let depth = if parallax && y > height / 2 {
                    1.12
                } else {
                    1.0
                };
                let sx = (x as f64 * zoom + position * depth) / 12.0;
                let sy = (y as f64 + (zoom - 1.0) * x as f64 * 0.2) / 12.0;
                let (ix, iy) = (sx.floor() as i32, sy.floor() as i32);
                let (fx, fy) = (sx - sx.floor(), sy - sy.floor());
                let value = ((node(ix, iy) * (1.0 - fx) + node(ix + 1, iy) * fx) * (1.0 - fy)
                    + (node(ix, iy + 1) * (1.0 - fx) + node(ix + 1, iy + 1) * fx) * fy)
                    .round() as u8;
                pixels.extend([value; 3]);
            }
        }
        pixels
    }

    #[test]
    fn remove_coarse_correspondence_supports_broad_motion_without_changing_fill() {
        let (width, height) = (512, 288);
        for (zoom, parallax) in [(1.0, false), (1.03, false), (1.0, true)] {
            let a = broad_texture(width, height, 6.0, 1.0, false);
            let b = broad_texture(width, height, 86.0, zoom, parallax);
            assert!(
                !continuous(&a, &b, width, height, Policy { strength: 75 }).unwrap(),
                "fixture must exercise fine-scale failure"
            );
            assert!(
                removable_continuous(&a, &b, width, height, Policy { strength: 75 }).unwrap(),
                "zoom={zoom},parallax={parallax}"
            );
        }
        let positions = [0., 2., 4., 6., 6., 86., 88., 90., 92., 94., 96., 98., 100.];
        let source: Vec<_> = positions
            .iter()
            .enumerate()
            .map(|(i, p)| {
                (
                    broad_texture(width, height, *p, 1., false),
                    i as u64 * 33_333_333,
                )
            })
            .collect();
        assert_eq!(
            run_sized(&source, 120, 1., 75, (width, height), false)
                .0
                .repaired_runs,
            0
        );
        let (d, map) = remove_sized(&source, width, height, 75);
        assert_eq!(map.cuts().len(), 1, "{d:?}");
        assert_eq!(
            (map.cuts()[0].start_ns, map.cuts()[0].end_ns),
            (source[4].1, source[5].1)
        );
        for regular in [false, true] {
            let positions: Vec<_> = if regular {
                (0..16)
                    .flat_map(|i| [i as f64 * 80., i as f64 * 80.])
                    .collect()
            } else {
                vec![0., 2., 4., 6., 6., 86., 88.]
            };
            let source: Vec<_> = positions
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    (
                        broad_texture(width, height, *p, 1., false),
                        i as u64 * 33_333_333,
                    )
                })
                .collect();
            let (d, map) = remove_sized(&source, width, height, 75);
            assert_eq!(
                map.removed_duration_ns(),
                0,
                "coarse-only evidence cannot enable retained Fill: {d:?}"
            );
            assert_eq!(
                d.repaired_runs, 0,
                "coarse-only evidence cannot enable retained Fill: {d:?}"
            );
        }
        for (width, height) in [(16, 12), (30, 20)] {
            assert!(
                !removable_continuous(
                    &broad_texture(width, height, 0., 1., false),
                    &broad_texture(width, height, 8., 1., false),
                    width,
                    height,
                    Policy { strength: 100 }
                )
                .unwrap()
            );
        }
    }

    #[test]
    fn remove_coarse_correspondence_rejects_unrelated_repetitive_and_lighting_controls() {
        let (width, height) = (512, 288);
        let a = broad_texture(width, height, 6., 1., false);
        let mut unrelated = a.clone();
        for row in unrelated.chunks_exact_mut(width * 3) {
            row.reverse();
        }
        assert!(!is_scene_cut(&a, &unrelated));
        assert!(
            !removable_continuous(&a, &unrelated, width, height, Policy { strength: 100 }).unwrap()
        );
        let mut source: Vec<_> = [0., 2., 4., 6., 6., 86., 88., 90., 92., 94., 96., 98.]
            .iter()
            .enumerate()
            .map(|(i, p)| {
                (
                    broad_texture(width, height, *p, 1., false),
                    i as u64 * 33_333_333,
                )
            })
            .collect();
        for (pixels, _) in source.iter_mut().skip(5) {
            for row in pixels.chunks_exact_mut(width * 3) {
                row.reverse();
            }
        }
        let (d, map) = remove_sized(&source, width, height, 100);
        assert_eq!(map.removed_duration_ns(), 0, "{d:?}");
        assert!(
            d.intervals
                .iter()
                .any(|i| i.reason == "unreliableCorrespondence"),
            "{d:?}"
        );
        let mut periodic = vec![0; width * height * 3];
        for y in 0..height {
            for x in 0..width {
                periodic[(y * width + x) * 3..(y * width + x + 1) * 3]
                    .fill(if (x / 8 + y / 8) % 2 == 0 { 40 } else { 180 });
            }
        }
        let mut shifted = periodic.clone();
        for row in shifted.chunks_exact_mut(width * 3) {
            row.rotate_left(3 * 3);
        }
        assert!(
            !removable_continuous(&periodic, &shifted, width, height, Policy { strength: 100 })
                .unwrap()
        );
        // Start from the genuine coarse-positive family, not the unrelated
        // control, so the discontinuity veto itself is necessary here.
        for color_only in [false, true] {
            let mut lit: Vec<_> = [0., 2., 4., 6., 6., 86., 88., 90., 92., 94., 96., 98.]
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    (
                        broad_texture(width, height, *p, 1., false),
                        i as u64 * 33_333_333,
                    )
                })
                .collect();
            assert!(
                removable_continuous(
                    &lit[3].0,
                    &lit[5].0,
                    width,
                    height,
                    Policy { strength: 100 }
                )
                .unwrap()
            );
            for pixel in lit[4].0.chunks_exact_mut(3) {
                if color_only {
                    pixel[0] = pixel[0].saturating_add(1);
                } else {
                    for channel in pixel {
                        *channel = channel.saturating_add(12);
                    }
                }
            }
            assert!(
                compare(&lit[3].0, &lit[4].0, width, height)
                    .unwrap()
                    .lighting
            );
            let (d, map) = remove_sized(&lit, width, height, 100);
            assert_eq!(map.removed_duration_ns(), 0, "{d:?}");
            assert_eq!(d.repaired_runs, 0, "{d:?}");
            if color_only {
                assert!(
                    compare(&lit[3].0, &lit[4].0, width, height)
                        .unwrap()
                        .plateau()
                );
                assert!(d.intervals.iter().any(|i| i.reason == "sceneCut"), "{d:?}");
            } else {
                assert_eq!(d.candidate_runs, 0, "{d:?}");
            }
        }
    }

    #[test]
    fn remove_retains_first_actual_vfr_interval_and_changed_endpoint() {
        let mut input = frames(&[0, 1, 2, 3, 3, 5, 6, 7, 8, 9, 10, 11, 12], 33_333_333);
        input[4].1 = 150_000_000;
        let (diagnostics, map) = remove(&input);
        assert_eq!(diagnostics.mode, Some(AntiStutterMode::Remove));
        assert_eq!(diagnostics.removed_runs, Some(1));
        assert_eq!(diagnostics.removed_frames, Some(1));
        assert_eq!(map.cuts().len(), 1);
        assert_eq!(map.cuts()[0].start_ns, input[4].1);
        assert_eq!(map.cuts()[0].end_ns, input[5].1);
        assert_eq!(map.map_ns(input[3].1), input[3].1);
        assert_eq!(map.map_ns(input[5].1), input[4].1);
        assert_eq!(diagnostics.repaired_runs, 0);
        assert_eq!(diagnostics.repaired_frames, 0);
        assert_eq!(diagnostics.intervals[0].start_s, input[4].1 as f64 / 1e9);
    }

    #[test]
    fn remove_preserves_ordinary_cadence_from_first_hold_and_across_transitions() {
        for pattern in [vec![2], vec![3], vec![2, 3], vec![1, 1, 1, 2]] {
            let positions: Vec<_> = (0..24)
                .flat_map(|i| std::iter::repeat_n(i, pattern[i % pattern.len()]))
                .collect();
            let (diagnostics, map) = remove(&frames(&positions, 33_333_333));
            assert_eq!(
                map.removed_duration_ns(),
                0,
                "pattern{pattern:?}: {diagnostics:?}"
            );
            assert_eq!(diagnostics.removed_runs, Some(0));
            assert!(
                diagnostics.repaired_runs > 0,
                "retained cadence must still be filled: {diagnostics:?}"
            );
            assert!(
                diagnostics
                    .intervals
                    .iter()
                    .filter(|i| i.reason == "regularCadence")
                    .any(|i| i.repaired_frames > 0)
            );
            assert!(
                diagnostics
                    .intervals
                    .iter()
                    .any(|i| i.reason == "regularCadence")
            );
        }
        let positions: Vec<_> = (0..20)
            .flat_map(|i| std::iter::repeat_n(i, if i < 9 { 2 } else { 3 }))
            .collect();
        assert_eq!(
            remove(&frames(&positions, 33_333_333))
                .1
                .removed_duration_ns(),
            0
        );
    }

    #[test]
    fn remove_keeps_static_tail_and_resource_uncertainty_but_removes_irregular_short_holds() {
        let (incomplete, map) = remove(&frames(&[0, 1, 2, 3, 3, 5, 6], 33_333_333));
        assert_eq!(map.removed_duration_ns(), 0);
        assert_eq!(incomplete.repaired_runs, 1);
        assert!(incomplete.intervals.iter().any(|interval| interval.reason
            == "insufficientCadence"
            && interval.repaired_frames > 0));
        for positions in [vec![3; 20], vec![0, 1, 2, 3, 3, 3, 3], vec![3, 3, 3, 6]] {
            assert_eq!(
                remove(&frames(&positions, 33_333_333))
                    .1
                    .removed_duration_ns(),
                0
            );
        }
        let source = frames(
            &[0, 1, 2, 3, 3, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14],
            33_333_333,
        );
        let (diagnostics, map) = remove(&source);
        assert_eq!(diagnostics.removed_runs, Some(1));
        assert_eq!(map.removed_duration_ns(), 33_333_333);
        assert_eq!(
            map.output_duration_ns(),
            map.source_duration_ns() - 33_333_333
        );
    }

    #[test]
    fn cadence_proof_uses_counts_and_real_time_without_average_fps() {
        let c = |steps, duration_ns| Cadence {
            source_steps: steps,
            duration_ns,
        };
        assert!(regular_cadence(
            &[c(2, 66_666_667), c(2, 66_666_666), c(2, 66_666_667)],
            0
        ));
        assert!(!regular_cadence(
            &[c(2, 66_666_667), c(2, 100_000_000), c(2, 133_333_333)],
            0
        ));
        assert!(!regular_cadence(
            &[c(2, 66_666_667), c(3, 66_666_667), c(4, 66_666_667)],
            0
        ));
        assert!(regular_cadence(
            &[
                c(2, 66_666_667),
                c(3, 100_000_000),
                c(2, 66_666_667),
                c(3, 100_000_000),
                c(2, 66_666_667)
            ],
            0
        ));
    }
    fn run_sized(
        input: &[(Vec<u8>, u64)],
        fps: u32,
        speed: f64,
        strength: u8,
        dimensions: (usize, usize),
        beginning: bool,
    ) -> RunResult {
        let (width, height) = dimensions;
        let duration =
            input.last().unwrap().1 + input.get(1).map_or(33_333_333, |p| p.1 - input[0].1);
        let mut window = Window::new(
            input[0].0.clone(),
            input[0].1,
            width,
            height,
            duration,
            true,
            strength,
        )
        .at_source_beginning(beginning);
        let mut index = 1;
        let mut read = |pixels: &mut [u8]| {
            if index == input.len() {
                return Ok(None);
            }
            pixels.copy_from_slice(&input[index].0);
            let pts = input[index].1;
            index += 1;
            Ok(Some(pts))
        };
        let rate = Rate::for_playback(fps, speed).unwrap();
        let count = rate.frame_count(duration).unwrap();
        let mut output = 0;
        let mut spans = Vec::new();
        while let Some(span) = window.next(&mut read).unwrap() {
            let left = window.pts(0);
            let right = span.right.map(|i| window.pts(i));
            let original = input.iter().find(|frame| frame.1 == left).unwrap();
            assert_eq!(
                window.pixels(0),
                original.0,
                "source anchor bytes must survive fallback"
            );
            if let Some(index) = span.right {
                let original = input
                    .iter()
                    .find(|frame| frame.1 == window.pts(index))
                    .unwrap();
                assert_eq!(window.pixels(index), original.0);
            }
            spans.push((left, right, span.repaired));
            let mut repaired = 0;
            while output < count && right.is_none_or(|r| rate.sample_before(output, r)) {
                let alpha = right
                    .map(|r| rate.alpha(output, left, r).unwrap())
                    .unwrap_or(0.0);
                repaired += u64::from(span.repaired && alpha > 0.0 && alpha < 1.0);
                output += 1;
            }
            window.advance(span, repaired);
        }
        assert_eq!(window.diagnostics.analyzed_frames, input.len() as u64);
        assert!(window.allocated_high_water <= MAX_PICTURES);
        (window.diagnostics, window.allocated_high_water, spans)
    }

    #[test]
    fn anti_stutter_keeps_one_anchor_for_long_noisy_vfr_holds_and_original_fallback() {
        let (width, height) = (384, 216);
        let mut positions = vec![0, 1, 2, 3];
        positions.extend(std::iter::repeat_n(3, 14));
        positions.extend([18, 19, 20, 21]);
        let mut source: Vec<_> = positions
            .iter()
            .enumerate()
            .map(|(index, position)| {
                let mut pixels = picture_sized(width, height, *position as f64);
                if (4..18).contains(&index) {
                    for y in 0..height {
                        for x in 0..width {
                            for channel in 0..3 {
                                let byte = &mut pixels[(y * width + x) * 3 + channel];
                                *byte = if (x + y + index) % 2 == 0 {
                                    byte.saturating_add(4)
                                } else {
                                    byte.saturating_sub(4)
                                };
                            }
                        }
                    }
                }
                (
                    pixels,
                    index as u64 * 33_333_333 + if index >= 8 { 100_000_000 } else { 0 },
                )
            })
            .collect();
        let held = compare(&source[3].0, &source[4].0, width, height).unwrap();
        assert!(held.mean > 500 && held.near && held.coarse_mean < 1_000);
        let expected = (source[3].1, Some(source[18].1), true);
        assert!(expected.1.unwrap() - expected.0 > 500_000_000);
        for strength in [0, 75, 100] {
            let (diagnostics, high, spans) =
                run_sized(&source, 120, 1.0, strength, (width, height), false);
            assert_eq!(diagnostics.repaired_runs, 1, "{diagnostics:?}");
            assert_eq!(diagnostics.intervals[0].source_repeated_frames, 14);
            assert!(spans.contains(&expected));
            assert!((17..=MAX_PICTURES).contains(&high));
        }
        for (_, pts) in source.iter_mut().skip(18) {
            *pts += 200_000_000;
        }
        for strength in [0, 75, 100] {
            let (diagnostics, _, spans) =
                run_sized(&source, 120, 1.0, strength, (width, height), false);
            assert_eq!(diagnostics.repaired_runs, 0, "{diagnostics:?}");
            assert!(
                diagnostics
                    .intervals
                    .iter()
                    .any(|i| i.reason == "excessiveHold")
            );
            assert_eq!(
                spans.len(),
                source.len(),
                "never repair a convenient prefix"
            );
            assert!(spans.iter().all(|span| !span.2));
        }
    }

    #[test]
    fn anti_stutter_opening_exception_requires_verified_source_beginning_and_future_coherence() {
        let source = frames(&[0, 0, 2, 3, 4], 33_333_333);
        for strength in [0, 75, 100] {
            assert_eq!(
                run_sized(&source, 120, 1.0, strength, (WIDTH, HEIGHT), true)
                    .0
                    .repaired_runs,
                1
            );
            assert_eq!(
                run_sized(&source, 120, 1.0, strength, (WIDTH, HEIGHT), false)
                    .0
                    .repaired_runs,
                0,
                "a trim boundary cannot borrow outside context"
            );
            let no_future = frames(&[0, 0, 2, 2, 2], 33_333_333);
            assert_eq!(
                run_sized(&no_future, 120, 1.0, strength, (WIDTH, HEIGHT), true)
                    .0
                    .repaired_runs,
                0
            );
            let mut cut = frames(&[0, 1, 1, 3, 4, 5], 33_333_333);
            cut[0].0.fill(0);
            assert!(is_scene_cut(&cut[0].0, &cut[1].0));
            assert_eq!(
                run_sized(&cut, 120, 1.0, strength, (WIDTH, HEIGHT), true)
                    .0
                    .repaired_runs,
                0,
                "past context cannot be replaced after a cut"
            );
        }
    }

    #[test]
    fn anti_stutter_longer_limits_still_reject_repeat_overflow_and_4k_memory_admission() {
        let mut positions = vec![0, 1, 2, 3];
        positions.extend(std::iter::repeat_n(3, MAX_REPEATED + 2));
        positions.extend([31, 32, 33]);
        for strength in [0, 75, 100] {
            let (diagnostics, high, spans) =
                run_at_strength(&frames(&positions, 5_000_000), 120, 1.0, strength);
            assert_eq!(diagnostics.repaired_runs, 0);
            assert!(
                diagnostics
                    .intervals
                    .iter()
                    .any(|i| i.reason == "excessiveHold")
            );
            assert!(high <= MAX_PICTURES && spans.iter().all(|span| !span.2));
        }
        let mut window = Window::new(vec![0; 3], 0, 1, 1, 1, true, 75);
        window.frame_bytes = 3840 * 2160 * 3;
        assert!(window.frame_bytes * 10 <= MAX_BUFFER_BYTES);
        assert!(window.frame_bytes * 11 > MAX_BUFFER_BYTES);
        assert!(
            !window
                .ensure(11, &mut |_| panic!(
                    "memory admission precedes reads/allocations"
                ))
                .unwrap()
        );
    }

    #[test]
    fn anti_stutter_low_amplitude_subpixel_and_chroma_drift_remain_motion() {
        let (width, height) = (384, 216);
        for chroma_only in [false, true] {
            let mut source = Vec::new();
            for index in 0..23 {
                let position = if index < 4 {
                    index as f64
                } else if index < 20 {
                    3.0 + (index - 3) as f64 * 0.1
                } else {
                    9.0 + (index - 20) as f64
                };
                let mut pixels = picture_sized(width, height, position);
                if chroma_only && (4..20).contains(&index) {
                    // Approximately isoluminant opposite chroma changes cannot
                    // disappear merely because the luma descriptor is stable.
                    pixels = picture_sized(width, height, 3.0);
                    let amount = (index - 3) as u8;
                    for (pixel_index, pixel) in pixels.chunks_exact_mut(3).enumerate() {
                        if pixel_index % 2 == 0 {
                            pixel[0] = pixel[0].saturating_add(amount * 3);
                            pixel[1] = pixel[1].saturating_sub(amount);
                            pixel[2] = pixel[2].saturating_add(amount);
                        } else {
                            pixel[0] = pixel[0].saturating_sub(amount * 3);
                            pixel[1] = pixel[1].saturating_add(amount);
                            pixel[2] = pixel[2].saturating_sub(amount);
                        }
                    }
                }
                source.push((pixels, index as u64 * 33_333_333));
            }
            for strength in [0, 75, 100] {
                let (diagnostics, _, _) =
                    run_sized(&source, 120, 1.0, strength, (width, height), false);
                assert_eq!(
                    diagnostics.repaired_runs, 0,
                    "chroma={chroma_only}: {diagnostics:?}"
                );
                let (removed, map) = remove_sized(&source, width, height, strength);
                assert_eq!(
                    map.removed_duration_ns(),
                    0,
                    "chroma={chroma_only}: {removed:?}"
                );
                assert_eq!(
                    removed.repaired_runs, 0,
                    "chroma={chroma_only}: {removed:?}"
                );
            }
        }
    }

    #[test]
    fn anti_stutter_ambiguous_codec_noise_retires_context_after_a_successful_repair() {
        let (width, height) = (384, 216);
        let mut source: Vec<_> = [0, 1, 2, 3, 3, 5, 6]
            .iter()
            .enumerate()
            .map(|(i, p)| {
                (
                    picture_sized(width, height, *p as f64),
                    i as u64 * 33_333_333,
                )
            })
            .collect();
        for frame in 0..3 {
            let mut seed = if frame == 0 { 17u32 } else { 29u32 };
            let pixels = (0..width * height * 3)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    125 + ((seed >> 24) % 7) as u8
                })
                .collect();
            source.push((pixels, source.len() as u64 * 33_333_333));
        }
        for position in [10, 11, 12] {
            source.push((
                picture_sized(width, height, position as f64),
                source.len() as u64 * 33_333_333,
            ));
        }
        let ambiguous = compare(&source[7].0, &source[8].0, width, height).unwrap();
        assert!(ambiguous.ambiguous && !ambiguous.plateau() && !ambiguous.moving());
        for strength in [0, 75, 100] {
            let mut window = Window::new(
                source[0].0.clone(),
                0,
                width,
                height,
                500_000_000,
                true,
                strength,
            );
            let mut next = 1;
            let mut read = |pixels: &mut [u8]| {
                if next == source.len() {
                    return Ok(None);
                }
                pixels.copy_from_slice(&source[next].0);
                let pts = source[next].1;
                next += 1;
                Ok(Some(pts))
            };
            let mut repaired_before = false;
            let mut retired = false;
            while let Some(span) = window.next(&mut read).unwrap() {
                let is_ambiguous_pair = window.pts(0) == source[7].1;
                if is_ambiguous_pair {
                    assert!(repaired_before);
                    assert!(!span.repaired);
                }
                let repaired = u64::from(span.repaired);
                repaired_before |= span.repaired;
                window.advance(span, repaired);
                if is_ambiguous_pair {
                    assert!(window.previous_motion.is_none());
                    retired = true;
                }
            }
            assert!(retired);
        }
    }

    #[test]
    fn anti_stutter_strength_has_nested_confidence_limits_and_fixed_coverage() {
        for (strength, minimum, factor) in [
            (0, 12, 400),
            (25, 11, 350),
            (50, 10, 300),
            (75, 9, 250),
            (100, 8, 200),
        ] {
            let policy = Policy { strength };
            assert_eq!(policy.minimum_matches(), minimum);
            assert_eq!(policy.residual_factor(), factor);
        }
        let context = Evidence {
            coarse_mean: 1_000,
            ..compare(&picture(0), &picture(1), WIDTH, HEIGHT).unwrap()
        };
        assert!(!Policy { strength: 0 }.accepts_residual(300, context));
        assert!(Policy { strength: 50 }.accepts_residual(300, context));
        assert!(Policy { strength: 100 }.accepts_residual(300, context));
        assert!(Policy { strength: 0 }.accepts_residual(250, context));
        assert!(!Policy { strength: 0 }.accepts_residual(251, context));
        assert!(Policy { strength: 100 }.accepts_residual(500, context));
        assert!(!Policy { strength: 100 }.accepts_residual(501, context));
        for residual in 0..=1_000 {
            let mut accepted = false;
            for strength in 0..=100 {
                let next = Policy { strength }.accepts_residual(residual, context);
                assert!(!accepted || next);
                accepted = next;
            }
        }
        let mut patches = vec![
            (0, 0, 4, 2),
            (1, 0, 4, 2),
            (2, 1, 4, 2),
            (3, 1, 4, 2),
            (0, 2, 4, 2),
            (1, 2, 4, 2),
            (2, 2, 4, 2),
        ];
        assert_eq!(coherent_matches(&patches), 7);
        assert!(coherent_matches(&patches) < Policy { strength: 100 }.minimum_matches());
        patches.push((3, 2, 4, 2));
        assert_eq!(coherent_matches(&patches), 8);
        assert!(coherent_matches(&patches) >= Policy { strength: 100 }.minimum_matches());
        assert!(coherent_matches(&patches) < Policy { strength: 0 }.minimum_matches());
        assert!(!Policy { strength: 100 }.accepts_matches(12, 8, 7));
        assert!(Policy { strength: 100 }.accepts_matches(12, 8, 8));
        assert!(!Policy { strength: 0 }.accepts_matches(12, 8, 8));
        assert!(!Policy { strength: 100 }.accepts_matches(11, 8, 8));
        assert!(Policy { strength: 100 }.accepts_matches(12, 12, 8));
        assert!(!Policy { strength: 100 }.accepts_matches(13, 13, 8));
        for patch in &mut patches {
            patch.0 %= 3;
        }
        assert_eq!(
            coherent_matches(&patches),
            0,
            "eight matches in only three columns are insufficient"
        );
        for patch in &mut patches {
            patch.2 = 0;
            patch.3 = 0;
        }
        assert_eq!(
            coherent_matches(&patches),
            0,
            "zero-motion support never qualifies"
        );
    }

    #[test]
    fn anti_stutter_strength_does_not_override_ambiguous_partial_occlusion() {
        let positions = [0, 1, 2, 3, 3, 3, 6, 7, 8, 9];
        let patches = [
            (1, 1),
            (2, 1),
            (4, 1),
            (6, 1),
            (1, 3),
            (2, 3),
            (4, 4),
            (6, 4),
            (4, 3),
            (6, 3),
        ];
        let input: Vec<_> = positions
            .iter()
            .enumerate()
            .map(|(frame, position)| {
                let original = picture(*position);
                let mut pixels = original.clone();
                if frame >= 6 {
                    for y in 0..HEIGHT {
                        for x in 0..WIDTH {
                            let value = (112.0
                                + 12.0
                                    * (x as f64 * 0.08 + y as f64 * 0.05 + *position as f64 * 0.3)
                                        .sin())
                            .round() as u8;
                            pixels[(y * WIDTH + x) * 3..(y * WIDTH + x + 1) * 3].fill(value);
                        }
                    }
                    for (tx, ty) in patches {
                        let cx = (2 * tx + 1) * WIDTH / 16 - (*position - 3) * 2;
                        let cy = (2 * ty + 1) * HEIGHT / 12;
                        for y in cy - 3..=cy + 3 {
                            for x in cx - 3..=cx + 3 {
                                let offset = (y * WIDTH + x) * 3;
                                pixels[offset..offset + 3]
                                    .copy_from_slice(&original[offset..offset + 3]);
                            }
                        }
                    }
                }
                (pixels, frame as u64 * 33_333_333)
            })
            .collect();
        let conservative = run_at_strength(&input, 120, 1.0, 0);
        let aggressive = run_at_strength(&input, 120, 1.0, 100);
        assert_eq!(conservative.0.repaired_runs, 0, "{:?}", conservative.0);
        assert_eq!(aggressive.0.repaired_runs, 0, "{:?}", aggressive.0);
        assert_eq!(aggressive.0.intervals[0].reason, "unreliableCorrespondence");
    }

    #[test]
    fn anti_stutter_vfr_codec_noise_is_not_treated_as_edge_velocity() {
        let positions = [0, 1, 2, 3, 3, 5, 6, 7];
        let times = [0, 33, 66, 166, 176, 186, 286, 319];
        let mut input: Vec<_> = positions
            .iter()
            .zip(times)
            .map(|(position, time)| (picture(*position), time * 1_000_000))
            .collect();
        for (index, byte) in input[4].0.iter_mut().step_by(3).enumerate() {
            *byte = if index % 2 == 0 {
                byte.saturating_add(1)
            } else {
                byte.saturating_sub(1)
            };
        }
        let conservative = run_at_strength(&input, 120, 1.0, 0);
        let aggressive = run_at_strength(&input, 120, 1.0, 100);
        assert_eq!(conservative.0.repaired_runs, 1, "{:?}", conservative.0);
        assert_eq!(aggressive.0.repaired_runs, 1, "{:?}", aggressive.0);
        assert!(
            aggressive
                .2
                .contains(&(166_000_000, Some(186_000_000), true))
        );
    }

    #[test]
    fn anti_stutter_uses_genuine_anchors_and_original_clock_at_both_rates_and_speed() {
        let off_grid = frames(&[0, 1, 2, 3, 3, 3, 6, 7, 8, 9], 33_333_333);
        let (diagnostics, _, spans) = run(&off_grid, 60, 1.0);
        assert_eq!(diagnostics.repaired_frames, 6);
        assert!(spans.contains(&(99_999_999, Some(199_999_998), true)));
        let mut source = frames(&[0, 1, 2, 3, 3, 3, 6, 7, 8, 9], 33_333_333);
        for (index, (_, pts)) in source.iter_mut().enumerate() {
            *pts = (index as u64 * 1_000_000_000 + 15) / 30;
        }
        for (fps, speed, expected) in [(60, 1.0, 5), (120, 1.0, 11), (60, 2.0, 2)] {
            let (diagnostics, _, spans) = run(&source, fps, speed);
            assert_eq!(diagnostics.repaired_runs, 1, "{diagnostics:?}");
            assert_eq!(diagnostics.repaired_frames, expected);
            assert_eq!(diagnostics.intervals[0].source_repeated_frames, 2);
            assert!(spans.contains(&(100_000_000, Some(200_000_000), true)));
            assert!(
                !spans
                    .iter()
                    .any(|(left, _, _)| *left == 133_333_332 || *left == 166_666_665)
            );
        }
    }

    #[test]
    fn anti_stutter_preserves_static_and_boundary_holds() {
        let controls = [
            vec![3; 20],
            vec![3, 3, 3, 6, 7, 8, 9],
            vec![0, 1, 2, 3, 3, 3],
        ];
        for positions in controls {
            let (diagnostics, _, spans) = run(&frames(&positions, 16_666_667), 60, 1.0);
            assert_eq!(
                diagnostics.repaired_runs, 0,
                "{positions:?}: {diagnostics:?}"
            );
            assert!(spans.iter().all(|s| !s.2));
        }
    }

    #[test]
    fn anti_stutter_v2_deholds_single_slots_and_regular_gameplay_cadence() {
        let single = frames(&[0, 1, 2, 3, 3, 5, 6, 7], 16_666_667);
        assert_eq!(run(&single, 60, 1.0).0.repaired_runs, 1);
        for repeats in [2, 4] {
            let cadence: Vec<_> = (0..10)
                .flat_map(|p| std::iter::repeat_n(p, repeats))
                .collect();
            let diagnostics = run(&frames(&cadence, 16_666_667), 60, 1.0).0;
            assert!(diagnostics.repaired_runs >= 5, "{diagnostics:?}");
        }
        let cadence: Vec<_> = (0..20)
            .flat_map(|p| std::iter::repeat_n(p, if p % 2 == 0 { 2 } else { 3 }))
            .skip(1)
            .collect();
        assert!(run(&frames(&cadence, 16_666_667), 60, 1.0).0.repaired_runs >= 10);
    }

    #[test]
    fn anti_stutter_preserves_color_drift_and_lacks_context_for_static_hud() {
        let mut near = frames(&[0, 1, 2, 3, 3, 3, 6, 7, 8, 9], 16_666_667);
        for (i, (pixels, _)) in near.iter_mut().enumerate().take(6).skip(4) {
            for pixel in pixels.chunks_exact_mut(3) {
                pixel[0] = pixel[0].saturating_add((i - 3) as u8);
            }
        }
        let (diagnostics, _, _) = run(&near, 60, 1.0);
        assert_eq!(diagnostics.repaired_runs, 0);
        let mut hud = frames(&[0, 1, 2, 3, 3, 3, 6, 7, 8, 9], 16_666_667);
        for (i, (pixels, _)) in hud.iter_mut().enumerate() {
            pixels.fill(20);
            for y in 0..4 {
                for x in 0..4 {
                    let offset = (y * WIDTH + x) * 3;
                    pixels[offset..offset + 3].fill((i * 8) as u8);
                }
            }
        }
        assert_eq!(run(&hud, 60, 1.0).0.repaired_runs, 0);
        let slow: Vec<_> = (0..20)
            .map(|i| (vec![80 + i as u8 / 4; WIDTH * HEIGHT * 3], i * 16_666_667))
            .collect();
        assert_eq!(run(&slow, 60, 1.0).0.repaired_runs, 0);
        let color: Vec<_> = (0..20)
            .map(|i| (vec![(i * 8) as u8; WIDTH * HEIGHT * 3], i * 16_666_667))
            .collect();
        assert_eq!(run(&color, 60, 1.0).0.candidate_runs, 0);
    }

    #[test]
    fn anti_stutter_never_repairs_a_long_run_prefix_and_bounds_the_actual_pool() {
        let mut positions = vec![0, 1, 2, 3];
        positions.extend(std::iter::repeat_n(3, 30));
        positions.extend([34, 35, 36]);
        let (diagnostics, high, spans) = run(&frames(&positions, 8_333_333), 120, 1.0);
        assert_eq!(diagnostics.repaired_runs, 0);
        assert_eq!(diagnostics.candidate_runs, 1);
        assert_eq!(diagnostics.unchanged_runs, 1);
        assert_eq!(diagnostics.intervals[0].source_repeated_frames, 30);
        assert!(high <= MAX_PICTURES && spans.iter().all(|s| !s.2));
        let mut exact = vec![0, 1, 2, 3];
        exact.extend(std::iter::repeat_n(3, 6));
        exact.extend([10, 11, 12]);
        let (_, high, _) = run(&frames(&exact, 8_333_333), 120, 1.0);
        assert!(high <= MAX_PICTURES);
    }

    #[test]
    fn anti_stutter_uses_vfr_elapsed_time_and_rejects_nonincreasing_pts() {
        let mut source = frames(&[0, 1, 2, 3, 3, 3, 6, 7, 8, 9], 16_666_667);
        for (_, pts) in source.iter_mut().skip(5) {
            *pts += 4_000_000;
        }
        assert_eq!(run(&source, 60, 1.0).0.repaired_runs, 1);
        let mut window = Window::new(picture(0), 20, WIDTH, HEIGHT, 100, true, 100);
        assert!(
            window
                .next(&mut |pixels| {
                    pixels.copy_from_slice(&picture(1));
                    Ok(Some(20))
                })
                .unwrap_err()
                .contains("strictly")
        );
    }

    #[test]
    fn anti_stutter_span_limit_and_empty_target_grid_do_not_claim_repairs() {
        let mut source = frames(&[0, 1, 2, 3, 3, 3, 6, 7, 8, 9], 33_333_333);
        for (index, (_, pts)) in source.iter_mut().enumerate() {
            *pts = (index as u64 * 1_000_000_000 + 15) / 30;
        }
        for (_, pts) in source.iter_mut().skip(6) {
            *pts += 650_000_001;
        }
        let diagnostics = run(&source, 60, 1.0).0;
        assert_eq!(diagnostics.repaired_runs, 0);
        assert!(
            diagnostics
                .intervals
                .iter()
                .any(|i| i.reason == "excessiveHold")
        );
        let tiny = frames(&[0, 1, 2, 3, 3, 3, 6, 7, 8, 9], 8_333_333);
        let diagnostics = run(&tiny, 60, 16.0).0;
        assert_eq!(diagnostics.repaired_runs, 0);
        assert_eq!(diagnostics.repaired_frames, 0);
        assert!(
            diagnostics
                .intervals
                .iter()
                .any(|i| i.reason == "noOutputSamples")
        );
    }

    #[test]
    fn anti_stutter_same_histogram_cuts_and_decorrelated_context_are_unchanged() {
        let mut source = frames(&[0, 1, 2, 3, 3, 3, 6, 7, 8, 9], 16_666_667);
        for (pixels, _) in source.iter_mut().skip(6) {
            for row in pixels.chunks_exact_mut(WIDTH * 3) {
                row.rotate_left(WIDTH / 2 * 3);
            }
        }
        assert!(!is_scene_cut(&source[3].0, &source[6].0));
        let diagnostics = run(&source, 60, 1.0).0;
        assert_eq!(diagnostics.repaired_runs, 0);
        assert!(
            diagnostics
                .intervals
                .iter()
                .any(|i| i.reason == "unreliableCorrespondence"),
            "{diagnostics:?}"
        );
        let mut noise = frames(&[0, 1, 2, 3, 3, 3, 6, 7, 8, 9], 16_666_667);
        for (index, (pixels, _)) in noise.iter_mut().enumerate() {
            let mut seed = if (3..6).contains(&index) { 3 } else { index } as u32 + 17;
            for byte in pixels {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                *byte = (seed >> 24) as u8;
            }
        }
        assert_eq!(run(&noise, 60, 1.0).0.repaired_runs, 0);
    }

    #[test]
    fn anti_stutter_caps_details_keeps_totals_and_rejects_unadmitted_memory_or_cancel() {
        let mut positions = Vec::new();
        for run in 0..45 {
            let n = run * 8;
            positions.extend([n, n + 1, n + 2, n + 3, n + 3, n + 3, n + 6, n + 7]);
        }
        let (diagnostics, _, _) = run(&frames(&positions, 16_666_667), 60, 1.0);
        assert!(diagnostics.candidate_runs > DETAIL_LIMIT as u64);
        assert_eq!(
            diagnostics.candidate_runs,
            diagnostics.repaired_runs + diagnostics.unchanged_runs
        );
        assert_eq!(diagnostics.intervals.len(), DETAIL_LIMIT);
        let mut window = Window::new(vec![0; 3], 0, 1, 1, 1, true, 100);
        window.frame_bytes = MAX_BUFFER_BYTES / 9 + 1;
        assert!(
            !window
                .ensure(10, &mut |_| panic!(
                    "resource admission must precede allocation/read"
                ))
                .unwrap()
        );
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let _scope = crate::process::JobScope::enter(crate::process::JobContext {
            cancel,
            child: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        });
        let mut window = Window::new(picture(0), 0, WIDTH, HEIGHT, 100, true, 100);
        assert!(
            window
                .next(&mut |_| panic!("cancelled input must not read"))
                .unwrap_err()
                .contains("Canceled")
        );
    }

    #[test]
    fn anti_stutter_accepts_sparse_compression_residuals_and_partial_holds() {
        let mut source = frames(&[0, 1, 2, 3, 3, 3, 6, 7, 8, 9], 33_333_333);
        for (i, (pixels, _)) in source.iter_mut().enumerate().take(6).skip(4) {
            for offset in (i..pixels.len()).step_by(311) {
                pixels[offset] = pixels[offset].saturating_add(9);
            }
            pixels[i * 3] = pixels[i * 3].saturating_add(26);
        }
        let diagnostics = run(&source, 120, 1.0).0;
        assert_eq!(diagnostics.repaired_runs, 1, "{diagnostics:?}");
        assert_eq!(diagnostics.version, 3);
        let mut source = frames(&[0, 1, 2, 3, 3, 3, 6, 7, 8, 9], 33_333_333);
        for (i, (pixels, _)) in source.iter_mut().enumerate().take(6).skip(4) {
            // Dense, oscillating one-level differences remain below half a
            // channel level on average. This is not a directional drift.
            for (index, byte) in pixels.iter_mut().enumerate().skip(i - 4).step_by(4) {
                *byte = if (index / 4) % 2 == 0 {
                    byte.saturating_add(1)
                } else {
                    byte.saturating_sub(1)
                };
            }
        }
        let diagnostics = run(&source, 120, 1.0).0;
        assert_eq!(diagnostics.repaired_runs, 1, "{diagnostics:?}");
    }

    #[test]
    fn anti_stutter_independent_seeded_texture_vfr_and_single_repeat() {
        fn texture(position: usize) -> Vec<u8> {
            let mut pixels = vec![0; WIDTH * HEIGHT * 3];
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    let mut seed = ((x + position) as u32).wrapping_mul(747796405)
                        ^ (y as u32).wrapping_mul(2891336453)
                        ^ 0x1475_1481;
                    seed ^= seed >> 16;
                    seed = seed.wrapping_mul(2246822519);
                    for channel in 0..3 {
                        pixels[(y * WIDTH + x) * 3 + channel] =
                            48 + ((seed >> (channel * 8)) & 127) as u8;
                    }
                }
            }
            pixels
        }
        let positions = [0, 1, 2, 3, 3, 6, 7, 8, 9];
        let times = [
            0,
            33_333_333,
            66_666_667,
            100_000_000,
            166_666_667,
            233_333_333,
            266_666_667,
            300_000_000,
            333_333_333,
        ];
        let mut input: Vec<_> = positions
            .iter()
            .zip(times)
            .map(|(p, t)| (texture(*p), t))
            .collect();
        // Sparse lossy-like residuals, including a large isolated channel error.
        for byte in input[4].0.iter_mut().step_by(401) {
            *byte = byte.saturating_add(13);
        }
        let (diagnostics, _, spans) = run(&input, 120, 1.0);
        assert_eq!(diagnostics.repaired_runs, 1, "{diagnostics:?}");
        assert!(spans.contains(&(100_000_000, Some(233_333_333), true)));
    }

    #[test]
    fn anti_stutter_near_limits_use_unrounded_sums_and_all_tiles() {
        let a = vec![80; WIDTH * HEIGHT * 3];
        let mut b = a.clone();
        let allowed = b.len() / 2;
        for p in b.iter_mut().take(allowed) {
            *p += 1;
        }
        assert!(compare(&a, &b, WIDTH, HEIGHT).unwrap().near);
        b[allowed] += 1;
        assert!(!compare(&a, &b, WIDTH, HEIGHT).unwrap().near);
        let a = vec![80; 8 * 6 * 3];
        let mut b = a.clone();
        b[..3].copy_from_slice(&[81, 81, 82]);
        assert!(compare(&a, &b, 8, 6).unwrap().near);
        b[1] = 82;
        assert!(!compare(&a, &b, 8, 6).unwrap().near);
    }

    #[test]
    fn anti_stutter_first_anchor_blocks_slow_cumulative_drift_and_stale_context() {
        let mut source = frames(&[0, 1, 2, 3, 3, 3, 3, 3, 8, 9, 10], 33_333_333);
        for (index, (pixels, _)) in source.iter_mut().enumerate().take(8).skip(4) {
            // Each adjacent change is near, but the entire run is not held.
            for byte in pixels.iter_mut().step_by(10) {
                *byte = byte.saturating_add((index - 3) as u8);
            }
        }
        let diagnostics = run(&source, 60, 1.0).0;
        assert_eq!(diagnostics.repaired_runs, 0, "{diagnostics:?}");
        assert!(
            diagnostics
                .intervals
                .iter()
                .any(|i| i.reason == "gradualDrift")
        );
        let mut source = frames(&[0, 1, 2, 3, 3, 5, 6, 7], 33_333_333);
        for (_, pts) in source.iter_mut().skip(3) {
            *pts += 800_000_000;
        }
        assert_eq!(run(&source, 60, 1.0).0.repaired_runs, 0);
    }

    #[test]
    fn anti_stutter_lighting_inside_a_near_hold_is_not_hidden_by_similarity() {
        for channel in 0..3 {
            let mut source = frames(&[0, 1, 2, 3, 3, 3, 6, 7, 8, 9], 33_333_333);
            for (pixels, _) in source.iter_mut().take(6).skip(4) {
                for pixel in pixels.chunks_exact_mut(3) {
                    pixel[channel] = pixel[channel].saturating_add(1);
                }
            }
            let evidence = compare(&source[3].0, &source[4].0, WIDTH, HEIGHT).unwrap();
            assert!(evidence.near && evidence.lighting && !evidence.cut);
            let diagnostics = run(&source, 120, 1.0).0;
            assert_eq!(diagnostics.repaired_runs, 0, "{diagnostics:?}");
        }
        let mut source = frames(&[0, 1, 2, 3, 3, 3, 3, 3, 3, 6, 7, 8], 33_333_333);
        for (i, (pixels, _)) in source.iter_mut().enumerate().take(9).skip(4) {
            let count = WIDTH * HEIGHT * (i - 3) / 5;
            for pixel in pixels.chunks_exact_mut(3).take(count) {
                pixel[0] += 1;
            }
        }
        assert_eq!(run(&source, 120, 1.0).0.repaired_runs, 0);
    }

    #[test]
    fn anti_stutter_near_lighting_cut_retires_history_before_later_hold() {
        for strength in [0, 100] {
            let mut input = frames(&[0, 1, 2, 3, 3, 3, 6, 7, 7, 9, 10, 11], 33_333_333);
            for (pixels, _) in input.iter_mut().take(6).skip(4) {
                for pixel in pixels.chunks_exact_mut(3) {
                    pixel[0] += 1;
                }
            }
            let (cut_at, changed_at, later_at) = (input[4].1, input[6].1, input[7].1);
            let mut window = Window::new(
                input[0].0.clone(),
                0,
                WIDTH,
                HEIGHT,
                400_000_000,
                true,
                strength,
            );
            let mut index = 1;
            let mut read = |pixels: &mut [u8]| {
                if index == input.len() {
                    return Ok(None);
                }
                pixels.copy_from_slice(&input[index].0);
                let pts = input[index].1;
                index += 1;
                Ok(Some(pts))
            };
            let (mut saw_cut, mut saw_later) = (false, false);
            while let Some(span) = window.next(&mut read).unwrap() {
                let left = window.pts(0);
                let right = span.right.map(|i| window.pts(i));
                let repaired = span.repaired;
                window.advance(span, u64::from(repaired));
                if right == Some(cut_at) {
                    saw_cut = true;
                    assert!(window.previous_motion.is_none());
                    assert!(window.hold_started.is_none());
                }
                if right == Some(changed_at) {
                    assert_eq!(
                        window.previous_motion.unwrap().0.delta_ns,
                        changed_at - cut_at
                    );
                }
                if left == later_at && repaired {
                    saw_later = true;
                }
            }
            assert!(
                saw_cut && saw_later,
                "retired context must not block a later supported hold"
            );
        }
    }

    #[test]
    fn anti_stutter_rejected_lighting_keeps_ordinary_smooth_spans() {
        for strength in [0, 100] {
            let mut input = frames(&[0, 1, 2, 3, 3, 3, 6, 7, 8, 9], 33_333_333);
            for (pixels, _) in input.iter_mut().take(6).skip(4) {
                for pixel in pixels.chunks_exact_mut(3) {
                    pixel[0] += 1;
                }
            }
            let mut window = Window::new(
                input[0].0.clone(),
                0,
                WIDTH,
                HEIGHT,
                333_333_330,
                true,
                strength,
            );
            let mut index = 1;
            let mut read = |pixels: &mut [u8]| {
                if index == input.len() {
                    return Ok(None);
                }
                pixels.copy_from_slice(&input[index].0);
                let pts = input[index].1;
                index += 1;
                Ok(Some(pts))
            };
            let rate = Rate::for_playback(120, 1.0).unwrap();
            let mut checked_lighting = false;
            let mut source_index = 0;
            let mut actual = Vec::new();
            let mut output = 0;
            let count = rate.frame_count(333_333_330).unwrap();
            while let Some(span) = window.next(&mut read).unwrap() {
                assert!(!span.repaired);
                assert_eq!(
                    span.right,
                    if source_index + 1 < input.len() {
                        Some(1)
                    } else {
                        None
                    }
                );
                assert_eq!(window.pixels(0), &input[source_index].0);
                let next = span.right.map(|i| window.pts(i));
                while output < count && next.is_none_or(|pts| rate.sample_before(output, pts)) {
                    let alpha = next
                        .map(|r| rate.alpha(output, window.pts(0), r).unwrap())
                        .unwrap_or(0.0);
                    let infer = alpha > 0.0
                        && alpha < 1.0
                        && !span.cut
                        && window.pixels(0) != window.pixels(span.right.unwrap_or(0));
                    actual.push((window.pts(0), next, alpha.to_bits(), infer));
                    output += 1;
                }
                if let Some(right) = span.right {
                    let (left, right) = (window.pixels(0), window.pixels(right));
                    let baseline_cut = is_scene_cut(left, right);
                    assert_eq!(
                        span.cut, baseline_cut,
                        "a qualification-only veto must not change execution"
                    );
                    let evidence = compare(left, right, WIDTH, HEIGHT).unwrap();
                    checked_lighting |= evidence.lighting && !baseline_cut;
                }
                window.advance(span, 0);
                source_index += 1;
            }
            let mut expected = Vec::new();
            let mut index = 0;
            for frame in 0..count {
                while index + 1 < input.len() && !rate.sample_before(frame, input[index + 1].1) {
                    index += 1;
                }
                let next = input.get(index + 1);
                let alpha = next
                    .map(|r| rate.alpha(frame, input[index].1, r.1).unwrap())
                    .unwrap_or(0.0);
                let infer = next.is_some_and(|r| {
                    alpha > 0.0
                        && alpha < 1.0
                        && !is_scene_cut(&input[index].0, &r.0)
                        && input[index].0 != r.0
                });
                expected.push((input[index].1, next.map(|r| r.1), alpha.to_bits(), infer));
            }
            assert_eq!(
                actual, expected,
                "worker inputs/alpha and holds must match the ordinary adjacent-pair schedule"
            );
            assert!(checked_lighting);
            assert_eq!(window.diagnostics.repaired_runs, 0);
            assert_eq!(window.cut_pairs, 0);
        }
    }

    #[test]
    fn anti_stutter_correspondence_rejects_flat_flash_and_ambiguous_periodic_matches() {
        for strength in [0, 100] {
            let a = picture(3);
            for amount in [1, 4, 12] {
                let mut flash = a.clone();
                for p in &mut flash {
                    *p = p.saturating_add(amount);
                }
                assert!(compare(&a, &flash, WIDTH, HEIGHT).unwrap().discontinuity());
            }
            let mut color = a.clone();
            for pixel in color.chunks_exact_mut(3) {
                pixel[2] = pixel[2].saturating_add(2);
            }
            assert!(compare(&a, &color, WIDTH, HEIGHT).unwrap().discontinuity());
            assert!(
                !continuous(
                    &vec![80; a.len()],
                    &vec![85; a.len()],
                    WIDTH,
                    HEIGHT,
                    Policy { strength }
                )
                .unwrap()
            );
            let mut periodic = vec![0; a.len()];
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    periodic[(y * WIDTH + x) * 3..(y * WIDTH + x + 1) * 3]
                        .fill(if (x / 2 + y / 2) % 2 == 0 { 40 } else { 180 });
                }
            }
            assert!(!continuous(&periodic, &periodic, WIDTH, HEIGHT, Policy { strength }).unwrap());
            let mut shifted = periodic.clone();
            for row in shifted.chunks_exact_mut(WIDTH * 3) {
                row.rotate_left(3);
            }
            assert!(!continuous(&periodic, &shifted, WIDTH, HEIGHT, Policy { strength }).unwrap());
        }
    }

    #[test]
    fn anti_stutter_fixed_textured_overlay_cannot_validate_a_switched_background() {
        let positions = [0, 1, 2, 3, 3, 3, 6, 7, 8, 9];
        let mut source = Vec::new();
        for (frame, position) in positions.into_iter().enumerate() {
            let mut pixels = vec![0; WIDTH * HEIGHT * 3];
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    let sx = if frame >= 6 { WIDTH - 1 - x } else { x };
                    let mut seed = ((sx + position) as u32).wrapping_mul(747796405)
                        ^ (y as u32).wrapping_mul(2891336453)
                        ^ 0x1475_8321;
                    seed ^= seed >> 16;
                    seed = seed.wrapping_mul(2246822519);
                    for c in 0..3 {
                        pixels[(y * WIDTH + x) * 3 + c] = 48 + ((seed >> (c * 8)) & 127) as u8;
                    }
                }
            }
            // Twenty-four fixed, individually textured patches span the image.
            // They can provide confident ZERO-motion matches across the cut.
            for ty in 0..6 {
                for tx in 0..8 {
                    if (tx + ty) % 2 != 0 {
                        continue;
                    }
                    let cx = (2 * tx + 1) * WIDTH / 16;
                    let cy = (2 * ty + 1) * HEIGHT / 12;
                    for py in 0..7 {
                        for px in 0..7 {
                            let value = ((px * 53 + py * 97 + tx * 31 + ty * 17) % 180 + 30) as u8;
                            let offset = ((cy + py - 3) * WIDTH + cx + px - 3) * 3;
                            pixels[offset..offset + 3].fill(value);
                        }
                    }
                }
            }
            source.push((pixels, frame as u64 * 33_333_333));
        }
        assert!(!is_scene_cut(&source[3].0, &source[6].0));
        let bridge = compare(&source[3].0, &source[6].0, WIDTH, HEIGHT).unwrap();
        assert!(
            bridge.moving(),
            "the background changes broadly despite the fixed overlay"
        );
        assert!(
            !continuous(
                &source[3].0,
                &source[6].0,
                WIDTH,
                HEIGHT,
                Policy { strength: 100 }
            )
            .unwrap()
        );
        assert!(
            !removable_continuous(
                &source[3].0,
                &source[6].0,
                WIDTH,
                HEIGHT,
                Policy { strength: 100 }
            )
            .unwrap(),
            "fixed overlay must not validate an unrelated background at coarse support"
        );
        let (removed, map) = remove_sized(&source, WIDTH, HEIGHT, 100);
        assert_eq!(map.removed_duration_ns(), 0, "{removed:?}");
        assert_eq!(run(&source, 120, 1.0).0.repaired_runs, 0);
    }

    #[test]
    fn anti_stutter_static_textured_background_does_not_make_small_object_motion_context() {
        let source: Vec<_> = (0..20)
            .map(|i| {
                let mut pixels = picture(3);
                for y in 10..14 {
                    for x in i * 2 + 4..i * 2 + 8 {
                        pixels[(y * WIDTH + x) * 3..(y * WIDTH + x + 1) * 3].fill(250);
                    }
                }
                (pixels, i as u64 * 33_333_333)
            })
            .collect();
        let (removed, map) = remove_sized(&source, WIDTH, HEIGHT, 100);
        assert_eq!(map.removed_duration_ns(), 0, "{removed:?}");
        assert_eq!(removed.repaired_runs, 0, "{removed:?}");
        assert_eq!(run(&source, 120, 1.0).0.repaired_runs, 0);
    }

    #[test]
    #[ignore = "requires explicitly supplied local RGB/PTS fixtures; never uploads media"]
    fn anti_stutter_local_source_diagnostics() {
        use std::io::Read;
        let width: usize = std::env::var("VFL_ANTI_WIDTH").unwrap().parse().unwrap();
        let height: usize = std::env::var("VFL_ANTI_HEIGHT").unwrap().parse().unwrap();
        let duration: f64 = std::env::var("VFL_ANTI_DURATION").unwrap().parse().unwrap();
        let rows: Vec<serde_json::Value> = serde_json::from_reader(
            std::fs::File::open(std::env::var("VFL_ANTI_PTS").unwrap()).unwrap(),
        )
        .unwrap();
        let pts: Vec<u64> = rows
            .iter()
            .map(|r| {
                r["ptsNs"]
                    .as_u64()
                    .unwrap_or_else(|| (r["t"].as_f64().unwrap() * 1e9).round() as u64)
            })
            .filter(|t| *t < (duration * 1e9).round() as u64)
            .collect();
        let mut file = std::fs::File::open(std::env::var("VFL_ANTI_RAW").unwrap()).unwrap();
        let mut first = vec![0; width * height * 3];
        file.read_exact(&mut first).unwrap();
        let mut window = Window::new(
            first,
            pts[0],
            width,
            height,
            (duration * 1e9).round() as u64,
            true,
            std::env::var("VFL_ANTI_STRENGTH")
                .unwrap_or_else(|_| "0".into())
                .parse()
                .unwrap(),
        )
        .at_source_beginning(std::env::var("VFL_ANTI_AT_SOURCE_BEGINNING").as_deref() == Ok("1"))
        .with_mode(
            if std::env::var("VFL_ANTI_MODE").as_deref() == Ok("remove") {
                AntiStutterMode::Remove
            } else {
                AntiStutterMode::Repair
            },
        );
        let mut index = 1;
        let mut read = |pixels: &mut [u8]| {
            if index == pts.len() {
                return Ok(None);
            }
            file.read_exact(pixels).unwrap();
            let t = pts[index];
            index += 1;
            Ok(Some(t))
        };
        let rate = Rate::for_playback(120, 1.0).unwrap();
        let mut count = rate.frame_count((duration * 1e9).round() as u64).unwrap();
        let mut map = super::super::RemovalMap::new((duration * 1e9).round() as u64);
        let mut output = 0;
        let mut spans = Vec::new();
        while let Some(span) = window.next(&mut read).unwrap() {
            if let Some(start) = span.remove_start {
                map.push(start, window.pts(span.right.unwrap())).unwrap();
                count = rate.frame_count(map.output_duration_ns()).unwrap();
            }
            let left = map.map_ns(window.pts(0));
            let right = span.right.map(|i| map.map_ns(window.pts(i)));
            let mut repaired = 0;
            let first_output = output;
            while output < count && right.is_none_or(|r| rate.sample_before(output, r)) {
                let alpha = right
                    .map(|r| rate.alpha(output, left, r).unwrap())
                    .unwrap_or(0.0);
                repaired += u64::from(
                    span.remove_start.is_none() && span.repaired && alpha > 0.0 && alpha < 1.0,
                );
                output += 1;
            }
            spans.push(serde_json::json!({"sourceStartNs":window.pts(0),"sourceEndNs":span.right.map(|i|window.pts(i)),"outputStartNs":left,"outputEndNs":right,"firstOutputIndex0":first_output,"endOutputIndex0Exclusive":output,"filled":span.repaired&&span.remove_start.is_none(),"removeStartNs":span.remove_start}));
            window.advance(span, repaired);
        }
        assert_eq!(output, count);
        assert!(window.allocated_high_water <= MAX_PICTURES);
        if let Ok(path) = std::env::var("VFL_ANTI_SPAN_REPORT") {
            serde_json::to_writer_pretty(std::fs::File::create(path).unwrap(), &spans).unwrap();
        }
        if let Ok(path) = std::env::var("VFL_ANTI_CUT_MAP_REPORT") {
            serde_json::to_writer_pretty(std::fs::File::create(path).unwrap(),&serde_json::json!({"sourceDurationNs":map.source_duration_ns(),"outputDurationNs":map.output_duration_ns(),"removedDurationNs":map.removed_duration_ns(),"cuts":map.cuts().iter().map(|c|serde_json::json!({"startNs":c.start_ns,"endNs":c.end_ns})).collect::<Vec<_>>(),"outputFrames":output,"pictureHighWater":window.allocated_high_water})).unwrap();
        }
        if let Ok(path) = std::env::var("VFL_ANTI_REPORT") {
            serde_json::to_writer_pretty(std::fs::File::create(path).unwrap(), &window.diagnostics)
                .unwrap();
        }
        eprintln!("{}", serde_json::to_string(&window.diagnostics).unwrap());
    }

    #[test]
    #[ignore = "local raw RGB diagnostic only; no user data in repository"]
    fn anti_stutter_local_pair_metrics() {
        use std::io::{Read, Seek, SeekFrom};
        let width: usize = std::env::var("VFL_ANTI_WIDTH").unwrap().parse().unwrap();
        let height: usize = std::env::var("VFL_ANTI_HEIGHT").unwrap().parse().unwrap();
        let pairs: Vec<(usize, usize)> = serde_json::from_reader(
            std::fs::File::open(std::env::var("VFL_ANTI_PAIRS").unwrap()).unwrap(),
        )
        .unwrap();
        let size = width * height * 3;
        let mut file = std::fs::File::open(std::env::var("VFL_ANTI_RAW").unwrap()).unwrap();
        let mut rows = Vec::new();
        for (a, b) in pairs {
            let mut left = vec![0; size];
            let mut right = vec![0; size];
            file.seek(SeekFrom::Start(((a - 1) * size) as u64)).unwrap();
            file.read_exact(&mut left).unwrap();
            file.seek(SeekFrom::Start(((b - 1) * size) as u64)).unwrap();
            file.read_exact(&mut right).unwrap();
            let evidence = compare(&left, &right, width, height).unwrap();
            let matched = correspondence(
                &Luma::new(&left, width, height).unwrap(),
                &Luma::new(&right, width, height).unwrap(),
            )
            .unwrap();
            let coarse = correspondence(
                &Luma::new(&left, width, height).unwrap().half().unwrap(),
                &Luma::new(&right, width, height).unwrap().half().unwrap(),
            )
            .unwrap();
            rows.push(serde_json::json!({"coarseLevel":{"textured":coarse.textured,"reliableMatches":coarse.matches.len(),"coherentNonzeroCluster":coarse.coherent,"continuous75":coarse.continuous(Policy{strength:75}),"matches":coarse.matches},"leftSourceFrame1Based":a,"rightSourceFrame1Based":b,"exact":evidence.exact,"near":evidence.near,"cut":evidence.cut,"lighting":evidence.lighting,"ambiguous":evidence.ambiguous,"moving":evidence.moving(),"rgbMeanMilli":evidence.mean,"coarseMeanMilli":evidence.coarse_mean,"textured":matched.textured,"reliableMatches":matched.matches.len(),"coherentNonzeroCluster":matched.coherent,"held":matched.held(),"matches":matched.matches,"continuous75":matched.continuous(Policy{strength:75}),"continuous100":matched.continuous(Policy{strength:100})}));
        }
        serde_json::to_writer_pretty(
            std::fs::File::create(std::env::var("VFL_ANTI_PAIR_REPORT").unwrap()).unwrap(),
            &rows,
        )
        .unwrap();
    }
}
