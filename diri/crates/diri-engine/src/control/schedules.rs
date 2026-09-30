//! Scheduled tasks. Records live in the Engine's own SQLite journal, apart
//! from every session, so closing a tab never cancels a schedule and an
//! Engine restart picks up where it stopped.
//!
//! Each due occurrence is claimed in a transaction before its session is
//! started: a crash between claim and spawn loses that run (recorded, never
//! repeated) rather than starting it twice. The runner sleeps until the next
//! due time, but never longer than [`MAX_WAIT`]: a relative wait on macOS
//! does not count time spent asleep, so a capped wait is what notices a wake
//! and fires the missed run. With no enabled schedule it parks indefinitely.
use super::message_delivery::open;
use super::operations::storage_error;
use crate::schedule::{self, Evaluation};
use diri_proto::schedules::{
    LateReason, MAX_SCHEDULE_RUNS, ScheduleIdParams, ScheduleListResult, ScheduleOutcome,
    ScheduleRecord, ScheduleRun, ScheduleSpec, ScheduleUpdateParams, ScheduleWhen,
};
use diri_proto::{ControlError, DateMillis, Method, SessionId, SessionStatus};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// Longest the runner sleeps while any schedule is enabled.
const MAX_WAIT: Duration = Duration::from_secs(30);
/// Wall time advancing this much more than monotonic time means the Mac slept.
const SLEEP_GAP_MS: i64 = 20_000;
/// A keep-awake schedule holds its assertion from this long before it is due.
const KEEP_AWAKE_LEAD_MS: i64 = 10 * 60 * 1000;
/// And while its run's session works, for at most this long.
const KEEP_AWAKE_RUN_CAP_MS: i64 = 4 * 60 * 60 * 1000;
/// Retries for a spawn refused while an account operation holds the Engine.
const SPAWN_ATTEMPTS: u32 = 6;
const SPAWN_RETRY_DELAY: Duration = Duration::from_secs(10);
const MAX_SCHEDULES: i64 = 500;
const MAX_TITLE_BYTES: usize = 200;
const MAX_PROMPT_BYTES: usize = 1_048_576;
const MAX_CATCH_UP_WINDOW_MS: u64 = 7 * 24 * 60 * 60 * 1000;

fn now_ms() -> i64 {
    DateMillis::from(SystemTime::now()).0 as i64
}

fn database(path: &Path) -> Result<Connection, ControlError> {
    let db = open(path)?;
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS schedules_v1 (
        id TEXT PRIMARY KEY, record TEXT NOT NULL
    );",
    )
    .map_err(storage_error)?;
    Ok(db)
}

fn load(db: &Connection, id: &str) -> Result<ScheduleRecord, ControlError> {
    let raw: Option<String> = db
        .query_row("SELECT record FROM schedules_v1 WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()
        .map_err(storage_error)?;
    decode_record(&raw.ok_or_else(|| ControlError::not_found("schedule"))?)
}

fn decode_record(raw: &str) -> Result<ScheduleRecord, ControlError> {
    serde_json::from_str(raw).map_err(|_| ControlError::internal("invalid stored schedule"))
}

fn load_all(db: &Connection) -> Result<Vec<ScheduleRecord>, ControlError> {
    let mut statement = db
        .prepare("SELECT record FROM schedules_v1")
        .map_err(storage_error)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(storage_error)?;
    let mut records = Vec::new();
    for raw in rows {
        // One unreadable row must not stop every other schedule.
        if let Ok(record) = decode_record(&raw.map_err(storage_error)?) {
            records.push(record);
        }
    }
    records.sort_by(|a, b| a.created_at.0.total_cmp(&b.created_at.0));
    Ok(records)
}

fn save(db: &Connection, record: &ScheduleRecord) -> Result<(), ControlError> {
    let raw = serde_json::to_string(record)
        .map_err(|_| ControlError::internal("cannot encode schedule"))?;
    db.execute(
        "INSERT INTO schedules_v1 (id, record) VALUES (?1, ?2)
         ON CONFLICT(id) DO UPDATE SET record=excluded.record",
        params![record.id, raw],
    )
    .map_err(storage_error)?;
    Ok(())
}

fn validate(spec: &mut ScheduleSpec, now: i64) -> Result<Option<i64>, ControlError> {
    spec.title = spec.title.trim().to_string();
    if spec.title.is_empty() || spec.title.len() > MAX_TITLE_BYTES {
        return Err(ControlError::bad_request(
            "schedule title must contain 1–200 bytes",
        ));
    }
    let prompt = spec.spawn.initial_prompt.as_deref().unwrap_or("").trim();
    if prompt.is_empty() || prompt.len() > MAX_PROMPT_BYTES {
        return Err(ControlError::bad_request(
            "a schedule needs a prompt (initialPrompt) of at most 1 MiB",
        ));
    }
    if spec.spawn.cwd.trim().is_empty() {
        return Err(ControlError::bad_request("a schedule needs a folder (cwd)"));
    }
    if spec.spawn.parent.is_some() {
        return Err(ControlError::bad_request(
            "a scheduled run cannot be a child of another session",
        ));
    }
    if spec.catch_up_window_ms > MAX_CATCH_UP_WINDOW_MS {
        return Err(ControlError::bad_request(
            "the catch-up window is at most 7 days",
        ));
    }
    if let ScheduleWhen::Once { at } = &spec.when
        && spec.enabled
        && (at.0 as i64) < now - schedule::ON_TIME_SLACK_MS
    {
        return Err(ControlError::bad_request("that time is in the past"));
    }
    if !spec.enabled {
        // Still reject an unparsable cron so it cannot be enabled later.
        schedule::first_due(&spec.when, now).map_err(ControlError::bad_request)?;
        return Ok(None);
    }
    schedule::first_due(&spec.when, now).map_err(ControlError::bad_request)
}

fn random_id() -> String {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).expect("the OS random source");
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("sched_{hex}")
}

fn push_run(record: &mut ScheduleRecord, run: ScheduleRun) {
    record.runs.push(run);
    let excess = record.runs.len().saturating_sub(MAX_SCHEDULE_RUNS);
    record.runs.drain(..excess);
}

/// The runner's view of one enabled schedule, refreshed after every write.
#[derive(Clone, Debug)]
struct Pending {
    next_due_ms: i64,
    keep_awake: bool,
}

/// One started run whose session may still hold the keep-awake assertion.
struct ActiveRun {
    session_id: String,
    fired_ms: i64,
}

#[derive(Default)]
struct RunnerState {
    pending: HashMap<String, Pending>,
    /// Set by any mutation: the runner reloads before sleeping again.
    dirty: bool,
    /// Runs started by hand, handed to the runner thread to spawn.
    manual: Vec<(ScheduleRecord, i64)>,
    loaded: bool,
}

pub(super) struct Scheduler {
    state: Mutex<RunnerState>,
    wake: Condvar,
    enabled: AtomicUsize,
    active: Mutex<Vec<ActiveRun>>,
    started_ms: i64,
    /// The most recent sleep the runner observed, as wall-clock bounds.
    last_sleep: Mutex<Option<(i64, i64)>>,
    keep_awake: Mutex<Option<std::process::Child>>,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            wake: Condvar::new(),
            enabled: AtomicUsize::new(0),
            active: Mutex::default(),
            started_ms: now_ms(),
            last_sleep: Mutex::default(),
            keep_awake: Mutex::default(),
        }
    }
}

impl Scheduler {
    /// Enabled schedules keep an otherwise idle Engine alive.
    pub(super) fn enabled_count(&self) -> usize {
        self.enabled.load(Ordering::Acquire)
    }

    fn poke(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.dirty = true;
        }
        self.wake.notify_all();
    }

    fn late_reason(&self, due_ms: i64) -> Option<LateReason> {
        if due_ms < self.started_ms {
            return Some(LateReason::NotRunning);
        }
        let slept = *self.last_sleep.lock().ok()?;
        slept
            .filter(|(from, to)| (*from..=*to).contains(&due_ms))
            .map(|_| LateReason::Asleep)
    }

    fn refresh(&self, records: &[ScheduleRecord]) {
        let pending: HashMap<String, Pending> = records
            .iter()
            .filter(|record| record.spec.enabled)
            .filter_map(|record| {
                Some((
                    record.id.clone(),
                    Pending {
                        next_due_ms: record.next_due?.0 as i64,
                        keep_awake: record.spec.keep_awake,
                    },
                ))
            })
            .collect();
        self.enabled.store(pending.len(), Ordering::Release);
        if let Ok(mut state) = self.state.lock() {
            state.pending = pending;
            state.loaded = true;
        }
    }

    fn set_keep_awake(&self, wanted: bool) {
        let Ok(mut child) = self.keep_awake.lock() else {
            return;
        };
        if wanted {
            if let Some(running) = child.as_mut()
                && matches!(running.try_wait(), Ok(None))
            {
                return;
            }
            *child = spawn_caffeinate();
        } else if let Some(mut running) = child.take() {
            let _ = running.kill();
            let _ = running.wait();
        }
    }
}

/// `caffeinate -i` holds an idle-sleep assertion without privileges, and
/// `-w` ties it to this Engine so a crash can never leave the Mac awake.
#[cfg(target_os = "macos")]
fn spawn_caffeinate() -> Option<std::process::Child> {
    std::process::Command::new("/usr/bin/caffeinate")
        .args(["-i", "-w", &std::process::id().to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
}

#[cfg(not(target_os = "macos"))]
fn spawn_caffeinate() -> Option<std::process::Child> {
    None
}

/// Whether a started run still counts as working for keep-awake.
fn still_working(status: Option<SessionStatus>) -> bool {
    matches!(
        status,
        Some(SessionStatus::Starting | SessionStatus::Working)
    )
}

impl super::ControlServer {
    fn schedules_path(&self) -> PathBuf {
        self.socket_path.with_file_name("schedules-v1.sqlite")
    }

    fn publish_schedule(&self, id: &str) {
        self.events.publish(
            diri_proto::EventName::SCHEDULE_UPDATED,
            json!({ "id": id }),
            None,
        );
    }

    pub(super) fn schedule_list(&self) -> Result<Value, ControlError> {
        let db = database(&self.schedules_path())?;
        let schedules = load_all(&db)?;
        Ok(serde_json::to_value(ScheduleListResult { schedules }).unwrap())
    }

    pub(super) fn schedule_create(&self, params: Option<Value>) -> Result<Value, ControlError> {
        let mut spec: ScheduleSpec = super::decode(params)?;
        let now = now_ms();
        let next_due = validate(&mut spec, now)?;
        let record = ScheduleRecord {
            id: random_id(),
            spec,
            created_at: DateMillis(now as f64),
            revision: 0,
            next_due: next_due.map(|due| DateMillis(due as f64)),
            runs: Vec::new(),
        };
        let mut db = database(&self.schedules_path())?;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM schedules_v1", [], |row| row.get(0))
            .map_err(storage_error)?;
        if count >= MAX_SCHEDULES {
            return Err(ControlError::new(
                "schedule_storage_full",
                "too many schedules; delete one first",
            ));
        }
        save(&tx, &record)?;
        tx.commit().map_err(storage_error)?;
        self.publish_schedule(&record.id);
        self.scheduler.poke();
        Ok(serde_json::to_value(record).unwrap())
    }

    pub(super) fn schedule_update(&self, params: Option<Value>) -> Result<Value, ControlError> {
        let ScheduleUpdateParams { id, mut spec } = super::decode(params)?;
        let now = now_ms();
        let next_due = validate(&mut spec, now)?;
        let mut db = database(&self.schedules_path())?;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let mut record = load(&tx, &id)?;
        // Re-arming from now: an edit never replays occurrences it skipped.
        record.spec = spec;
        record.next_due = next_due.map(|due| DateMillis(due as f64));
        record.revision += 1;
        save(&tx, &record)?;
        tx.commit().map_err(storage_error)?;
        self.publish_schedule(&record.id);
        self.scheduler.poke();
        Ok(serde_json::to_value(record).unwrap())
    }

    pub(super) fn schedule_delete(&self, params: Option<Value>) -> Result<Value, ControlError> {
        let ScheduleIdParams { id } = super::decode(params)?;
        let db = database(&self.schedules_path())?;
        let removed = db
            .execute("DELETE FROM schedules_v1 WHERE id=?1", [&id])
            .map_err(storage_error)?;
        if removed == 0 {
            return Err(ControlError::not_found("schedule"));
        }
        self.publish_schedule(&id);
        self.scheduler.poke();
        Ok(json!({}))
    }

    /// Starts one run now. The schedule's next due time is unchanged.
    pub(super) fn schedule_run_now(&self, params: Option<Value>) -> Result<Value, ControlError> {
        let ScheduleIdParams { id } = super::decode(params)?;
        let now = now_ms();
        let mut db = database(&self.schedules_path())?;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let mut record = load(&tx, &id)?;
        push_run(
            &mut record,
            ScheduleRun {
                due_at: DateMillis(now as f64),
                fired_at: DateMillis(now as f64),
                outcome: ScheduleOutcome::Manual,
                late_reason: None,
                collapsed: 0,
                session_id: None,
                error: None,
            },
        );
        record.revision += 1;
        save(&tx, &record)?;
        tx.commit().map_err(storage_error)?;
        self.publish_schedule(&id);
        if let Ok(mut state) = self.scheduler.state.lock() {
            state.manual.push((record.clone(), now));
        }
        self.scheduler.wake.notify_all();
        Ok(serde_json::to_value(record).unwrap())
    }

    /// Starts the schedule runner. Called once, after the socket is bound.
    pub fn spawn_scheduler(self: &Arc<Self>) {
        let server = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name("diri-scheduler".into())
            .spawn(move || server.run_scheduler());
    }

    fn run_scheduler(self: Arc<Self>) {
        let mut previous = (now_ms(), Instant::now());
        loop {
            let now = now_ms();
            let instant = Instant::now();
            let monotonic_ms = instant.duration_since(previous.1).as_millis() as i64;
            if (now - previous.0) - monotonic_ms > SLEEP_GAP_MS
                && let Ok(mut slept) = self.scheduler.last_sleep.lock()
            {
                *slept = Some((previous.0 + monotonic_ms, now));
            }
            previous = (now, instant);

            let (reload, manual) = self
                .scheduler
                .state
                .lock()
                .map(|mut state| {
                    (
                        std::mem::take(&mut state.dirty) || !state.loaded,
                        std::mem::take(&mut state.manual),
                    )
                })
                .unwrap_or_default();
            for (record, fired_ms) in manual {
                self.start_run(record, fired_ms);
            }
            if reload {
                match database(&self.schedules_path()).and_then(|db| load_all(&db)) {
                    Ok(records) => self.scheduler.refresh(&records),
                    Err(error) => eprintln!("diri-scheduler: load failed: {}", error.message),
                }
            }

            let due: Vec<String> = self
                .scheduler
                .state
                .lock()
                .map(|state| {
                    state
                        .pending
                        .iter()
                        .filter(|(_, pending)| pending.next_due_ms <= now)
                        .map(|(id, _)| id.clone())
                        .collect()
                })
                .unwrap_or_default();
            if !due.is_empty() {
                for id in due {
                    self.claim_due(&id, now);
                }
                match database(&self.schedules_path()).and_then(|db| load_all(&db)) {
                    Ok(records) => self.scheduler.refresh(&records),
                    Err(error) => eprintln!("diri-scheduler: load failed: {}", error.message),
                }
            }

            let keep_awake = self.keep_awake_wanted(now);
            self.scheduler.set_keep_awake(keep_awake);

            let Ok(state) = self.scheduler.state.lock() else {
                return;
            };
            if state.dirty || !state.manual.is_empty() {
                continue;
            }
            let next = state.pending.values().map(|p| p.next_due_ms).min();
            let wait = match next {
                Some(next) => Some(
                    Duration::from_millis((next - now).max(0) as u64)
                        .min(MAX_WAIT)
                        .max(Duration::from_millis(50)),
                ),
                None if keep_awake => Some(MAX_WAIT),
                None => None,
            };
            match wait {
                Some(wait) => drop(self.scheduler.wake.wait_timeout(state, wait)),
                None => drop(self.scheduler.wake.wait(state)),
            }
        }
    }

    fn keep_awake_wanted(&self, now: i64) -> bool {
        let upcoming = self
            .scheduler
            .state
            .lock()
            .map(|state| {
                state.pending.values().any(|pending| {
                    pending.keep_awake && pending.next_due_ms - now <= KEEP_AWAKE_LEAD_MS
                })
            })
            .unwrap_or(false);
        let Ok(mut active) = self.scheduler.active.lock() else {
            return upcoming;
        };
        if !active.is_empty() {
            let registry = self.registry.lock().ok();
            active.retain(|run| {
                now - run.fired_ms < KEEP_AWAKE_RUN_CAP_MS
                    && still_working(
                        registry
                            .as_ref()
                            .and_then(|registry| registry.get(&run.session_id))
                            .map(|session| session.status()),
                    )
            });
        }
        upcoming || !active.is_empty()
    }

    /// Claims the occurrence that made `id` due, then starts its run.
    fn claim_due(self: &Arc<Self>, id: &str, now: i64) {
        let claimed = (|| -> Result<Option<(ScheduleRecord, Evaluation)>, ControlError> {
            let mut db = database(&self.schedules_path())?;
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage_error)?;
            let mut record = load(&tx, id)?;
            let Some(next_due) = record.next_due.filter(|_| record.spec.enabled) else {
                return Ok(None);
            };
            let Some(evaluation) = schedule::evaluate(
                &record.spec.when,
                next_due.0 as i64,
                now,
                record.spec.catch_up_window_ms,
            ) else {
                return Ok(None);
            };
            let late_reason = match evaluation.outcome {
                ScheduleOutcome::Late | ScheduleOutcome::Missed => {
                    self.scheduler.late_reason(evaluation.due_ms)
                }
                _ => None,
            };
            push_run(
                &mut record,
                ScheduleRun {
                    due_at: DateMillis(evaluation.due_ms as f64),
                    fired_at: DateMillis(now as f64),
                    outcome: evaluation.outcome,
                    late_reason,
                    collapsed: evaluation.collapsed,
                    session_id: None,
                    error: (evaluation.outcome == ScheduleOutcome::Failed)
                        .then(|| "the schedule's cron expression is invalid".to_string()),
                },
            );
            record.next_due = evaluation.next_due_ms.map(|due| DateMillis(due as f64));
            if record.next_due.is_none() {
                record.spec.enabled = false;
            }
            record.revision += 1;
            save(&tx, &record)?;
            tx.commit().map_err(storage_error)?;
            Ok(Some((record, evaluation)))
        })();
        match claimed {
            Ok(Some((record, evaluation))) => {
                self.publish_schedule(&record.id);
                if matches!(
                    evaluation.outcome,
                    ScheduleOutcome::OnTime | ScheduleOutcome::Late
                ) {
                    self.start_run(record, now);
                }
            }
            Ok(None) => {}
            Err(error) => eprintln!("diri-scheduler: claim {id} failed: {}", error.message),
        }
    }

    /// Spawns the run's session off the runner thread; the spawn can wait on
    /// Git, an account operation, or the agent's first prompt.
    fn start_run(self: &Arc<Self>, record: ScheduleRecord, fired_ms: i64) {
        let server = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name("diri-scheduled-run".into())
            .spawn(move || {
                let mut spawn = record.spec.spawn.clone();
                if spawn.title.is_none() {
                    spawn.title = Some(record.spec.title.clone());
                }
                let params = serde_json::to_value(&spawn).unwrap();
                let mut result = server.dispatch(Method::SESSION_SPAWN, Some(params.clone()));
                for _ in 1..SPAWN_ATTEMPTS {
                    match &result {
                        Err(error) if error.message.contains("Retry") || error.code == "busy" => {
                            std::thread::sleep(SPAWN_RETRY_DELAY);
                            result = server.dispatch(Method::SESSION_SPAWN, Some(params.clone()));
                        }
                        _ => break,
                    }
                }
                let session_id = result
                    .as_ref()
                    .ok()
                    .and_then(|value| value.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let Some(session_id) = &session_id
                    && record.spec.keep_awake
                    && let Ok(mut active) = server.scheduler.active.lock()
                {
                    active.push(ActiveRun {
                        session_id: session_id.clone(),
                        fired_ms,
                    });
                    server.scheduler.poke();
                }
                if let Err(error) = &result {
                    eprintln!(
                        "diri-scheduler: run of {} failed: {}",
                        record.id, error.message
                    );
                }
                server.record_run_result(&record.id, fired_ms, session_id, result.err());
            });
    }

    fn record_run_result(
        &self,
        id: &str,
        fired_ms: i64,
        session_id: Option<String>,
        error: Option<ControlError>,
    ) {
        let written = (|| -> Result<(), ControlError> {
            let mut db = database(&self.schedules_path())?;
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage_error)?;
            let mut record = load(&tx, id)?;
            let Some(run) = record
                .runs
                .iter_mut()
                .rev()
                .find(|run| run.fired_at.0 as i64 == fired_ms && run.session_id.is_none())
            else {
                return Ok(());
            };
            run.session_id = session_id.map(SessionId::new);
            if let Some(error) = error {
                run.outcome = ScheduleOutcome::Failed;
                run.error = Some(error.message);
            }
            record.revision += 1;
            save(&tx, &record)?;
            tx.commit().map_err(storage_error)
        })();
        match written {
            Ok(()) => self.publish_schedule(id),
            // Deleted meanwhile: nothing to record.
            Err(error) if error.code == "not_found" => {}
            Err(error) => eprintln!("diri-scheduler: record {id} failed: {}", error.message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diri_proto::AgentKind;

    fn spec(when: ScheduleWhen) -> ScheduleSpec {
        ScheduleSpec {
            title: " Morning triage ".into(),
            when,
            spawn: serde_json::from_value(json!({
                "kind": AgentKind::CLAUDE_CODE, "cwd": "/tmp", "initialPrompt": "triage"
            }))
            .unwrap(),
            catch_up_window_ms: diri_proto::schedules::DEFAULT_CATCH_UP_WINDOW_MS,
            keep_awake: false,
            enabled: true,
        }
    }

    #[test]
    fn validation_trims_and_arms() {
        let now = 1_800_000_000_000;
        let mut cron = spec(ScheduleWhen::Cron {
            expr: "0 9 * * *".into(),
        });
        let next = validate(&mut cron, now).unwrap().unwrap();
        assert!(next > now);
        assert_eq!(cron.title, "Morning triage");
    }

    #[test]
    fn validation_rejects_bad_schedules() {
        let now = 1_800_000_000_000;
        let mut past = spec(ScheduleWhen::Once {
            at: DateMillis((now - 3_600_000) as f64),
        });
        assert!(validate(&mut past, now).is_err());
        let mut no_prompt = spec(ScheduleWhen::Cron {
            expr: "0 9 * * *".into(),
        });
        no_prompt.spawn.initial_prompt = Some("  ".into());
        assert!(validate(&mut no_prompt, now).is_err());
        let mut bad_cron = spec(ScheduleWhen::Cron {
            expr: "every morning".into(),
        });
        bad_cron.enabled = false;
        assert!(validate(&mut bad_cron, now).is_err());
    }

    #[test]
    fn disabled_schedules_are_not_armed() {
        let mut disabled = spec(ScheduleWhen::Cron {
            expr: "0 9 * * *".into(),
        });
        disabled.enabled = false;
        assert_eq!(validate(&mut disabled, 0).unwrap(), None);
    }

    #[test]
    fn run_history_is_bounded() {
        let mut record = ScheduleRecord {
            id: random_id(),
            spec: spec(ScheduleWhen::Cron {
                expr: "* * * * *".into(),
            }),
            created_at: DateMillis(0.0),
            revision: 0,
            next_due: None,
            runs: Vec::new(),
        };
        for index in 0..(MAX_SCHEDULE_RUNS + 5) {
            push_run(
                &mut record,
                ScheduleRun {
                    due_at: DateMillis(index as f64),
                    fired_at: DateMillis(index as f64),
                    outcome: ScheduleOutcome::OnTime,
                    late_reason: None,
                    collapsed: 0,
                    session_id: None,
                    error: None,
                },
            );
        }
        assert_eq!(record.runs.len(), MAX_SCHEDULE_RUNS);
        assert_eq!(record.runs[0].due_at.0, 5.0);
    }

    #[test]
    fn late_reason_prefers_not_running_then_observed_sleep() {
        let scheduler = Scheduler::default();
        assert_eq!(
            scheduler.late_reason(scheduler.started_ms - 1),
            Some(LateReason::NotRunning)
        );
        let later = scheduler.started_ms + 60_000;
        assert_eq!(scheduler.late_reason(later), None);
        *scheduler.last_sleep.lock().unwrap() = Some((later - 1_000, later + 1_000));
        assert_eq!(scheduler.late_reason(later), Some(LateReason::Asleep));
    }

    #[test]
    fn store_round_trips_and_isolates_bad_rows() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("schedules.sqlite");
        let db = database(&path).unwrap();
        let record = ScheduleRecord {
            id: "sched_a".into(),
            spec: spec(ScheduleWhen::Cron {
                expr: "0 9 * * *".into(),
            }),
            created_at: DateMillis(1.0),
            revision: 0,
            next_due: Some(DateMillis(2.0)),
            runs: Vec::new(),
        };
        save(&db, &record).unwrap();
        db.execute(
            "INSERT INTO schedules_v1 VALUES ('sched_bad', '{not json')",
            [],
        )
        .unwrap();
        let all = load_all(&db).unwrap();
        assert_eq!(all, vec![record]);
    }
}
