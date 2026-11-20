//! What a playbook is (design section 8; review events-11): typed tables, never an expression
//! language. A trigger is an event kind with a `where` table of field = value (or a list of
//! allowed values), all of which must match; conditions are typed; an action is an op with
//! arguments whose slots are fixed text the person wrote or a typed field of the event (a number,
//! an id, a URL, a branch), never free text from outside; a limit is a count per commit, PR,
//! session, hour or day. The fields each event kind has, and their types, come from the journal's
//! catalogue (M14) as a [`Schema`]; a playbook is checked against it when it loads
//! ([`validate`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::gate::Level;

/// How far a playbook goes on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Autonomy {
    /// Only note that it would have fired.
    Watch,
    /// Tell the person.
    Tell,
    /// Ask the person first (a card).
    Ask,
    /// Do it.
    Act,
}

impl Autonomy {
    /// When two playbooks would do the same thing about one event, the stricter wins: watch, then
    /// ask, then tell, then act (events-11's "never > ask > tell > act", with watch as never).
    pub fn strictness(self) -> u8 {
        match self {
            Autonomy::Watch => 0,
            Autonomy::Ask => 1,
            Autonomy::Tell => 2,
            Autonomy::Act => 3,
        }
    }

    pub fn stricter(self, other: Autonomy) -> Autonomy {
        if self.strictness() <= other.strictness() { self } else { other }
    }
}

/// When a schedule fires (M41 runs them).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "every", rename_all = "snake_case")]
pub enum ScheduleSpec {
    /// Every so many seconds.
    Every { secs: u64 },
    /// At a local time (minutes after midnight) on some days (bit 0 is Sunday; 0 is every day), in
    /// a time zone.
    At { minutes: u16, days: u8, tz: String },
    /// Once, at a time.
    Once { at_ms: u64 },
}

/// One field's test: the value, or one of a list of values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Match {
    Any(Vec<serde_json::Value>),
    One(serde_json::Value),
}

impl Match {
    pub fn matches(&self, value: &serde_json::Value) -> bool {
        match self {
            Match::One(want) => want == value,
            Match::Any(allowed) => allowed.contains(value),
        }
    }

    fn values(&self) -> Vec<&serde_json::Value> {
        match self {
            Match::One(value) => vec![value],
            Match::Any(values) => values.iter().collect(),
        }
    }
}

/// Events of a kind whose fields match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventFilter {
    /// The event kind, as the journal names it: `ci.failed`.
    pub kind: String,
    /// Field = value (or allowed values), all of which must hold.
    #[serde(default, rename = "where", skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, Match>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "on", rename_all = "snake_case")]
pub enum Trigger {
    Event(EventFilter),
    Schedule { schedule: ScheduleSpec },
    /// An event, then a wait (a follow-up that stays quiet).
    After { event: EventFilter, delay_secs: u64 },
}

impl Trigger {
    pub fn filter(&self) -> Option<&EventFilter> {
        match self {
            Trigger::Event(filter) | Trigger::After { event: filter, .. } => Some(filter),
            Trigger::Schedule { .. } => None,
        }
    }
}

/// A typed condition (events-11), checked against the event and what Glasscode knows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "if", rename_all = "snake_case")]
pub enum Cond {
    /// The project's level is one of these.
    Level { is: Vec<Level> },
    /// The author of the outside text passes `trusted_author` (the event's `author_trusted`).
    AuthorTrusted,
    /// The PR is the person's own (the event's `pr_author` is the person's login).
    PrAuthorIsMe,
    /// The PR has this label (the event's `labels`).
    Label { name: String },
    /// The branch is not the repository's default (the event's `branch` and `default_branch`).
    NotDefaultBranch,
}

/// A part of an argument.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    /// Text the person wrote into the playbook.
    Text(String),
    /// A typed field of the event: a number, an id, a URL or a branch, never free text.
    Field(String),
}

/// What it does: an op and its arguments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    /// The op's name in the registry: `session.send`.
    pub op: String,
    /// Each argument, made of slots joined together.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, Vec<Slot>>,
}

/// What a limit counts by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Per {
    /// The PR's head commit (`head`).
    Commit,
    /// The PR (`repo` and `number`).
    Pr,
    /// The session (`session`).
    Session,
    Hour,
    Day,
}

/// At most `count` firings per subject (or per hour or day).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limit {
    pub count: u32,
    pub per: Per,
}

impl Default for Limit {
    fn default() -> Limit {
        Limit { count: 1, per: Per::Commit }
    }
}

/// Who stopped a playbook, and why (D15: any caller may; only the person releases it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paused {
    pub by: String,
    pub since_ms: u64,
    pub why: String,
}

/// One playbook of `authority/playbooks.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Playbook {
    pub id: String,
    pub name: String,
    pub trigger: Trigger,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub when: Vec<Cond>,
    pub then: Action,
    pub autonomy: Autonomy,
    #[serde(default)]
    pub limit: Limit,
    /// One of Glasscode's own ([`super::builtins`]): its rules are code, this entry its settings.
    #[serde(default)]
    pub builtin: bool,
    #[serde(default)]
    pub enabled: bool,
    /// It also reacts to events the manager or a playbook caused (events-13: by default it does
    /// not).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub manager_events: bool,
    /// Who wrote it, when not the person (a wake, a session): it is only proposed then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_by: Option<String>,
    /// Stopped by a caller or the breaker; not part of what was approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<Paused>,
    /// The hash this machine approved it with ([`super::ApprovedPlaybooks`]), filled in on load;
    /// never on disk in the live file.
    #[serde(skip)]
    pub approved_hash: Option<[u8; 32]>,
}

impl Playbook {
    /// What the person approves: the definition, its switch included, and nothing else (not its
    /// pause, not who proposed it).
    pub fn hash(&self) -> [u8; 32] {
        let definition = serde_json::json!({
            "id": self.id,
            "name": self.name,
            "trigger": self.trigger,
            "when": self.when,
            "then": self.then,
            "autonomy": self.autonomy,
            "limit": self.limit,
            "builtin": self.builtin,
            "enabled": self.enabled,
            "manager_events": self.manager_events,
        });
        Sha256::digest(definition.to_string().as_bytes()).into()
    }

    /// Approved on this machine, as it is now.
    pub fn approved(&self) -> bool {
        self.approved_hash == Some(self.hash())
    }

    /// It may fire: approved, switched on, and not paused.
    pub fn live(&self) -> bool {
        self.enabled && self.paused.is_none() && self.approved()
    }
}

// ----- the field schema ------------------------------------------------------------------------

/// What a field of an event holds, as the journal's catalogue says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldType {
    Number,
    Bool,
    /// An id: a session's, a commit's, a PR's repository, an account's.
    Id,
    Url,
    Branch,
    /// One word of a fixed set (a state, a conclusion).
    Word,
    /// A list of words (labels, check names): outside words, so it can be tested, never put into
    /// an argument.
    Words,
    /// Free text from outside (a title, a comment): it can be tested, never put into an argument.
    Text,
}

impl FieldType {
    /// It may go into an argument (events-11: free outside text never does, nor a list of outside
    /// words such as check names).
    pub fn typed(self) -> bool {
        !matches!(self, FieldType::Text | FieldType::Words)
    }

    /// Whether a value is of this type.
    pub fn holds(self, value: &serde_json::Value) -> bool {
        use serde_json::Value;
        match self {
            FieldType::Number => value.is_u64() || value.is_i64(),
            FieldType::Bool => value.is_boolean(),
            FieldType::Words => value.as_array().is_some_and(|list| list.iter().all(Value::is_string)),
            FieldType::Id | FieldType::Word => value.as_str().is_some_and(|text| !text.is_empty() && text.len() <= 200 && !text.chars().any(char::is_whitespace)),
            FieldType::Branch => value.as_str().is_some_and(|text| !text.is_empty() && text.len() <= 250 && !text.chars().any(|c| c.is_whitespace() || c.is_control())),
            FieldType::Url => value.as_str().is_some_and(|text| text.starts_with("https://") && !text.chars().any(char::is_whitespace)),
            FieldType::Text => value.is_string(),
        }
    }
}

/// The fields of each event kind (the journal's catalogue, M14).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Schema {
    pub kinds: BTreeMap<String, BTreeMap<String, FieldType>>,
}

impl Schema {
    pub fn with(mut self, kind: &str, fields: &[(&str, FieldType)]) -> Schema {
        self.kinds.insert(kind.to_string(), fields.iter().map(|(name, ty)| (name.to_string(), *ty)).collect());
        self
    }

    pub fn field(&self, kind: &str, name: &str) -> Option<FieldType> {
        self.kinds.get(kind)?.get(name).copied()
    }
}

/// The typed fields of the journal's events that a playbook may test or use (the catalogue of
/// `journal_kinds`, whose `data` column says what each kind holds). The session is every event's.
pub fn journal_schema() -> Schema {
    use FieldType::{Bool, Branch, Id, Number, Text, Word, Words};
    let pr: &[(&str, FieldType)] = &[("repo", Id), ("number", Number), ("session", Id), ("project", Id)];
    let with = |extra: &[(&'static str, FieldType)]| -> Vec<(&'static str, FieldType)> { pr.iter().copied().chain(extra.iter().copied()).collect() };
    let session: &[(&str, FieldType)] = &[("session", Id), ("project", Id)];
    let and = |extra: &[(&'static str, FieldType)]| -> Vec<(&'static str, FieldType)> { session.iter().copied().chain(extra.iter().copied()).collect() };
    Schema::default()
        .with("ci.failed", &with(&[("head", Id), ("failed", Words)]))
        .with("ci.passed", &with(&[("head", Id), ("failed", Words)]))
        .with("ci.running", &with(&[("head", Id), ("failed", Words)]))
        .with("pr.conflict", &with(&[("mergeable", Word)]))
        .with("pr.merged", &with(&[]))
        .with("pr.closed", &with(&[]))
        .with("pr.opened", &with(&[("title", Text), ("draft", Bool)]))
        .with("pr.changed", &with(&[("state", Word), ("draft", Bool), ("before", Word)]))
        .with("session.started", &and(&[("agent", Word), ("source", Word)]))
        .with("session.activity", &and(&[("activity", Word), ("previous", Word)]))
        .with("session.needs_input", &and(&[("notification_type", Word), ("message", Text)]))
        .with("session.ended", &and(&[("reason", Word)]))
        .with("git.commit", &and(&[("worktree", Text), ("branch", Branch), ("head", Id), ("before", Id)]))
        .with("git.push", &and(&[("worktree", Text), ("branch", Branch), ("head", Id)]))
        .with("git.branch", &and(&[("worktree", Text), ("branch", Branch), ("before", Branch)]))
}

/// The fields a condition reads.
fn cond_fields(cond: &Cond) -> &'static [&'static str] {
    match cond {
        Cond::Level { .. } => &[],
        Cond::AuthorTrusted => &["author_trusted"],
        Cond::PrAuthorIsMe => &["pr_author"],
        Cond::Label { .. } => &["labels"],
        Cond::NotDefaultBranch => &["branch", "default_branch"],
    }
}

/// The field a limit counts by.
pub fn per_fields(per: Per) -> &'static [&'static str] {
    match per {
        Per::Commit => &["head"],
        Per::Pr => &["repo", "number"],
        Per::Session => &["session"],
        Per::Hour | Per::Day => &[],
    }
}

/// Check a playbook against the registry and the schema: every problem, in words. A built-in's
/// rules are code: only its entry is checked.
pub fn validate(playbook: &Playbook, schema: &Schema) -> Result<(), Vec<String>> {
    let mut problems = Vec::new();
    if playbook.id.is_empty() || playbook.id.len() > 64 || !playbook.id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        problems.push(format!("its id {:?} is not 1 to 64 of a-z, 0-9 and -", playbook.id));
    }
    if playbook.name.trim().is_empty() {
        problems.push("it has no name".to_string());
    }
    if playbook.limit.count == 0 {
        problems.push("its limit is 0, so it would never fire".to_string());
    }
    if playbook.builtin {
        // Only one of Glasscode's own is a built-in, whatever the file says.
        if super::builtins::find(&playbook.id).is_none() {
            problems.push(format!("no built-in is called {:?}", playbook.id));
        }
        return if problems.is_empty() { Ok(()) } else { Err(problems) };
    }
    let kind = playbook.trigger.filter().map(|filter| filter.kind.clone());
    if let Some(filter) = playbook.trigger.filter() {
        match schema.kinds.get(&filter.kind) {
            None => problems.push(format!("no event is of the kind {:?}", filter.kind)),
            Some(_) => {
                for (name, test) in &filter.fields {
                    match schema.field(&filter.kind, name) {
                        None => problems.push(format!("{} events have no field {name:?}", filter.kind)),
                        Some(ty) => {
                            for value in test.values() {
                                if !ty.holds(value) {
                                    problems.push(format!("{name:?} is {ty:?}, and {value} is not"));
                                }
                            }
                        }
                    }
                }
                for cond in &playbook.when {
                    for field in cond_fields(cond) {
                        if schema.field(&filter.kind, field).is_none() {
                            problems.push(format!("{:?} needs the field {field:?}, which {} events do not have", cond, filter.kind));
                        }
                    }
                }
                for field in per_fields(playbook.limit.per) {
                    if schema.field(&filter.kind, field).is_none() {
                        problems.push(format!("its limit counts by {field:?}, which {} events do not have", filter.kind));
                    }
                }
            }
        }
    } else if !playbook.when.iter().all(|cond| matches!(cond, Cond::Level { .. })) {
        problems.push("a scheduled playbook has no event to test its conditions on".to_string());
    }
    match crate::ops::find(&playbook.then.op) {
        None => problems.push(format!("no op is called {:?}", playbook.then.op)),
        Some(spec) => {
            if spec.human_only {
                problems.push(format!("{} is for the person only", spec.name));
            }
            for (name, slots) in &playbook.then.args {
                if !spec.args.iter().any(|arg| arg.name == name) {
                    problems.push(format!("{} has no argument {name:?}", spec.name));
                }
                for slot in slots {
                    let Slot::Field(field) = slot else { continue };
                    match kind.as_deref().and_then(|kind| schema.field(kind, field)) {
                        None => problems.push(format!("the argument {name:?} reads {field:?}, which the event does not have")),
                        Some(ty) if !ty.typed() => problems.push(format!("the argument {name:?} would put the outside text {field:?} into what is typed (events-11)")),
                        Some(_) => {}
                    }
                }
            }
            for arg in spec.args.iter().filter(|arg| arg.required) {
                if !playbook.then.args.contains_key(arg.name) {
                    problems.push(format!("{} needs the argument {:?}", spec.name, arg.name));
                }
            }
        }
    }
    if problems.is_empty() { Ok(()) } else { Err(problems) }
}
