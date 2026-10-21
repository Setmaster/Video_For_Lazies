//! Private, bounded stdio protocol. Media APIs are linked only into this helper.
#[cfg(windows)]
#[path = "../recording/host_protocol.rs"]
mod host_protocol;
#[cfg(windows)]
#[path = "../recording/windows.rs"]
mod windows;

#[cfg(windows)]
fn main() {
    use std::io::{BufRead, Read, Write};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    fn send(mut value: serde_json::Value) {
        value["protocolVersion"] = serde_json::json!(3);
        let mut out = std::io::stdout().lock();
        if serde_json::to_writer(&mut out, &value).is_err()
            || out.write_all(b"\n").is_err()
            || out.flush().is_err()
        {
            // A missing parent must never leave capture running.
            std::process::exit(2);
        }
    }
    let mut input = std::io::BufReader::new(std::io::stdin());
    let mut first = Vec::new();
    let initial = input.by_ref().take(65_537).read_until(b'\n', &mut first);
    if initial.is_err() || first.len() > 65_536 || !first.ends_with(b"\n") {
        send(serde_json::json!({"type":"error","message":"Invalid recording request."}));
        return;
    }
    let request: serde_json::Value = match serde_json::from_slice(&first) {
        Ok(value) => value,
        Err(_) => {
            send(serde_json::json!({"type":"error","message":"Invalid recording request."}));
            return;
        }
    };
    if request["protocolVersion"] != 3 {
        send(
            serde_json::json!({"type":"error","message":"Recording component version mismatch. Restore the complete app folder."}),
        );
        return;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let cancel = stop.clone();
    let preview = Arc::new(windows::preview_control::Controls::default());
    let controls = preview.clone();
    let audio = Arc::new(windows::audio_pipeline::Controls::default());
    let audio_input = audio.clone();
    std::thread::spawn(move || {
        loop {
            let mut line = Vec::new();
            let result = input.by_ref().take(513).read_until(b'\n', &mut line);
            if result.is_err()
                || line.is_empty()
                || line.len() > 512
                || !line.ends_with(b"\n")
                || line == b"stop\n"
            {
                break;
            }
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&line) else {
                break;
            };
            match value["operation"].as_str() {
                Some("preview-renew") => {
                    if let (Some(id), Some(epoch)) =
                        (value["previewId"].as_str(), value["configEpoch"].as_u64())
                    {
                        controls.renew(id, epoch, value["leaseMs"].as_u64().unwrap_or(3000));
                    } else {
                        break;
                    }
                }
                Some("preview-off") => controls.off(),
                Some("audio-levels") => {
                    let Ok(levels) = serde_json::from_value(value["value"].clone()) else {
                        break;
                    };
                    let _ = audio_input.submit(levels);
                }
                _ => break,
            }
        }
        controls.off();
        cancel.store(true, Ordering::Release);
    });
    let reporter = match host_protocol::Reporter::new() {
        Ok(r) => r,
        Err(message) => {
            send(serde_json::json!({"type":"error","message":message}));
            return;
        }
    };
    let mut no_media = false;
    let result: Result<serde_json::Value, String> = (|| match request["operation"]
        .as_str()
        .unwrap_or("")
    {
        "sources" => {
            if !windows::supports_capture() {
                return Err("Screen recording requires Windows Graphics Capture support.".into());
            }
            serde_json::to_value(windows::enumerate_displays()?).map_err(|e| e.to_string())
        }
        "audio-applications" => serde_json::to_value(windows::audio_applications(
            request["excludedPid"].as_u64().unwrap_or(0) as u32,
        )?)
        .map_err(|e| e.to_string()),
        "audio-capabilities" => {
            let reason = windows::audio_capability()?;
            Ok(serde_json::json!({"application":{"available":reason.is_none(),"reason":reason}}))
        }
        "windows" => serde_json::to_value(windows::enumerate_windows(
            request["excludedPid"].as_u64().unwrap_or(0) as u32,
        )?)
        .map_err(|e| e.to_string()),
        "resolve-source" => {
            let source = serde_json::from_value(request["source"].clone())
                .map_err(|_| "Invalid capture source.")?;
            serde_json::to_value(windows::resolve_source(source)?).map_err(|e| e.to_string())
        }
        "preview" => {
            let source = serde_json::from_value(request["source"].clone())
                .map_err(|_| "Invalid preview source.")?;
            let id = request["previewId"]
                .as_str()
                .ok_or("Invalid preview identity.")?;
            preview.renew(id, 1, 3000);
            windows::run_preview(
                source,
                request["showCursor"].as_bool().unwrap_or(true),
                stop,
                preview,
                |value| reporter.progress(value),
            )
        }
        "audio-devices" => {
            serde_json::to_value(windows::audio_devices()?).map_err(|e| e.to_string())
        }
        "select-region" => {
            let id = request["displayId"]
                .as_str()
                .ok_or("Choose a display first.")?;
            serde_json::to_value(windows::select_region(id, stop)?).map_err(|e| e.to_string())
        }
        "record" => {
            let config = serde_json::from_value(request["config"].clone())
                .map_err(|e| format!("Invalid recording settings: {e}"))?;
            let progress_reporter = reporter.clone();
            let outcome = windows::run(
                config,
                stop,
                move |progress| progress_reporter.progress(serde_json::to_value(progress).unwrap()),
                preview,
                audio.clone(),
            )
            .map_err(|failure| {
                no_media = failure.no_media;
                failure.message
            })?;
            serde_json::to_value(outcome).map_err(|e| e.to_string())
        }
        _ => Err("Unsupported recording operation.".into()),
    })();
    match result {
        Ok(value) => reporter.finish(serde_json::json!({"type":"result","value":value})),
        Err(message) => {
            reporter.finish(serde_json::json!({"type":"error","message":message,"noMedia":no_media,"audio":audio.snapshot()}))
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("The recording host is currently available on Windows only.");
    std::process::exit(1);
}
