//! Bytes on their way from the PTY pump to the output log.
//!
//! A macOS PTY hands over one kilobyte per read, so a queue of chunks carried
//! one kilobyte per entry: an allocation to hand it over, a thread wakeup to
//! receive it, and a `write(2)` to append it. At streaming rates that is tens
//! of thousands of syscalls a second per session, and a bound counted in
//! chunks was only a quarter of a megabyte of slack.
//!
//! This is one byte buffer instead. The pump appends to it, and the writer
//! swaps it for an empty one and appends the lot in one write. The writer
//! lingers briefly for a batch to build rather than waking per kilobyte. The
//! log is not on the display path (subscribers get their bytes directly) and
//! is synced only every couple of seconds, so a few milliseconds here change
//! no promise. The linger exists only while bytes are pending; an idle feed
//! parks the writer with no deadline.

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// A batch this large is written at once instead of lingering for more.
pub(super) const LOG_BATCH_BYTES: usize = 64 << 10;

/// The longest a pending byte waits for its batch to fill.
pub(super) const LOG_LINGER: Duration = Duration::from_millis(4);

/// How many bytes may be waiting before the pump has to wait for the writer.
/// A filesystem stall (a truncation rewrite, most of all) is absorbed up to
/// here without stalling the PTY; beyond it the writer applies backpressure
/// rather than letting memory grow without bound.
pub(super) const LOG_PENDING_LIMIT: usize = 4 << 20;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Writer {
    /// Waiting with no deadline for the first pending byte.
    Parked,
    /// Waiting with a deadline for a batch to fill.
    Lingering,
    /// Writing, or about to look at the buffer again.
    Busy,
}

struct State {
    pending: Vec<u8>,
    writer: Writer,
    /// When the oldest pending byte arrived.
    since: Option<Instant>,
    closed: bool,
}

pub(super) struct LogFeed {
    state: Mutex<State>,
    changed: Condvar,
    batch: usize,
    linger: Duration,
    limit: usize,
    /// Every time the writer woke, so a test can prove it is not woken per
    /// chunk.
    #[cfg(test)]
    pub(super) writer_wakeups: std::sync::atomic::AtomicUsize,
}

impl LogFeed {
    pub(super) fn new() -> Self {
        Self::with_limits(LOG_BATCH_BYTES, LOG_LINGER, LOG_PENDING_LIMIT)
    }

    pub(super) fn with_limits(batch: usize, linger: Duration, limit: usize) -> Self {
        Self {
            state: Mutex::new(State {
                pending: Vec::new(),
                writer: Writer::Busy,
                since: None,
                closed: false,
            }),
            changed: Condvar::new(),
            batch,
            linger,
            limit: limit.max(batch),
            #[cfg(test)]
            writer_wakeups: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Queues bytes for the log, waiting for room if the writer has fallen a
    /// full [`LOG_PENDING_LIMIT`] behind. Returns false once the feed is
    /// closed, so the caller appends inline instead of losing the bytes.
    pub(super) fn push(&self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return true;
        }
        let mut state = self.state.lock().expect("log feed");
        while !state.closed && state.pending.len() >= self.limit {
            state = self.changed.wait(state).expect("log feed");
        }
        if state.closed {
            return false;
        }
        let was_empty = state.pending.is_empty();
        if was_empty {
            state.since = Some(Instant::now());
        }
        state.pending.extend_from_slice(bytes);
        // Wake the writer only when that changes what it would do: the first
        // byte for a parked writer, or a full batch for a lingering one.
        // Anything else it collects when its linger ends.
        let wake = match state.writer {
            Writer::Parked => was_empty,
            Writer::Lingering => state.pending.len() >= self.batch,
            Writer::Busy => false,
        };
        drop(state);
        if wake {
            self.changed.notify_all();
        }
        true
    }

    /// Swaps everything pending into `batch` (which must be empty), waiting
    /// for bytes to arrive and, briefly, for a batch to build. Returns false
    /// once the feed is closed and drained.
    pub(super) fn take(&self, batch: &mut Vec<u8>) -> bool {
        debug_assert!(batch.is_empty());
        let mut state = self.state.lock().expect("log feed");
        loop {
            if state.pending.is_empty() {
                if state.closed {
                    state.writer = Writer::Busy;
                    return false;
                }
                state.writer = Writer::Parked;
                state = self.changed.wait(state).expect("log feed");
                #[cfg(test)]
                self.count_wakeup();
                continue;
            }
            let lingered = state.since.map_or(Duration::MAX, |since| since.elapsed());
            if state.closed || state.pending.len() >= self.batch || lingered >= self.linger {
                break;
            }
            state.writer = Writer::Lingering;
            let remaining = self.linger - lingered;
            state = self
                .changed
                .wait_timeout(state, remaining)
                .expect("log feed")
                .0;
            #[cfg(test)]
            self.count_wakeup();
        }
        state.writer = Writer::Busy;
        state.since = None;
        std::mem::swap(&mut state.pending, batch);
        drop(state);
        // A pump waiting for room can continue.
        self.changed.notify_all();
        true
    }

    /// Ends the feed. The writer still drains what is pending.
    pub(super) fn close(&self) {
        self.state.lock().expect("log feed").closed = true;
        self.changed.notify_all();
    }

    #[cfg(test)]
    fn count_wakeup(&self) {
        self.writer_wakeups
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    fn drain(feed: &LogFeed) -> Vec<Vec<u8>> {
        let mut batches = Vec::new();
        let mut batch = Vec::new();
        while feed.take(&mut batch) {
            batches.push(std::mem::take(&mut batch));
        }
        batches
    }

    #[test]
    fn kilobyte_chunks_reach_the_log_in_few_large_writes() {
        let feed = Arc::new(LogFeed::with_limits(
            64 << 10,
            Duration::from_millis(50),
            1 << 20,
        ));
        let writer = {
            let feed = Arc::clone(&feed);
            std::thread::spawn(move || drain(&feed))
        };
        let mut expected = Vec::new();
        for index in 0..1024_u32 {
            let chunk = [(index % 251) as u8; 1024];
            expected.extend_from_slice(&chunk);
            assert!(feed.push(&chunk));
        }
        feed.close();
        let batches = writer.join().expect("writer");
        assert_eq!(batches.concat(), expected, "every byte, in order");
        // A megabyte in kilobyte chunks: one write per chunk was the old
        // shape. Batches of 64 KiB need 16; allow slack for scheduling.
        assert!(
            batches.len() <= 64,
            "{} writes for 1024 chunks",
            batches.len()
        );
        assert!(
            feed.writer_wakeups.load(Ordering::SeqCst) <= 128,
            "writer woke {} times",
            feed.writer_wakeups.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn a_lone_chunk_is_written_within_the_linger() {
        let feed = Arc::new(LogFeed::with_limits(
            64 << 10,
            Duration::from_millis(20),
            1 << 20,
        ));
        let writer = {
            let feed = Arc::clone(&feed);
            std::thread::spawn(move || {
                let mut batch = Vec::new();
                assert!(feed.take(&mut batch));
                batch
            })
        };
        std::thread::sleep(Duration::from_millis(30));
        let pushed = Instant::now();
        assert!(feed.push(b"prompt$ "));
        assert_eq!(writer.join().expect("writer"), b"prompt$ ");
        assert!(
            pushed.elapsed() < Duration::from_secs(1),
            "a small write must not wait for a batch that never fills"
        );
    }

    #[test]
    fn an_idle_writer_stays_parked() {
        let feed = Arc::new(LogFeed::new());
        let writer = {
            let feed = Arc::clone(&feed);
            std::thread::spawn(move || drain(&feed))
        };
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(feed.writer_wakeups.load(Ordering::SeqCst), 0);
        feed.close();
        assert!(writer.join().expect("writer").is_empty());
    }

    #[test]
    fn a_full_feed_holds_the_pump_until_the_writer_drains() {
        let feed = Arc::new(LogFeed::with_limits(4, Duration::from_millis(1), 8));
        assert!(feed.push(&[1; 8]));
        let pusher = {
            let feed = Arc::clone(&feed);
            std::thread::spawn(move || feed.push(&[2; 4]))
        };
        std::thread::sleep(Duration::from_millis(50));
        assert!(!pusher.is_finished(), "the pump waits for room");
        let mut batch = Vec::new();
        assert!(feed.take(&mut batch));
        assert_eq!(batch, [1; 8]);
        assert!(pusher.join().expect("pusher"));
        feed.close();
        batch.clear();
        assert!(
            feed.take(&mut batch),
            "closing still drains what is pending"
        );
        assert_eq!(batch, [2; 4]);
        batch.clear();
        assert!(!feed.take(&mut batch));
        assert!(!feed.push(b"late"), "a closed feed refuses bytes");
    }
}
