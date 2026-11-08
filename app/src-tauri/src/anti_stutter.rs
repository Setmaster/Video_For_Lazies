//! Conservative, opt-in source-anchor selection. This module never owns a clock.
use crate::interpolation_timeline::is_scene_cut;
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AntiStutterDiagnostics {
    pub version: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub strength: u8,
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
    detail: Option<usize>,
    motion: Option<Evidence>,
}

pub(super) struct Window {
    policy: Policy,
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
        });
        Some(index)
    }

    fn finish_skip(&mut self, end: u64) {
        if let Some(skipped) = self.skipped.take() {
            self.detail(skipped.start, end, skipped.repeated, skipped.reason);
        }
    }

    fn unchanged(&mut self, reason: &'static str) -> Span {
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

    pub fn next(
        &mut self,
        read: &mut impl FnMut(&mut [u8]) -> Result<Option<u64>, String>,
    ) -> Result<Option<Span>, String> {
        if self.pictures.is_empty() {
            return Ok(None);
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
        if let Some(reason) = self.qualify(right, residual, read)? {
            return Ok(Some(self.unchanged(reason)));
        }
        let detail = self.detail(self.pts(0), self.pts(right), (right - 1) as u64, "repaired");
        Ok(Some(Span {
            right: Some(right),
            repaired: true,
            cut: false,
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
        if span.repaired {
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
            if evidence.discontinuity() {
                // Near similarity and a lighting/cut flag can coexist. A cut
                // always retires history before any new plateau is timed.
                self.previous_motion = None;
                self.hold_started = None;
            } else if !span.repaired && evidence.plateau() {
                let start = *self.hold_started.get_or_insert(left_pts);
                if right_pts - start > MAX_SPAN_NS {
                    self.previous_motion = None;
                }
            } else {
                let start = self.hold_started.take().unwrap_or(left_pts);
                evidence.delta_ns = right_pts - start;
                self.previous_motion = if evidence.moving() && evidence.delta_ns <= MAX_SPAN_NS {
                    Some((evidence, right_pts))
                } else {
                    None
                };
            }
        } else {
            self.previous_motion = None;
            self.hold_started = None;
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
        .at_source_beginning(std::env::var("VFL_ANTI_AT_SOURCE_BEGINNING").as_deref() == Ok("1"));
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
        let count = rate.frame_count((duration * 1e9).round() as u64).unwrap();
        let mut output = 0;
        while let Some(span) = window.next(&mut read).unwrap() {
            let left = window.pts(0);
            let right = span.right.map(|i| window.pts(i));
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
        assert_eq!(output, count);
        assert!(window.allocated_high_water <= MAX_PICTURES);
        if let Ok(path) = std::env::var("VFL_ANTI_REPORT") {
            serde_json::to_writer_pretty(std::fs::File::create(path).unwrap(), &window.diagnostics)
                .unwrap();
        }
        eprintln!("{}", serde_json::to_string(&window.diagnostics).unwrap());
    }
}
