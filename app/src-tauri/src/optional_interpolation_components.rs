//! Optional, content-pinned Torch components. No pip, dependency resolution or
//! package hooks run on the client. The app-owned offline assembler receives
//! only verified archives, and a real model inference precedes activation.
use crate::interpolation_components::{
    self, InstallUpdate, InterpolationStatus, Locations, ModelExecution, ResolvedModel,
    check_cancel, file_digest, read_bounded,
};
use crate::process::ManagedChild;
use flate2::read::GzDecoder;
use reqwest::Url;
use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::AppHandle;

const RUNTIME_LOCK: &str = include_str!("../../optional-interpolation-runtime.json");
const RUNTIME_HELPER: &[u8] = include_bytes!("../../optional-interpolation/install_runtime.py");
const OWNERSHIP: &str = "VFL_OPTIONAL_COMPONENT.json";
const INVENTORY: &str = "runtime-files.json";
const MAX_MODEL_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const MAX_RUNTIME_BYTES: u64 = 24 * 1024 * 1024 * 1024;
const MAX_RUNTIME_FILES: usize = 150_000;
const CACHE_HEADROOM: u64 = 1024 * 1024 * 1024;
const SELF_TEST_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const ASSEMBLY_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    id: String,
    label: String,
    description: String,
    noncommercial: bool,
    backend: String,
    files: Vec<PinnedFile>,
    #[serde(default)]
    provenance: serde_json::Value,
    #[serde(default)]
    license_notes: Vec<String>,
    #[serde(default)]
    licenses: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PinnedFile {
    path: String,
    url: String,
    bytes: u64,
    sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeLock {
    schema: u32,
    id: String,
    python_version: String,
    torch_version: String,
    platforms: BTreeMap<String, RuntimePlatform>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimePlatform {
    python: RuntimeArtifact,
    python_executable: String,
    site_packages: String,
    minimum_free_bytes: u64,
    wheels: Vec<RuntimeArtifact>,
}

#[derive(Debug, Deserialize)]
struct RuntimeArtifact {
    id: String,
    filename: String,
    url: String,
    bytes: u64,
    sha256: String,
    #[serde(default)]
    archive: String,
    #[serde(default)]
    prefix: String,
}

impl RuntimeArtifact {
    fn pinned(&self) -> PinnedFile {
        PinnedFile {
            path: format!("{}-{}", self.sha256, self.filename),
            url: self.url.clone(),
            bytes: self.bytes,
            sha256: self.sha256.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Ownership {
    schema: u32,
    component: String,
    id: String,
    digest: String,
    inventory_sha256: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeInventory {
    schema: u32,
    runtime_id: String,
    files: Vec<InventoryFile>,
    directories: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InventoryFile {
    path: String,
    bytes: u64,
    sha256: String,
    mode: u32,
}

struct Store {
    models: PathBuf,
    runtimes: PathBuf,
    caches: PathBuf,
    bundles: Vec<PathBuf>,
}

struct RuntimePaths {
    root: PathBuf,
    python: PathBuf,
    inventory_digest: String,
}

struct PreparedRuntime {
    paths: RuntimePaths,
    // Retain staged bytes until the actual model inference has succeeded.
    _stage: Option<tempfile::TempDir>,
    target: PathBuf,
}

impl PreparedRuntime {
    fn activate(self, lock: &RuntimeLock, cancel: &AtomicBool) -> Result<(), String> {
        check_cancel(cancel)?;
        if self._stage.is_some() {
            activate(&self.paths.root, &self.target, || {
                runtime_replaceable(&self.target, &self.paths.root, lock, cancel)
            })?;
        }
        Ok(())
    }
}

fn runtime_path_key(value: &str) -> String {
    if cfg!(windows) {
        value.to_ascii_lowercase()
    } else {
        value.to_string()
    }
}

pub(crate) fn supports(id: &str) -> bool {
    matches!(id, "gmfss-fortuna-union" | "gimm-vfi-r-p" | "ldf-vfi")
}

fn manifest_text(id: &str) -> Result<&'static str, String> {
    match id {
        "gmfss-fortuna-union" => Ok(include_str!(
            "../../optional-interpolation/gmfss-files.json"
        )),
        "gimm-vfi-r-p" => Ok(include_str!("../../optional-interpolation/gimm-files.json")),
        "ldf-vfi" => Ok(include_str!("../../optional-interpolation/ldf-files.json")),
        _ => Err("Choose a supported interpolation model.".into()),
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn runtime_digest() -> String {
    let mut digest = Sha256::new();
    digest.update(RUNTIME_LOCK.as_bytes());
    digest.update(RUNTIME_HELPER);
    format!("{:x}", digest.finalize())
}

fn digest_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn safe_relative(value: &str) -> Result<&str, String> {
    if value.is_empty() || value.len() > 1024 || value.contains('\\') {
        return Err("A component path is not portable.".into());
    }
    for part in value.split('/') {
        let stem = part.split('.').next().unwrap_or("").to_ascii_lowercase();
        if part.is_empty()
            || matches!(part, "." | "..")
            || part.ends_with([' ', '.'])
            || part
                .bytes()
                .any(|b| b < 32 || b == 127 || b"<>:\"|?*".contains(&b))
            || matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
            || ((stem.starts_with("com") || stem.starts_with("lpt"))
                && stem.len() == 4
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            return Err("A component path is not portable.".into());
        }
    }
    Ok(value)
}

fn parse_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "A component URL is invalid.")?;
    if url.scheme() != "https"
        || url.username() != ""
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        return Err("Component downloads require a reviewed HTTPS URL.".into());
    }
    Ok(url)
}

fn source_url(value: &str) -> Result<(), String> {
    let url = parse_url(value)?;
    let parts: Vec<_> = url.path().trim_start_matches('/').split('/').collect();
    let revision = |part: &&str| part.len() == 40 && part.bytes().all(|b| b.is_ascii_hexdigit());
    let valid = match url.host_str() {
        Some("raw.githubusercontent.com") => parts.len() >= 4 && revision(&parts[2]),
        Some("huggingface.co") => parts.len() >= 5 && parts[2] == "resolve" && revision(&parts[3]),
        _ => false,
    };
    if !valid || url.query().is_some() {
        return Err("Model components must name a reviewed immutable source revision.".into());
    }
    Ok(())
}

fn runtime_url(value: &str) -> Result<(), String> {
    let url = parse_url(value)?;
    let valid = match url.host_str() {
        Some("github.com") => url
            .path()
            .starts_with("/astral-sh/python-build-standalone/releases/download/"),
        Some("files.pythonhosted.org") => url.path().starts_with("/packages/"),
        Some("download.pytorch.org") => url.path().starts_with("/whl/"),
        _ => false,
    };
    if !valid || url.query().is_some() {
        return Err("The optional runtime has an unreviewed artifact URL.".into());
    }
    Ok(())
}

fn redirect_url(url: &Url) -> bool {
    parse_url(url.as_str()).is_ok()
        && url.host_str().is_some_and(|host| {
            matches!(
                host,
                "github.com"
                    | "raw.githubusercontent.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
                    | "files.pythonhosted.org"
                    | "download.pytorch.org"
                    | "huggingface.co"
                    | "cdn-lfs.huggingface.co"
                    | "cdn-lfs.hf.co"
                    | "cdn-lfs-us-1.hf.co"
                    | "cdn-lfs-eu-1.hf.co"
                    | "us.aws.cdn.hf.co"
                    | "cas-bridge.xethub.hf.co"
            )
        })
}

fn manifest(id: &str) -> Result<(Manifest, String), String> {
    let text = manifest_text(id)?;
    let model: Manifest = serde_json::from_str(text)
        .map_err(|_| "The embedded optional model manifest is invalid.")?;
    if model.schema_version != 1
        || model.id != id
        || model.label.is_empty()
        || model.description.is_empty()
        || model.files.is_empty()
        || model.files.len() > 2048
        || !matches!(model.backend.as_str(), "torch-pair" | "torch-sequence")
    {
        return Err("The embedded optional model identity is invalid.".into());
    }
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    for file in &model.files {
        safe_relative(&file.path)?;
        if !["models/", "sources/", "licenses/"]
            .iter()
            .any(|prefix| file.path.starts_with(prefix))
            || !names.insert(file.path.to_ascii_lowercase())
            || file.bytes == 0
            || !digest_valid(&file.sha256)
        {
            return Err("The embedded optional model file inventory is invalid.".into());
        }
        source_url(&file.url)?;
        total = total
            .checked_add(file.bytes)
            .ok_or("Optional model size overflow.")?;
    }
    if total > MAX_MODEL_BYTES
        || !model.files.iter().any(|f| f.path.starts_with("models/"))
        || !model.files.iter().any(|f| f.path.starts_with("sources/"))
    {
        return Err("The embedded optional model size or source inventory is invalid.".into());
    }
    // Descriptive/license fields are retained in the embedded manifest digest.
    let _ = (
        &model.provenance,
        &model.license_notes,
        &model.licenses,
        model.noncommercial,
    );
    Ok((model, sha256(text.as_bytes())))
}

fn platform_key() -> Result<&'static str, String> {
    if !cfg!(target_arch = "x86_64") {
        return Err("Optional interpolation supports Windows and Linux x64.".into());
    }
    if cfg!(windows) {
        Ok("windows-x86_64")
    } else if cfg!(target_os = "linux") {
        Ok("linux-x86_64")
    } else {
        Err("Optional interpolation supports Windows and Linux x64.".into())
    }
}

fn runtime_lock() -> Result<RuntimeLock, String> {
    let lock: RuntimeLock = serde_json::from_str(RUNTIME_LOCK)
        .map_err(|_| "The embedded optional runtime lock is invalid.")?;
    safe_relative(&lock.id)?;
    if lock.schema != 1
        || lock.id.contains('/')
        || !lock.python_version.starts_with("3.11.")
        || lock.torch_version != "2.10.0+cu128"
    {
        return Err("The embedded optional runtime identity is invalid.".into());
    }
    for target in lock.platforms.values() {
        safe_relative(&target.python_executable)?;
        safe_relative(&target.site_packages)?;
        if target.python.archive != "tar.gz"
            || target.python.prefix != "python/"
            || target.wheels.is_empty()
            || target.wheels.len() > 128
            || target.minimum_free_bytes < CACHE_HEADROOM
            || target.minimum_free_bytes > 128 * 1024 * 1024 * 1024
        {
            return Err("The optional runtime archive layout is invalid.".into());
        }
        let mut names = BTreeSet::new();
        let mut total = 0u64;
        for artifact in std::iter::once(&target.python).chain(&target.wheels) {
            safe_relative(&artifact.filename)?;
            if artifact.id.is_empty()
                || artifact.filename.contains('/')
                || !names.insert(artifact.filename.to_ascii_lowercase())
                || artifact.bytes == 0
                || !digest_valid(&artifact.sha256)
            {
                return Err("The optional runtime artifact inventory is invalid.".into());
            }
            runtime_url(&artifact.url)?;
            total = total
                .checked_add(artifact.bytes)
                .ok_or("Optional runtime size overflow.")?;
        }
        if total > MAX_RUNTIME_BYTES {
            return Err("The optional runtime exceeds its size limit.".into());
        }
    }
    Ok(lock)
}

fn store_at(locations: Locations) -> Result<Store, String> {
    let root = locations
        .downloaded
        .parent()
        .ok_or("The interpolation data folder is invalid.")?;
    Ok(Store {
        models: root.join("optional-models"),
        runtimes: root.join("optional-runtimes"),
        caches: root.join("optional-cache"),
        bundles: locations.bundles,
    })
}

fn owned_dir(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        owned_dir(parent)?;
    }
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if !meta.is_dir() || redirected(&meta) {
                return Err(
                    "The optional component folder is redirected or not a directory.".into(),
                );
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|_| "Cannot create the optional component folder.".into())
        }
        Err(_) => Err("Cannot inspect the optional component folder.".into()),
    }
}

fn redirected(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0 || metadata.file_type().is_symlink()
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn regular(path: &Path) -> Result<fs::Metadata, String> {
    let meta = fs::symlink_metadata(path).map_err(|_| "An optional component is missing.")?;
    if !meta.is_file() || redirected(&meta) {
        return Err("Optional components must be regular files.".into());
    }
    Ok(meta)
}

fn lock_store(root: &Path, exclusive: bool) -> Result<File, String> {
    owned_dir(root)?;
    let path = root.join(".components.lock");
    if path
        .try_exists()
        .map_err(|_| "Cannot inspect component lease.")?
    {
        regular(&path)?;
    }
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|_| "Cannot open the optional component lease.")?;
    let result = if exclusive {
        file.try_lock()
    } else {
        file.try_lock_shared()
    };
    result.map_err(
        |_| "Optional components are in use by another app window. Try again when it finishes.",
    )?;
    Ok(file)
}

fn model_path(store: &Store, model: &Manifest, digest: &str) -> PathBuf {
    store.models.join(&model.id).join(digest)
}

fn expected_ownership(component: &str, id: &str, digest: &str) -> Ownership {
    Ownership {
        schema: 1,
        component: component.into(),
        id: id.into(),
        digest: digest.into(),
        inventory_sha256: None,
    }
}

fn write_ownership(dir: &Path, ownership: &Ownership) -> Result<(), String> {
    let mut file = File::options()
        .create_new(true)
        .write(true)
        .open(dir.join(OWNERSHIP))
        .map_err(|_| "Cannot record optional component ownership.")?;
    serde_json::to_writer(&mut file, ownership)
        .map_err(|_| "Cannot record optional component ownership.")?;
    file.sync_all()
        .map_err(|_| "Cannot save optional component ownership.".into())
}

fn check_ownership(dir: &Path, expected: &Ownership) -> Result<Ownership, String> {
    owned_dir(dir.parent().ok_or("Invalid component parent.")?)?;
    let meta = fs::symlink_metadata(dir).map_err(|_| "Install this optional model to use it.")?;
    if !meta.is_dir() || redirected(&meta) {
        return Err("The optional component directory is redirected.".into());
    }
    let marker = dir.join(OWNERSHIP);
    if regular(&marker)?.len() > 4096 {
        return Err("Optional component ownership is invalid.".into());
    }
    let actual: Ownership = serde_json::from_reader(
        File::open(marker)
            .map_err(|_| "Cannot read component ownership.")?
            .take(4097),
    )
    .map_err(|_| "Optional component ownership is invalid.")?;
    if actual.schema != 1
        || actual.component != expected.component
        || actual.id != expected.id
        || actual.digest != expected.digest
    {
        return Err("Refusing to change an optional component not owned by the app.".into());
    }
    Ok(actual)
}

fn tree_files(root: &Path, limit: usize) -> Result<(BTreeSet<String>, BTreeSet<String>), String> {
    struct Bounds {
        count: usize,
        limit: usize,
    }
    fn walk(
        root: &Path,
        dir: &Path,
        files: &mut BTreeSet<String>,
        names: &mut BTreeSet<String>,
        directories: &mut BTreeSet<String>,
        bounds: &mut Bounds,
        depth: usize,
    ) -> Result<(), String> {
        if depth > 24 {
            return Err("Optional component directories are too deeply nested.".into());
        }
        for entry in fs::read_dir(dir).map_err(|_| "Cannot inspect optional component files.")? {
            let entry = entry.map_err(|_| "Cannot inspect optional component files.")?;
            bounds.count += 1;
            if bounds.count > bounds.limit * 2 {
                return Err("Optional component inventory exceeds its file limit.".into());
            }
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "Invalid component path.")?
                .components()
                .map(|p| p.as_os_str().to_str().ok_or("Non-Unicode component path."))
                .collect::<Result<Vec<_>, _>>()?
                .join("/");
            safe_relative(&relative)?;
            if !names.insert(runtime_path_key(&relative)) {
                return Err("Optional component path collision.".into());
            }
            let metadata = fs::symlink_metadata(&path)
                .map_err(|_| "Cannot inspect optional component files.")?;
            if redirected(&metadata) {
                return Err("Optional component links are not allowed.".into());
            }
            if metadata.is_dir() {
                directories.insert(relative);
                walk(root, &path, files, names, directories, bounds, depth + 1)?;
            } else if metadata.is_file() {
                files.insert(relative);
            } else {
                return Err("Optional component special files are not allowed.".into());
            }
        }
        Ok(())
    }
    let mut files = BTreeSet::new();
    let mut directories = BTreeSet::new();
    walk(
        root,
        root,
        &mut files,
        &mut BTreeSet::new(),
        &mut directories,
        &mut Bounds { count: 0, limit },
        0,
    )?;
    Ok((files, directories))
}

fn allowed_directories(files: &BTreeSet<String>) -> BTreeSet<String> {
    let mut dirs = BTreeSet::new();
    for file in files {
        let mut parts: Vec<&str> = file.split('/').collect();
        while parts.len() > 1 {
            parts.pop();
            dirs.insert(parts.join("/"));
        }
    }
    dirs
}

fn verify_file(path: &Path, bytes: u64, digest: &str, cancel: &AtomicBool) -> Result<(), String> {
    check_cancel(cancel)?;
    if regular(path)?.len() != bytes {
        return Err("Optional component size verification failed. Retry installation.".into());
    }
    let mut file = File::open(path).map_err(|_| "Cannot read an optional component.")?;
    let mut hash = Sha256::new();
    let mut actual = 0u64;
    let mut buffer = [0u8; 256 * 1024];
    loop {
        check_cancel(cancel)?;
        let count = file
            .read(&mut buffer)
            .map_err(|_| "Cannot verify an optional component.")?;
        if count == 0 {
            break;
        }
        actual = actual
            .checked_add(count as u64)
            .ok_or("Optional component size overflow.")?;
        if actual > bytes {
            return Err("Optional component changed during verification.".into());
        }
        hash.update(&buffer[..count]);
    }
    if actual != bytes || format!("{:x}", hash.finalize()) != digest {
        return Err("Optional component SHA256 verification failed. Retry installation.".into());
    }
    Ok(())
}

fn model_owned(
    dir: &Path,
    model: &Manifest,
    digest: &str,
    verify: bool,
    cancel: &AtomicBool,
) -> Result<(), String> {
    check_ownership(dir, &expected_ownership("model", &model.id, digest))?;
    let allowed: BTreeSet<_> = model
        .files
        .iter()
        .map(|f| f.path.clone())
        .chain(std::iter::once(OWNERSHIP.into()))
        .collect();
    let (actual, directories) = tree_files(dir, 4096)?;
    if verify && actual != allowed
        || !actual.is_subset(&allowed)
        || !directories.is_subset(&allowed_directories(&allowed))
    {
        return Err(
            "The optional model contains missing or unknown files; existing files were preserved."
                .into(),
        );
    }
    if verify {
        for file in &model.files {
            verify_file(&dir.join(&file.path), file.bytes, &file.sha256, cancel)?;
        }
    }
    Ok(())
}

fn runtime_owned(
    dir: &Path,
    lock: &RuntimeLock,
    verify: bool,
    cancel: &AtomicBool,
) -> Result<RuntimePaths, String> {
    let digest = runtime_digest();
    let ownership = check_ownership(dir, &expected_ownership("runtime", &lock.id, &digest))?;
    let inventory_path = dir.join(INVENTORY);
    if regular(&inventory_path)?.len() > 40 * 1024 * 1024 {
        return Err("Optional runtime inventory is too large.".into());
    }
    let inventory_bytes =
        fs::read(&inventory_path).map_err(|_| "Cannot read optional runtime inventory.")?;
    let inventory_digest = sha256(&inventory_bytes);
    if ownership.inventory_sha256.as_ref() != Some(&inventory_digest) {
        return Err("Optional runtime inventory verification failed.".into());
    }
    let inventory: RuntimeInventory = serde_json::from_slice(&inventory_bytes)
        .map_err(|_| "Optional runtime inventory is invalid.")?;
    if inventory.schema != 1
        || inventory.runtime_id != lock.id
        || inventory.files.is_empty()
        || inventory.files.len() > MAX_RUNTIME_FILES
        || inventory.directories.len() > MAX_RUNTIME_FILES
    {
        return Err("Optional runtime inventory identity is invalid.".into());
    }
    let mut allowed = BTreeSet::from([OWNERSHIP.to_string(), INVENTORY.to_string()]);
    let mut folded = BTreeSet::new();
    let mut total = 0u64;
    for file in &inventory.files {
        safe_relative(&file.path)?;
        if !allowed.insert(file.path.clone())
            || !folded.insert(runtime_path_key(&file.path))
            || !digest_valid(&file.sha256)
            || file.mode > 0o777
        {
            return Err("Optional runtime inventory entries are invalid.".into());
        }
        total = total
            .checked_add(file.bytes)
            .ok_or("Optional runtime size overflow.")?;
        if total > MAX_RUNTIME_BYTES {
            return Err("Optional runtime expanded size exceeds its limit.".into());
        }
        if verify {
            let path = dir.join(&file.path);
            verify_file(&path, file.bytes, &file.sha256, cancel)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if regular(&path)?.permissions().mode() & 0o777 != file.mode {
                    return Err("Optional runtime executable permissions changed.".into());
                }
            }
        }
    }
    let mut allowed_dirs = BTreeSet::new();
    for directory in &inventory.directories {
        safe_relative(directory)?;
        if !allowed_dirs.insert(directory.clone()) || !folded.insert(runtime_path_key(directory)) {
            return Err("Optional runtime directory inventory is invalid.".into());
        }
    }
    if !allowed_directories(&allowed).is_subset(&allowed_dirs) {
        return Err("Optional runtime parent directory inventory is incomplete.".into());
    }
    let (actual, directories) = tree_files(dir, MAX_RUNTIME_FILES)?;
    if actual != allowed || directories != allowed_dirs {
        return Err("The optional runtime contains unknown or missing files; existing files were preserved.".into());
    }
    let config = lock
        .platforms
        .get(platform_key()?)
        .ok_or("Optional runtime is unavailable for this platform.")?;
    if !allowed.contains(&config.python_executable) {
        return Err("The optional runtime interpreter is not owned.".into());
    }
    Ok(RuntimePaths {
        root: dir.into(),
        python: dir.join(&config.python_executable),
        inventory_digest,
    })
}

// Repair may replace a recognized generation with missing/corrupt known bytes.
// The fully verified replacement supplies the allowlist; unknown additions and
// redirected paths still prevent any removal of the previous installation.
fn runtime_replaceable(
    previous: &Path,
    replacement: &Path,
    lock: &RuntimeLock,
    cancel: &AtomicBool,
) -> Result<(), String> {
    check_cancel(cancel)?;
    check_ownership(
        previous,
        &expected_ownership("runtime", &lock.id, &runtime_digest()),
    )?;
    runtime_owned(replacement, lock, false, cancel)?;
    let inventory: RuntimeInventory = serde_json::from_slice(
        &fs::read(replacement.join(INVENTORY))
            .map_err(|_| "Cannot read replacement runtime inventory.")?,
    )
    .map_err(|_| "Invalid replacement runtime inventory.")?;
    let allowed: BTreeSet<String> = inventory
        .files
        .into_iter()
        .map(|f| f.path)
        .chain([OWNERSHIP.to_string(), INVENTORY.to_string()])
        .collect();
    let allowed_dirs: BTreeSet<String> = inventory.directories.into_iter().collect();
    let (actual, directories) = tree_files(previous, MAX_RUNTIME_FILES)?;
    if !actual.is_subset(&allowed) || !directories.is_subset(&allowed_dirs) {
        return Err("The previous optional runtime contains unknown files or folders; existing files were preserved.".into());
    }
    Ok(())
}

fn qualified_scripts(store: &Store) -> Result<PathBuf, String> {
    let scripts: [(&str, &[u8]); 8] = [
        (
            "worker.py",
            include_bytes!("../../optional-interpolation/worker.py"),
        ),
        (
            "gmfss.py",
            include_bytes!("../../optional-interpolation/gmfss.py"),
        ),
        (
            "gimm.py",
            include_bytes!("../../optional-interpolation/gimm.py"),
        ),
        (
            "ldf.py",
            include_bytes!("../../optional-interpolation/ldf.py"),
        ),
        (
            "install_runtime.py",
            include_bytes!("../../optional-interpolation/install_runtime.py"),
        ),
        (
            "gmfss-files.json",
            include_bytes!("../../optional-interpolation/gmfss-files.json"),
        ),
        (
            "gimm-files.json",
            include_bytes!("../../optional-interpolation/gimm-files.json"),
        ),
        (
            "ldf-files.json",
            include_bytes!("../../optional-interpolation/ldf-files.json"),
        ),
    ];
    for bundle in &store.bundles {
        let root = bundle.join("optional");
        let valid = scripts.iter().all(|(name, expected)| {
            let path = root.join(name);
            regular(&path).is_ok_and(|meta| meta.len() == expected.len() as u64)
                && fs::read(path).is_ok_and(|actual| actual == *expected)
        });
        if valid {
            return Ok(root);
        }
    }
    Err("The optional interpolation app scripts are missing or changed. Reinstall the complete app folder.".into())
}

fn download_client() -> Result<Client, String> {
    Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "Cannot initialize optional component downloads.".into())
}

fn fetch(client: &Client, file: &PinnedFile) -> Result<Response, String> {
    let mut url = parse_url(&file.url)?;
    for _ in 0..6 {
        let response = client
            .get(url.clone())
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .send()
            .map_err(|_| "Cannot download this component. Check your connection and retry.")?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or("A component download returned an invalid redirect.")?;
            let next = url
                .join(location)
                .map_err(|_| "A component download returned an invalid redirect.")?;
            if !redirect_url(&next) {
                return Err("A component download redirected outside its approved hosts.".into());
            }
            url = next;
            continue;
        }
        let response = response
            .error_for_status()
            .map_err(|_| "Cannot download this component. Check your connection and retry.")?;
        if response
            .content_length()
            .is_some_and(|size| size != file.bytes)
        {
            return Err("A component download has an unexpected size.".into());
        }
        return Ok(response);
    }
    Err("A component download redirected too many times.".into())
}

fn download_to(
    mut input: impl Read,
    path: &Path,
    file: &PinnedFile,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<(), String> {
    // A healthy multi-GB transfer gets a size-dependent budget at 128 KiB/s;
    // each blocked network read is separately bounded to thirty seconds.
    let budget =
        Duration::from_secs(600 + file.bytes / (128 * 1024)).min(Duration::from_secs(24 * 60 * 60));
    let started = Instant::now();
    check_cancel(cancel)?;
    let mut output = File::options()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|_| "Cannot create a staged component.")?;
    let mut hash = Sha256::new();
    let mut count = 0u64;
    let mut buffer = [0u8; 256 * 1024];
    let mut last = Instant::now();
    loop {
        check_cancel(cancel)?;
        if started.elapsed() >= budget {
            return Err("Component download exceeded its size-dependent time budget. Retry on a faster connection.".into());
        }
        let n = input
            .read(&mut buffer)
            .map_err(|_| "Component download interrupted. Retry installation.")?;
        if n == 0 {
            break;
        }
        count = count
            .checked_add(n as u64)
            .ok_or("Component download size overflow.")?;
        if count > file.bytes {
            return Err("Component download exceeded its reviewed size.".into());
        }
        output
            .write_all(&buffer[..n])
            .map_err(|_| "Cannot save this component. Check free disk space.")?;
        hash.update(&buffer[..n]);
        if count == file.bytes || last.elapsed() >= Duration::from_millis(200) {
            progress(count);
            last = Instant::now();
        }
    }
    check_cancel(cancel)?;
    if count != file.bytes || format!("{:x}", hash.finalize()) != file.sha256 {
        return Err(
            "Component download failed size/SHA256 verification. Retry installation.".into(),
        );
    }
    output
        .sync_all()
        .map_err(|_| "Cannot finish saving this component.".into())
}

#[derive(Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArtifactReceipt {
    schema: u32,
    id: String,
    path: String,
    bytes: u64,
    sha256: String,
}

fn cache_receipt(path: &Path, expected: &ArtifactReceipt) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err("Cannot inspect runtime cache ownership.".into()),
        Ok(_) => (),
    }
    if regular(path)?.len() > 4096 {
        return Err("Runtime cache ownership is invalid; existing files were preserved.".into());
    }
    let actual: ArtifactReceipt = serde_json::from_slice(
        &fs::read(path).map_err(|_| "Cannot read runtime cache ownership.")?,
    )
    .map_err(|_| "Runtime cache ownership is invalid; existing files were preserved.")?;
    if &actual != expected {
        return Err("Runtime cache ownership differs; existing files were preserved.".into());
    }
    Ok(true)
}

fn cached_artifact(
    root: &Path,
    artifact: &RuntimeArtifact,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
    fetch_file: impl FnOnce(&PinnedFile) -> Result<Box<dyn Read>, String>,
) -> Result<PathBuf, String> {
    owned_dir(root)?;
    check_cancel(cancel)?;
    let file = artifact.pinned();
    let path = root.join(&file.path);
    let receipt_path = root.join(format!("{}.receipt.json", file.path));
    let expected = ArtifactReceipt {
        schema: 1,
        id: artifact.id.clone(),
        path: file.path.clone(),
        bytes: file.bytes,
        sha256: file.sha256.clone(),
    };
    let receipt_exists = cache_receipt(&receipt_path, &expected)?;
    let exists = match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return Err("Cannot inspect the optional runtime download cache.".into()),
        Ok(_) => {
            regular(&path)?;
            true
        }
    };
    let record = || -> Result<(), String> {
        if !cache_receipt(&receipt_path, &expected)? {
            let mut receipt = tempfile::Builder::new()
                .prefix(".receipt-")
                .tempfile_in(root)
                .map_err(|_| "Cannot stage runtime cache ownership.")?;
            serde_json::to_writer(&mut receipt, &expected)
                .map_err(|_| "Cannot record runtime cache ownership.")?;
            receipt
                .as_file()
                .sync_all()
                .map_err(|_| "Cannot save runtime cache ownership.")?;
            fs::hard_link(receipt.path(), &receipt_path).map_err(
                |_| "Cannot activate runtime cache ownership; existing files were preserved.",
            )?;
        }
        Ok(())
    };
    if exists {
        if verify_file(&path, file.bytes, &file.sha256, cancel).is_ok() {
            record()?;
            progress(file.bytes);
            return Ok(path);
        }
        check_cancel(cancel)?;
        if !receipt_exists {
            return Err("An unrecognized runtime cache file failed verification; existing files were preserved.".into());
        }
    }
    let temporary = tempfile::Builder::new()
        .prefix(".download-")
        .tempdir_in(root)
        .map_err(|_| "Cannot stage a runtime download. Check free disk space.")?;
    let partial = temporary.path().join("artifact");
    download_to(fetch_file(&file)?, &partial, &file, cancel, &mut progress)?;
    check_cancel(cancel)?;
    record()?;
    if exists {
        regular(&path)?;
        if !cache_receipt(&receipt_path, &expected)? {
            return Err("Runtime cache ownership disappeared.".into());
        }
        let previous = temporary.path().join("previous");
        fs::rename(&path, &previous).map_err(|_| "Cannot preserve the old runtime cache file.")?;
        if fs::hard_link(&partial, &path).is_err() {
            if fs::rename(&previous, &path).is_err() {
                let _recovery = temporary.keep();
                return Err(
                    "Cannot restore the old runtime cache file; recovery bytes were preserved."
                        .into(),
                );
            }
            return Err(
                "Cannot activate the repaired runtime cache; the previous file was preserved."
                    .into(),
            );
        }
    } else {
        fs::hard_link(&partial, &path)
            .map_err(|_| "Cannot activate the verified runtime download cache.")?;
    }
    Ok(path)
}

fn artifact_cache(
    client: &Client,
    root: &Path,
    artifact: &RuntimeArtifact,
    cancel: &AtomicBool,
    progress: impl FnMut(u64),
) -> Result<PathBuf, String> {
    cached_artifact(root, artifact, cancel, progress, |file| {
        fetch(client, file).map(|response| Box::new(response) as Box<dyn Read>)
    })
}

fn unpack_python(
    archive: &Path,
    root: &Path,
    prefix: &str,
    cancel: &AtomicBool,
) -> Result<(), String> {
    owned_dir(root)?;
    let input = File::open(archive).map_err(|_| "Cannot read the verified Python archive.")?;
    let mut tar = tar::Archive::new(GzDecoder::new(input));
    let mut names = BTreeSet::new();
    let mut files = BTreeSet::new();
    let mut links: Vec<(String, String)> = Vec::new();
    let mut total = 0u64;
    for (index, entry) in tar
        .entries()
        .map_err(|_| "The Python archive is invalid.")?
        .enumerate()
    {
        check_cancel(cancel)?;
        if index >= MAX_RUNTIME_FILES {
            return Err("The Python archive contains too many entries.".into());
        }
        let mut entry = entry.map_err(|_| "The Python archive entry is invalid.")?;
        let original = entry
            .path()
            .map_err(|_| "The Python archive path is invalid.")?
            .into_owned();
        let original = original
            .to_str()
            .ok_or("Non-Unicode Python archive path.")?
            .trim_end_matches('/');
        if original == prefix.trim_end_matches('/') && entry.header().entry_type().is_dir() {
            continue;
        }
        let relative = original
            .strip_prefix(prefix)
            .ok_or("The Python archive escaped its expected root.")?
            .to_string();
        safe_relative(&relative)?;
        if !names.insert(runtime_path_key(&relative)) {
            return Err("The Python archive has duplicate paths.".into());
        }
        let path = root.join(&relative);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            owned_dir(&path)?;
            continue;
        }
        if let Some(parent) = path.parent() {
            owned_dir(parent)?;
        }
        if kind.is_symlink() || kind.is_hard_link() {
            let target = entry
                .link_name()
                .map_err(|_| "Invalid Python archive link.")?
                .ok_or("Empty Python archive link.")?;
            let target = target.to_str().ok_or("Non-Unicode Python archive link.")?;
            let target = if kind.is_hard_link() {
                target
                    .strip_prefix(prefix)
                    .ok_or("Python hardlink escaped its root.")?
                    .to_string()
            } else {
                let mut parts: Vec<&str> = relative.split('/').collect();
                parts.pop();
                if target.starts_with('/') || target.contains('\\') {
                    return Err("Python link escaped its root.".into());
                }
                for part in target.split('/') {
                    match part {
                        "." => (),
                        ".." => {
                            if parts.pop().is_none() {
                                return Err("Python link escaped its root.".into());
                            }
                        }
                        part => {
                            safe_relative(part)?;
                            parts.push(part);
                        }
                    }
                }
                parts.join("/")
            };
            safe_relative(&target)?;
            links.push((relative, target));
            continue;
        }
        if !kind.is_file() {
            return Err("Python archive special entries are not allowed.".into());
        }
        let bytes = entry
            .header()
            .size()
            .map_err(|_| "Invalid Python archive size.")?;
        total = total
            .checked_add(bytes)
            .ok_or("Python archive size overflow.")?;
        if bytes > 4 * 1024 * 1024 * 1024 || total > MAX_RUNTIME_BYTES {
            return Err("The Python archive exceeds its expanded size limit.".into());
        }
        let mut output = File::options()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|_| "Cannot extract the private Python runtime.")?;
        let mut actual = 0u64;
        let mut buffer = [0u8; 256 * 1024];
        loop {
            check_cancel(cancel)?;
            let n = entry
                .read(&mut buffer)
                .map_err(|_| "Cannot read a Python archive entry.")?;
            if n == 0 {
                break;
            }
            actual += n as u64;
            if actual > bytes {
                return Err("Python archive expanded beyond its declared size.".into());
            }
            output
                .write_all(&buffer[..n])
                .map_err(|_| "Cannot save the private Python runtime.")?;
        }
        if actual != bytes {
            return Err("Python archive entry was incomplete.".into());
        }
        output
            .sync_all()
            .map_err(|_| "Cannot finish extracting Python.")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = entry
                .header()
                .mode()
                .map_err(|_| "Invalid Python file permissions.")?
                & 0o777;
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))
                .map_err(|_| "Cannot set Python file permissions.")?;
        }
        files.insert(relative);
    }
    // Materialize only contained links to verified regular entries. Nothing in
    // the managed prefix remains a symlink or hardlink to an external inode.
    for _ in 0..32 {
        if links.is_empty() {
            return Ok(());
        }
        let mut pending = Vec::new();
        let before = links.len();
        for (name, target) in links {
            check_cancel(cancel)?;
            if files.contains(&target) {
                let source = root.join(&target);
                let destination = root.join(&name);
                let size = regular(&source)?.len();
                total = total
                    .checked_add(size)
                    .ok_or("Python link materialization size overflow.")?;
                if total > MAX_RUNTIME_BYTES {
                    return Err("Python links exceed the expanded size limit.".into());
                }
                if destination
                    .try_exists()
                    .map_err(|_| "Cannot inspect Python link destination.")?
                {
                    return Err("Python link collided with an existing file.".into());
                }
                fs::copy(source, &destination)
                    .map_err(|_| "Cannot materialize the private Python runtime link.")?;
                files.insert(name);
            } else {
                pending.push((name, target));
            }
        }
        if pending.len() == before {
            return Err("Python archive has unresolved, cyclic or directory links.".into());
        }
        links = pending;
    }
    Err("Python archive link depth exceeds its limit.".into())
}

/// Also used by the media path. Do not inherit Python, Torch, Hugging Face,
/// browser/profile, proxy, credential or package-manager configuration.
pub(crate) fn configure_command(command: &mut Command, cache: &Path) -> Result<(), String> {
    owned_dir(cache)?;
    for name in [
        "home",
        "tmp",
        "local",
        "roaming",
        "torch",
        "cuda",
        "triton",
        "inductor",
        "huggingface",
        "zig-global",
        "zig-local",
    ] {
        owned_dir(&cache.join(name))?;
    }
    command.env_clear();
    for name in [
        "SystemRoot",
        "WINDIR",
        "COMSPEC",
        "PROCESSOR_ARCHITECTURE",
        "NUMBER_OF_PROCESSORS",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("HOME", cache.join("home"))
        .env("USERPROFILE", cache.join("home"))
        .env("LOCALAPPDATA", cache.join("local"))
        .env("APPDATA", cache.join("roaming"))
        .env("TEMP", cache.join("tmp"))
        .env("TMP", cache.join("tmp"))
        .env("TMPDIR", cache.join("tmp"))
        .env("XDG_CACHE_HOME", cache)
        .env("TORCH_HOME", cache.join("torch"))
        .env("CUDA_CACHE_PATH", cache.join("cuda"))
        .env("TRITON_CACHE_DIR", cache.join("triton"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("HF_HUB_OFFLINE", "1")
        .env("TRANSFORMERS_OFFLINE", "1")
        .env("LANG", "C.UTF-8")
        .current_dir(cache);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    Ok(())
}

fn run_bounded(
    command: Command,
    cancel: &AtomicBool,
    timeout: Duration,
    failure: &str,
) -> Result<Vec<u8>, String> {
    check_cancel(cancel)?;
    let mut child = ManagedChild::spawn(command).map_err(|_| failure.to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Optional engine output is unavailable.")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Optional engine diagnostics are unavailable.")?;
    let output = std::thread::spawn(move || read_bounded(stdout));
    let diagnostics = std::thread::spawn(move || read_bounded(stderr));
    let start = Instant::now();
    let outcome = loop {
        if let Err(error) = check_cancel(cancel) {
            break Err(error);
        }
        if start.elapsed() >= timeout {
            break Err(format!("{failure} The operation timed out."));
        }
        match child.wait_timeout(Duration::from_millis(100)) {
            Ok(Some(status)) => {
                break if status.success() {
                    Ok(())
                } else {
                    Err(failure.to_string())
                };
            }
            Ok(None) => (),
            Err(_) => break Err(failure.to_string()),
        }
    };
    drop(child);
    let stdout = output
        .join()
        .map_err(|_| "Optional engine output reader stopped.")?;
    let stderr = diagnostics
        .join()
        .map_err(|_| "Optional engine diagnostic reader stopped.")?;
    outcome?;
    stderr?;
    stdout
}

fn emit(
    progress: &mut impl FnMut(InstallUpdate),
    state: &str,
    completed: u64,
    total: u64,
    message: &str,
) {
    progress(InstallUpdate {
        state: state.into(),
        completed_bytes: completed,
        total_bytes: total,
        message: message.into(),
    });
}

fn total_download(model: &Manifest, platform: &RuntimePlatform) -> u64 {
    model
        .files
        .iter()
        .map(|f| f.bytes)
        .chain(std::iter::once(platform.python.bytes))
        .chain(platform.wheels.iter().map(|f| f.bytes))
        .sum()
}

fn remaining_download(
    model: &Manifest,
    platform: &RuntimePlatform,
    model_verified: bool,
    runtime_verified: bool,
) -> u64 {
    let mut bytes = total_download(model, platform);
    if model_verified {
        bytes -= model.files.iter().map(|file| file.bytes).sum::<u64>();
    }
    if runtime_verified {
        bytes -= platform.python.bytes + platform.wheels.iter().map(|file| file.bytes).sum::<u64>();
    }
    bytes
}

struct DownloadProgress {
    completed: u64,
    total: u64,
    verified_runtime: Option<RuntimePaths>,
}

fn required_storage(
    minimum: u64,
    runtime_verified: bool,
    missing_model_bytes: u64,
) -> Result<u64, String> {
    (if runtime_verified {
        CACHE_HEADROOM
    } else {
        minimum
    })
    .checked_add(missing_model_bytes)
    .ok_or_else(|| "Optional installation storage requirement overflow.".into())
}

fn admit_storage(store: &Store, required: u64, cancel: &AtomicBool) -> Result<(), String> {
    let helper = interpolation_components::worker_path(&Locations {
        downloaded: store.models.clone(),
        bundles: store.bundles.clone(),
    })?;
    for root in [&store.models, &store.runtimes, &store.caches] {
        check_cancel(cancel)?;
        owned_dir(root)?;
        let free = crate::interpolation_media::available_space(&helper, root)?;
        if free < required {
            return Err(format!(
                "Not enough free storage for optional installation: need at least {} GiB available. Free storage and retry.",
                required.div_ceil(1024 * 1024 * 1024)
            ));
        }
    }
    check_cancel(cancel)
}

fn assemble_runtime(
    store: &Store,
    lock: &RuntimeLock,
    scripts: &Path,
    cancel: &AtomicBool,
    client: &Client,
    counts: &mut DownloadProgress,
    progress: &mut impl FnMut(InstallUpdate),
) -> Result<PreparedRuntime, String> {
    let completed = &mut counts.completed;
    let total = counts.total;
    let platform = lock
        .platforms
        .get(platform_key()?)
        .ok_or("The optional runtime is unavailable for this platform.")?;
    let target = store.runtimes.join(&lock.id).join(runtime_digest());
    if let Some(paths) = counts.verified_runtime.take() {
        emit(
            progress,
            "verifying",
            *completed,
            total,
            "Verified the shared optional runtime.",
        );
        return Ok(PreparedRuntime {
            paths,
            _stage: None,
            target,
        });
    }
    check_cancel(cancel)?;
    let artifacts = store.runtimes.join("artifacts");
    let mut python_archive = None;
    for artifact in std::iter::once(&platform.python).chain(&platform.wheels) {
        let path = artifact_cache(client, &artifacts, artifact, cancel, |bytes| {
            emit(
                progress,
                "downloading",
                *completed + bytes,
                total,
                "Downloading the verified shared optional runtime.",
            );
        })?;
        if artifact.id == platform.python.id {
            python_archive = Some(path);
        }
        *completed += artifact.bytes;
    }
    let parent = target.parent().ok_or("Invalid optional runtime folder.")?;
    owned_dir(parent)?;
    let stage = tempfile::Builder::new()
        .prefix(".install-")
        .tempdir_in(parent)
        .map_err(|_| "Cannot stage the optional runtime. Check free disk space.")?;
    let root = stage.path().join("runtime");
    owned_dir(&root)?;
    emit(
        progress,
        "verifying",
        *completed,
        total,
        "Extracting the verified private Python runtime.",
    );
    unpack_python(
        &python_archive.ok_or("Missing Python runtime archive.")?,
        &root,
        &platform.python.prefix,
        cancel,
    )?;
    let lock_path = stage.path().join("runtime-lock.json");
    fs::write(&lock_path, RUNTIME_LOCK).map_err(|_| "Cannot stage the embedded runtime lock.")?;
    emit(
        progress,
        "verifying",
        *completed,
        total,
        "Assembling the verified shared optional runtime offline.",
    );
    let mut command = Command::new(root.join(&platform.python_executable));
    command
        .args(["-I", "-B"])
        .arg(scripts.join("install_runtime.py"))
        .arg("--lock")
        .arg(&lock_path)
        .arg("--platform")
        .arg(platform_key()?)
        .arg("--cache")
        .arg(&artifacts)
        .arg("--root")
        .arg(&root);
    configure_command(&mut command, &store.caches.join("assembly"))?;
    let result = run_bounded(
        command,
        cancel,
        ASSEMBLY_TIMEOUT,
        "The optional runtime could not be assembled. Check free disk space and retry.",
    )?;
    let result: serde_json::Value =
        serde_json::from_slice(&result).map_err(|_| "Invalid optional runtime assembly result.")?;
    if result["schema"] != 1 || result["status"] != "assembled" || result["runtimeId"] != lock.id {
        return Err("The optional runtime returned an invalid assembly identity.".into());
    }
    let mut ownership = expected_ownership("runtime", &lock.id, &runtime_digest());
    ownership.inventory_sha256 = Some(file_digest(&root.join(INVENTORY))?);
    write_ownership(&root, &ownership)?;
    let paths = runtime_owned(&root, lock, true, cancel)?;
    check_cancel(cancel)?;
    Ok(PreparedRuntime {
        paths,
        _stage: Some(stage),
        target,
    })
}

fn activate(
    stage: &Path,
    target: &Path,
    validate_previous: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if !target
        .try_exists()
        .map_err(|_| "Cannot inspect component activation destination.")?
    {
        return fs::rename(stage, target)
            .map_err(|_| "Cannot activate the verified optional component.".into());
    }
    validate_previous()?;
    let backup = tempfile::Builder::new()
        .prefix(".replaced-")
        .tempdir_in(target.parent().ok_or("Invalid component parent.")?)
        .map_err(|_| "Cannot preserve the previous optional component.")?;
    let previous = backup.path().join("previous");
    fs::rename(target, &previous)
        .map_err(|_| "Cannot preserve the previous optional component.")?;
    if fs::rename(stage, target).is_err() {
        if fs::rename(&previous, target).is_err() {
            let _ = backup.keep();
            return Err(
                "Component activation failed. The previous installation is preserved for recovery."
                    .into(),
            );
        }
        return Err(
            "Cannot activate the optional component. The previous installation was preserved."
                .into(),
        );
    }
    Ok(())
}

fn resolved(
    store: &Store,
    model: &Manifest,
    digest: &str,
    root: &Path,
    runtime: &RuntimePaths,
    scripts: &Path,
) -> Result<ResolvedModel, String> {
    let locations = Locations {
        downloaded: store.models.clone(),
        bundles: store.bundles.clone(),
    };
    let helper = interpolation_components::worker_path(&locations)?;
    let python = runtime.python.clone();
    let script = scripts.join("worker.py");
    let source = root.join("sources");
    let cache = store.caches.join(&model.id).join(digest);
    owned_dir(&cache)?;
    let execution = if model.backend == "torch-sequence" {
        ModelExecution::TorchSequence {
            python_path: python,
            script_path: script,
            source_dir: source,
            cache_dir: cache,
        }
    } else {
        ModelExecution::TorchPair {
            python_path: python,
            script_path: script,
            source_dir: source,
            cache_dir: cache,
        }
    };
    let mut hash = Sha256::new();
    hash.update(runtime.inventory_digest.as_bytes());
    hash.update(runtime_digest().as_bytes());
    for name in ["worker.py", "gmfss.py", "gimm.py", "ldf.py"] {
        hash.update(file_digest(&scripts.join(name))?);
    }
    Ok(ResolvedModel {
        execution,
        worker_path: helper,
        model_dir: root.join("models"),
        model_id: model.id.clone(),
        model_digest: digest.into(),
        worker_digest: format!("{:x}", hash.finalize()),
        _lease: None,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelfTest {
    protocol: u32,
    ok: bool,
    device_name: String,
    model_id: String,
    backend: String,
}
static TEST_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn self_test(model: &ResolvedModel, cancel: &AtomicBool) -> Result<String, String> {
    check_cancel(cancel)?;
    let (python, script, sources, cache) = match &model.execution {
        ModelExecution::TorchPair {
            python_path,
            script_path,
            source_dir,
            cache_dir,
        }
        | ModelExecution::TorchSequence {
            python_path,
            script_path,
            source_dir,
            cache_dir,
        } => (python_path, script_path, source_dir, cache_dir),
        ModelExecution::Native => {
            return Err("Invalid optional interpolation execution backend.".into());
        }
    };
    let gpu = interpolation_components::gpu_choice();
    let key = format!(
        "{}|{}|{}|{}",
        model.model_id, model.model_digest, model.worker_digest, gpu
    );
    let cache_results = TEST_CACHE.get_or_init(Mutex::default);
    if let Some(device) = cache_results
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned()
    {
        return Ok(device);
    }
    let mut command = Command::new(python);
    command
        .args(["-I", "-B"])
        .arg(script)
        .arg("--model-id")
        .arg(&model.model_id)
        .arg("--model")
        .arg(&model.model_dir)
        .arg("--sources")
        .arg(sources)
        .arg("--cache")
        .arg(cache)
        .arg("--gpu")
        .arg(&gpu)
        .arg("--self-test");
    configure_command(&mut command, cache)?;
    let output = run_bounded(
        command,
        cancel,
        SELF_TEST_TIMEOUT,
        "The optional model could not complete a tiny inference. Check your graphics driver, available memory and model installation.",
    )?;
    let device = validate_self_test(&output, &model.model_id)?;
    check_cancel(cancel)?;
    let mut cache = cache_results.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= 32 {
        cache.clear();
    }
    cache.insert(key, device.clone());
    Ok(device)
}

fn validate_self_test(output: &[u8], id: &str) -> Result<String, String> {
    let result: SelfTest = serde_json::from_slice(output)
        .map_err(|_| "The optional model returned an invalid self-test result.")?;
    let backend = match id {
        "gmfss-fortuna-union" => "gmfss",
        "gimm-vfi-r-p" => "gimm",
        "ldf-vfi" => "ldf",
        _ => return Err("Invalid optional interpolation model identity.".into()),
    };
    if result.protocol != 1
        || !result.ok
        || result.model_id != id
        || result.backend != backend
        || result.device_name.trim().is_empty()
        || result.device_name.len() > 256
    {
        return Err("The optional model returned an incompatible self-test result.".into());
    }
    Ok(result.device_name)
}

pub(crate) fn resolve_model(app: &AppHandle, id: &str) -> Result<ResolvedModel, String> {
    resolve_at(&store_at(interpolation_components::locations(app)?)?, id)
}

fn resolve_at(store: &Store, id: &str) -> Result<ResolvedModel, String> {
    let (model, digest) = manifest(id)?;
    let lock = runtime_lock()?;
    let model_lease = lock_store(&store.models, false)?;
    let runtime_lease = lock_store(&store.runtimes, false)?;
    let scripts = qualified_scripts(store)?;
    let path = model_path(store, &model, &digest);
    model_owned(&path, &model, &digest, true, &AtomicBool::new(false))?;
    let runtime = runtime_owned(
        &store.runtimes.join(&lock.id).join(runtime_digest()),
        &lock,
        true,
        &AtomicBool::new(false),
    )?;
    let mut resolved = resolved(store, &model, &digest, &path, &runtime, &scripts)?;
    resolved._lease = Some(Arc::new(vec![model_lease, runtime_lease]));
    Ok(resolved)
}

fn status_value(
    model: &Manifest,
    state: &str,
    message: String,
    bytes: u64,
    installed: u64,
    device: Option<String>,
) -> InterpolationStatus {
    InterpolationStatus {
        state: state.into(),
        model_id: model.id.clone(),
        message,
        download_bytes: bytes,
        installed_bytes: installed,
        device_name: device,
        bundled: false,
    }
}

pub(crate) fn status(app: &AppHandle, id: &str) -> Result<InterpolationStatus, String> {
    status_at(&store_at(interpolation_components::locations(app)?)?, id)
}

fn status_at(store: &Store, id: &str) -> Result<InterpolationStatus, String> {
    let (model, digest) = manifest(id)?;
    let lock = runtime_lock()?;
    let platform = lock
        .platforms
        .get(platform_key()?)
        .ok_or("Optional runtime is unavailable for this platform.")?;
    let model_lease = lock_store(&store.models, false)?;
    let runtime_lease = lock_store(&store.runtimes, false)?;
    let scripts = qualified_scripts(store)?;
    let cancel = AtomicBool::new(false);
    let path = model_path(store, &model, &digest);
    let model_valid = model_owned(&path, &model, &digest, true, &cancel);
    let runtime = runtime_owned(
        &store.runtimes.join(&lock.id).join(runtime_digest()),
        &lock,
        true,
        &cancel,
    );
    let total = remaining_download(&model, platform, model_valid.is_ok(), runtime.is_ok());
    let installed = if model_owned(&path, &model, &digest, false, &cancel).is_ok() {
        model
            .files
            .iter()
            .filter_map(|file| regular(&path.join(&file.path)).ok())
            .map(|m| m.len())
            .sum()
    } else {
        0
    };
    let checked = (|| {
        model_valid?;
        let runtime = runtime?;
        let mut resolved = resolved(store, &model, &digest, &path, &runtime, &scripts)?;
        resolved._lease = Some(Arc::new(vec![model_lease, runtime_lease]));
        Ok::<_, String>(resolved)
    })();
    let resolved = match checked {
        Ok(resolved) => resolved,
        Err(message) => {
            return Ok(status_value(
                &model,
                if installed == 0 { "missing" } else { "failed" },
                message,
                total,
                installed,
                None,
            ));
        }
    };
    match self_test(&resolved, &cancel) {
        Ok(device) => Ok(status_value(
            &model,
            "ready",
            "Ready for smooth motion.".into(),
            total,
            installed,
            Some(device),
        )),
        Err(error) => Ok(status_value(
            &model,
            "unavailable",
            error,
            total,
            installed,
            None,
        )),
    }
}

pub(crate) fn install(
    app: &AppHandle,
    id: &str,
    cancel: &AtomicBool,
    mut progress: impl FnMut(InstallUpdate),
) -> Result<InterpolationStatus, String> {
    let store = store_at(interpolation_components::locations(app)?)?;
    let client = download_client()?;
    install_at(&store, id, cancel, &mut progress, |file| {
        fetch(&client, file).map(|response| Box::new(response) as Box<dyn Read>)
    })
}

fn install_at(
    store: &Store,
    id: &str,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(InstallUpdate),
    mut model_fetch: impl FnMut(&PinnedFile) -> Result<Box<dyn Read>, String>,
) -> Result<InterpolationStatus, String> {
    let (model, digest) = manifest(id)?;
    let lock = runtime_lock()?;
    let _model_lease = lock_store(&store.models, true)?;
    let _runtime_lease = lock_store(&store.runtimes, true)?;
    let scripts = qualified_scripts(store)?;
    let platform = lock
        .platforms
        .get(platform_key()?)
        .ok_or("Optional runtime is unavailable for this platform.")?;
    let client = download_client()?;
    let target = model_path(store, &model, &digest);
    let model_bytes: u64 = model.files.iter().map(|f| f.bytes).sum();
    let model_verified = model_owned(&target, &model, &digest, true, cancel).is_ok();
    let verified_runtime = runtime_owned(
        &store.runtimes.join(&lock.id).join(runtime_digest()),
        &lock,
        true,
        cancel,
    )
    .ok();
    check_cancel(cancel)?;
    admit_storage(
        store,
        required_storage(
            platform.minimum_free_bytes,
            verified_runtime.is_some(),
            if model_verified { 0 } else { model_bytes },
        )?,
        cancel,
    )?;
    let total = remaining_download(&model, platform, model_verified, verified_runtime.is_some());
    let mut counts = DownloadProgress {
        completed: 0,
        total,
        verified_runtime,
    };
    // Model network transfer precedes runtime assembly. All network events stay
    // in downloading; the UI deliberately rejects backwards phase transitions.
    let stage = if model_verified {
        None
    } else {
        let parent = target.parent().ok_or("Invalid optional model folder.")?;
        owned_dir(parent)?;
        let stage = tempfile::Builder::new()
            .prefix(".install-")
            .tempdir_in(parent)
            .map_err(|_| "Cannot stage the optional model. Check free disk space.")?;
        write_ownership(stage.path(), &expected_ownership("model", id, &digest))?;
        for file in &model.files {
            check_cancel(cancel)?;
            let path = stage.path().join(&file.path);
            owned_dir(path.parent().ok_or("Invalid optional model file path.")?)?;
            download_to(model_fetch(file)?, &path, file, cancel, |bytes| {
                emit(
                    progress,
                    "downloading",
                    counts.completed + bytes,
                    total,
                    "Downloading the verified optional model and source files.",
                );
            })?;
            counts.completed += file.bytes;
        }
        Some(stage)
    };
    let runtime = assemble_runtime(
        store,
        &lock,
        &scripts,
        cancel,
        &client,
        &mut counts,
        progress,
    )?;
    check_cancel(cancel)?;
    let model_root = stage
        .as_ref()
        .map_or(target.as_path(), |stage| stage.path());
    emit(
        progress,
        "verifying",
        counts.completed,
        total,
        "Verifying every optional model and source file.",
    );
    model_owned(model_root, &model, &digest, true, cancel)?;
    let resolved = resolved(store, &model, &digest, model_root, &runtime.paths, &scripts)?;
    emit(
        progress,
        "testing",
        counts.completed,
        total,
        "Testing the optional model with a tiny inference.",
    );
    let device = self_test(&resolved, cancel)?;
    check_cancel(cancel)?;
    runtime.activate(&lock, cancel)?;
    if let Some(stage) = &stage {
        activate(stage.path(), &target, || {
            model_owned(&target, &model, &digest, false, cancel)
        })?;
    }
    Ok(status_value(
        &model,
        "ready",
        "Ready for smooth motion.".into(),
        total,
        model_bytes,
        Some(device),
    ))
}

pub(crate) fn remove(app: &AppHandle, id: &str) -> Result<InterpolationStatus, String> {
    remove_at(&store_at(interpolation_components::locations(app)?)?, id)
}

fn remove_at(store: &Store, id: &str) -> Result<InterpolationStatus, String> {
    let (model, digest) = manifest(id)?;
    let lock = runtime_lock()?;
    let _lease = lock_store(&store.models, true)?;
    let _runtime_lease = lock_store(&store.runtimes, false)?;
    let retained_runtime_verified = runtime_owned(
        &store.runtimes.join(&lock.id).join(runtime_digest()),
        &lock,
        true,
        &AtomicBool::new(false),
    )
    .is_ok();
    let target = model_path(store, &model, &digest);
    if target
        .try_exists()
        .map_err(|_| "Cannot inspect the optional model folder.")?
    {
        model_owned(&target, &model, &digest, false, &AtomicBool::new(false))?;
        fs::remove_dir_all(&target)
            .map_err(|_| "Cannot remove the optional model. Close other app windows and retry.")?;
    }
    // The shared runtime and cache are deliberately retained. Another installed
    // model or a leased export may still need them; no implicit shared uninstall.
    let platform = lock
        .platforms
        .get(platform_key()?)
        .ok_or("Optional runtime is unavailable for this platform.")?;
    Ok(status_value(
        &model,
        "missing",
        "Optional model removed. Its shared runtime is retained for other models.".into(),
        remaining_download(&model, platform, false, retained_runtime_verified),
        0,
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Cursor;

    fn fixture_model() -> (Manifest, String) {
        (
            Manifest {
                schema_version: 1,
                id: "gmfss-fortuna-union".into(),
                label: "Fixture".into(),
                description: "Fixture only".into(),
                noncommercial: false,
                backend: "torch-pair".into(),
                files: vec![
                    PinnedFile {
                        path: "sources/package/model.py".into(),
                        url: "unused".into(),
                        bytes: 4,
                        sha256: sha256(b"code"),
                    },
                    PinnedFile {
                        path: "models/weights.bin".into(),
                        url: "unused".into(),
                        bytes: 4,
                        sha256: sha256(b"data"),
                    },
                ],
                provenance: serde_json::Value::Null,
                license_notes: Vec::new(),
                licenses: serde_json::Value::Null,
            },
            sha256(b"fixture manifest"),
        )
    }

    fn put_model(path: &Path, model: &Manifest, digest: &str) {
        owned_dir(path).unwrap();
        write_ownership(path, &expected_ownership("model", &model.id, digest)).unwrap();
        owned_dir(&path.join("sources/package")).unwrap();
        owned_dir(&path.join("models")).unwrap();
        fs::write(path.join("sources/package/model.py"), b"code").unwrap();
        fs::write(path.join("models/weights.bin"), b"data").unwrap();
    }

    fn cache_fixture() -> RuntimeArtifact {
        RuntimeArtifact {
            id: "fixture".into(),
            filename: "fixture.whl".into(),
            url: "https://files.pythonhosted.org/packages/fixture.whl".into(),
            bytes: 4,
            sha256: sha256(b"data"),
            archive: String::new(),
            prefix: String::new(),
        }
    }

    #[test]
    fn removal_accounts_for_only_verified_retained_runtime_under_shared_lease() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Store {
            models: temporary.path().join("models"),
            runtimes: temporary.path().join("runtimes"),
            caches: temporary.path().join("cache"),
            bundles: vec![],
        };
        let (model, digest) = manifest("gmfss-fortuna-union").unwrap();
        let lock = runtime_lock().unwrap();
        let platform = lock.platforms.get(platform_key().unwrap()).unwrap();
        let target = model_path(&store, &model, &digest);
        let runtime = store.runtimes.join(&lock.id).join(runtime_digest());
        let create_owned_partial_model = || {
            owned_dir(&target).unwrap();
            write_ownership(&target, &expected_ownership("model", &model.id, &digest)).unwrap();
        };
        // Inert inventory fixtures exercise removal/accounting only. No fake
        // Ready status, interpreter launch or model inference is introduced.
        put_runtime(&runtime, &lock);
        create_owned_partial_model();
        let held = lock_store(&store.runtimes, true).unwrap();
        assert!(remove_at(&store, &model.id).is_err());
        assert!(target.exists(), "Runtime admission precedes model removal");
        drop(held);
        let removed = remove_at(&store, &model.id).unwrap();
        assert_eq!(removed.state, "missing");
        assert_eq!(
            removed.download_bytes,
            model.files.iter().map(|f| f.bytes).sum::<u64>()
        );
        runtime_owned(&runtime, &lock, true, &AtomicBool::new(false)).unwrap();
        let python = runtime.join(&platform.python_executable);
        for state in ["corrupt", "unknown", "missing"] {
            create_owned_partial_model();
            if state == "corrupt" {
                fs::write(&python, b"corrupt").unwrap();
            }
            if state == "unknown" {
                fs::write(&python, b"inert").unwrap();
                fs::write(runtime.join("user-note"), b"preserved").unwrap();
            }
            if state == "missing" {
                fs::remove_file(runtime.join("user-note")).unwrap();
                fs::remove_file(&python).unwrap();
            }
            let removed = remove_at(&store, &model.id).unwrap();
            assert_eq!(removed.download_bytes, total_download(&model, platform));
            assert!(!target.exists() && runtime.exists());
            if state == "corrupt" {
                assert_eq!(fs::read(&python).unwrap(), b"corrupt");
            }
            if state == "unknown" {
                assert_eq!(fs::read(runtime.join("user-note")).unwrap(), b"preserved");
            }
        }
    }

    #[test]
    fn required_download_bytes_exclude_verified_shared_runtime_and_model() {
        let (model, _) = manifest("ldf-vfi").unwrap();
        let lock = runtime_lock().unwrap();
        let platform = lock.platforms.get(platform_key().unwrap()).unwrap();
        let model_bytes = model.files.iter().map(|f| f.bytes).sum::<u64>();
        assert_eq!(
            remaining_download(&model, platform, false, false),
            total_download(&model, platform)
        );
        assert_eq!(
            remaining_download(&model, platform, false, true),
            model_bytes
        );
        assert_eq!(remaining_download(&model, platform, true, true), 0);
        assert_eq!(
            remaining_download(&model, platform, true, false),
            total_download(&model, platform) - model_bytes
        );
    }

    #[test]
    fn install_space_admission_uses_u64_minimum_staging_and_cache_headroom() {
        let minimum = 24 * 1024 * 1024 * 1024;
        let large_model = 7 * 1024 * 1024 * 1024;
        assert_eq!(
            required_storage(minimum, false, large_model).unwrap(),
            minimum + large_model
        );
        assert_eq!(
            required_storage(minimum, true, large_model).unwrap(),
            CACHE_HEADROOM + large_model
        );
        assert_eq!(required_storage(minimum, true, 0).unwrap(), CACHE_HEADROOM);
        assert!(required_storage(minimum, false, u64::MAX).is_err());
    }

    #[test]
    fn runtime_cache_repairs_only_receipted_bytes_and_preserves_failed_repair() {
        let temp = tempfile::tempdir().unwrap();
        let artifact = cache_fixture();
        let cancel = AtomicBool::new(false);
        let fetch_good = |_: &PinnedFile| -> Result<Box<dyn Read>, String> {
            Ok(Box::new(std::io::Cursor::new(b"data")))
        };
        let path = cached_artifact(temp.path(), &artifact, &cancel, |_| (), fetch_good).unwrap();
        fs::write(&path, b"corrupt").unwrap();
        assert!(
            cached_artifact(
                temp.path(),
                &artifact,
                &cancel,
                |_| (),
                |_| Err("fixture transfer failure".into())
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"corrupt");
        assert!(
            cached_artifact(
                temp.path(),
                &artifact,
                &cancel,
                |_| cancel.store(true, std::sync::atomic::Ordering::Release),
                fetch_good
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"corrupt");
        cancel.store(false, std::sync::atomic::Ordering::Release);
        cached_artifact(temp.path(), &artifact, &cancel, |_| (), fetch_good).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"data");
        // A complete pinned artifact from a former version is safely adopted.
        fs::remove_file(
            temp.path()
                .join(format!("{}.receipt.json", artifact.pinned().path)),
        )
        .unwrap();
        cached_artifact(
            temp.path(),
            &artifact,
            &cancel,
            |_| (),
            |_| panic!("valid cached bytes need no fetch"),
        )
        .unwrap();
        fs::remove_file(
            temp.path()
                .join(format!("{}.receipt.json", artifact.pinned().path)),
        )
        .unwrap();
        fs::write(&path, b"unknown").unwrap();
        assert!(
            cached_artifact(
                temp.path(),
                &artifact,
                &cancel,
                |_| (),
                |_| panic!("unknown corrupt bytes must be preserved")
            )
            .is_err()
        );
        assert_eq!(fs::read(path).unwrap(), b"unknown");
    }

    #[cfg(unix)]
    #[test]
    fn redirected_runtime_artifacts_and_worker_cache_leaves_are_preserved() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let external = temp.path().join("outside");
        owned_dir(&external).unwrap();
        fs::write(external.join("sentinel"), b"untouched").unwrap();
        for name in ["inductor", "huggingface", "zig-global", "zig-local"] {
            let cache = temp.path().join(name);
            owned_dir(&cache).unwrap();
            symlink(&external, cache.join(name)).unwrap();
            assert!(configure_command(&mut Command::new("never-executed"), &cache).is_err());
            assert!(
                fs::symlink_metadata(cache.join(name))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
        let artifacts = temp.path().join("artifacts");
        owned_dir(&artifacts).unwrap();
        let artifact = cache_fixture();
        symlink(
            external.join("sentinel"),
            artifacts.join(artifact.pinned().path),
        )
        .unwrap();
        assert!(
            cached_artifact(
                &artifacts,
                &artifact,
                &AtomicBool::new(false),
                |_| (),
                |_| panic!("redirected artifact must not fetch")
            )
            .is_err()
        );
        assert_eq!(fs::read(external.join("sentinel")).unwrap(), b"untouched");
        assert_eq!(fs::read_dir(&external).unwrap().count(), 1);
    }

    fn put_runtime(root: &Path, lock: &RuntimeLock) {
        owned_dir(root).unwrap();
        let python_path = &lock
            .platforms
            .get(platform_key().unwrap())
            .unwrap()
            .python_executable;
        let python = root.join(python_path);
        owned_dir(python.parent().unwrap()).unwrap();
        owned_dir(&root.join("known-empty")).unwrap();
        fs::write(&python, b"inert").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&python, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let (_, directories) = tree_files(root, MAX_RUNTIME_FILES).unwrap();
        let inventory = serde_json::to_vec(&serde_json::json!({"schema":1,"runtimeId":lock.id,"directories":directories,"files":[{"path":python_path,"bytes":5,"sha256":sha256(b"inert"),"mode":0o755}]})).unwrap();
        fs::write(root.join(INVENTORY), &inventory).unwrap();
        let mut owner = expected_ownership("runtime", &lock.id, &runtime_digest());
        owner.inventory_sha256 = Some(sha256(&inventory));
        write_ownership(root, &owner).unwrap();
    }

    #[test]
    fn runtime_repair_accepts_missing_known_bytes_but_preserves_unknown_additions() {
        let temp = tempfile::tempdir().unwrap();
        let lock = runtime_lock().unwrap();
        let cancel = AtomicBool::new(false);
        let old = temp.path().join("old");
        let new = temp.path().join("new");
        put_runtime(&old, &lock);
        put_runtime(&new, &lock);
        runtime_owned(&new, &lock, true, &cancel).unwrap();
        let python = &lock
            .platforms
            .get(platform_key().unwrap())
            .unwrap()
            .python_executable;
        fs::remove_file(old.join(python)).unwrap();
        fs::remove_dir(old.join("known-empty")).unwrap();
        fs::write(old.join(INVENTORY), b"damaged inventory").unwrap();
        assert!(runtime_owned(&old, &lock, true, &cancel).is_err());
        runtime_replaceable(&old, &new, &lock, &cancel).unwrap();
        for name in ["unknown-file", "unknown-directory"] {
            if name.ends_with("file") {
                fs::write(old.join(name), b"preserve").unwrap();
            } else {
                owned_dir(&old.join(name)).unwrap();
            }
            assert!(runtime_replaceable(&old, &new, &lock, &cancel).is_err());
            assert!(old.join(name).exists());
            if name.ends_with("file") {
                fs::remove_file(old.join(name)).unwrap();
            } else {
                fs::remove_dir(old.join(name)).unwrap();
            }
        }
        activate(&new, &old, || {
            runtime_replaceable(&old, &new, &lock, &cancel)
        })
        .unwrap();
        runtime_owned(&old, &lock, true, &cancel).unwrap();
    }

    #[test]
    fn embedded_optional_pins_and_large_counts_are_valid() {
        for id in ["gmfss-fortuna-union", "gimm-vfi-r-p", "ldf-vfi"] {
            let (model, digest) = manifest(id).unwrap();
            assert_eq!(model.id, id);
            assert_eq!(digest.len(), 64);
        }
        let lock = runtime_lock().unwrap();
        let (ldf, _) = manifest("ldf-vfi").unwrap();
        assert!(ldf.files.iter().map(|f| f.bytes).sum::<u64>() > u32::MAX as u64);
        assert!(
            total_download(&ldf, lock.platforms.get("linux-x86_64").unwrap()) > u32::MAX as u64
        );
        assert!(!supports("rife-v4.25"));
        assert!(!supports("gmfss-unreviewed"));
    }

    #[test]
    fn component_paths_reject_traversal_aliases_devices_and_platform_ambiguity() {
        for value in [
            "",
            "/a",
            "../a",
            "a/../b",
            "a//b",
            "a/./b",
            "C:/x",
            "a\\b",
            "a/CON.txt",
            "a/lpt9",
            "a/trailing.",
            "a/space ",
            "a/evil\u{0}",
        ] {
            assert!(safe_relative(value).is_err(), "accepted {value:?}");
        }
        assert_eq!(
            safe_relative("sources/module-name/a_b.py").unwrap(),
            "sources/module-name/a_b.py"
        );
    }

    #[test]
    fn sources_are_revision_pinned_and_redirects_remain_https_allowlisted() {
        assert!(source_url("https://raw.githubusercontent.com/owner/repo/0123456789abcdef0123456789abcdef01234567/model.py").is_ok());
        assert!(source_url("https://huggingface.co/owner/model/resolve/0123456789abcdef0123456789abcdef01234567/model.bin").is_ok());
        for value in [
            "https://raw.githubusercontent.com/o/r/main/model.py",
            "https://huggingface.co/o/r/resolve/main/model.bin",
            "https://example.com/model.bin",
            "http://huggingface.co/o/r/resolve/0123456789abcdef0123456789abcdef01234567/m",
            "https://user:pass@raw.githubusercontent.com/o/r/0123456789abcdef0123456789abcdef01234567/x",
        ] {
            assert!(source_url(value).is_err(), "accepted {value}");
        }
        assert!(redirect_url(
            &Url::parse("https://cas-bridge.xethub.hf.co/x?token=opaque").unwrap()
        ));
        assert!(!redirect_url(
            &Url::parse("https://cas-bridge.xethub.hf.co.evil.test/x").unwrap()
        ));
        assert!(redirect_url(
            &Url::parse("https://us.aws.cdn.hf.co/x?token=opaque").unwrap()
        ));
        for url in [
            "https://us.aws.cdn.hf.co.evil.test/x",
            "https://evil.us.aws.cdn.hf.co/x",
            "http://us.aws.cdn.hf.co/x",
        ] {
            assert!(!redirect_url(&Url::parse(url).unwrap()));
        }
        assert!(!redirect_url(
            &Url::parse("http://release-assets.githubusercontent.com/x").unwrap()
        ));
        assert!(!redirect_url(
            &Url::parse("https://user:pass@release-assets.githubusercontent.com/x").unwrap()
        ));
    }

    #[test]
    fn streamed_download_checks_exact_size_hash_and_cancel_without_activation() {
        let temp = tempfile::tempdir().unwrap();
        let file = PinnedFile {
            path: "data".into(),
            url: "unused".into(),
            bytes: 4,
            sha256: sha256(b"data"),
        };
        let mut reports = Vec::new();
        download_to(
            Cursor::new(b"data"),
            &temp.path().join("good"),
            &file,
            &AtomicBool::new(false),
            |n| reports.push(n),
        )
        .unwrap();
        assert_eq!(reports.last(), Some(&4));
        for (label, bytes) in [
            ("short", b"dat".as_slice()),
            ("long", b"data!".as_slice()),
            ("wrong", b"fail".as_slice()),
        ] {
            assert!(
                download_to(
                    Cursor::new(bytes),
                    &temp.path().join(label),
                    &file,
                    &AtomicBool::new(false),
                    |_| {}
                )
                .is_err()
            );
        }
        assert!(
            download_to(
                Cursor::new(b"data"),
                &temp.path().join("canceled"),
                &file,
                &AtomicBool::new(true),
                |_| {}
            )
            .is_err()
        );
        assert!(!temp.path().join("activated").exists());
    }

    #[test]
    fn model_hash_verification_and_unknown_content_preserve_existing_install() {
        let temp = tempfile::tempdir().unwrap();
        let (model, digest) = fixture_model();
        let path = temp.path().join("model");
        put_model(&path, &model, &digest);
        model_owned(&path, &model, &digest, true, &AtomicBool::new(false)).unwrap();
        fs::write(path.join("models/weights.bin"), b"FAIL").unwrap();
        assert!(model_owned(&path, &model, &digest, true, &AtomicBool::new(false)).is_err());
        model_owned(&path, &model, &digest, false, &AtomicBool::new(false)).unwrap();
        owned_dir(&path.join("unknown-empty-directory")).unwrap();
        assert!(model_owned(&path, &model, &digest, false, &AtomicBool::new(false)).is_err());
        assert_eq!(fs::read(path.join("models/weights.bin")).unwrap(), b"FAIL");
        assert!(path.join("unknown-empty-directory").exists());
    }

    #[test]
    fn refused_activation_keeps_previous_and_staged_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("old");
        let stage = temp.path().join("stage");
        fs::create_dir(&old).unwrap();
        fs::create_dir(&stage).unwrap();
        fs::write(old.join("user.txt"), b"preserve").unwrap();
        fs::write(stage.join("new.txt"), b"new").unwrap();
        assert!(activate(&stage, &old, || Err("Unowned destination".into())).is_err());
        assert_eq!(fs::read(old.join("user.txt")).unwrap(), b"preserve");
        assert_eq!(fs::read(stage.join("new.txt")).unwrap(), b"new");
    }

    #[test]
    fn shared_resolved_leases_hold_both_model_and_runtime_until_last_clone_drops() {
        let temp = tempfile::tempdir().unwrap();
        let models = temp.path().join("models");
        let runtimes = temp.path().join("runtimes");
        let leases = Arc::new(vec![
            lock_store(&models, false).unwrap(),
            lock_store(&runtimes, false).unwrap(),
        ]);
        let second = leases.clone();
        drop(leases);
        assert!(lock_store(&models, true).is_err());
        assert!(lock_store(&runtimes, true).is_err());
        drop(second);
        assert!(lock_store(&models, true).is_ok());
        assert!(lock_store(&runtimes, true).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn optional_ownership_never_follows_redirected_parents_or_components() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, temp.path().join("redirect")).unwrap();
        assert!(lock_store(&temp.path().join("redirect/models"), true).is_err());
        assert!(!outside.join("models").exists());
        let (model, digest) = fixture_model();
        let target = temp.path().join("model");
        put_model(&target, &model, &digest);
        fs::remove_file(target.join("models/weights.bin")).unwrap();
        symlink(
            target.join("sources/package/model.py"),
            target.join("models/weights.bin"),
        )
        .unwrap();
        assert!(model_owned(&target, &model, &digest, false, &AtomicBool::new(false)).is_err());
    }

    fn python_archive(path: &Path, entries: &[(&str, &[u8], Option<&str>)]) {
        let gzip = GzEncoder::new(File::create(path).unwrap(), Compression::default());
        let mut builder = tar::Builder::new(gzip);
        for (name, data, link) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o755);
            if let Some(target) = link {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                header.set_link_name(target).unwrap();
                builder
                    .append_data(&mut header, name, Cursor::new([]))
                    .unwrap();
            } else {
                header.set_size(data.len() as u64);
                builder
                    .append_data(&mut header, name, Cursor::new(data))
                    .unwrap();
            }
        }
        builder.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn python_archive_materializes_only_contained_regular_targets() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("python.tar.gz");
        python_archive(
            &archive,
            &[
                ("python/bin/python3.11", b"inert fixture", None),
                ("python/bin/python", b"", Some("python3.11")),
            ],
        );
        let root = temp.path().join("runtime");
        unpack_python(&archive, &root, "python/", &AtomicBool::new(false)).unwrap();
        assert_eq!(fs::read(root.join("bin/python")).unwrap(), b"inert fixture");
        assert!(
            !fs::symlink_metadata(root.join("bin/python"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn python_archive_rejects_external_links_cycles_and_wrong_roots() {
        let temp = tempfile::tempdir().unwrap();
        for (index, entries) in [
            vec![("python/bin/python", b"".as_slice(), Some("../../outside"))],
            vec![
                ("python/bin/a", b"".as_slice(), Some("b")),
                ("python/bin/b", b"".as_slice(), Some("a")),
            ],
            vec![("other/python", b"data".as_slice(), None)],
        ]
        .iter()
        .enumerate()
        {
            let archive = temp.path().join(format!("{index}.tar.gz"));
            python_archive(&archive, entries);
            assert!(
                unpack_python(
                    &archive,
                    &temp.path().join(format!("root-{index}")),
                    "python/",
                    &AtomicBool::new(false)
                )
                .is_err()
            );
        }
        assert!(!temp.path().join("outside").exists());
    }

    #[test]
    fn self_test_accepts_only_actual_protocol_model_backend_and_device_identity() {
        let valid = serde_json::json!({"protocol":1,"ok":true,"deviceName":"Fixture GPU","modelId":"gimm-vfi-r-p","backend":"gimm"});
        assert_eq!(
            validate_self_test(&serde_json::to_vec(&valid).unwrap(), "gimm-vfi-r-p").unwrap(),
            "Fixture GPU"
        );
        for (field, value) in [
            ("protocol", serde_json::json!(2)),
            ("ok", serde_json::json!(false)),
            ("deviceName", serde_json::json!("")),
            ("modelId", serde_json::json!("ldf-vfi")),
            ("backend", serde_json::json!("torch-pair")),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            assert!(
                validate_self_test(&serde_json::to_vec(&invalid).unwrap(), "gimm-vfi-r-p").is_err()
            );
        }
    }

    #[test]
    fn isolated_command_cannot_inherit_python_profiles_or_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let mut command = Command::new("never-executed");
        command
            .env("HF_TOKEN", "inert fixture")
            .env("PYTHONPATH", "untrusted fixture");
        configure_command(&mut command, temp.path()).unwrap();
        let environment: BTreeMap<_, _> = command
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().to_string(),
                    v.map(|v| v.to_string_lossy().to_string()),
                )
            })
            .collect();
        assert_eq!(environment.get("HF_HUB_OFFLINE"), Some(&Some("1".into())));
        assert_eq!(
            environment.get("PYTHONDONTWRITEBYTECODE"),
            Some(&Some("1".into()))
        );
        for forbidden in [
            "PYTHONPATH",
            "PYTHONHOME",
            "HF_TOKEN",
            "AWS_ACCESS_KEY_ID",
            "HTTP_PROXY",
        ] {
            assert!(!environment.contains_key(forbidden));
        }
        assert_eq!(command.get_current_dir(), Some(temp.path()));
        assert_eq!(
            environment.get("TMPDIR"),
            Some(&Some(temp.path().join("tmp").to_string_lossy().to_string()))
        );
    }

    #[test]
    fn runtime_inventory_binds_every_file_directory_mode_and_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runtime");
        owned_dir(&root).unwrap();
        let lock = runtime_lock().unwrap();
        let platform = lock.platforms.get(platform_key().unwrap()).unwrap();
        let python = root.join(&platform.python_executable);
        owned_dir(python.parent().unwrap()).unwrap();
        fs::write(&python, b"inert interpreter fixture").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&python, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let (_, directories) = tree_files(&root, MAX_RUNTIME_FILES).unwrap();
        let inventory = serde_json::json!({"schema":1,"runtimeId":lock.id,"directories":directories,
            "files":[{"path":platform.python_executable,"bytes":25,"sha256":sha256(b"inert interpreter fixture"),"mode":0o755}]});
        let bytes = serde_json::to_vec(&inventory).unwrap();
        fs::write(root.join(INVENTORY), &bytes).unwrap();
        let mut owner = expected_ownership("runtime", &lock.id, &runtime_digest());
        owner.inventory_sha256 = Some(sha256(&bytes));
        write_ownership(&root, &owner).unwrap();
        runtime_owned(&root, &lock, true, &AtomicBool::new(false)).unwrap();
        fs::write(&python, b"broken interpreter fixture").unwrap();
        assert!(runtime_owned(&root, &lock, true, &AtomicBool::new(false)).is_err());
        // Structural repair validation can preserve known corrupt bytes, but it
        // cannot remove an unknown file or even an unrecorded empty directory.
        runtime_owned(&root, &lock, false, &AtomicBool::new(false)).unwrap();
        owned_dir(&root.join("user-folder")).unwrap();
        assert!(runtime_owned(&root, &lock, false, &AtomicBool::new(false)).is_err());
        fs::remove_dir(root.join("user-folder")).unwrap();
        fs::write(root.join(INVENTORY), b"{}").unwrap();
        assert!(runtime_owned(&root, &lock, false, &AtomicBool::new(false)).is_err());
        assert!(python.exists());
    }

    #[test]
    #[ignore = "requires explicit network qualification authorization; downloads and verifies pinned model data without execution"]
    fn actual_pinned_hugging_face_redirect_download_verifies_complete_payload() {
        assert_eq!(
            std::env::var("VFL_OPTIONAL_NETWORK_QUALIFICATION").as_deref(),
            Ok("1")
        );
        let (model, _) = manifest("gimm-vfi-r-p").unwrap();
        let file = model
            .files
            .iter()
            .filter(|f| {
                f.path.starts_with("models/") && f.url.starts_with("https://huggingface.co/")
            })
            .min_by_key(|f| f.bytes)
            .unwrap();
        let response = fetch(&download_client().unwrap(), file).unwrap();
        let host = response.url().host_str().unwrap().to_string();
        assert_eq!(
            host, "us.aws.cdn.hf.co",
            "Record the actual tested official redirect host"
        );
        let temporary = tempfile::tempdir().unwrap();
        let payload = temporary.path().join("verified-model-data");
        let cancel = AtomicBool::new(false);
        download_to(response, &payload, file, &cancel, |_| ()).unwrap();
        verify_file(&payload, file.bytes, &file.sha256, &cancel).unwrap();
        eprintln!(
            "Actual Rust HTTPS redirect download verified: model={}, path={}, finalHost={}, bytes={}, sha256={}; no payload execution.",
            model.id, file.path, host, file.bytes, file.sha256
        );
    }

    #[test]
    #[ignore = "requires an explicitly supplied, pinned Python archive; extracts data only and never launches Python"]
    fn actual_pinned_python_archive_is_materialized_without_execution() {
        let archive = PathBuf::from(
            std::env::var_os("VFL_OPTIONAL_PYTHON_ARCHIVE")
                .expect("Provide the already-downloaded pinned archive"),
        );
        let lock = runtime_lock().unwrap();
        let platform = lock.platforms.get(platform_key().unwrap()).unwrap();
        verify_file(
            &archive,
            platform.python.bytes,
            &platform.python.sha256,
            &AtomicBool::new(false),
        )
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("runtime");
        unpack_python(
            &archive,
            &root,
            &platform.python.prefix,
            &AtomicBool::new(false),
        )
        .unwrap();
        regular(&root.join(&platform.python_executable)).unwrap();
        let (files, directories) = tree_files(&root, MAX_RUNTIME_FILES).unwrap();
        assert!(files.len() > 1000);
        eprintln!(
            "Verified and safely materialized {} Python files and {} directories without executing any payload.",
            files.len(),
            directories.len()
        );
    }

    #[test]
    #[ignore = "requires parent-reviewed runtime/model qualification metadata and explicitly executes real tiny inference in disposable state"]
    fn actual_cached_runtime_model_install_resolve_status_remove_and_cancel() {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Inputs {
            runtime_root: PathBuf,
            runtime_inventory_sha256: String,
            runtime_lock_sha256: String,
            runtime_helper_sha256: String,
            model_root: PathBuf,
            model_id: String,
            model_manifest_sha256: String,
        }
        let metadata = PathBuf::from(std::env::var_os("VFL_OPTIONAL_INSTALL_QUALIFICATION")
            .expect("Provide the parent-reviewed qualification JSON; source approval precedes execution"));
        assert!(regular(&metadata).unwrap().len() < 16 * 1024);
        let inputs: Inputs = serde_json::from_slice(&fs::read(metadata).unwrap()).unwrap();
        assert_eq!(inputs.runtime_lock_sha256, sha256(RUNTIME_LOCK.as_bytes()));
        assert_eq!(inputs.runtime_helper_sha256, sha256(RUNTIME_HELPER));
        let (model, digest) = manifest(&inputs.model_id).unwrap();
        assert_eq!(inputs.model_manifest_sha256, digest);
        let source_inventory = inputs.runtime_root.join(INVENTORY);
        assert_eq!(
            file_digest(&source_inventory).unwrap(),
            inputs.runtime_inventory_sha256
        );
        let inventory: RuntimeInventory =
            serde_json::from_slice(&fs::read(&source_inventory).unwrap()).unwrap();
        let lock = runtime_lock().unwrap();
        assert_eq!(inventory.runtime_id, lock.id);
        let source_tree = tree_files(&inputs.runtime_root, MAX_RUNTIME_FILES).unwrap();
        let expected_tree = (
            inventory
                .files
                .iter()
                .map(|f| f.path.clone())
                .chain(std::iter::once(INVENTORY.to_string()))
                .collect(),
            inventory.directories.iter().cloned().collect(),
        );
        assert!(
            source_tree == expected_tree,
            "The qualification runtime has missing or unknown files/directories; source state was preserved."
        );
        for file in &model.files {
            verify_file(
                &inputs.model_root.join(&file.path),
                file.bytes,
                &file.sha256,
                &AtomicBool::new(false),
            )
            .unwrap();
        }
        let temporary = tempfile::tempdir().unwrap();
        let store = Store {
            models: temporary.path().join("optional-models"),
            runtimes: temporary.path().join("optional-runtimes"),
            caches: temporary.path().join("optional-cache"),
            bundles: vec![
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../.interpolation-bundle/current/interpolation-sidecar"),
            ],
        };
        qualified_scripts(&store).unwrap();
        owned_dir(&store.models).unwrap();
        let native_helper = interpolation_components::worker_path(&Locations {
            downloaded: store.models.clone(),
            bundles: store.bundles.clone(),
        })
        .unwrap();
        assert!(
            crate::interpolation_media::available_space(&native_helper, &store.models).unwrap() > 0
        );
        let runtime_target = store.runtimes.join(&lock.id).join(runtime_digest());
        owned_dir(&runtime_target).unwrap();
        for directory in &inventory.directories {
            owned_dir(&runtime_target.join(safe_relative(directory).unwrap())).unwrap();
        }
        for file in &inventory.files {
            safe_relative(&file.path).unwrap();
            let source = inputs.runtime_root.join(&file.path);
            verify_file(&source, file.bytes, &file.sha256, &AtomicBool::new(false)).unwrap();
            // All input paths are parent-created disposable qualification
            // files. Inference uses -I -B and never edits the managed prefix;
            // hardlinks avoid duplicating several GB of inert pinned bytes.
            fs::hard_link(source, runtime_target.join(&file.path)).unwrap();
        }
        fs::copy(&source_inventory, runtime_target.join(INVENTORY)).unwrap();
        let mut owner = expected_ownership("runtime", &lock.id, &runtime_digest());
        owner.inventory_sha256 = Some(inputs.runtime_inventory_sha256.clone());
        write_ownership(&runtime_target, &owner).unwrap();
        runtime_owned(&runtime_target, &lock, true, &AtomicBool::new(false)).unwrap();
        let target = model_path(&store, &model, &digest);
        let cancel = AtomicBool::new(false);
        let fetch_local = |file: &PinnedFile| -> Result<Box<dyn Read>, String> {
            verify_file(
                &inputs.model_root.join(&file.path),
                file.bytes,
                &file.sha256,
                &cancel,
            )?;
            File::open(inputs.model_root.join(&file.path))
                .map(|f| Box::new(f) as Box<dyn Read>)
                .map_err(|e| e.to_string())
        };
        let mut canceled_before_activation = false;
        let first = install_at(
            &store,
            &model.id,
            &cancel,
            &mut |update| {
                if update.state == "testing" {
                    assert!(
                        !target.exists(),
                        "No model is activated before inference succeeds"
                    );
                    canceled_before_activation = true;
                    cancel.store(true, std::sync::atomic::Ordering::Release);
                }
            },
            fetch_local,
        );
        assert!(first.is_err() && canceled_before_activation && !target.exists());
        assert!(runtime_target.exists());
        cancel.store(false, std::sync::atomic::Ordering::Release);
        let mut phases = Vec::new();
        let ready = install_at(
            &store,
            &model.id,
            &cancel,
            &mut |update| phases.push(update),
            fetch_local,
        )
        .unwrap();
        assert_eq!(ready.state, "ready");
        assert!(ready.device_name.as_ref().is_some_and(|v| !v.is_empty()));
        assert!(phases.iter().any(|v| v.state == "testing"));
        let model_bytes = model.files.iter().map(|f| f.bytes).sum::<u64>();
        assert_eq!(
            ready.download_bytes, model_bytes,
            "Shared verified runtime must not inflate required transfer bytes"
        );
        let rank = |state: &str| match state {
            "downloading" => 1,
            "verifying" => 2,
            "testing" => 3,
            _ => panic!("unexpected phase"),
        };
        for pair in phases.windows(2) {
            assert!(
                rank(&pair[0].state) <= rank(&pair[1].state),
                "Install phases never regress"
            );
            assert!(pair[0].completed_bytes <= pair[1].completed_bytes);
        }
        assert!(
            phases
                .iter()
                .all(|event| event.total_bytes == model_bytes
                    && event.completed_bytes <= model_bytes)
        );
        eprintln!("Actual installed-generation progress: {}", serde_json::to_string(&phases.iter().map(|e| serde_json::json!({"state":e.state,"completedBytes":e.completed_bytes,"totalBytes":e.total_bytes})).collect::<Vec<_>>()).unwrap());
        model_owned(&target, &model, &digest, true, &cancel).unwrap();
        let checked = status_at(&store, &model.id).unwrap();
        assert_eq!(checked.state, "ready");
        assert_eq!(checked.download_bytes, 0);
        let held = resolve_at(&store, &model.id).unwrap();
        assert!(matches!(
            held.execution,
            ModelExecution::TorchPair { .. } | ModelExecution::TorchSequence { .. }
        ));
        assert!(
            remove_at(&store, &model.id).is_err(),
            "Live resolved model lease prevents uninstall"
        );
        assert!(
            lock_store(&store.runtimes, true).is_err(),
            "Export also leases the shared runtime"
        );
        crate::optional_interpolation_tests::exercise_native_media(&held);
        drop(held);
        let weight = model
            .files
            .iter()
            .find(|f| f.path.starts_with("models/"))
            .unwrap();
        fs::write(target.join(&weight.path), b"inert corruption fixture").unwrap();
        assert!(resolve_at(&store, &model.id).is_err());
        assert_ne!(status_at(&store, &model.id).unwrap().state, "ready");
        let removed = remove_at(&store, &model.id).unwrap();
        assert_eq!(removed.state, "missing");
        assert_eq!(removed.download_bytes, model_bytes);
        assert!(!target.exists() && runtime_target.exists());
        runtime_owned(&runtime_target, &lock, true, &cancel).unwrap();
        assert_eq!(
            file_digest(&source_inventory).unwrap(),
            inputs.runtime_inventory_sha256
        );
        eprintln!(
            "Actual isolated installer proof passed: model={}, device={}, source+runtime pins, cancellation-before-activation, actual tiny inference, Ready, resolve, dual leases, corruption rejection, model removal and shared-runtime retention. Runtime lock digest={} (provisional candidate; later pin changes require reconciliation).",
            model.id,
            ready.device_name.unwrap(),
            inputs.runtime_lock_sha256
        );
    }
}
