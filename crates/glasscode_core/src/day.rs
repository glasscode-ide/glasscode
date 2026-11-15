//! Calendar days in this machine's time zone, for the Today board: "today", due dates,
//! "3 days late", and the days recurring tasks come back on.
//!
//! A day is a count of days since 1970-01-01 (`Day`), so comparing and adding is plain
//! arithmetic; it is written as `2026-10-27` in files and on the command line.

/// Days since 1970-01-01.
pub type Day = i64;

const WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// The day it is now, here.
pub fn today() -> Day {
    local(crate::model::now() as i64)
}

/// The local day of a Unix time.
pub fn local(unix: i64) -> Day {
    let (y, m, d, _) = local_parts(unix);
    from_civil(y, m, d)
}

/// "09:40": the local time of a Unix time.
pub fn clock(unix: i64) -> String {
    let t = crate::sys::local_time(unix).unwrap_or_default();
    format!("{:02}:{:02}", t.hour, t.minute)
}

/// The hour of the day now, here (0–23).
pub fn hour_now() -> u32 {
    local_parts(crate::model::now() as i64).3
}

fn local_parts(unix: i64) -> (i64, u32, u32, u32) {
    let Some(t) = crate::sys::local_time(unix) else {
        let day = unix.div_euclid(86_400);
        let (y, m, d) = to_civil(day);
        return (y, m, d, 0);
    };
    (i64::from(t.year), t.month, t.day, t.hour)
}

/// Howard Hinnant's days_from_civil.
pub fn from_civil(y: i64, m: u32, d: u32) -> Day {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn to_civil(day: Day) -> (i64, u32, u32) {
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// 0 = Monday … 6 = Sunday.
pub fn weekday(day: Day) -> u32 {
    // 1970-01-01 was a Thursday.
    (day + 3).rem_euclid(7) as u32
}

/// A weekday's number (0 = Monday) from its name or a short form, English or German.
pub fn weekday_of(word: &str) -> Option<u32> {
    weekday_index(word)
}

/// Whether a word is a weekday written out in full ("friday", "freitag").
pub fn is_full_weekday(word: &str) -> bool {
    let w = word.trim_end_matches(['.', ',', '!', '?', ':']).to_lowercase();
    ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday", "montag", "dienstag", "mittwoch", "donnerstag", "freitag", "samstag", "sonntag"].contains(&w.as_str())
}

pub fn weekday_name(day: Day) -> &'static str {
    WEEKDAYS[weekday(day) as usize]
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

pub fn is_month_end(day: Day) -> bool {
    let (y, m, d) = to_civil(day);
    d == days_in_month(y, m)
}

pub fn day_of_month(day: Day) -> u32 {
    to_civil(day).2
}

/// `2026-10-27`.
pub fn iso(day: Day) -> String {
    let (y, m, d) = to_civil(day);
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn parse_iso(text: &str) -> Option<Day> {
    let text = text.get(..10)?;
    let mut parts = text.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    ((1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m)).then(|| from_civil(y, m, d))
}

/// "Tuesday, 27 October".
pub fn long(day: Day) -> String {
    let (_, m, d) = to_civil(day);
    format!("{}, {d} {}", weekday_name(day), MONTHS[(m - 1) as usize])
}

/// "27 Oct".
pub fn short(day: Day) -> String {
    let (_, m, d) = to_civil(day);
    format!("{d} {}", &MONTHS[(m - 1) as usize][..3])
}

/// How a due day reads next to a task: "Today", "Tomorrow", "Yesterday", "3 days late",
/// "Fri" (this week), else "3 Nov".
pub fn relative(day: Day, today: Day) -> String {
    match day - today {
        0 => "Today".into(),
        1 => "Tomorrow".into(),
        -1 => "Yesterday".into(),
        n if n < -1 => format!("{} days late", -n),
        n if n < 7 => WEEKDAYS[weekday(day) as usize][..3].to_string(),
        _ => short(day),
    }
}

fn weekday_index(word: &str) -> Option<u32> {
    let word = word.trim_end_matches(['.', ',', '!', '?']).to_lowercase();
    let names: [&[&str]; 7] = [
        &["monday", "mon", "montag"],
        &["tuesday", "tue", "tues", "dienstag"],
        &["wednesday", "wed", "weds", "mittwoch"],
        &["thursday", "thu", "thur", "thurs", "donnerstag"],
        &["friday", "fri", "freitag"],
        &["saturday", "sat", "samstag"],
        &["sunday", "sun", "sonntag"],
    ];
    names.iter().position(|n| n.contains(&word.as_str())).map(|i| i as u32)
}

fn month_index(word: &str) -> Option<u32> {
    let word = word.trim_end_matches(['.', ',']).to_lowercase();
    if word.len() < 3 {
        return None;
    }
    MONTHS.iter().position(|m| m.to_lowercase().starts_with(&word)).map(|i| i as u32 + 1)
}

/// The next day (today counts) with that weekday.
pub fn next_weekday(today: Day, weekday_wanted: u32) -> Day {
    let ahead = (i64::from(weekday_wanted) - i64::from(weekday(today))).rem_euclid(7);
    today + ahead
}

/// A day as people write it: today, tonight, tomorrow, a weekday ("fri", "next monday", "this
/// friday"), "next week", "in 3 days", "3d", "end of week", `2026-10-30`, `30.10.`,
/// `30.10.2026`, "oct 30", "30 oct", and the German everyday ones (heute, morgen, freitag).
/// `None` when it is not a day.
pub fn parse_when(text: &str, today: Day) -> Option<Day> {
    let text = text.trim().trim_end_matches(['.', ',', '!', ':', ';']).to_lowercase();
    let words: Vec<&str> = text.split_whitespace().collect();
    // "in 100000000 days" is not a day anyone means.
    let small = |n: i64| (n.abs() <= 3650).then_some(n);
    match words.as_slice() {
        [] => None,
        ["today" | "tonight" | "eod" | "now" | "heute"] | ["this", "morning" | "afternoon" | "evening"] | ["end", "of", "day"] => Some(today),
        ["tomorrow" | "tmrw" | "tmr" | "morgen"] => Some(today + 1),
        ["übermorgen" | "uebermorgen"] | ["day", "after", "tomorrow"] => Some(today + 2),
        ["yesterday" | "gestern"] => Some(today - 1),
        ["next", "week"] | ["nächste", "woche"] | ["naechste", "woche"] => Some(next_weekday(today + 1, 0)),
        ["this", "week"] | ["end", "of", "week"] | ["eow"] => Some(next_weekday(today, 4)),
        ["end", "of", "month"] | ["eom"] => {
            let (y, m, _) = to_civil(today);
            Some(from_civil(y, m, days_in_month(y, m)))
        }
        ["next", w] => weekday_index(w).map(|i| {
            let day = next_weekday(today, i);
            if day == today { day + 7 } else { day }
        }),
        ["this", w] => weekday_index(w).map(|i| next_weekday(today, i)),
        ["in", n, unit] if unit.starts_with("day") => n.parse::<i64>().ok().and_then(small).map(|n| today + n),
        ["in", n, unit] if unit.starts_with("week") => n.parse::<i64>().ok().and_then(|n| small(n * 7)).map(|n| today + n),
        ["in", "a", "week"] => Some(today + 7),
        [w] if weekday_index(w).is_some() => weekday_index(w).map(|i| next_weekday(today, i)),
        [w] if w.ends_with('d') && w[..w.len() - 1].parse::<i64>().is_ok() => w[..w.len() - 1].parse::<i64>().ok().and_then(small).map(|n| today + n),
        [w] if w.ends_with('w') && w[..w.len() - 1].parse::<i64>().is_ok() => w[..w.len() - 1].parse::<i64>().ok().and_then(|n| small(n * 7)).map(|n| today + n),
        [w] if parse_iso(w).is_some() => parse_iso(w),
        [w] if w.contains('.') || w.contains('/') => {
            // 30.10. / 30.10.2026 / 10/30 (day first unless the second number cannot be a month).
            let sep = if w.contains('.') { '.' } else { '/' };
            let parts: Vec<&str> = w.split(sep).filter(|p| !p.is_empty()).collect();
            let nums: Vec<i64> = parts.iter().filter_map(|p| p.parse().ok()).collect();
            if nums.len() != parts.len() {
                return None;
            }
            let (y0, _, _) = to_civil(today);
            let (d, m, y) = match nums.as_slice() {
                [a, b] if sep == '/' && *a <= 12 && *b > 12 => (*b, *a, y0),
                [a, b] => (*a, *b, y0),
                // A year is two or four digits ("1.2.3" is a version, not a day).
                [a, b, c] if parts[2].len() == 4 => (*a, *b, *c),
                [a, b, c] if parts[2].len() == 2 => (*a, *b, 2000 + c),
                _ => return None,
            };
            dated(y, m, d, today, nums.len() == 2)
        }
        [a, b] => {
            // "oct 30" / "30 oct"
            let (m, d) = match (month_index(a), month_index(b)) {
                (Some(m), None) => (m, b.trim_end_matches(|c: char| !c.is_ascii_digit()).parse().ok()?),
                (None, Some(m)) => (m, a.trim_end_matches(|c: char| !c.is_ascii_digit()).parse().ok()?),
                _ => return None,
            };
            let (y0, _, _) = to_civil(today);
            dated(y0, i64::from(m), d, today, true)
        }
        _ => None,
    }
}

/// A day of a month; without a year, the next one that has not passed.
fn dated(y: i64, m: i64, d: i64, today: Day, roll: bool) -> Option<Day> {
    if !(1..=12).contains(&m) || d < 1 || d > i64::from(days_in_month(y, m as u32)) {
        return None;
    }
    let day = from_civil(y, m as u32, d as u32);
    if roll && day < today - 31 {
        let next = y + 1;
        return (d <= i64::from(days_in_month(next, m as u32))).then(|| from_civil(next, m as u32, d as u32));
    }
    Some(day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        let day = from_civil(2026, 10, 27);
        assert_eq!(iso(day), "2026-10-27");
        assert_eq!(to_civil(day), (2026, 10, 27));
        assert_eq!(weekday_name(day), "Tuesday");
        assert_eq!(long(day), "Tuesday, 27 October");
        assert_eq!(parse_iso("2026-02-29"), None);
        assert_eq!(parse_iso("2028-02-29").map(iso).as_deref(), Some("2028-02-29"));
        assert!(is_month_end(from_civil(2026, 2, 28)));
    }

    #[test]
    fn reads_days_like_people_write_them() {
        let today = from_civil(2026, 10, 27); // a Tuesday
        let at = |t: &str| parse_when(t, today).map(iso);
        assert_eq!(at("today").as_deref(), Some("2026-10-27"));
        assert_eq!(at("Tomorrow").as_deref(), Some("2026-10-28"));
        assert_eq!(at("fri").as_deref(), Some("2026-10-30"));
        assert_eq!(at("Friday").as_deref(), Some("2026-10-30"));
        assert_eq!(at("tuesday").as_deref(), Some("2026-10-27"));
        assert_eq!(at("next tuesday").as_deref(), Some("2026-11-03"));
        assert_eq!(at("next week").as_deref(), Some("2026-11-02"));
        assert_eq!(at("in 3 days").as_deref(), Some("2026-10-30"));
        assert_eq!(at("2d").as_deref(), Some("2026-10-29"));
        assert_eq!(at("30.10.").as_deref(), Some("2026-10-30"));
        assert_eq!(at("3.11.2026").as_deref(), Some("2026-11-03"));
        assert_eq!(at("oct 30").as_deref(), Some("2026-10-30"));
        assert_eq!(at("2 Jan").as_deref(), Some("2027-01-02"));
        assert_eq!(at("end of month").as_deref(), Some("2026-10-31"));
        assert_eq!(at("soon"), None);
        assert_eq!(at("this friday").as_deref(), Some("2026-10-30"));
        assert_eq!(at("thurs").as_deref(), Some("2026-10-29"));
        assert_eq!(at("Freitag").as_deref(), Some("2026-10-30"));
        assert_eq!(at("morgen").as_deref(), Some("2026-10-28"));
        assert_eq!(at("tomorrow:").as_deref(), Some("2026-10-28"));
        assert_eq!(at("1.2.3"), None, "a version");
        assert_eq!(at("2.4.1"), None, "a version");
        assert_eq!(at("30.10.26").as_deref(), Some("2026-10-30"));
        assert_eq!(at("9223372036854775807d"), None, "no overflow");
        assert_eq!(at("in 100000000 days"), None);
        assert_eq!(relative(today - 3, today), "3 days late");
        assert_eq!(relative(today + 3, today), "Fri");
        assert_eq!(relative(today + 9, today), "5 Nov");
    }
}
