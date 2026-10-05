//! Which executable starts hosted Agents as launchd jobs of their own (macOS).
//!
//! It has to be diri.app's main executable: TCC credits a launchd job's
//! processes to the job's program, through its fork and its exec, so with
//! the app's own executable as the program the Agents keep diri's privacy
//! grants (a job running anything else is asked for every one again). See
//! [`diri_pty::detached`] for the launch itself.

use std::path::{Path, PathBuf};

/// The manager argument naming the launcher: `--agent-launcher <path>`.
pub const AGENT_LAUNCHER_FLAG: &str = "--agent-launcher";
/// Overrides the launcher (a path), or turns detached launches off (`0`).
pub const AGENT_LAUNCHER_ENV: &str = "DIRIJOR_AGENT_LAUNCHER";

/// The [`AGENT_LAUNCHER_ENV`] override, or the main executable of the app
/// bundle the running Engine ships in. `None` outside a bundle (development
/// builds, tests) and off macOS: Agents are then the Holder's children.
pub fn agent_launcher() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    if let Some(configured) = std::env::var_os(AGENT_LAUNCHER_ENV) {
        if configured == "0" || configured.is_empty() {
            return None;
        }
        return Some(PathBuf::from(configured)).filter(|path| is_executable(path));
    }
    let engine = std::env::current_exe().ok()?.canonicalize().ok()?;
    bundle_main_executable(&engine)
}

/// `<bundle>/Contents/MacOS/<CFBundleExecutable>` for an executable at
/// `<bundle>/Contents/Resources/bin/<name>`.
fn bundle_main_executable(engine: &Path) -> Option<PathBuf> {
    let bin = engine.parent()?;
    let resources = bin.parent()?;
    let contents = resources.parent()?;
    if bin.file_name()? != "bin"
        || resources.file_name()? != "Resources"
        || contents.file_name()? != "Contents"
    {
        return None;
    }
    let plist = std::fs::read_to_string(contents.join("Info.plist")).ok()?;
    let main = contents.join("MacOS").join(bundle_executable_name(&plist)?);
    is_executable(&main).then_some(main)
}

/// The `CFBundleExecutable` of an XML `Info.plist`; a name with a path
/// separator is refused rather than followed.
fn bundle_executable_name(plist: &str) -> Option<&str> {
    let after_key = plist.split("<key>CFBundleExecutable</key>").nth(1)?;
    let value = after_key.trim_start().strip_prefix("<string>")?;
    let name = value.split("</string>").next()?.trim();
    (!name.is_empty() && !name.contains('/') && name != "." && name != "..").then_some(name)
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launcher_is_the_bundle_main_executable() {
        let root = tempfile::tempdir().unwrap();
        let contents = root.path().join("diri.app/Contents");
        let bin = contents.join("Resources/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(contents.join("MacOS")).unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            "<dict>\n\t<key>CFBundleExecutable</key>\n\t<string>diri</string>\n</dict>",
        )
        .unwrap();
        let main = contents.join("MacOS/diri");
        std::fs::write(&main, "").unwrap();
        let engine = bin.join("dirijord-rs");
        assert_eq!(bundle_main_executable(&engine), None, "not executable yet");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&main, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(bundle_main_executable(&engine), Some(main));
        // Outside a bundle (a dev or test build) there is none.
        assert_eq!(
            bundle_main_executable(&root.path().join("x/dirijord-rs")),
            None
        );
        assert_eq!(
            bundle_executable_name("<key>CFBundleExecutable</key><string>../evil</string>"),
            None
        );
    }
}
