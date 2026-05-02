fn main() {
    ensure_ffmpeg_sidecar_placeholder();
    tauri_build::build()
}

fn ensure_ffmpeg_sidecar_placeholder() {
    use std::fs;
    use std::path::PathBuf;

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let sidecar_dir = PathBuf::from(manifest_dir)
        .join("..")
        .join(".ffmpeg-bundle")
        .join("windows-x64")
        .join("ffmpeg-sidecar");

    let _ = fs::create_dir_all(&sidecar_dir);

    let placeholder_path = sidecar_dir.join("README.txt");
    let sidecar_is_empty = fs::read_dir(&sidecar_dir)
        .ok()
        .and_then(|mut entries| entries.next())
        .is_none();
    if sidecar_is_empty && !placeholder_path.exists() {
        let _ = fs::write(
            placeholder_path,
            "Windows FFmpeg sidecar resources are populated on Windows build hosts.\n",
        );
    }
}
