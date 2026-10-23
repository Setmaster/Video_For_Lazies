use super::{Bindings, bounded_message};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
const CAP: u64 = 4096;
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Preferences {
    schema_version: u32,
    bindings: Bindings,
}
pub(super) trait Store {
    fn save(&mut self, bindings: &Bindings) -> Result<(), String>;
}
pub(super) struct FileStore {
    path: PathBuf,
}
impl FileStore {
    pub fn new(directory: &Path) -> Self {
        Self {
            path: directory.join("recording-controls.json"),
        }
    }
    pub fn load(&self) -> (Bindings, Option<String>) {
        let read = || -> Result<Bindings, String> {
            regular(&self.path)?;
            if !fs::symlink_metadata(&self.path)
                .map_err(|e| e.to_string())?
                .is_file()
            {
                return Err("Shortcut settings require a regular file; original preserved.".into());
            }
            let f = fs::File::open(&self.path).map_err(|e| e.to_string())?;
            let mut bytes = Vec::new();
            f.take(CAP + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() as u64 > CAP {
                return Err("Shortcut settings exceed 4KiB; original file preserved.".into());
            }
            let p: Preferences = serde_json::from_slice(&bytes)
                .map_err(|_| "Shortcut settings are invalid; original file preserved.")?;
            if p.schema_version != 1 {
                return Err("Unsupported shortcut settings; original file preserved.".into());
            }
            p.bindings.validate()?;
            Ok(p.bindings)
        };
        if !self.path.exists() {
            return (Bindings::default(), None);
        }
        match read() {
            Ok(b) => (b, None),
            Err(e) => (Bindings::default(), Some(bounded_message(e))),
        }
    }
}
fn regular(path: &Path) -> Result<(), String> {
    let mut current = Some(path);
    while let Some(p) = current {
        current = p.parent();
        let Ok(m) = fs::symlink_metadata(p) else {
            continue;
        };
        if m.file_type().is_symlink() {
            return Err("Shortcut settings require a regular local path.".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if m.file_attributes() & (0x400 | 0x1000 | 0x40000 | 0x100000 | 0x400000) != 0 {
                return Err("Shortcut settings cannot use redirected/cloud paths.".into());
            }
        }
    }
    Ok(())
}
impl Store for FileStore {
    fn save(&mut self, bindings: &Bindings) -> Result<(), String> {
        bindings.validate()?;
        let bytes = serde_json::to_vec(&Preferences {
            schema_version: 1,
            bindings: bindings.clone(),
        })
        .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > CAP {
            return Err("Shortcut settings exceed 4KiB.".into());
        }
        let parent = self
            .path
            .parent()
            .ok_or("Shortcut settings directory unavailable.")?;
        regular(parent)?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        regular(&self.path)?;
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.as_file().sync_all())
            .map_err(|e| e.to_string())?;
        file.persist(&self.path).map_err(|e| e.error.to_string())?;
        Ok(())
    }
}
