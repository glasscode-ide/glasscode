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

/// The clock a test put in place ([`use_test_clock`]); none: [`SystemClock`].
static CLOCK: RwLock<Option<Arc<dyn Clock>>> = RwLock::new(None);

fn current() -> Option<Arc<dyn Clock>> {
    // A clone, so a clock that asks the time itself never waits on this lock.
    CLOCK.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
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

/// Tests, and the test kit's virtual clock: `clock` answers for the whole process until the guard
/// is dropped, then the one before it does again. Tests that use it run one at a time, as the
/// core's do (`--test-threads=1`).
#[must_use = "the clock goes back when the guard is dropped"]
pub fn use_test_clock(clock: Arc<dyn Clock>) -> TestClockGuard {
    let before = CLOCK.write().unwrap_or_else(|poisoned| poisoned.into_inner()).replace(clock);
    TestClockGuard { before }
}

/// Puts the clock before [`use_test_clock`] back when dropped.
pub struct TestClockGuard {
    before: Option<Arc<dyn Clock>>,
}

impl Drop for TestClockGuard {
    fn drop(&mut self) {
        *CLOCK.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = self.before.take();
    }
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
}
