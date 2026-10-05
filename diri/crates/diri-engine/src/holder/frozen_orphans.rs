//! Agents left stopped and parentless after their Holder died.
//!
//! The Governor hibernates an idle session by stopping its tree, including
//! members that left the session's process group (a Codex node wrapper, a
//! Chrome DevTools MCP watchdog). If the Holder is then killed from outside,
//! as macOS does to a whole coalition at a force-quit or when diri.app dies
//! with its bundle unreadable, the SIGTERM those stopped processes get stays
//! pending forever: nothing continues them, launchd adopts them, and they sit
//! stopped until reboot. Three such Codex wrappers had survived since
//! 2026-09-23 on the owner's Mac.
//!
//! The sweep finds them by what diri gave them: a stopped process parented
//! to launchd, whose environment names this Engine's socket and a session
//! that is no longer live. A live session's processes, hibernated or not,
//! are never touched; neither is anything whose environment cannot be read
//! (another user's, or an Apple platform binary's, which macOS withholds).
//! It runs once at startup and after a session ends interrupted, never while
//! idle.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use super::process_tree;

const SOCKET_ENV: &str = "DIRIJOR_SOCKET";
const SESSION_ENV: &str = "DIRIJOR_SESSION_ID";

static SOCKET: OnceLock<PathBuf> = OnceLock::new();
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Names the socket this Engine's Agents carry in `DIRIJOR_SOCKET`.
pub fn configure(socket: PathBuf) {
    let _ = SOCKET.set(socket);
}

/// Sweeps off the caller's thread; a sweep already running absorbs this one.
pub fn request_sweep(registry: Arc<Mutex<crate::registry::Registry>>) {
    let Some(socket) = SOCKET.get().cloned() else {
        return;
    };
    if RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("diri-frozen-orphans".into())
        .spawn(move || {
            let killed = sweep(&socket.to_string_lossy(), |session| {
                registry
                    .lock()
                    .map_or(true, |registry| registry.is_live(session))
            });
            if killed > 0 {
                eprintln!("diri-engine: killed {killed} stopped orphan(s) of ended sessions");
                diri_telemetry::event!("engine.frozen_orphans", killed = killed);
            }
            RUNNING.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        RUNNING.store(false, Ordering::Release);
    }
}

/// Kills every stopped, launchd-parented process whose environment names
/// `socket` and a session `is_live` denies. Returns how many.
pub fn sweep(socket: &str, is_live: impl Fn(&str) -> bool) -> usize {
    let mut killed = 0;
    for sample in process_tree::stopped_orphans() {
        let Ok(values) =
            diri_pty::foreground::environment_values(sample.pid as u32, &[SOCKET_ENV, SESSION_ENV])
        else {
            continue;
        };
        let [Some(owner), Some(session)] = values.as_slice() else {
            continue;
        };
        if owner != socket || is_live(session) {
            continue;
        }
        if process_tree::kill_verified(&sample) {
            killed += 1;
        }
    }
    killed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{Duration, Instant};

    /// A stopped, launchd-parented process carrying `socket` and `session`,
    /// made by double-forking a non-platform executable (this test binary).
    fn orphan(socket: &str, session: &str) -> i32 {
        let script = format!(
            "{} --ignored --exact holder::frozen_orphans::tests::sleeper >/dev/null 2>&1 & echo $!",
            std::env::current_exe().unwrap().display()
        );
        let output = Command::new("/bin/sh")
            .args(["-c", &script])
            .env_clear()
            .env(SOCKET_ENV, socket)
            .env(SESSION_ENV, session)
            .output()
            .unwrap();
        let pid: i32 = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .unwrap();
        // Parented to launchd once the shell has exited; then stop it.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !process_tree::stopped_orphans().iter().any(|s| s.pid == pid) {
            // SAFETY: signalling a process this test created.
            unsafe { libc::kill(pid, libc::SIGSTOP) };
            assert!(Instant::now() < deadline, "orphan {pid} never stopped");
            std::thread::sleep(Duration::from_millis(20));
        }
        pid
    }

    #[test]
    #[ignore = "a helper process, not a test"]
    fn sleeper() {
        std::thread::sleep(Duration::from_secs(30));
    }

    fn alive(pid: i32) -> bool {
        // SAFETY: signal 0 only probes.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[test]
    fn only_stopped_orphans_of_ended_sessions_of_this_engine_die() {
        let socket = format!("/tmp/frozen-orphans-{}.sock", std::process::id());
        let ended = orphan(&socket, "s_ended");
        let live = orphan(&socket, "s_live");
        let other_engine = orphan("/tmp/someone-else.sock", "s_ended");

        let killed = sweep(&socket, |session| session == "s_live");
        assert_eq!(killed, 1);
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(ended) {
            assert!(
                Instant::now() < deadline,
                "the ended session's orphan survived"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(alive(live), "a live (hibernated) session is never touched");
        assert!(
            alive(other_engine),
            "another Engine's processes are never touched"
        );
        for pid in [live, other_engine] {
            // SAFETY: cleaning up processes this test created.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGCONT);
            }
        }
    }
}
