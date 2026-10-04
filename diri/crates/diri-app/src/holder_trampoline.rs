//! diri.app's main executable as a session Holder's launchd trampoline.
//!
//! The Engine runs each session's Holder as a launchd job of its own so no
//! session shares a process coalition with another (see the Engine's
//! `holder::launchd`). The job's program is this executable, not the Holder:
//! TCC credits a process to its launchd job's program, and while that is
//! diri.app the Agents keep diri's privacy grants. So it spawns the Holder,
//! stays its parent until it exits, and exits 0 (the job never restarts).

use std::process::{Command, Stdio};

/// Never returns when started as a trampoline. Call first thing in `main`,
/// before anything touches the window server.
pub fn run_if_requested() {
    let mut arguments = std::env::args_os().skip(1);
    if arguments
        .next()
        .is_none_or(|flag| flag != diri_proto::paths::HOLDER_TRAMPOLINE_FLAG)
    {
        return;
    }
    if let Some(holder) = arguments.next()
        && let Err(error) = Command::new(holder)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    {
        eprintln!("diri: holder trampoline could not start the Holder: {error}");
    }
    std::process::exit(0);
}
