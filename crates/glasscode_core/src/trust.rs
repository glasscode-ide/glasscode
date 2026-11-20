//! What Settings shows and changes of trust (slice M53, ux-13): the table of what each level
//! allows, generated from the gate's own default table; what a change would do, by asking
//! `gate::decide` before and after it; the conditions in words and back; the playbooks and the
//! schedules as plain sentences; and the change itself, which is written only once the person
//! confirmed its preview. Nothing here is TOML or YAML: the files are written whole from these
//! types, and only the person's confirmed change approves them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::caller::{Caller, Lane};
use crate::control::Rule;
use crate::gate::{self, Base, Cell, Column, Condition, ForgeFacts, Level, Policy, PolicySet, ProjectKey, Row};
use crate::ops::{self, Op, OpSpec, Targets};
use crate::playbooks::model::{Autonomy, Per, Playbook, ScheduleSpec, Trigger};
use crate::playbooks::{Book, builtins};
use crate::schedule::{Schedule, ScheduleFile};

// ----- the table of levels ---------------------------------------------------------------------

/// A cell of the default table in words.
pub fn cell_words(cell: Cell) -> &'static str {
    match (cell.base, cell.ceiling) {
        (Base::Rule(Rule::Allow), _) => "yes",
        (Base::Rule(Rule::Ask), Rule::Ask) => "a card each time",
        (Base::Rule(Rule::Ask), _) => "asks",
        (Base::Rule(Rule::Never), _) => "never",
        (Base::StartedByCaller, _) => "in its own sessions; asks for others",
        (Base::ForgeWall, _) => "where GitHub guards merges; else asks",
    }
}

/// One line of the table: what the manager does, and each level's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TableLine {
    pub what: &'static str,
    /// Careful (the manager), Careful (a session it drives), Standard, Autopilot.
    pub cells: [&'static str; 4],
    pub note: &'static str,
}

/// The table of what each level allows, row by row of `gate::defaults::TABLE` (a row of moves
/// only once).
pub fn level_table() -> Vec<TableLine> {
    let mut out: Vec<TableLine> = Vec::new();
    for row in gate::TABLE {
        let cells = [
            cell_words(row.cell(Level::Careful, Column::Manager)),
            cell_words(row.cell(Level::Careful, Column::Automation)),
            cell_words(row.cell(Level::Standard, Column::Manager)),
            cell_words(row.cell(Level::Autopilot, Column::Manager)),
        ];
        let line = TableLine { what: row.what, cells, note: row.note };
        if !out.contains(&line) {
            out.push(line);
        }
    }
    out
}

/// A level's consequences for the manager, in sentences ("May read, keep the board …. Asks before
/// it starts a session. Never changes settings."), from the same table.
pub fn consequences(level: Level) -> Vec<String> {
    let mut may: Vec<&str> = Vec::new();
    let mut asks: Vec<&str> = Vec::new();
    let mut cards: Vec<&str> = Vec::new();
    let mut never: Vec<&str> = Vec::new();
    let mut partly: Vec<String> = Vec::new();
    for row in gate::TABLE {
        let cell = row.cell(level, Column::Manager);
        let list = match (cell.base, cell.ceiling) {
            (Base::Rule(Rule::Allow), _) => &mut may,
            (Base::Rule(Rule::Ask), Rule::Ask) => &mut cards,
            (Base::Rule(Rule::Ask), _) => &mut asks,
            (Base::Rule(Rule::Never), _) => &mut never,
            (Base::StartedByCaller | Base::ForgeWall, _) => {
                let line = format!("It {} {}.", row.what, cell_words(cell));
                if !partly.contains(&line) {
                    partly.push(line);
                }
                continue;
            }
        };
        if !list.contains(&row.what) {
            list.push(row.what);
        }
    }
    let join = |items: &[&str]| -> String {
        match items {
            [] => String::new(),
            [one] => one.to_string(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        }
    };
    let mut out = Vec::new();
    if !may.is_empty() {
        out.push(format!("It {} without asking.", join(&may)));
    }
    if !asks.is_empty() {
        out.push(format!("It asks before it {}.", join(&asks)));
    }
    if !cards.is_empty() {
        out.push(format!("A card with everything shows before it {}.", join(&cards)));
    }
    out.extend(partly);
    if !never.is_empty() {
        out.push(format!("It never {}.", join(&never)));
    }
    out
}

// ----- previews --------------------------------------------------------------------------------

/// One op for each row of the table that has one: what a preview asks `decide` about.
pub fn probes() -> Vec<(&'static Row, &'static OpSpec)> {
    let mut out: Vec<(&'static Row, &'static OpSpec)> = Vec::new();
    for spec in ops::OPS.iter().filter(|spec| !spec.human_only) {
        let Some(row) = gate::row_of(spec) else { continue };
        if !out.iter().any(|(have, _)| std::ptr::eq(*have, row)) {
            out.push((row, spec));
        }
    }
    out
}

/// What `decide` says of a probe for the manager in `key`'s project, under `policy`.
pub fn probe(spec: &'static OpSpec, policy: &Policy, key: &ProjectKey, forge: &BTreeMap<ProjectKey, ForgeFacts>) -> Rule {
    let caller = Caller::Manager { lane: Lane::Chat, wake: None, project: Some(key.clone()) };
    let op = Op { spec, args: json!({}) };
    let targets = Targets { projects: vec![key.clone()], ..Targets::default() };
    let set = PolicySet { policy: policy.clone(), forge: forge.clone(), ..PolicySet::default() };
    gate::decide(&caller, &op, &targets, &set, None).rule()
}

/// One row whose answer a change moves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Moved {
    pub what: &'static str,
    pub op: &'static str,
    pub before: Rule,
    pub after: Rule,
}

fn rule_words(rule: Rule) -> &'static str {
    match rule {
        Rule::Allow => "without asking",
        Rule::Ask => "asks",
        Rule::Never => "never",
    }
}

/// What a change of the policy does in a project, for the manager: each row whose answer moves.
pub fn moved(before: &Policy, after: &Policy, key: &ProjectKey, forge: &BTreeMap<ProjectKey, ForgeFacts>) -> Vec<Moved> {
    probes()
        .into_iter()
        .filter_map(|(row, spec)| {
            let (b, a) = (probe(spec, before, key, forge), probe(spec, after, key, forge));
            (b != a).then_some(Moved { what: row.what, op: spec.name, before: b, after: a })
        })
        .collect()
}

// ----- conditions in words ---------------------------------------------------------------------

/// A condition from a line a person typed in Settings:
/// - `not on main, release/*` (a branch rule);
/// - `never secrets/**, ~/vault/**` (paths);
/// - `hours 09:00-18:00 weekdays` (or `every day`, or `Mon,Tue,Wed`);
/// - `only Sam, Alex` (who may allow what it asks).
pub fn parse_condition(text: &str) -> Result<Condition, String> {
    let text = text.trim();
    let list = |rest: &str| -> Vec<String> { rest.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect() };
    let lower = text.to_lowercase();
    let after = |prefix: &str| lower.starts_with(prefix).then(|| text[prefix.len()..].trim());
    if let Some(rest) = after("not on ") {
        let not = list(rest);
        return if not.is_empty() { Err("Say which branches: not on main, release/*".into()) } else { Ok(Condition::Branch { not }) };
    }
    if let Some(rest) = after("never ") {
        let deny = list(rest);
        return if deny.is_empty() { Err("Say which paths: never secrets/**".into()) } else { Ok(Condition::Paths { deny }) };
    }
    if let Some(rest) = after("only ") {
        let only = list(rest);
        return if only.is_empty() { Err("Say who: only Sam".into()) } else { Ok(Condition::Person { only }) };
    }
    if let Some(rest) = after("hours ") {
        let (range, days) = rest.split_once(' ').unwrap_or((rest, ""));
        let (from, to) = range.split_once('-').ok_or("Say the hours as 09:00-18:00")?;
        let minutes = |t: &str| -> Result<u16, String> {
            let (h, m) = t.trim().split_once(':').ok_or(format!("{t} is not a time like 09:00"))?;
            let (h, m): (u16, u16) = (h.parse().map_err(|_| format!("{t} is not a time"))?, m.parse().map_err(|_| format!("{t} is not a time"))?);
            if h > 24 || m > 59 || (h == 24 && m > 0) { Err(format!("{t} is not a time")) } else { Ok(h * 60 + m) }
        };
        let local = (minutes(from)?, minutes(to)?);
        let days = match days.trim().to_lowercase().as_str() {
            "" | "every day" | "daily" => 0,
            "weekdays" => 0b0111110,
            "weekends" => 0b1000001,
            named => {
                let names = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
                let mut bits = 0u8;
                for day in named.split(',').map(str::trim) {
                    let at = names.iter().position(|n| day.starts_with(n)).ok_or(format!("{day} is not a day"))?;
                    bits |= 1 << at;
                }
                bits
            }
        };
        return Ok(Condition::Hours { local, days });
    }
    Err("Start with: not on …, never …, hours …, or only …".into())
}

// ----- playbooks and schedules in words --------------------------------------------------------

fn autonomy_words(autonomy: Autonomy, op: &str) -> String {
    match autonomy {
        Autonomy::Watch => "notes it in the journal".into(),
        Autonomy::Tell => "tells you".into(),
        Autonomy::Ask => format!("asks you, then runs {op}"),
        Autonomy::Act => format!("runs {op} by itself"),
    }
}

fn per_words(per: Per) -> &'static str {
    match per {
        Per::Commit => "commit",
        Per::Pr => "pull request",
        Per::Session => "session",
        Per::Hour => "hour",
        Per::Day => "day",
    }
}

/// A schedule's time in words: "every 30 minutes", "at 09:00 on weekdays (Europe/Berlin)".
pub fn schedule_words(spec: &ScheduleSpec) -> String {
    match spec {
        ScheduleSpec::Every { secs } if *secs % 3600 == 0 && *secs >= 3600 => format!("every {} hours", secs / 3600).replace("every 1 hours", "every hour"),
        ScheduleSpec::Every { secs } if *secs % 60 == 0 => format!("every {} minutes", secs / 60).replace("every 1 minutes", "every minute"),
        ScheduleSpec::Every { secs } => format!("every {secs} seconds"),
        ScheduleSpec::At { minutes, days, tz } => {
            let names = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
            let on = match *days & 0x7f {
                0 | 0x7f => "every day".to_string(),
                0b0111110 => "on weekdays".to_string(),
                bits => format!("on {}", (0..7).filter(|d| bits & (1 << d) != 0).map(|d| names[d]).collect::<Vec<_>>().join(", ")),
            };
            format!("at {:02}:{:02} {on} ({tz})", minutes / 60, minutes % 60)
        }
        ScheduleSpec::Once { at_ms } => {
            let minutes = (*at_ms / 60_000) as i64;
            let day = crate::day::iso(minutes.div_euclid(1440));
            format!("once, on {day} at {:02}:{:02} UTC", minutes.rem_euclid(1440) / 60, minutes.rem_euclid(60))
        }
    }
}

/// A playbook as a plain sentence: when it fires, what it does and how often at most. A built-in's
/// starts from what it says of itself.
pub fn playbook_sentence(playbook: &Playbook) -> String {
    let limit = format!("at most {} a {}", playbook.limit.count, per_words(playbook.limit.per)).replace("at most 1 a", "once a");
    let does = autonomy_words(playbook.autonomy, &playbook.then.op);
    if let Some(builtin) = builtins::find(&playbook.id).filter(|_| playbook.builtin) {
        return format!("{} It {does}, {limit}.", builtin.what);
    }
    let when = match &playbook.trigger {
        Trigger::Event(filter) => event_words(filter),
        Trigger::After { event, delay_secs } => format!("{} after {}", schedule_words(&ScheduleSpec::Every { secs: *delay_secs }).replacen("every ", "", 1), event_words(event)),
        Trigger::Schedule { schedule } => capital(&schedule_words(schedule)),
    };
    let conditions = if playbook.when.is_empty() { String::new() } else { format!(", if {}", playbook.when.iter().map(cond_words).collect::<Vec<_>>().join(" and ")) };
    format!("{when}{conditions}, it {does}, {limit}.")
}

fn event_words(filter: &crate::playbooks::model::EventFilter) -> String {
    let fields: Vec<String> = filter.fields.iter().map(|(k, v)| format!("{k} {}", serde_json::to_string(v).unwrap_or_default())).collect();
    if fields.is_empty() { format!("When {} happens", filter.kind) } else { format!("When {} happens with {}", filter.kind, fields.join(", ")) }
}

fn cond_words(cond: &crate::playbooks::model::Cond) -> String {
    use crate::playbooks::model::Cond;
    match cond {
        Cond::Level { is } => format!("the project is {}", is.iter().map(|l| l.label()).collect::<Vec<_>>().join(" or ")),
        Cond::AuthorTrusted => "its author is someone you trust".into(),
        Cond::PrAuthorIsMe => "the pull request is yours".into(),
        Cond::Label { name } => format!("it has the label {name}"),
        Cond::NotDefaultBranch => "it is not on the default branch".into(),
    }
}

/// A schedule as a plain sentence.
pub fn schedule_sentence(schedule: &Schedule) -> String {
    format!("{}: {}, it {}.", schedule.name, schedule_words(&schedule.spec), autonomy_words(schedule.autonomy, &schedule.then.op))
}

fn capital(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

// ----- the change, previewed, then confirmed ---------------------------------------------------

/// A site as the allowlist keeps it: its host, lower case, with no scheme, path or port (`docs.rs`
/// covers its subdomains too).
pub fn site_of(text: &str) -> Result<String, String> {
    let t = text.trim().trim_start_matches("https://").trim_start_matches("http://");
    let host = t.split(['/', '?', '#']).next().unwrap_or_default().split(':').next().unwrap_or_default().trim_start_matches("*.").trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || !host.contains('.') || !host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
        return Err(format!("“{}” is not a site: say its host, such as docs.rs.", text.trim()));
    }
    Ok(host)
}

/// A change the person makes in Settings (or with `glasscode policy set …`, `playbook set …`,
/// `schedule set …`, person-only ops).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum Change {
    Level { project: ProjectKey, level: Level },
    AddCondition { project: ProjectKey, condition: Condition },
    RemoveCondition { project: ProjectKey, index: usize },
    /// How the project picks work by itself (M60): a label for Linear tickets, one for GitHub
    /// issues (an empty one stops that source), or no intake at all (`off`).
    Intake { project: ProjectKey, linear: Option<String>, github: Option<String>, off: bool },
    /// A question the manager may answer by itself on Autopilot (M61), and one it no longer may.
    AddDelegation { project: ProjectKey, delegation: crate::delegation::Delegation },
    RemoveDelegation { project: ProjectKey, index: usize },
    /// A site the project's sessions that the person does not drive may open (M23's allowlist).
    AddBrowserSite { project: ProjectKey, host: String },
    RemoveBrowserSite { project: ProjectKey, host: String },
    Playbook { id: String, on: bool },
    ApprovePlaybook { id: String },
    Schedule { id: String, on: bool },
    ApproveSchedule { id: String },
}

/// A change and its preview: what the person reads before they confirm it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub change: Change,
    pub title: String,
    pub lines: Vec<String>,
    /// The policy that is approved when it is confirmed (a change of the policy).
    pub policy: Option<Policy>,
    pub playbook: Option<Playbook>,
    pub schedule: Option<Schedule>,
}

/// What the preview reads: the approved policy, the playbooks, the schedules and what GitHub
/// guards, as the app holds them.
pub struct World<'a> {
    pub policy: &'a Policy,
    pub book: Option<&'a Book>,
    pub schedules: Option<&'a ScheduleFile>,
    pub forge: &'a BTreeMap<ProjectKey, ForgeFacts>,
}

fn with_project(policy: &Policy, key: &ProjectKey, change: impl FnOnce(&mut gate::ProjectPolicy)) -> Policy {
    let mut next = policy.clone();
    let project = next.projects.entry(key.clone()).or_insert_with(|| policy.default.clone());
    change(project);
    next
}

/// The preview of a change. Nothing is written.
pub fn prepare(change: Change, world: &World) -> Result<Pending, String> {
    let pending = |title: String, lines: Vec<String>| Pending { change: change.clone(), title, lines, policy: None, playbook: None, schedule: None };
    match &change {
        Change::Level { project, level } => {
            let next = with_project(world.policy, project, |p| p.level = *level);
            let mut lines = gate::diff(world.policy, &next);
            for moved in moved(world.policy, &next, project, world.forge) {
                lines.push(format!("It {}: {} until now, {} from now on.", moved.what, rule_words(moved.before), rule_words(moved.after)));
            }
            // Which playbooks start acting: an action whose op the manager may now run without asking.
            for playbook in world.book.map(|b| b.playbooks.as_slice()).unwrap_or_default() {
                let Some(spec) = ops::find(&playbook.then.op) else { continue };
                let (before, after) = (probe(spec, world.policy, project, world.forge), probe(spec, &next, project, world.forge));
                if playbook.enabled && playbook.autonomy == Autonomy::Act && before != Rule::Allow && after == Rule::Allow {
                    lines.push(format!("“{}” starts acting by itself here.", playbook.name));
                }
            }
            if *level == Level::Autopilot && !world.forge.get(project).is_some_and(ForgeFacts::holds) {
                lines.push(format!("GitHub does not guard merges and force-pushes in {project} (or it is not known yet): what Autopilot does on GitHub keeps asking until the repository's rules hold (D29). {}", gate::forge_line(world.forge.get(project))));
            }
            if lines.is_empty() {
                lines.push("Nothing changes.".into());
            }
            Ok(Pending { policy: Some(next), ..pending(format!("Set {project} to {}?", level.label()), lines) })
        }
        Change::AddCondition { project, condition } => {
            let next = with_project(world.policy, project, |p| {
                if !p.conditions.contains(condition) {
                    p.conditions.push(condition.clone());
                }
            });
            Ok(Pending { policy: Some(next.clone()), ..pending(format!("Add a condition to {project}?"), gate::diff(world.policy, &next)) })
        }
        Change::RemoveCondition { project, index } => {
            let have = world.policy.of(project).conditions.get(*index).cloned().ok_or("There is no such condition.")?;
            let next = with_project(world.policy, project, |p| p.conditions.retain(|c| *c != have));
            Ok(Pending { policy: Some(next.clone()), ..pending(format!("Remove a condition of {project}?"), gate::diff(world.policy, &next)) })
        }
        Change::AddDelegation { project, delegation } => {
            let next = with_project(world.policy, project, |p| {
                if !p.delegate.contains(delegation) {
                    p.delegate.push(delegation.clone());
                }
            });
            let mut lines = gate::diff(world.policy, &next);
            if next.of(project).level != Level::Autopilot {
                lines.push(format!("{project} is not on Autopilot: the manager answers by itself only once it is; until then its answers are cards."));
            }
            if lines.is_empty() {
                lines.push("Nothing changes.".into());
            }
            Ok(Pending { policy: Some(next), ..pending(format!("Let the manager answer this in {project}?"), lines) })
        }
        Change::RemoveDelegation { project, index } => {
            let have = world.policy.of(project).delegate.get(*index).cloned().ok_or("There is no such delegated decision.")?;
            let next = with_project(world.policy, project, |p| p.delegate.retain(|d| *d != have));
            Ok(Pending { policy: Some(next.clone()), ..pending(format!("Take a delegated decision of {project} back?"), gate::diff(world.policy, &next)) })
        }
        Change::AddBrowserSite { project, host } => {
            let host = site_of(host)?;
            let next = with_project(world.policy, project, |p| {
                if !p.browser_hosts.contains(&host) {
                    p.browser_hosts.push(host.clone());
                }
            });
            Ok(Pending { policy: Some(next.clone()), ..pending(format!("Let {project}'s sessions open {host}?"), gate::diff(world.policy, &next)) })
        }
        Change::RemoveBrowserSite { project, host } => {
            let host = site_of(host)?;
            if !world.policy.of(project).browser_hosts.contains(&host) {
                return Err(format!("{host} is not one of {project}'s sites."));
            }
            let next = with_project(world.policy, project, |p| p.browser_hosts.retain(|h| *h != host));
            Ok(Pending { policy: Some(next.clone()), ..pending(format!("Take {host} off {project}'s sites?"), gate::diff(world.policy, &next)) })
        }
        Change::Intake { project, linear, github, off } => {
            let label = |text: &Option<String>, had: Option<String>| match text {
                Some(text) => Some(text.trim().to_string()).filter(|t| !t.is_empty()),
                None => had,
            };
            let next = with_project(world.policy, project, |p| {
                if *off {
                    p.intake = None;
                    return;
                }
                let mut intake = p.intake.clone().unwrap_or_default();
                intake.linear_label = label(linear, intake.linear_label.clone());
                intake.github_label = label(github, intake.github_label.clone());
                p.intake = (intake.linear_label.is_some() || intake.github_label.is_some()).then_some(intake);
            });
            let mut lines = gate::diff(world.policy, &next);
            if next.of(project).intake.is_some() && next.of(project).level != Level::Autopilot {
                lines.push(format!("{project} is not on Autopilot: it picks work only once it is."));
            }
            if lines.is_empty() {
                lines.push("Nothing changes.".into());
            }
            Ok(Pending { policy: Some(next), ..pending(format!("Change how {project} picks work?"), lines) })
        }
        Change::Playbook { id, .. } | Change::ApprovePlaybook { id } => {
            let book = world.book.ok_or("The playbooks could not be read.")?;
            let have = book.playbooks.iter().find(|p| p.id == *id).cloned().ok_or(format!("There is no playbook “{id}”."))?;
            if have.builtin && [builtins::ci_fix::ID, builtins::ci_fix::CONFLICTS].contains(&id.as_str()) {
                return Err(format!("“{}” is switched in Settings › Automation, where older versions read it too.", have.name));
            }
            let mut next = have.clone();
            let title = match &change {
                Change::Playbook { on: true, .. } => format!("Switch on “{}”?", have.name),
                Change::Playbook { on: false, .. } => format!("Switch off “{}”?", have.name),
                _ => format!("Approve “{}”?", have.name),
            };
            if let Change::Playbook { on, .. } = &change {
                next.enabled = *on;
            }
            let mut lines = vec![playbook_sentence(&next)];
            if let Some(by) = &have.proposed_by {
                lines.push(format!("Proposed by {by}; it runs only once you approve it."));
            }
            if next.enabled && !matches!(change, Change::Playbook { on: false, .. }) {
                lines.push("Approving it means it runs as written above, on this Mac.".into());
            }
            Ok(Pending { playbook: Some(next), ..pending(title, lines) })
        }
        Change::Schedule { id, .. } | Change::ApproveSchedule { id } => {
            let file = world.schedules.ok_or("The schedules could not be read.")?;
            let have = file.schedules.iter().find(|s| s.id == *id).cloned().ok_or(format!("There is no schedule “{id}”."))?;
            let mut next = have.clone();
            let title = match &change {
                Change::Schedule { on: true, .. } => format!("Switch on “{}”?", have.name),
                Change::Schedule { on: false, .. } => format!("Switch off “{}”?", have.name),
                _ => format!("Approve “{}”?", have.name),
            };
            if let Change::Schedule { on, .. } = &change {
                next.enabled = *on;
            }
            Ok(Pending { schedule: Some(next.clone()), ..pending(title, vec![schedule_sentence(&next)]) })
        }
    }
}

/// Write a previewed change, once the person confirmed it; never without the confirmation. A
/// policy is approved whole (live file and snapshot); a playbook or a schedule is written as the
/// person's and approved as it is. It writes the disk: off the main thread.
pub fn apply(pending: &Pending, confirmed: bool) -> Result<String, String> {
    if !confirmed {
        return Err("Nothing changed: confirm the preview first.".into());
    }
    if let Some(policy) = &pending.policy {
        gate::approve(policy).map_err(|err| err.to_string())?;
        return Ok("The policy is changed and approved.".into());
    }
    if let Some(playbook) = &pending.playbook {
        crate::playbooks::propose(playbook.clone(), None).map_err(|err| err.to_string())?;
        crate::playbooks::approve(&playbook.id, true).map_err(|err| err.to_string())?;
        return Ok(format!("“{}” is {}.", playbook.name, if playbook.enabled { "on and approved" } else { "off" }));
    }
    if let Some(schedule) = &pending.schedule {
        crate::schedule::propose(schedule.clone(), None).map_err(|err| err.to_string())?;
        crate::schedule::approve(&schedule.id, true).map_err(|err| err.to_string())?;
        return Ok(format!("“{}” is {}.", schedule.name, if schedule.enabled { "on and approved" } else { "off" }));
    }
    Err("Nothing to change.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> ProjectKey {
        ProjectKey::parse("app-3f9a1c").unwrap()
    }

    fn world<'a>(policy: &'a Policy, book: Option<&'a Book>, forge: &'a BTreeMap<ProjectKey, ForgeFacts>) -> World<'a> {
        World { policy, book, schedules: None, forge }
    }

    /// M60's intake, set by the person: a label for each source, previewed with what it picks and
    /// that it waits for Autopilot; an empty label stops that source, and off stops intake.
    #[test]
    fn intake_is_set_by_labels_and_previewed() {
        let policy = Policy::default();
        let forge = BTreeMap::new();
        let change = Change::Intake { project: key(), linear: None, github: Some("ready".into()), off: false };
        let pending = prepare(change, &world(&policy, None, &forge)).unwrap();
        let next = pending.policy.clone().unwrap();
        assert_eq!(next.of(&key()).intake.as_ref().and_then(|i| i.github_label.as_deref()), Some("ready"));
        assert!(pending.lines.iter().any(|l| l.contains("GitHub issues labelled “ready”")), "{:?}", pending.lines);
        assert!(pending.lines.iter().any(|l| l.contains("not on Autopilot")), "{:?}", pending.lines);
        let both = prepare(Change::Intake { project: key(), linear: Some("auto".into()), github: None, off: false }, &world(&next, None, &forge)).unwrap().policy.unwrap();
        let intake = both.of(&key()).intake.clone().unwrap();
        assert_eq!((intake.linear_label.as_deref(), intake.github_label.as_deref()), (Some("auto"), Some("ready")), "a label not named stays");
        let one = prepare(Change::Intake { project: key(), linear: Some(" ".into()), github: None, off: false }, &world(&both, None, &forge)).unwrap().policy.unwrap();
        assert_eq!(one.of(&key()).intake.as_ref().map(|i| i.linear_label.is_none()), Some(true));
        let off = prepare(Change::Intake { project: key(), linear: None, github: None, off: true }, &world(&both, None, &forge)).unwrap();
        assert!(off.policy.unwrap().of(&key()).intake.is_none());
        assert!(off.lines.iter().any(|l| l.contains("no longer picks work")), "{:?}", off.lines);
    }

    /// M61's delegated decisions and M23's sites, set by the person: added and taken back, each
    /// previewed; a delegation says it waits for Autopilot, a site is kept as its host.
    #[test]
    fn delegations_and_sites_are_added_and_taken_back() {
        let policy = Policy::default();
        let forge = BTreeMap::new();
        let delegation = crate::delegation::parse("run the tests: proceed for cargo test => Yes").unwrap();
        let pending = prepare(Change::AddDelegation { project: key(), delegation: delegation.clone() }, &world(&policy, None, &forge)).unwrap();
        let next = pending.policy.clone().unwrap();
        assert_eq!(next.of(&key()).delegate, vec![delegation]);
        assert!(pending.lines.iter().any(|l| l.contains("not on Autopilot")), "{:?}", pending.lines);
        let back = prepare(Change::RemoveDelegation { project: key(), index: 0 }, &world(&next, None, &forge)).unwrap().policy.unwrap();
        assert!(back.of(&key()).delegate.is_empty());
        assert!(prepare(Change::RemoveDelegation { project: key(), index: 3 }, &world(&back, None, &forge)).is_err());
        let site = prepare(Change::AddBrowserSite { project: key(), host: "https://Docs.rs/serde".into() }, &world(&policy, None, &forge)).unwrap();
        let with_site = site.policy.clone().unwrap();
        assert_eq!(with_site.of(&key()).browser_hosts, vec!["docs.rs".to_string()]);
        assert!(site.lines.iter().any(|l| l.contains("docs.rs")), "{:?}", site.lines);
        assert!(prepare(Change::AddBrowserSite { project: key(), host: "not a site".into() }, &world(&policy, None, &forge)).is_err());
        let gone = prepare(Change::RemoveBrowserSite { project: key(), host: "docs.rs".into() }, &world(&with_site, None, &forge)).unwrap().policy.unwrap();
        assert!(gone.of(&key()).browser_hosts.is_empty());
    }

    #[test]
    fn sentence_for_each_builtin() {
        let mut seen = std::collections::BTreeSet::new();
        for builtin in builtins::BUILTINS {
            let sentence = playbook_sentence(&builtins::entry(builtin));
            assert!(sentence.starts_with(builtin.what), "{sentence}");
            assert!(sentence.ends_with('.') && (sentence.contains("once a") || sentence.contains("at most")), "{sentence}");
            assert!(!sentence.contains('{') && !sentence.contains("TOML") && !sentence.contains("YAML"), "{sentence}");
            assert!(seen.insert(sentence), "two built-ins read alike");
        }
        // One of the person's own, from its parts.
        let mut own = builtins::entry(&builtins::BUILTINS[0]);
        own.builtin = false;
        own.id = "mine".into();
        own.when = vec![crate::playbooks::model::Cond::PrAuthorIsMe];
        assert_eq!(playbook_sentence(&own), "When ci.failed happens, if the pull request is yours, it runs session.send by itself, once a commit.");
        assert_eq!(schedule_words(&ScheduleSpec::At { minutes: 9 * 60, days: 0b0111110, tz: "Europe/Berlin".into() }), "at 09:00 on weekdays (Europe/Berlin)");
        assert_eq!(schedule_words(&ScheduleSpec::Every { secs: 1800 }), "every 30 minutes");
        assert_eq!(schedule_words(&ScheduleSpec::Every { secs: 3600 }), "every hour");
    }

    #[test]
    fn preview_matches_decide() {
        let (policy, forge) = (Policy::default(), BTreeMap::new());
        let pending = prepare(Change::Level { project: key(), level: Level::Standard }, &world(&policy, None, &forge)).unwrap();
        let next = pending.policy.clone().unwrap();
        assert_eq!(next.of(&key()).level, Level::Standard);
        // Every line of what moves is what decide says, before and after.
        let moves = moved(&policy, &next, &key(), &forge);
        assert!(!moves.is_empty());
        for m in &moves {
            let spec = ops::find(m.op).unwrap();
            assert_eq!((probe(spec, &policy, &key(), &forge), probe(spec, &next, &key(), &forge)), (m.before, m.after), "{}", m.op);
            assert!(pending.lines.iter().any(|line| line.contains(m.what)), "{}: {:?}", m.what, pending.lines);
        }
        // Rows that do not move are not listed: a read stays allowed.
        let read = ops::find("session.list").unwrap();
        assert_eq!((probe(read, &policy, &key(), &forge), probe(read, &next, &key(), &forge)), (Rule::Allow, Rule::Allow));
        // The table and the consequences come from the same rows.
        assert!(!level_table().is_empty() && level_table().len() <= gate::TABLE.len());
        assert!(level_table().iter().any(|line| line.what == "changes settings" && line.cells == ["never"; 4]));
        assert!(consequences(Level::Careful).iter().any(|s| s.contains("never changes settings")), "{:?}", consequences(Level::Careful));
        assert!(consequences(Level::Standard).iter().any(|s| s.contains("starts a session")));
    }

    #[test]
    fn autopilot_says_which_forge_rules_are_missing() {
        let (policy, forge) = (Policy::default(), BTreeMap::new());
        let pending = prepare(Change::Level { project: key(), level: Level::Autopilot }, &world(&policy, None, &forge)).unwrap();
        assert!(pending.lines.iter().any(|l| l.contains("GitHub does not guard merges")), "{:?}", pending.lines);
    }

    #[test]
    fn change_needs_confirm() {
        let (policy, forge) = (Policy::default(), BTreeMap::new());
        let pending = prepare(Change::Level { project: key(), level: Level::Autopilot }, &world(&policy, None, &forge)).unwrap();
        // Not confirmed: nothing is written, whatever the preview says.
        assert_eq!(apply(&pending, false), Err("Nothing changed: confirm the preview first.".into()));
        // The ops that make these changes are the person's only.
        for name in ["policy.set", "playbook.set", "schedule.set"] {
            let spec = ops::find(name).unwrap();
            assert!(spec.human_only && spec.v2_only && spec.args.iter().any(|a| a.name == "confirm"), "{name}");
        }
    }

    #[test]
    fn conditions_from_words() {
        assert_eq!(parse_condition("not on main, release/*"), Ok(Condition::Branch { not: vec!["main".into(), "release/*".into()] }));
        assert_eq!(parse_condition("Never secrets/**"), Ok(Condition::Paths { deny: vec!["secrets/**".into()] }));
        assert_eq!(parse_condition("only Sam, Alex"), Ok(Condition::Person { only: vec!["Sam".into(), "Alex".into()] }));
        assert_eq!(parse_condition("hours 09:00-18:00 weekdays"), Ok(Condition::Hours { local: (540, 1080), days: 0b0111110 }));
        assert_eq!(parse_condition("hours 22:00-06:00 mon,fri"), Ok(Condition::Hours { local: (1320, 360), days: 0b0100010 }));
        for bad in ["", "sometimes", "not on ", "hours 9-18", "hours 25:00-26:00"] {
            assert!(parse_condition(bad).is_err(), "{bad}");
        }
        // A condition added is one the gate reads, and its words come back.
        let (policy, forge) = (Policy::default(), BTreeMap::new());
        let condition = parse_condition("not on main").unwrap();
        let pending = prepare(Change::AddCondition { project: key(), condition: condition.clone() }, &world(&policy, None, &forge)).unwrap();
        assert!(pending.lines.iter().any(|l| l.contains(&condition.describe())), "{:?}", pending.lines);
        assert!(prepare(Change::RemoveCondition { project: key(), index: 0 }, &world(&policy, None, &forge)).is_err());
    }

    #[test]
    fn the_ci_fix_is_switched_where_it_always_was() {
        let book = Book { playbooks: vec![builtins::entry(&builtins::BUILTINS[0])], ..Book::default() };
        let (policy, forge) = (Policy::default(), BTreeMap::new());
        let err = prepare(Change::Playbook { id: builtins::ci_fix::ID.into(), on: false }, &world(&policy, Some(&book), &forge)).unwrap_err();
        assert!(err.contains("Settings › Automation"), "{err}");
        let other = builtins::BUILTINS.iter().find(|b| b.needs_manager).unwrap();
        let book = Book { playbooks: vec![builtins::entry(other)], ..Book::default() };
        let pending = prepare(Change::Playbook { id: other.id.into(), on: false }, &world(&policy, Some(&book), &forge)).unwrap();
        assert_eq!(pending.playbook.as_ref().map(|p| p.enabled), Some(false));
        assert!(pending.lines[0].starts_with(other.what));
    }
}
