//! macOS: each session's Holder in a launchd job of its own, so one
//! session's process coalition is never another's.
//!
//! macOS groups every process an app spawns into the app's coalition, and
//! children inherit it, so the Engine, the shared manager, every Holder, every
//! Agent, and anything an Agent starts (a Chrome launched by a browser MCP
//! server) all used to share one. LaunchServices treats a coalition as one
//! app's: force-quitting any member kills the rest ("killing coalition pid"
//! in `quitsupport`). On 2026-10-04 force-quitting such a Chrome from the
//! Dock killed 184 processes, every session included. loginwindow does the
//! same when diri.app dies with its bundle unreadable (see `install-local.sh`).
//!
//! A launchd job starts in a fresh coalition, which is the only way out
//! without private entitlements. The job runs diri's own main executable as a
//! trampoline that spawns the Holder and waits for it: TCC credits a process
//! to its *responsible* process, which for a launchd job is the job's program,
//! and while that is diri.app's main executable the Agents keep diri's privacy
//! grants (a job running `diri-holder` directly is credited to "diri-holder",
//! so every grant would be asked for again). The trampoline must outlive the
//! Holder; once it exits the attribution falls to the Holder.
//!
//! Jobs are transient: bootstrapped from a plist that is deleted at once, in
//! the `gui/<uid>` domain, gone at logout, never registered with Background
//! Task Management. `KeepAlive` is off, so launchd never restarts one.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::client::HolderClient;
use super::guard::SPEC_GUARD_FLAG;
use super::paths::HolderPaths;
use super::protocol::HolderLaunchSpec;
use super::{HolderError, HolderResult};

/// The argument that runs diri's main executable as a Holder trampoline;
/// diri-app implements the trampoline itself (`holder_trampoline.rs`).
pub const TRAMPOLINE_FLAG: &str = diri_proto::paths::HOLDER_TRAMPOLINE_FLAG;
/// Every job label starts with this; the rest is `<session>.<millis>`.
pub const LABEL_PREFIX: &str = "com.dirijor.diri.holder.";
/// Overrides the trampoline (a path), or disables launchd launches (`0`).
pub const TRAMPOLINE_ENV: &str = "DIRIJOR_HOLDER_TRAMPOLINE";

const LAUNCHCTL: &str = "/bin/launchctl";
/// How long to wait for a launched Holder: 250 readiness polls, about 5 s.
const READINESS_ATTEMPTS: usize = 250;
/// A loaded job that is not running and is older than this has finished.
/// Younger ones may still be between `bootstrap` and their spawn.
const STALE_AFTER: Duration = Duration::from_secs(120);

/// Why a launchd launch did not produce a Holder.
#[derive(Debug)]
pub enum LaunchdFailure {
    /// No Holder can be running for this spec: launching another way is safe.
    NotStarted(HolderError),
    /// A Holder may still be starting; launching another would double-run.
    Uncertain(HolderError),
}

/// The executable that runs as the trampoline: the [`TRAMPOLINE_ENV`]
/// override, or the main executable of the app bundle the running Engine
/// ships in. `None` (launch through the shared manager) outside a bundle:
/// development builds and tests.
pub fn trampoline_executable() -> Option<PathBuf> {
    if let Some(configured) = std::env::var_os(TRAMPOLINE_ENV) {
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
    let name = bundle_executable_name(&plist)?;
    let main = contents.join("MacOS").join(name);
    is_executable(&main).then_some(main)
}

/// The `CFBundleExecutable` of an XML `Info.plist`. A name with a path
/// separator is refused rather than followed.
fn bundle_executable_name(plist: &str) -> Option<&str> {
    let after_key = plist.split("<key>CFBundleExecutable</key>").nth(1)?;
    let value = after_key.trim_start().strip_prefix("<string>")?;
    let name = value.split("</string>").next()?.trim();
    (!name.is_empty() && !name.contains('/') && name != "." && name != "..").then_some(name)
}

/// Launches the Holder for `spec` as its own launchd job and waits until it
/// serves the session. Returns the Holder's pid.
pub fn launch(
    trampoline: &Path,
    holder_executable: &Path,
    paths: &HolderPaths,
    spec: &HolderLaunchSpec,
) -> Result<i32, LaunchdFailure> {
    let started = std::time::Instant::now();
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    let label = format!("{LABEL_PREFIX}{}.{millis}", label_safe(&spec.session_id));
    let spec_path = paths.directory.join(format!("{label}.spec.json"));
    let plist_path = paths.directory.join(format!("{label}.plist"));

    let spec_json = serde_json::to_vec(spec)
        .map_err(|error| LaunchdFailure::NotStarted(HolderError::Launch(error.to_string())))?;
    write_private(&spec_path, &spec_json).map_err(LaunchdFailure::NotStarted)?;

    let mut program = vec![
        trampoline.to_string_lossy().into_owned(),
        TRAMPOLINE_FLAG.to_owned(),
        holder_executable.to_string_lossy().into_owned(),
        "--spec".to_owned(),
        spec_path.to_string_lossy().into_owned(),
        SPEC_GUARD_FLAG.to_owned(),
    ];
    if let Some(state_dir) = crate::telemetry::holder_state_dir() {
        program.push(crate::telemetry::HOLDER_TELEMETRY_FLAG.to_owned());
        program.push(state_dir.to_string_lossy().into_owned());
    }
    let bootstrapped = write_private(&plist_path, job_plist(&label, &program).as_bytes())
        .and_then(|()| launchctl(&["bootstrap", &gui_domain(), &path_str(&plist_path)]));
    // launchd has read the plist (or never will); it must not linger.
    let _ = std::fs::remove_file(&plist_path);
    if let Err(error) = bootstrapped {
        let _ = std::fs::remove_file(&spec_path);
        return Err(LaunchdFailure::NotStarted(error));
    }

    let client = HolderClient::new(paths.socket());
    let mut ready = wait_until(&client, READINESS_ATTEMPTS);
    if !ready {
        // The Holder deletes its spec once it has read it. Still there means
        // it never started: take the spec back so it never can, and let the
        // caller launch another way.
        if std::fs::remove_file(&spec_path).is_ok() {
            let _ = launchctl(&["bootout", &format!("{}/{label}", gui_domain())]);
            return Err(LaunchdFailure::NotStarted(HolderError::Launch(
                "launchd Holder did not start".into(),
            )));
        }
        ready = wait_until(&client, READINESS_ATTEMPTS);
    }
    if !ready {
        return Err(LaunchdFailure::Uncertain(HolderError::Launch(
            "launchd Holder took its spec but does not answer".into(),
        )));
    }
    let pid = read_pid(&paths.pid_file()).ok_or_else(|| {
        LaunchdFailure::Uncertain(HolderError::Launch(
            "launchd Holder answers without a pid file".into(),
        ))
    })?;
    diri_telemetry::event!(
        "holder.launchd_launch",
        session = diri_telemetry::id(&spec.session_id),
        ms = started.elapsed(),
    );
    // Finished jobs stay loaded (not running) until booted out. Sweeping
    // here, off the launch path, keeps them from piling up.
    let _ = std::thread::Builder::new()
        .name("holder-job-sweep".into())
        .spawn(sweep_finished_jobs);
    Ok(pid)
}

fn wait_until(client: &HolderClient, attempts: usize) -> bool {
    super::readiness_delays().take(attempts).any(|delay| {
        if client.is_alive() {
            return true;
        }
        std::thread::sleep(delay);
        false
    })
}

/// Boots out Holder jobs that ran and finished. A running job is never
/// touched, nor one young enough to be between `bootstrap` and its spawn.
pub fn sweep_finished_jobs() {
    let Ok(output) = Command::new(LAUNCHCTL)
        .arg("list")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    for label in finished_jobs(&String::from_utf8_lossy(&output.stdout), now) {
        let _ = launchctl(&["bootout", &format!("{}/{label}", gui_domain())]);
    }
}

/// The labels in `launchctl list` output (`PID\tStatus\tLabel` rows) that
/// are Holder jobs, not running, and older than [`STALE_AFTER`].
fn finished_jobs(list: &str, now_millis: u128) -> Vec<&str> {
    list.lines()
        .filter_map(|line| {
            let mut columns = line.split('\t');
            let pid = columns.next()?;
            let _status = columns.next()?;
            let label = columns.next()?.trim();
            let suffix = label.strip_prefix(LABEL_PREFIX)?;
            let millis: u128 = suffix.rsplit_once('.')?.1.parse().ok()?;
            let old = now_millis.saturating_sub(millis) > STALE_AFTER.as_millis();
            (pid.trim() == "-" && old).then_some(label)
        })
        .collect()
}

fn job_plist(label: &str, program: &[String]) -> String {
    let arguments: String = program
        .iter()
        .map(|argument| format!("    <string>{}</string>\n", xml_escape(argument)))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
{arguments}  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <false/>
  <key>AbandonProcessGroup</key>
  <true/>
  <key>ProcessType</key>
  <string>Interactive</string>
</dict>
</plist>
"#,
        label = xml_escape(label),
    )
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A launchd label component: session ids are `s_<hex>`, but nothing else
/// may reach the label.
fn label_safe(session_id: &str) -> String {
    session_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn gui_domain() -> String {
    // SAFETY: getuid has no failure mode.
    format!("gui/{}", unsafe { libc::getuid() })
}

fn launchctl(arguments: &[&str]) -> HolderResult<()> {
    let output = Command::new(LAUNCHCTL)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| HolderError::io("run launchctl", error))?;
    if output.status.success() {
        return Ok(());
    }
    Err(HolderError::Launch(format!(
        "launchctl {}: {} {}",
        arguments.first().copied().unwrap_or_default(),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

fn write_private(path: &Path, contents: &[u8]) -> HolderResult<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| HolderError::io("create launchd file", error))?;
    file.write_all(contents)
        .map_err(|error| HolderError::io("write launchd file", error))
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn read_pid(path: &Path) -> Option<i32> {
    let pid = std::fs::read_to_string(path).ok()?.trim().parse().ok()?;
    (pid > 1).then_some(pid)
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
    fn the_trampoline_is_the_bundle_main_executable() {
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
        // Outside a bundle (a dev or test build) there is no trampoline.
        assert_eq!(
            bundle_main_executable(&root.path().join("x/dirijord-rs")),
            None
        );
        assert_eq!(
            bundle_executable_name("<key>CFBundleExecutable</key><string>../evil</string>"),
            None
        );
    }

    #[test]
    fn only_finished_old_holder_jobs_are_swept() {
        let now = 1_000_000_000u128;
        let old = now - 600_000;
        let young = now - 1_000;
        let list = format!(
            "PID\tStatus\tLabel\n\
             -\t0\t{LABEL_PREFIX}s_aaa.{old}\n\
             4242\t0\t{LABEL_PREFIX}s_bbb.{old}\n\
             -\t0\t{LABEL_PREFIX}s_ccc.{young}\n\
             -\t-15\t{LABEL_PREFIX}s_ddd.{old}\n\
             -\t0\tcom.apple.something.{old}\n\
             -\t0\t{LABEL_PREFIX}garbage\n"
        );
        assert_eq!(
            finished_jobs(&list, now),
            vec![
                format!("{LABEL_PREFIX}s_aaa.{old}"),
                format!("{LABEL_PREFIX}s_ddd.{old}")
            ]
        );
    }

    #[test]
    fn the_job_never_restarts_and_escapes_its_arguments() {
        let plist = job_plist(
            "com.dirijor.diri.holder.s_1.2",
            &["/A & B/diri".to_owned(), "<x>".to_owned()],
        );
        assert!(plist.contains("<key>KeepAlive</key>\n  <false/>"));
        assert!(plist.contains("<key>AbandonProcessGroup</key>\n  <true/>"));
        assert!(plist.contains("<string>/A &amp; B/diri</string>"));
        assert!(plist.contains("<string>&lt;x&gt;</string>"));
        assert_eq!(label_safe("s_ab/../c d"), "s_ab____c_d");
    }
}
