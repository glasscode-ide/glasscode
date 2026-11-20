//! The time for everything Manager v2 adds (rule 4 of the slice plan): one place to ask, so a test
//! can run on a clock of its own, such as the test kit's virtual clock with sleep, jumps and DST
//! changes. v1 code keeps `model::now`.

use std::sync::{Arc, RwLock};

/// Where the time comes from.
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch (UTC).
    fn now_ms(&self) -> u64;

    /// What this machine's wall clock shows at `at_ms`, as minutes since 1970-01-01 00:00 on that
    /// wall clock: UTC plus the time zone's offset at that moment, so a DST change is a jump of an
    /// hour. [`minute_of_day`] and [`weekday`] read it. None when the system cannot say.
    fn local_minutes(&self, at_ms: u64) -> Option<i64>;
}

/// The machine's own clock and time zone.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
    }

    fn local_minutes(&self, at_ms: u64) -> Option<i64> {
        let local = crate::sys::local_time((at_ms / 1000) as i64)?;
        Some(days_from_civil(local.year, local.month, local.day) * 1440 + i64::from(local.hour) * 60 + i64::from(local.minute))
    }
}

/// The clocks tests put in place ([`use_test_clock`]), each with its guard's token; the newest
/// answers. None: [`SystemClock`].
static CLOCKS: RwLock<Vec<(u64, Arc<dyn Clock>)>> = RwLock::new(Vec::new());

/// The token of the next guard.
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn current() -> Option<Arc<dyn Clock>> {
    // A clone, so a clock that asks the time itself never waits on this lock.
    CLOCKS.read().unwrap_or_else(|poisoned| poisoned.into_inner()).last().map(|(_, clock)| clock.clone())
}

/// Now, in milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    match current() {
        Some(clock) => clock.now_ms(),
        None => SystemClock.now_ms(),
    }
}

/// The local wall time of `at_ms` ([`Clock::local_minutes`]).
pub fn local_minutes(at_ms: u64) -> Option<i64> {
    match current() {
        Some(clock) => clock.local_minutes(at_ms),
        None => SystemClock.local_minutes(at_ms),
    }
}

/// The local time's offset from UTC now, in minutes (`local_minutes` less UTC's), if known.
pub fn offset_minutes(at_ms: u64) -> Option<i64> {
    local_minutes(at_ms).map(|local| local - (at_ms / 60_000) as i64)
}

/// An offset in words: `UTC`, `UTC+2`, `UTC−5`, `UTC+5:30`.
pub fn offset_words(minutes: i64) -> String {
    if minutes == 0 {
        return "UTC".into();
    }
    let sign = if minutes > 0 { "+" } else { "−" };
    let (h, m) = (minutes.unsigned_abs() / 60, minutes.unsigned_abs() % 60);
    if m == 0 { format!("UTC{sign}{h}") } else { format!("UTC{sign}{h}:{m:02}") }
}

/// What the person is told when the time zone changes (M41 step 1): schedules follow it.
pub fn zone_note(before: i64, now: i64) -> String {
    format!("Your time zone is {} now (it was {}). Schedules run at their times there.", offset_words(now), offset_words(before))
}

/// The minute of the local day (0 to 1439) of a [`Clock::local_minutes`] value.
pub fn minute_of_day(local_minutes: i64) -> u16 {
    local_minutes.rem_euclid(1440) as u16
}

/// The day of the week of a [`Clock::local_minutes`] value: 0 = Sunday … 6 = Saturday, as
/// `sys::LocalTime`.
pub fn weekday(local_minutes: i64) -> u32 {
    // 1970-01-01 was a Thursday.
    (local_minutes.div_euclid(1440) + 4).rem_euclid(7) as u32
}

/// Tests, and the test kit's virtual clock: `clock` answers for the whole process until its guard
/// is dropped. Guards may be dropped in any order: each takes away its own clock, and the newest
/// one left answers (the system clock when none is). The clock is the process's, so tests that use
/// it run one at a time, as the core's do (`--test-threads=1`).
#[must_use = "the clock goes back when the guard is dropped"]
pub fn use_test_clock(clock: Arc<dyn Clock>) -> TestClockGuard {
    let token = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    CLOCKS.write().unwrap_or_else(|poisoned| poisoned.into_inner()).push((token, clock));
    TestClockGuard { token }
}

/// Takes the clock of its [`use_test_clock`] away when dropped.
pub struct TestClockGuard {
    token: u64,
}

impl Drop for TestClockGuard {
    fn drop(&mut self) {
        CLOCKS.write().unwrap_or_else(|poisoned| poisoned.into_inner()).retain(|(token, _)| *token != self.token);
    }
}

/// The system clock moved by a fixed offset: it runs at the wall clock's pace from another time, in
/// this machine's time zone.
pub struct OffsetClock {
    offset_ms: i64,
}

impl OffsetClock {
    /// A clock that shows `start_ms` now.
    pub fn starting_at(start_ms: u64) -> OffsetClock {
        OffsetClock { offset_ms: start_ms as i64 - SystemClock.now_ms() as i64 }
    }
}

impl Clock for OffsetClock {
    fn now_ms(&self) -> u64 {
        (SystemClock.now_ms() as i64 + self.offset_ms).max(0) as u64
    }

    fn local_minutes(&self, at_ms: u64) -> Option<i64> {
        SystemClock.local_minutes(at_ms)
    }
}

/// The variable of a debug build's start time (`script/journeys.sh`): Unix milliseconds.
pub const DEBUG_CLOCK: &str = "GLASSCODE_DEBUG_CLOCK";

/// Debug builds' journeys: with `GLASSCODE_DEBUG_CLOCK=<unix ms>`, this process's clock starts at
/// that time and runs on at the wall clock's pace ([`OffsetClock`]), so a scratch app can start a
/// minute before quiet hours end and see them end. Nothing in a release build, or without it.
pub fn debug_clock_from_env() -> Option<TestClockGuard> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let start: u64 = std::env::var(DEBUG_CLOCK).ok()?.trim().parse().ok()?;
    log::info!("clock: a debug clock that started at {start} ms ({DEBUG_CLOCK})");
    Some(use_test_clock(Arc::new(OffsetClock::starting_at(start))))
}

/// Days from 1970-01-01 to a date of the proleptic Gregorian calendar (H. Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let month = i64::from(month);
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An offset clock shows its start now, then runs on with the wall clock.
    #[test]
    fn an_offset_clock_runs_from_its_start() {
        let start = 1_795_000_000_000;
        let clock = OffsetClock::starting_at(start);
        let now = clock.now_ms();
        assert!(now >= start && now < start + 5_000, "{now}");
        assert_eq!(clock.local_minutes(start), SystemClock.local_minutes(start));
    }

    /// A clock that stands still, in a zone `offset` minutes from UTC.
    struct Fixed {
        ms: u64,
        offset: i64,
    }

    impl Clock for Fixed {
        fn now_ms(&self) -> u64 {
            self.ms
        }

        fn local_minutes(&self, at_ms: u64) -> Option<i64> {
            Some((at_ms / 60_000) as i64 + self.offset)
        }
    }

    #[test]
    fn a_test_clock_answers_until_its_guard_is_dropped() {
        let system = now_ms();
        {
            let _outer = use_test_clock(Arc::new(Fixed { ms: 1_000, offset: 0 }));
            assert_eq!(now_ms(), 1_000);
            {
                let _inner = use_test_clock(Arc::new(Fixed { ms: 2_000, offset: 60 }));
                assert_eq!((now_ms(), local_minutes(0)), (2_000, Some(60)));
            }
            // The one before it again.
            assert_eq!((now_ms(), local_minutes(0)), (1_000, Some(0)));
        }
        let after = now_ms();
        assert!(after >= system && after - system < 60_000, "the system clock again: {system} then {after}");
    }

    /// Guards dropped out of order: each takes away its own clock, and none is left in place
    /// (review of PR 78, E8).
    #[test]
    fn guards_dropped_out_of_order_leave_no_test_clock() {
        let system = now_ms();
        let first = use_test_clock(Arc::new(Fixed { ms: 1_000, offset: 0 }));
        let second = use_test_clock(Arc::new(Fixed { ms: 2_000, offset: 0 }));
        drop(first);
        assert_eq!(now_ms(), 2_000, "the newest is still in place");
        let third = use_test_clock(Arc::new(Fixed { ms: 3_000, offset: 0 }));
        drop(second);
        assert_eq!(now_ms(), 3_000);
        drop(third);
        let after = now_ms();
        assert!(after >= system && after - system < 60_000, "the system clock again: {system} then {after}");
    }

    #[test]
    fn local_minutes_are_the_systems_local_time() {
        let now = SystemClock.now_ms();
        let local = crate::sys::local_time((now / 1000) as i64).expect("the system knows its local time");
        let minutes = SystemClock.local_minutes(now).unwrap();
        assert_eq!(u32::from(minute_of_day(minutes)), local.hour * 60 + local.minute);
        assert_eq!(weekday(minutes), local.weekday);
        // The offset from UTC is whole minutes, and less than a day.
        let offset = minutes - (now / 60_000) as i64;
        assert!(offset.abs() < 1440, "{offset}");
    }

    #[test]
    fn days_minutes_and_weekdays_of_known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        // Thursday 2026-11-19, 09:30 on the wall clock.
        let minutes = days_from_civil(2026, 11, 19) * 1440 + 9 * 60 + 30;
        assert_eq!((minute_of_day(minutes), weekday(minutes)), (570, 4));
        // Five hours west of UTC at the epoch: Wednesday 19:00.
        assert_eq!((minute_of_day(-300), weekday(-300)), (1140, 3));
    }

    /// M41 step 1: a change of zone is said in words.
    #[test]
    fn a_zone_is_said_in_words() {
        assert_eq!(offset_words(0), "UTC");
        assert_eq!(offset_words(120), "UTC+2");
        assert_eq!(offset_words(-300), "UTC−5");
        assert_eq!(offset_words(330), "UTC+5:30");
        assert_eq!(zone_note(60, 120), "Your time zone is UTC+2 now (it was UTC+1). Schedules run at their times there.");
    }

}
