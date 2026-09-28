//! The install identity and the user-facing telemetry settings.
//!
//! Both live under `<state>/telemetry/`. The app writes `config.json` from
//! Settings; the Engine's uploader re-reads it every cycle, so a toggle takes
//! effect without restarting anything.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const INSTALL_FILE: &str = "install.json";
const CONFIG_FILE: &str = "config.json";
const NAME_MAX_CHARS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// Random, 128-bit, generated once per install.
    pub install_id: String,
    pub created_ms: u64,
}

impl Identity {
    /// Loads the install identity, creating it on first use.
    pub fn load_or_create(state_dir: &Path) -> std::io::Result<Self> {
        let path = telemetry_dir(state_dir).join(INSTALL_FILE);
        if let Ok(bytes) = std::fs::read(&path)
            && let Ok(identity) = serde_json::from_slice::<Identity>(&bytes)
            && identity.install_id.len() == 36
        {
            return Ok(identity);
        }
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|error| std::io::Error::other(error.to_string()))?;
        let hex: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let identity = Identity {
            install_id: format!(
                "{}-{}-{}-{}-{}",
                &hex[0..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..32]
            ),
            created_ms: crate::now_ms(),
        };
        write_private(&path, &serde_json::to_vec_pretty(&identity)?)?;
        Ok(identity)
    }

    /// The short code shown in Settings > About and quoted in bug reports:
    /// `D-` plus the first 40 bits of the install id in Crockford base32.
    #[must_use]
    pub fn support_id(&self) -> String {
        support_id(&self.install_id)
    }
}

/// See [`Identity::support_id`].
#[must_use]
pub fn support_id(install_id: &str) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let hex: String = install_id
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(10)
        .collect();
    let bits = u64::from_str_radix(&hex, 16).unwrap_or(0);
    let code: String = (0..8)
        .rev()
        .map(|i| ALPHABET[((bits >> (i * 5)) & 31) as usize] as char)
        .collect();
    format!("D-{code}")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Upload recorded diagnostics. Recording itself is always local.
    pub upload: bool,
    /// The name shown next to this install when investigating a report.
    /// `None` means "never set": the login name is used. `Some("")` means
    /// the user cleared it.
    pub name: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            upload: true,
            name: None,
        }
    }
}

impl Config {
    #[must_use]
    pub fn load(state_dir: &Path) -> Self {
        std::fs::read(telemetry_dir(state_dir).join(CONFIG_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, state_dir: &Path) -> std::io::Result<()> {
        write_private(
            &telemetry_dir(state_dir).join(CONFIG_FILE),
            &serde_json::to_vec_pretty(self)?,
        )
    }

    /// The name to send, bounded and stripped of control characters.
    #[must_use]
    pub fn effective_name(&self) -> Option<String> {
        let name = match &self.name {
            Some(name) => name.clone(),
            None => login_name()?,
        };
        let name: String = name
            .chars()
            .filter(|c| !c.is_control())
            .take(NAME_MAX_CHARS)
            .collect();
        let name = name.trim().to_owned();
        (!name.is_empty()).then_some(name)
    }
}

#[must_use]
pub fn telemetry_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("telemetry")
}

/// The login name from the user database (not `$USER`, which a launcher may
/// have rewritten).
#[must_use]
pub fn login_name() -> Option<String> {
    #[cfg(unix)]
    {
        // SAFETY: getpwuid returns a pointer into static storage or null; the
        // name is copied out before any other passwd call.
        unsafe {
            let entry = libc::getpwuid(libc::getuid());
            if !entry.is_null() && !(*entry).pw_name.is_null() {
                let name = std::ffi::CStr::from_ptr((*entry).pw_name);
                if let Ok(name) = name.to_str()
                    && !name.is_empty()
                {
                    return Some(name.to_owned());
                }
            }
        }
    }
    std::env::var("USER").ok().filter(|user| !user.is_empty())
}

pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent"))?;
    create_private_dir(dir)?;
    let temp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temp)?;
        std::io::Write::write_all(&mut file, bytes)?;
    }
    std::fs::rename(&temp, path)
}

pub(crate) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_created_once_and_reused() {
        let dir = tempfile::tempdir().unwrap();
        let first = Identity::load_or_create(dir.path()).unwrap();
        let second = Identity::load_or_create(dir.path()).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.install_id.len(), 36);
        assert!(first.support_id().starts_with("D-"));
        assert_eq!(first.support_id().len(), 10);
    }

    #[test]
    fn support_id_is_deterministic() {
        assert_eq!(
            support_id("00000000-0000-0000-0000-000000000000"),
            "D-00000000"
        );
        assert_eq!(
            support_id("ffffffff-ff00-0000-0000-000000000000"),
            "D-ZZZZZZZZ"
        );
    }

    #[test]
    fn config_defaults_to_upload_with_login_name() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(dir.path());
        assert!(config.upload);
        assert_eq!(config.effective_name(), login_name());

        let cleared = Config {
            upload: false,
            name: Some("  ".into()),
        };
        cleared.save(dir.path()).unwrap();
        let loaded = Config::load(dir.path());
        assert_eq!(loaded, cleared);
        assert_eq!(loaded.effective_name(), None);
    }
}
