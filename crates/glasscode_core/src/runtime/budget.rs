//! The manager's own token ledger (D6; brain-6): what each brain turn used, read from its result's
//! usage, per lane and per UTC day, with a daily cap and a cap of wakes per hour. The vendors'
//! own readings (the stream's rate-limit events, the status line files) are advisory: any window
//! not "allowed" stops the background lane until it resets, whichever windows the vendor reports.
//! An answer the chat makes from code uses no tokens and never asks the budget.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::caller::Lane;
use crate::store::{OnDamage, Store};

/// Days the ledger keeps.
pub const KEEP_DAYS: usize = 30;

/// The caps a person can change; these are where they start.
pub const DAILY_TOKENS: u64 = 1_500_000;
pub const WAKES_PER_HOUR: u32 = 6;

/// A daily cap of "off": no token count of the day stops a turn (the hour's wakes and the
/// vendor's windows still do).
pub const NO_CAP: u64 = u64::MAX;

/// The lowest daily cap a person can set: below it the manager could not answer a question.
pub const MIN_CAP: u64 = 100_000;

/// The daily caps Settings › Manager offers, and their words.
pub const CAP_CHOICES: &[(u64, &str)] = &[(DAILY_TOKENS, "1.5M"), (3_000_000, "3M"), (6_000_000, "6M"), (10_000_000, "10M"), (NO_CAP, "Off")];

/// A daily cap as typed (`manager budget`): "off", "default", or tokens a day, with `k` or `m`
/// after it if wanted ("3m", "1.5M", "500k", "2000000").
pub fn parse_cap(word: &str) -> Result<u64, String> {
    let word = word.trim().to_lowercase().replace(['_', ','], "");
    match word.as_str() {
        "off" | "none" | "no" | "unlimited" => return Ok(NO_CAP),
        "default" => return Ok(DAILY_TOKENS),
        _ => {}
    }
    let (number, times) = match word.strip_suffix('m') {
        Some(n) => (n, 1_000_000.0),
        None => match word.strip_suffix('k') {
            Some(n) => (n, 1_000.0),
            None => (word.as_str(), 1.0),
        },
    };
    let tokens = number.trim().parse::<f64>().ok().filter(|n| n.is_finite() && *n > 0.0).map(|n| (n * times).round()).ok_or_else(|| format!("“{word}” is no daily cap: say a number of tokens (3m, 500k, 2000000) or off."))?;
    if tokens < MIN_CAP as f64 {
        return Err(format!("A daily cap under {} tokens would stop the manager answering: say more, or off.", thousands(MIN_CAP)));
    }
    if tokens >= NO_CAP as f64 { Ok(NO_CAP) } else { Ok(tokens as u64) }
}

/// A daily cap in words: "off", or "1,500,000 tokens a day".
pub fn cap_words(cap: u64) -> String {
    if cap == NO_CAP { "off".to_string() } else { format!("{} tokens a day", thousands(cap)) }
}

/// 1500000 as "1,500,000".
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Keep a new daily cap in the ledger, its counts as they are; the ledger as it is then.
pub fn set_daily_cap(cap: u64) -> Result<Ledger, crate::store::StoreError> {
    crate::store::update::<Ledger, _>(|ledger| {
        ledger.daily_cap = cap;
        ledger.clone()
    })
}

/// A vendor reading older than this is none.
pub const FRESH: std::time::Duration = std::time::Duration::from_secs(20 * 60);

/// Routine wakes in an hour while no vendor reading is fresh.
pub const STALE_WAKES_PER_HOUR: u32 = 2;

/// What one turn used, from its result line (Claude's `usage`, Codex's `turn.completed`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    /// From a Claude `result` line's `usage`.
    pub fn from_claude(usage: &Value) -> Usage {
        let n = |key: &str| usage[key].as_u64().unwrap_or(0);
        Usage { input: n("input_tokens"), output: n("output_tokens"), cache_read: n("cache_read_input_tokens"), cache_write: n("cache_creation_input_tokens") }
    }

    /// From a Codex `turn.completed` event's `usage`.
    pub fn from_codex(usage: &Value) -> Usage {
        let n = |key: &str| usage[key].as_u64().unwrap_or(0);
        // Codex counts cached input inside its input.
        let cached = n("cached_input_tokens");
        Usage { input: n("input_tokens").saturating_sub(cached), output: n("output_tokens"), cache_read: cached, cache_write: 0 }
    }
}

/// A vendor's window as its last reading said it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub status: String,
    /// When this reading came (ms): one older than [`FRESH`] is no reading.
    #[serde(default)]
    pub read_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utilization: Option<f64>,
}

/// Why a turn may not start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocked {
    /// Today's tokens are spent (the cap, and what was used).
    DailyCap { cap: u64, used: u64 },
    /// This hour's wakes are done.
    WakesPerHour { cap: u32 },
    /// A vendor window is used up until then.
    Vendor { window: String, until: Option<i64> },
}

impl Blocked {
    pub fn sentence(&self) -> String {
        match self {
            Blocked::DailyCap { cap, used } => format!("The manager used {} of its {} tokens for today; it thinks again tomorrow (UTC), or when you raise its daily cap in Settings › Manager (or `glasscode manager budget off`).", thousands(*used), thousands(*cap)),
            Blocked::WakesPerHour { cap } => format!("The manager woke {cap} times this hour; the next wake waits for the hour to pass."),
            Blocked::Vendor { window, until: Some(at) } => format!("The plan's {window} window is used up until {}; background work waits.", crate::day::clock(*at)),
            Blocked::Vendor { window, until: None } => format!("The plan's {window} window is used up; background work waits."),
        }
    }
}

/// What kind of turn asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Turn {
    /// The person asked in the chat.
    Chat,
    /// A wake, with nobody in the loop.
    Wake,
    /// A wake about something urgent: the hour's cap does not hold it back (the daily cap and
    /// the vendor's window still do).
    Urgent,
    /// A study run of moving in (M54), or a learning run (M62): background work, so the daily cap
    /// and the vendor's windows hold it back; it is no wake, so the hour's wakes do not.
    Study,
}

/// The ledger (`manager/tokens.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ledger {
    /// Tokens per UTC day (`YYYY-MM-DD`) and lane.
    pub days: BTreeMap<String, BTreeMap<String, u64>>,
    /// When the wakes of the last hour started (ms).
    pub wakes: Vec<u64>,
    /// Each vendor window as last read (`seven_day`, `five_hour`, …).
    pub windows: BTreeMap<String, Window>,
    pub daily_cap: u64,
    pub wakes_per_hour: u32,
    /// Background lanes may run on paid overage or bought credits (off unless the person says).
    pub overage: bool,
}

impl Default for Ledger {
    fn default() -> Self {
        Ledger { days: BTreeMap::new(), wakes: Vec::new(), windows: BTreeMap::new(), daily_cap: DAILY_TOKENS, wakes_per_hour: WAKES_PER_HOUR, overage: false }
    }
}

impl Store for Ledger {
    const NAME: &'static str = "manager/tokens";
    const VERSION: u32 = 1;
    // A lost ledger counts from nothing again: the caps are the vendors' too.
    const ON_DAMAGE: OnDamage = OnDamage::Empty;

    fn path() -> PathBuf {
        crate::paths::root().join("manager").join("tokens.json")
    }
}

fn lane_word(lane: Lane) -> &'static str {
    match lane {
        Lane::Chat => "chat",
        Lane::Wake => "wake",
    }
}

/// The UTC day of a time (ms).
fn day_of(ms: u64) -> String {
    crate::day::iso((ms / 86_400_000) as i64)
}

impl Ledger {
    /// What a turn of `lane` used.
    pub fn record(&mut self, lane: Lane, usage: Usage, now_ms: u64) {
        *self.days.entry(day_of(now_ms)).or_default().entry(lane_word(lane).into()).or_default() += usage.total();
        while self.days.len() > KEEP_DAYS {
            let first = self.days.keys().next().cloned().unwrap_or_default();
            self.days.remove(&first);
        }
    }

    /// What a study run of moving in used (M54), under its own name for the day.
    pub fn record_study(&mut self, usage: Usage, now_ms: u64) {
        self.record_named("study", usage, now_ms);
    }

    /// What a background run used, under its own name for the day ("study", "learning").
    pub fn record_named(&mut self, name: &str, usage: Usage, now_ms: u64) {
        *self.days.entry(day_of(now_ms)).or_default().entry(name.into()).or_default() += usage.total();
    }

    /// A wake started.
    pub fn record_wake(&mut self, now_ms: u64) {
        self.wakes.retain(|at| now_ms.saturating_sub(*at) < 3_600_000);
        self.wakes.push(now_ms);
    }

    /// A vendor's window, from a stream's `rate_limit_event`, read at `now_ms`.
    pub fn record_window_at(&mut self, event: &Value, now_ms: u64) {
        let info = &event["rate_limit_info"];
        let name = info["rateLimitType"].as_str().unwrap_or("plan").to_string();
        let window = Window { status: info["status"].as_str().unwrap_or("allowed").to_string(), read_ms: now_ms, resets_at: info["resetsAt"].as_i64(), utilization: info["utilization"].as_f64() };
        self.windows.insert(name, window);
    }

    /// What a lane recorded in its copy (`before` → `after`), added to this one: two lanes keep
    /// one ledger without losing each other's turns.
    pub fn add_since(&mut self, before: &Ledger, after: &Ledger) {
        for (day, lanes) in &after.days {
            for (lane, tokens) in lanes {
                let had = before.days.get(day).and_then(|l| l.get(lane)).copied().unwrap_or(0);
                *self.days.entry(day.clone()).or_default().entry(lane.clone()).or_insert(0) += tokens.saturating_sub(had);
            }
        }
        for at in after.wakes.iter().filter(|at| !before.wakes.contains(at)) {
            self.wakes.push(*at);
        }
        for (name, window) in &after.windows {
            if self.windows.get(name).is_none_or(|have| have.read_ms <= window.read_ms) {
                self.windows.insert(name.clone(), window.clone());
            }
        }
    }

    /// [`Ledger::record_window_at`] now.
    pub fn record_window(&mut self, event: &Value) {
        self.record_window_at(event, crate::clock::now_ms());
    }

    /// Whether a vendor reading is fresh (exec-18): without one, routine wakes run at
    /// [`STALE_WAKES_PER_HOUR`], never unlimited.
    pub fn fresh(&self, now_ms: u64) -> bool {
        self.windows.values().any(|w| now_ms.saturating_sub(w.read_ms) <= FRESH.as_millis() as u64)
    }

    /// Today's tokens, per lane.
    pub fn today(&self, now_ms: u64) -> BTreeMap<String, u64> {
        self.days.get(&day_of(now_ms)).cloned().unwrap_or_default()
    }

    /// The cap a background share of the day is taken from: the daily cap, but never more than
    /// the default when the person raised it or turned it off, so that lifting the cap for the
    /// chat does not let learning run all day on the person's plan.
    pub fn background_cap(&self) -> u64 {
        self.daily_cap.min(DAILY_TOKENS)
    }

    /// What today's cap leaves (moving in's estimate, M35).
    pub fn left_today(&self, now_ms: u64) -> u64 {
        self.daily_cap.saturating_sub(self.today(now_ms).values().sum())
    }

    /// Whether a turn of this kind may start now. A chat turn stops only at the daily cap; a wake
    /// also at the hour's cap and at any vendor window that is not "allowed" (overage counts as
    /// used up unless the person allowed it).
    pub fn allow(&self, turn: Turn, now_ms: u64) -> Result<(), Blocked> {
        let used: u64 = self.today(now_ms).values().sum();
        if self.daily_cap != NO_CAP && used >= self.daily_cap {
            return Err(Blocked::DailyCap { cap: self.daily_cap, used });
        }
        if turn == Turn::Chat {
            return Ok(());
        }
        let this_hour = self.wakes.iter().filter(|at| now_ms.saturating_sub(**at) < 3_600_000).count();
        let cap = if self.fresh(now_ms) { self.wakes_per_hour } else { self.wakes_per_hour.min(STALE_WAKES_PER_HOUR) };
        if turn == Turn::Wake && this_hour >= cap as usize {
            return Err(Blocked::WakesPerHour { cap });
        }
        let now_secs = (now_ms / 1000) as i64;
        for (name, window) in &self.windows {
            let over = match window.status.as_str() {
                "allowed" | "allowed_warning" => false,
                "overage" | "allowed_overage" => !self.overage,
                _ => true,
            };
            if over && window.resets_at.is_none_or(|at| at > now_secs) {
                return Err(Blocked::Vendor { window: name.clone(), until: window.resets_at });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOON: u64 = 1_795_000_000_000;

    #[test]
    fn budget_cap_blocks_wakes_not_chat_answers_from_code() {
        let mut ledger = Ledger { daily_cap: 30_000, ..Ledger::default() };
        ledger.record(Lane::Wake, Usage { input: 10_000, output: 500, ..Usage::default() }, NOON);
        assert_eq!(ledger.allow(Turn::Wake, NOON), Ok(()));
        ledger.record(Lane::Chat, Usage::from_claude(&json!({ "input_tokens": 5000, "output_tokens": 200, "cache_read_input_tokens": 14_300 })), NOON);
        assert!(matches!(ledger.allow(Turn::Wake, NOON), Err(Blocked::DailyCap { used: 30_000, .. })));
        assert!(matches!(ledger.allow(Turn::Chat, NOON), Err(Blocked::DailyCap { .. })), "a chat turn that thinks stops too");
        // An answer from code never asks the ledger; tomorrow (UTC) counts from nothing.
        assert_eq!(ledger.allow(Turn::Wake, NOON + 86_400_000), Ok(()));
        assert_eq!(ledger.today(NOON)["chat"], 19_500);
    }

    /// The person can raise the daily cap or turn it off (`manager budget`, Settings › Manager);
    /// off, no count of the day stops a turn, but the hour's wakes and the vendor's windows do.
    #[test]
    fn a_daily_cap_turned_off_stops_no_turn_but_the_other_walls_hold() {
        let mut ledger = Ledger { daily_cap: parse_cap("off").unwrap(), wakes_per_hour: 1, ..Ledger::default() };
        ledger.record(Lane::Chat, Usage { input: 50_000_000, ..Usage::default() }, NOON);
        assert_eq!(ledger.allow(Turn::Chat, NOON), Ok(()));
        ledger.record_window_at(&json!({ "rate_limit_info": { "status": "allowed", "rateLimitType": "seven_day" } }), NOON);
        ledger.record_wake(NOON);
        assert_eq!(ledger.allow(Turn::Wake, NOON + 1), Err(Blocked::WakesPerHour { cap: 1 }));
        ledger.wakes.clear();
        ledger.record_window_at(&json!({ "rate_limit_info": { "status": "rejected", "rateLimitType": "seven_day" } }), NOON);
        assert!(matches!(ledger.allow(Turn::Wake, NOON + 1), Err(Blocked::Vendor { .. })));
        assert_eq!(ledger.background_cap(), DAILY_TOKENS, "learning's share stays the default's");
        assert_eq!(cap_words(ledger.daily_cap), "off");
    }

    #[test]
    fn a_daily_cap_is_read_as_typed() {
        assert_eq!(parse_cap("3m"), Ok(3_000_000));
        assert_eq!(parse_cap("1.5M"), Ok(1_500_000));
        assert_eq!(parse_cap("500k"), Ok(500_000));
        assert_eq!(parse_cap("2,000,000"), Ok(2_000_000));
        assert_eq!(parse_cap(" Off "), Ok(NO_CAP));
        assert_eq!(parse_cap("default"), Ok(DAILY_TOKENS));
        assert!(parse_cap("10k").unwrap_err().contains("100,000"));
        assert!(parse_cap("lots").is_err() && parse_cap("-3m").is_err() && parse_cap("0").is_err());
        assert_eq!(cap_words(1_500_000), "1,500,000 tokens a day");
        assert!(Blocked::DailyCap { cap: 1_500_000, used: 1_502_870 }.sentence().contains("Settings › Manager"));
        assert!(CAP_CHOICES.iter().all(|(cap, _)| *cap == NO_CAP || *cap >= MIN_CAP));
    }

    #[test]
    fn wakes_per_hour_and_vendor_windows_stop_background_work() {
        let mut ledger = Ledger { wakes_per_hour: 2, ..Ledger::default() };
        ledger.record_window_at(&json!({ "rate_limit_info": { "status": "allowed", "rateLimitType": "seven_day" } }), NOON);
        ledger.record_wake(NOON);
        ledger.record_wake(NOON + 60_000);
        assert_eq!(ledger.allow(Turn::Wake, NOON + 120_000), Err(Blocked::WakesPerHour { cap: 2 }));
        assert_eq!(ledger.allow(Turn::Chat, NOON + 120_000), Ok(()), "the person's question still goes");
        assert_eq!(ledger.allow(Turn::Wake, NOON + 3_700_000), Ok(()), "an hour later");
        ledger.wakes.clear();
        let resets = (NOON / 1000) as i64 + 3600;
        ledger.record_window_at(&json!({ "type": "rate_limit_event", "rate_limit_info": { "status": "rejected", "rateLimitType": "seven_day", "resetsAt": resets } }), NOON);
        assert_eq!(ledger.allow(Turn::Wake, NOON + 3_700_000), Ok(()), "it reset by then");
        assert!(matches!(ledger.allow(Turn::Wake, NOON + 1_000), Err(Blocked::Vendor { .. })));
        ledger.record_window_at(&json!({ "rate_limit_info": { "status": "allowed_overage", "rateLimitType": "five_hour" } }), NOON);
        ledger.windows.remove("seven_day");
        assert!(matches!(ledger.allow(Turn::Wake, NOON + 1_000), Err(Blocked::Vendor { .. })), "no overage unless the person allows it");
        ledger.overage = true;
        assert_eq!(ledger.allow(Turn::Wake, NOON + 1_000), Ok(()));
        assert!(Blocked::Vendor { window: "seven_day".into(), until: None }.sentence().contains("seven_day"));
    }

    /// With no fresh vendor reading, routine wakes run at a low cap, never unlimited (exec-18).
    #[test]
    fn a_stale_reading_means_a_low_cap() {
        let mut ledger = Ledger { wakes_per_hour: 6, ..Ledger::default() };
        ledger.record_wake(NOON);
        ledger.record_wake(NOON + 1);
        assert_eq!(ledger.allow(Turn::Wake, NOON + 2), Err(Blocked::WakesPerHour { cap: STALE_WAKES_PER_HOUR }), "no reading at all");
        ledger.record_window_at(&json!({ "rate_limit_info": { "status": "allowed", "rateLimitType": "seven_day" } }), NOON);
        assert_eq!(ledger.allow(Turn::Wake, NOON + 2), Ok(()));
        ledger.record_wake(NOON + 30 * 60_000);
        ledger.record_wake(NOON + 30 * 60_000 + 1);
        assert!(matches!(ledger.allow(Turn::Wake, NOON + 31 * 60_000), Err(Blocked::WakesPerHour { cap: STALE_WAKES_PER_HOUR })), "an old reading is none");
        assert_eq!(ledger.allow(Turn::Chat, NOON + 31 * 60_000), Ok(()), "the chat is never held by it");
        assert_eq!(ledger.allow(Turn::Urgent, NOON + 31 * 60_000), Ok(()), "nor an urgent wake");
    }

    /// A lane's turn, added to the ledger another lane wrote meanwhile: both count.
    #[test]
    fn two_lanes_keep_one_ledger() {
        let before = Ledger::default();
        let mut wake = before.clone();
        wake.record_wake(NOON);
        wake.record(Lane::Wake, Usage { input: 30, ..Usage::default() }, NOON);
        let mut stored = before.clone();
        stored.record(Lane::Chat, Usage { input: 50, ..Usage::default() }, NOON);
        stored.record(Lane::Wake, Usage { input: 5, ..Usage::default() }, NOON);
        stored.add_since(&before, &wake);
        assert_eq!((stored.today(NOON)["chat"], stored.today(NOON)["wake"], stored.wakes.len()), (50, 35, 1));
    }

    #[test]
    fn codex_counts_cached_input_inside_its_input() {
        let usage = Usage::from_codex(&json!({ "input_tokens": 20_000, "cached_input_tokens": 15_000, "output_tokens": 300 }));
        assert_eq!((usage.input, usage.cache_read, usage.total()), (5_000, 15_000, 20_300));
    }

    #[test]
    fn the_ledger_keeps_thirty_days() {
        let mut ledger = Ledger::default();
        for day in 0..40 {
            ledger.record(Lane::Chat, Usage { output: 1, ..Usage::default() }, NOON + day * 86_400_000);
        }
        assert_eq!(ledger.days.len(), KEEP_DAYS);
    }
}
