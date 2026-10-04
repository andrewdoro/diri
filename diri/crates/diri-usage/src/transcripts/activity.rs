//! Day-by-day activity for the Usage calendar and weekly rhythm heatmaps.
//!
//! Unlike the dashboard, which reads UTC days, these bins follow the local
//! calendar: each epoch hour is placed on the local day and hour it began in,
//! with the zone offset taken at that hour, so DST days hold 23 or 25 hours.
//! Remote machines report UTC-day totals only; they keep their date and add
//! tokens and cost, never active hours.
use super::dashboard::UsageHistory;
use diri_proto::remote_pty::TranscriptUsageResult;
use std::collections::BTreeMap;

/// Days of transcript history the ledger keeps; older days have no data.
pub const HISTORY_DAYS: i64 = super::store::RETENTION_DAYS;

/// Weeks the calendar spans: every retained day, plus the current week.
pub const CALENDAR_WEEKS: usize = (HISTORY_DAYS as usize).div_ceil(7) + 1;

/// What a calendar cell's strength reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActivityMetric {
    /// Hours with any agent activity in hourly (local) transcripts.
    #[default]
    Hours,
    Tokens,
    Cost,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ActivityDay {
    /// Local epoch day (days since 1970-01-01 on the local calendar).
    pub day: i64,
    /// Distinct hours with activity, from hourly sources only.
    pub hours: u8,
    /// Per provider: Claude, Codex, Cursor.
    pub tokens: [i64; 3],
    pub cost: [f64; 3],
}

impl ActivityDay {
    pub fn total_tokens(&self) -> i64 {
        self.tokens.iter().sum()
    }

    pub fn total_cost(&self) -> f64 {
        self.cost.iter().sum()
    }

    pub fn active(&self) -> bool {
        self.hours > 0 || self.total_tokens() > 0 || self.total_cost() > 0.0
    }

    pub fn value(&self, metric: ActivityMetric) -> f64 {
        match metric {
            ActivityMetric::Hours => f64::from(self.hours),
            ActivityMetric::Tokens => self.total_tokens() as f64,
            ActivityMetric::Cost => self.total_cost(),
        }
    }
}

/// One weekday-and-hour slot of the weekly rhythm.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RhythmCell {
    /// Active hours that fell in this slot across the window.
    pub hours: u32,
    pub tokens: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Activity {
    /// Monday-zero weekday the calendar's rows start on.
    pub week_start: u8,
    /// The first cell: a `week_start` day.
    pub first_day: i64,
    /// The local day containing `now`: the last cell.
    pub today: i64,
    /// The earliest local day the ledger can still hold.
    pub history_start: i64,
    /// Every day from `first_day` through `today`, empty days included.
    pub days: Vec<ActivityDay>,
    /// Rows follow the calendar's (relative to `week_start`), columns are
    /// local hours.
    pub rhythm: [[RhythmCell; 24]; 7],
    /// How many days of each row the rhythm window holds.
    pub rhythm_days: [u32; 7],
    /// Whether any hourly source contributed. Without one there are no
    /// active hours and no rhythm to read.
    pub hourly: bool,
}

/// Monday-zero weekday of an epoch day; day 0 was a Thursday.
pub fn weekday(day: i64) -> u8 {
    (day + 3).rem_euclid(7) as u8
}

/// Row of `day` in a week starting on `week_start` (Monday-zero).
pub fn row(day: i64, week_start: u8) -> usize {
    (i64::from(weekday(day)) - i64::from(week_start)).rem_euclid(7) as usize
}

/// Gregorian (year, month, day) of an epoch day.
pub fn civil(day: i64) -> (i32, i32, i32) {
    super::store::civil_from_days(day)
}

/// Epoch day of a Gregorian date.
pub fn civil_day(year: i32, month: i32, day: i32) -> i64 {
    super::timestamp::days_from_civil(year, month, day)
}

/// Seconds east of UTC on this machine's zone at `unix`, DST included.
#[cfg(unix)]
pub fn system_offset(unix: i64) -> i64 {
    let time = unix as libc::time_t;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: both pointers are valid for the call; `localtime_r` is the
    // reentrant form and writes only into `tm`.
    let result = unsafe { libc::localtime_r(&raw const time, tm.as_mut_ptr()) };
    if result.is_null() {
        return 0;
    }
    // SAFETY: a non-null result means `tm` was initialized.
    // `c_long` is narrower than i64 on 32-bit targets.
    #[allow(clippy::useless_conversion)]
    i64::from(unsafe { tm.assume_init() }.tm_gmtoff)
}

#[cfg(not(unix))]
pub fn system_offset(_unix: i64) -> i64 {
    0
}

fn local_day(unix: i64, offset: &dyn Fn(i64) -> i64) -> i64 {
    (unix + offset(unix)).div_euclid(86_400)
}

/// Bins `local` (hourly) and `remote` (daily) usage into the `weeks` most
/// recent local weeks ending with the one holding `now`.
pub fn build(
    local: Option<&UsageHistory>,
    remote: &[&TranscriptUsageResult],
    now: i64,
    weeks: usize,
    week_start: u8,
    offset: &dyn Fn(i64) -> i64,
) -> Activity {
    let week_start = week_start % 7;
    let weeks = weeks.max(1);
    let today = local_day(now, offset);
    let first_day = today - row(today, week_start) as i64 - (weeks as i64 - 1) * 7;
    let history_start = local_day(now - HISTORY_DAYS * 86_400, offset);
    let mut activity = Activity {
        week_start,
        first_day,
        today,
        history_start,
        days: (first_day..=today)
            .map(|day| ActivityDay {
                day,
                ..ActivityDay::default()
            })
            .collect(),
        rhythm: [[RhythmCell::default(); 24]; 7],
        rhythm_days: [0; 7],
        hourly: local.is_some(),
    };
    for day in first_day.max(history_start)..=today {
        activity.rhythm_days[row(day, week_start)] += 1;
    }

    if let Some(history) = local {
        // Local days start up to 14 hours either side of UTC midnight.
        let start_hour = (first_day - 1) * 24;
        let end_hour = now.div_euclid(3_600);
        let mut hours = BTreeMap::<i64, ([i64; 3], [f64; 3])>::new();
        for (provider, models) in [&history.claude, &history.codex, &history.cursor]
            .into_iter()
            .enumerate()
        {
            for model in models.values() {
                for (&hour, detail) in model.range(start_hour..=end_hour) {
                    let slot = hours.entry(hour).or_default();
                    slot.0[provider] += detail.totals().total_tokens();
                    slot.1[provider] += detail.tokens.c;
                }
            }
        }
        for (hour, (tokens, cost)) in hours {
            let unix = hour * 3_600;
            let local = unix + offset(unix);
            let day = local.div_euclid(86_400);
            let Some(index) = activity.index(day) else {
                continue;
            };
            let entry = &mut activity.days[index];
            for provider in 0..3 {
                entry.tokens[provider] += tokens[provider];
                entry.cost[provider] += cost[provider];
            }
            let total: i64 = tokens.iter().sum();
            if total <= 0 && cost.iter().sum::<f64>() <= 0.0 {
                continue;
            }
            entry.hours = entry.hours.saturating_add(1);
            let cell = &mut activity.rhythm[row(day, week_start)]
                [(local.rem_euclid(86_400) / 3_600) as usize];
            cell.hours += 1;
            cell.tokens += total;
        }
    }

    for result in remote {
        for bucket in &result.buckets {
            let provider = match bucket.provider.as_str() {
                "claude" => 0,
                "codex" => 1,
                _ => continue,
            };
            // A remote day is a UTC date; it keeps its label here rather
            // than sliding to the local day its UTC midnight falls on.
            let Some(index) = activity.index(bucket.day) else {
                continue;
            };
            let entry = &mut activity.days[index];
            entry.tokens[provider] +=
                bucket.input + bucket.output + bucket.cache_read + bucket.cache_write;
            entry.cost[provider] += bucket.estimated_usd;
        }
    }
    activity
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Streaks {
    /// Consecutive active days ending today, or yesterday when today has
    /// nothing yet.
    pub current: usize,
    pub longest: usize,
}

impl Activity {
    pub fn index(&self, day: i64) -> Option<usize> {
        (self.first_day..=self.today)
            .contains(&day)
            .then(|| (day - self.first_day) as usize)
    }

    pub fn weeks(&self) -> usize {
        self.days.len().div_ceil(7)
    }

    /// (week column, weekday row) of the day at `index`.
    pub fn place(index: usize) -> (usize, usize) {
        (index / 7, index % 7)
    }

    /// Whether the ledger could hold the day at `index` at all.
    pub fn retained(&self, index: usize) -> bool {
        self.days
            .get(index)
            .is_some_and(|day| day.day >= self.history_start)
    }

    pub fn active_days(&self) -> usize {
        self.days.iter().filter(|day| day.active()).count()
    }

    pub fn active_hours(&self) -> u32 {
        self.days.iter().map(|day| u32::from(day.hours)).sum()
    }

    pub fn streaks(&self) -> Streaks {
        let mut longest = 0;
        let mut run = 0;
        for day in &self.days {
            if day.active() {
                run += 1;
                longest = longest.max(run);
            } else {
                run = 0;
            }
        }
        let mut days = self.days.iter().rev().peekable();
        if days.peek().is_some_and(|day| !day.active()) {
            days.next();
        }
        let current = days.take_while(|day| day.active()).count();
        Streaks { current, longest }
    }

    /// Column of each month's first appearance on the calendar's top row,
    /// as (week, month 1–12). A month whose first week is cut off by the
    /// grid's start is labeled only if it has room before the next one.
    pub fn month_columns(&self) -> Vec<(usize, i32)> {
        let mut columns: Vec<(usize, i32)> = Vec::new();
        let mut last = None;
        for week in 0..self.weeks() {
            let (_, month, _) = civil(self.first_day + week as i64 * 7);
            if last != Some(month) {
                columns.push((week, month));
                last = Some(month);
            }
        }
        if columns.len() > 1 && columns[1].0 < 3 {
            columns.remove(0);
        }
        columns
    }
}

/// Five steps of strength from quantiles of the nonzero values: zero stays
/// empty and each nonzero value lands by its mid-rank, so one heavy day
/// cannot wash every other day out to the faintest step.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IntensityScale {
    sorted: Vec<f64>,
}

impl IntensityScale {
    pub fn new(values: impl IntoIterator<Item = f64>) -> Self {
        let mut sorted: Vec<f64> = values
            .into_iter()
            .filter(|value| value.is_finite() && *value > 0.0)
            .collect();
        sorted.sort_by(f64::total_cmp);
        Self { sorted }
    }

    /// 0 for nothing, then 1 (least) through 4 (most).
    pub fn level(&self, value: f64) -> u8 {
        if value.is_nan() || value <= 0.0 || self.sorted.is_empty() {
            return 0;
        }
        let below = self.sorted.partition_point(|other| *other < value);
        let equal = self.sorted.partition_point(|other| *other <= value) - below;
        let rank = (below as f64 + equal as f64 * 0.5) / self.sorted.len() as f64;
        1 + ((rank * 4.0).floor() as u8).min(3)
    }
}

#[cfg(test)]
mod tests {
    use super::super::UsageHourAgg;
    use super::super::dashboard::record;
    use super::super::timestamp::days_from_civil;
    use super::*;

    const MONDAY: u8 = 0;
    const SUNDAY: u8 = 6;

    fn utc(_: i64) -> i64 {
        0
    }

    fn seed(history: &mut UsageHistory, hour: i64, tokens: i64) {
        record(
            &mut history.claude,
            "claude-sonnet",
            hour,
            UsageHourAgg {
                i: tokens,
                o: 0,
                cr: 0,
                cw: 0,
                c: tokens as f64 / 1_000.0,
            },
            None,
            0,
        );
    }

    fn unix(year: i32, month: i32, day: i32, hour: i64) -> i64 {
        days_from_civil(year, month, day) * 86_400 + hour * 3_600
    }

    #[test]
    fn weekdays_follow_the_epoch() {
        // 1970-01-01 was a Thursday; 2026-10-04 is a Sunday.
        assert_eq!(weekday(0), 3);
        assert_eq!(weekday(days_from_civil(2026, 10, 4)), 6);
        assert_eq!(row(days_from_civil(2026, 10, 4), MONDAY), 6);
        assert_eq!(row(days_from_civil(2026, 10, 4), SUNDAY), 0);
    }

    #[test]
    fn grid_starts_on_the_week_start_and_ends_today() {
        let now = unix(2026, 10, 1, 12); // Thursday
        for week_start in [MONDAY, SUNDAY, 5] {
            let activity = build(None, &[], now, CALENDAR_WEEKS, week_start, &utc);
            assert_eq!(weekday(activity.first_day), week_start);
            assert_eq!(activity.today, days_from_civil(2026, 10, 1));
            assert_eq!(activity.weeks(), CALENDAR_WEEKS);
            let last = activity.days.len() - 1;
            assert_eq!(Activity::place(last).0, CALENDAR_WEEKS - 1);
            assert_eq!(Activity::place(last).1, row(activity.today, week_start));
        }
        // A Monday start puts Thursday in the fourth row; a Sunday start in
        // the fifth.
        let monday = build(None, &[], now, 2, MONDAY, &utc);
        let sunday = build(None, &[], now, 2, SUNDAY, &utc);
        assert_eq!(monday.days.len(), 7 + 4);
        assert_eq!(sunday.days.len(), 7 + 5);
    }

    #[test]
    fn calendar_covers_every_retained_day() {
        assert_eq!(CALENDAR_WEEKS, 27);
        let now = unix(2026, 10, 4, 12);
        let activity = build(None, &[], now, CALENDAR_WEEKS, MONDAY, &utc);
        assert!(activity.first_day <= activity.history_start);
        assert!(activity.retained(activity.days.len() - 1));
        assert!(!activity.retained(0));
    }

    #[test]
    fn empty_days_are_present_and_inactive() {
        let mut history = UsageHistory::default();
        let now = unix(2026, 10, 4, 18);
        seed(&mut history, unix(2026, 10, 1, 10) / 3_600, 500);
        let activity = build(Some(&history), &[], now, 2, MONDAY, &utc);
        assert_eq!(activity.days.len(), 7 + 7);
        assert_eq!(activity.active_days(), 1);
        let day = activity.days[activity.index(days_from_civil(2026, 10, 1)).unwrap()];
        assert_eq!((day.hours, day.total_tokens()), (1, 500));
        let empty = activity.days[activity.index(days_from_civil(2026, 10, 2)).unwrap()];
        assert!(!empty.active());
        assert_eq!(empty.value(ActivityMetric::Tokens), 0.0);
    }

    #[test]
    fn hours_land_on_the_local_day_they_began() {
        let mut history = UsageHistory::default();
        // 22:00 UTC on Oct 1 is 07:00 on Oct 2 in UTC+9 and 18:00 on Oct 1
        // in UTC-4.
        let hour = unix(2026, 10, 1, 22) / 3_600;
        seed(&mut history, hour, 100);
        let now = unix(2026, 10, 4, 12);
        let tokyo = build(Some(&history), &[], now, 2, MONDAY, &|_| 9 * 3_600);
        let index = tokyo.index(days_from_civil(2026, 10, 2)).unwrap();
        assert_eq!(tokyo.days[index].hours, 1);
        assert_eq!(
            tokyo.rhythm[row(days_from_civil(2026, 10, 2), MONDAY)][7].hours,
            1
        );
        let new_york = build(Some(&history), &[], now, 2, MONDAY, &|_| -4 * 3_600);
        let index = new_york.index(days_from_civil(2026, 10, 1)).unwrap();
        assert_eq!(new_york.days[index].hours, 1);
        assert_eq!(
            new_york.rhythm[row(days_from_civil(2026, 10, 1), MONDAY)][18].hours,
            1
        );
    }

    /// US Eastern time for 2026: EDT from Mar 8 07:00 UTC to Nov 1 06:00 UTC.
    fn eastern(at: i64) -> i64 {
        if (unix(2026, 3, 8, 7)..unix(2026, 11, 1, 6)).contains(&at) {
            -4 * 3_600
        } else {
            -5 * 3_600
        }
    }

    fn every_hour_of(history: &mut UsageHistory, from: i64, to: i64) {
        for hour in from / 3_600..to / 3_600 {
            seed(history, hour, 1);
        }
    }

    #[test]
    fn dst_days_hold_twenty_three_and_twenty_five_hours() {
        let mut history = UsageHistory::default();
        // Local Mar 8 runs 05:00 UTC Mar 8 to 04:00 UTC Mar 9 (23 hours);
        // local Nov 1 runs 04:00 UTC Nov 1 to 05:00 UTC Nov 2 (25 hours).
        every_hour_of(&mut history, unix(2026, 3, 7, 0), unix(2026, 3, 10, 0));
        every_hour_of(&mut history, unix(2026, 10, 31, 0), unix(2026, 11, 3, 0));
        let spring = build(
            Some(&history),
            &[],
            unix(2026, 3, 9, 20),
            2,
            MONDAY,
            &eastern,
        );
        let day = |activity: &Activity, y, m, d| {
            activity.days[activity.index(days_from_civil(y, m, d)).unwrap()].hours
        };
        assert_eq!(day(&spring, 2026, 3, 8), 23);
        assert_eq!(day(&spring, 2026, 3, 7), 24);
        let fall = build(
            Some(&history),
            &[],
            unix(2026, 11, 2, 20),
            2,
            MONDAY,
            &eastern,
        );
        assert_eq!(day(&fall, 2026, 11, 1), 25);
        assert_eq!(day(&fall, 2026, 11, 2), 16);
        // 01:00 happens twice on Nov 1; 02:00 never happens on Mar 8.
        let sunday = row(days_from_civil(2026, 11, 1), MONDAY);
        assert_eq!(fall.rhythm[sunday][1].hours, 2);
        assert_eq!(spring.rhythm[sunday][2].hours, 0);
    }

    #[test]
    fn week_boundaries_follow_the_week_start() {
        let mut history = UsageHistory::default();
        // Sunday 2026-09-27 at 23:00 local in UTC+2.
        seed(&mut history, unix(2026, 9, 27, 21) / 3_600, 10);
        let now = unix(2026, 10, 1, 12);
        let plus_two = |_| 2 * 3_600;
        let monday = build(Some(&history), &[], now, 2, MONDAY, &plus_two);
        let index = monday.index(days_from_civil(2026, 9, 27)).unwrap();
        assert_eq!(Activity::place(index), (0, 6), "last row of the first week");
        let sunday = build(Some(&history), &[], now, 2, SUNDAY, &plus_two);
        let index = sunday.index(days_from_civil(2026, 9, 27)).unwrap();
        assert_eq!(
            Activity::place(index),
            (1, 0),
            "first row of the second week"
        );
        assert_eq!(sunday.rhythm[0][23].hours, 1);
    }

    #[test]
    fn leap_days_take_their_own_cell() {
        let mut history = UsageHistory::default();
        seed(&mut history, unix(2028, 2, 29, 12) / 3_600, 7);
        seed(&mut history, unix(2028, 3, 1, 12) / 3_600, 9);
        let activity = build(Some(&history), &[], unix(2028, 3, 5, 12), 2, MONDAY, &utc);
        let leap = activity.index(days_from_civil(2028, 2, 29)).unwrap();
        assert_eq!(civil(activity.days[leap].day), (2028, 2, 29));
        assert_eq!(activity.days[leap].total_tokens(), 7);
        assert_eq!(activity.days[leap + 1].total_tokens(), 9);
        assert_eq!(civil(activity.days[leap + 1].day), (2028, 3, 1));
        assert_eq!(civil(days_from_civil(2100, 3, 1) - 1), (2100, 2, 28));
    }

    #[test]
    fn remote_days_keep_their_date_and_add_no_hours() {
        use diri_proto::remote_pty::TranscriptUsageBucket;
        let result = TranscriptUsageResult {
            source_id: "a".repeat(32),
            collected_at: 0,
            buckets: vec![TranscriptUsageBucket {
                provider: "codex".into(),
                model: "gpt-5.4".into(),
                day: days_from_civil(2026, 10, 2),
                input: 10,
                output: 5,
                cache_read: 3,
                cache_write: 2,
                estimated_usd: 1.5,
                ..Default::default()
            }],
        };
        let activity = build(None, &[&result], unix(2026, 10, 4, 12), 2, MONDAY, &|_| {
            -7 * 3_600
        });
        let day = activity.days[activity.index(days_from_civil(2026, 10, 2)).unwrap()];
        assert_eq!(day.tokens, [0, 20, 0]);
        assert_eq!(day.cost[1], 1.5);
        assert_eq!(day.hours, 0);
        assert!(day.active());
        assert!(!activity.hourly);
    }

    #[test]
    fn quantile_levels_spread_and_hold_ties() {
        let scale = IntensityScale::new((1..=8).map(f64::from));
        let levels: Vec<u8> = (1..=8).map(|v| scale.level(f64::from(v))).collect();
        assert_eq!(levels, [1, 1, 2, 2, 3, 3, 4, 4]);
        assert_eq!(scale.level(0.0), 0);
        assert_eq!(scale.level(f64::NAN), 0);
        assert_eq!(scale.level(100.0), 4);
        // One huge day does not flatten the rest into the faintest step.
        let skewed = IntensityScale::new([1.0, 2.0, 3.0, 1_000.0]);
        assert_eq!(skewed.level(3.0), 3);
        assert_eq!(skewed.level(1_000.0), 4);
        let same = IntensityScale::new([5.0, 5.0, 5.0, 0.0]);
        assert_eq!(same.level(5.0), 3);
        assert_eq!(IntensityScale::new([]).level(1.0), 0);
    }

    #[test]
    fn streaks_tolerate_a_quiet_today() {
        let mut history = UsageHistory::default();
        let now = unix(2026, 10, 4, 8);
        for back in [1, 2, 3, 6, 7, 8, 9] {
            seed(&mut history, unix(2026, 10, 4 - back, 10) / 3_600, 1);
        }
        let activity = build(Some(&history), &[], now, 2, MONDAY, &utc);
        assert_eq!(
            activity.streaks(),
            Streaks {
                current: 3,
                longest: 4
            }
        );
        assert_eq!(activity.active_hours(), 7);
    }

    #[test]
    fn month_labels_mark_the_first_week_of_each_month() {
        let activity = build(None, &[], unix(2026, 10, 4, 12), 10, MONDAY, &utc);
        // First column starts Monday 2026-07-27: July has one column before
        // August, so it is dropped for room. October's first Monday is not
        // on the grid yet.
        assert_eq!(civil(activity.first_day), (2026, 7, 27));
        let months: Vec<i32> = activity
            .month_columns()
            .iter()
            .map(|(_, month)| *month)
            .collect();
        assert_eq!(months, [8, 9]);
        assert_eq!(activity.month_columns()[0].0, 1);
    }
}
