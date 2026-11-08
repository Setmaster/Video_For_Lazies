//! Local image decoding and the final, timeline-preserving thumbnail overlay.
use super::*;
use base64::Engine;

pub(super) const FILE_NAME: &str = "vfl_thumbnail.png";
const MAX_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PIXELS: u64 = 40_000_000;
const COMPOSITE_START: &str = "null[vfl_thumb_base]";

/// Interpolation installs a final frame clock around the ordinary video chain.
/// Composite the thumbnail after that clock, including its final fps admission.
pub(crate) fn wrap_timeline(graph: &str, prefix: &str, suffix: &str) -> String {
    if let Some(index) = graph.find(COMPOSITE_START) {
        let timeline = graph[..index].trim_end_matches(',');
        let timeline = if timeline.is_empty() {
            prefix.to_string()
        } else {
            format!("{prefix},{timeline}")
        };
        format!("{timeline},{suffix},{}", &graph[index..])
    } else {
        format!("{prefix},{graph},{suffix}")
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ThumbnailFit {
    Fit,
    Fill,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThumbnailSettings {
    pub image_path: String,
    pub fit: ThumbnailFit,
    pub source_identity: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailInspection {
    pub preview_data_url: String,
    pub source_identity: String,
}

fn image_source(
    path: &Path,
) -> Result<(crate::media_source::SourceIdentity, &'static str), String> {
    let source = crate::media_source::SourceIdentity::capture(path).map_err(|_| {
        "The thumbnail image is missing or cannot be read. Choose it again.".to_string()
    })?;
    if source.len == 0 || source.len > MAX_BYTES {
        return Err("Choose a nonempty thumbnail image no larger than 32 MiB.".into());
    }
    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut file =
        fs::File::open(source.path()).map_err(|_| "Could not read the thumbnail image.")?;
    let mut signature = [0u8; 12];
    let count = file
        .read(&mut signature)
        .map_err(|_| "Could not read the thumbnail image.")?;
    let (valid, decoder) = match extension.as_str() {
        "jpg" | "jpeg" | "jpe" => (signature.starts_with(&[0xff, 0xd8, 0xff]), "mjpeg"),
        "png" => (signature.starts_with(b"\x89PNG\r\n\x1a\n"), "png"),
        "webp" => (
            count == 12 && &signature[..4] == b"RIFF" && &signature[8..] == b"WEBP",
            "webp",
        ),
        "bmp" => (signature.starts_with(b"BM"), "bmp"),
        "tif" | "tiff" => (
            signature.starts_with(b"II\x2a\0") || signature.starts_with(b"MM\0\x2a"),
            "tiff",
        ),
        "gif" => (
            signature.starts_with(b"GIF87a") || signature.starts_with(b"GIF89a"),
            "gif",
        ),
        _ => return Err("Choose a JPEG, PNG, WebP, BMP, TIFF or GIF thumbnail image.".into()),
    };
    if !valid {
        return Err(
            "The thumbnail file does not contain the selected image format. Choose a valid image."
                .into(),
        );
    }
    Ok((source, decoder))
}

fn decode_image(
    ffmpeg_bin: &str,
    source: &crate::media_source::SourceIdentity,
    decoder: &str,
    output: &Path,
    preview: bool,
) -> Result<(), String> {
    let mut command = command_no_window(ffmpeg_bin);
    // Force a single local image, never a playlist or a filename sequence.
    // The decoder whitelist and magic check also reject disguised video files.
    command
        .args([
            "-nostdin",
            "-v",
            "error",
            "-n",
            "-max_alloc",
            "268435456",
            "-protocol_whitelist",
            "file",
            "-f",
            "image2",
            "-pattern_type",
            "none",
            "-c:v",
            decoder,
            "-codec_whitelist",
            "mjpeg,png,webp,bmp,tiff,gif",
            "-max_pixels",
            &MAX_PIXELS.to_string(),
            "-threads",
            "1",
            "-i",
        ])
        .arg(source.path());
    // Composite transparency in RGB before any YUV range conversion. Painting
    // black on the main YUV stream would use limited-range Y=16 even when the
    // output is full-range, lifting transparent pixels instead of making black.
    // Make the composited image opaque before dropping alpha. Newer FFmpeg
    // converters otherwise unpremultiply it again while removing that plane.
    let mut filters = "format=gbrap,premultiply=inplace=1,lutrgb=a=255,format=rgb24".to_string();
    if preview {
        filters.push_str(",scale=w=640:h=640:force_original_aspect_ratio=decrease,setsar=1");
    }
    command.args(["-vf", &filters]);
    command
        .args([
            "-frames:v",
            "1",
            "-an",
            "-map_metadata",
            "-1",
            "-c:v",
            "png",
            "-pix_fmt",
            "rgb24",
            "-threads",
            "1",
            "-update",
            "1",
        ])
        .arg(output);
    let result = run_command_output_with_timeout(
        command,
        "ffmpeg",
        "VFL_FFMPEG_PATH",
        ffmpeg_bin,
        "thumbnail image validation",
        FRAME_EXTRACT_TIMEOUT,
    )?;
    if !result.status.success() || validate_frame_output(output).is_err() {
        return Err("The thumbnail image could not be decoded. Choose a valid image of at most 40 megapixels.".into());
    }
    source.verify(source.path()).map_err(|_| {
        "The thumbnail image changed during validation. Choose it again.".to_string()
    })?;
    Ok(())
}

pub fn inspect_thumbnail(path: String) -> Result<ThumbnailInspection, String> {
    let (source, decoder) = image_source(Path::new(&path))?;
    let temp = TempDir::new().map_err(|_| "Could not create thumbnail preview storage.")?;
    let output = temp.path().join(FILE_NAME);
    decode_image(&default_ffmpeg(), &source, decoder, &output, true)?;
    let bytes = fs::read(output).map_err(|_| "Could not read the thumbnail preview.")?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("The thumbnail preview is too large. Choose a smaller image.".into());
    }
    Ok(ThumbnailInspection {
        preview_data_url: format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ),
        source_identity: source.fingerprint().to_string(),
    })
}

pub(super) fn stage(
    ffmpeg_bin: &str,
    settings: &ThumbnailSettings,
    directory: &Path,
) -> Result<(), String> {
    let (source, decoder) = image_source(Path::new(&settings.image_path))?;
    source
        .verify_expected(Some(&settings.source_identity))
        .map_err(|_| {
            "The thumbnail image changed since it was selected. Choose it again before exporting."
                .to_string()
        })?;
    decode_image(
        ffmpeg_bin,
        &source,
        decoder,
        &directory.join(FILE_NAME),
        false,
    )
}

pub(super) fn apply(
    chain: Option<String>,
    settings: &ThumbnailSettings,
    width: u32,
    height: u32,
    overlay_format: &str,
    image_format: &str,
) -> String {
    let sizing = match settings.fit {
        ThumbnailFit::Fit => format!(
            "scale=w={width}:h={height}:force_original_aspect_ratio=decrease,format=rgb24,pad=w={width}:h={height}:x=(ow-iw)/2:y=(oh-ih)/2:color=black"
        ),
        ThumbnailFit::Fill => format!(
            "scale=w={width}:h={height}:force_original_aspect_ratio=increase,crop=w={width}:h={height}"
        ),
    };
    let prefix = chain.map(|chain| format!("{chain},")).unwrap_or_default();
    // RGB overlay negotiates an RGBA working surface. Opaque RGB video must
    // return to its original opaque domain, rather than making VP9 choose the
    // experimental GBR+alpha encoder merely because the image has an alpha plane.
    let opaque_rgb = if overlay_format == "rgb" {
        ",format=gbrp"
    } else {
        ""
    };
    // Keep the main stream in its existing YUV export domain. `auto` can choose
    // RGB to match the PNG, round-trip every video frame and leak RGB/full-range
    // metadata into a YUV encode. Only the opaque image branch is converted;
    // scale negotiates its YUV matrix and range from the unchanged main link.
    // RGB padding precedes that conversion so black stays correct in both
    // limited and full range. Main video owns every timestamp and EOF.
    format!(
        "{prefix}{COMPOSITE_START};movie=filename={FILE_NAME},setpts=PTS-STARTPTS,{sizing},scale=w=iw:h=ih:in_range=pc,format={image_format},setsar=1[vfl_thumb_image];[vfl_thumb_base][vfl_thumb_image]overlay=eof_action=repeat:repeatlast=1:shortest=0:format={overlay_format}:enable='eq(n,0)'{opaque_rgb}"
    )
}

pub(super) fn pixel_formats(
    request: &EncodeRequest,
    probe: &VideoProbe,
    policy: MediaPolicyPlan,
) -> Result<(&'static str, &'static str), String> {
    if request.format == OutputFormat::Mp4
        || policy.color_action.explicit_range().is_some()
        || policy.color_action.converts_to_standard_sdr()
    {
        return Ok(("yuv420", "yuva420p"));
    }
    // PreparedVideo restores generated pictures as yuv444p before the ordinary
    // filter chain, independently of the original source's pixel layout.
    if request.interpolation.is_some() {
        return Ok(("yuv444", "yuva444p"));
    }
    // VP9 can retain richer source chroma. Pin the image to the main domain,
    // rather than making every frame adopt PNG's RGB or downsample to 4:2:0.
    let pixel = probe.pixel_format.as_deref().unwrap_or("");
    if pixel.starts_with("gbrap") || matches!(pixel, "rgba" | "bgra" | "argb" | "abgr" | "pal8") {
        return Err("Thumbnail images cannot preserve this source's RGB alpha in WebM. Export an MP4 first, or remove the thumbnail.".into());
    }
    if pixel.starts_with("yuv444") || pixel.starts_with("yuvj444") || pixel.starts_with("yuva444") {
        Ok(("yuv444", "yuva444p"))
    } else if pixel.starts_with("yuv422")
        || pixel.starts_with("yuvj422")
        || pixel.starts_with("yuva422")
        || matches!(pixel, "yuyv422" | "uyvy422" | "yvyu422")
    {
        Ok(("yuv422", "yuva422p"))
    } else if pixel.starts_with("gbr")
        || pixel.contains("rgb")
        || pixel.contains("bgr")
        || pixel == "pal8"
    {
        if color_is_noop(&request.color) {
            Ok(("rgb", "rgba"))
        } else {
            Ok(("yuv444", "yuva444p"))
        }
    } else if pixel.starts_with("yuv420")
        || pixel.starts_with("yuva420")
        || pixel.starts_with("yuvj420")
        || pixel.starts_with("gray")
        || matches!(pixel, "nv12" | "nv21")
    {
        Ok(("yuv420", "yuva420p"))
    } else {
        Err("Thumbnail images cannot preserve this source's pixel layout in WebM. Export an MP4 first, or remove the thumbnail.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> ThumbnailSettings {
        ThumbnailSettings {
            image_path: "C:\\private\\O'Brien,[draft];图片.png".into(),
            fit: ThumbnailFit::Fit,
            source_identity: "a".repeat(64),
        }
    }

    #[test]
    fn thumbnail_graph_uses_only_fixed_staged_name_and_final_timeline() {
        let settings = settings();
        let graph = apply(
            Some("trim=start=1,reverse,fps=30".into()),
            &settings,
            160,
            90,
            "yuv420",
            "yuva420p",
        );
        assert!(!graph.contains(&settings.image_path));
        let graph = wrap_timeline(&graph, "settb=1/120000", "fps=60,trim=end_frame=120");
        assert!(graph.find("trim=end_frame=120").unwrap() < graph.find(COMPOSITE_START).unwrap());
        assert!(graph.contains("scale=w=160:h=90:force_original_aspect_ratio=decrease"));
        assert!(graph.contains("shortest=0"));
        assert_eq!(
            wrap_timeline("reverse", "settb=1/120000", "fps=60"),
            "settb=1/120000,reverse,fps=60"
        );
        let names = filter_names_from_graph(Some(&graph));
        assert!(names.contains("movie") && names.contains("overlay") && names.contains("null"));
    }

    #[test]
    fn thumbnail_rejects_disguised_inputs_and_missing_or_changed_files_before_export() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("private.png");
        fs::write(&path, b"#EXTM3U\nfile:///private/video.mp4").unwrap();
        assert!(
            inspect_thumbnail(path.to_string_lossy().into())
                .unwrap_err()
                .contains("format")
        );
        fs::write(&path, b"\x89PNG\r\n\x1a\nplaceholder").unwrap();
        let (source, _) = image_source(&path).unwrap();
        let mut settings = settings();
        settings.image_path = path.to_string_lossy().into();
        settings.source_identity = source.fingerprint().into();
        drop(source);
        fs::write(&path, b"\x89PNG\r\n\x1a\nchanged-placeholder").unwrap();
        assert!(
            stage("unreachable-ffmpeg", &settings, temp.path())
                .unwrap_err()
                .contains("changed")
        );
        fs::remove_file(&path).unwrap();
        assert!(
            stage("unreachable-ffmpeg", &settings, temp.path())
                .unwrap_err()
                .contains("missing")
        );
    }

    #[test]
    fn thumbnail_forces_video_encode_and_composites_after_loop_before_uniqueness() {
        let probe = parse_probe_output(r#"{"format":{"duration":"1"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":160,"height":90,"pix_fmt":"yuv420p","avg_frame_rate":"30/1"}]}"#).unwrap();
        let mut req = super::super::tests::base_request();
        req.thumbnail = Some(settings());
        req.loop_video = true;
        req.reverse = true;
        req.perturb_first_frame = true;
        req.perturb_seed = Some(42);
        req.advanced.frame_rate_cap_fps = Some(24);
        assert!(video_transform_requires_encode(&req).unwrap());
        let graph = build_video_filters(&req, &probe).unwrap().unwrap();
        assert!(graph.find("concat").unwrap() < graph.find(COMPOSITE_START).unwrap());
        assert!(graph.find("fps=24").unwrap() < graph.find(COMPOSITE_START).unwrap());
        assert!(graph.find("overlay=").unwrap() < graph.find("noise=").unwrap());
        assert_eq!(graph.matches("noise=").count(), 1);
    }

    #[test]
    fn thumbnail_uses_prepared_interpolation_domain_and_retains_explicit_output_policy() {
        let probe = parse_probe_output(r#"{"format":{"duration":"1"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":160,"height":90,"pix_fmt":"yuv420p","avg_frame_rate":"30/1"}]}"#).unwrap();
        let mut req = super::super::tests::base_request();
        req.format = OutputFormat::Webm;
        req.thumbnail = Some(settings());
        req.interpolation = Some(InterpolationSettings {
            anti_stutter: false,
            anti_stutter_strength: 0,
            target_fps: 60,
            model_id: "rife-v4.25".into(),
        });
        let graph = build_video_filters(&req, &probe).unwrap().unwrap();
        assert!(graph.contains("format=yuva444p"));
        assert!(graph.contains("format=yuv444:enable="));
        req.advanced.color_range = ColorRangePreference::Full;
        let graph = build_video_filters(&req, &probe).unwrap().unwrap();
        assert!(graph.contains("format=yuva420p"));
        assert!(graph.contains("format=yuv420:enable="));
        req.format = OutputFormat::Mp4;
        req.advanced.color_range = ColorRangePreference::Auto;
        assert!(
            build_video_filters(&req, &probe)
                .unwrap()
                .unwrap()
                .contains("format=yuv420:enable=")
        );
    }
}
