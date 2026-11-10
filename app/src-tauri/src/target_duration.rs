//! Clip-scoped desired-duration intent; manual playback speeds are unchanged.
use super::*;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TargetDurationDiagnostics {
    pub requested_duration_s: f64,
    pub retained_duration_s: f64,
    pub calculated_speed: Option<f64>,
    pub effective_speed: f64,
    pub estimated_output_duration_s: f64,
    pub speed_limited: bool,
    pub frame_rounded: bool,
}

pub(super) fn target_timing_requested(request: &EncodeRequest) -> bool {
    request.target_duration_s.is_some() || request.target_duration_sample
}
pub(super) fn ordinary_target_timing(request: &EncodeRequest) -> bool {
    target_timing_requested(request)
        && request.interpolation.is_none()
        && request.format != OutputFormat::Mp3
}

pub(super) fn resolve_target_duration(
    request: &mut EncodeRequest,
    retained: f64,
) -> Result<Option<TargetDurationDiagnostics>, String> {
    let Some(target) = request.target_duration_s else {
        return Ok(None);
    };
    if !target.is_finite() || target <= 0.0 {
        return Err("Target duration must be a finite number greater than zero seconds.".into());
    }
    if !retained.is_finite() || retained <= 0.0 || retained * 1e9 >= u64::MAX as f64 {
        return Err(
            "The retained duration cannot be represented for automatic playback speed.".into(),
        );
    }
    let loops = if request.loop_video && request.format != OutputFormat::Mp3 {
        2.0
    } else {
        1.0
    };
    let calculated = retained * loops / target;
    let wanted = calculated.clamp(0.05, 16.0);
    let limited = !(0.05..=16.0).contains(&calculated);
    let units = |value: f64| value.clamp(50_000.0, 16_000_000.0) as u64;
    let mut candidates = vec![units((wanted * 1e6).round())];
    let fps = request.interpolation.as_ref().map(|s| s.target_fps);
    let duration_ns = (retained * 1e9).round() as u64;
    if let Some(fps) = fps {
        candidates.extend([
            50_000,
            16_000_000,
            units((wanted * 1e6).floor()),
            units((wanted * 1e6).ceil()),
        ]);
        let frames = target * f64::from(fps) / loops;
        for n in [frames.floor().max(1.0), frames.ceil().max(1.0)] {
            let boundary = retained * f64::from(fps) / n * 1e6;
            candidates.extend([units(boundary.floor()), units(boundary.ceil())]);
        }
    }
    let mut best: Option<(f64, f64, u64, f64)> = None;
    for value in candidates {
        let speed = value as f64 / 1e6;
        let duration = if let Some(fps) = fps {
            let rate = crate::interpolation_timeline::Rate::for_playback(fps, speed)?;
            let count = (u128::from(duration_ns).saturating_mul(2).saturating_sub(1)
                * u128::from(rate.numerator))
            .div_ceil(2_000_000_000 * u128::from(rate.denominator));
            count as f64 / f64::from(fps) * loops
        } else {
            retained / speed * loops
        };
        let score = (
            (duration - target).abs(),
            (speed - wanted).abs(),
            value,
            duration,
        );
        if best.as_ref().is_none_or(|old| {
            score
                .0
                .total_cmp(&old.0)
                .then(score.1.total_cmp(&old.1))
                .then(score.2.cmp(&old.2))
                .is_lt()
        }) {
            best = Some(score);
        }
    }
    let (_, _, value, estimated) = best.ok_or("Could not resolve the target duration.")?;
    let speed = value as f64 / 1e6;
    if let Some(fps) = fps {
        crate::interpolation_timeline::Rate::for_playback(fps, speed)?.frame_count(duration_ns)?;
    }
    request.speed = speed;
    Ok(Some(TargetDurationDiagnostics {
        requested_duration_s: target,
        retained_duration_s: retained,
        calculated_speed: calculated.is_finite().then_some(calculated),
        effective_speed: speed,
        estimated_output_duration_s: estimated,
        speed_limited: limited,
        frame_rounded: fps.is_some() && (estimated - target).abs() > 1e-9,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn target_duration_request_keeps_manual_ipc_and_rejects_unresolved_samples() {
        let mut req = super::super::tests::base_request();
        req.speed = 32.;
        assert!(validate_base_request_scalars(&req).is_ok());
        assert!(resolve_target_duration(&mut req, 2.).unwrap().is_none());
        assert_eq!(req.speed, 32.);
        let legacy = serde_json::json!({"inputPath":"in.mp4","outputPath":"out.mp4","format":"mp4","sizeLimitMb":0.,"audioEnabled":true,"reverse":false,"speed":32.,"rotateDeg":0});
        let old: EncodeRequest = serde_json::from_value(legacy).unwrap();
        assert!(old.target_duration_s.is_none() && !old.is_sample);
        req.target_duration_s = Some(1.);
        req.is_sample = true;
        assert!(
            validate_base_request_scalars(&req)
                .unwrap_err()
                .contains("resolved full-video")
        );
        req.interpolation=Some(serde_json::from_value(serde_json::json!({"targetFps":60,"modelId":"rife-v4.25","antiStutter":true,"antiStutterMode":"remove"})).unwrap());
        assert!(
            validate_base_request_scalars(&req)
                .unwrap_err()
                .contains("full export")
        );
        req.is_sample = false;
        req.target_duration_s = Some(f64::NAN);
        assert!(validate_base_request_scalars(&req).is_err());
    }
    #[test]
    fn target_duration_manual_legacy_clamps_fractional_grid_and_loop() {
        let mut req = super::super::tests::base_request();
        let manual = req.speed;
        assert!(resolve_target_duration(&mut req, 2.).unwrap().is_none());
        assert_eq!(req.speed, manual);
        req.target_duration_s = Some(1.);
        req.loop_video = true;
        let d = resolve_target_duration(&mut req, 0.75).unwrap().unwrap();
        assert_eq!(d.effective_speed, 1.5);
        assert_eq!(
            resolve_target_duration(&mut req, 0.6)
                .unwrap()
                .unwrap()
                .effective_speed,
            1.2
        );
        req.target_duration_s = Some(f64::MIN_POSITIVE);
        let d = resolve_target_duration(&mut req, 2.).unwrap().unwrap();
        assert_eq!(d.effective_speed, 16.);
        assert!(d.speed_limited && d.calculated_speed.is_none());
        req.target_duration_s = Some(f64::MAX);
        assert_eq!(
            resolve_target_duration(&mut req, 2.)
                .unwrap()
                .unwrap()
                .effective_speed,
            0.05
        );
        for target in [0., -1., f64::NAN, f64::INFINITY] {
            req.target_duration_s = Some(target);
            assert!(resolve_target_duration(&mut req, 2.).is_err());
        }
        req.interpolation = Some(
            serde_json::from_value(serde_json::json!({"targetFps":60,"modelId":"rife-v4.25"}))
                .unwrap(),
        );
        req.target_duration_s = Some(1.01);
        let d = resolve_target_duration(&mut req, 1.).unwrap().unwrap();
        assert_eq!(d.estimated_output_duration_s, 1.);
        assert!(d.frame_rounded);
        req.target_duration_s = Some(0.5);
        req.loop_video = false;
        let d = resolve_target_duration(&mut req, 1.01).unwrap().unwrap();
        assert_eq!(d.estimated_output_duration_s, 0.5);
        assert_eq!(d.effective_speed, 2.02);
        req.loop_video = true;
        let d = resolve_target_duration(&mut req, 1.01).unwrap().unwrap();
        assert_eq!(d.estimated_output_duration_s, 0.5);
        assert_eq!(d.effective_speed, 4.04);
        req.format = OutputFormat::Mp3;
        req.interpolation = None;
        let d = resolve_target_duration(&mut req, 1.01).unwrap().unwrap();
        assert_eq!(d.effective_speed, 2.02);
    }
}
