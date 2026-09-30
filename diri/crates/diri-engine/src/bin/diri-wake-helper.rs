//! Privileged wake helper. See `diri_engine::wake` for what it may do and
//! why it exists. launchd starts it as root when a client connects to its
//! socket; it serves connections and exits after a short idle period, so it
//! costs nothing between scheduled runs.

#[cfg(target_os = "macos")]
fn main() {
    helper::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("diri-wake-helper: only macOS can schedule wakes");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
mod helper {
    use std::ffi::{CString, c_char, c_int, c_void};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use diri_engine::wake::{
        MAX_REQUEST_BYTES, MIN_SLEEP_IDLE_SECS, Request, Response, owner_tag, reconcile,
        validate_wakes,
    };

    /// Exit after this long with no connection; launchd restarts on demand.
    const IDLE_EXIT_MS: c_int = 20_000;
    /// Seconds between the Unix epoch and CoreFoundation's 2001 reference date.
    const CF_EPOCH_OFFSET: f64 = 978_307_200.0;
    const UTF8: u32 = 0x0800_0100;
    const SINT64: isize = 4;

    type CfRef = *const c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            alloc: CfRef,
            bytes: *const u8,
            len: isize,
            encoding: u32,
            external: u8,
        ) -> CfRef;
        fn CFStringGetCString(string: CfRef, buffer: *mut c_char, size: isize, encoding: u32)
        -> u8;
        fn CFStringGetTypeID() -> usize;
        fn CFDateCreate(alloc: CfRef, at: f64) -> CfRef;
        fn CFDateGetAbsoluteTime(date: CfRef) -> f64;
        fn CFDateGetTypeID() -> usize;
        fn CFArrayGetCount(array: CfRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CfRef, index: isize) -> CfRef;
        fn CFDictionaryGetValue(dictionary: CfRef, key: CfRef) -> CfRef;
        fn CFNumberGetValue(number: CfRef, kind: isize, out: *mut c_void) -> u8;
        fn CFNumberGetTypeID() -> usize;
        fn CFGetTypeID(value: CfRef) -> usize;
        fn CFRelease(value: CfRef);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMSchedulePowerEvent(at: CfRef, owner: CfRef, kind: CfRef) -> c_int;
        fn IOPMCancelScheduledPowerEvent(at: CfRef, owner: CfRef, kind: CfRef) -> c_int;
        fn IOPMCopyScheduledPowerEvents() -> CfRef;
        fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> u32;
        fn IORegistryEntryCreateCFProperty(
            entry: u32,
            key: CfRef,
            alloc: CfRef,
            options: u32,
        ) -> CfRef;
        fn IOObjectRelease(object: u32) -> c_int;
    }

    unsafe extern "C" {
        fn launch_activate_socket(
            name: *const c_char,
            fds: *mut *mut c_int,
            count: *mut usize,
        ) -> c_int;
    }

    /// An owned CoreFoundation object, released on drop.
    struct Cf(CfRef);

    impl Drop for Cf {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: every `Cf` holds a +1 reference from a Create/Copy call.
                unsafe { CFRelease(self.0) }
            }
        }
    }

    fn cf_string(text: &str) -> Cf {
        // SAFETY: the bytes are valid UTF-8 for the given length.
        Cf(unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                text.as_ptr(),
                text.len() as isize,
                UTF8,
                0,
            )
        })
    }

    fn cf_date(epoch_ms: i64) -> Cf {
        // SAFETY: plain value constructor.
        Cf(unsafe { CFDateCreate(std::ptr::null(), epoch_ms as f64 / 1000.0 - CF_EPOCH_OFFSET) })
    }

    fn read_string(value: CfRef) -> Option<String> {
        // SAFETY: the type is checked before the value is read as a string.
        unsafe {
            if value.is_null() || CFGetTypeID(value) != CFStringGetTypeID() {
                return None;
            }
            let mut buffer = [0 as c_char; 256];
            (CFStringGetCString(value, buffer.as_mut_ptr(), buffer.len() as isize, UTF8) != 0).then(
                || {
                    std::ffi::CStr::from_ptr(buffer.as_ptr())
                        .to_string_lossy()
                        .into_owned()
                },
            )
        }
    }

    fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or_default()
    }

    /// Wake events this owner scheduled, as whole-second epoch milliseconds.
    fn owned_wakes(owner: &str) -> Vec<i64> {
        let time_key = cf_string("time");
        let owner_key = cf_string("scheduledby");
        let type_key = cf_string("eventtype");
        // SAFETY: a Copy call; the array and its dictionaries are read only
        // while `events` holds the reference.
        let events = Cf(unsafe { IOPMCopyScheduledPowerEvents() });
        if events.0.is_null() {
            return Vec::new();
        }
        let mut wakes = Vec::new();
        unsafe {
            for index in 0..CFArrayGetCount(events.0) {
                let event = CFArrayGetValueAtIndex(events.0, index);
                if read_string(CFDictionaryGetValue(event, owner_key.0)).as_deref() != Some(owner)
                    || read_string(CFDictionaryGetValue(event, type_key.0)).as_deref()
                        != Some("wake")
                {
                    continue;
                }
                let date = CFDictionaryGetValue(event, time_key.0);
                if date.is_null() || CFGetTypeID(date) != CFDateGetTypeID() {
                    continue;
                }
                let seconds = (CFDateGetAbsoluteTime(date) + CF_EPOCH_OFFSET).round();
                wakes.push(seconds as i64 * 1000);
            }
        }
        wakes.sort_unstable();
        wakes
    }

    fn set_wakes(owner: &str, desired: &[i64]) -> Result<Vec<i64>, String> {
        let owner_cf = cf_string(owner);
        let kind = cf_string("wake");
        let (cancel, add) = reconcile(&owned_wakes(owner), desired);
        for time in cancel {
            let date = cf_date(time);
            // SAFETY: all arguments are live CF objects.
            unsafe { IOPMCancelScheduledPowerEvent(date.0, owner_cf.0, kind.0) };
        }
        for time in add {
            let date = cf_date(time);
            // SAFETY: all arguments are live CF objects.
            let status = unsafe { IOPMSchedulePowerEvent(date.0, owner_cf.0, kind.0) };
            if status != 0 {
                return Err(format!("macOS refused to schedule a wake ({status:#x})"));
            }
        }
        Ok(owned_wakes(owner))
    }

    /// Seconds since the last keyboard, mouse, or trackpad event.
    fn hid_idle_secs() -> Option<u64> {
        let name = CString::new("IOHIDSystem").ok()?;
        // SAFETY: IOServiceGetMatchingService consumes the matching
        // dictionary; the service and property are released below.
        unsafe {
            let service = IOServiceGetMatchingService(0, IOServiceMatching(name.as_ptr()));
            if service == 0 {
                return None;
            }
            let key = cf_string("HIDIdleTime");
            let value = Cf(IORegistryEntryCreateCFProperty(
                service,
                key.0,
                std::ptr::null(),
                0,
            ));
            IOObjectRelease(service);
            if value.0.is_null() || CFGetTypeID(value.0) != CFNumberGetTypeID() {
                return None;
            }
            let mut nanos: i64 = 0;
            (CFNumberGetValue(value.0, SINT64, (&mut nanos as *mut i64).cast()) != 0)
                .then(|| (nanos.max(0) as u64) / 1_000_000_000)
        }
    }

    fn console_uid() -> Option<u32> {
        std::fs::metadata("/dev/console")
            .ok()
            .map(|metadata| metadata.uid())
    }

    fn sleep_if_idle(uid: u32, min_idle_secs: u64) -> Result<bool, String> {
        if min_idle_secs < MIN_SLEEP_IDLE_SECS {
            return Err(format!(
                "minimum idle time is {MIN_SLEEP_IDLE_SECS} seconds"
            ));
        }
        // Another logged-in user must not put the active user's Mac to sleep.
        if console_uid() != Some(uid) {
            return Ok(false);
        }
        if hid_idle_secs().is_none_or(|idle| idle < min_idle_secs) {
            return Ok(false);
        }
        std::process::Command::new("/usr/bin/pmset")
            .arg("sleepnow")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|error| error.to_string())
            .map(|status| status.success())
    }

    fn peer_uid(stream: &UnixStream) -> Option<u32> {
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        // SAFETY: getpeereid writes both ids for a connected Unix socket.
        (unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } == 0).then_some(uid)
    }

    fn handle(stream: UnixStream) {
        let Some(uid) = peer_uid(&stream) else {
            return;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        let mut line = String::new();
        let Ok(reader) = stream.try_clone() else {
            return;
        };
        if BufReader::new(reader.take(MAX_REQUEST_BYTES as u64))
            .read_line(&mut line)
            .is_err()
        {
            return;
        }
        let owner = owner_tag(uid);
        let response = match serde_json::from_str::<Request>(&line) {
            Err(_) => Response::failure("unknown request"),
            Ok(Request::Status) => Response {
                ok: true,
                wakes_ms: owned_wakes(&owner),
                ..Response::default()
            },
            Ok(Request::SetWakes { times_ms }) => match validate_wakes(&times_ms, now_ms())
                .and_then(|desired| set_wakes(&owner, &desired))
            {
                Ok(wakes_ms) => Response {
                    ok: true,
                    wakes_ms,
                    ..Response::default()
                },
                Err(error) => Response::failure(error),
            },
            Ok(Request::SleepIfIdle { min_idle_secs }) => match sleep_if_idle(uid, min_idle_secs) {
                Ok(slept) => Response {
                    ok: true,
                    slept,
                    ..Response::default()
                },
                Err(error) => Response::failure(error),
            },
        };
        let mut reply = serde_json::to_vec(&response).unwrap_or_default();
        reply.push(b'\n');
        let mut stream = stream;
        let _ = stream.write_all(&reply);
    }

    fn listener() -> Option<UnixListener> {
        let name = CString::new("Listeners").ok()?;
        let mut fds: *mut c_int = std::ptr::null_mut();
        let mut count = 0usize;
        // SAFETY: launchd hands back a malloc'd array of `count` descriptors.
        unsafe {
            if launch_activate_socket(name.as_ptr(), &mut fds, &mut count) != 0 || count == 0 {
                return None;
            }
            let fd = *fds;
            libc::free(fds.cast());
            Some(UnixListener::from_raw_fd(fd))
        }
    }

    pub(super) fn run() {
        let Some(listener) = listener() else {
            eprintln!("diri-wake-helper: not started by launchd");
            std::process::exit(1);
        };
        loop {
            let mut poll = libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd.
            match unsafe { libc::poll(&mut poll, 1, IDLE_EXIT_MS) } {
                0 => return,
                n if n < 0 => {
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return;
                }
                _ => {
                    if let Ok((stream, _)) = listener.accept() {
                        handle(stream);
                    }
                }
            }
        }
    }
}
