//! Per-project collections, environments and history for the API surface.
//!
//! One JSON file per project under `<app data>/api/`, named by a hash of the
//! project id so no path component comes from session data. Environments may
//! hold tokens, so the directory is `0700` and each file `0600`, written to a
//! temp file opened `O_NOFOLLOW` and renamed into place.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::model::{Environment, HistoryEntry, Saved};

const VERSION: u32 = 1;
/// A store larger than this is refused rather than parsed.
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiProject {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub collections: Vec<Saved>,
    #[serde(default)]
    pub environments: Vec<Environment>,
    #[serde(default)]
    pub active_environment: Option<String>,
    #[serde(default)]
    pub history: Vec<HistoryEntry>,
}

/// Where API stores live: beside the app's preferences.
pub fn default_root() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/nonexistent"));
    diri_proto::paths::DirijorPaths::prefs_file(&home)
        .parent()
        .map_or_else(|| home.join(".diri"), Path::to_path_buf)
        .join("api")
}

/// The store file for a project id.
pub fn project_file(root: &Path, project: &str) -> PathBuf {
    let digest = Sha256::digest(project.as_bytes());
    let name: String = digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    root.join(format!("{name}.json"))
}

pub fn load(path: &Path) -> io::Result<ApiProject> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(ApiProject::default()),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "API store is not a regular file",
        ));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "API store is too large",
        ));
    }
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub fn save(path: &Path, project: &ApiProject) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "API store has no folder"))?;
    create_private_dir(parent)?;
    let mut project = project.clone();
    project.version = VERSION;
    let bytes = serde_json::to_vec_pretty(&project)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let _ = fs::remove_file(&temporary);
    {
        let mut file = private_options().open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })
}

fn private_options() -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        if !path.exists() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            match fs::DirBuilder::new().mode(0o700).create(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "API store folder is not a directory",
            ));
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    fs::create_dir_all(path)
}
