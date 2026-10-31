//! The Today board: everything the person has to do today in one list. Tasks they (or an
//! agent, through the `glasscode` command) add, tasks that come back on a schedule, and items
//! from elsewhere that Glasscode already knows about: Linear tickets and the pull requests
//! asking for their review. It lives in `~/.glasscode/board.json`; items from elsewhere are not
//! copied there, only what the person did with them (done today, snoozed, moved, in Focus).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::day::{self, Day};

/// Focus holds this many items at most.
pub const FOCUS_MAX: usize = 3;
/// Done tasks are kept this long (days), then forgotten.
const KEEP_DONE_DAYS: i64 = 30;

/// Where a task of the person's own sits when nothing else (a due day, Focus) puts it first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum List {
    /// Things to do, no one else involved.
    #[default]
    Todo,
    /// Someone asked for it.
    Asked,
    /// Waiting for someone else.
    Waiting,
    Later,
}

impl List {
    pub fn parse(text: &str) -> Option<List> {
        match text.trim().to_lowercase().replace(['-', '_', ' '], "").as_str() {
            "todo" | "today" | "do" => Some(List::Todo),
            "asked" | "askedofme" | "asks" | "people" => Some(List::Asked),
            "waiting" | "waitingon" | "waitingonothers" | "blocked" => Some(List::Waiting),
            "later" | "someday" | "backlog" => Some(List::Later),
            _ => None,
        }
    }

    pub fn section(self) -> Section {
        match self {
            List::Todo => Section::Todo,
            List::Asked => Section::Asked,
            List::Waiting => Section::Waiting,
            List::Later => Section::Later,
        }
    }
}

/// The board's sections, in the order they show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    Focus,
    Due,
    Asked,
    Todo,
    Reviews,
    Tickets,
    Waiting,
    Recurring,
    Later,
    Done,
}

impl Section {
    pub const ALL: [Section; 10] = [
        Section::Focus,
        Section::Due,
        Section::Asked,
        Section::Todo,
        Section::Reviews,
        Section::Tickets,
        Section::Waiting,
        Section::Recurring,
        Section::Later,
        Section::Done,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Focus => "Focus",
            Section::Due => "Due and late",
            Section::Asked => "Asked of me",
            Section::Todo => "To do",
            Section::Reviews => "Reviews",
            Section::Tickets => "My tickets",
            Section::Waiting => "Waiting on others",
            Section::Later => "Later",
            Section::Recurring => "Recurring",
            Section::Done => "Done today",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Section::Focus => "focus",
            Section::Due => "due",
            Section::Asked => "asked",
            Section::Todo => "todo",
            Section::Reviews => "reviews",
            Section::Tickets => "tickets",
            Section::Waiting => "waiting",
            Section::Later => "later",
            Section::Recurring => "recurring",
            Section::Done => "done",
        }
    }

    pub fn parse(text: &str) -> Option<Section> {
        let t = text.trim().to_lowercase().replace(['-', '_', ' '], "");
        Section::ALL.into_iter().find(|s| s.key() == t || s.title().to_lowercase().replace(' ', "") == t).or_else(|| match t.as_str() {
            "review" | "prs" => Some(Section::Reviews),
            "ticket" | "mytickets" | "linear" => Some(Section::Tickets),
            "duelate" | "late" | "overdue" => Some(Section::Due),
            _ => List::parse(&t).map(List::section),
        })
    }

    /// Sections a task or item can be moved to by hand.
    pub fn movable(self) -> bool {
        matches!(self, Section::Asked | Section::Todo | Section::Waiting | Section::Later | Section::Tickets | Section::Reviews)
    }
}

/// Who put a task on the board.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// "You", "Claude", "Codex", "A terminal".
    pub who: String,
    /// The Glasscode session it came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
}

impl Origin {
    pub fn you() -> Origin {
        Origin { who: "You".into(), ..Default::default() }
    }

    pub fn is_you(&self) -> bool {
        self.who == "You"
    }

    /// "Added by Claude · API connectors", or `None` for the person's own.
    pub fn label(&self) -> Option<String> {
        if self.is_you() {
            return None;
        }
        let who = if self.who.starts_with("A ") { format!("a{}", &self.who[1..]) } else { self.who.clone() };
        Some(match &self.session_title {
            Some(title) if !title.is_empty() => format!("Added by {who} · {title}"),
            _ => format!("Added by {who}"),
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub text: String,
    pub at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default, deserialize_with = "lenient_value")]
    pub list: List,
    /// `2026-10-30`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    /// Who asked (list Asked), who it waits for (Waiting), or who it is with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    /// Where they asked: "Teams", "Meeting, Oct 24", "Email".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// Tickets, pull requests and links it is about ("ENG-42", "#958", a URL).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub urgent: bool,
    /// Out of sight until this day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<String>,
    /// The day it was done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_day: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<u64>,
    /// Made by a recurring task (its id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurring: Option<String>,
    /// The session started from it (Today's Start).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default)]
    pub added_by: Origin,
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
}

impl Task {
    pub fn key(&self) -> String {
        format!("task:{}", self.id)
    }

    pub fn is_done(&self) -> bool {
        self.done_day.is_some()
    }
}

/// When a recurring task comes back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "every")]
pub enum Every {
    Day,
    /// Monday to Friday.
    Weekday,
    /// One day a week (0 = Monday).
    Week { day: u32 },
    /// Several days a week (bit 0 = Monday).
    Days { mask: u8 },
    MonthStart,
    MonthEnd,
}

const DAY_NAMES: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

impl Every {
    pub fn occurs(self, day: Day) -> bool {
        match self {
            Every::Day => true,
            Every::Weekday => day::weekday(day) < 5,
            Every::Week { day: d } => day::weekday(day) == d,
            Every::Days { mask } => mask & (1 << day::weekday(day)) != 0,
            Every::MonthStart => day::day_of_month(day) == 1,
            Every::MonthEnd => day::is_month_end(day),
        }
    }

    pub fn label(self) -> String {
        match self {
            Every::Day => "Every day".into(),
            Every::Weekday => "Every weekday".into(),
            Every::Week { day } => format!("Every {}", DAY_NAMES[day.min(6) as usize]),
            Every::Days { mask } => {
                let days: Vec<&str> = (0..7).filter(|d| mask & (1 << d) != 0).map(|d| DAY_NAMES[d]).collect();
                match days.split_last() {
                    Some((last, rest)) if !rest.is_empty() => format!("Every {} and {last}", rest.join(", ")),
                    Some((only, _)) => format!("Every {only}"),
                    None => "Never".into(),
                }
            }
            Every::MonthStart => "First day of the month".into(),
            Every::MonthEnd => "Last day of the month".into(),
        }
    }

    /// "monday", "mondays", "thurs", "monday and friday", "day", "weekday", "month-end",
    /// "month start".
    pub fn parse(text: &str) -> Option<Every> {
        let t = text.trim().to_lowercase().replace(['-', '_'], " ");
        let t = t.trim_start_matches("every ").trim().trim_end_matches([':', ',', '.']);
        match t {
            "day" | "daily" | "morning" | "day of the week" => return Some(Every::Day),
            "weekday" | "workday" | "working day" | "weekdays" => return Some(Every::Weekday),
            "month end" | "end of month" | "end of the month" | "last day of the month" | "month" => return Some(Every::MonthEnd),
            "month start" | "start of month" | "first of the month" | "first day of the month" => return Some(Every::MonthStart),
            "week" | "weekly" => return Some(Every::Week { day: 0 }),
            _ => {}
        }
        let one = |w: &str| {
            let w = w.trim();
            day::weekday_of(w).or_else(|| w.strip_suffix('s').and_then(day::weekday_of))
        };
        let parts: Vec<&str> = t.split([',', '&']).flat_map(|p| p.split(" and ")).map(str::trim).filter(|p| !p.is_empty()).collect();
        let days: Vec<u32> = parts.iter().map(|p| one(p)).collect::<Option<Vec<_>>>()?;
        match days.as_slice() {
            [] => None,
            [d] => Some(Every::Week { day: *d }),
            many => Some(Every::Days { mask: many.iter().fold(0u8, |m, d| m | (1 << d)) }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recurring {
    pub id: String,
    pub title: String,
    #[serde(flatten)]
    pub every: Every,
    #[serde(default)]
    pub list: List,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    #[serde(default)]
    pub added_by: Origin,
    pub created_at: u64,
}

/// What the person did with an item from elsewhere.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_day: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient_value")]
    pub moved: Option<Section>,
    #[serde(default)]
    pub updated_at: u64,
}

impl Mark {
    fn is_empty(&self) -> bool {
        self.done_day.is_none() && self.snoozed_until.is_none() && self.moved.is_none()
    }
}

/// An item from elsewhere (Linear, GitHub), as the app sees it now.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Outside {
    /// Stable: "linear:ENG-42", "review:acme/app#958".
    pub key: String,
    /// "ENG-42", "#958".
    pub reference: String,
    pub title: String,
    pub section: Option<Section>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    /// "Linear · In Progress", "GitHub · acme/app".
    pub source: String,
    /// The pull request's author, the ticket's assignee.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub urgent: bool,
    /// Finished today (a ticket completed today): shows under Done today.
    #[serde(default)]
    pub done_today: bool,
}

/// A list where an entry that does not read (from a newer build, or written by hand) is left
/// out instead of failing the whole file.
fn lenient_list<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let values = Vec::<serde_json::Value>::deserialize(deserializer).unwrap_or_default();
    Ok(values.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
}

fn lenient_map<'de, D, T>(deserializer: D) -> Result<BTreeMap<String, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let values = BTreeMap::<String, serde_json::Value>::deserialize(deserializer).unwrap_or_default();
    Ok(values.into_iter().filter_map(|(k, v)| Some((k, serde_json::from_value(v).ok()?))).collect())
}

/// A value that, when it does not read, is its default.
fn lenient_value<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(T::deserialize(value).unwrap_or_default())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Board {
    #[serde(default, deserialize_with = "lenient_list")]
    pub tasks: Vec<Task>,
    #[serde(default, deserialize_with = "lenient_list")]
    pub recurring: Vec<Recurring>,
    #[serde(default, deserialize_with = "lenient_map")]
    pub marks: BTreeMap<String, Mark>,
    /// Item keys in Focus, in order ("task:<id>" or an item's key).
    #[serde(default)]
    pub focus: Vec<String>,
    /// The day recurring tasks were last made for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared: Option<String>,
    /// The day the person last opened the board (the morning open happens once a day).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen: Option<String>,
    #[serde(default)]
    pub updated_at: u64,
}

// ----- the view ------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// "task:<id>", an item's key, or "recurring:<id>".
    pub key: String,
    /// What to type on the command line: the task's id or the item's reference.
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub section: Option<Section>,
    pub done: bool,
    pub focus: bool,
    /// `2026-10-30`, and how it reads ("Today", "3 days late", "Fri").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_label: Option<String>,
    /// Due today or before.
    pub due_now: bool,
    pub late: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    /// How the person is involved: "asked", "waiting", "with", "author".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_by: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// "Tomorrow", "Mon": back on the board that day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed: Option<String>,
    /// An item from elsewhere (Linear, GitHub).
    pub outside: bool,
    /// "Every Monday", for a recurring task (or one it made).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurring: Option<String>,
    pub urgent: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SectionView {
    pub section: Option<Section>,
    pub key: String,
    pub title: String,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TodayView {
    /// `2026-10-27` and "Tuesday, 27 October".
    pub day: String,
    pub date: String,
    pub sections: Vec<SectionView>,
    /// Open items due today or before.
    pub due: usize,
    /// Open items someone asked for.
    pub asked: usize,
    /// Open items, not counting Later and Done.
    pub open: usize,
    pub done: usize,
}

impl TodayView {
    pub fn section(&self, section: Section) -> Option<&SectionView> {
        self.sections.iter().find(|s| s.section == Some(section))
    }

    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.sections.iter().flat_map(|s| s.items.iter())
    }

    /// The sentence at the top: "3 things are due or late, 2 people are waiting on you, and 1
    /// session needs you."
    pub fn summary(&self, sessions_waiting: usize) -> String {
        self.summary_with(sessions_waiting, 0)
    }

    /// The same, with requests from agents that wait for the person's OK.
    pub fn summary_with(&self, sessions_waiting: usize, approvals: usize) -> String {
        let mut parts = Vec::new();
        if self.due > 0 {
            parts.push(format!("{} {} due or late", count(self.due, "thing", "things"), if self.due == 1 { "is" } else { "are" }));
        }
        if self.asked > 0 {
            parts.push(format!("{} {} waiting on you", count(self.asked, "person", "people"), if self.asked == 1 { "is" } else { "are" }));
        }
        if sessions_waiting > 0 {
            parts.push(format!("{} {} you", count(sessions_waiting, "session", "sessions"), if sessions_waiting == 1 { "needs" } else { "need" }));
        }
        if approvals > 0 {
            parts.push(format!("{} {} for your OK", count(approvals, "request", "requests"), if approvals == 1 { "waits" } else { "wait" }));
        }
        match parts.len() {
            0 if self.open == 0 && self.done > 0 => "Everything on today's board is done.".into(),
            0 if self.open == 0 => "Nothing on the board. Add what you have to do today below.".into(),
            0 => format!("Nothing is due and nobody is waiting on you. {} on the board.", count(self.open, "thing", "things")),
            1 => format!("{}.", parts[0]),
            _ => {
                let last = parts.pop().unwrap_or_default();
                format!("{}, and {last}.", parts.join(", "))
            }
        }
    }
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

// ----- the board -----------------------------------------------------------------------

/// A new task, as the command line or the add box describes it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Draft {
    pub title: String,
    pub list: List,
    pub due: Option<Day>,
    pub person: Option<String>,
    pub via: Option<String>,
    pub links: Vec<String>,
    pub note: Option<String>,
    pub urgent: bool,
    /// Make it a recurring task instead.
    pub every: Option<Every>,
}

pub fn file() -> std::path::PathBuf {
    crate::paths::root().join("board.json")
}

pub fn load() -> Board {
    let path = file();
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
            log::error!("board.json is unreadable ({err}); keeping a copy and starting empty");
            let _ = std::fs::copy(&path, path.with_extension("json.broken"));
            Board::default()
        }),
        Err(_) => Board::default(),
    }
}

fn short_id(taken: impl Fn(&str) -> bool) -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut rng = rand::rng();
    loop {
        let id: String = (0..4).map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char).collect();
        if !taken(&id) {
            return id;
        }
    }
}

impl Board {
    pub fn save(&self) -> anyhow::Result<()> {
        crate::paths::ensure_dirs()?;
        let path = file();
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn touch(&mut self) {
        self.updated_at = crate::model::now();
    }

    fn new_id(&self) -> String {
        short_id(|id| self.tasks.iter().any(|t| t.id == id) || self.recurring.iter().any(|r| r.id == id))
    }

    pub fn task(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn task_mut(&mut self, id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    /// Make today's recurring tasks (once a day) and forget old done ones. Returns whether
    /// anything changed.
    pub fn prepare(&mut self, today: Day) -> bool {
        let stamp = day::iso(today);
        if self.prepared.as_deref() == Some(stamp.as_str()) {
            return false;
        }
        let now = crate::model::now();
        // Days missed while Glasscode was closed count too (since the last time, a week at most),
        // but each recurring task has one open copy at a time: an unfinished one shows as late.
        let from = self.prepared.as_deref().and_then(day::parse_iso).map(|d| (d + 1).max(today - 6)).unwrap_or(today);
        for day in from..=today {
            for r in self.recurring.clone() {
                if !r.every.occurs(day) {
                    continue;
                }
                let open = self.tasks.iter().any(|t| t.recurring.as_deref() == Some(&r.id) && (!t.is_done() || t.due.as_deref() == Some(&day::iso(day))));
                if open {
                    continue;
                }
                let id = self.new_id();
                self.tasks.push(Task {
                    id,
                    title: r.title.clone(),
                    list: r.list,
                    due: Some(day::iso(day)),
                    person: r.person.clone(),
                    recurring: Some(r.id.clone()),
                    added_by: r.added_by.clone(),
                    created_at: now,
                    updated_at: now,
                    ..Default::default()
                });
            }
        }
        self.tasks.retain(|t| t.done_day.as_deref().and_then(day::parse_iso).is_none_or(|d| today - d <= KEEP_DONE_DAYS));
        self.marks.retain(|_, m| {
            if m.is_empty() {
                return false;
            }
            let recent = |d: &Option<String>| d.as_deref().and_then(day::parse_iso).is_some_and(|d| d >= today - 1);
            m.moved.is_some() || recent(&m.done_day) || m.snoozed_until.as_deref().and_then(day::parse_iso).is_some_and(|d| d > today)
        });
        self.prepared = Some(stamp);
        self.touch();
        true
    }

    /// Open tasks from earlier days (due before today), for the morning's "carried over".
    pub fn carried_over(&self, today: Day) -> usize {
        self.tasks.iter().filter(|t| !t.is_done() && t.due.as_deref().and_then(day::parse_iso).is_some_and(|d| d < today)).count()
    }

    /// Add a task (or a recurring one); returns its id.
    pub fn add(&mut self, draft: Draft, by: Origin, today: Day) -> String {
        let now = crate::model::now();
        let id = self.new_id();
        let title = draft.title.trim().to_string();
        if let Some(every) = draft.every {
            self.recurring.push(Recurring { id: id.clone(), title, every, list: draft.list, person: draft.person, added_by: by, created_at: now });
            // Today's copy, when today is one of its days.
            self.prepared = None;
            self.prepare(today);
            self.touch();
            return id;
        }
        self.tasks.push(Task {
            id: id.clone(),
            title,
            list: draft.list,
            due: draft.due.map(day::iso),
            person: draft.person,
            via: draft.via,
            links: draft.links,
            notes: draft.note.map(|text| vec![Note { text, at: now, by: (!by.is_you()).then(|| by.who.clone()) }]).unwrap_or_default(),
            urgent: draft.urgent,
            added_by: by,
            created_at: now,
            updated_at: now,
            ..Default::default()
        });
        self.touch();
        id
    }

    /// Which item a word from the command line means: a task's id (or its start), a
    /// recurring task's id, a reference ("ENG-42", "#958") of an item from elsewhere or of a
    /// task linking to it, or an item's key.
    pub fn resolve(&self, outside: &[Outside], word: &str) -> Result<String, String> {
        let w = word.trim().trim_start_matches("task:");
        if w.is_empty() {
            return Err("Say which task: its id (from `glasscode board`) or a ticket like ENG-42.".into());
        }
        if let Some(t) = self.task(w) {
            return Ok(t.key());
        }
        if let Some(r) = self.recurring.iter().find(|r| r.id == w || format!("recurring:{}", r.id) == w) {
            return Ok(format!("recurring:{}", r.id));
        }
        if let Some(o) = outside.iter().find(|o| o.key == word) {
            return Ok(o.key.clone());
        }
        // "958" is "#958" (an unquoted "#958" is a comment to a shell).
        let reference = if w.chars().all(|c| c.is_ascii_digit()) { format!("#{w}") } else { w.to_string() };
        let by_reference: Vec<&Outside> = outside.iter().filter(|o| o.reference.eq_ignore_ascii_case(&reference)).collect();
        match by_reference.as_slice() {
            [one] => return Ok(one.key.clone()),
            [] => {}
            many => {
                return Err(format!("“{word}” is more than one item: {}. Use its key.", many.iter().map(|o| format!("{} ({})", o.key, o.source)).collect::<Vec<_>>().join(", ")));
            }
        }
        // Focus keeps keys of items that may be gone; they can still be taken out.
        if let Some(key) = self.focus.iter().find(|k| *k == word || k.ends_with(&format!(":{w}"))) {
            return Ok(key.clone());
        }
        let starts: Vec<&Task> = self.tasks.iter().filter(|t| w.len() >= 2 && t.id.starts_with(&w.to_lowercase())).collect();
        match starts.as_slice() {
            [one] => return Ok(one.key()),
            [] => {}
            many => return Err(format!("“{word}” is the start of {} task ids: {}. Use the whole id.", many.len(), many.iter().take(5).map(|t| t.id.clone()).collect::<Vec<_>>().join(", "))),
        }
        let linked: Vec<&Task> = self.tasks.iter().filter(|t| !t.is_done() && t.links.iter().any(|l| l.eq_ignore_ascii_case(w))).collect();
        if let [one] = linked.as_slice() {
            return Ok(one.key());
        }
        // Words from the title, when only one open task has them all.
        let words: Vec<String> = w.to_lowercase().split_whitespace().map(str::to_string).collect();
        if words.len() >= 2 || w.len() >= 6 {
            let hits: Vec<&Task> = self.tasks.iter().filter(|t| !t.is_done() && words.iter().all(|x| t.title.to_lowercase().contains(x))).collect();
            if let [one] = hits.as_slice() {
                return Ok(one.key());
            }
            if hits.len() > 1 {
                return Err(format!("“{word}” matches {} tasks: {}. Use the id.", hits.len(), hits.iter().take(5).map(|t| format!("{} ({})", t.id, t.title)).collect::<Vec<_>>().join(", ")));
            }
        }
        Err(format!("No task or item “{word}” on the board. `glasscode board` lists them with their ids."))
    }

    fn mark(&mut self, key: &str) -> &mut Mark {
        self.marks.entry(key.to_string()).or_default()
    }

    /// Tick an item off (or back on).
    pub fn set_done(&mut self, key: &str, done: bool, today: Day) -> Result<(), String> {
        let now = crate::model::now();
        if let Some(id) = key.strip_prefix("task:") {
            let task = self.task_mut(id).ok_or("That task is gone.")?;
            task.done_day = done.then(|| day::iso(today));
            task.done_at = done.then_some(now);
            task.updated_at = now;
        } else if key.starts_with("recurring:") {
            return Err("That is a recurring task; tick off today's copy instead (or remove the recurring task).".into());
        } else {
            let mark = self.mark(key);
            mark.done_day = done.then(|| day::iso(today));
            mark.updated_at = now;
        }
        if done {
            self.focus.retain(|k| k != key);
        }
        self.touch();
        Ok(())
    }

    /// Out of sight until `until` (`None`: back now).
    pub fn snooze(&mut self, key: &str, until: Option<Day>) -> Result<(), String> {
        let now = crate::model::now();
        let until = until.map(day::iso);
        if let Some(id) = key.strip_prefix("task:") {
            let task = self.task_mut(id).ok_or("That task is gone.")?;
            task.snoozed_until = until.clone();
            task.updated_at = now;
        } else if key.starts_with("recurring:") {
            return Err("A recurring task cannot be snoozed; snooze today's copy.".into());
        } else {
            let mark = self.mark(key);
            mark.snoozed_until = until.clone();
            mark.updated_at = now;
        }
        if until.is_some() {
            self.focus.retain(|k| k != key);
        }
        self.touch();
        Ok(())
    }

    /// Forget Focus entries whose item is gone (a task removed, a ticket finished in Linear),
    /// so they do not hold one of the three places. Returns whether anything changed.
    pub fn prune_focus(&mut self, outside: &[Outside]) -> bool {
        let before = self.focus.len();
        let tasks: Vec<String> = self.tasks.iter().filter(|t| !t.is_done()).map(Task::key).collect();
        self.focus.retain(|k| tasks.contains(k) || outside.iter().any(|o| o.key == *k && !o.done_today));
        self.focus.len() != before
    }

    pub fn set_focus(&mut self, key: &str, on: bool) -> Result<(), String> {
        if key.starts_with("recurring:") {
            return Err("Put today's copy of a recurring task in Focus, not the recurring task.".into());
        }
        if on {
            if self.focus.iter().any(|k| k == key) {
                return Ok(());
            }
            if self.focus.len() >= FOCUS_MAX {
                return Err("Focus holds three things. Take one out first.".to_string());
            }
            self.focus.push(key.to_string());
            // Back from wherever it was hiding.
            if let Some(id) = key.strip_prefix("task:") {
                if let Some(task) = self.task_mut(id) {
                    task.snoozed_until = None;
                    task.done_day = None;
                    task.done_at = None;
                }
            } else {
                let mark = self.mark(key);
                mark.snoozed_until = None;
                mark.done_day = None;
            }
        } else {
            self.focus.retain(|k| k != key);
        }
        self.touch();
        Ok(())
    }

    pub fn move_to(&mut self, key: &str, section: Section) -> Result<(), String> {
        if !section.movable() {
            return Err(format!("Tasks move to: asked, todo, waiting or later (not {}).", section.key()));
        }
        let now = crate::model::now();
        if let Some(id) = key.strip_prefix("task:") {
            let list = match section {
                Section::Asked => List::Asked,
                Section::Waiting => List::Waiting,
                Section::Later => List::Later,
                _ => List::Todo,
            };
            let task = self.task_mut(id).ok_or("That task is gone.")?;
            task.list = list;
            task.snoozed_until = None;
            task.updated_at = now;
        } else if key.starts_with("recurring:") {
            return Err("Move today's copy of a recurring task, not the recurring task.".into());
        } else {
            let mark = self.mark(key);
            mark.moved = Some(section);
            mark.snoozed_until = None;
            mark.updated_at = now;
        }
        self.touch();
        Ok(())
    }

    pub fn add_note(&mut self, key: &str, text: &str, by: &Origin) -> Result<(), String> {
        let id = key.strip_prefix("task:").ok_or("Notes go on your own tasks; items from Linear or GitHub keep theirs there.")?;
        let now = crate::model::now();
        let task = self.task_mut(id).ok_or("That task is gone.")?;
        task.notes.push(Note { text: text.trim().to_string(), at: now, by: (!by.is_you()).then(|| by.who.clone()) });
        task.updated_at = now;
        self.touch();
        Ok(())
    }

    /// Take a task (or a recurring one) off the board for good.
    pub fn remove(&mut self, key: &str) -> Result<String, String> {
        if let Some(id) = key.strip_prefix("recurring:") {
            let index = self.recurring.iter().position(|r| r.id == id).ok_or("That recurring task is gone.")?;
            let r = self.recurring.remove(index);
            self.touch();
            return Ok(r.title);
        }
        let id = key.strip_prefix("task:").ok_or("Items from Linear or GitHub cannot be removed here: tick them off or snooze them.")?;
        let index = self.tasks.iter().position(|t| t.id == id).ok_or("That task is gone.")?;
        let task = self.tasks.remove(index);
        self.focus.retain(|k| k != key);
        self.touch();
        Ok(task.title)
    }

    /// Where a task sits today (`None`: not shown: done another day).
    fn place_task(&self, task: &Task, today: Day) -> Option<Section> {
        if let Some(done) = task.done_day.as_deref() {
            return (day::parse_iso(done) == Some(today)).then_some(Section::Done);
        }
        if task.snoozed_until.as_deref().and_then(day::parse_iso).is_some_and(|d| d > today) {
            return Some(Section::Later);
        }
        if self.focus.contains(&task.key()) {
            return Some(Section::Focus);
        }
        // What someone asked for stays under Asked of me, with its day in red or orange.
        if task.list != List::Asked && task.due.as_deref().and_then(day::parse_iso).is_some_and(|d| d <= today) {
            return Some(Section::Due);
        }
        Some(task.list.section())
    }

    fn place_outside(&self, item: &Outside, today: Day) -> Option<Section> {
        let mark = self.marks.get(&item.key);
        let marked_done = mark.and_then(|m| m.done_day.as_deref()).and_then(day::parse_iso);
        if item.done_today || marked_done == Some(today) {
            return Some(Section::Done);
        }
        if mark.and_then(|m| m.snoozed_until.as_deref()).and_then(day::parse_iso).is_some_and(|d| d > today) {
            return Some(Section::Later);
        }
        if self.focus.contains(&item.key) {
            return Some(Section::Focus);
        }
        let section = mark.and_then(|m| m.moved).or(item.section);
        // Reviews and what someone asked for stay where they are, with their day.
        let stays = matches!(section, Some(Section::Reviews | Section::Asked));
        if !stays && item.due.as_deref().and_then(day::parse_iso).is_some_and(|d| d <= today) {
            return Some(Section::Due);
        }
        section
    }

    fn task_item(&self, task: &Task, section: Section, today: Day) -> Item {
        let due = task.due.as_deref().and_then(day::parse_iso);
        let (role, person) = match (&task.person, task.list) {
            (Some(p), List::Asked) => (Some("asked"), Some(p.clone())),
            (Some(p), List::Waiting) => (Some("waiting"), Some(p.clone())),
            (Some(p), _) => (Some("with"), Some(p.clone())),
            (None, _) => (None, None),
        };
        let recurring = task.recurring.as_deref().and_then(|id| self.recurring.iter().find(|r| r.id == id)).map(|r| r.every.label());
        Item {
            key: task.key(),
            id: task.id.clone(),
            title: task.title.clone(),
            reference: task.links.iter().find(|l| !l.starts_with("http")).cloned(),
            section: Some(section),
            done: task.is_done(),
            focus: self.focus.contains(&task.key()),
            due: task.due.clone(),
            due_label: due.map(|d| day::relative(d, today)),
            due_now: due.is_some_and(|d| d <= today) && !task.is_done(),
            late: due.is_some_and(|d| d < today) && !task.is_done(),
            person,
            person_role: role.map(str::to_string),
            source: task.via.clone().or(recurring.clone()),
            notes: task.notes.iter().map(|n| n.text.clone()).collect(),
            added_by: task.added_by.label(),
            links: task.links.clone(),
            url: task.links.iter().find(|l| l.starts_with("http")).cloned(),
            snoozed: task.snoozed_until.as_deref().and_then(day::parse_iso).filter(|d| *d > today).map(|d| day::relative(d, today)),
            outside: false,
            recurring,
            urgent: task.urgent,
        }
    }

    fn outside_item(&self, item: &Outside, section: Section, today: Day) -> Item {
        let due = item.due.as_deref().and_then(day::parse_iso);
        let done = section == Section::Done;
        let snoozed = self.marks.get(&item.key).and_then(|m| m.snoozed_until.as_deref()).and_then(day::parse_iso).filter(|d| *d > today);
        Item {
            key: item.key.clone(),
            id: item.reference.clone(),
            title: item.title.clone(),
            reference: Some(item.reference.clone()),
            section: Some(section),
            done,
            focus: self.focus.contains(&item.key),
            due: item.due.clone(),
            due_label: due.map(|d| day::relative(d, today)),
            due_now: due.is_some_and(|d| d <= today) && !done,
            late: due.is_some_and(|d| d < today) && !done,
            person: item.person.clone(),
            person_role: item.person.as_ref().map(|_| if item.key.starts_with("review:") { "author" } else { "with" }.to_string()),
            source: Some(item.source.clone()),
            url: item.url.clone(),
            snoozed: snoozed.map(|d| day::relative(d, today)),
            outside: true,
            urgent: item.urgent,
            ..Default::default()
        }
    }

    /// The board as it reads today.
    pub fn view(&self, outside: &[Outside], today: Day) -> TodayView {
        let mut by: BTreeMap<Section, Vec<(Item, (i64, i64, u64))>> = BTreeMap::new();
        for task in &self.tasks {
            if let Some(section) = self.place_task(task, today) {
                let item = self.task_item(task, section, today);
                let due = task.due.as_deref().and_then(day::parse_iso).unwrap_or(i64::MAX);
                let order = match section {
                    Section::Done => (-(task.done_at.unwrap_or(0) as i64), 0, 0),
                    _ => (i64::from(!task.urgent), due, task.created_at),
                };
                by.entry(section).or_default().push((item, order));
            }
        }
        for o in outside {
            if let Some(section) = self.place_outside(o, today) {
                let item = self.outside_item(o, section, today);
                let due = o.due.as_deref().and_then(day::parse_iso).unwrap_or(i64::MAX);
                by.entry(section).or_default().push((item, (i64::from(!o.urgent), due, u64::MAX)));
            }
        }
        for r in &self.recurring {
            let item = Item {
                key: format!("recurring:{}", r.id),
                id: r.id.clone(),
                title: r.title.clone(),
                section: Some(Section::Recurring),
                person: r.person.clone(),
                person_role: r.person.as_ref().map(|_| "with".to_string()),
                source: Some(r.every.label()),
                added_by: r.added_by.label(),
                recurring: Some(r.every.label()),
                ..Default::default()
            };
            by.entry(Section::Recurring).or_default().push((item, (0, 0, r.created_at)));
        }
        let mut view = TodayView { day: day::iso(today), date: day::long(today), ..Default::default() };
        let mut people: Vec<String> = Vec::new();
        for section in Section::ALL {
            let mut items = by.remove(&section).unwrap_or_default();
            if section == Section::Focus {
                items.sort_by_key(|(i, _)| self.focus.iter().position(|k| *k == i.key).unwrap_or(usize::MAX));
            } else {
                items.sort_by(|a, b| a.1.cmp(&b.1));
            }
            let items: Vec<Item> = items.into_iter().map(|(i, _)| i).collect();
            for item in &items {
                if item.done {
                    view.done += 1;
                    continue;
                }
                if section == Section::Recurring {
                    continue;
                }
                if section == Section::Later {
                    continue;
                }
                if item.due_now {
                    view.due += 1;
                }
                // Who waits on the person: each one once; something asked with no name, alone.
                let asked = item.person_role.as_deref() == Some("asked") || (section == Section::Asked && item.person.is_none());
                if asked {
                    match &item.person {
                        Some(p) if people.iter().any(|q| q.eq_ignore_ascii_case(p)) => {}
                        Some(p) => {
                            people.push(p.clone());
                            view.asked += 1;
                        }
                        None => view.asked += 1,
                    }
                }
                view.open += 1;
            }
            view.sections.push(SectionView { section: Some(section), key: section.key().into(), title: section.title().into(), items });
        }
        view
    }
}

// ----- items from elsewhere ----------------------------------------------------------------

/// The person's Linear tickets (assigned to them, and the ones in review with their reviewer
/// label) as board items.
pub fn from_linear(snapshot: &crate::linear::Snapshot, today: Day) -> Vec<Outside> {
    let mut out = Vec::new();
    for t in &snapshot.mine {
        let kind = t.status.kind.as_str();
        let in_review = kind == "started" && t.status.name.to_lowercase().contains("review");
        let due_soon = t.due.as_deref().and_then(day::parse_iso).is_some_and(|d| d <= today + 7);
        let section = match kind {
            "completed" | "canceled" | "cancelled" | "duplicate" => {
                let done_today = t.completed_at.as_deref().and_then(crate::github::parse_time).is_some_and(|at| day::local(at) == today);
                if !done_today || kind != "completed" {
                    continue;
                }
                None
            }
            "started" if in_review => Some(Section::Waiting),
            "started" | "unstarted" => Some(Section::Tickets),
            _ if t.priority == 1 || due_soon => Some(Section::Tickets),
            _ => Some(Section::Later),
        };
        out.push(Outside {
            key: format!("linear:{}", t.identifier),
            reference: t.identifier.clone(),
            title: t.title.clone(),
            section,
            due: t.due.clone(),
            source: format!("Linear · {}", if in_review { "In Review, waiting for review".to_string() } else { t.status.name.clone() }),
            person: None,
            url: Some(t.url.clone()),
            urgent: t.priority == 1,
            done_today: section.is_none(),
        });
    }
    for t in snapshot.reviewing.iter().filter(|t| t.assignee.as_ref().is_none_or(|a| a.id != snapshot.me)) {
        let in_review = t.status.kind == "started" && t.status.name.to_lowercase().contains("review");
        if !in_review || out.iter().any(|o| o.reference == t.identifier) {
            continue;
        }
        out.push(Outside {
            key: format!("linear-review:{}", t.identifier),
            reference: t.identifier.clone(),
            title: t.title.clone(),
            section: Some(Section::Reviews),
            due: t.due.clone(),
            source: "Linear · waiting for your review".into(),
            person: t.assignee.as_ref().map(|a| a.name.clone()),
            url: Some(t.url.clone()),
            urgent: t.priority == 1,
            done_today: false,
        });
    }
    out
}

/// Pull requests asking for the person's review.
pub fn from_reviews(reviews: &[crate::github::ReviewRequest]) -> Vec<Outside> {
    reviews
        .iter()
        .map(|r| Outside {
            key: format!("review:{}#{}", r.repo, r.hit.number),
            reference: format!("#{}", r.hit.number),
            title: r.hit.title.clone(),
            section: Some(Section::Reviews),
            due: None,
            source: format!("GitHub · {}", r.repo),
            person: Some(r.hit.author.clone()).filter(|a| !a.is_empty()),
            url: Some(r.hit.url.clone()).filter(|u| !u.is_empty()),
            urgent: false,
            done_today: false,
        })
        .collect()
}

/// Both, without a Linear review item when its pull request asks for the review on GitHub too.
pub fn outside_items(snapshot: Option<&crate::linear::Snapshot>, reviews: &[crate::github::ReviewRequest], today: Day) -> Vec<Outside> {
    let prs = from_reviews(reviews);
    let mut out: Vec<Outside> = snapshot.map(|s| from_linear(s, today)).unwrap_or_default();
    let mentioned: Vec<String> = reviews.iter().flat_map(|r| crate::linear::identifiers_in(&format!("{} {}", r.hit.title, r.hit.head))).collect();
    out.retain(|o| !(o.key.starts_with("linear-review:") && mentioned.contains(&o.reference)));
    out.extend(prs);
    out
}

// ----- the add box ---------------------------------------------------------------------------

const ASK_VERBS: [&str; 12] = ["ask", "tell", "remind", "ping", "email", "mail", "call", "message", "text", "chase", "invite", "meet"];
/// Verbs whose person usually comes after "to" ("send the slides to Sam").
const GIVE_VERBS: [&str; 5] = ["send", "give", "forward", "show", "hand"];
/// Capitalised words that are not people.
const NOT_PEOPLE: [&str; 35] = [
    "Claude", "Codex", "Glasscode", "Linear", "GitHub", "Github", "GitLab", "Slack", "Teams", "Outlook", "Jira", "Docker", "Azure", "Office", "Notion",
    "Today", "Tomorrow", "The", "A", "An", "All", "Everyone", "Everybody", "Them", "Him", "Her", "Me", "Us", "It", "This", "That", "My", "Our", "Their", "Your",
];
/// After a name, words that make it a group, not a person ("Ask the design team").
const GROUP_WORDS: [&str; 6] = ["team", "folks", "people", "channel", "group", "devs"];

fn is_name(word: &str) -> bool {
    let mut chars = word.chars();
    let first_upper = chars.next().is_some_and(|c| c.is_uppercase());
    let rest_ok = chars.all(|c| c.is_alphabetic() || c == '-' || c == '+');
    // "PR", "CI", "QA": acronyms, not names.
    let acronym = word.chars().filter(|c| c.is_alphabetic()).all(|c| c.is_uppercase());
    first_upper && rest_ok && word.chars().count() >= 2 && !acronym && !NOT_PEOPLE.contains(&word)
}

fn clean_name(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric() && c != '+').trim_end_matches("'s").trim_end_matches("’s").to_string()
}

fn bare(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric() && c != '-').to_lowercase()
}

/// What a line in the add box means: who is involved, when it is due, which list, whether it
/// comes back ("every Monday …"). `people` are names Glasscode knows (they count after
/// "for"/"from" and before "needs"/"wants", where an unknown capitalised word is not enough).
pub fn parse_quick(text: &str, today: Day, people: &[String]) -> Draft {
    let mut title = text.trim().to_string();
    let mut draft = Draft::default();
    if let Some(rest) = title.strip_prefix('!') {
        draft.urgent = true;
        title = rest.trim().to_string();
    }
    {
        let words: Vec<String> = title.split_whitespace().map(bare).collect();
        draft.urgent |= words.iter().enumerate().any(|(i, w)| (w == "urgent" || w == "asap") && !matches!(i.checked_sub(1).map(|j| words[j].as_str()), Some("not" | "non" | "no")));
    }

    // "every monday …" / "… every month-end" / "every monday and friday"
    let words: Vec<String> = title.split_whitespace().map(str::to_string).collect();
    if let Some(at) = words.iter().position(|w| w.eq_ignore_ascii_case("every")) {
        for len in (1..=5).rev() {
            if at + 1 + len > words.len() {
                continue;
            }
            let phrase = words[at + 1..at + 1 + len].join(" ");
            if let Some(every) = Every::parse(&phrase) {
                draft.every = Some(every);
                let mut rest: Vec<String> = words[..at].to_vec();
                rest.extend(words[at + 1 + len..].iter().cloned());
                title = rest.join(" ").trim_matches([':', '-', ',', ' ']).to_string();
                break;
            }
        }
    }

    // A due day. At the end ("… by Friday", "… tomorrow"), first with a colon ("Tomorrow: call
    // Sam"), or after by/on/until anywhere. Short forms and numbers ("fri", "30.10",
    // "3.12") only after such a word: alone they are too often something else (a version, the
    // sun, a person called Jan).
    let words: Vec<String> = title.split_whitespace().map(str::to_string).collect();
    let prepositions = ["by", "on", "until", "till", "before", "due", "bis", "am"];
    'due: for len in (1..=3).rev() {
        for start in (0..words.len().saturating_sub(len - 1)).rev() {
            let end = start + len;
            let phrase = words[start..end].join(" ");
            let Some(day) = day::parse_when(&phrase, today) else { continue };
            let at_end = end == words.len();
            let before = start.checked_sub(1).map(|i| bare(&words[i]));
            let lower_phrase = phrase.to_lowercase();
            // "for" goes with a day ("for Monday"), not with a number ("tests for 2.4").
            let prep = before.as_deref().is_some_and(|b| prepositions.contains(&b) || (b == "for" && !lower_phrase.chars().any(|c| c.is_ascii_digit())));
            let leading = start == 0 && words[end - 1].ends_with(':');
            let lower = phrase.to_lowercase();
            let numeric = lower.chars().any(|c| c.is_ascii_digit());
            let short_weekday = len == 1 && day::weekday_of(&lower).is_some() && !day::is_full_weekday(&lower);
            let month_day = len == 2 && numeric && !lower.starts_with("in ");
            let plain_ok = !numeric && !short_weekday || lower.starts_with("in ") || (numeric && lower.ends_with('.') && lower.matches('.').count() == 2);
            let ok = prep || leading || (at_end && plain_ok && !month_day);
            if !ok {
                continue;
            }
            draft.due = Some(day);
            let (from, to) = if leading { (0, end) } else if at_end { (if prep { start - 1 } else { start }, end) } else if prep { (start - 1, end) } else { (start, end) };
            let rest: Vec<String> = words[..from].iter().chain(words[to..].iter()).cloned().collect();
            // Keep the words when taking them out would leave next to nothing ("Plan next week").
            if rest.len() >= 2 {
                title = rest.join(" ");
            }
            break 'due;
        }
    }

    // Who is involved.
    let words: Vec<&str> = title.split_whitespace().collect();
    let known = |w: &str| {
        let w = clean_name(w);
        // Written as a name: a known name in lower case is usually a word ("max width").
        if !w.chars().next().is_some_and(char::is_uppercase) {
            return None;
        }
        people.iter().find(|p| p.eq_ignore_ascii_case(&w) || p.split_whitespace().next().is_some_and(|first| first.eq_ignore_ascii_case(&w))).cloned()
    };
    let group_after = |i: usize| words.get(i + 1).is_some_and(|w| GROUP_WORDS.contains(&bare(w).as_str()));
    let name_at = |i: usize| -> Option<String> {
        let w = *words.get(i)?;
        if group_after(i) {
            return None;
        }
        known(w).or_else(|| is_name(&clean_name(w)).then(|| clean_name(w)))
    };
    let to_name = |from: usize| (from..words.len()).find(|&j| bare(words[j]) == "to" && name_at(j + 1).is_some()).and_then(|j| name_at(j + 1));
    for (i, w) in words.iter().enumerate() {
        let lw = bare(w);
        if let Some(name) = w.strip_prefix('@') {
            draft.person = Some(clean_name(name));
            draft.list = List::Asked;
            break;
        }
        if lw == "waiting" && matches!(words.get(i + 1).map(|w| bare(w)).as_deref(), Some("on" | "for")) {
            if let Some(name) = name_at(i + 2) {
                draft.person = Some(name);
                draft.list = List::Waiting;
                break;
            }
        }
        if i > 0 && matches!(lw.as_str(), "asked" | "asks" | "requested") {
            if let Some(name) = name_at(i - 1) {
                draft.person = Some(name);
                draft.list = List::Asked;
                break;
            }
        }
        if i > 0 && matches!(lw.as_str(), "wants" | "needs") {
            if let Some(name) = known(words[i - 1]) {
                draft.person = Some(name);
                draft.list = List::Asked;
                break;
            }
        }
        if matches!(lw.as_str(), "for" | "from") {
            if let Some(name) = words.get(i + 1).and_then(|w| known(w)) {
                draft.person = Some(name);
                draft.list = List::Asked;
                break;
            }
        }
        let person = if ASK_VERBS.contains(&lw.as_str()) {
            name_at(i + 1)
        } else if GIVE_VERBS.contains(&lw.as_str()) {
            to_name(i + 1).or_else(|| name_at(i + 1))
        } else if lw == "reply" && words.get(i + 1).is_some_and(|w| bare(w) == "to") {
            name_at(i + 2)
        } else if lw == "follow" && words.get(i + 1).is_some_and(|w| bare(w) == "up") {
            name_at(if words.get(i + 2).is_some_and(|w| bare(w) == "with") { i + 3 } else { i + 2 })
        } else {
            None
        };
        if let Some(name) = person {
            draft.person = Some(name);
            draft.list = List::Todo;
            break;
        }
    }

    // Tickets and pull requests it mentions.
    for w in title.split_whitespace() {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '#' && c != '-');
        let is_pr = w.strip_prefix('#').is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        let is_ticket = crate::linear::identifiers_in(w).first().is_some_and(|id| id == w);
        if (is_pr || is_ticket) && !draft.links.iter().any(|l| l == w) {
            draft.links.push(w.to_string());
        }
    }

    let mut title = title.trim().trim_end_matches([',', ';', '-', ':']).trim().to_string();
    if let Some(first) = title.chars().next() {
        if first.is_lowercase() {
            title = first.to_uppercase().collect::<String>() + &title[first.len_utf8()..];
        }
    }
    draft.title = if title.is_empty() { text.trim().to_string() } else { title };
    draft
}

/// The board as plain text, for `glasscode board`.
pub fn render_text(view: &TodayView, summary: &str, all: bool) -> String {
    let mut out = format!("Today · {}\n{summary}\n", view.date);
    for s in &view.sections {
        let hidden = !all && matches!(s.section, Some(Section::Later | Section::Recurring | Section::Done));
        if s.items.is_empty() {
            continue;
        }
        out.push('\n');
        if hidden {
            out.push_str(&format!("{} ({}) — `glasscode board --all` shows them\n", s.title, s.items.len()));
            continue;
        }
        out.push_str(&format!("{}\n", s.title));
        for item in &s.items {
            out.push_str(&format!("  {} {:<9} {}{}\n", if item.done { "[x]" } else { "[ ]" }, item.id, item.title, item_meta(item)));
        }
    }
    out.trim_end().to_string()
}

fn item_meta(item: &Item) -> String {
    let mut meta = Vec::new();
    if let (Some(label), false) = (&item.due_label, item.done) {
        meta.push(label.clone());
    }
    if let Some(snoozed) = &item.snoozed {
        meta.push(format!("back {snoozed}"));
    }
    if let Some(person) = &item.person {
        meta.push(match item.person_role.as_deref() {
            Some("asked") => format!("{person} asked"),
            Some("waiting") => format!("waiting on {person}"),
            Some("author") => format!("by {person}"),
            _ => format!("with {person}"),
        });
    }
    if let Some(source) = &item.source {
        meta.push(source.clone());
    }
    if item.urgent {
        meta.push("urgent".into());
    }
    if let Some(by) = &item.added_by {
        meta.push(by.clone());
    }
    if meta.is_empty() { String::new() } else { format!("  · {}", meta.join(" · ")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tuesday() -> Day {
        day::from_civil(2026, 10, 27)
    }

    #[test]
    fn reads_the_add_box() {
        let people = vec!["Sam".to_string(), "Jörg".to_string(), "Alex".to_string()];
        let d = parse_quick("Ask Sam about the API token by Friday", tuesday(), &people);
        assert_eq!((d.title.as_str(), d.person.as_deref(), d.list, d.due.map(day::iso).as_deref()), ("Ask Sam about the API token", Some("Sam"), List::Todo, Some("2026-10-30")));
        let d = parse_quick("Robin asked which queue the tickets use", tuesday(), &people);
        assert_eq!((d.person.as_deref(), d.list), (Some("Robin"), List::Asked));
        let d = parse_quick("export meeting summaries as Word for Jörg tomorrow", tuesday(), &people);
        assert_eq!((d.title.as_str(), d.person.as_deref(), d.list, d.due.map(day::iso).as_deref()), ("Export meeting summaries as Word for Jörg", Some("Jörg"), List::Asked, Some("2026-10-28")));
        let d = parse_quick("Waiting on Alex for the API permissions", tuesday(), &people);
        assert_eq!((d.person.as_deref(), d.list), (Some("Alex"), List::Waiting));
        let d = parse_quick("Every Monday: review the Renovate PRs", tuesday(), &people);
        assert_eq!((d.title.as_str(), d.every), ("Review the Renovate PRs", Some(Every::Week { day: 0 })));
        let d = parse_quick("Check the staging cluster every month-end", tuesday(), &people);
        assert_eq!(d.every, Some(Every::MonthEnd));
        let d = parse_quick("Fix ENG-42 before #958 merges today", tuesday(), &people);
        assert_eq!((d.links.clone(), d.due.map(day::iso)), (vec!["ENG-42".to_string(), "#958".to_string()], Some("2026-10-27".to_string())));
        let d = parse_quick("Prepare the report for Monday", tuesday(), &people);
        assert_eq!((d.title.as_str(), d.due.map(day::iso).as_deref()), ("Prepare the report", Some("2026-11-02")), "a weekday at the end is its day");
        let d = parse_quick("Prepare the Monday report", tuesday(), &people);
        assert_eq!(d.due, None, "a weekday inside the words is not");
        let d = parse_quick("Write tests for Parser", tuesday(), &people);
        assert_eq!(d.person, None, "an unknown name after “for” is not a person");
        // Not dates.
        for line in ["Update Python to 3.12", "Upgrade node to 20.10", "Release 2.4.1", "Bump glasscode to 1.2.3", "Review PR 2/3", "Write tests for 2.4", "Fix the today board sorting", "Today view: fix counts", "Talk about the sun", "Meet Jan 12"] {
            assert_eq!(parse_quick(line, tuesday(), &people).due, None, "{line}");
            assert_eq!(parse_quick(line, tuesday(), &people).title, line, "{line}");
        }
        // Dates.
        let due = |line: &str| parse_quick(line, tuesday(), &people).due.map(day::iso);
        assert_eq!(due("Tomorrow: call Sam").as_deref(), Some("2026-10-28"));
        assert_eq!(parse_quick("Tomorrow: call Sam", tuesday(), &people).title, "Call Sam");
        assert_eq!(due("Send the report by this Friday").as_deref(), Some("2026-10-30"));
        assert_eq!(parse_quick("Send the report by this Friday", tuesday(), &people).title, "Send the report");
        assert_eq!(due("Pay the invoice by 30.10.").as_deref(), Some("2026-10-30"));
        assert_eq!(due("Pay the invoice on Jan 12").as_deref(), Some("2027-01-12"));
        assert_eq!(due("Plan next week").as_deref(), Some("2026-11-02"));
        assert_eq!(parse_quick("Plan next week", tuesday(), &people).title, "Plan next week", "too little left to cut");
        assert_eq!(due("Rechnung bezahlen bis Freitag").as_deref(), Some("2026-10-30"));
        // Not people.
        for line in ["Build needs tests", "Dashboard needs a refresh", "Parser wants a rewrite", "Tell Claude to rerun the checks", "Ask the design team about limits", "Fix bug for max width"] {
            assert_eq!(parse_quick(line, tuesday(), &people).person, None, "{line}");
        }
        let d = parse_quick("Send PR to Sam", tuesday(), &people);
        assert_eq!(d.person.as_deref(), Some("Sam"));
        let d = parse_quick("Send the wording to Jörg", tuesday(), &people);
        assert_eq!(d.person.as_deref(), Some("Jörg"));
        assert!(!parse_quick("Fix non-urgent typo", tuesday(), &people).urgent);
        assert!(parse_quick("Fix the login, urgent", tuesday(), &people).urgent);
        // Recurring.
        assert_eq!(parse_quick("Review Renovate PRs every thurs", tuesday(), &people).every, Some(Every::Week { day: 3 }));
        let d = parse_quick("Every Monday and Friday: sync with Jan", tuesday(), &people);
        assert_eq!((d.every, d.title.as_str()), (Some(Every::Days { mask: 0b10001 }), "Sync with Jan"));
        assert_eq!(Every::Days { mask: 0b10001 }.label(), "Every Monday and Friday");
    }

    #[test]
    fn places_items_where_they_belong() {
        let today = tuesday();
        let mut board = Board::default();
        let asked = board.add(Draft { title: "Tell Robin the queue".into(), list: List::Asked, person: Some("Robin".into()), ..Default::default() }, Origin::you(), today);
        let late = board.add(Draft { title: "Weekly Renovate review".into(), due: Some(today - 1), ..Default::default() }, Origin::you(), today);
        let todo = board.add(Draft { title: "Read the OCR notes".into(), ..Default::default() }, Origin { who: "Claude".into(), session: Some("s1".into()), session_title: Some("OCR service".into()) }, today);
        let outside = vec![
            Outside { key: "linear:ENG-42".into(), reference: "ENG-42".into(), title: "The API integration".into(), section: Some(Section::Tickets), due: Some(day::iso(today - 4)), source: "Linear · In Progress".into(), ..Default::default() },
            Outside { key: "review:acme/app#958".into(), reference: "#958".into(), title: "Browser side panel".into(), section: Some(Section::Reviews), source: "GitHub".into(), person: Some("octocat".into()), ..Default::default() },
        ];
        board.set_focus(&format!("task:{todo}"), true).unwrap();
        let view = board.view(&outside, today);
        let ids = |s: Section| view.section(s).map(|s| s.items.iter().map(|i| i.id.clone()).collect::<Vec<_>>()).unwrap_or_default();
        assert_eq!(ids(Section::Focus), vec![todo.clone()]);
        assert_eq!(ids(Section::Due), vec!["ENG-42".to_string(), late.clone()], "the most overdue first");
        assert_eq!(ids(Section::Asked), vec![asked.clone()]);
        assert_eq!(ids(Section::Reviews), vec!["#958".to_string()]);
        assert_eq!((view.due, view.asked), (2, 1));
        assert_eq!(view.summary(1), "2 things are due or late, 1 person is waiting on you, and 1 session needs you.");
        let focus = &view.section(Section::Focus).unwrap().items[0];
        assert_eq!(focus.added_by.as_deref(), Some("Added by Claude · OCR service"));

        // Done, snoozed, moved: for tasks and for items from elsewhere.
        board.set_done(&format!("task:{asked}"), true, today).unwrap();
        board.snooze("linear:ENG-42", Some(today + 1)).unwrap();
        board.move_to("review:acme/app#958", Section::Later).unwrap();
        let view = board.view(&outside, today);
        let ids = |s: Section| view.section(s).map(|s| s.items.iter().map(|i| i.id.clone()).collect::<Vec<_>>()).unwrap_or_default();
        assert_eq!(ids(Section::Done), vec![asked.clone()]);
        assert!(ids(Section::Later).contains(&"ENG-42".to_string()) && ids(Section::Later).contains(&"#958".to_string()));
        // The next day the done task is gone and the snoozed ticket is back (and late).
        let view = board.view(&outside, today + 1);
        assert!(view.items().all(|i| i.id != asked));
        assert!(view.section(Section::Due).unwrap().items.iter().any(|i| i.id == "ENG-42"));

        assert_eq!(board.resolve(&outside, "eng-42"), Ok("linear:ENG-42".to_string()));
        assert_eq!(board.resolve(&outside, &late), Ok(format!("task:{late}")));
        assert_eq!(board.resolve(&outside, "renovate review"), Ok(format!("task:{late}")));
        assert!(board.resolve(&outside, "nothing like it").is_err());
        assert!(board.set_focus(&format!("task:{late}"), true).is_ok());
        board.set_focus("linear:ENG-42", true).unwrap();
        assert!(board.set_focus(&format!("task:{asked}"), true).is_err(), "Focus holds three");
    }

    #[test]
    fn asked_items_stay_counts_and_lookups() {
        let today = tuesday();
        let mut board = Board::default();
        let robin = board.add(Draft { title: "Tell Robin the queue".into(), list: List::Asked, person: Some("Robin".into()), due: Some(today), ..Default::default() }, Origin::you(), today);
        board.add(Draft { title: "Another for Robin".into(), list: List::Asked, person: Some("Robin".into()), ..Default::default() }, Origin::you(), today);
        let outside = vec![
            Outside { key: "linear:ENG-42".into(), reference: "ENG-42".into(), title: "The API integration".into(), section: Some(Section::Tickets), due: Some(day::iso(today - 4)), source: "Linear".into(), ..Default::default() },
            Outside { key: "review:a/one#958".into(), reference: "#958".into(), title: "One".into(), section: Some(Section::Reviews), due: Some(day::iso(today)), source: "GitHub · a/one".into(), ..Default::default() },
            Outside { key: "review:b/two#958".into(), reference: "#958".into(), title: "Two".into(), section: Some(Section::Reviews), source: "GitHub · b/two".into(), ..Default::default() },
            Outside { key: "review:b/two#12".into(), reference: "#12".into(), title: "Twelve".into(), section: Some(Section::Reviews), source: "GitHub · b/two".into(), ..Default::default() },
        ];
        let view = board.view(&outside, today);
        let asked: Vec<String> = view.section(Section::Asked).unwrap().items.iter().map(|i| i.id.clone()).collect();
        assert!(asked.contains(&robin), "due today, and still under Asked of me");
        assert!(view.section(Section::Reviews).unwrap().items.iter().any(|i| i.key == "review:a/one#958"), "a review due today stays a review");
        assert_eq!(view.asked, 1, "Robin once, not per task");
        // A snoozed ticket is not due today.
        board.snooze("linear:ENG-42", Some(today + 1)).unwrap();
        let view = board.view(&outside, today);
        assert_eq!(view.due, 2, "Robin's task and the review; not the snoozed ticket");
        // Un-snoozing brings a task back as it was.
        let key = format!("task:{robin}");
        board.snooze(&key, Some(today + 2)).unwrap();
        board.snooze(&key, None).unwrap();
        assert_eq!(board.task(&robin).unwrap().due, Some(day::iso(today)));
        // Lookups.
        assert_eq!(board.resolve(&outside, "12"), Ok("review:b/two#12".to_string()), "a bare number is a pull request");
        assert!(board.resolve(&outside, "#958").unwrap_err().contains("more than one"));
        // Focus: a key whose item is gone does not hold a place, and can be taken out by name.
        board.focus = vec!["review:gone#1".into(), "linear:ENG-42".into(), key.clone()];
        assert_eq!(board.resolve(&outside, "gone#1"), Ok("review:gone#1".to_string()));
        assert!(board.prune_focus(&outside));
        assert_eq!(board.focus, vec!["linear:ENG-42".to_string(), key]);
        // All done.
        let mut done = Board::default();
        let id = done.add(Draft { title: "One".into(), ..Default::default() }, Origin::you(), today);
        done.set_done(&format!("task:{id}"), true, today).unwrap();
        assert_eq!(done.view(&[], today).summary(0), "Everything on today's board is done.");
    }

    #[test]
    fn one_bad_entry_keeps_the_board() {
        let json = r#"{
            "tasks": [
                { "id": "aaaa", "title": "Fine", "created_at": 1 },
                { "id": "bbbb", "title": "From a newer build", "list": "someday-maybe", "created_at": 1 },
                { "title": "No id" }
            ],
            "recurring": [
                { "id": "rrrr", "title": "Weekly", "every": "week", "day": 0, "created_at": 1 },
                { "id": "ssss", "title": "Unknown rule", "every": "glasscoder", "created_at": 1 }
            ],
            "marks": { "linear:ENG-1": { "moved": "a-new-section" }, "linear:ENG-2": { "snoozed_until": "2026-11-01" } }
        }"#;
        let board: Board = serde_json::from_str(json).unwrap();
        assert_eq!(board.tasks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["aaaa", "bbbb"]);
        assert_eq!(board.tasks[1].list, List::Todo, "an unknown list is To do");
        assert_eq!(board.recurring.len(), 1);
        assert_eq!(board.marks.len(), 2);
        assert_eq!(board.marks["linear:ENG-1"].moved, None);
    }

    #[test]
    fn recurring_tasks_come_back_once() {
        let monday = day::from_civil(2026, 10, 26);
        let mut board = Board::default();
        let id = board.add(Draft { title: "Weekly Renovate review".into(), every: Some(Every::Week { day: 0 }), ..Default::default() }, Origin::you(), monday);
        let copies = |b: &Board| b.tasks.iter().filter(|t| t.recurring.as_deref() == Some(&id)).count();
        assert_eq!(copies(&board), 1, "today is a Monday: today's copy");
        board.prepare(monday + 1);
        assert_eq!(copies(&board), 1);
        // A week later the first one is still open: no second copy, the first shows late.
        board.prepare(monday + 7);
        assert_eq!(copies(&board), 1);
        let view = board.view(&[], monday + 7);
        assert_eq!(view.section(Section::Due).unwrap().items[0].due_label.as_deref(), Some("7 days late"));
        // Done: the next Monday makes a new one.
        let key = board.tasks[0].key();
        board.set_done(&key, true, monday + 7).unwrap();
        board.prepare(monday + 14);
        assert_eq!(copies(&board), 2);
        assert_eq!(board.view(&[], monday + 14).section(Section::Recurring).unwrap().items[0].source.as_deref(), Some("Every Monday"));
    }
}
