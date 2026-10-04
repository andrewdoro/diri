//! Opt-in (macOS, real launchd): each session's Holder runs in a launchd job
//! of its own, so it shares no process coalition with the Engine or with
//! another session. A coalition is what macOS kills as one when any member is
//! force-quit (2026-10-04: force-quitting an Agent's Chrome ended every
//! session).
//!
//! Needs a trampoline: diri.app's main executable, or any build of the
//! `diri` binary (`cargo build -p diri-app --bin diri`):
//!
//! ```sh
//! DIRI_LAUNCHD_TRAMPOLINE=/abs/target/debug/diri \
//!   cargo test -p diri-engine --test launchd_holder -- --ignored
//! ```
//!
//! Creates two transient `gui/<uid>` jobs labelled
//! `com.dirijor.diri.holder.s_launchd_test_*`, kills their fixture sessions,
//! and boots the jobs out; nothing outlives the test.
#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use diri_engine::holder::launchd::{LABEL_PREFIX, TRAMPOLINE_ENV};
use diri_engine::holder::{HolderClient, HolderLaunchSpec, HolderLauncher, HolderPaths};

/// `[resource, jetsam]` coalition ids (`PROC_PIDCOALITIONINFO`, private but
/// stable in XNU's `proc_info.h`).
fn coalitions(pid: i32) -> [u64; 2] {
    #[repr(C)]
    #[derive(Default)]
    struct Info {
        ids: [u64; 2],
        reserved: [u64; 3],
    }
    let mut info = Info::default();
    // SAFETY: the buffer is exactly the kernel's struct size.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            20,
            0,
            (&mut info as *mut Info).cast(),
            std::mem::size_of::<Info>() as i32,
        )
    };
    assert_eq!(written as usize, std::mem::size_of::<Info>(), "pid {pid}");
    info.ids
}

fn parent(pid: i32) -> i32 {
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().parse().unwrap()
}

fn executable(pid: i32) -> PathBuf {
    let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is the documented maximum size.
    let length = unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    assert!(length > 0, "pid {pid}");
    buffer.truncate(length as usize);
    PathBuf::from(String::from_utf8(buffer).unwrap())
}

fn wait_until(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn spec(paths: &HolderPaths, logs: &Path) -> HolderLaunchSpec {
    HolderLaunchSpec {
        session_id: paths.session_id.clone(),
        socket_path: paths.socket().to_string_lossy().into_owned(),
        pid_file_path: paths.pid_file().to_string_lossy().into_owned(),
        log_file_path: logs
            .join(format!("{}.bin", paths.session_id))
            .to_string_lossy()
            .into_owned(),
        argv: vec!["/bin/sleep".into(), "120".into()],
        cwd: "/tmp".into(),
        environment: [("PATH".to_string(), "/usr/bin:/bin".to_string())].into(),
        cols: 80,
        rows: 24,
        disk_capacity: 1 << 20,
    }
}

#[test]
#[ignore = "launches real launchd jobs; set DIRI_LAUNCHD_TRAMPOLINE"]
fn each_session_holder_gets_a_coalition_of_its_own() {
    let trampoline = PathBuf::from(
        std::env::var_os("DIRI_LAUNCHD_TRAMPOLINE").expect("DIRI_LAUNCHD_TRAMPOLINE"),
    )
    .canonicalize()
    .unwrap();
    // SAFETY: set before any launch; this file holds a single test.
    unsafe { std::env::set_var(TRAMPOLINE_ENV, &trampoline) };
    let holder = PathBuf::from(env!("CARGO_BIN_EXE_diri-holder"));
    // Socket paths must stay under SUN_LEN.
    let root = tempfile::Builder::new()
        .prefix("dlh-")
        .tempdir_in("/tmp")
        .unwrap();
    let directory = root.path().join("h");
    let logs = root.path().join("logs");
    std::fs::create_dir_all(&logs).unwrap();

    let sessions: Vec<_> = ["s_launchd_test_a", "s_launchd_test_b"]
        .iter()
        .map(|id| {
            let paths = HolderPaths::new(&directory, id);
            let pid = HolderLauncher::launch(&holder, &paths, &spec(&paths, &logs))
                .expect("launchd launch");
            let client = HolderClient::new(paths.socket());
            let child = client.stat().expect("stat").child_pid;
            (paths, client, pid, child)
        })
        .collect();

    // SAFETY: getpid has no failure mode.
    let engine = coalitions(unsafe { libc::getpid() });
    let (_, _, holder_a, child_a) = &sessions[0];
    let (_, _, holder_b, child_b) = &sessions[1];
    let a = coalitions(*holder_a);
    let b = coalitions(*holder_b);
    assert_ne!(a[1], engine[1], "Holder A escaped the Engine's coalition");
    assert_ne!(b[1], engine[1], "Holder B escaped the Engine's coalition");
    assert_ne!(a[1], b[1], "the two sessions share nothing");
    assert_eq!(coalitions(*child_a), a, "the Agent stays with its Holder");
    assert_eq!(coalitions(*child_b), b);
    // The trampoline stays the Holder's parent: TCC credits the Agents to it.
    for holder_pid in [holder_a, holder_b] {
        let trampoline_pid = parent(*holder_pid);
        assert_eq!(executable(trampoline_pid), trampoline);
        assert_eq!(executable(*holder_pid), holder.canonicalize().unwrap());
    }

    for (paths, client, holder_pid, _) in &sessions {
        client.kill_tree().expect("kill tree");
        wait_until("holder exit", || {
            // SAFETY: signal 0 only probes.
            unsafe { libc::kill(*holder_pid, 0) != 0 }
        });
        assert!(!paths.socket().exists() || !client.is_alive());
    }
    // Both jobs finished; boot them out (the Engine's sweep waits for age).
    let list = std::process::Command::new("/bin/launchctl")
        .arg("list")
        .output()
        .unwrap();
    let list = String::from_utf8_lossy(&list.stdout);
    // SAFETY: getuid has no failure mode.
    let uid = unsafe { libc::getuid() };
    let mut booted = 0;
    for line in list.lines() {
        let mut columns = line.split('\t');
        let (Some(pid), Some(_), Some(label)) = (columns.next(), columns.next(), columns.next())
        else {
            continue;
        };
        if label.starts_with(&format!("{LABEL_PREFIX}s_launchd_test_")) {
            assert_eq!(pid, "-", "{label} still running after its Holder exited");
            let status = std::process::Command::new("/bin/launchctl")
                .args(["bootout", &format!("gui/{uid}/{label}")])
                .status()
                .unwrap();
            assert!(status.success());
            booted += 1;
        }
    }
    assert_eq!(booted, 2);
}
