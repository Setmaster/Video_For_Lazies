//! Conservative, opt-in source-anchor selection. This module never owns a clock.
use crate::interpolation_timeline::is_scene_cut;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

const MAX_PICTURES: usize = 10;
const MAX_BUFFER_BYTES: usize = 256 * 1024 * 1024;
const MAX_REPEATED: usize = 6;
const MAX_SPAN_NS: u64 = 100_000_000;
const DETAIL_LIMIT: usize = 32;
const TILES: usize = 48;

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
    pub analyzed_frames: u64,
    pub candidate_runs: u64,
    pub repaired_runs: u64,
    pub repaired_frames: u64,
    pub unchanged_runs: u64,
    pub intervals: Vec<Interval>,
}

#[derive(Clone, Copy)]
struct Evidence {
    exact: bool,
    near: bool,
    cut: bool,
    mean: u64,
    tiles: [u64; TILES],
    delta_ns: u64,
}
impl Evidence {
    fn plateau(self) -> bool {
        self.exact || self.near
    }
    fn moving(self) -> bool {
        !self.cut
            && self.mean >= 2_000
            && self.tiles.iter().filter(|mean| **mean >= 2_000).count() >= 12
    }
}

fn compare(a: &[u8], b: &[u8], width: usize, height: usize) -> Result<Evidence, String> {
    let mut total = 0u64;
    let mut max_difference = 0u8;
    let mut tiles = [0u64; TILES];
    for ty in 0..6 {
        for tx in 0..8 {
            let mut sum = 0u64;
            let mut count = 0u64;
            for y in height * ty / 6..height * (ty + 1) / 6 {
                crate::process::check_cancelled().map_err(|_| "Canceled.".to_string())?;
                let first = (y * width + width * tx / 8) * 3;
                let last = (y * width + width * (tx + 1) / 8) * 3;
                for (&left, &right) in a[first..last].iter().zip(&b[first..last]) {
                    let difference = left.abs_diff(right);
                    sum += u64::from(difference);
                    max_difference = max_difference.max(difference);
                    count += 1;
                }
            }
            tiles[ty * 8 + tx] = sum * 1_000 / count.max(1);
            total += sum;
        }
    }
    let mean = total * 1_000 / a.len().max(1) as u64;
    Ok(Evidence {
        exact: total == 0,
        // Near matches are reported, never repaired by policy version 1.
        near: total != 0 && mean <= 500 && max_difference <= 6,
        cut: is_scene_cut(a, b),
        mean,
        tiles,
        delta_ns: 0,
    })
}

struct Picture {
    pixels: Vec<u8>,
    pts: u64,
    incoming: Option<Evidence>,
    before: Option<Evidence>,
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
}

pub(super) struct Window {
    pictures: VecDeque<Picture>,
    pool: Vec<Vec<u8>>,
    width: usize,
    height: usize,
    frame_bytes: usize,
    duration_ns: u64,
    eof: bool,
    skipped: Option<Skipped>,
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
    ) -> Self {
        let frame_bytes = pixels.len();
        Self {
            pictures: VecDeque::from([Picture {
                pixels,
                pts,
                incoming: None,
                before: None,
            }]),
            pool: Vec::new(),
            width,
            height,
            frame_bytes,
            duration_ns,
            eof: false,
            skipped: None,
            cut_pairs: 0,
            high_water: 1,
            allocated_high_water: 1,
            diagnostics: AntiStutterDiagnostics {
                version: 1,
                analyzed_frames: u64::from(real_first),
                candidate_runs: 0,
                repaired_runs: 0,
                repaired_frames: 0,
                unchanged_runs: 0,
                intervals: Vec::new(),
            },
        }
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
            let before = previous.incoming;
            self.pictures.push_back(Picture {
                pixels,
                pts,
                incoming: Some(evidence),
                before,
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
        }
    }

    fn qualify(&self, right: usize) -> Result<Option<&'static str>, String> {
        let first = &self.pictures[0];
        let Some(before) = first.before else {
            return Ok(Some("missingMotionContext"));
        };
        let Some(incoming) = first.incoming else {
            return Ok(Some("missingMotionContext"));
        };
        let after = self.pictures[right + 1]
            .incoming
            .expect("post-context evidence");
        let after_next = self.pictures[right + 2]
            .incoming
            .expect("post-context evidence");
        let context = [before, incoming, after, after_next];
        let bridge = compare(
            &first.pixels,
            &self.pictures[right].pixels,
            self.width,
            self.height,
        )?;
        if bridge.cut
            || context.iter().any(|e| e.cut)
            || self
                .pictures
                .iter()
                .take(right + 1)
                .skip(1)
                .any(|p| p.incoming.is_some_and(|e| e.cut))
        {
            return Ok(Some("sceneCut"));
        }
        if !context.iter().all(|e| e.moving()) {
            return Ok(Some("missingMotionContext"));
        }
        let minimum_step = context
            .iter()
            .map(|e| e.delta_ns)
            .chain(
                self.pictures
                    .iter()
                    .take(right + 1)
                    .skip(1)
                    .filter_map(|p| p.incoming.map(|e| e.delta_ns)),
            )
            .min()
            .unwrap_or(0);
        let maximum_step = context
            .iter()
            .map(|e| e.delta_ns)
            .chain(
                self.pictures
                    .iter()
                    .take(right + 1)
                    .skip(1)
                    .filter_map(|p| p.incoming.map(|e| e.delta_ns)),
            )
            .max()
            .unwrap_or(0);
        if minimum_step == 0 || maximum_step - minimum_step > minimum_step / 50 + 2 {
            return Ok(Some("cadence"));
        }
        let low = context.iter().map(|e| e.mean).min().unwrap_or(0);
        let high = context.iter().map(|e| e.mean).max().unwrap_or(0);
        // Large/decorrelated motion is not a reliable reconstruction context,
        // even if an elapsed-step bound would otherwise admit any possible SAD.
        if high > 20_000 {
            return Ok(Some("missingMotionContext"));
        }
        if high > low * 2 {
            return Ok(Some("missingMotionContext"));
        }
        // A bridge must resemble the surrounding spatial motion accumulated
        // over these source steps. Full RGB tile errors catch changes missed
        // by a strided histogram, including same-histogram scene switches.
        let steps = right as u64;
        if bridge.mean * 2 < low * steps {
            return Ok(Some("missingMotionContext"));
        }
        if bridge.mean * 2 > high * steps * 3 {
            return Ok(Some("sceneCut"));
        }
        for tile in 0..TILES {
            let bound = context.iter().map(|e| e.tiles[tile]).max().unwrap_or(0);
            if bridge.tiles[tile] > bound * steps * 2 + 2_000 {
                return Ok(Some("sceneCut"));
            }
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
            if !self.pictures[right]
                .incoming
                .expect("incoming evidence")
                .plateau()
            {
                break;
            }
            if right > MAX_REPEATED {
                return Ok(Some(self.unchanged("excessiveHold")));
            }
            right += 1;
        }
        if right < 3 {
            return Ok(Some(self.unchanged("cadence")));
        }
        if self
            .pictures
            .iter()
            .take(right)
            .skip(1)
            .any(|p| p.incoming.is_some_and(|e| !e.exact))
        {
            return Ok(Some(self.unchanged("uncertainNearDuplicate")));
        }
        if !self.ensure(right + 3, read)? {
            return Ok(Some(self.unchanged(if self.eof {
                "missingMotionContext"
            } else {
                "resourceLimit"
            })));
        }
        if let Some(reason) = self.qualify(right)? {
            return Ok(Some(self.unchanged(reason)));
        }
        let detail = self.detail(self.pts(0), self.pts(right), (right - 1) as u64, "repaired");
        Ok(Some(Span {
            right: Some(right),
            repaired: true,
            cut: false,
            detail,
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
        let mut result = Vec::with_capacity(WIDTH * HEIGHT * 3);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let x = x as f64 + position as f64 * 2.0;
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
        let duration =
            input.last().unwrap().1 + input.get(1).map_or(33_333_333, |p| p.1 - input[0].1);
        let mut window = Window::new(
            input[0].0.clone(),
            input[0].1,
            WIDTH,
            HEIGHT,
            duration,
            true,
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
        let rate = Rate::for_playback(fps, speed).unwrap();
        let count = rate.frame_count(duration).unwrap();
        let mut output = 0;
        let mut spans = Vec::new();
        while let Some(span) = window.next(&mut read).unwrap() {
            let left = window.pts(0);
            let right = span.right.map(|i| window.pts(i));
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
    fn anti_stutter_preserves_static_single_slots_cadence_and_boundary_holds() {
        let controls = [
            vec![3; 20],
            vec![0, 1, 2, 3, 3, 5, 6, 7],
            (0..10).flat_map(|p| [p, p]).collect(),
            (0..10).flat_map(|p| [p, p, p, p]).collect(),
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
        let cadence: Vec<_> = (0..20)
            .flat_map(|p| std::iter::repeat_n(p, if p % 2 == 0 { 2 } else { 3 }))
            .skip(1)
            .collect();
        assert_eq!(
            run(&frames(&cadence, 16_666_667), 60, 1.0).0.repaired_runs,
            0
        );
    }

    #[test]
    fn anti_stutter_near_noise_small_hud_slow_and_color_changes_are_not_repaired() {
        let mut near = frames(&[0, 1, 2, 3, 3, 3, 6, 7, 8, 9], 16_666_667);
        for (i, (pixels, _)) in near.iter_mut().enumerate().take(6).skip(4) {
            for pixel in pixels.chunks_exact_mut(3) {
                pixel[0] = pixel[0].saturating_add((i - 3) as u8);
            }
        }
        let (diagnostics, _, _) = run(&near, 60, 1.0);
        assert_eq!(diagnostics.repaired_runs, 0);
        assert!(
            diagnostics
                .intervals
                .iter()
                .any(|i| i.reason == "uncertainNearDuplicate")
        );
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
        assert_eq!(high, MAX_PICTURES);
    }

    #[test]
    fn anti_stutter_rejects_vfr_gaps_and_nonincreasing_pts() {
        let mut source = frames(&[0, 1, 2, 3, 3, 3, 6, 7, 8, 9], 16_666_667);
        for (_, pts) in source.iter_mut().skip(5) {
            *pts += 4_000_000;
        }
        assert_eq!(run(&source, 60, 1.0).0.repaired_runs, 0);
        let mut window = Window::new(picture(0), 20, WIDTH, HEIGHT, 100, true);
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
            *pts += 1;
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
            diagnostics.intervals.iter().any(|i| i.reason == "sceneCut"),
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
        let mut window = Window::new(vec![0; 3], 0, 1, 1, 1, true);
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
        let mut window = Window::new(picture(0), 0, WIDTH, HEIGHT, 100, true);
        assert!(
            window
                .next(&mut |_| panic!("cancelled input must not read"))
                .unwrap_err()
                .contains("Canceled")
        );
    }
}
