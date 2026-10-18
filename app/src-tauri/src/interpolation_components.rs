//! Native RIFE uses app-shipped code; optional backends use pinned reviewed components.
//! Admission between installation, removal and exports belongs to lib.rs.
use crate::optional_interpolation_components;
use crate::process::ManagedChild;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

const CATALOG: &str = include_str!("../../interpolation-models.json");
const OWNERSHIP_FILE: &str = "installed-model.json";
const SELF_TEST_TIMEOUT: Duration = Duration::from_secs(120);
const OUTPUT_LIMIT: usize = 16 * 1024;
const DOWNLOAD_FILE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Catalog {
    catalog_version: String,
    source_commit: String,
    default_model_id: String,
    models: Vec<Model>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Model {
    id: String,
    label: String,
    files: Vec<ModelFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelFile {
    name: String,
    url: String,
    bytes: u64,
    sha256: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InterpolationStatus {
    pub state: String,
    pub model_id: String,
    pub message: String,
    pub download_bytes: u64,
    pub installed_bytes: u64,
    pub device_name: Option<String>,
    pub bundled: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct InstallUpdate {
    pub state: String,
    pub completed_bytes: u64,
    pub total_bytes: u64,
    pub message: String,
}

#[derive(Debug, Clone)]
pub(crate) enum ModelExecution {
    Native,
    TorchPair {
        python_path: PathBuf,
        script_path: PathBuf,
        source_dir: PathBuf,
        cache_dir: PathBuf,
    },
    TorchSequence {
        python_path: PathBuf,
        script_path: PathBuf,
        source_dir: PathBuf,
        cache_dir: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedModel {
    pub execution: ModelExecution,
    pub worker_path: PathBuf,
    pub model_dir: PathBuf,
    pub model_id: String,
    pub model_digest: String,
    pub worker_digest: String,
    pub(super) _lease: Option<Arc<Vec<File>>>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Ownership {
    catalog_version: String,
    model_id: String,
    model_digest: String,
}

pub(super) struct Locations {
    pub(super) downloaded: PathBuf,
    pub(super) bundles: Vec<PathBuf>,
}

fn catalog() -> Result<Catalog, String> {
    let catalog: Catalog = serde_json::from_str(CATALOG)
        .map_err(|_| "The embedded interpolation catalog is invalid.".to_string())?;
    if !safe_name(&catalog.catalog_version)
        || catalog.source_commit.len() != 40
        || !catalog
            .models
            .iter()
            .any(|m| m.id == catalog.default_model_id)
    {
        return Err("The embedded interpolation catalog has invalid identity.".into());
    }
    for model in &catalog.models {
        if !safe_name(&model.id)
            || model.label.is_empty()
            || model.files.len() != 2
            || model.files[0].name != "flownet.bin"
            || model.files[1].name != "flownet.param"
        {
            return Err("The embedded interpolation catalog has invalid model files.".into());
        }
        for file in &model.files {
            let expected = format!(
                "https://raw.githubusercontent.com/TNTwise/rife-ncnn-vulkan/{}/models/{}/{}",
                catalog.source_commit, model.id, file.name
            );
            if file.url != expected
                || file.bytes == 0
                || file.bytes > 64 * 1024 * 1024
                || file.sha256.len() != 64
                || !file
                    .sha256
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            {
                return Err("The embedded interpolation catalog has invalid pinned data.".into());
            }
        }
    }
    Ok(catalog)
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'))
        && name != "."
        && name != ".."
}

fn selected_model<'a>(catalog: &'a Catalog, id: &str) -> Result<&'a Model, String> {
    catalog
        .models
        .iter()
        .find(|m| m.id == id)
        .ok_or_else(|| "Choose a supported interpolation model.".to_string())
}

fn total_bytes(model: &Model) -> u64 {
    model.files.iter().map(|f| f.bytes).sum()
}

fn model_digest(model: &Model) -> String {
    let mut hasher = Sha256::new();
    for file in &model.files {
        hasher.update(file.name.as_bytes());
        hasher.update(file.bytes.to_le_bytes());
        hasher.update(file.sha256.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

pub(super) fn locations(app: &AppHandle) -> Result<Locations, String> {
    let downloaded = app
        .path()
        .app_local_data_dir()
        .map_err(|_| "The local interpolation data folder is unavailable.".to_string())?
        .join("interpolation")
        .join("models");
    let mut bundles = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        bundles.push(parent.join("interpolation-sidecar"));
    }
    if let Ok(resource) = app.path().resource_dir() {
        bundles.push(resource.join("interpolation-sidecar"));
    }
    if cfg!(debug_assertions) {
        bundles.push(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../.interpolation-bundle/current/interpolation-sidecar"),
        );
    }
    Ok(Locations {
        downloaded,
        bundles,
    })
}

pub(super) fn file_digest(path: &Path) -> Result<String, String> {
    let mut input =
        File::open(path).map_err(|_| "Cannot read the interpolation engine.".to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = input
            .read(&mut buffer)
            .map_err(|_| "Cannot verify the interpolation engine.".to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(super) fn worker_path(locations: &Locations) -> Result<PathBuf, String> {
    if cfg!(debug_assertions)
        && let Some(path) = std::env::var_os("VFL_INTERPOLATION_WORKER_PATH")
    {
        let path = PathBuf::from(path);
        return regular_file(&path)
            .map(|_| path)
            .map_err(|_| "The developer interpolation worker is unavailable.".into());
    }
    let name = if cfg!(windows) {
        "vfl-rife.exe"
    } else {
        "vfl-rife"
    };
    locations
        .bundles
        .iter()
        .map(|b| b.join(name))
        .find(|p| regular_file(p).is_ok())
        .ok_or_else(|| {
            "The interpolation engine is missing. Reinstall the complete app folder.".into()
        })
}

fn regular_file(path: &Path) -> Result<fs::Metadata, String> {
    let meta =
        fs::symlink_metadata(path).map_err(|_| "A model component is missing.".to_string())?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err("Interpolation components must be regular files.".into());
    }
    Ok(meta)
}

pub(super) fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("Canceled.".into())
    } else {
        Ok(())
    }
}

fn verify_model(dir: &Path, model: &Model, cancel: &AtomicBool) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(dir).map_err(|_| "The interpolation model is missing.".to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("The interpolation model folder is invalid.".into());
    }
    for file in &model.files {
        check_cancel(cancel)?;
        let path = dir.join(&file.name);
        if regular_file(&path)?.len() != file.bytes {
            return Err("Interpolation model verification failed. Install the model again.".into());
        }
        let mut input =
            File::open(&path).map_err(|_| "Cannot read the interpolation model.".to_string())?;
        let mut hash = Sha256::new();
        let mut count = 0;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            check_cancel(cancel)?;
            let n = input
                .read(&mut buffer)
                .map_err(|_| "Cannot verify the interpolation model.".to_string())?;
            if n == 0 {
                break;
            }
            count += n as u64;
            if count > file.bytes {
                return Err("Interpolation model size changed during verification.".into());
            }
            hash.update(&buffer[..n]);
        }
        if count != file.bytes || format!("{:x}", hash.finalize()) != file.sha256 {
            return Err("Interpolation model verification failed. Install the model again.".into());
        }
    }
    Ok(())
}

fn owned_path(locations: &Locations, catalog: &Catalog, model: &Model) -> PathBuf {
    locations
        .downloaded
        .join(&model.id)
        .join(&catalog.catalog_version)
}

fn resolve_at(
    locations: &Locations,
    catalog: &Catalog,
    model: &Model,
) -> Result<(ResolvedModel, bool), String> {
    let (dir, bundled) = locate_model(locations, catalog, model)?;
    let worker = worker_path(locations)?;
    Ok((
        ResolvedModel {
            execution: ModelExecution::Native,
            worker_digest: file_digest(&worker)?,
            worker_path: worker,
            model_dir: dir,
            model_id: model.id.clone(),
            model_digest: model_digest(model),
            _lease: None,
        },
        bundled,
    ))
}

fn locate_model(
    locations: &Locations,
    catalog: &Catalog,
    model: &Model,
) -> Result<(PathBuf, bool), String> {
    let cancel = AtomicBool::new(false);
    let downloaded = owned_path(locations, catalog, model);
    let candidates = std::iter::once((downloaded, false)).chain(
        locations
            .bundles
            .iter()
            .map(|b| (b.join("models").join(&model.id), true)),
    );
    let mut corrupt = false;
    for (dir, bundled) in candidates {
        if !dir.exists() {
            continue;
        }
        if !bundled
            && fs::symlink_metadata(dir.parent().unwrap())
                .is_ok_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_dir())
        {
            corrupt = true;
            continue;
        }
        match verify_model(&dir, model, &cancel) {
            Ok(()) => return Ok((dir, bundled)),
            Err(_) => corrupt = true,
        }
    }
    let incomplete = abandoned_stages(
        owned_path(locations, catalog, model).parent().unwrap(),
        catalog,
        model,
    )
    .is_ok_and(|stages| !stages.is_empty());
    Err(if corrupt {
        "Interpolation model verification failed. Install the model again."
    } else if incomplete {
        "An earlier model installation did not finish. Retry installation."
    } else {
        "Install the interpolation model to use smooth motion."
    }
    .into())
}

pub(crate) fn resolve_model(app: &AppHandle, model_id: &str) -> Result<ResolvedModel, String> {
    if optional_interpolation_components::supports(model_id) {
        return optional_interpolation_components::resolve_model(app, model_id);
    }
    let catalog = catalog()?;
    let model = selected_model(&catalog, model_id)?;
    let locations = locations(app)?;
    let lease = model_lock(&locations, false)?;
    resolve_at(&locations, &catalog, model).map(|(mut resolved, _)| {
        resolved._lease = Some(Arc::new(vec![lease]));
        resolved
    })
}

fn make_status(
    model: &Model,
    state: &str,
    message: String,
    bundled: bool,
    device: Option<String>,
) -> InterpolationStatus {
    InterpolationStatus {
        state: state.into(),
        model_id: model.id.clone(),
        message,
        download_bytes: total_bytes(model),
        installed_bytes: if state == "ready" {
            total_bytes(model)
        } else {
            0
        },
        device_name: device,
        bundled,
    }
}

pub(crate) fn status(app: &AppHandle, model_id: &str) -> Result<InterpolationStatus, String> {
    if optional_interpolation_components::supports(model_id) {
        return optional_interpolation_components::status(app, model_id);
    }
    let catalog = catalog()?;
    let model = selected_model(&catalog, model_id)?;
    let locations = locations(app)?;
    let _lease = model_lock(&locations, false)?;
    if let Err(error) = worker_path(&locations) {
        let present = locate_model(&locations, &catalog, model).ok();
        let mut status = make_status(
            model,
            "unavailable",
            error,
            present.as_ref().is_some_and(|(_, bundled)| *bundled),
            None,
        );
        status.installed_bytes = if present.is_some() {
            total_bytes(model)
        } else {
            owned_installed_bytes(&locations, &catalog, model)
        };
        return Ok(status);
    }
    let (resolved, bundled) = match resolve_at(&locations, &catalog, model) {
        Ok(value) => value,
        Err(message) => {
            let mut status = make_status(
                model,
                if message.contains("verification failed") || message.contains("did not finish") {
                    "failed"
                } else {
                    "missing"
                },
                message,
                false,
                None,
            );
            status.installed_bytes = owned_installed_bytes(&locations, &catalog, model);
            return Ok(status);
        }
    };
    match self_test(&resolved, &AtomicBool::new(false)) {
        Ok(device) => Ok(make_status(
            model,
            "ready",
            "Ready for smooth motion.".into(),
            bundled,
            Some(device),
        )),
        Err(error) => {
            let mut status = make_status(model, "unavailable", error, bundled, None);
            status.installed_bytes = total_bytes(model);
            Ok(status)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestResult {
    protocol: u32,
    ok: bool,
    device_name: String,
}
static TEST_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

pub(crate) fn gpu_choice() -> String {
    if cfg!(debug_assertions)
        && let Ok(value) = std::env::var("VFL_INTERPOLATION_GPU")
        && (value == "-1" || value.parse::<u32>().is_ok())
    {
        return value;
    }
    "auto".into()
}

#[cfg(test)]
pub(crate) fn resolve_test_model(model_id: &str) -> Result<ResolvedModel, String> {
    let catalog = catalog()?;
    let model = selected_model(&catalog, model_id)?;
    let bundle = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../.interpolation-bundle/current/interpolation-sidecar");
    let locations = Locations {
        downloaded: bundle.join("not-installed-test-models"),
        bundles: vec![bundle],
    };
    resolve_at(&locations, &catalog, model).map(|(resolved, _)| resolved)
}

fn self_test(resolved: &ResolvedModel, cancel: &AtomicBool) -> Result<String, String> {
    check_cancel(cancel)?;
    // Hash the executable as well as validating model bytes before accepting a cached test.
    let gpu = gpu_choice();
    let key = format!(
        "{}|{}|{}|{}|{}",
        resolved.worker_path.display(),
        resolved.model_dir.display(),
        resolved.model_digest,
        resolved.worker_digest,
        gpu
    );
    let cache = TEST_CACHE.get_or_init(Mutex::default);
    if let Some(device) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned()
    {
        return Ok(device);
    }
    let mut command = Command::new(&resolved.worker_path);
    command
        .args(["--self-test", "--model"])
        .arg(&resolved.model_dir)
        .args(["--gpu", &gpu]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = ManagedChild::spawn(command).map_err(|_| {
        "Cannot start the interpolation engine. Reinstall the complete app folder.".to_string()
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Interpolation engine output is unavailable.")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Interpolation engine diagnostics are unavailable.")?;
    let out = std::thread::spawn(move || read_bounded(stdout));
    let err = std::thread::spawn(move || read_bounded(stderr));
    let started = Instant::now();
    let outcome = loop {
        if let Err(error) = check_cancel(cancel) {
            break Err(error);
        }
        if started.elapsed() >= SELF_TEST_TIMEOUT {
            break Err(
                "The interpolation engine self-test timed out. Check your Vulkan graphics driver."
                    .into(),
            );
        }
        match child.wait_timeout(Duration::from_millis(50)) {
            Ok(Some(status)) => {
                break if status.success() {
                    Ok(())
                } else {
                    Err("The interpolation engine self-test failed. Check your Vulkan graphics driver or reinstall the app.".into())
                };
            }
            Ok(None) => (),
            Err(_) => break Err("The interpolation engine self-test could not complete.".into()),
        }
    };
    // Drop kills the whole registered child tree and joins its waiter before pipe readers.
    drop(child);
    let output = out
        .join()
        .map_err(|_| "Interpolation engine output reader failed.")?;
    let diagnostic = err
        .join()
        .map_err(|_| "Interpolation engine diagnostic reader failed.")?;
    outcome?;
    let output = output?;
    diagnostic?;
    check_cancel(cancel)?;
    let result: TestResult = serde_json::from_slice(&output).map_err(|_| {
        "The interpolation engine returned an invalid self-test result.".to_string()
    })?;
    if result.protocol != 1
        || !result.ok
        || result.device_name.trim().is_empty()
        || result.device_name.len() > 256
    {
        return Err("The interpolation engine returned an incompatible self-test result.".into());
    }
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= 32 {
        cache.clear();
    }
    cache.insert(key, result.device_name.clone());
    Ok(result.device_name)
}

pub(super) fn read_bounded(mut input: impl Read) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    let mut buffer = [0u8; 4096];
    let mut overflow = false;
    loop {
        let n = input
            .read(&mut buffer)
            .map_err(|_| "Cannot read interpolation engine output.".to_string())?;
        if n == 0 {
            break;
        }
        if output.len() + n <= OUTPUT_LIMIT {
            output.extend_from_slice(&buffer[..n]);
        } else {
            overflow = true;
        }
    }
    if overflow {
        Err("The interpolation engine returned excessive output.".into())
    } else {
        Ok(output)
    }
}

// Reject redirected ownership roots instead of recursively deleting through a user-created link.
fn ensure_owned_directory(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        ensure_owned_directory(parent)?;
    }
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("The interpolation data folder is not an owned directory.".into());
        }
        return Ok(());
    }
    fs::create_dir(path).map_err(|_| "Cannot create the interpolation data folder.".to_string())
}

// OS advisory locks survive neither crashes nor restarts. ResolvedModel retains a
// shared lease until the export drops it; mutations need an exclusive lease.
fn model_lock(locations: &Locations, exclusive: bool) -> Result<File, String> {
    ensure_owned_directory(&locations.downloaded)?;
    let path = locations.downloaded.join(".models.lock");
    if fs::symlink_metadata(&path).is_ok() {
        regular_file(&path)?;
    }
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|_| "Cannot open interpolation model ownership lock.".to_string())?;
    let result = if exclusive {
        file.try_lock()
    } else {
        file.try_lock_shared()
    };
    result.map_err(|_| {
        "Interpolation components are in use by another app window. Try again when it finishes."
            .to_string()
    })?;
    Ok(file)
}

fn update(
    progress: &mut impl FnMut(InstallUpdate),
    model: &Model,
    state: &'static str,
    completed: u64,
    message: &str,
) {
    progress(InstallUpdate {
        state: state.into(),
        completed_bytes: completed,
        total_bytes: total_bytes(model),
        message: message.into(),
    });
}

fn download_to(
    mut input: impl Read,
    path: &Path,
    file: &ModelFile,
    cancel: &AtomicBool,
    mut report: impl FnMut(u64),
) -> Result<(), String> {
    let started = Instant::now();
    let mut output = File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| "Cannot create the staged interpolation model.".to_string())?;
    let mut hash = Sha256::new();
    let mut count = 0;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check_cancel(cancel)?;
        if started.elapsed() >= DOWNLOAD_FILE_TIMEOUT {
            return Err("Model download timed out. Check your connection and retry.".into());
        }
        let n = input
            .read(&mut buffer)
            .map_err(|_| "Model download interrupted. Retry the installation.".to_string())?;
        if n == 0 {
            break;
        }
        count += n as u64;
        if count > file.bytes {
            return Err("Model download exceeded its reviewed size.".into());
        }
        output.write_all(&buffer[..n]).map_err(|_| {
            "Cannot save the interpolation model. Check free disk space.".to_string()
        })?;
        hash.update(&buffer[..n]);
        report(count);
    }
    check_cancel(cancel)?;
    if count != file.bytes {
        return Err("Model download was incomplete. Retry the installation.".into());
    }
    if format!("{:x}", hash.finalize()) != file.sha256 {
        return Err("Model download failed SHA256 verification. Retry the installation.".into());
    }
    output
        .sync_all()
        .map_err(|_| "Cannot finish saving the interpolation model.".to_string())
}

fn download_client_policy() -> reqwest::blocking::ClientBuilder {
    Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .connect_timeout(Duration::from_secs(10))
        // In reqwest 0.12's blocking API this bounds each send/read operation,
        // and Response::read starts a fresh timeout for every body read. It is
        // not the async/request-builder total-transfer timeout. Keep streaming
        // through Read rather than Response::bytes(); download_to owns
        // the separate ten-minute file budget and checks Cancel every chunk.
        .timeout(Duration::from_secs(10))
}

fn download_client() -> Result<Client, String> {
    download_client_policy()
        .build()
        .map_err(|_| "Cannot initialize model downloads.".to_string())
}

fn fetch_model(client: &Client, file: &ModelFile) -> Result<Box<dyn Read>, String> {
    let response = client
        .get(&file.url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|_| {
            "Cannot download the interpolation model. Check your connection and retry.".to_string()
        })?;
    if response.content_length().is_some_and(|n| n != file.bytes) {
        return Err("Model download has an unexpected size.".into());
    }
    Ok(Box::new(response))
}

pub(crate) fn install(
    app: &AppHandle,
    model_id: &str,
    cancel: &AtomicBool,
    mut progress: impl FnMut(InstallUpdate),
) -> Result<InterpolationStatus, String> {
    if optional_interpolation_components::supports(model_id) {
        return optional_interpolation_components::install(app, model_id, cancel, progress);
    }
    let catalog = catalog()?;
    let model = selected_model(&catalog, model_id)?;
    let locations = locations(app)?;
    worker_path(&locations)?;
    check_cancel(cancel)?;
    // Both transport and command destination are fixed by the embedded catalog.
    let client = download_client()?;
    let result = install_at(
        &locations,
        &catalog,
        model,
        cancel,
        &mut progress,
        |file| fetch_model(&client, file),
        self_test,
    );
    if let Err(error) = &result {
        update(
            &mut progress,
            model,
            if cancel.load(Ordering::Relaxed) {
                "canceled"
            } else {
                "failed"
            },
            0,
            error,
        );
    }
    result
}

fn install_at(
    locations: &Locations,
    catalog: &Catalog,
    model: &Model,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(InstallUpdate),
    mut fetch: impl FnMut(&ModelFile) -> Result<Box<dyn Read>, String>,
    mut test: impl FnMut(&ResolvedModel, &AtomicBool) -> Result<String, String>,
) -> Result<InterpolationStatus, String> {
    check_cancel(cancel)?;
    let _lease = model_lock(locations, true)?;
    let worker = worker_path(locations)?;
    let target = owned_path(locations, catalog, model);
    let parent = target
        .parent()
        .ok_or("The interpolation data folder is invalid.")?;
    ensure_owned_directory(parent)?;
    // Every installer holds the exclusive model-store lease. These marked
    // pre-existing stages therefore cannot belong to a live transaction.
    for stage in abandoned_stages(parent, catalog, model)? {
        check_cancel(cancel)?;
        fs::remove_dir_all(stage).map_err(|_| {
            "Cannot clear an interrupted model download. Close other app windows and retry."
                .to_string()
        })?;
    }
    // A valid existing owned model can be re-tested without any network or mutation.
    if verify_model(&target, model, cancel).is_ok() {
        update(
            progress,
            model,
            "testing",
            total_bytes(model),
            "Testing the interpolation model.",
        );
        let resolved = ResolvedModel {
            execution: ModelExecution::Native,
            worker_path: worker.clone(),
            model_dir: target,
            model_id: model.id.clone(),
            model_digest: model_digest(model),
            worker_digest: file_digest(&worker)?,
            _lease: None,
        };
        let device = test(&resolved, cancel)?;
        check_cancel(cancel)?;
        update(
            progress,
            model,
            "ready",
            total_bytes(model),
            "Ready for smooth motion.",
        );
        return Ok(make_status(
            model,
            "ready",
            "Ready for smooth motion.".into(),
            false,
            Some(device),
        ));
    }
    check_cancel(cancel)?;
    let staging = tempfile::Builder::new()
        .prefix(".install-")
        .tempdir_in(parent)
        .map_err(|_| "Cannot stage the interpolation model. Check free disk space.".to_string())?;
    // Ownership is durable before downloading the first byte, so a crash can
    // be detected and safely retried without trusting a directory prefix.
    write_ownership(staging.path(), catalog, model)?;
    let mut completed = 0;
    for file in &model.files {
        update(
            progress,
            model,
            "downloading",
            completed,
            "Downloading the interpolation model.",
        );
        check_cancel(cancel)?;
        let input = fetch(file)?;
        let mut last_progress = Instant::now();
        download_to(input, &staging.path().join(&file.name), file, cancel, |n| {
            if n == file.bytes || last_progress.elapsed() >= Duration::from_millis(200) {
                update(
                    progress,
                    model,
                    "downloading",
                    completed + n,
                    "Downloading the interpolation model.",
                );
                last_progress = Instant::now();
            }
        })?;
        completed += file.bytes;
    }
    update(
        progress,
        model,
        "verifying",
        completed,
        "Verifying the interpolation model.",
    );
    verify_model(staging.path(), model, cancel)?;
    update(
        progress,
        model,
        "testing",
        completed,
        "Testing the interpolation model.",
    );
    let resolved = ResolvedModel {
        execution: ModelExecution::Native,
        worker_path: worker.clone(),
        model_dir: staging.path().into(),
        model_id: model.id.clone(),
        model_digest: model_digest(model),
        worker_digest: file_digest(&worker)?,
        _lease: None,
    };
    let device = test(&resolved, cancel)?;
    check_cancel(cancel)?;
    check_cancel(cancel)?;
    activate(staging.path(), &target, model, catalog)?;
    update(
        progress,
        model,
        "ready",
        completed,
        "Ready for smooth motion.",
    );
    Ok(make_status(
        model,
        "ready",
        "Ready for smooth motion.".into(),
        false,
        Some(device),
    ))
}

fn write_ownership(dir: &Path, catalog: &Catalog, model: &Model) -> Result<(), String> {
    let ownership = Ownership {
        catalog_version: catalog.catalog_version.clone(),
        model_id: model.id.clone(),
        model_digest: model_digest(model),
    };
    let mut marker = File::options()
        .create_new(true)
        .write(true)
        .open(dir.join(OWNERSHIP_FILE))
        .map_err(|_| "Cannot record interpolation model ownership.".to_string())?;
    serde_json::to_writer(&mut marker, &ownership)
        .map_err(|_| "Cannot record interpolation model ownership.".to_string())?;
    marker
        .sync_all()
        .map_err(|_| "Cannot save interpolation model ownership.".to_string())
}

fn abandoned_stages(
    parent: &Path,
    catalog: &Catalog,
    model: &Model,
) -> Result<Vec<PathBuf>, String> {
    if fs::symlink_metadata(parent)
        .is_ok_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_dir())
    {
        return Err("The interpolation staging folder is not an owned directory.".into());
    }
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("Cannot inspect interrupted interpolation downloads.".into()),
    };
    let mut stages = Vec::new();
    for (count, entry) in entries.enumerate() {
        if count >= 64 {
            return Err("The interpolation data folder has too many entries. Check its contents before retrying.".into());
        }
        let entry = entry.map_err(|_| "Cannot inspect interrupted interpolation downloads.")?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(".install-"))
            && validate_owned(&entry.path(), model, catalog).is_ok()
        {
            stages.push(entry.path());
        }
    }
    Ok(stages)
}

fn validate_owned(dir: &Path, model: &Model, catalog: &Catalog) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(dir).map_err(|_| "The downloaded model is missing.".to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Refusing to change a model folder not owned by the app.".into());
    }
    let marker = dir.join(OWNERSHIP_FILE);
    if regular_file(&marker)?.len() > 4096 {
        return Err("Model ownership is invalid.".into());
    }
    let ownership: Ownership = serde_json::from_reader(
        File::open(marker)
            .map_err(|_| "Model ownership is unavailable.")?
            .take(4097),
    )
    .map_err(|_| "Model ownership is invalid.".to_string())?;
    if ownership.model_id != model.id
        || ownership.catalog_version != catalog.catalog_version
        || ownership.model_digest != model_digest(model)
    {
        return Err("Refusing to change a model folder not owned by the app.".into());
    }
    for entry in fs::read_dir(dir).map_err(|_| "Cannot inspect the downloaded model folder.")? {
        let entry = entry.map_err(|_| "Cannot inspect the downloaded model folder.")?;
        let name = entry.file_name();
        if name != OWNERSHIP_FILE && !model.files.iter().any(|file| name == file.name.as_str()) {
            return Err(
                "The model folder contains other files. Move them before removing or repairing it."
                    .into(),
            );
        }
        regular_file(&entry.path())?;
    }
    Ok(())
}

fn owned_installed_bytes(locations: &Locations, catalog: &Catalog, model: &Model) -> u64 {
    let path = owned_path(locations, catalog, model);
    if validate_owned(&path, model, catalog).is_err() {
        return 0;
    }
    model
        .files
        .iter()
        .filter_map(|file| regular_file(&path.join(&file.name)).ok())
        .fold(0u64, |total, metadata| total.saturating_add(metadata.len()))
}

fn activate(staging: &Path, target: &Path, model: &Model, catalog: &Catalog) -> Result<(), String> {
    if fs::symlink_metadata(target).is_err() {
        return fs::rename(staging, target)
            .map_err(|_| "Cannot activate the interpolation model.".into());
    }
    validate_owned(target, model, catalog)?;
    // Preserve the previous owned version until the new directory has activated.
    let backup = tempfile::Builder::new()
        .prefix(".replaced-")
        .tempdir_in(target.parent().unwrap())
        .map_err(|_| "Cannot preserve the previous model during repair.".to_string())?;
    let previous = backup.path().join("previous");
    fs::rename(target, &previous)
        .map_err(|_| "Cannot preserve the previous model during repair.".to_string())?;
    if fs::rename(staging, target).is_err() {
        if fs::rename(&previous, target).is_err() {
            // Keep recovery bytes if the filesystem also refuses rollback.
            let _ = backup.keep();
            return Err("Model activation failed. The previous model is preserved in the data folder for recovery.".into());
        }
        return Err(
            "Cannot activate the interpolation model. The previous model was preserved.".into(),
        );
    }
    Ok(())
}

pub(crate) fn remove(app: &AppHandle, model_id: &str) -> Result<InterpolationStatus, String> {
    if optional_interpolation_components::supports(model_id) {
        return optional_interpolation_components::remove(app, model_id);
    }
    let catalog = catalog()?;
    let model = selected_model(&catalog, model_id)?;
    let locations = locations(app)?;
    let lease = model_lock(&locations, true)?;
    remove_owned(&locations, &catalog, model)?;
    drop(lease);
    status(app, model_id)
}

fn remove_owned(locations: &Locations, catalog: &Catalog, model: &Model) -> Result<(), String> {
    let target = owned_path(locations, catalog, model);
    if target
        .try_exists()
        .map_err(|_| "Cannot inspect downloaded interpolation models.")?
    {
        // Validate ancestors as well: a redirected parent is never ours to remove.
        ensure_owned_directory(target.parent().unwrap())?;
        validate_owned(&target, model, catalog)?;
        fs::remove_dir_all(&target)
            .map_err(|_| "Cannot remove the downloaded interpolation model.".to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use std::net::TcpListener;

    fn fixture() -> (tempfile::TempDir, Locations, Catalog) {
        let temp = tempfile::tempdir().unwrap();
        let model = Model {
            id: "rife-test".into(),
            label: "Test model".into(),
            files: vec![
                ModelFile {
                    name: "flownet.bin".into(),
                    url: "unused".into(),
                    bytes: 3,
                    sha256: format!("{:x}", Sha256::digest(b"bin")),
                },
                ModelFile {
                    name: "flownet.param".into(),
                    url: "unused".into(),
                    bytes: 5,
                    sha256: format!("{:x}", Sha256::digest(b"param")),
                },
            ],
        };
        let locations = Locations {
            downloaded: temp.path().join("downloaded"),
            bundles: vec![temp.path().join("bundle")],
        };
        fs::create_dir_all(&locations.bundles[0]).unwrap();
        fs::write(
            locations.bundles[0].join(if cfg!(windows) {
                "vfl-rife.exe"
            } else {
                "vfl-rife"
            }),
            b"app-owned-test-fixture",
        )
        .unwrap();
        let catalog = Catalog {
            catalog_version: "test-v1".into(),
            source_commit: "0".repeat(40),
            default_model_id: model.id.clone(),
            models: vec![model],
        };
        (temp, locations, catalog)
    }

    fn put_model(path: &Path, catalog: &Catalog, owned: bool) {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("flownet.bin"), b"bin").unwrap();
        fs::write(path.join("flownet.param"), b"param").unwrap();
        if owned {
            fs::write(
                path.join(OWNERSHIP_FILE),
                serde_json::to_vec(&Ownership {
                    catalog_version: catalog.catalog_version.clone(),
                    model_id: catalog.models[0].id.clone(),
                    model_digest: model_digest(&catalog.models[0]),
                })
                .unwrap(),
            )
            .unwrap();
        }
    }

    fn input(file: &ModelFile) -> Result<Box<dyn Read>, String> {
        Ok(Box::new(Cursor::new(if file.name == "flownet.bin" {
            b"bin".to_vec()
        } else {
            b"param".to_vec()
        })))
    }

    #[test]
    fn embedded_catalog_is_immutable_and_bounded() {
        let catalog = catalog().unwrap();
        assert_eq!(catalog.default_model_id, "rife-v4.25");
        assert_eq!(catalog.models.len(), 2);
        assert_eq!(
            total_bytes(selected_model(&catalog, "rife-v4.25").unwrap()),
            11_391_973
        );
        assert_eq!(
            total_bytes(selected_model(&catalog, "rife-v4.26").unwrap()),
            11_428_569
        );
        for id in ["../model", "", "rife-v4.27", "https://example.com/model"] {
            assert!(selected_model(&catalog, id).is_err());
        }
    }

    #[test]
    fn exact_size_and_hash_are_required_before_activation() {
        let (_temp, locations, catalog) = fixture();
        let model = &catalog.models[0];
        let file = &model.files[0];
        for (index, bytes, expected) in [
            (0, b"bi".as_slice(), "incomplete"),
            (1, b"bins".as_slice(), "exceeded"),
            (2, b"bad".as_slice(), "SHA256"),
        ] {
            let path = locations.bundles[0].join(format!("download-{index}"));
            let error = download_to(
                Cursor::new(bytes),
                &path,
                file,
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap_err();
            assert!(error.contains(expected), "{error}");
            assert!(fs::metadata(path).unwrap().len() <= file.bytes);
        }
    }

    #[test]
    fn cancel_while_streaming_removes_only_created_staging() {
        let (_temp, locations, catalog) = fixture();
        let model = &catalog.models[0];
        let cancel = AtomicBool::new(false);
        let sentinel = locations.bundles[0].join("user-data");
        fs::write(&sentinel, "keep").unwrap();
        let mut updates = Vec::new();
        let error = install_at(
            &locations,
            &catalog,
            model,
            &cancel,
            &mut |u| {
                if u.completed_bytes > 0 {
                    cancel.store(true, Ordering::Relaxed);
                }
                updates.push(u.state);
            },
            input,
            |_, _| panic!("canceled model must not self-test"),
        )
        .unwrap_err();
        assert_eq!(error, "Canceled.");
        assert!(!owned_path(&locations, &catalog, model).exists());
        assert_eq!(fs::read_to_string(sentinel).unwrap(), "keep");
        assert_eq!(
            fs::read_dir(locations.downloaded.join(&model.id))
                .unwrap()
                .count(),
            0
        );
        assert!(updates.iter().all(|state| state != "ready"));
    }

    #[test]
    fn cancel_between_network_chunks_stops_before_reading_more() {
        struct CancelingReader<'a>(&'a AtomicBool);
        impl Read for CancelingReader<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                assert!(
                    !self.0.swap(true, Ordering::Relaxed),
                    "must not read after cancellation"
                );
                bytes[0] = b'b';
                Ok(1)
            }
        }
        let (_temp, locations, catalog) = fixture();
        let file = &catalog.models[0].files[0];
        let cancel = AtomicBool::new(false);
        let path = locations.bundles[0].join("partial-download");
        assert_eq!(
            download_to(CancelingReader(&cancel), &path, file, &cancel, |_| {}).unwrap_err(),
            "Canceled."
        );
        assert_eq!(fs::read(path).unwrap(), b"b");
    }

    #[test]
    fn failed_staged_self_test_preserves_previous_bytes() {
        let (_temp, locations, catalog) = fixture();
        let model = &catalog.models[0];
        let target = owned_path(&locations, &catalog, model);
        put_model(&target, &catalog, true);
        fs::write(target.join("flownet.bin"), b"old").unwrap();
        let error = install_at(
            &locations,
            &catalog,
            model,
            &AtomicBool::new(false),
            &mut |_| {},
            input,
            |resolved, _| {
                assert_ne!(resolved.model_dir, target);
                assert_eq!(
                    fs::read(resolved.model_dir.join("flownet.bin")).unwrap(),
                    b"bin"
                );
                Err("self-test rejected".into())
            },
        )
        .unwrap_err();
        assert_eq!(error, "self-test rejected");
        assert_eq!(fs::read(target.join("flownet.bin")).unwrap(), b"old");
        assert_eq!(fs::read_dir(target.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn restart_detects_owned_partial_stage_and_retry_cleans_only_verified_inventory() {
        let (_temp, locations, catalog) = fixture();
        let model = &catalog.models[0];
        let parent = locations.downloaded.join(&model.id);
        ensure_owned_directory(&parent).unwrap();
        let abandoned = parent.join(".install-crashed");
        fs::create_dir(&abandoned).unwrap();
        write_ownership(&abandoned, &catalog, model).unwrap();
        fs::write(abandoned.join("flownet.bin"), b"partial").unwrap();
        assert!(
            resolve_at(&locations, &catalog, model)
                .unwrap_err()
                .contains("did not finish")
        );
        let unmarked = parent.join(".install-user-folder");
        fs::create_dir(&unmarked).unwrap();
        fs::write(unmarked.join("personal.txt"), b"keep").unwrap();
        let ambiguous = parent.join(".install-extra-files");
        fs::create_dir(&ambiguous).unwrap();
        write_ownership(&ambiguous, &catalog, model).unwrap();
        fs::write(ambiguous.join("personal.txt"), b"keep").unwrap();
        install_at(
            &locations,
            &catalog,
            model,
            &AtomicBool::new(false),
            &mut |_| {},
            |file| {
                assert!(!abandoned.exists());
                assert!(unmarked.exists());
                assert!(ambiguous.exists());
                input(file)
            },
            |resolved, _| {
                // Durable ownership exists before download/inference, but does not
                // bypass the byte verification or actual self-test requirement.
                validate_owned(&resolved.model_dir, model, &catalog).unwrap();
                Ok("Test GPU".into())
            },
        )
        .unwrap();
        assert!(!abandoned.exists());
        assert_eq!(fs::read(unmarked.join("personal.txt")).unwrap(), b"keep");
        assert_eq!(fs::read(ambiguous.join("personal.txt")).unwrap(), b"keep");
    }

    #[test]
    fn restart_scan_is_bounded_and_cannot_delete_under_a_live_model_lease() {
        let (_temp, locations, catalog) = fixture();
        let model = &catalog.models[0];
        let parent = locations.downloaded.join(&model.id);
        ensure_owned_directory(&parent).unwrap();
        let abandoned = parent.join(".install-crashed");
        fs::create_dir(&abandoned).unwrap();
        write_ownership(&abandoned, &catalog, model).unwrap();
        let lease = model_lock(&locations, false).unwrap();
        assert!(
            install_at(
                &locations,
                &catalog,
                model,
                &AtomicBool::new(false),
                &mut |_| {},
                input,
                |_, _| panic!("live lease must prevent recovery")
            )
            .is_err()
        );
        assert!(abandoned.exists());
        drop(lease);
        for i in 0..64 {
            fs::create_dir(parent.join(format!("other-{i}"))).unwrap();
        }
        assert!(
            abandoned_stages(&parent, &catalog, model)
                .unwrap_err()
                .contains("too many entries")
        );
        assert!(abandoned.exists());
    }

    #[test]
    fn successful_install_tests_before_atomic_activation_and_reuses_good_model() {
        let (_temp, locations, catalog) = fixture();
        let model = &catalog.models[0];
        let target = owned_path(&locations, &catalog, model);
        let mut updates = Vec::new();
        let status = install_at(
            &locations,
            &catalog,
            model,
            &AtomicBool::new(false),
            &mut |u| updates.push(u.state),
            input,
            |resolved, _| {
                assert!(!target.exists());
                assert_eq!(resolved.model_id, model.id);
                assert_eq!(resolved.worker_digest.len(), 64);
                Ok("Test GPU".into())
            },
        )
        .unwrap();
        assert_eq!(status.state, "ready");
        assert_eq!(status.installed_bytes, 8);
        assert_eq!(updates.last().unwrap(), "ready");
        validate_owned(&target, model, &catalog).unwrap();
        verify_model(&target, model, &AtomicBool::new(false)).unwrap();
        install_at(
            &locations,
            &catalog,
            model,
            &AtomicBool::new(false),
            &mut |_| {},
            |_| panic!("good model must not download"),
            |_, _| Err("GPU unavailable".into()),
        )
        .unwrap_err();
        verify_model(&target, model, &AtomicBool::new(false)).unwrap();
    }

    #[test]
    fn resolver_verifies_bundled_and_owned_and_falls_back_to_valid_bundle() {
        let (_temp, locations, catalog) = fixture();
        let model = &catalog.models[0];
        let bundled = locations.bundles[0].join("models").join(&model.id);
        assert!(resolve_at(&locations, &catalog, model).is_err());
        put_model(&bundled, &catalog, false);
        assert!(resolve_at(&locations, &catalog, model).unwrap().1);
        let target = owned_path(&locations, &catalog, model);
        put_model(&target, &catalog, true);
        assert!(!resolve_at(&locations, &catalog, model).unwrap().1);
        fs::write(target.join("flownet.bin"), b"bad").unwrap();
        assert!(resolve_at(&locations, &catalog, model).unwrap().1);
        fs::write(bundled.join("flownet.param"), b"wrong").unwrap();
        assert!(
            resolve_at(&locations, &catalog, model)
                .unwrap_err()
                .contains("verification failed")
        );
    }

    #[test]
    fn ownership_rejects_unmarked_or_extra_user_files() {
        let (_temp, locations, catalog) = fixture();
        let model = &catalog.models[0];
        let target = owned_path(&locations, &catalog, model);
        put_model(&target, &catalog, false);
        assert!(validate_owned(&target, model, &catalog).is_err());
        put_model(&target, &catalog, true);
        validate_owned(&target, model, &catalog).unwrap();
        fs::write(target.join("personal.txt"), b"keep").unwrap();
        assert!(
            validate_owned(&target, model, &catalog)
                .unwrap_err()
                .contains("other files")
        );
    }

    #[test]
    fn remove_deletes_only_recognized_owned_version_and_keeps_bundled_model() {
        let (_temp, locations, catalog) = fixture();
        let model = &catalog.models[0];
        let bundled = locations.bundles[0].join("models").join(&model.id);
        put_model(&bundled, &catalog, false);
        let target = owned_path(&locations, &catalog, model);
        put_model(&target, &catalog, true);
        fs::write(target.join("flownet.bin"), b"corrupt").unwrap();
        assert_eq!(owned_installed_bytes(&locations, &catalog, model), 12);
        remove_owned(&locations, &catalog, model).unwrap();
        assert!(!target.exists());
        verify_model(&bundled, model, &AtomicBool::new(false)).unwrap();
        assert!(resolve_at(&locations, &catalog, model).unwrap().1);
        put_model(&target, &catalog, false);
        assert!(remove_owned(&locations, &catalog, model).is_err());
        assert!(target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn ownership_rejects_redirected_parent_and_model_files() {
        use std::os::unix::fs::symlink;
        let (temp, mut locations, catalog) = fixture();
        let real = temp.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = temp.path().join("link");
        symlink(&real, &link).unwrap();
        locations.downloaded = link.join("models");
        assert!(model_lock(&locations, true).is_err());
        assert!(!real.join("models").exists());
        let model = &catalog.models[0];
        put_model(&real, &catalog, true);
        fs::remove_file(real.join("flownet.bin")).unwrap();
        symlink(real.join("flownet.param"), real.join("flownet.bin")).unwrap();
        assert!(verify_model(&real, model, &AtomicBool::new(false)).is_err());
        assert!(validate_owned(&real, model, &catalog).is_err());
        locations.downloaded = temp.path().join("safe-data-root");
        ensure_owned_directory(&locations.downloaded).unwrap();
        let outside = temp.path().join("outside-model-parent");
        put_model(&outside.join(&catalog.catalog_version), &catalog, true);
        symlink(&outside, locations.downloaded.join(&model.id)).unwrap();
        assert!(resolve_at(&locations, &catalog, model).is_err());
        assert!(abandoned_stages(&locations.downloaded.join(&model.id), &catalog, model).is_err());
    }

    #[test]
    fn model_lease_blocks_mutation_until_job_drops_it() {
        let (_temp, locations, _catalog) = fixture();
        let lease = model_lock(&locations, false).unwrap();
        let second_reader = model_lock(&locations, false).unwrap();
        assert!(model_lock(&locations, true).is_err());
        drop(lease);
        drop(second_reader);
        let writer = model_lock(&locations, true).unwrap();
        assert!(model_lock(&locations, false).is_err());
        drop(writer);
        assert!(model_lock(&locations, false).is_ok());
    }

    #[test]
    fn http_failure_never_creates_an_activated_model() {
        // A loopback-only transport is injected solely inside this test. The
        // production path always uses the HTTPS-only pinned catalog client.
        let (_temp, locations, catalog) = fixture();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = socket.read(&mut request);
            socket.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let error = install_at(
            &locations,
            &catalog,
            &catalog.models[0],
            &AtomicBool::new(false),
            &mut |_| {},
            |file| {
                let client = Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(3))
                    .build()
                    .unwrap();
                let mut file = file.clone();
                file.url = format!("http://{address}/model");
                fetch_model(&client, &file)
            },
            |_, _| panic!("HTTP error must not self-test"),
        )
        .unwrap_err();
        server.join().unwrap();
        assert!(error.contains("Cannot download"));
        assert!(!owned_path(&locations, &catalog, &catalog.models[0]).exists());
        assert_eq!(
            fs::read_dir(locations.downloaded.join("rife-test"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn healthy_download_outlasts_each_individual_network_read_timeout() {
        let (_temp, locations, catalog) = fixture();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || -> std::io::Result<()> {
            let (mut socket, _) = listener.accept()?;
            socket.set_read_timeout(Some(Duration::from_secs(3)))?;
            socket.set_write_timeout(Some(Duration::from_secs(3)))?;
            let mut request = [0; 4096];
            if socket.read(&mut request)? == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Missing fixture request",
                ));
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\n")?;
            for byte in b"abcdef" {
                std::thread::sleep(Duration::from_millis(250));
                socket.write_all(&[*byte])?;
                socket.flush()?;
            }
            Ok(())
        });
        // Only this test lowers the I/O timeout and permits a loopback HTTP
        // fixture. All other policy comes from the production client builder.
        let io_timeout = Duration::from_secs(1);
        let client = download_client_policy()
            .https_only(false)
            .no_proxy()
            .timeout(io_timeout)
            .build()
            .unwrap();
        let mut file = catalog.models[0].files[0].clone();
        file.url = format!("http://{address}/model");
        file.bytes = 6;
        file.sha256 = format!("{:x}", Sha256::digest(b"abcdef"));
        let path = locations.bundles[0].join("slow-download");
        let started = Instant::now();
        let result = fetch_model(&client, &file)
            .and_then(|body| download_to(body, &path, &file, &AtomicBool::new(false), |_| {}));
        let served = server.join().unwrap();
        assert!(
            result.is_ok(),
            "Healthy chunked transfer must outlast the individual I/O timeout: {result:?}"
        );
        served.unwrap();
        assert!(started.elapsed() > io_timeout);
        assert_eq!(fs::read(path).unwrap(), b"abcdef");
    }

    #[test]
    fn stalled_network_read_remains_bounded_without_a_detached_download_thread() {
        let (_temp, locations, catalog) = fixture();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (release, stop) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || -> std::io::Result<()> {
            let (mut socket, _) = listener.accept()?;
            socket.set_read_timeout(Some(Duration::from_secs(3)))?;
            let mut request = [0; 4096];
            if socket.read(&mut request)? == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Missing fixture request",
                ));
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nb")?;
            socket.flush()?;
            // Release immediately after the real read times out; the finite
            // fallback also stops this fixture if timeout policy regresses.
            let _ = stop.recv_timeout(Duration::from_secs(5));
            Ok(())
        });
        let io_timeout = Duration::from_millis(250);
        let client = download_client_policy()
            .https_only(false)
            .no_proxy()
            .timeout(io_timeout)
            .build()
            .unwrap();
        let mut file = catalog.models[0].files[0].clone();
        file.url = format!("http://{address}/model");
        let path = locations.bundles[0].join("stalled-download");
        let started = Instant::now();
        let result = fetch_model(&client, &file)
            .and_then(|body| download_to(body, &path, &file, &AtomicBool::new(false), |_| {}));
        let elapsed = started.elapsed();
        let _ = release.send(());
        server.join().unwrap().unwrap();
        assert!(result.unwrap_err().contains("interrupted"));
        assert!(
            elapsed >= io_timeout && elapsed < Duration::from_secs(2),
            "Stalled read took {elapsed:?}"
        );
        assert_eq!(fs::read(path).unwrap(), b"b");
    }

    #[test]
    fn excessive_worker_output_is_bounded_and_rejected() {
        assert!(read_bounded(Cursor::new(vec![0; OUTPUT_LIMIT + 1])).is_err());
        assert_eq!(read_bounded(Cursor::new(b"ok")).unwrap(), b"ok");
    }

    #[test]
    #[ignore = "requires the compiled app-owned worker, network, and Vulkan or explicit developer CPU selection"]
    fn real_catalog_download_native_selftest_and_remove() {
        let catalog = catalog().unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let bundle = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../.interpolation-bundle/current/interpolation-sidecar");
        let locations = Locations {
            downloaded: temporary.path().join("models"),
            bundles: vec![bundle],
        };
        let client = download_client().unwrap();
        for model in &catalog.models {
            let mut last = 0;
            let installed = install_at(
                &locations,
                &catalog,
                model,
                &AtomicBool::new(false),
                &mut |update| {
                    assert!(update.completed_bytes >= last);
                    last = update.completed_bytes;
                    eprintln!(
                        "{} {} {}/{}",
                        model.id, update.state, update.completed_bytes, update.total_bytes
                    );
                },
                |file| fetch_model(&client, file),
                self_test,
            )
            .unwrap();
            assert_eq!(installed.state, "ready");
            let (resolved, bundled) = resolve_at(&locations, &catalog, model).unwrap();
            assert!(!bundled);
            assert_eq!(resolved.model_digest.len(), 64);
            assert_eq!(resolved.worker_digest.len(), 64);
            assert!(
                !self_test(&resolved, &AtomicBool::new(false))
                    .unwrap()
                    .is_empty()
            );
            assert!(
                install_at(
                    &locations,
                    &catalog,
                    model,
                    &AtomicBool::new(true),
                    &mut |_| {},
                    |_| panic!("canceled install must not download"),
                    self_test
                )
                .is_err()
            );
            verify_model(&resolved.model_dir, model, &AtomicBool::new(false)).unwrap();
            remove_owned(&locations, &catalog, model).unwrap();
            assert!(!resolved.model_dir.exists());
        }
    }
}
