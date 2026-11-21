//! The gate (design D2, D3, D10, D15, D17 and sections 4 and 8): one pure Rust function,
//! [`decide`], that every op of every caller but the person passes before it runs.
//!
//! The order of precedence is: a hard limit (it binds the person too), then `never` (the
//! project's list, the account a project allows, a path it keeps out of reach, a pause), then the
//! level's ceiling, then an explicit ask (the project's list and its conditions), then the request
//! (a chat grant, D3), then the level's default ([`defaults::TABLE`]).
//!
//! The rule is per project, and the strictest of the projects an op is about wins (trust-3): the
//! caller's own project, taken from Glasscode's own record of the session and never from the
//! op's words, and the project of every target. An op that touches no project reads the policy's
//! `default`, as does a project the policy does not name.
//!
//! [`decide`] does no IO: it reads only the [`PolicySet`] it is given, which the app builds off the
//! main thread from the approved snapshot (`authority`), never from the live file, and from what
//! it knows of its sessions. It never calls [`crate::paths::class_of`].

mod authority;
mod conditions;
mod defaults;
mod grant;
mod levels;
mod pending;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub use authority::{ApprovedPolicy, Authority, Devices, ForgeStore, FromMac, forge_line, MAX_COPY_AGE_MS, PairedDevice, PolicyFile, SessionOrigins, Started, approve, diff, effective, import_live, load, device_id, mark_session, pin_device, pin_phone, policy_header, set_pause, unpin_device, with_policy_header};
pub use conditions::{Condition, Verdict, glob_match};
pub use defaults::{Base, Cell, Effects, Row, TABLE, all_groups, row_of};
pub use grant::{ChatGrants, Proposal, Refused, SessionGrants, WAKE_GAP_MS, targets_hash, yes_binds};
pub use pending::{ANSWER_WAIT_MS, KEPT_MS, Pending, PendingAction, PendingState, WAIT_MS, add_pending, new_id as pending_id, settle_pending};
pub use levels::{Column, Level, stricter, strictness};

pub use crate::control::Rule;
use crate::caller::{Caller, Lane};
use crate::control::AgentSettings;
use crate::ops::{EffectClass, Op, OpGroup, Targets};
pub use crate::paths::ProjectKey;
use crate::words::WordsId;

// ----- policy ----------------------------------------------------------------------------------

/// One project's policy, as the person approved it in the app.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectPolicy {
    pub level: Level,
    /// Ops (by name, `session.new`) and groups (by key, `session_start`) that always ask.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ask: Vec<String>,
    /// Ops and groups that never run for anyone but the person.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub never: Vec<String>,
    /// Ops and groups the person allowed always in this project ("Always allow this in <project>",
    /// M21.2): an ask of the level's default becomes allow, up to the ceiling. A card that always
    /// shows (what leaves the Mac) stays one.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    /// Commands the project's sessions may run without a prompt (M22 compiles them into the
    /// agents' own allow rules).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allow_commands: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// The one account whose model may read the project (D17); None: any account of the person.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_account: Option<String>,
    /// "Stays on this Mac" (ux-m2): nothing of it goes to the server.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub local_only: bool,
    /// On Autopilot, the manager picks work by itself (M60): from where, and how much.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intake: Option<crate::intake::Intake>,
    /// The sites a session the person does not drive may open (M23, D28): it gets a browser with
    /// no sign-ins (a throwaway context) that reaches only these hosts and their subdomains. Empty:
    /// such a session has no browser.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub browser_hosts: Vec<String>,
    /// On Autopilot, the questions of its sessions the manager may answer by itself (M61); any
    /// other answer of the manager's is a card.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub delegate: Vec<crate::delegation::Delegation>,
}

impl ProjectPolicy {
    /// What Careful leaves of this policy: the level becomes Careful, and allow-commands go;
    /// every restriction stays (`never`, `ask`, conditions, the model account, "stays on this
    /// Mac"). This is the policy of a project while the live file differs from the approved
    /// snapshot.
    pub fn at_most_careful(&self) -> ProjectPolicy {
        ProjectPolicy { level: Level::Careful, allow_commands: Vec::new(), allow: Vec::new(), browser_hosts: Vec::new(), ..self.clone() }
    }

    /// Whether `list` names the op or its group.
    fn names(list: &[String], op: &Op) -> bool {
        let group = group_key(op.spec.group);
        list.iter().any(|entry| entry == op.name() || entry == group)
    }

    /// The entries of `ask` and `never` that name neither an op of the registry nor a group: a
    /// policy that has one is not approved (ops-12).
    pub fn unknown_names(&self) -> Vec<String> {
        self.ask.iter().chain(&self.never).chain(&self.allow).filter(|entry| crate::ops::find(entry).is_none() && !all_groups().iter().any(|g| group_key(*g) == entry.as_str())).cloned().collect()
    }
}

/// The policy of every project, and the default for the others.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Policy {
    /// The policy of a project it does not name, and of ops that touch no project (Careful
    /// unless the person chose another).
    pub default: ProjectPolicy,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub projects: BTreeMap<ProjectKey, ProjectPolicy>,
}

impl Policy {
    pub fn of(&self, key: &ProjectKey) -> &ProjectPolicy {
        self.projects.get(key).unwrap_or(&self.default)
    }

    /// Every project at most Careful ([`ProjectPolicy::at_most_careful`]).
    pub fn at_most_careful(&self) -> Policy {
        Policy { default: self.default.at_most_careful(), projects: self.projects.iter().map(|(k, p)| (k.clone(), p.at_most_careful())).collect() }
    }

    /// What the server gets (crit-5): every project but those that stay on this Mac.
    pub fn for_server(&self) -> Policy {
        Policy { default: self.default.clone(), projects: self.projects.iter().filter(|(_, p)| !p.local_only).map(|(k, p)| (k.clone(), p.clone())).collect() }
    }

    pub fn unknown_names(&self) -> Vec<String> {
        std::iter::once(&self.default).chain(self.projects.values()).flat_map(ProjectPolicy::unknown_names).collect()
    }

    /// The hash of the approved content (`ApprovedPolicy.hash`).
    pub fn hash(&self) -> String {
        hex(&sha256_json(&serde_json::to_value(self).unwrap_or(Value::Null)))
    }
}

/// Whether "Always allow this in <project>" can lift this op's ask there for this caller: an ask
/// of the level's default that a grant could lift, never a card that always shows.
pub fn can_always_allow(caller: &Caller, op: &Op, policy: &PolicySet, key: &ProjectKey) -> bool {
    if op.spec.human_only || matches!(op.spec.effect, EffectClass::Outward | EffectClass::Destructive) {
        return false;
    }
    let level = policy.policy.of(key).level;
    match caller {
        Caller::Manager { .. } | Caller::Session { .. } => row_of(op.spec).is_some_and(|row| {
            let column = if matches!(caller, Caller::Manager { .. }) { Column::Manager } else { Column::Automation };
            let cell = row.cell(level, column);
            cell.ceiling == Rule::Allow
        }),
        _ => false,
    }
}

/// The policy with `op` allowed always in `key`'s project: what the preview shows and the person
/// confirms a second time.
pub fn with_always_allowed(policy: &Policy, key: &ProjectKey, op: &str) -> Policy {
    let mut out = policy.clone();
    let entry = out.projects.entry(key.clone()).or_insert_with(|| policy.default.clone());
    if !entry.allow.iter().any(|a| a == op) {
        entry.allow.push(op.to_string());
    }
    out
}

/// Who may point a session at their text (D27, trust-8): the person (any of their own logins), a
/// teammate the person confirmed in the app, or a GitHub author whose association with the
/// repository is OWNER, MEMBER or COLLABORATOR. Anyone else's text (a stranger's review on a
/// public repository, a fork's PR) is untrusted: a notification, or a quarantined session.
pub fn trusted_author(login: &str, association: Option<&str>, person: &[String], confirmed: &[String]) -> bool {
    let same = |other: &String| other.eq_ignore_ascii_case(login.trim());
    if login.trim().is_empty() {
        return false;
    }
    if person.iter().any(same) || confirmed.iter().any(same) {
        return true;
    }
    matches!(association.map(str::to_ascii_uppercase).as_deref(), Some("OWNER" | "MEMBER" | "COLLABORATOR"))
}

/// An op group's key in a policy: `session_start`.
pub fn group_key(group: OpGroup) -> &'static str {
    match group {
        OpGroup::Read => "read",
        OpGroup::Navigate => "navigate",
        OpGroup::Board => "board",
        OpGroup::Memory => "memory",
        OpGroup::SessionStart => "session_start",
        OpGroup::SessionInput => "session_input",
        OpGroup::SessionControl => "session_control",
        OpGroup::Worktree => "worktree",
        OpGroup::Project => "project",
        OpGroup::Settings => "settings",
        OpGroup::Tickets => "tickets",
        OpGroup::ForgeRead => "forge_read",
        OpGroup::ForgeWrite => "forge_write",
        OpGroup::Message => "message",
        OpGroup::Browser => "browser",
        OpGroup::Manager => "manager",
    }
}

// ----- what the gate knows -----------------------------------------------------------------------

/// Where a session's instructions come from (M22 gives each its own profile).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOrigin {
    /// The person started it and drives it.
    Person,
    /// A playbook or the manager started it, or sent it text: for its whole life. A session the
    /// gate knows nothing of counts as one.
    #[default]
    Automation,
    /// Sent to untrusted input (D27).
    Quarantine,
}

/// What the app knows of a session, from its own records (never from what a request says).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionFacts {
    pub project: Option<ProjectKey>,
    pub origin: SessionOrigin,
    /// Who started it: None for the person, "manager", "playbook:<id>", or a session's id.
    pub started_by: Option<String>,
    pub branch: Option<String>,
    /// Its permission mode now (`default`, `auto`, `plan`, …).
    pub mode: Option<String>,
    /// The account it runs on.
    pub account: Option<String>,
}

/// What the forge enforces for a project's default branch (D29), as read at project setup and
/// during moving in (M23).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ForgeFacts {
    pub review_required: bool,
    pub no_force_push: bool,
    /// The person cannot bypass the rules either.
    pub no_bypass: bool,
    pub read_ms: u64,
}

impl ForgeFacts {
    /// Whether the forge carries the wall: all three hold.
    pub fn holds(&self) -> bool {
        self.review_required && self.no_force_push && self.no_bypass
    }
}

/// A pause of the manager (D15): of everything, or of some projects. Any caller may pause; only
/// the person releases.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pause {
    pub all: bool,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub projects: BTreeSet<ProjectKey>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
    /// Who paused it, in words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

impl Pause {
    pub fn everything() -> Pause {
        Pause { all: true, ..Pause::default() }
    }

    pub fn is_paused(&self, key: Option<&ProjectKey>) -> bool {
        self.all || key.is_some_and(|k| self.projects.contains(k))
    }

    /// The stricter of two pauses: paused wherever either is (crit-5: the one value that wins in
    /// both directions between the Mac and the server).
    pub fn stricter(&self, other: &Pause) -> Pause {
        Pause { all: self.all || other.all, projects: self.projects.union(&other.projects).cloned().collect(), since_ms: self.since_ms.or(other.since_ms), by: self.by.clone().or_else(|| other.by.clone()) }
    }
}

/// Everything [`decide`] reads. The app builds it off the main thread and keeps it in memory: the
/// approved policy (from the snapshot, made stricter when the live file differs), the person's
/// rules of Settings › Permissions, the pause, the phones this Mac paired, and what it knows of
/// sessions, accounts, the forge and the projects' modes.
#[derive(Clone, Debug, Default)]
pub struct PolicySet {
    /// The effective policy ([`effective`]).
    pub policy: Policy,
    /// The live `authority/policy.json` differs from the approved snapshot (or cannot be read):
    /// the policy is at most Careful, and the app offers the import screen.
    pub live_differs: bool,
    /// The 13 kinds of `state.json` (exec-7): the rules of person-started sessions, and of
    /// programs outside Glasscode through [`crate::caller::outside_rule`].
    pub person_rules: AgentSettings,
    pub pause: Pause,
    /// The phones this Mac paired itself (`authority/devices.json`, trust-m5), by device id.
    pub devices: BTreeSet<String>,
    pub sessions: BTreeMap<String, SessionFacts>,
    /// The projects hosted on the server: a server session the app has no record of is taken to
    /// be in the strictest of them (trust-3).
    pub server_projects: Vec<ProjectKey>,
    /// The account the manager's brain runs on (D17).
    pub manager_account: Option<String>,
    pub forge: BTreeMap<ProjectKey, ForgeFacts>,
    /// Each project's own default mode for new sessions: the most a non-person caller may ask for.
    pub project_modes: BTreeMap<ProjectKey, String>,
    /// The person's home folder, for `~/` in path conditions.
    pub home: PathBuf,
}

impl PolicySet {
    /// What the gate reads when nothing could be read: every project Careful, the manager paused.
    pub fn fail_closed() -> PolicySet {
        PolicySet { live_differs: true, pause: Pause::everything(), ..PolicySet::default() }
    }

    /// D17: whether the manager's brain may read `key`. A project bound to one account
    /// (`model_account`) only when that is the manager's account (`manager_account`).
    pub fn manager_may_read(&self, key: &ProjectKey) -> bool {
        self.account_may_read(key, self.manager_account.as_deref())
    }

    /// D17: whether a model on `account` may read `key` (a headless run of Glasscode's own, such as
    /// moving in's study): a project bound to one account only on that account.
    pub fn account_may_read(&self, key: &ProjectKey, account: Option<&str>) -> bool {
        self.policy.of(key).model_account.as_deref().is_none_or(|only| account == Some(only))
    }

    /// Of `keys`, the projects the manager's brain may not read (D17): what its reads with no
    /// project named (`memory.search`, `memory.show`, `journal.events`, the manager's brief) leave
    /// out, as the gate refuses them when named.
    pub fn hidden_from_manager<'a>(&self, keys: impl IntoIterator<Item = &'a ProjectKey>) -> Vec<ProjectKey> {
        let mut hidden: Vec<ProjectKey> = keys.into_iter().filter(|key| !self.manager_may_read(key)).cloned().collect();
        hidden.sort();
        hidden.dedup();
        hidden
    }
}

// ----- decisions and grants ----------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Ask { why: String },
    Never { why: String },
}

impl Decision {
    pub fn rule(&self) -> Rule {
        match self {
            Decision::Allow => Rule::Allow,
            Decision::Ask { .. } => Rule::Ask,
            Decision::Never { .. } => Rule::Never,
        }
    }

    pub fn why(&self) -> &str {
        match self {
            Decision::Allow => "",
            Decision::Ask { why } | Decision::Never { why } => why,
        }
    }

    fn ask(why: impl Into<String>) -> Decision {
        Decision::Ask { why: why.into() }
    }

    fn never(why: impl Into<String>) -> Decision {
        Decision::Never { why: why.into() }
    }
}

/// A chat grant (D3): the person's own words in chat allow one op. M21.2 makes one only after
/// Rust checked that the quote is inside a tier-one message of this conversation, that the target
/// ids were resolved from that message, and that it is used once; [`decide`] checks that it is
/// this op, with these arguments, on targets it names, from the chat lane, within the ceiling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    pub message: WordsId,
    pub quote: (usize, usize),
    /// [`op_hash`] of the op it allows.
    pub op_hash: [u8; 32],
    pub targets: Targets,
    /// The chat turn it ends with.
    pub turn: String,
}

impl Grant {
    /// Whether it covers this op from this caller: the chat lane, the same op and arguments, and
    /// only targets it names.
    pub fn covers(&self, caller: &Caller, op: &Op, targets: &Targets) -> bool {
        matches!(caller, Caller::Manager { lane: Lane::Chat, .. }) && self.op_hash == op_hash(op) && within(targets, &self.targets)
    }
}

/// Whether every target of `inner` is among those of `outer`.
fn within(inner: &Targets, outer: &Targets) -> bool {
    inner.projects.iter().all(|p| outer.projects.contains(p))
        && inner.sessions.iter().all(|s| outer.sessions.contains(s))
        && inner.repos.iter().all(|r| outer.repos.contains(r))
        && inner.paths.iter().all(|p| outer.paths.contains(p))
        && (!inner.leaves_machine || outer.leaves_machine)
}

/// The hash an approval and a grant are bound to: the op's name and its arguments, as JSON with
/// every object's keys in order.
pub fn op_hash(op: &Op) -> [u8; 32] {
    sha256_json(&serde_json::json!({ "op": op.name(), "args": op.args }))
}

fn sha256_json(value: &Value) -> [u8; 32] {
    fn canonical(value: &Value, out: &mut String) {
        match value {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, key) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String(key.clone()).to_string());
                    out.push(':');
                    canonical(&map[key], out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    canonical(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut text = String::new();
    canonical(value, &mut text);
    Sha256::digest(text.as_bytes()).into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ----- decide ------------------------------------------------------------------------------------

/// Which rules a caller that is not the person reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Manager,
    /// A session a playbook or the manager started or drives, or one the gate knows nothing of.
    Automation,
    /// A session the person started: Settings › Permissions, as in v1.
    PersonSession,
    /// A program outside Glasscode: it reads, and the rest asks (O13).
    Terminal,
}

impl Class {
    fn who(self) -> &'static str {
        match self {
            Class::Manager => "the manager",
            Class::Automation => "a session the manager drives",
            Class::PersonSession => "a session",
            Class::Terminal => "a program outside Glasscode",
        }
    }
}

/// The ops a paired phone may confirm itself, signed over the op's argument hash (trust-12): an
/// approval's answer. Every other person-only op asks in the app.
const PHONE_CONFIRMS: &[&str] = &["approvals.decide"];

/// The one op a paused manager may still run.
const PAUSE_OP: &str = "manager.pause";

/// Whether an op only looks.
fn reads(op: &Op) -> bool {
    matches!(op.spec.effect, EffectClass::Read | EffectClass::Navigate)
}

/// The decision for `op` from `caller`, on `targets`, under `policy`, with the chat grant the
/// caller offers. Pure: no IO, no clock but [`crate::clock`].
pub fn decide(caller: &Caller, op: &Op, targets: &Targets, policy: &PolicySet, grant: Option<&Grant>) -> Decision {
    if let Some(why) = hard_limit(op, targets, policy) {
        return Decision::never(why);
    }
    let class = match caller {
        // The person, in the app's window; and another Glasscode machine's sync connection, whose
        // own gate decided (authority flows from the Mac, D5).
        Caller::Ui | Caller::Machine { .. } => return Decision::Allow,
        // The person typing in a Glasscode shell tab (O13): full rights, but an op that needs the
        // person present asks in the app, as a program in that tab could fire inside the window.
        Caller::ShellTab { .. } => {
            return if op.spec.human_only { Decision::ask(format!("only you, in the app, can {}: confirm it there", op.spec.summary)) } else { Decision::Allow };
        }
        Caller::Phone { device } => return phone(device, op, policy),
        Caller::Terminal { .. } => Class::Terminal,
        Caller::Manager { .. } => Class::Manager,
        Caller::Session { id, .. } => match policy.sessions.get(id) {
            Some(facts) if facts.origin == SessionOrigin::Person => Class::PersonSession,
            _ => Class::Automation,
        },
    };
    if op.spec.human_only {
        return Decision::never(format!("only you can {}, in the app", only_you(op.spec.summary)));
    }
    let places = places(caller, targets, policy);
    if matches!(class, Class::Manager | Class::Automation) && !reads(op) && op.name() != PAUSE_OP && places.iter().any(|place| policy.pause.is_paused(place.key)) {
        return Decision::never("the manager is paused; only you can release it");
    }
    if let Some(why) = mode_raised(class, op, targets, policy, &places) {
        return Decision::never(why);
    }
    let branches = branches(caller, targets, policy);
    let local_minutes = crate::clock::local_minutes(crate::clock::now_ms());
    let mut out = Decision::Allow;
    for place in &places {
        let decision = decide_in(class, caller, op, targets, policy, grant, place, &branches, local_minutes);
        if strictness(decision.rule()) > strictness(out.rule()) {
            out = decision;
        }
    }
    out
}

/// A project an op is about, or (key None) none: the policy's default.
#[derive(Clone, Debug)]
struct Place<'a> {
    key: Option<&'a ProjectKey>,
    /// The caller's own project.
    own: bool,
}

/// The projects an op is about: the caller's own (from the app's record), every target project,
/// and the project of every target session. None of them: the policy's default.
fn places<'a>(caller: &'a Caller, targets: &'a Targets, policy: &'a PolicySet) -> Vec<Place<'a>> {
    let mut out: Vec<Place<'a>> = Vec::new();
    let mut add = |key: &'a ProjectKey, own: bool| match out.iter_mut().find(|p| p.key == Some(key)) {
        Some(place) => place.own |= own,
        None => out.push(Place { key: Some(key), own }),
    };
    match caller {
        Caller::Session { id, .. } => match policy.sessions.get(id).map(|facts| facts.project.as_ref()) {
            Some(Some(key)) => add(key, true),
            Some(None) => {}
            // A session the app has no record of: a server session is in one of the server's
            // projects, any other in any project; the strictest of them decides.
            None if id.starts_with("server:") => policy.server_projects.iter().for_each(|key| add(key, true)),
            None => policy.policy.projects.keys().for_each(|key| add(key, false)),
        },
        Caller::Manager { project: Some(key), .. } => add(key, true),
        _ => {}
    }
    for key in &targets.projects {
        add(key, false);
    }
    for id in &targets.sessions {
        match policy.sessions.get(id) {
            Some(SessionFacts { project: Some(key), .. }) => add(key, false),
            Some(_) => {}
            None if id.starts_with("server:") => policy.server_projects.iter().for_each(|key| add(key, false)),
            None => {}
        }
    }
    if out.is_empty() {
        out.push(Place { key: None, own: true });
    }
    out
}

/// The branches an op is about: those of its target sessions.
fn branches(_caller: &Caller, targets: &Targets, policy: &PolicySet) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in &targets.sessions {
        if let Some(branch) = policy.sessions.get(id).and_then(|facts| facts.branch.clone())
            && !out.contains(&branch)
        {
            out.push(branch);
        }
    }
    out
}

/// What binds everyone, the person too: a project that stays on this Mac never goes to the
/// server, neither its sessions nor its worktrees.
fn hard_limit(op: &Op, targets: &Targets, policy: &PolicySet) -> Option<String> {
    let flag = |name: &str| op.args.get(name).and_then(Value::as_bool).unwrap_or(false);
    let to_server = match op.name() {
        "session.move" => !flag("to_this_computer"),
        "switch.move" | "worktree.move" => op.args.get("to").and_then(Value::as_str).is_some_and(|to| to.eq_ignore_ascii_case("server")),
        _ => flag("on_server"),
    };
    if !to_server {
        return None;
    }
    let mut keys: Vec<&ProjectKey> = targets.projects.iter().collect();
    keys.extend(targets.sessions.iter().filter_map(|id| policy.sessions.get(id)).filter_map(|facts| facts.project.as_ref()));
    keys.into_iter().find(|key| policy.policy.of(key).local_only).map(|key| format!("{key} stays on this Mac: nothing of it goes to the server"))
}

/// A phone's decision: only a phone this Mac paired itself; ordinary ops as the person, and an
/// op that needs the person present asks in the app unless the phone may confirm it itself.
fn phone(device: &str, op: &Op, policy: &PolicySet) -> Decision {
    if !policy.devices.contains(device) {
        return Decision::never("this phone is not paired with this Mac");
    }
    if op.spec.human_only && !PHONE_CONFIRMS.contains(&op.name()) {
        return Decision::ask(format!("only you, in the app, can {}: confirm it there", op.spec.summary));
    }
    Decision::Allow
}

/// How far a permission mode lets an agent go without asking: plan and read-only 0, the default
/// 1, accepting edits 2, auto (Codex full-auto) 3, bypass 4. None: a mode Glasscode does not know.
pub fn mode_rank(mode: &str) -> Option<u8> {
    match mode.to_lowercase().replace(['-', '_', ' '], "").as_str() {
        "plan" | "readonly" => Some(0),
        "default" | "ask" | "normal" => Some(1),
        "acceptedits" | "edits" => Some(2),
        "auto" | "fullauto" => Some(3),
        "bypasspermissions" | "bypass" | "yolo" | "dangerfullaccess" => Some(4),
        _ => None,
    }
}

/// A mode the op asks for that is above what a caller other than the person may give (ops-6):
/// for a new session, above the default and the project's own default mode; for a session that
/// runs, above its mode now; bypass never.
fn mode_raised(class: Class, op: &Op, targets: &Targets, policy: &PolicySet, places: &[Place]) -> Option<String> {
    let wanted = op.args.get("mode").and_then(Value::as_str)?;
    let who = class.who();
    let Some(rank) = mode_rank(wanted) else { return Some(format!("“{wanted}” is not a mode Glasscode knows, so {who} cannot ask for it")) };
    let limit = if op.name() == "session.mode" {
        targets.sessions.iter().map(|id| policy.sessions.get(id).and_then(|f| f.mode.as_deref()).and_then(mode_rank).unwrap_or(1)).min().unwrap_or(1)
    } else {
        places.iter().map(|place| place.key.and_then(|key| policy.project_modes.get(key)).and_then(|mode| mode_rank(mode)).unwrap_or(1).max(1)).min().unwrap_or(1)
    };
    (rank > limit.min(3)).then(|| format!("{who} may not raise a session's mode to {wanted}: only you can"))
}

/// [`decide_place`], and D17 for a program outside Glasscode: it runs on no account Glasscode
/// knows, and any session can be one (it connects to the control socket without its signature),
/// so what it would be allowed in a project kept to one account asks, and the person answers.
#[allow(clippy::too_many_arguments)]
fn decide_in(class: Class, caller: &Caller, op: &Op, targets: &Targets, set: &PolicySet, grant: Option<&Grant>, place: &Place, branches: &[String], local_minutes: Option<i64>) -> Decision {
    let decision = decide_place(class, caller, op, targets, set, grant, place, branches, local_minutes);
    let policy = place.key.map(|key| set.policy.of(key)).unwrap_or(&set.policy.default);
    match (&decision, class, &policy.model_account) {
        (Decision::Allow, Class::Terminal, Some(account)) => {
            let at = place.key.map(|key| key.as_str()).unwrap_or("Glasscode");
            Decision::ask(format!("only the account {account} may read {at}: a program outside Glasscode asks you first"))
        }
        _ => decision,
    }
}

#[allow(clippy::too_many_arguments)]
fn decide_place(class: Class, caller: &Caller, op: &Op, targets: &Targets, set: &PolicySet, grant: Option<&Grant>, place: &Place, branches: &[String], local_minutes: Option<i64>) -> Decision {
    let policy = place.key.map(|key| set.policy.of(key)).unwrap_or(&set.policy.default);
    let at = place.key.map(|key| key.as_str()).unwrap_or("Glasscode");
    let who = class.who();
    let looks = reads(op);
    // Never: the project's list.
    if ProjectPolicy::names(&policy.never, op) {
        return Decision::never(format!("{at}'s policy never lets {who} run {}", op.name()));
    }
    // Never: the one account whose model may read the project (D17), for the manager and for
    // every session, in its own project too (a session on another account there reads nothing of
    // it through Glasscode: not its memory, not its sessions).
    if let Some(account) = &policy.model_account {
        let own_account = match class {
            Class::Manager => set.manager_account.as_deref(),
            Class::Automation | Class::PersonSession => match caller {
                Caller::Session { id, .. } => set.sessions.get(id).and_then(|f| f.account.as_deref()),
                _ => None,
            },
            // A program outside Glasscode asks instead ([`decide_in`]).
            Class::Terminal => Some(account.as_str()),
        };
        if own_account != Some(account.as_str()) {
            return Decision::never(format!("only the account {account} may read {at}"));
        }
    }
    // A project that stays on this Mac is its own sessions' only (ops-m4).
    if policy.local_only && !place.own && class != Class::Manager {
        let why = format!("{at} stays on this Mac and is for its own sessions only");
        return if class == Class::Terminal { Decision::ask(why) } else { Decision::never(why) };
    }
    let facts = conditions::Facts { branches, paths: &targets.paths, local_minutes, home: &set.home };
    if !looks {
        for condition in &policy.conditions {
            if let Verdict::Never(why) = condition.check(&facts) {
                return Decision::never(why);
            }
        }
    }
    // The cell: the level's for the manager and the sessions it drives, the person's own rules for
    // the sessions they started, today's outside rule for other programs.
    let (base, ceiling, what) = match class {
        // In its own project, a session the person started keeps their rules. Reaching into
        // another project, it is an agent acting there: the stricter of their rules and what that
        // project's level lets the manager do (trust-3).
        Class::PersonSession => {
            let rule = set.person_rules.rule(op.spec.kind_v1);
            let there = match (place.own, row_of(op.spec)) {
                (false, Some(row)) => match row.cell(policy.level, Column::Manager).base {
                    Base::Rule(rule) => rule,
                    Base::StartedByCaller | Base::ForgeWall => Rule::Ask,
                },
                _ => Rule::Allow,
            };
            let rule = stricter(rule, there);
            (rule, rule, op.spec.summary)
        }
        Class::Terminal => {
            let rule = crate::caller::outside_rule(op.spec.kind_v1, &set.person_rules);
            (rule, rule, op.spec.summary)
        }
        Class::Manager | Class::Automation => {
            let column = if class == Class::Manager { Column::Manager } else { Column::Automation };
            let Some(row) = row_of(op.spec) else { return Decision::never(format!("{} has no row in the gate's table", op.name())) };
            let cell = row.cell(policy.level, column);
            let base = match cell.base {
                Base::Rule(rule) => rule,
                Base::StartedByCaller => {
                    let starter = match caller {
                        Caller::Manager { .. } => Some("manager".to_string()),
                        Caller::Session { id, .. } => Some(id.clone()),
                        _ => None,
                    };
                    let all_its_own = !targets.sessions.is_empty() && targets.sessions.iter().all(|id| set.sessions.get(id).is_some_and(|f| f.started_by.is_some() && f.started_by == starter));
                    if all_its_own { Rule::Allow } else { Rule::Ask }
                }
                Base::ForgeWall => {
                    if place.key.and_then(|key| set.forge.get(key)).is_some_and(ForgeFacts::holds) {
                        Rule::Allow
                    } else {
                        Rule::Ask
                    }
                }
            };
            (base, cell.ceiling, row.what)
        }
    };
    let level = policy.level.label();
    if ceiling == Rule::Never || base == Rule::Never {
        return Decision::never(match class {
            Class::Manager | Class::Automation => format!("{who} never {what}"),
            _ => format!("Settings › Permissions never let {who} {what}"),
        });
    }
    // An explicit ask: the project's list, then its conditions.
    if ProjectPolicy::names(&policy.ask, op) {
        return Decision::ask(format!("{at}'s policy asks before {}", op.name()));
    }
    if !looks {
        for condition in &policy.conditions {
            if let Verdict::Ask(why) = condition.check(&facts) {
                return Decision::ask(why);
            }
        }
    }
    let default_why = || match class {
        Class::Manager | Class::Automation => format!("{at} is {level}: {who} asks before it {what}"),
        Class::PersonSession if !place.own => format!("{at} is {level} and not this session's project: it asks before {who} can {what}"),
        _ => format!("Settings › Permissions ask before {who} can {what}"),
    };
    // D28: acting in, or running script in, a site the browser holds the person's sign-in for
    // asks, whoever asks and whatever the level (trust-m1).
    if op.spec.group == OpGroup::Browser && op.args.get("signed_in").and_then(Value::as_bool) == Some(true) {
        let site = op.args.get("url").and_then(Value::as_str).unwrap_or("the page");
        return Decision::ask(format!("{who} would act on {site}, where the browser is signed in as you"));
    }
    // A session the person does not drive browses only the project's sites (M23): in a browser
    // with no sign-ins, so on them it acts without a card; anywhere else, never.
    if op.spec.group == OpGroup::Browser && class == Class::Automation {
        let site = op.args.get("url").and_then(Value::as_str).unwrap_or_default();
        return if crate::browser::host_allowed(&policy.browser_hosts, site) { Decision::Allow } else { Decision::never(format!("{site} is not one of the sites {at} lets {who} open")) };
    }
    if base == Rule::Allow {
        return Decision::Allow;
    }
    // The person allowed it always in this project, where a grant could lift it.
    let liftable = match class {
        Class::Manager | Class::Automation => ceiling == Rule::Allow,
        Class::PersonSession => true,
        Class::Terminal => false,
    };
    if liftable && ProjectPolicy::names(&policy.allow, op) {
        return Decision::Allow;
    }
    // The request: a grant lifts an ask, where the ceiling lets it.
    if ceiling == Rule::Allow && grant.is_some_and(|grant| grant.covers(caller, op, targets)) {
        return Decision::Allow;
    }
    Decision::ask(default_why())
}


/// An op's summary after "only you can": a verb's as it is ("drop a note"), a thing's after "see"
/// ("what waits for you: …" reads "see what waits for you: …").
fn only_you(summary: &str) -> String {
    const VERBS: &[&str] = &[
        "accept", "add", "allow", "answer", "approve", "ask", "cancel", "change", "choose", "close", "drop", "edit", "export", "forget", "give", "have", "import", "keep", "let",
        "list", "make", "mark", "merge", "move", "open", "pick", "put", "read", "reject", "remove", "rename", "rewrite", "run", "say", "search", "see", "send", "set", "show",
        "start", "stop", "switch", "take", "turn", "undo", "write",
    ];
    let first = summary.split_whitespace().next().unwrap_or("").to_lowercase();
    if VERBS.contains(&first.as_str()) { summary.to_string() } else { format!("see {summary}") }
}

#[cfg(test)]
mod tests;
