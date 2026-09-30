//! What a terminal's foreground job is, in the words a user would use.
//!
//! A shell session's tab is named after the program in its foreground, and an
//! Agent started by hand inside it is recognised by that same name. Both read
//! the argument vector of the foreground group's leader, which is also where
//! passwords and tokens typed on a command line live, so nothing past the
//! program's own name ever leaves this module.

use std::io;

/// The program name of `pid`: `vim`, `claude`, `npm`, never its arguments.
///
/// Scripts are named after the script rather than the interpreter running
/// them, so `node /opt/homebrew/bin/codex` is `codex` and `python3 -m http.server`
/// stays `python3`.
pub fn program_name(pid: u32) -> io::Result<String> {
    let (first, second) = leading_arguments(pid)?;
    name_from_arguments(&first, second.as_deref())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unnamed foreground program"))
}

/// The live working directory of `pid`, as the kernel reports it.
pub fn working_directory(pid: u32) -> io::Result<String> {
    crate::process_facts::working_directory(pid)
}

/// Interpreters whose first operand names the program a user started.
const INTERPRETERS: &[&str] = &[
    "node", "nodejs", "bun", "deno", "python", "python2", "python3", "ruby", "perl", "sh", "bash",
    "dash", "zsh", "env",
];

/// Extensions a script's file name carries but its command does not.
const SCRIPT_EXTENSIONS: &[&str] = &[".js", ".mjs", ".cjs", ".ts", ".py", ".rb", ".pl", ".sh"];

fn name_from_arguments(first: &str, second: Option<&str>) -> Option<String> {
    let program = base_name(first).trim_start_matches('-');
    let interpreted = INTERPRETERS.contains(&program)
        || program
            .strip_prefix("python")
            .is_some_and(|version| version.chars().all(|c| c.is_ascii_digit() || c == '.'));
    let name = match second {
        Some(script) if interpreted && !script.starts_with('-') && !script.is_empty() => {
            let script = base_name(script);
            SCRIPT_EXTENSIONS
                .iter()
                .find_map(|extension| script.strip_suffix(extension))
                .unwrap_or(script)
        }
        _ => program,
    };
    let name = name.trim();
    (!name.is_empty() && name.len() <= 64 && !name.chars().any(char::is_control))
        .then(|| name.to_owned())
}

fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Splits a NUL-separated argument block, keeping only the first two entries.
fn first_two(block: &[u8]) -> Option<(String, Option<String>)> {
    let mut parts = block.split(|byte| *byte == 0);
    let first = String::from_utf8(parts.next()?.to_vec()).ok()?;
    if first.is_empty() {
        return None;
    }
    let second = parts
        .next()
        .filter(|part| !part.is_empty())
        .and_then(|part| String::from_utf8(part.to_vec()).ok());
    Some((first, second))
}

#[cfg(target_os = "macos")]
fn leading_arguments(pid: u32) -> io::Result<(String, Option<String>)> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut buffer = vec![0u8; 64 * 1024];
    let mut size = buffer.len();
    // SAFETY: mib is a valid three-level name; buffer is writable for size bytes.
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buffer.as_mut_ptr().cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    parse_procargs2(&buffer[..size.min(buffer.len())])
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid argument block"))
}

/// `KERN_PROCARGS2` is `argc`, the executable path, NUL padding, then argv.
#[cfg(any(target_os = "macos", test))]
fn parse_procargs2(bytes: &[u8]) -> Option<(String, Option<String>)> {
    let argc = i32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?);
    if argc < 1 {
        return None;
    }
    let rest = &bytes[4..];
    let path_end = rest.iter().position(|byte| *byte == 0)?;
    let argv_start = path_end + rest[path_end..].iter().position(|byte| *byte != 0)?;
    let (first, second) = first_two(&rest[argv_start..])?;
    Some((first, second.filter(|_| argc >= 2)))
}

#[cfg(target_os = "linux")]
fn leading_arguments(pid: u32) -> io::Result<(String, Option<String>)> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(format!("/proc/{pid}/cmdline"))?
        .take(8 * 1024)
        .read_to_end(&mut bytes)?;
    first_two(&bytes)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid argument block"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn leading_arguments(_: u32) -> io::Result<(String, Option<String>)> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_are_named_without_their_arguments() {
        assert_eq!(
            name_from_arguments("vim", Some("secret.txt")).unwrap(),
            "vim"
        );
        assert_eq!(name_from_arguments("/usr/bin/htop", None).unwrap(), "htop");
        assert_eq!(name_from_arguments("-zsh", None).unwrap(), "zsh");
        assert_eq!(
            name_from_arguments("claude", Some("--resume")).unwrap(),
            "claude"
        );
    }

    #[test]
    fn scripts_are_named_after_the_script_not_the_interpreter() {
        assert_eq!(
            name_from_arguments("node", Some("/opt/homebrew/bin/codex")).unwrap(),
            "codex"
        );
        assert_eq!(
            name_from_arguments("/usr/bin/python3.12", Some("/tmp/serve.py")).unwrap(),
            "serve"
        );
        assert_eq!(
            name_from_arguments("bash", Some("./cursor-agent")).unwrap(),
            "cursor-agent"
        );
        assert_eq!(
            name_from_arguments("python3", Some("-m")).unwrap(),
            "python3"
        );
        assert_eq!(name_from_arguments("node", None).unwrap(), "node");
    }

    #[test]
    fn procargs2_skips_the_executable_path_and_padding() {
        let mut block = 3i32.to_ne_bytes().to_vec();
        block.extend_from_slice(b"/opt/homebrew/Cellar/node/bin/node\0\0\0\0");
        block.extend_from_slice(b"node\0/opt/homebrew/bin/codex\0--yolo\0HOME=/x\0");
        assert_eq!(
            parse_procargs2(&block).unwrap(),
            ("node".into(), Some("/opt/homebrew/bin/codex".into()))
        );
        let mut single = 1i32.to_ne_bytes().to_vec();
        single.extend_from_slice(b"/bin/vim\0vim\0HOME=/x\0");
        assert_eq!(parse_procargs2(&single).unwrap(), ("vim".into(), None));
    }

    #[test]
    fn reads_this_process_name() {
        let name = program_name(std::process::id()).unwrap();
        assert!(!name.is_empty() && !name.contains('/'), "{name}");
    }
}
