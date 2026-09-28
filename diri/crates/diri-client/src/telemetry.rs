//! Flight-recorder hooks for the control and attachment clients.
//!
//! Every call is a no-op unless the host process initialized
//! `diri-telemetry` (the desktop app does; tests and `diri-web` do not).
//! Nothing here records a payload: only method names, error classes and
//! timings. See `diri/TELEMETRY.md` (App section).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use diri_proto::methods::Method;
use diri_telemetry::{error_event, id, text, warn_event};

use crate::client::ClientError;

/// Requests slower than this record `rpc.slow`.
const SLOW_RPC: Duration = Duration::from_secs(2);
/// Distinct method histograms kept before the rest share `rpc.other`.
const METHOD_METRICS_MAX: usize = 160;

/// Methods whose latency is a wait by design, not a symptom.
fn is_long_poll(method: &str) -> bool {
    matches!(method, Method::EVENTS_WAIT | Method::TEST_RUN)
}

/// The per-method latency histogram name, `rpc.<method>`. Method names are a
/// finite, code-authored set, so interning them once is bounded; anything
/// that is not a valid identifier, or past the cap, shares `rpc.other`.
fn method_metric(method: &str) -> &'static str {
    static NAMES: Mutex<Option<HashMap<String, &'static str>>> = Mutex::new(None);
    if id(method).as_str() == "!invalid" {
        return "rpc.other";
    }
    let Ok(mut names) = NAMES.lock() else {
        return "rpc.other";
    };
    let names = names.get_or_insert_with(HashMap::new);
    if let Some(name) = names.get(method) {
        return name;
    }
    if names.len() >= METHOD_METRICS_MAX {
        return "rpc.other";
    }
    let name: &'static str = Box::leak(format!("rpc.{method}").into_boxed_str());
    names.insert(method.to_owned(), name);
    name
}

/// The grouping class of a client error.
pub(crate) fn error_kind(error: &ClientError) -> &'static str {
    match error {
        ClientError::Control(_) => "control",
        ClientError::Disconnected(_) => "disconnected",
        ClientError::Timeout(_) => "timeout",
        ClientError::Io(_) => "io",
        ClientError::Json(_) => "json",
        ClientError::Protocol(_) => "protocol",
    }
}

/// One finished control request: its latency always, and an event when it
/// failed or was slow.
pub(crate) fn rpc_finished(method: &str, started: Instant, error: Option<&ClientError>) {
    if !diri_telemetry::is_enabled() {
        return;
    }
    let elapsed = started.elapsed();
    diri_telemetry::observe(method_metric(method), elapsed);
    diri_telemetry::count("rpc.calls", 1);
    match error {
        // A dropped connection fails every pending and new request at once;
        // `client.disconnected` records it once, so these are only counted.
        Some(ClientError::Disconnected(_)) => diri_telemetry::count("rpc.disconnected", 1),
        Some(error) => {
            diri_telemetry::count("rpc.errors", 1);
            // An Engine error code is an identifier; its message may carry a
            // path or a host, so only its scrubbed form is kept.
            let (code, message) = match error {
                ClientError::Control(control) => {
                    (Some(id(&control.code)), Some(text(&control.message)))
                }
                ClientError::Timeout(message) | ClientError::Protocol(message) => {
                    (None, Some(text(message)))
                }
                ClientError::Io(_) | ClientError::Json(_) | ClientError::Disconnected(_) => {
                    (None, None)
                }
            };
            error_event!(
                "rpc.error",
                method = id(method),
                kind = error_kind(error),
                code = code,
                error = message,
                ms = elapsed
            );
        }
        None if elapsed >= SLOW_RPC && !is_long_poll(method) => {
            warn_event!("rpc.slow", method = id(method), ms = elapsed);
        }
        None => {}
    }
}

/// The Engine answered Hello with an identity the client refuses. Fails
/// closed either way; a changed instance means the Engine was replaced
/// under a live connection, anything else is a foreign or broken daemon.
pub(crate) fn identity_rejected(
    previously_rejected: bool,
    engine_kind: Option<&str>,
    invalid_instance: bool,
    hello: &diri_proto::HelloResult,
) {
    let reason = if previously_rejected {
        "previously_rejected"
    } else if engine_kind != Some(diri_proto::RUST_ENGINE_KIND) {
        "engine_kind"
    } else if invalid_instance {
        "invalid_instance"
    } else {
        "instance_changed"
    };
    if reason == "instance_changed" {
        warn_event!(
            "client.identity_rejected",
            reason = reason,
            engine_build = id(&hello.build),
            engine_pid = hello.pid
        );
    } else {
        error_event!(
            "client.identity_rejected",
            reason = reason,
            engine_kind = engine_kind.map(id),
            engine_build = id(&hello.build),
            engine_pid = hello.pid,
            proto = hello.proto
        );
    }
}

/// An interactive attachment's data connection ended other than by the
/// app closing it. `receiver_closed` is the app dropping the view mid-read.
pub(crate) fn attachment_ended(
    session: &diri_proto::SessionId,
    reason: &'static str,
    live: Duration,
) {
    match reason {
        "receiver_closed" => {}
        "decode_error" | "bad_grid" | "bad_modes" => error_event!(
            "attach.closed",
            session = id(&session.0),
            reason = reason,
            live_ms = live
        ),
        _ => warn_event!(
            "attach.closed",
            session = id(&session.0),
            reason = reason,
            live_ms = live
        ),
    }
}

/// Failed attempts in one outage before it is recorded as an error. With the
/// 0.5 s → 8 s backoff this is about 45 s without an Engine.
const FAILING_AFTER_ATTEMPTS: u32 = 10;

/// Follows the reconnect loop so an outage is one `client.disconnected`,
/// at most one `client.connect_failing`, and one `client.connected` with the
/// time it took, rather than an event per retry.
pub(crate) struct ConnectionRecorder {
    down_since: Instant,
    failed_attempts: u32,
    first_failure: Option<&'static str>,
    attempt_at: Instant,
    opened_at: Option<Instant>,
    connected_at: Option<Instant>,
    connections: u64,
}

impl Default for ConnectionRecorder {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            down_since: now,
            failed_attempts: 0,
            first_failure: None,
            attempt_at: now,
            opened_at: None,
            connected_at: None,
            connections: 0,
        }
    }
}

impl ConnectionRecorder {
    pub(crate) fn attempt_started(&mut self) {
        self.attempt_at = Instant::now();
        self.opened_at = None;
    }

    pub(crate) fn socket_opened(&mut self) {
        self.opened_at = Some(Instant::now());
    }

    pub(crate) fn connected(&mut self, hello: &diri_proto::HelloResult) {
        let now = Instant::now();
        let opened = self.opened_at.unwrap_or(self.attempt_at);
        diri_telemetry::observe("client.connect", now.duration_since(self.attempt_at));
        diri_telemetry::event!(
            "client.connected",
            reconnect = self.connections > 0,
            attempts = self.failed_attempts + 1,
            down_ms = now.duration_since(self.down_since),
            connect_ms = opened.duration_since(self.attempt_at),
            hello_ms = now.duration_since(opened),
            first_failure = self.first_failure,
            engine_build = id(&hello.build),
            engine_pid = hello.pid,
            proto = hello.proto
        );
        self.failed_attempts = 0;
        self.first_failure = None;
        self.connected_at = Some(now);
        self.connections += 1;
    }

    pub(crate) fn attempt_ended(
        &mut self,
        error: &ClientError,
        established: bool,
        shutting_down: bool,
    ) {
        let now = Instant::now();
        if let Some(connected_at) = self.connected_at.take() {
            self.down_since = now;
            if shutting_down {
                return;
            }
            warn_event!(
                "client.disconnected",
                kind = error_kind(error),
                error = text(error.to_string()),
                connected_s = now.duration_since(connected_at).as_secs()
            );
            return;
        }
        if shutting_down {
            return;
        }
        self.failed_attempts += 1;
        self.first_failure.get_or_insert(error_kind(error));
        if self.failed_attempts == FAILING_AFTER_ATTEMPTS {
            error_event!(
                "client.connect_failing",
                attempts = self.failed_attempts,
                down_ms = now.duration_since(self.down_since),
                kind = error_kind(error),
                error = text(error.to_string()),
                handshake = established
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_metrics_are_interned_and_bounded_to_identifiers() {
        let first = method_metric("session.send_text");
        assert_eq!(first, "rpc.session.send_text");
        assert!(std::ptr::eq(first, method_metric("session.send_text")));
        assert_eq!(method_metric("not a method"), "rpc.other");
        assert_eq!(method_metric("/Users/julia"), "rpc.other");
    }
}
