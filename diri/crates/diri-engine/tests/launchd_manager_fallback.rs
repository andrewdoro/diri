//! Opt-in (macOS, real launchd): a Holder manager launchd accepts but never
//! runs must not fail every session.
//!
//! 0.9.3 started the manager as a launchd job and, when `launchctl
//! bootstrap` succeeded but the manager never answered, failed every new
//! session after 5 s with exit 127, on every launch, until the app was
//! reinstalled. Here the manager executable is a wrapper that exits 127 when
//! launchd runs it as a manager job and serves normally when spawned
//! directly, so only the fallback can make the launch succeed.
//!
//! ```sh
//! cargo test -p diri-engine --test launchd_manager_fallback -- --ignored
//! ```
//!
//! The failing job is booted out by the code under test; the session is
//! ended and the manager idles out after one second.
#![cfg(target_os = "macos")]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;

use std::time::{Duration, Instant};

use diri_engine::holder::agent_launcher::AGENT_LAUNCHER_ENV;
use diri_engine::holder::protocol::DEFAULT_DISK_CAPACITY;
use diri_engine::holder::{
    HolderClient, HolderLaunchSpec, HolderLauncher, HolderManagerPaths, HolderPaths,
};

#[test]
#[ignore = "real launchd: creates a transient gui/<uid> job"]
fn a_manager_job_launchd_never_runs_falls_back_to_a_direct_spawn() {
    let root = std::env::temp_dir().join(format!("diri-lmf-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("holders dir");
    let logs = root.join("logs");

    let holder = env!("CARGO_BIN_EXE_diri-holder");
    let wrapper = root.join("diri-holder");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n\
             case \"$XPC_SERVICE_NAME\" in com.dirijor.diri.holders.*) exit 127;; esac\n\
             exec '{holder}' \"$@\"\n"
        ),
    )
    .expect("wrapper");
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    // Any executable turns the launchd path on; the Agent launch through it
    // falls back to a direct spawn, which this test does not wait for.
    // SAFETY: set before any launch; this file holds one test.
    unsafe {
        std::env::set_var(AGENT_LAUNCHER_ENV, "/usr/bin/true");
        std::env::set_var("DIRI_HOLDER_IDLE_SECONDS", "1");
    }

    let paths = HolderPaths::new(&root, "s_lmf");
    let spec = HolderLaunchSpec {
        session_id: paths.session_id.clone(),
        socket_path: paths.socket().to_string_lossy().into_owned(),
        pid_file_path: paths.pid_file().to_string_lossy().into_owned(),
        log_file_path: logs.join("s_lmf.bin").to_string_lossy().into_owned(),
        argv: vec!["/bin/cat".into()],
        cwd: "/tmp".into(),
        environment: HashMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]),
        cols: 80,
        rows: 24,
        disk_capacity: DEFAULT_DISK_CAPACITY,
    };

    let started = Instant::now();
    let manager_pid = HolderLauncher::launch(&wrapper, &paths, &spec)
        .expect("the launch survives a manager job that never answers");
    assert!(manager_pid > 1);
    eprintln!("fell back after {:?}", started.elapsed());

    // The next launch goes straight to the direct manager, with no wait.
    let paths_2 = HolderPaths::new(&root, "s_lmf2");
    let spec_2 = HolderLaunchSpec {
        session_id: paths_2.session_id.clone(),
        socket_path: paths_2.socket().to_string_lossy().into_owned(),
        pid_file_path: paths_2.pid_file().to_string_lossy().into_owned(),
        log_file_path: logs.join("s_lmf2.bin").to_string_lossy().into_owned(),
        ..spec.clone()
    };
    let again = Instant::now();
    assert_eq!(
        HolderLauncher::launch(&wrapper, &paths_2, &spec_2).expect("second launch"),
        manager_pid
    );
    assert!(again.elapsed() < Duration::from_secs(1));
    // Later Engines and managers read it and leave launchd alone too.
    let manager_dir = HolderManagerPaths::new(&root).directory;
    assert!(manager_dir.join("launchd-unavailable").exists());

    for paths in [&paths, &paths_2] {
        let client = HolderClient::new(paths.socket());
        let deadline = Instant::now() + Duration::from_secs(20);
        while !client.is_alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = client.kill_tree();
    }
}
