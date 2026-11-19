//! The virtual clock (M02 step 3): a [`Clock`] that a test moves itself. Time stands still until the
//! test advances it, the machine can sleep through hours, the wall clock can be set back, and a time
//! zone with daylight saving time changes its offset on the days its rules say. Every change but a
//! plain advance is logged, so a simulation can tell the runtime about it as the app would (a wake,
//! a clock jump, a new time zone).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::clock::{Clock, TestClockGuard};

pub const SECOND: u64 = 1_000;
pub const MINUTE: u64 = 60 * SECOND;
pub const HOUR: u64 = 60 * MINUTE;
pub const DAY: u64 = 24 * HOUR;

/// Milliseconds since the Unix epoch of a date and time in UTC.
pub fn utc_ms(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> u64 {
    let days = days_from_civil(year, month, day);
    u64::try_from(days).expect("a date after 1970") * DAY + u64::from(hour) * HOUR + u64::from(minute) * MINUTE
}

/// A time in UTC as Claude and Codex write it in their files: `2026-11-19T09:30:00.123Z`.
pub fn iso_ms(ms: u64) -> String {
    let (year, month, day) = civil_from_days((ms / DAY) as i64);
    let rest = ms % DAY;
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z", rest / HOUR, rest / MINUTE % 60, rest / SECOND % 60, rest % SECOND)
}

/// The date and time (year, month, day, hour, minute) that a [`Clock::local_minutes`] value shows.
pub fn wall(local_minutes: i64) -> (i32, u32, u32, u32, u32) {
    let (year, month, day) = civil_from_days(local_minutes.div_euclid(1440));
    let minute = local_minutes.rem_euclid(1440);
    (year, month, day, (minute / 60) as u32, (minute % 60) as u32)
}

/// Days from 1970-01-01 to a date of the proleptic Gregorian calendar (H. Hinnant's
/// `days_from_civil`, as `crate::clock` has it).
pub fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let month = i64::from(month);
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The date of a day counted from 1970-01-01 (H. Hinnant's `civil_from_days`).
pub fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 { shifted + 3 } else { shifted - 9 };
    ((year_of_era + era * 400 + i64::from(month <= 2)) as i32, month as u32, day as u32)
}

/// 0 = Sunday … 6 = Saturday.
fn weekday_of(days: i64) -> i64 {
    (days + 4).rem_euclid(7)
}

/// One change of a daylight saving rule: on which day of the year, and when on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    pub month: u32,
    /// The n-th Sunday of the month (1 to 4), or 0 for its last Sunday.
    pub sunday: u32,
    /// Minutes after midnight: of UTC when `utc`, else of the wall clock just before the change.
    pub minute: u32,
    pub utc: bool,
}

impl Rule {
    /// The day of the change in `year`, counted from 1970-01-01.
    fn day(&self, year: i32) -> i64 {
        if self.sunday == 0 {
            let (next_year, next_month) = if self.month == 12 { (year + 1, 1) } else { (year, self.month + 1) };
            let last = days_from_civil(next_year, next_month, 1) - 1;
            last - weekday_of(last)
        } else {
            let first = days_from_civil(year, self.month, 1);
            first + (7 - weekday_of(first)) % 7 + 7 * (i64::from(self.sunday) - 1)
        }
    }

    /// The instant of the change, in milliseconds, given the zone's offset just before it.
    fn instant(&self, year: i32, offset_before: i32) -> i64 {
        let at = self.day(year) * DAY as i64 + i64::from(self.minute) * MINUTE as i64;
        if self.utc { at } else { at - i64::from(offset_before) * MINUTE as i64 }
    }
}

/// Daylight saving time: `save` minutes more from `start` to `end` of each year.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dst {
    pub save: i32,
    pub start: Rule,
    pub end: Rule,
}

/// A time zone: an offset from UTC, and maybe rules for daylight saving time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Zone {
    pub name: String,
    /// Minutes east of UTC outside daylight saving time.
    pub standard: i32,
    pub dst: Option<Dst>,
}

impl Zone {
    pub fn utc() -> Zone {
        Zone::fixed("UTC", 0)
    }

    /// A zone that never changes its offset.
    pub fn fixed(name: &str, minutes_east: i32) -> Zone {
        Zone { name: name.to_string(), standard: minutes_east, dst: None }
    }

    /// Central Europe (Europe/Berlin): UTC+1, and UTC+2 from the last Sunday of March to the last
    /// Sunday of October, both changes at 01:00 UTC.
    pub fn berlin() -> Zone {
        let rule = |month| Rule { month, sunday: 0, minute: 60, utc: true };
        Zone { name: "Europe/Berlin".into(), standard: 60, dst: Some(Dst { save: 60, start: rule(3), end: rule(10) }) }
    }

    /// US Eastern (America/New_York): UTC-5, and UTC-4 from 02:00 on the second Sunday of March to
    /// 02:00 on the first Sunday of November (wall clock).
    pub fn new_york() -> Zone {
        Zone {
            name: "America/New_York".into(),
            standard: -300,
            dst: Some(Dst { save: 60, start: Rule { month: 3, sunday: 2, minute: 120, utc: false }, end: Rule { month: 11, sunday: 1, minute: 120, utc: false } }),
        }
    }

    /// Eastern Australia (Australia/Sydney): UTC+10, and UTC+11 from 02:00 on the first Sunday of
    /// October to 03:00 on the first Sunday of April: daylight time spans the new year.
    pub fn sydney() -> Zone {
        Zone {
            name: "Australia/Sydney".into(),
            standard: 600,
            dst: Some(Dst { save: 60, start: Rule { month: 10, sunday: 1, minute: 120, utc: false }, end: Rule { month: 4, sunday: 1, minute: 180, utc: false } }),
        }
    }

    /// The two changes of `year` in milliseconds: into daylight time, and out of it.
    pub fn changes(&self, year: i32) -> Option<(i64, i64)> {
        let dst = self.dst?;
        Some((dst.start.instant(year, self.standard), dst.end.instant(year, self.standard + dst.save)))
    }

    /// Minutes east of UTC at `at_ms`.
    pub fn offset_at(&self, at_ms: u64) -> i32 {
        let Some(dst) = self.dst else { return self.standard };
        let (year, ..) = civil_from_days((at_ms / DAY) as i64);
        let (start, end) = self.changes(year).expect("a zone with rules");
        let at = at_ms as i64;
        // In the south, daylight time runs from the change late in the year into the next one.
        let daylight = if start < end { start <= at && at < end } else { at >= start || at < end };
        if daylight { self.standard + dst.save } else { self.standard }
    }
}

/// What happened to a [`VirtualClock`], besides time running on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// The machine slept from `from` to `to`.
    Slept { from: u64, to: u64 },
    /// The wall clock was set from `from` to `to` (either way).
    Jumped { from: u64, to: u64 },
    /// The time zone changed at `at`.
    Zone { at: u64, from: String, to: String },
}

/// A clock that moves only when the test moves it.
pub struct VirtualClock {
    now: AtomicU64,
    state: Mutex<State>,
}

struct State {
    zone: Zone,
    log: Vec<Change>,
}

impl VirtualClock {
    pub fn new(start_ms: u64, zone: Zone) -> Arc<VirtualClock> {
        Arc::new(VirtualClock { now: AtomicU64::new(start_ms), state: Mutex::new(State { zone, log: Vec::new() }) })
    }

    /// This clock answers [`crate::clock::now_ms`] for the whole process until the guard is
    /// dropped.
    pub fn install(self: &Arc<Self>) -> TestClockGuard {
        crate::clock::use_test_clock(self.clone() as Arc<dyn Clock>)
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn now(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }

    /// Time runs on by `ms`, with the machine awake. Returns the new time.
    pub fn advance(&self, ms: u64) -> u64 {
        self.now.fetch_add(ms, Ordering::SeqCst) + ms
    }

    /// The machine sleeps for `ms`: the time is later when it wakes, and nothing ran in between.
    pub fn sleep(&self, ms: u64) -> u64 {
        let from = self.now();
        let to = self.advance(ms);
        self.state().log.push(Change::Slept { from, to });
        to
    }

    /// The wall clock is set to `at_ms`, which may be earlier than now.
    pub fn jump_to(&self, at_ms: u64) {
        let from = self.now.swap(at_ms, Ordering::SeqCst);
        self.state().log.push(Change::Jumped { from, to: at_ms });
    }

    /// The machine moves to another time zone (a person travelling, a setting changed).
    pub fn set_zone(&self, zone: Zone) {
        let at = self.now();
        let mut state = self.state();
        let from = std::mem::replace(&mut state.zone, zone);
        let to = state.zone.name.clone();
        state.log.push(Change::Zone { at, from: from.name, to });
    }

    pub fn zone(&self) -> Zone {
        self.state().zone.clone()
    }

    /// Every sleep, jump and zone change so far, in order.
    pub fn changes(&self) -> Vec<Change> {
        self.state().log.clone()
    }

    /// The local date and time now (year, month, day, hour, minute).
    pub fn wall_now(&self) -> (i32, u32, u32, u32, u32) {
        let now = self.now();
        wall(self.local_minutes(now).expect("a virtual zone always knows"))
    }
}

impl Clock for VirtualClock {
    fn now_ms(&self) -> u64 {
        self.now()
    }

    fn local_minutes(&self, at_ms: u64) -> Option<i64> {
        let offset = self.state().zone.offset_at(at_ms);
        Some((at_ms / MINUTE) as i64 + i64::from(offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{minute_of_day, weekday};

    #[test]
    fn dates_and_times_in_utc() {
        assert_eq!(iso_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_ms(utc_ms(2026, 11, 19, 9, 30) + 4_123), "2026-11-19T09:30:04.123Z");
        // Leap days and the turn of the century: there and back.
        for (year, month, day) in [(2024, 2, 29), (2025, 3, 1), (2100, 2, 28), (2100, 3, 1), (2000, 2, 29), (1970, 1, 1), (1969, 12, 31)] {
            assert_eq!(civil_from_days(days_from_civil(year, month, day)), (year, month, day));
        }
        let mut days = days_from_civil(2023, 12, 25);
        for _ in 0..800 {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days);
            days += 1;
        }
    }

    /// Berlin on 2026-03-29: at 01:00 UTC the wall clock goes from 01:59 to 03:00, so 02:30 that day
    /// never shows.
    #[test]
    fn virtual_clock_dst_gap() {
        let before = utc_ms(2026, 3, 29, 0, 59);
        let clock = VirtualClock::new(before, Zone::berlin());
        assert_eq!(clock.wall_now(), (2026, 3, 29, 1, 59));
        clock.advance(MINUTE);
        assert_eq!(clock.wall_now(), (2026, 3, 29, 3, 0));
        // One minute of real time, sixty-one on the wall.
        let minutes = |at| clock.local_minutes(at).unwrap();
        assert_eq!(minutes(before + MINUTE) - minutes(before), 61);
        // No minute of that day is 02:30 on the wall.
        let day = utc_ms(2026, 3, 28, 22, 0);
        assert!((0..24 * 60).map(|m| minute_of_day(minutes(day + m * MINUTE))).all(|m| m != 150));
        assert_eq!(weekday(minutes(before)), 0, "a Sunday");
        // A week earlier and a week later: an hour each side.
        assert_eq!(Zone::berlin().offset_at(before - 7 * DAY), 60);
        assert_eq!(Zone::berlin().offset_at(before + 7 * DAY), 120);
    }

    /// Berlin on 2026-10-25: at 01:00 UTC the wall clock goes from 02:59 back to 02:00, so 02:30
    /// shows twice, an hour apart.
    #[test]
    fn virtual_clock_dst_fold() {
        let clock = VirtualClock::new(0, Zone::berlin());
        let first = utc_ms(2026, 10, 25, 0, 30);
        let second = utc_ms(2026, 10, 25, 1, 30);
        assert_eq!(wall(clock.local_minutes(first).unwrap()), (2026, 10, 25, 2, 30));
        assert_eq!(wall(clock.local_minutes(second).unwrap()), (2026, 10, 25, 2, 30));
        assert_eq!(clock.local_minutes(utc_ms(2026, 10, 25, 0, 59)).unwrap() + 1 - 60, clock.local_minutes(utc_ms(2026, 10, 25, 1, 0)).unwrap());
    }

    #[test]
    fn zones_change_on_the_days_their_rules_say() {
        let york = Zone::new_york();
        // 2026-03-08 02:00 EST (07:00 UTC) becomes 03:00 EDT; 2026-11-01 02:00 EDT (06:00 UTC)
        // becomes 01:00 EST.
        assert_eq!(york.changes(2026), Some((utc_ms(2026, 3, 8, 7, 0) as i64, utc_ms(2026, 11, 1, 6, 0) as i64)));
        assert_eq!(york.offset_at(utc_ms(2026, 3, 8, 6, 59)), -300);
        assert_eq!(york.offset_at(utc_ms(2026, 3, 8, 7, 0)), -240);
        assert_eq!(york.offset_at(utc_ms(2026, 11, 1, 5, 59)), -240);
        assert_eq!(york.offset_at(utc_ms(2026, 11, 1, 6, 0)), -300);
        // Sydney: daylight time over the new year, from 2026-10-04 02:00 AEST (16:00 UTC the day
        // before) to 2026-04-05 03:00 AEDT (16:00 UTC the day before).
        let sydney = Zone::sydney();
        assert_eq!(sydney.offset_at(utc_ms(2026, 1, 15, 0, 0)), 660);
        assert_eq!(sydney.offset_at(utc_ms(2026, 4, 4, 15, 59)), 660);
        assert_eq!(sydney.offset_at(utc_ms(2026, 4, 4, 16, 0)), 600);
        assert_eq!(sydney.offset_at(utc_ms(2026, 10, 3, 15, 59)), 600);
        assert_eq!(sydney.offset_at(utc_ms(2026, 10, 3, 16, 0)), 660);
        assert_eq!(Zone::fixed("India", 330).offset_at(utc_ms(2026, 6, 1, 0, 0)), 330);
    }

    #[test]
    fn virtual_clock_sleeps_jumps_and_travels() {
        let start = utc_ms(2026, 11, 19, 8, 0);
        let clock = VirtualClock::new(start, Zone::utc());
        {
            let _guard = clock.install();
            assert_eq!(crate::clock::now_ms(), start);
            clock.advance(5 * MINUTE);
            assert_eq!(crate::clock::now_ms(), start + 5 * MINUTE);
            // Three days asleep, then the clock set back an hour, then a flight to New York.
            clock.sleep(3 * DAY);
            clock.jump_to(start + 3 * DAY + 5 * MINUTE - HOUR);
            clock.set_zone(Zone::new_york());
            assert_eq!(crate::clock::now_ms(), start + 3 * DAY + 5 * MINUTE - HOUR);
            assert_eq!(crate::clock::local_minutes(clock.now()), clock.local_minutes(clock.now()));
            assert_eq!(clock.wall_now(), (2026, 11, 22, 2, 5));
        }
        assert_eq!(
            clock.changes(),
            [
                Change::Slept { from: start + 5 * MINUTE, to: start + 3 * DAY + 5 * MINUTE },
                Change::Jumped { from: start + 3 * DAY + 5 * MINUTE, to: start + 3 * DAY + 5 * MINUTE - HOUR },
                Change::Zone { at: start + 3 * DAY + 5 * MINUTE - HOUR, from: "UTC".into(), to: "America/New_York".into() },
            ]
        );
        // Back on the system clock once the guard is gone.
        assert!(crate::clock::now_ms() > utc_ms(2026, 1, 1, 0, 0) && crate::clock::now_ms() != clock.now());
    }
}
