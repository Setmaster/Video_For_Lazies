//! A held source-file identity shared by probing, preview, export and interpolation.
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

#[derive(Debug)]
pub(crate) struct SourceIdentity {
    path: PathBuf,
    file: File,
    fingerprint: String,
    pub(crate) len: u64,
}
impl SourceIdentity {
    pub(crate) fn capture(path: &Path) -> Result<Self, String> {
        let path =
            fs::canonicalize(path).map_err(|e| format!("Cannot resolve media source: {e}"))?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Hold read sharing only: a newly opened writer/replacement cannot race a job.
            options.share_mode(1);
        }
        let file = options
            .open(&path)
            .map_err(|e| format!("Cannot open media source: {e}"))?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() {
            return Err("Media source must be a regular file.".into());
        }
        let fingerprint = fingerprint(&path, &file, &metadata)?;
        Ok(Self {
            path,
            file,
            fingerprint,
            len: metadata.len(),
        })
    }
    pub(crate) fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn verify_expected(&self, expected: Option<&str>) -> Result<(), String> {
        if let Some(expected) = expected {
            if expected.len() != 64
                || !expected
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err("Invalid audio source identity. Reload the source.".into());
            }
            if expected != self.fingerprint {
                return Err(changed());
            }
        }
        Ok(())
    }
    pub(crate) fn verify(&self, path: &Path) -> Result<(), String> {
        let metadata = self.file.metadata().map_err(|_| changed())?;
        if fingerprint(&self.path, &self.file, &metadata)? != self.fingerprint {
            return Err(changed());
        }
        let current = Self::capture(path)?;
        if current.fingerprint != self.fingerprint {
            return Err(changed());
        }
        Ok(())
    }
}
fn changed() -> String {
    "The media source changed. Reload it and select the audio track again before exporting or previewing.".into()
}
fn fingerprint(path: &Path, file: &File, metadata: &fs::Metadata) -> Result<String, String> {
    let mut hash = Sha256::new();
    hash.update(b"vfl-media-source-v1\0");
    #[cfg(unix)]
    {
        use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
        hash.update(path.as_os_str().as_bytes());
        hash.update(metadata.dev().to_le_bytes());
        hash.update(metadata.ino().to_le_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::{ffi::OsStrExt, io::AsRawHandle};
        #[repr(C)]
        struct FileId {
            volume: u64,
            id: [u8; 16],
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandleEx(
                handle: *mut std::ffi::c_void,
                class: i32,
                info: *mut std::ffi::c_void,
                size: u32,
            ) -> i32;
        }
        let mut id = FileId {
            volume: 0,
            id: [0; 16],
        };
        // FILE_ID_INFO is stable across rename and supports the full 128-bit ReFS ID.
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                18,
                (&mut id as *mut FileId).cast(),
                std::mem::size_of::<FileId>() as u32,
            )
        } == 0
        {
            return Err("Cannot identify the media file on this filesystem.".into());
        }
        for c in path.as_os_str().encode_wide() {
            hash.update(c.to_le_bytes());
        }
        hash.update(id.volume.to_le_bytes());
        hash.update(id.id);
    }
    #[cfg(not(any(unix, windows)))]
    {
        hash.update(path.to_string_lossy().as_bytes());
    }
    #[cfg(not(windows))]
    let _ = file;
    hash.update([0]);
    hash.update(metadata.len().to_le_bytes());
    let modified = metadata
        .modified()
        .map_err(|_| "Source modification time unavailable.")?;
    let (negative, span) = match modified.duration_since(UNIX_EPOCH) {
        Ok(v) => (false, v),
        Err(e) => (true, e.duration()),
    };
    hash.update([u8::from(negative)]);
    hash.update(span.as_secs().to_le_bytes());
    hash.update(span.subsec_nanos().to_le_bytes());
    Ok(format!("{:x}", hash.finalize()))
}
thread_local! { static ACTIVE: RefCell<Option<Arc<SourceIdentity>>> = const {RefCell::new(None)}; }
pub(crate) struct SourceScope {
    previous: Option<Arc<SourceIdentity>>,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl SourceScope {
    pub(crate) fn enter(identity: Arc<SourceIdentity>) -> Self {
        Self {
            previous: ACTIVE.replace(Some(identity)),
            _thread: std::marker::PhantomData,
        }
    }
}
impl Drop for SourceScope {
    fn drop(&mut self) {
        ACTIVE.set(self.previous.take());
    }
}
pub(crate) fn verify_active() -> Result<(), String> {
    ACTIVE.with_borrow(|v| v.as_ref().map_or(Ok(()), |v| v.verify(v.path())))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_binds_the_actual_file_and_fresh_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("in.mp4");
        fs::write(&p, b"one").unwrap();
        let id = SourceIdentity::capture(&p).unwrap();
        assert_eq!(id.fingerprint().len(), 64);
        id.verify(&p).unwrap();
        assert!(id.verify_expected(Some(&"a".repeat(64))).is_err());
        #[cfg(unix)]
        {
            fs::write(&p, b"replacement").unwrap();
            assert!(id.verify(&p).is_err());
        }
    }
    #[test]
    fn source_scope_restores_on_drop() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.mp4");
        fs::write(&path, b"source").unwrap();
        let id = Arc::new(SourceIdentity::capture(&path).unwrap());
        let scope = SourceScope::enter(id);
        verify_active().unwrap();
        drop(scope);
        verify_active().unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn replacement_with_same_length_and_mtime_still_changes_identity() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.mp4");
        fs::write(&source, b"old").unwrap();
        let id = SourceIdentity::capture(&source).unwrap();
        let modified = fs::metadata(&source).unwrap().modified().unwrap();
        let replacement = dir.path().join("replacement.mp4");
        fs::write(&replacement, b"new").unwrap();
        File::open(&replacement)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        fs::rename(&replacement, &source).unwrap();
        assert!(id.verify(&source).is_err());
    }
}
