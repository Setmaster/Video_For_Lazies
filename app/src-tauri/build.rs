fn main() {
    ensure_ffmpeg_sidecar_placeholder();
    ensure_interpolation_sidecar_placeholder();
    ensure_recording_sidecar_placeholder();
    let attributes = configure_windows_binary_manifests();
    tauri_build::try_build(attributes).expect("failed to run the Tauri build script")
}

fn configure_windows_binary_manifests() -> tauri_build::Attributes {
    use std::path::PathBuf;

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return tauri_build::Attributes::new();
    }

    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        panic!("Windows portable builds require the supported MSVC target");
    }

    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string()));
    let out_dir = PathBuf::from(
        std::env::var("OUT_DIR").expect("Cargo must provide OUT_DIR to the build script"),
    );

    // tauri-build normally links one manifest resource into every binary in
    // this Cargo package. Disable only that shared manifest, retain Tauri's
    // shared icon/version resource, then attach the equivalent main-app
    // manifest and explicit helper manifest to their respective binaries.
    for (binary, file_name) in [
        ("video_for_lazies", "video-for-lazies.manifest"),
        ("vfl-update-helper", "vfl-update-helper.manifest"),
        ("vfl_recording_host", "vfl-recording-host.manifest"),
    ] {
        let source_path = manifest_dir.join(file_name);
        let linker_path = out_dir.join(file_name);
        println!("cargo:rerun-if-changed={}", source_path.display());
        std::fs::copy(&source_path, &linker_path)
            .unwrap_or_else(|error| panic!("failed to stage {file_name}: {error}"));

        println!("cargo:rustc-link-arg-bin={binary}=/MANIFEST:EMBED");
        if binary == "video_for_lazies" {
            // Preserve Tauri's existing main-app manifest verbatim. link.exe
            // otherwise injects its own UAC declaration during manifest merge.
            println!("cargo:rustc-link-arg-bin={binary}=/MANIFESTUAC:NO");
        }
        println!(
            "cargo:rustc-link-arg-bin={binary}=/MANIFESTINPUT:{}",
            linker_path.display()
        );
    }

    tauri_build::Attributes::new()
        .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest())
}

fn ensure_recording_sidecar_placeholder() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let sidecar = manifest.join("target/recording-sidecar");
    std::fs::create_dir_all(&sidecar).expect("create recording resource directory");
    let notice = sidecar.join("RECORDING_NOTICES.txt");
    if !notice.exists() {
        std::fs::write(
            notice,
            "Recording resources are prepared by npm run prepare:recording-sidecar.\n",
        )
        .expect("create recording resource placeholder");
    }
}

fn ensure_ffmpeg_sidecar_placeholder() {
    use std::fs;
    use std::path::PathBuf;

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let sidecar_dir = PathBuf::from(manifest_dir)
        .join("..")
        .join(".ffmpeg-bundle")
        .join("current")
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
            "FFmpeg sidecar resources are populated on supported release build hosts.\n",
        );
    }
}

fn ensure_interpolation_sidecar_placeholder() {
    let manifest_dir = std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string()),
    );
    let sidecar = manifest_dir
        .join("..")
        .join(".interpolation-bundle")
        .join("current")
        .join("interpolation-sidecar");
    let _ = std::fs::create_dir_all(&sidecar);
    if std::fs::read_dir(&sidecar)
        .ok()
        .and_then(|mut entries| entries.next())
        .is_none()
    {
        let _ = std::fs::write(
            sidecar.join("README.txt"),
            "The native interpolation runtime is prepared by npm run prepare:interpolation-sidecar.\n",
        );
    }
}
