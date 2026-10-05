//! Opt-in (macOS, real launchd): a manager-hosted Agent started as a launchd
//! job of its own. It gets a process coalition no other session shares, so
//! the SIGTERM sweep LaunchServices sends a force-quit app's coalition
//! (2026-10-04: an Agent's Chrome, force-quit from the Dock, took every
//! session) ends only that session. It still has the PTY as its controlling
//! terminal, and the manager still learns its exact exit.
//!
//! Needs the launcher: any build of the `diri` binary.
//!
//! ```sh
//! cargo build -p diri-app --bin diri
//! DIRI_AGENT_LAUNCHER=$PWD/target/debug/diri \
//!   cargo test -p diri-engine --test detached_agent -- --ignored
//! ```
//!
//! Each launch creates a transient `gui/<uid>` job that is booted out at
//! once; the fixture sessions are ended and the manager idles out after one
//! second, so nothing outlives the test.
#![cfg(target_os = "macos")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use diri_engine::OutputLog;
use diri_engine::holder::agent_launcher::AGENT_LAUNCHER_ENV;
use diri_engine::holder::protocol::DEFAULT_DISK_CAPACITY;
use diri_engine::holder::{
    HolderClient, HolderExitMarker, HolderLaunchSpec, HolderLauncher, HolderPaths,
};

/// The jetsam coalition id: what LaunchServices sweeps as one app.
fn coalition(pid: i32) -> u64 {
    try_coalition(pid).unwrap_or_else(|| panic!("no coalition for pid {pid}"))
}

/// `None` for a process this user may not inspect, or one already gone.
fn try_coalition(pid: i32) -> Option<u64> {
    #[repr(C)]
    #[derive(Default)]
    struct Info {
        ids: [u64; 2],
        reserved: [u64; 3],
    }
    let mut info = Info::default();
    // SAFETY: the buffer is exactly the kernel's `proc_pidcoalitioninfo`.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            20, // PROC_PIDCOALITIONINFO
            0,
            (&mut info as *mut Info).cast(),
            std::mem::size_of::<Info>() as i32,
        )
    };
    (written as usize == std::mem::size_of::<Info>()).then_some(info.ids[1])
}

fn all_pids() -> Vec<i32> {
    let mut pids = vec![0i32; 16_384];
    // SAFETY: the buffer is writable for its whole byte length.
    let count = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr().cast(),
            (pids.len() * std::mem::size_of::<i32>()) as i32,
        )
    };
    pids.truncate(count.max(0) as usize);
    pids
}

fn ps(pid: i32, field: &str) -> String {
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", &format!("{field}="), "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn wait_until(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn log_bytes(logs: &Path, session_id: &str) -> Vec<u8> {
    let mut log = OutputLog::reader(logs, session_id).expect("open log");
    log.refresh_from_disk();
    let tail = log.tail_offset();
    log.read(0, tail as usize).1
}

fn exit_of(logs: &Path, session_id: &str) -> Option<(Option<i32>, Option<i32>)> {
    let mut buffer = log_bytes(logs, session_id);
    let (_, exit) = HolderExitMarker::drain(&mut buffer);
    exit.map(|exit| (exit.code, exit.signal))
}

fn spec(paths: &HolderPaths, logs: &Path, argv: &[&str]) -> HolderLaunchSpec {
    HolderLaunchSpec {
        session_id: paths.session_id.clone(),
        socket_path: paths.socket().to_string_lossy().into_owned(),
        pid_file_path: paths.pid_file().to_string_lossy().into_owned(),
        log_file_path: logs
            .join(format!("{}.bin", paths.session_id))
            .to_string_lossy()
            .into_owned(),
        argv: argv.iter().map(|word| word.to_string()).collect(),
        cwd: "/tmp".into(),
        environment: HashMap::from([
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("HOME".to_string(), "/tmp".to_string()),
            ("PS1".to_string(), "$ ".to_string()),
        ]),
        cols: 80,
        rows: 24,
        disk_capacity: DEFAULT_DISK_CAPACITY,
    }
}

#[test]
#[ignore = "starts real launchd jobs; set DIRI_AGENT_LAUNCHER"]
fn a_force_quit_sweep_ends_only_its_own_session() {
    let launcher =
        PathBuf::from(std::env::var_os("DIRI_AGENT_LAUNCHER").expect("DIRI_AGENT_LAUNCHER"))
            .canonicalize()
            .unwrap();
    // SAFETY: set before any launch; this file holds a single test.
    unsafe {
        std::env::set_var(AGENT_LAUNCHER_ENV, &launcher);
        std::env::set_var("DIRI_HOLDER_IDLE_SECONDS", "1");
    }
    let holder = PathBuf::from(env!("CARGO_BIN_EXE_diri-holder"));
    // Socket paths must stay under SUN_LEN.
    let root = tempfile::Builder::new()
        .prefix("dda-")
        .tempdir_in("/tmp")
        .unwrap();
    let directory = root.path().join("h");
    let logs = root.path().join("logs");
    std::fs::create_dir_all(&logs).unwrap();

    let launch = |id: &str, argv: &[&str]| {
        let paths = HolderPaths::new(&directory, id);
        let manager =
            HolderLauncher::launch(&holder, &paths, &spec(&paths, &logs, argv)).expect("launch");
        let client = HolderClient::new(paths.socket());
        wait_until("session ready", || client.is_alive());
        let agent = client.stat().expect("stat").child_pid;
        (client, agent, manager)
    };
    let (victim, victim_agent, manager) = launch("s_victim", &["/bin/sleep", "300"]);
    let (shell, shell_agent, _) = launch("s_shell", &["/bin/sh"]);

    // SAFETY: getpid has no failure mode.
    let ours = coalition(unsafe { libc::getpid() });
    let victim_coalition = coalition(victim_agent);
    assert_ne!(
        victim_coalition, ours,
        "the Agent left the Holder's coalition"
    );
    assert_ne!(coalition(shell_agent), ours);
    assert_ne!(
        coalition(shell_agent),
        victim_coalition,
        "no two sessions share one"
    );
    // launchd's child, leading its own session on the PTY.
    assert_eq!(ps(shell_agent, "ppid"), "1");
    assert_eq!(ps(shell_agent, "pgid"), shell_agent.to_string());
    assert_ne!(
        ps(shell_agent, "tty"),
        "??",
        "the PTY is its controlling terminal"
    );

    // What a Dock force-quit does to the victim's coalition: SIGTERM to every
    // member. Refuse outright if that could reach anything of ours.
    let sweep: Vec<i32> = all_pids()
        .into_iter()
        .filter(|&pid| pid > 1 && try_coalition(pid) == Some(victim_coalition))
        .collect();
    assert!(sweep.contains(&victim_agent));
    assert!(!sweep.contains(&shell_agent));
    // SAFETY: getpid has no failure mode.
    assert!(!sweep.contains(&unsafe { libc::getpid() }));
    for pid in &sweep {
        // SAFETY: integer arguments only.
        unsafe { libc::kill(*pid, libc::SIGTERM) };
    }
    wait_until("victim exit marker", || {
        exit_of(&logs, "s_victim").is_some()
    });
    assert_eq!(
        exit_of(&logs, "s_victim"),
        Some((None, Some(libc::SIGTERM)))
    );
    assert!(!victim.is_alive() || exit_of(&logs, "s_victim").is_some());
    assert!(shell.is_alive(), "the other session survives the sweep");
    // SAFETY: signal 0 only probes.
    assert_eq!(
        unsafe { libc::kill(manager, 0) },
        0,
        "and so does the manager"
    );

    // Job control works: Ctrl-C reaches the foreground job through the line
    // discipline, which needs the PTY to be the controlling terminal.
    shell.write(b"sleep 30\n").unwrap();
    wait_until("sleep running", || {
        all_pids()
            .into_iter()
            .any(|pid| ps(pid, "ppid") == shell_agent.to_string())
    });
    shell.write(b"\x03").unwrap();
    shell.write(b"echo after-int\n").unwrap();
    wait_until("prompt back after Ctrl-C", || {
        String::from_utf8_lossy(&log_bytes(&logs, "s_shell")).contains("\nafter-int")
    });

    // The exact status of an Agent the manager did not fork.
    shell.write(b"exit 42\n").unwrap();
    wait_until("shell exit marker", || exit_of(&logs, "s_shell").is_some());
    assert_eq!(exit_of(&logs, "s_shell"), Some((Some(42), None)));

    // The manager idles out a second after its last session; then its
    // finished job is swept, and no helper job was ever left loaded.
    wait_until("manager idle exit", || {
        // SAFETY: signal 0 only probes.
        unsafe { libc::kill(manager, 0) != 0 }
    });
    diri_pty::detached::sweep_finished_jobs("com.dirijor.diri.holders.", Duration::ZERO);
    let list = std::process::Command::new("/bin/launchctl")
        .arg("list")
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&list.stdout).contains("com.dirijor.diri.agent."),
        "a helper job was left loaded"
    );
    assert!(
        !String::from_utf8_lossy(&list.stdout)
            .contains(&format!("{manager}\tcom.dirijor.diri.holders.")),
        "the manager job was left loaded"
    );
}
