//! Waking the Mac for a scheduled run.
//!
//! Only root may schedule a wake on macOS, so a tiny privileged helper
//! (`diri-wake-helper`) does it. The user approves it once in System
//! Settings > Login Items; launchd then starts it on demand when the Engine
//! connects to its socket, and it exits when idle. It can do exactly three
//! things for the connecting user: replace that user's diri wake times, report
//! them, and put the Mac back to sleep if the console user has been idle for
//! as long as the Engine says its run took. Nothing else runs as root.
//!
//! Wire: one JSON request line, one JSON response line, per connection.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// launchd creates this socket from the helper's plist (`Sockets`).
pub const SOCKET_PATH: &str = "/var/run/com.dirijor.diri.wake.sock";
/// launchd label and plist name the app registers with SMAppService.
pub const HELPER_LABEL: &str = "com.dirijor.diri.wake";
/// Wake this long before a run is due, so the Engine is up to fire it.
pub const WAKE_LEAD_MS: i64 = 2 * 60 * 1000;
/// At most this many wakes per user are kept scheduled.
pub const MAX_WAKES: usize = 64;
/// Wakes further out than this are refused; the Engine only asks for each
/// schedule's next occurrence anyway.
pub const MAX_WAKE_AHEAD_MS: i64 = 35 * 24 * 60 * 60 * 1000;
/// Requests for sleep below this idle time are refused.
pub const MIN_SLEEP_IDLE_SECS: u64 = 60;
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum Request {
    /// Replace every diri wake for the calling user with these times.
    #[serde(rename_all = "camelCase")]
    SetWakes {
        times_ms: Vec<i64>,
    },
    /// Sleep now, but only if the console belongs to the caller and nobody
    /// has touched a keyboard, mouse, or trackpad for `min_idle_secs`.
    #[serde(rename_all = "camelCase")]
    SleepIfIdle {
        min_idle_secs: u64,
    },
    Status,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The caller's diri wake times now scheduled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wakes_ms: Vec<i64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub slept: bool,
}

impl Response {
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
            ..Self::default()
        }
    }
}

/// The owner tag the helper stamps on a user's wake events, so one user can
/// never cancel another's, or anything macOS or other apps scheduled.
pub fn owner_tag(uid: u32) -> String {
    format!("{HELPER_LABEL}.{uid}")
}

/// Sorted, de-duplicated, whole-second wake times, or why they are refused.
pub fn validate_wakes(times_ms: &[i64], now_ms: i64) -> Result<Vec<i64>, String> {
    if times_ms.len() > MAX_WAKES {
        return Err(format!("at most {MAX_WAKES} wake times"));
    }
    let mut times: Vec<i64> = Vec::with_capacity(times_ms.len());
    for &time in times_ms {
        if time <= now_ms + 30_000 || time > now_ms + MAX_WAKE_AHEAD_MS {
            return Err("wake times must be between 30 seconds and 35 days ahead".into());
        }
        times.push(time.div_euclid(1000) * 1000);
    }
    times.sort_unstable();
    times.dedup();
    Ok(times)
}

/// What the helper must cancel and add to turn `existing` into `desired`.
/// Both are whole-second epoch milliseconds owned by one user.
pub fn reconcile(existing: &[i64], desired: &[i64]) -> (Vec<i64>, Vec<i64>) {
    let cancel = existing
        .iter()
        .copied()
        .filter(|time| !desired.contains(time))
        .collect();
    let add = desired
        .iter()
        .copied()
        .filter(|time| !existing.contains(time))
        .collect();
    (cancel, add)
}

/// One request to the helper. Fails fast when it is not installed.
pub fn call(request: &Request) -> Result<Response, String> {
    call_at(SOCKET_PATH, request)
}

pub fn call_at(path: &str, request: &Request) -> Result<Response, String> {
    let mut stream = UnixStream::connect(path)
        .map_err(|error| format!("the wake helper isn't available ({error})"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .and_then(|()| stream.set_write_timeout(Some(Duration::from_secs(5))))
        .map_err(|error| error.to_string())?;
    let mut line = serde_json::to_vec(request).map_err(|error| error.to_string())?;
    line.push(b'\n');
    stream.write_all(&line).map_err(|error| error.to_string())?;
    let mut reply = String::new();
    BufReader::new(std::io::Read::take(stream, MAX_REQUEST_BYTES as u64))
        .read_line(&mut reply)
        .map_err(|error| error.to_string())?;
    let response: Response =
        serde_json::from_str(&reply).map_err(|_| "the wake helper sent a bad reply".to_owned())?;
    if response.ok {
        Ok(response)
    } else {
        Err(response
            .error
            .unwrap_or_else(|| "the wake helper refused".to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;

    #[test]
    fn requests_round_trip_in_camel_case() {
        let request = Request::SetWakes {
            times_ms: vec![NOW],
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["op"], "setWakes");
        assert_eq!(json["timesMs"][0], NOW);
        let sleep: Request =
            serde_json::from_str(r#"{"op":"sleepIfIdle","minIdleSecs":90}"#).unwrap();
        assert_eq!(sleep, Request::SleepIfIdle { min_idle_secs: 90 });
        assert!(serde_json::from_str::<Request>(r#"{"op":"shell","cmd":"id"}"#).is_err());
    }

    #[test]
    fn wake_times_are_bounded_rounded_and_deduplicated() {
        let hour = 3_600_000;
        assert_eq!(
            validate_wakes(&[NOW + 2 * hour + 999, NOW + hour, NOW + hour + 400], NOW).unwrap(),
            vec![NOW + hour, NOW + 2 * hour]
        );
        assert!(validate_wakes(&[NOW + 1_000], NOW).is_err(), "too soon");
        assert!(validate_wakes(&[NOW - hour], NOW).is_err(), "past");
        assert!(
            validate_wakes(&[NOW + 40 * 24 * hour], NOW).is_err(),
            "too far"
        );
        let many: Vec<i64> = (1..=65).map(|i| NOW + i * hour).collect();
        assert!(validate_wakes(&many, NOW).is_err(), "too many");
        assert_eq!(validate_wakes(&[], NOW).unwrap(), Vec::<i64>::new());
    }

    #[test]
    fn reconcile_touches_only_the_difference() {
        let (cancel, add) = reconcile(&[1, 2, 3], &[2, 3, 4]);
        assert_eq!(cancel, vec![1]);
        assert_eq!(add, vec![4]);
        assert_eq!(reconcile(&[5], &[5]), (vec![], vec![]));
    }

    #[test]
    fn owner_tags_are_per_user() {
        assert_eq!(owner_tag(501), "com.dirijor.diri.wake.501");
        assert_ne!(owner_tag(501), owner_tag(502));
    }

    #[test]
    fn a_missing_helper_fails_fast_with_a_readable_error() {
        let error = call_at("/nonexistent/diri-wake.sock", &Request::Status).unwrap_err();
        assert!(error.contains("isn't available"), "{error}");
    }
}
