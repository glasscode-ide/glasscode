//! The manager's conversation with the person (M43; design D8, data map `manager/conversation/`,
//! review ux-2).
//!
//! - **Items** are typed: the person's message (with its words record, [`crate::words`]), the
//!   manager's message (streaming, stopped or done), an action line (its effect key), a nudge (its
//!   matter), a card (its pending action), a system line, an error, the summary the brain wrote
//!   when its conversation rolled over, the rollover link itself, and a line imported from v1's
//!   chat ("v1, origin unknown").
//! - **Topics** come from what an item refers to: a PR, a session or a project, else the general
//!   topic ([`Conversation::topic_for`]). A PR or session also leads to the topic of the first item
//!   that named it, so a session's messages stay with its PR's. The person can split items off
//!   into a topic of their own and merge one topic into another.
//! - **Every change is a record**: the whole new version of an item or a topic, with its id and the
//!   HLC stamp of that version. The newest stamp of an id wins, whatever file or machine it came
//!   from, so two homes (M57) merge by taking each other's records ([`Conversation::take_theirs`]),
//!   in any order. A split writes the items again under their new topic; a merge writes the topic
//!   again, pointing at the one it went into. Only retention rewrites a file.
//! - **Files**: `manager/conversation/<topic>.jsonl`, one per topic. An item goes to its topic's
//!   file, a topic record to its own. A line that does not read is skipped (and left in place), as
//!   is a line of a newer schema.
//! - **Retention**: items for 90 days ([`TEXT_DAYS`]); summaries and rollover links last. An
//!   expired item is gone, from the files ([`Conversation::prune`]) and from what is read or taken
//!   in from the other home, so neither side brings it back.
//! - **No tier on disk.** A person's item carries only its words record's id; how far its words
//!   count is read from the words ledger each time ([`Item::tier`]), and only while the item's text
//!   is the record's. A v1 line has none, so it can back nothing.
//! - **Secrets** in posted text are replaced before it is kept, as in the words ledger.
//!
//! The model ([`Conversation`]: folding records, topics, search, reply binding, what the chat lane
//! has not seen, rollover) does no IO. The methods that write ([`Conversation::post`] and the
//! others) stamp with this machine's clock and append to the files, so they never run on the main
//! thread, and they write only in a v2 home (rule 2 of the slice plan): with the engine at v1 they
//! refuse.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::identity::Hlc;
use crate::manager::{ChatLine, Who};
use crate::paths::{MachineId, ProjectKey};
use crate::words::{Tier, WordsId, WordsRecord};

/// How many days an item is kept.
pub const TEXT_DAYS: u64 = 90;

const DAY_MS: u64 = 86_400_000;
const HOUR_MS: u64 = 3_600_000;

/// How long an item is kept, from when it was posted.
pub const RETENTION_MS: u64 = TEXT_DAYS * DAY_MS;

/// The schema of a record. A record of a newer one is never read, and retention leaves it alone.
pub const SCHEMA: u16 = 1;

/// The most items of "since you last spoke" a chat turn is given.
pub const SINCE_LIMIT: usize = 20;

/// After this long without an item, the chat lane's next turn starts a fresh brain conversation.
pub const IDLE_ROLLOVER_MS: u64 = 4 * HOUR_MS;

/// A brain conversation that has seen this many items rolls over (D8: "past a set size").
pub const ROLLOVER_ITEMS: usize = 200;

/// The topic of items that refer to nothing.
pub const GENERAL: &str = "general";

// ----- ids -------------------------------------------------------------------------------------

/// An item's id: 24 hex digits, random; `v1-` and 21 from the line's content for an imported v1
/// line, so importing it again, or from the server's copy of v1, gives the same item.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemId(String);

impl ItemId {
    fn new() -> ItemId {
        let mut bytes = [0u8; 12];
        crate::sys::random_bytes(&mut bytes);
        ItemId(hex(&bytes))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ItemId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A topic's id, which is also its file's name: a safe folder name, as a project key is (1 to 64
/// of a-z 0-9 . _ -, not a Windows device name), checked on parse and on deserialize, so a record
/// from the other home cannot name a file elsewhere. A topic made from a reference has the same id
/// on every machine ([`Ref::topic_id`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct TopicId(String);

impl TopicId {
    pub fn parse(text: &str) -> Option<TopicId> {
        ProjectKey::parse(text).map(|_| TopicId(text.to_string()))
    }

    pub fn general() -> TopicId {
        TopicId(GENERAL.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A new topic of the person's own (a split): `split-` and 12 hex digits.
    fn split() -> TopicId {
        let mut bytes = [0u8; 6];
        crate::sys::random_bytes(&mut bytes);
        TopicId(format!("split-{}", hex(&bytes)))
    }
}

impl<'de> Deserialize<'de> for TopicId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<TopicId, D::Error> {
        let text = String::deserialize(deserializer)?;
        TopicId::parse(&text).ok_or_else(|| serde::de::Error::custom(format!("{text:?} is not a topic")))
    }
}

impl std::fmt::Display for TopicId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ----- references, authors, kinds --------------------------------------------------------------

/// What an item is about.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ref {
    /// A session, by its whole id.
    Session(String),
    /// A pull request: `owner/name` and its number.
    Pr { repo: String, number: u64 },
    Project(ProjectKey),
}

impl Ref {
    /// A pull request's reference, its repository in lower case.
    pub fn pr(repo: &str, number: u64) -> Ref {
        Ref::Pr { repo: repo.to_ascii_lowercase(), number }
    }

    /// The topic made from this reference: the same on every machine.
    pub fn topic_id(&self) -> TopicId {
        let (prefix, name) = match self {
            Ref::Session(id) => ("session", id.clone()),
            Ref::Pr { repo, number } => ("pr", format!("{repo}-{number}")),
            Ref::Project(key) => ("project", key.as_str().to_string()),
        };
        let hash = hex(&Sha256::digest(self.word().as_bytes()));
        TopicId(format!("{prefix}-{}-{}", slug(&name, 40), &hash[..6]))
    }

    /// One word for the reference: `session:<id>`, `pr:<repo>#<n>`, `project:<key>`.
    fn word(&self) -> String {
        match self {
            Ref::Session(id) => format!("session:{id}"),
            Ref::Pr { repo, number } => format!("pr:{}#{number}", repo.to_ascii_lowercase()),
            Ref::Project(key) => format!("project:{key}"),
        }
    }

    fn same(&self, other: &Ref) -> bool {
        self.word() == other.word()
    }

    /// The most specific first: a PR, then a session, then a project.
    fn rank(&self) -> u8 {
        match self {
            Ref::Pr { .. } => 0,
            Ref::Session(_) => 1,
            Ref::Project(_) => 2,
        }
    }

    fn title(&self) -> String {
        match self {
            Ref::Session(id) => format!("Session {}", id.chars().take(8).collect::<String>()),
            Ref::Pr { repo, number } => format!("{repo}#{number}"),
            Ref::Project(key) => key.as_str().to_string(),
        }
    }
}

/// Who posted an item, as Glasscode saw it on the writing side (never what the text says).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    /// The person, through one of the app's composers.
    Person,
    /// The manager's chat lane, answering the person.
    Chat,
    /// A wake of the manager (a playbook, a schedule, an event), by the wake's id.
    Wake(String),
    /// Glasscode itself.
    App,
    /// Imported from v1's chat: who wrote it is not known.
    V1,
}

/// Where a manager message is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    /// Still coming in.
    Streaming,
    /// Cut off: the person stopped it, or the turn ended early.
    Stopped,
    Done,
}

/// What an item is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Kind {
    /// The person's message. Its words record says how far its words count.
    You {
        words: WordsId,
        /// The item it answers ([`Conversation::reply_target`]).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<ItemId>,
    },
    Manager { state: Stream },
    /// Something the manager did, by its effect key (64 hex digits, `effects`), and the journal
    /// event that tells of it.
    Action {
        effect: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event: Option<String>,
    },
    /// Something for the person to look at, by its matter.
    Nudge { matter: String },
    /// An approval card, by its pending action's id.
    Card { pending: String },
    System,
    Error,
    /// What the brain kept of its conversation when it rolled over, up to the item stamped
    /// `through`. It lasts.
    Summary {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        through: Option<Hlc>,
    },
    /// A fresh brain conversation began here, from this summary (none: it began from nothing). It
    /// lasts.
    Rollover {
        brain: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<ItemId>,
    },
    /// A line of v1's chat: origin unknown, so it backs nothing.
    V1 { who: Who },
}

impl Kind {
    /// Kept past [`RETENTION_MS`].
    fn lasts(&self) -> bool {
        matches!(self, Kind::Summary { .. } | Kind::Rollover { .. })
    }

    /// A nudge or a card: open for a reply until it is closed.
    fn is_matter(&self) -> bool {
        matches!(self, Kind::Nudge { .. } | Kind::Card { .. })
    }
}

// ----- records ---------------------------------------------------------------------------------

fn schema() -> u16 {
    SCHEMA
}

/// One version of an item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    #[serde(default = "schema")]
    pub v: u16,
    pub id: ItemId,
    pub topic: TopicId,
    /// When it was first posted: its place in the conversation, and what retention counts from.
    pub posted: Hlc,
    /// This version's stamp: the newest version of an id wins.
    pub at: Hlc,
    pub author: Author,
    pub kind: Kind,
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<Ref>,
    /// A nudge or card that needs nothing more.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub closed: bool,
}

impl Item {
    /// The words record of a person's message; none for anything else, a v1 line included.
    pub fn words(&self) -> Option<&WordsId> {
        match &self.kind {
            Kind::You { words, .. } => Some(words),
            _ => None,
        }
    }

    /// How far the item's words count, from the words ledger ([`crate::words::find`]): it reads the
    /// disk. Only a person's message whose record is found, and whose text is that record's, has a
    /// tier above never: a version from elsewhere that kept the id and changed the text has none.
    pub fn tier(&self) -> Tier {
        self.tier_with(crate::words::find)
    }

    /// [`Item::tier`], with the ledger looked up by `find`.
    pub fn tier_with(&self, find: impl Fn(&WordsId) -> Option<WordsRecord>) -> Tier {
        self.record_with(find).map_or(Tier::Never, |record| record.tier())
    }

    /// Where `quote` is in the person's own typed words of this message ([`WordsRecord::quote`]);
    /// none for any other item, or one whose text is not its record's. It reads the disk.
    pub fn quote(&self, quote: &str) -> Option<(usize, usize)> {
        self.record_with(crate::words::find)?.quote(quote)
    }

    /// The words record of a person's message, when the item's text is the record's (secrets
    /// replaced as the ledger replaces them).
    fn record_with(&self, find: impl Fn(&WordsId) -> Option<WordsRecord>) -> Option<WordsRecord> {
        let record = find(self.words()?)?;
        let bound = !record.measured && (record.hash == sha256(&self.text) || record.hash == sha256(&crate::words::scrub(&self.text)));
        bound.then_some(record)
    }
}

fn sha256(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

/// One version of a topic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Topic {
    #[serde(default = "schema")]
    pub v: u16,
    pub id: TopicId,
    pub at: Hlc,
    pub title: String,
    /// What it was made from; none for the general topic and a split.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<Ref>,
    /// The topic it was merged into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_into: Option<TopicId>,
    /// The topic it was split from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_from: Option<TopicId>,
}

/// A line of a topic's file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum Record {
    Item(Item),
    Topic(Topic),
}

impl Record {
    pub fn at(&self) -> &Hlc {
        match self {
            Record::Item(item) => &item.at,
            Record::Topic(topic) => &topic.at,
        }
    }

    fn v(&self) -> u16 {
        match self {
            Record::Item(item) => item.v,
            Record::Topic(topic) => topic.v,
        }
    }

    /// What it refers to: an item's references, a topic's key (what a sync keeps back, M57).
    pub fn refs(&self) -> Vec<&Ref> {
        match self {
            Record::Item(item) => item.refs.iter().collect(),
            Record::Topic(topic) => topic.key.iter().collect(),
        }
    }

    /// The topic it belongs to (an item's) or is (a topic's).
    pub fn topic(&self) -> &TopicId {
        self.file()
    }

    /// The topic whose file it goes to.
    fn file(&self) -> &TopicId {
        match self {
            Record::Item(item) => &item.topic,
            Record::Topic(topic) => &topic.id,
        }
    }
}

/// What to post: the writer stamps it and gives it an id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub author: Author,
    pub kind: Kind,
    pub text: String,
    pub refs: Vec<Ref>,
    /// The topic it was posted in (the person wrote in a topic's view); none: from its references.
    pub topic: Option<TopicId>,
}

/// Why the next chat turn starts a fresh brain conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RollReason {
    /// The first turn of a new local day.
    NewDay,
    /// Nothing was said for [`IDLE_ROLLOVER_MS`].
    Idle,
    /// The brain conversation saw [`ROLLOVER_ITEMS`] items.
    Size,
}

/// Whether the chat lane's next turn starts a fresh brain conversation (D8, ux-2): one that began
/// at `started_ms`, whose last item was at `last_ms`, and that has seen `items` items since. The
/// screen stays one conversation; the brain's is fresh per day, after an idle spell, or past a size,
/// and starts from a summary.
pub fn rollover_due(started_ms: u64, last_ms: u64, now_ms: u64, items: usize) -> Option<RollReason> {
    let day = |at: u64| crate::clock::local_minutes(at).map(|minutes| minutes.div_euclid(1440));
    if day(started_ms).zip(day(now_ms)).is_some_and(|(started, now)| now > started) {
        Some(RollReason::NewDay)
    } else if now_ms.saturating_sub(last_ms) >= IDLE_ROLLOVER_MS {
        Some(RollReason::Idle)
    } else if items >= ROLLOVER_ITEMS {
        Some(RollReason::Size)
    } else {
        None
    }
}

// ----- the model -------------------------------------------------------------------------------

/// The conversation as its records fold: the newest version of each item and topic. It does no IO
/// but in the methods that say so.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Conversation {
    items: HashMap<ItemId, Item>,
    topics: BTreeMap<TopicId, Topic>,
    /// Items posted before this are expired (0: none is).
    cutoff_ms: u64,
}

impl Conversation {
    /// An empty conversation in which items posted more than [`RETENTION_MS`] before `now_ms` are
    /// expired.
    pub fn new(now_ms: u64) -> Conversation {
        Conversation { cutoff_ms: now_ms.saturating_sub(RETENTION_MS), ..Conversation::default() }
    }

    /// The conversation of `records`, in any order.
    pub fn from_records(now_ms: u64, records: impl IntoIterator<Item = Record>) -> Conversation {
        let mut conversation = Conversation::new(now_ms);
        for record in records {
            conversation.take(record);
        }
        conversation
    }

    fn expired(&self, item: &Item) -> bool {
        !item.kind.lasts() && item.posted.wall_ms < self.cutoff_ms
    }

    /// Whether taking `record` would change the conversation.
    pub fn is_new(&self, record: &Record) -> bool {
        if record.v() > SCHEMA {
            return false;
        }
        match record {
            Record::Item(item) => !self.expired(item) && self.items.get(&item.id).is_none_or(|have| item.at > have.at),
            Record::Topic(topic) => self.topics.get(&topic.id).is_none_or(|have| topic.at > have.at),
        }
    }

    /// Take one record in: the newer version of an item or a topic replaces the older; an expired
    /// item is not taken. True when it changed something. The result does not depend on the order
    /// records come in.
    pub fn take(&mut self, record: Record) -> bool {
        if !self.is_new(&record) {
            return false;
        }
        match record {
            Record::Item(item) => {
                self.items.insert(item.id.clone(), item);
            }
            Record::Topic(topic) => {
                self.topics.insert(topic.id.clone(), topic);
            }
        }
        true
    }

    /// Drop the items posted more than [`RETENTION_MS`] before `now_ms` (summaries and rollover
    /// links last); later records of them are not taken either.
    pub fn expire(&mut self, now_ms: u64) {
        self.cutoff_ms = self.cutoff_ms.max(now_ms.saturating_sub(RETENTION_MS));
        let cutoff = self.cutoff_ms;
        self.items.retain(|_, item| item.kind.lasts() || item.posted.wall_ms >= cutoff);
    }

    pub fn item(&self, id: &ItemId) -> Option<&Item> {
        self.items.get(id)
    }

    pub fn topic(&self, id: &TopicId) -> Option<&Topic> {
        self.topics.get(id)
    }

    /// Every item, in the order they were posted.
    pub fn items(&self) -> Vec<&Item> {
        let mut items: Vec<&Item> = self.items.values().collect();
        items.sort_by(|a, b| a.posted.cmp(&b.posted).then_with(|| a.id.cmp(&b.id)));
        items
    }

    /// The topic `topic` is now: the one it was merged into, through every merge. A merge that
    /// would go round in a circle (two homes merged each into the other) holds for neither.
    pub fn resolve(&self, topic: &TopicId) -> TopicId {
        let mut at = topic;
        let mut seen = vec![topic];
        while let Some(next) = self.topics.get(at).and_then(|t| t.merged_into.as_ref()) {
            if seen.contains(&next) {
                return topic.clone();
            }
            seen.push(next);
            at = next;
        }
        at.clone()
    }

    /// The topics not merged into another, by id.
    pub fn topics(&self) -> Vec<&Topic> {
        self.topics.values().filter(|topic| self.resolve(&topic.id) == topic.id).collect()
    }

    /// The items of a topic and of every topic merged into it, in the order they were posted.
    pub fn topic_items(&self, topic: &TopicId) -> Vec<&Item> {
        let topic = self.resolve(topic);
        self.items().into_iter().filter(|item| self.resolve(&item.topic) == topic).collect()
    }

    /// The topic that already has `refs`, else the one to make for them: a topic made from one of
    /// them, or the PR's or session's topic of the first item that named one of its PRs or sessions
    /// (a project counts only for the topic made from it), the most specific reference first; else
    /// the topic of the most specific reference; else the general topic. Through merges.
    pub fn topic_for(&self, refs: &[Ref]) -> TopicId {
        let mut sorted: Vec<&Ref> = refs.iter().collect();
        sorted.sort_by_key(|r| r.rank());
        if let Some(found) = sorted.iter().find_map(|r| self.find_topic(r)) {
            return found;
        }
        sorted.first().map_or_else(TopicId::general, |r| r.topic_id())
    }

    /// The live topic a reference already leads to: its own, else that of the first item naming it
    /// that is in a PR's or a session's topic (not one the person split off, nor the general one).
    fn find_topic(&self, r: &Ref) -> Option<TopicId> {
        let own = r.topic_id();
        if self.topics.contains_key(&own) {
            return Some(self.resolve(&own));
        }
        if matches!(r, Ref::Project(_)) {
            return None;
        }
        let of_work = |item: &&Item| matches!(self.topics.get(&self.resolve(&item.topic)).and_then(|topic| topic.key.as_ref()), Some(Ref::Pr { .. } | Ref::Session(_)));
        let first = self.items.values().filter(|item| item.refs.iter().any(|a| a.same(r))).filter(of_work).min_by(|a, b| a.posted.cmp(&b.posted).then_with(|| a.id.cmp(&b.id)))?;
        Some(self.resolve(&first.topic))
    }

    /// The person's search: items whose text has every word of `query` (a word of the text that
    /// starts with it, ignoring case), summaries among them, newest first; only in `topic` (and
    /// what was merged into it) when one is given.
    pub fn search(&self, query: &str, topic: Option<&TopicId>, limit: usize) -> Vec<&Item> {
        let wanted = words_of(query);
        if wanted.is_empty() {
            return Vec::new();
        }
        let topic = topic.map(|topic| self.resolve(topic));
        let mut hits: Vec<&Item> = self
            .items
            .values()
            .filter(|item| topic.as_ref().is_none_or(|topic| &self.resolve(&item.topic) == topic))
            .filter(|item| {
                let have = words_of(&item.text);
                wanted.iter().all(|word| have.iter().any(|h| h.starts_with(word.as_str())))
            })
            .collect();
        hits.sort_by(|a, b| b.posted.cmp(&a.posted).then_with(|| a.id.cmp(&b.id)));
        hits.truncate(limit);
        hits
    }

    /// What the chat lane has not seen ("since you last spoke", ux-2): the items wakes and
    /// Glasscode posted after the chat lane's own last message (nudges, action lines, cards,
    /// errors), at most [`SINCE_LIMIT`] of the newest, in the order they were posted, with their
    /// ids. The chat turn that answers the person is given them, so "do the second one" has
    /// something to point at.
    pub fn since_last_spoke(&self) -> Vec<&Item> {
        let last = self.items.values().filter(|item| item.author == Author::Chat && matches!(item.kind, Kind::Manager { .. })).map(|item| &item.posted).max();
        let mut items: Vec<&Item> = self
            .items()
            .into_iter()
            .filter(|item| last.is_none_or(|last| &item.posted > last))
            .filter(|item| matches!(item.author, Author::Wake(_) | Author::App) && !item.kind.lasts())
            .collect();
        let skip = items.len().saturating_sub(SINCE_LIMIT);
        items.drain(..skip);
        items
    }

    /// The nudges and cards that are open, in the order they were posted.
    pub fn open(&self) -> Vec<&Item> {
        self.items().into_iter().filter(|item| item.kind.is_matter() && !item.closed).collect()
    }

    /// The item a message of the person answers: the one they replied on, when it is there; else
    /// the only open nudge or card, when exactly one is open; else none.
    pub fn reply_target(&self, on: Option<&ItemId>) -> Option<ItemId> {
        if let Some(on) = on {
            return self.items.contains_key(on).then(|| on.clone());
        }
        match self.open().as_slice() {
            [only] => Some(only.id.clone()),
            _ => None,
        }
    }

    /// The newest rollover link.
    pub fn last_rollover(&self) -> Option<&Item> {
        self.items.values().filter(|item| matches!(item.kind, Kind::Rollover { .. })).max_by(|a, b| a.posted.cmp(&b.posted))
    }

    /// The summary the brain conversation `brain` started from (D8).
    pub fn started_from(&self, brain: &str) -> Option<&Item> {
        let link = self.items.values().find(|item| matches!(&item.kind, Kind::Rollover { brain: b, .. } if b == brain))?;
        match &link.kind {
            Kind::Rollover { summary: Some(summary), .. } => self.items.get(summary),
            _ => None,
        }
    }

    /// How many items were posted since the newest rollover link (all, without one): for
    /// [`rollover_due`].
    pub fn items_since_rollover(&self) -> usize {
        let since = self.last_rollover().map(|item| item.posted.clone());
        self.items.values().filter(|item| !item.kind.lasts() && since.as_ref().is_none_or(|since| &item.posted > since)).count()
    }

    /// Every record stamped after `after` (all, without it), oldest first: what M57 sends the other
    /// home. Each is the newest version of its item or topic.
    pub fn changes_after(&self, after: Option<&Hlc>) -> Vec<Record> {
        let items = self.items.values().cloned().map(Record::Item);
        let topics = self.topics.values().cloned().map(Record::Topic);
        let mut records: Vec<Record> = items.chain(topics).filter(|record| after.is_none_or(|after| record.at() > after)).collect();
        records.sort_by(|a, b| a.at().cmp(b.at()));
        records
    }

    /// The records that post `draft`, stamped `at` (the item) and `topic_at` (its topic, when that
    /// is new): the topic's first.
    fn plan_post(&self, draft: Draft, id: ItemId, at: Hlc, topic_at: Hlc) -> (Item, Vec<Record>) {
        let topic = match &draft.topic {
            Some(topic) => self.resolve(topic),
            None => self.topic_for(&draft.refs),
        };
        let mut records = Vec::new();
        if !self.topics.contains_key(&topic) {
            let key = draft.refs.iter().filter(|r| r.topic_id() == topic).min_by_key(|r| r.rank()).cloned();
            let title = key.as_ref().map_or_else(|| "General".to_string(), Ref::title);
            records.push(Record::Topic(Topic { v: SCHEMA, id: topic.clone(), at: topic_at, title, key, merged_into: None, split_from: None }));
        }
        let item = Item { v: SCHEMA, id, topic, posted: at.clone(), at, author: draft.author, kind: draft.kind, text: draft.text, refs: draft.refs, closed: false };
        records.push(Record::Item(item.clone()));
        (item, records)
    }

    /// The records that import v1's chat lines, stamped at their own times (v1 kept seconds), with
    /// ids from their content: importing again, or the server's copy too, changes nothing. A
    /// line that leads to a session goes to that session's topic. Pure: [`Conversation::import_v1`]
    /// writes them.
    pub fn plan_v1(&self, lines: &[ChatLine], machine: &MachineId) -> Vec<Record> {
        let mut plan = self.clone();
        let mut records = Vec::new();
        // The same line again in the same second is another item; lines of one second keep their
        // order.
        let mut seen: HashMap<(&str, u64, &str), u32> = HashMap::new();
        let mut in_second: HashMap<u64, u32> = HashMap::new();
        for line in lines {
            let who = match line.who {
                Who::You => "you",
                Who::Manager => "manager",
                Who::System => "system",
            };
            let again = seen.entry((who, line.at, line.text.as_str())).or_insert(0);
            let place = in_second.entry(line.at).or_insert(0);
            let at = Hlc { wall_ms: line.at.saturating_mul(1000), count: *place, machine: machine.clone() };
            let hash = hex(&Sha256::digest(format!("v1\0{who}\0{}\0{again}\0{}", line.at, line.text).as_bytes()));
            *again += 1;
            *place += 1;
            let refs: Vec<Ref> = line.show.as_ref().and_then(|show| show.session.clone()).map(Ref::Session).into_iter().collect();
            let draft = Draft { author: Author::V1, kind: Kind::V1 { who: line.who }, text: line.text.clone(), refs, topic: None };
            let (_, planned) = plan.plan_post(draft, ItemId(format!("v1-{}", &hash[..21])), at.clone(), at);
            for record in planned {
                if plan.take(record.clone()) {
                    records.push(record);
                }
            }
        }
        records
    }
}

/// The words of a text, in lower case.
fn words_of(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()).map(str::to_lowercase).collect()
}

// ----- files -----------------------------------------------------------------------------------

/// `manager/conversation/`.
pub fn dir() -> PathBuf {
    crate::paths::root().join("manager").join("conversation")
}

fn file_of(topic: &TopicId) -> PathBuf {
    dir().join(format!("{topic}.jsonl"))
}

/// Where a sync stopped (M57): the newest of this machine's stamps sent, and each topic file's size
/// and modification time when it was last read, so a sync reads only the files that changed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncCursor {
    after: Option<Hlc>,
    seen: BTreeMap<PathBuf, (u64, Option<std::time::SystemTime>)>,
}

/// This machine's own records stamped after `cursor` (at most `max`, oldest first), from the files
/// that changed since it; `cursor` goes on past them. Records taken from the other home keep their
/// stamps, which name that machine, so they never go back. It reads the disk.
pub fn own_changed(cursor: &mut SyncCursor, machine: &MachineId, max: usize) -> Vec<Record> {
    let mut out = Vec::new();
    let mut seen = Vec::new();
    for path in files() {
        let meta = std::fs::metadata(&path).ok();
        let now = meta.as_ref().map(|m| (m.len(), m.modified().ok()));
        if now.is_some() && cursor.seen.get(&path) == now.as_ref() {
            continue;
        }
        let Ok(lines) = read_lines(&path) else { continue };
        out.extend(lines.into_iter().filter_map(|(_, record)| record).filter(|record| &record.at().machine == machine && cursor.after.as_ref().is_none_or(|after| record.at() > after)));
        if let Some(now) = now {
            seen.push((path, now));
        }
    }
    out.sort_by(|a, b| a.at().cmp(b.at()));
    out.dedup_by(|a, b| a == b);
    if out.len() > max {
        // The rest goes next time: the files are read again.
        out.truncate(max);
    } else {
        cursor.seen.extend(seen);
    }
    if let Some(last) = out.last() {
        cursor.after = Some(last.at().clone());
    }
    out
}

/// Take the other home's records (a sync, M57) into this home's conversation files: those new here
/// are written ([`Conversation::take_theirs`]). It reads and writes the disk; with none it does
/// nothing.
pub fn take_from_other_home(records: Vec<Record>) -> std::io::Result<usize> {
    if records.is_empty() {
        return Ok(0);
    }
    Conversation::load().take_theirs(records)
}

/// The topics' files.
fn files() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir()) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "jsonl") && path.file_stem().and_then(|stem| stem.to_str()).and_then(TopicId::parse).is_some())
        .collect();
    files.sort();
    files
}

/// A file's lines, each with its record when it reads as one of this schema.
fn read_lines(path: &Path) -> std::io::Result<Vec<(String, Option<Record>)>> {
    let text = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    Ok(text.lines().filter(|line| !line.trim().is_empty()).map(|line| (line.to_string(), serde_json::from_str::<Record>(line).ok().filter(|record| record.v() <= SCHEMA))).collect())
}

fn lock(path: &Path) -> std::io::Result<crate::sys::FileLock> {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    crate::sys::FileLock::take(Path::new(&name)).map_err(|err| std::io::Error::other(err.to_string()))
}

/// `count` stamps of this machine's clock.
fn stamps(count: usize) -> std::io::Result<Vec<Hlc>> {
    let machine = MachineId::this().map_err(|err| std::io::Error::other(err.to_string()))?;
    crate::identity::next_stamps(&machine, count).map_err(|err| std::io::Error::other(err.to_string()))
}

/// Append records to their topics' files, each file's lines synced before the next
/// ([`crate::words::append_lines`]).
fn write(records: &[Record]) -> std::io::Result<()> {
    let mut files: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for record in records {
        let line = serde_json::to_string(record).map_err(std::io::Error::other)?;
        files.entry(file_of(record.file())).or_default().push(line);
    }
    for (path, lines) in files {
        crate::words::append_lines(&path, &lines)?;
    }
    Ok(())
}

/// Replace a file with `lines` in one step (a temporary file, synced, renamed over it), or remove
/// it when there are none. The caller holds its lock.
fn rewrite(path: &Path, lines: &[String]) -> std::io::Result<()> {
    if lines.is_empty() {
        return match std::fs::remove_file(path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        };
    }
    let folder = path.parent().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a file's path"))?;
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let mut temp = tempfile::Builder::new().prefix(&format!(".{name}.")).suffix(".tmp").tempfile_in(folder)?;
    let mut text = lines.join("\n");
    text.push('\n');
    temp.write_all(text.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|err| err.error)?;
    #[cfg(unix)]
    std::fs::File::open(folder)?.sync_all()?;
    Ok(())
}

impl Conversation {
    /// Read every topic's file: lines that do not read are skipped, and items past retention are
    /// left out. It reads the disk: never on the main thread.
    pub fn load() -> Conversation {
        let mut conversation = Conversation::new(crate::clock::now_ms());
        for path in files() {
            match read_lines(&path) {
                Ok(lines) => {
                    let bad = lines.iter().filter(|(_, record)| record.is_none()).count();
                    if bad > 0 {
                        log::warn!("conversation: {bad} lines of {} could not be read; they are skipped", path.display());
                    }
                    for record in lines.into_iter().filter_map(|(_, record)| record) {
                        conversation.take(record);
                    }
                }
                Err(err) => log::warn!("conversation: {} could not be read ({err})", path.display()),
            }
        }
        conversation
    }

    /// Write records that are new here, then take them in.
    fn commit(&mut self, records: Vec<Record>) -> std::io::Result<usize> {
        let records: Vec<Record> = records.into_iter().filter(|record| self.is_new(record)).collect();
        write(&records)?;
        let count = records.len();
        for record in records {
            self.take(record);
        }
        Ok(count)
    }

    /// Post an item: in the topic given, else in the topic of its references (made when it is new).
    /// Its text is kept with its secrets replaced, as the words ledger keeps it ([`crate::words::scrub`]).
    /// A person's message is posted with the words record the composer made for it; a v1 line or a
    /// rollover link cannot be posted ([`Conversation::import_v1`], [`Conversation::roll_over`]),
    /// and an action line's effect key is 64 hex digits. It writes the disk.
    pub fn post(&mut self, draft: Draft) -> std::io::Result<Item> {
        let invalid = |why: &str| Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, why.to_string()));
        match (&draft.kind, &draft.author) {
            (Kind::V1 { .. }, _) | (_, Author::V1) => return invalid("a v1 line is only imported"),
            (Kind::Rollover { .. }, _) => return invalid("a rollover link is made by roll_over"),
            (Kind::You { .. }, Author::Person) => {}
            (Kind::You { .. }, _) | (_, Author::Person) => return invalid("only the person's own message is the person's"),
            (Kind::Action { effect, .. }, _) if !(effect.len() == 64 && effect.bytes().all(|b| b.is_ascii_hexdigit())) => return invalid("an effect key is 64 hex digits"),
            _ => {}
        }
        if let Some(topic) = &draft.topic
            && *topic != TopicId::general()
            && !self.topics.contains_key(&self.resolve(topic))
        {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("no topic {topic}")));
        }
        let draft = Draft { text: crate::words::scrub(&draft.text), ..draft };
        let [topic_at, at]: [Hlc; 2] = stamps(2)?.try_into().map_err(|_| std::io::Error::other("no stamps"))?;
        let (item, records) = self.plan_post(draft, ItemId::new(), at, topic_at);
        self.commit(records)?;
        Ok(item)
    }

    /// Write a new version of an item, stamped now.
    fn revise(&mut self, id: &ItemId, change: impl FnOnce(&mut Item)) -> std::io::Result<()> {
        let mut item = self.items.get(id).cloned().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, format!("no item {id}")))?;
        change(&mut item);
        item.at = stamps(1)?.remove(0);
        self.commit(vec![Record::Item(item)]).map(drop)
    }

    /// A manager message's text and state, as it streams in and when it ends.
    pub fn finish(&mut self, id: &ItemId, text: &str, state: Stream) -> std::io::Result<()> {
        if !matches!(self.items.get(id).map(|item| &item.kind), Some(Kind::Manager { .. })) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{id} is not a manager message")));
        }
        self.revise(id, |item| {
            item.text = crate::words::scrub(text);
            item.kind = Kind::Manager { state };
        })
    }

    /// A nudge or card needs nothing more.
    pub fn close(&mut self, id: &ItemId) -> std::io::Result<()> {
        self.revise(id, |item| item.closed = true)
    }

    /// A topic's new title.
    pub fn retitle(&mut self, topic: &TopicId, title: &str) -> std::io::Result<()> {
        let have = self.topics.get(topic).cloned().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, format!("no topic {topic}")))?;
        let at = stamps(1)?.remove(0);
        self.commit(vec![Record::Topic(Topic { at, title: title.to_string(), ..have })]).map(drop)
    }

    /// Move `items` into a new topic of the person's own, titled `title`: each is written again
    /// there. A PR or session the items name still leads to its own topic, not the split one.
    pub fn split(&mut self, items: &[ItemId], title: &str) -> std::io::Result<TopicId> {
        let moving: Vec<Item> = items.iter().map(|id| self.items.get(id).cloned().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, format!("no item {id}")))).collect::<Result<_, _>>()?;
        let Some(first) = moving.first() else {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "nothing to split off"));
        };
        let topic = TopicId::split();
        let mut stamps = stamps(moving.len() + 1)?.into_iter();
        let from = self.resolve(&first.topic);
        let mut records =
            vec![Record::Topic(Topic { v: SCHEMA, id: topic.clone(), at: stamps.next().unwrap_or_else(|| first.at.clone()), title: title.to_string(), key: None, merged_into: None, split_from: Some(from) })];
        for (item, at) in moving.into_iter().zip(stamps) {
            records.push(Record::Item(Item { topic: topic.clone(), at, ..item }));
        }
        self.commit(records)?;
        Ok(topic)
    }

    /// Merge topic `from` into `into`: its items and references go there from now on.
    pub fn merge(&mut self, from: &TopicId, into: &TopicId) -> std::io::Result<()> {
        let have = self.topics.get(from).cloned().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, format!("no topic {from}")))?;
        if !self.topics.contains_key(into) {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("no topic {into}")));
        }
        if self.resolve(into) == self.resolve(from) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{from} is already {into}")));
        }
        let at = stamps(1)?.remove(0);
        self.commit(vec![Record::Topic(Topic { at, merged_into: Some(into.clone()), ..have })]).map(drop)
    }

    /// A fresh brain conversation `brain` begins (D8): the brain's summary of the one before, when
    /// there is one, is kept as a summary item (through `through`), and a rollover link ties
    /// `brain` to it. Both last. Gives the link's id.
    pub fn roll_over(&mut self, brain: &str, summary: Option<&str>, through: Option<Hlc>) -> std::io::Result<ItemId> {
        let mut stamps = stamps(4)?.into_iter();
        let mut next = || stamps.next().ok_or_else(|| std::io::Error::other("no stamps"));
        let mut records = Vec::new();
        let summary_id = match summary {
            Some(text) => {
                let draft = Draft { author: Author::Chat, kind: Kind::Summary { through }, text: text.to_string(), refs: Vec::new(), topic: Some(TopicId::general()) };
                let (item, planned) = self.plan_post(draft, ItemId::new(), next()?, next()?);
                records.extend(planned);
                Some(item.id)
            }
            None => None,
        };
        let mut plan = self.clone();
        for record in &records {
            plan.take(record.clone());
        }
        let link = Draft { author: Author::App, kind: Kind::Rollover { brain: brain.to_string(), summary: summary_id }, text: String::new(), refs: Vec::new(), topic: Some(TopicId::general()) };
        let (item, planned) = plan.plan_post(link, ItemId::new(), next()?, next()?);
        records.extend(planned);
        self.commit(records)?;
        Ok(item.id)
    }

    /// Records from the other home (M57): those new here are written to their files and taken in;
    /// a record more than [`crate::identity::MAX_AHEAD_MS`] ahead of this clock is quarantined
    /// (not taken), and an expired item is not taken either. Gives how many were taken.
    pub fn take_theirs(&mut self, records: Vec<Record>) -> std::io::Result<usize> {
        let (ahead, records): (Vec<Record>, Vec<Record>) = records.into_iter().partition(|record| record.at().too_far_ahead());
        if !ahead.is_empty() {
            log::warn!("conversation: {} records from the other home are stamped too far ahead; they are not taken", ahead.len());
            crate::identity::quarantined("conversation", ahead.len(), ahead.first().map(|r| &r.at().machine));
        }
        let mut sorted = records;
        sorted.sort_by(|a, b| a.at().cmp(b.at()));
        let mut plan = self.clone();
        let fresh: Vec<Record> = sorted.into_iter().filter(|record| plan.take(record.clone())).collect();
        self.commit(fresh)
    }

    /// Import v1's chat lines ([`Conversation::plan_v1`]): M44 does it once from `data.json`.
    /// Gives how many records were written.
    pub fn import_v1(&mut self, lines: &[ChatLine], machine: &MachineId) -> std::io::Result<usize> {
        let records = self.plan_v1(lines, machine);
        self.commit(records)
    }

    /// Retention: rewrite each topic's file without the items past [`RETENTION_MS`] and the versions
    /// a newer one replaced. A line that does not read, or is of a
    /// newer schema, stays as it is. Each file is read again under its lock first, so what another
    /// process appended meanwhile is kept. Gives how many lines went.
    pub fn prune(&mut self) -> std::io::Result<usize> {
        self.expire(crate::clock::now_ms());
        let mut dropped = 0;
        for path in files() {
            let _lock = lock(&path)?;
            let lines = read_lines(&path)?;
            for (_, record) in &lines {
                if let Some(record) = record {
                    self.take(record.clone());
                }
            }
            let mut keep = Vec::new();
            let mut kept_once = BTreeSet::new();
            let mut went = 0;
            for (line, record) in lines {
                // The newest version of each, once, in its own file.
                let newest = match &record {
                    None => true,
                    Some(Record::Item(item)) => self.items.get(&item.id).is_some_and(|have| have.at == item.at && have.topic == item.topic) && kept_once.insert(format!("item:{}", item.id)),
                    Some(Record::Topic(topic)) => self.topics.get(&topic.id).is_some_and(|have| have.at == topic.at) && file_of(&topic.id) == path && kept_once.insert(format!("topic:{}", topic.id)),
                };
                if newest {
                    keep.push(line);
                } else {
                    went += 1;
                }
            }
            if went > 0 {
                rewrite(&path, &keep)?;
                dropped += went;
            }
        }
        Ok(dropped)
    }
}

/// A short, safe file-name part of a text.
fn slug(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.chars().map(|c| c.to_ascii_lowercase()) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(max);
    let out = out.trim_end_matches('-');
    if out.is_empty() { "x".to_string() } else { out.to_string() }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::manager::Show;
    use crate::testkit::clock::{DAY, HOUR, VirtualClock, Zone, utc_ms};
    use crate::words::{ComposerKind, Origin};

    /// A scratch home of the test's own with a virtual clock, until dropped.
    struct Home {
        _dir: tempfile::TempDir,
        clock: std::sync::Arc<VirtualClock>,
        _guard: crate::clock::TestClockGuard,
        before: Vec<(&'static str, Option<OsString>)>,
    }

    impl Home {
        fn new() -> Home {
            let dir = tempfile::tempdir().unwrap();
            let names = ["GLASSCODE_HOME", crate::words::MEASURE_VARIABLE];
            let before = names.map(|name| (name, std::env::var_os(name))).to_vec();
            // SAFETY: the core tests run one at a time (`--test-threads=1`).
            unsafe {
                std::env::set_var("GLASSCODE_HOME", dir.path());
                std::env::remove_var(crate::words::MEASURE_VARIABLE);
            }
            crate::words::reset();
            let clock = VirtualClock::new(utc_ms(2026, 11, 20, 9, 0), Zone::utc());
            let guard = clock.install();
            Home { _dir: dir, clock, _guard: guard, before }
        }

        fn v2() -> Home {
            Home::new()
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            crate::words::reset();
            for (name, value) in self.before.drain(..) {
                // SAFETY: as in `new`.
                match value {
                    Some(value) => unsafe { std::env::set_var(name, value) },
                    None => unsafe { std::env::remove_var(name) },
                }
            }
        }
    }

    fn draft(author: Author, kind: Kind, text: &str, refs: &[Ref]) -> Draft {
        Draft { author, kind, text: text.to_string(), refs: refs.to_vec(), topic: None }
    }

    fn manager(text: &str, refs: &[Ref]) -> Draft {
        draft(Author::Chat, Kind::Manager { state: Stream::Done }, text, refs)
    }

    fn you(text: &str, refs: &[Ref]) -> Draft {
        let words = crate::words::record(Origin::Composer(ComposerKind::ManagerChat), text, None, None);
        draft(Author::Person, Kind::You { words, reply_to: None }, text, refs)
    }

    fn nudge(matter: &str, text: &str) -> Draft {
        draft(Author::Wake("w1".into()), Kind::Nudge { matter: matter.into() }, text, &[])
    }

    /// M57: a sync sends this machine's own changes once, never what it took from the other home,
    /// and takes the other home's changes in once.
    #[test]
    fn conversation_changes_both_ways() {
        let home = Home::v2();
        let machine = MachineId::this().unwrap();
        let mut c = Conversation::load();
        let first = c.post(manager("The build is green.", &[])).unwrap();
        let mut cursor = SyncCursor::default();
        let sent: Vec<Record> = own_changed(&mut cursor, &machine, 100);
        assert!(sent.contains(&Record::Item(first.clone())), "the new item goes");
        assert!(sent.iter().all(|r| r.at().machine == machine), "only this machine's own");
        assert!(own_changed(&mut cursor, &machine, 100).is_empty(), "nothing changed since");

        // The other home's change comes in once and never goes back.
        let other = MachineId::parse("00000000000000bb").unwrap();
        home.clock.advance(1_000);
        let theirs = Record::Item(Item { text: "The build is red again.".into(), at: Hlc { wall_ms: crate::clock::now_ms(), count: 0, machine: other }, ..first.clone() });
        assert_eq!(take_from_other_home(vec![theirs.clone()]).unwrap(), 1);
        assert_eq!(take_from_other_home(vec![theirs]).unwrap(), 0, "taken once");
        assert_eq!(Conversation::load().item(&first.id).unwrap().text, "The build is red again.");
        assert!(own_changed(&mut cursor, &machine, 100).is_empty(), "the other home's change is not sent back");

        // New changes here go, in parts when there are many.
        let mut c = Conversation::load();
        let posted: Vec<ItemId> = (0..3).map(|i| c.post(manager(&format!("note {i}"), &[])).unwrap().id).collect();
        let part: Vec<Record> = own_changed(&mut cursor, &machine, 2);
        assert_eq!(part.len(), 2);
        let rest: Vec<Record> = own_changed(&mut cursor, &machine, 2);
        let ids: Vec<ItemId> = part.iter().chain(&rest).filter_map(|r| match r { Record::Item(item) => Some(item.id.clone()), Record::Topic(_) => None }).collect();
        assert_eq!(ids, posted);
        assert!(own_changed(&mut cursor, &machine, 2).is_empty());
    }

    fn texts(items: &[&Item]) -> Vec<String> {
        items.iter().map(|item| item.text.clone()).collect()
    }

    fn project() -> ProjectKey {
        ProjectKey::parse("glasscode-3f9a1c").unwrap()
    }

    #[test]
    fn topic_from_reference() {
        let _home = Home::v2();
        let mut c = Conversation::load();
        let pr = Ref::pr("Acme/App", 42);
        let first = c.post(manager("CI failed on #42.", std::slice::from_ref(&pr))).unwrap();
        assert_eq!(first.topic, pr.topic_id());
        assert!(first.topic.as_str().starts_with("pr-acme-app-42-"), "{}", first.topic);
        assert_eq!(c.topic(&first.topic).unwrap().title, "acme/app#42");
        // The same PR, written in another case, is the same topic on every machine.
        assert_eq!(Ref::pr("acme/app", 42).topic_id(), first.topic);
        // An item about the PR and its session goes to the PR's topic, which gathers the session.
        let session = Ref::Session("0b5e7c1a-session".into());
        let both = c.post(manager("The session is fixing it.", &[session.clone(), pr.clone()])).unwrap();
        assert_eq!(both.topic, first.topic);
        let only_session = c.post(manager("It pushed a fix.", std::slice::from_ref(&session))).unwrap();
        assert_eq!(only_session.topic, first.topic, "the session leads to the topic that gathered it");
        // A project alone has its own topic, even when a PR topic names the project.
        let with_project = c.post(manager("Review asked.", &[pr.clone(), Ref::Project(project())])).unwrap();
        assert_eq!(with_project.topic, first.topic);
        let project_only = c.post(manager("Two PRs are open in glasscode.", &[Ref::Project(project())])).unwrap();
        assert_eq!(project_only.topic, Ref::Project(project()).topic_id());
        assert_ne!(project_only.topic, first.topic);
        // Nothing referred to: the general topic.
        let general = c.post(manager("Good morning.", &[])).unwrap();
        assert_eq!(general.topic, TopicId::general());
        assert_eq!(c.topic(&TopicId::general()).unwrap().title, "General");
        // Another session has a topic of its own.
        let other = c.post(manager("Another session started.", &[Ref::Session("77aa-other".into())])).unwrap();
        assert!(other.topic.as_str().starts_with("session-77aa-other-"), "{}", other.topic);
        // What is on disk reads back the same.
        let again = Conversation::load();
        assert_eq!(again, c);
        assert_eq!(texts(&again.topic_items(&first.topic)), ["CI failed on #42.", "The session is fixing it.", "It pushed a fix.", "Review asked."]);
        assert!(dir().join(format!("{}.jsonl", first.topic)).is_file());
        assert_eq!(crate::paths::class_of(&dir()).class(), Some(crate::paths::PathClass::Ledger));
    }

    #[test]
    fn split_and_merge() {
        let _home = Home::v2();
        let mut c = Conversation::load();
        let pr = Ref::pr("acme/app", 7);
        let a = c.post(manager("Checks are green.", std::slice::from_ref(&pr))).unwrap();
        let b = c.post(manager("Release notes need a line.", std::slice::from_ref(&pr))).unwrap();
        let session = Ref::Session("s-notes".into());
        let d = c.post(manager("The changelog is out of date.", &[pr.clone(), session.clone()])).unwrap();
        assert_eq!(c.topic_for(std::slice::from_ref(&session)), a.topic);
        let notes = c.split(&[b.id.clone(), d.id.clone()], "Release notes").unwrap();
        assert_eq!(texts(&c.topic_items(&notes)), ["Release notes need a line.", "The changelog is out of date."]);
        assert_eq!(texts(&c.topic_items(&a.topic)), ["Checks are green."]);
        assert_eq!(c.topic(&notes).unwrap().split_from.as_ref(), Some(&a.topic));
        // A moved item keeps its place in the conversation.
        assert_eq!(c.item(&b.id).unwrap().posted, b.posted);
        // The PR still leads to its own topic, and the session the moved item named does not lead
        // to the split one.
        assert_eq!(c.topic_for(std::slice::from_ref(&pr)), a.topic);
        assert_ne!(c.topic_for(std::slice::from_ref(&session)), notes);
        assert_eq!(Conversation::load(), c);
        // Merged back, the PR's topic has all three again, and the split topic is not listed.
        c.merge(&notes, &a.topic).unwrap();
        assert_eq!(texts(&c.topic_items(&a.topic)), ["Checks are green.", "Release notes need a line.", "The changelog is out of date."]);
        assert_eq!(c.topic_items(&notes).len(), 3, "a merged topic reads as the one it went into");
        assert!(c.topics().iter().all(|topic| topic.id != notes));
        // A merge the other way would go round: refused.
        assert!(c.merge(&a.topic, &notes).is_err());
        // Posting into the merged topic lands in the one it went into.
        let posted = c.post(Draft { topic: Some(notes.clone()), ..manager("Done.", &[]) }).unwrap();
        assert_eq!(posted.topic, a.topic);
        assert_eq!(Conversation::load(), c);
        // Retitled.
        c.retitle(&a.topic, "Release 2.4").unwrap();
        assert_eq!(Conversation::load().topic(&a.topic).unwrap().title, "Release 2.4");
    }

    #[test]
    fn retention() {
        let home = Home::v2();
        let mut c = Conversation::load();
        let old = c.post(manager("The release branch is release/2.4.", &[])).unwrap();
        c.roll_over("brain-1", Some("Release 2.4 is on release/2.4."), Some(old.at.clone())).unwrap();
        home.clock.advance(TEXT_DAYS * DAY - HOUR);
        let recent = c.post(manager("Still here.", &[])).unwrap();
        assert_eq!(Conversation::load().items().len(), 4, "nothing is past retention yet");
        home.clock.advance(2 * HOUR);
        // Read again, the old item is gone; the summary and the link last.
        let mut later = Conversation::load();
        assert!(later.item(&old.id).is_none());
        assert!(later.item(&recent.id).is_some());
        assert_eq!(later.started_from("brain-1").map(|summary| summary.text.as_str()), Some("Release 2.4 is on release/2.4."));
        // The other home's copy of it is not taken back.
        assert_eq!(later.take_theirs(vec![Record::Item(old.clone())]).unwrap(), 0);
        // Pruning drops it from the files too, and what stays reads the same.
        let lines = || -> usize { files().iter().map(|path| read_lines(path).unwrap().len()).sum() };
        let before = lines();
        assert_eq!(later.prune().unwrap(), 1);
        assert_eq!(lines(), before - 1);
        assert_eq!(Conversation::load(), later);
        assert!(!std::fs::read_to_string(file_of(&TopicId::general())).unwrap().contains("The release branch is release/2.4."));
        // A second prune finds nothing more.
        assert_eq!(later.prune().unwrap(), 0);
    }

    #[test]
    fn v1_lines_have_no_tier() {
        let _home = Home::v2();
        let machine = MachineId::this().unwrap();
        let mut c = Conversation::load();
        // v1 kept seconds.
        let at = utc_ms(2026, 11, 19, 12, 0) / 1000;
        let lines = vec![
            ChatLine { who: Who::You, text: "ship it".into(), at, show: None },
            ChatLine { who: Who::Manager, text: "Shipped.".into(), at: at + 1, show: Some(Show { session: Some("s-1".into()), ..Show::default() }) },
            ChatLine { who: Who::You, text: "ship it".into(), at, show: None },
            ChatLine { who: Who::System, text: "The Mac is asleep.".into(), at: at + 2, show: None },
        ];
        assert_eq!(c.import_v1(&lines, &machine).unwrap(), 6, "four items, the general topic and the session's");
        let items: Vec<Item> = c.items().into_iter().cloned().collect();
        assert_eq!(items.len(), 4, "the same line twice is two items");
        for item in &items {
            assert_eq!(item.author, Author::V1);
            assert!(matches!(item.kind, Kind::V1 { .. }));
            assert_eq!(item.words(), None);
            assert_eq!(item.tier(), Tier::Never);
            assert_eq!(item.quote("ship it"), None);
        }
        assert_eq!(items[0].posted.wall_ms, at * 1000);
        assert_eq!(c.topic_items(&Ref::Session("s-1".into()).topic_id()).len(), 1);
        // Imported again, or from the server's copy of v1: nothing new.
        assert_eq!(c.import_v1(&lines, &machine).unwrap(), 0);
        let mut fresh = Conversation::load();
        assert_eq!(fresh.import_v1(&lines, &machine).unwrap(), 0);
        assert_eq!(fresh, c);
        // A v1 line cannot be posted as new; the person's own message has the composer's tier.
        assert!(c.post(draft(Author::V1, Kind::V1 { who: Who::You }, "ship it", &[])).is_err());
        assert!(c.post(draft(Author::Chat, Kind::You { words: crate::words::record(Origin::Outside, "x", None, None), reply_to: None }, "x", &[])).is_err());
        let mine = c.post(you("ship it now", &[])).unwrap();
        assert_eq!(mine.tier(), Tier::One);
        assert_eq!(mine.quote("ship it"), Some((0, 7)));
        let record = crate::words::find(mine.words().unwrap()).unwrap();
        for item in &items {
            assert_eq!(item.tier_with(|_| Some(record.clone())), Tier::Never, "no words record to look up");
        }
        // A version from the other home that keeps the record's id but changes the text backs
        // nothing.
        let other = MachineId::parse("00000000000000bb").unwrap();
        let later = Hlc { wall_ms: mine.at.wall_ms + 1_000, count: 0, machine: other };
        assert_eq!(c.take_theirs(vec![Record::Item(Item { text: "delete main".into(), at: later, ..mine.clone() })]).unwrap(), 1);
        let changed = c.item(&mine.id).unwrap();
        assert_eq!(changed.text, "delete main");
        assert_eq!(changed.tier(), Tier::Never);
        assert_eq!(changed.quote("ship it"), None);
        // Secrets are kept replaced, in the item as in its record, and the two still agree.
        let token = format!("ghp_{}", "a".repeat(36));
        let secret = c.post(you(&format!("use {token} for it"), &[])).unwrap();
        assert_eq!(secret.text, "use [secret] for it");
        assert_eq!(secret.tier(), Tier::One);
        let said = c.post(manager(&format!("Set GITHUB_TOKEN={token}."), &[])).unwrap();
        assert!(!said.text.contains(&token), "{}", said.text);
    }

    #[test]
    fn search_finds_old_answer() {
        let home = Home::v2();
        let mut c = Conversation::load();
        c.post(you("which branch is the release on?", &[])).unwrap();
        let answer = c.post(manager("The release branch is release/2.4, cut on Monday.", &[Ref::Project(project())])).unwrap();
        for day in 0..40 {
            home.clock.advance(DAY);
            c.post(manager(&format!("Morning brief {day}: two reviews waiting."), &[])).unwrap();
        }
        let c = Conversation::load();
        let hits = c.search("Release BRANCH", None, 5);
        assert_eq!(hits.first().map(|item| &item.id), Some(&answer.id));
        assert_eq!(texts(&hits), ["The release branch is release/2.4, cut on Monday.", "which branch is the release on?"]);
        // Words match from their start; every word must be there; a topic narrows it.
        assert_eq!(c.search("releas mon", None, 5).first().map(|item| &item.id), Some(&answer.id));
        assert!(c.search("release tuesday", None, 5).is_empty());
        assert_eq!(c.search("release", Some(&TopicId::general()), 5).len(), 1);
        assert_eq!(c.search("brief", None, 3).len(), 3);
        assert!(c.search("  ", None, 5).is_empty());
        // Past retention the answer is gone, and the summary that kept it is found instead.
        let mut c = c;
        c.roll_over("brain-2", Some("Release 2.4 lives on release/2.4."), None).unwrap();
        home.clock.advance(TEXT_DAYS * DAY);
        let c = Conversation::load();
        assert_eq!(texts(&c.search("release", None, 5)), ["Release 2.4 lives on release/2.4."]);
    }

    #[test]
    fn bad_lines_are_skipped() {
        let _home = Home::v2();
        let mut c = Conversation::load();
        let good = c.post(manager("Still readable.", &[])).unwrap();
        let path = file_of(&TopicId::general());
        // A record of a newer schema is not read, even when it would parse.
        let newer = serde_json::to_string(&Record::Item(Item { v: SCHEMA + 1, id: ItemId("newer".into()), ..good.clone() })).unwrap();
        let newer = newer.as_str();
        let mut text = std::fs::read_to_string(&path).unwrap();
        text = format!("not json\n{text}{newer}\n{{\"record\":\"item\",\"id\":\"cut");
        std::fs::write(&path, &text).unwrap();
        // A file whose name is no topic is not read.
        std::fs::write(dir().join("..jsonl"), "{}").unwrap();
        let read = Conversation::load();
        assert_eq!(read.items().len(), 1);
        assert_eq!(read.item(&good.id).map(|item| item.text.as_str()), Some("Still readable."));
        // An append after a cut line still reads.
        let mut c = read;
        c.post(manager("After the cut.", &[])).unwrap();
        assert_eq!(Conversation::load().items().len(), 2);
        // Retention leaves the lines it cannot read as they are.
        c.prune().unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("not json") && after.contains(newer));
    }

    #[test]
    fn records_merge_in_any_order() {
        let mac = MachineId::parse("00000000000000aa").unwrap();
        let server = MachineId::parse("00000000000000bb").unwrap();
        let stamp = |machine: &MachineId, wall_ms: u64| Hlc { wall_ms, count: 0, machine: machine.clone() };
        let pr = Ref::pr("acme/app", 1);
        let session = Ref::Session("s-1".into());
        let split = TopicId::parse("split-0123456789ab").unwrap();
        let base = Conversation::default();
        // Each home made the PR's topic and posted in it; the server's item also named the session.
        let (a, mut records) = base.plan_post(manager("from the Mac", std::slice::from_ref(&pr)), ItemId("a".into()), stamp(&mac, 10), stamp(&mac, 9));
        let (b, more) = base.plan_post(manager("from the server", &[pr.clone(), session.clone()]), ItemId("b".into()), stamp(&server, 12), stamp(&server, 11));
        records.extend(more);
        let topic = pr.topic_id();
        let made = match &records[0] {
            Record::Topic(made) => made.clone(),
            _ => unreachable!(),
        };
        // The Mac renamed the topic; the server edited its item twice.
        records.push(Record::Topic(Topic { at: stamp(&mac, 20), title: "Login fix".into(), ..made.clone() }));
        records.push(Record::Item(Item { text: "from the server, edited".into(), at: stamp(&server, 30), ..b.clone() }));
        records.push(Record::Item(Item { text: "from the server, edited again".into(), at: stamp(&server, 31), ..b.clone() }));
        // The Mac split its item off.
        let split_topic = Topic { v: SCHEMA, id: split.clone(), at: stamp(&mac, 40), title: "Moved".into(), key: None, merged_into: None, split_from: Some(topic.clone()) };
        records.push(Record::Topic(split_topic.clone()));
        records.push(Record::Item(Item { topic: split.clone(), at: stamp(&mac, 41), ..a.clone() }));
        // Then each home merged one topic into the other: a circle, which holds for neither.
        records.push(Record::Topic(Topic { at: stamp(&mac, 50), merged_into: Some(topic.clone()), ..split_topic }));
        records.push(Record::Topic(Topic { at: stamp(&server, 51), title: "Login fix".into(), merged_into: Some(split.clone()), ..made }));
        let forward = Conversation::from_records(0, records.clone());
        let mut reversed = records.clone();
        reversed.reverse();
        assert_eq!(Conversation::from_records(0, reversed), forward);
        for turn in 1..records.len() {
            let mut turned = records.clone();
            turned.rotate_left(turn);
            assert_eq!(Conversation::from_records(0, turned), forward, "turned by {turn}");
        }
        // What won: the newest edit, the move, the rename.
        assert_eq!(forward.item(&b.id).unwrap().text, "from the server, edited again");
        assert_eq!(forward.item(&a.id).unwrap().topic, split);
        assert_eq!(forward.topic(&topic).unwrap().title, "Login fix");
        assert_eq!(forward.topics().len(), 2);
        // The session leads to the topic of the item that named it.
        assert_eq!(forward.topic_for(std::slice::from_ref(&session)), topic);
        // Sending what changed after a stamp sends only that; sending everything rebuilds it all.
        let after = stamp(&server, 45);
        let sent = forward.changes_after(Some(&after));
        assert_eq!(sent.len(), 2);
        assert!(sent.iter().all(|record| record.at() > &after));
        assert_eq!(Conversation::from_records(0, forward.changes_after(None)), forward);
    }

    #[test]
    fn reply_binding_and_since_last_spoke() {
        let _home = Home::v2();
        let mut c = Conversation::load();
        c.post(you("anything new?", &[])).unwrap();
        let first = c.post(nudge("pr:acme/app#1", "PR 1 has a review.")).unwrap();
        assert_eq!(c.reply_target(None), Some(first.id.clone()), "one open: a bare reply binds to it");
        let second = c.post(draft(Author::Wake("w2".into()), Kind::Card { pending: "p-9".into() }, "Send the message to Sam?", &[])).unwrap();
        assert_eq!(c.reply_target(None), None, "two open: a bare reply binds to neither");
        assert_eq!(c.reply_target(Some(&second.id)), Some(second.id.clone()));
        let effect = "ab".repeat(32);
        c.post(draft(Author::Wake("w2".into()), Kind::Action { effect: effect.clone(), event: Some("e-1".into()) }, "Pushed the fix to PR 1.", &[])).unwrap();
        // The person answers; the chat turn that answers them is told what wakes posted since the
        // chat lane last spoke, with their ids.
        let reply = c.post(you("do the second one", &[])).unwrap();
        assert_eq!(texts(&c.since_last_spoke()), ["PR 1 has a review.", "Send the message to Sam?", "Pushed the fix to PR 1."]);
        c.close(&first.id).unwrap();
        assert_eq!(c.reply_target(None), Some(second.id.clone()));
        c.post(manager("Sending it.", &[])).unwrap();
        assert!(c.since_last_spoke().is_empty(), "the chat lane spoke after them");
        c.post(draft(Author::App, Kind::Error, "The send failed.", &[])).unwrap();
        assert_eq!(texts(&c.since_last_spoke()), ["The send failed."]);
        assert!(c.post(draft(Author::App, Kind::Action { effect: "nothex".into(), event: None }, "x", &[])).is_err());
        // A manager message streams, then ends.
        let streaming = c.post(draft(Author::Chat, Kind::Manager { state: Stream::Streaming }, "Look", &[])).unwrap();
        c.finish(&streaming.id, "Looking at it now.", Stream::Done).unwrap();
        assert!(c.finish(&reply.id, "no", Stream::Done).is_err());
        let read = Conversation::load();
        assert_eq!(read.item(&streaming.id).map(|item| (item.text.as_str(), item.kind.clone())), Some(("Looking at it now.", Kind::Manager { state: Stream::Done })));
        assert!(read.item(&first.id).unwrap().closed);
        assert_eq!(read, c);
    }

    #[test]
    fn rollover_links_and_when_it_is_due() {
        let home = Home::v2();
        let mut c = Conversation::load();
        let first = c.roll_over("brain-1", None, None).unwrap();
        assert_eq!(c.started_from("brain-1"), None, "the first began from nothing");
        assert_eq!(c.last_rollover().map(|item| &item.id), Some(&first));
        for n in 0..3 {
            c.post(manager(&format!("answer {n}"), &[])).unwrap();
        }
        assert_eq!(c.items_since_rollover(), 3);
        c.roll_over("brain-2", Some("Three answers."), None).unwrap();
        assert_eq!(c.started_from("brain-2").map(|item| item.text.as_str()), Some("Three answers."));
        assert_eq!(c.items_since_rollover(), 0);
        let now = home.clock.now();
        assert_eq!(rollover_due(now, now, now + HOUR, 3), None);
        assert_eq!(rollover_due(now, now, now + IDLE_ROLLOVER_MS, 3), Some(RollReason::Idle));
        assert_eq!(rollover_due(now, now + HOUR, now + 2 * HOUR, ROLLOVER_ITEMS), Some(RollReason::Size));
        assert_eq!(rollover_due(now, now + 14 * HOUR, now + 15 * HOUR + 1, 3), Some(RollReason::NewDay), "09:00 to past midnight");
        assert_eq!(Conversation::load(), c);
    }

}
