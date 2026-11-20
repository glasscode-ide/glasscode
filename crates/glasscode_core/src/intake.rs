//! Intake: the manager finds work by itself (M60, crit-12), on an Autopilot project only.
//!
//! - **Where from.** Linear tickets with the project's label, and the repository's GitHub issues
//!   with its label, opened by a trusted author (D27: the person, a confirmed teammate, or an
//!   OWNER, MEMBER or COLLABORATOR). The rule is the project's policy (`ProjectPolicy::intake`):
//!   the person approves it with the rest of the policy, so nothing else can switch it on.
//! - **How much.** At most `max_sessions` automation sessions of the project at once, and at most
//!   `daily_tokens` tokens a day for all of its sessions.
//! - **Back-pressure.** No new work while `max_waiting` of the project's pull requests wait for a
//!   review, or while CI is red on one of them.
//! - **Done when.** Each item it picks is a work item with typed done conditions: its pull
//!   request ready for review, with CI green.
//! - **Once.** An item is picked once: its effect (`intent`) is claimed before its session starts,
//!   and work the ledger knows already is not picked again.
//!
//! Pure: the app hands it what it found and how loaded the project is, and starts what it picks.

use serde::{Deserialize, Serialize};

use crate::gate::{Level, ProjectPolicy};
use crate::identity::{WorkKey, WorkKind};
use crate::paths::ProjectKey;
use crate::work::DoneCondition;

/// A project's intake rule (its policy's `intake`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Intake {
    /// Linear tickets with this label are picked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linear_label: Option<String>,
    /// The repository's GitHub issues with this label, opened by a trusted author, are picked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github_label: Option<String>,
    /// At most this many automation sessions of the project at once.
    pub max_sessions: u32,
    /// At most this many tokens a day, for all of the project's sessions.
    pub daily_tokens: u64,
    /// No new work while this many of its pull requests wait for a review.
    pub max_waiting: u32,
    /// No new work while CI is red on one of its pull requests.
    pub stop_on_red: bool,
}

impl Default for Intake {
    fn default() -> Self {
        Intake { linear_label: None, github_label: None, max_sessions: 2, daily_tokens: 3_000_000, max_waiting: 3, stop_on_red: true }
    }
}

impl Intake {
    /// One line for Settings and `POLICY.md`'s header.
    pub fn describe(&self) -> String {
        let from: Vec<String> = [self.linear_label.as_ref().map(|l| format!("Linear tickets labelled “{l}”")), self.github_label.as_ref().map(|l| format!("GitHub issues labelled “{l}” by trusted authors"))].into_iter().flatten().collect();
        let from = if from.is_empty() { "nothing (no label set)".to_string() } else { from.join(" and ") };
        format!(
            "On Autopilot, picks work by itself from {from}: at most {} sessions at once and {} tokens a day, none while {} pull requests wait for review{}.",
            self.max_sessions,
            self.daily_tokens,
            self.max_waiting,
            if self.stop_on_red { " or CI is red" } else { "" }
        )
    }
}

/// Where a piece of work comes from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum Source {
    Linear { id: String, identifier: String },
    Issue { repo: String, number: u64 },
}

/// A ticket or an issue that may be work for the project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Found {
    pub source: Option<Source>,
    pub title: String,
    pub body: String,
    pub url: String,
    /// Who opened it (a login, for an issue).
    pub author: String,
    /// The author's association with the repository (an issue's `author_association`).
    pub association: Option<String>,
    pub labels: Vec<String>,
    /// Linear's: 1 urgent … 4 low, 0 none.
    pub priority: u8,
    /// When it was opened (ISO 8601: sorts as text).
    pub created: String,
}

impl Found {
    /// Its id: `ENG-42`, or `acme/app#12`.
    pub fn id(&self) -> String {
        match &self.source {
            Some(Source::Linear { identifier, .. }) => identifier.clone(),
            Some(Source::Issue { repo, number }) => format!("{repo}#{number}"),
            None => String::new(),
        }
    }

    fn labelled(&self, label: &str) -> bool {
        self.labels.iter().any(|l| l.trim().eq_ignore_ascii_case(label.trim()))
    }

    /// Picked first: the most urgent (Linear's priority; none counts as lowest), then the oldest.
    fn order(&self) -> (u8, String) {
        (if self.priority == 0 { 5 } else { self.priority }, self.created.clone())
    }
}

/// A Linear ticket as found work (a GitHub issue on the board is found as an issue instead).
pub fn from_ticket(ticket: &crate::linear::Ticket) -> Option<Found> {
    if ticket.provider != crate::model::Provider::Linear || ticket.is_done() {
        return None;
    }
    Some(Found {
        source: Some(Source::Linear { id: ticket.id.clone(), identifier: ticket.identifier.clone() }),
        title: ticket.title.clone(),
        body: ticket.description.clone(),
        url: ticket.url.clone(),
        author: ticket.creator.clone().unwrap_or_default(),
        association: None,
        labels: ticket.labels.clone(),
        priority: ticket.priority,
        created: ticket.created_at.clone(),
    })
}

/// A repository's open issues as GitHub's REST API lists them (`repos/{repo}/issues`): pull
/// requests, which it lists too, are left out.
pub fn issues_from_json(repo: &str, list: &serde_json::Value) -> Vec<Found> {
    let text = |value: &serde_json::Value| value.as_str().unwrap_or_default().to_string();
    list.as_array()
        .into_iter()
        .flatten()
        .filter(|issue| issue.get("pull_request").is_none() && issue["state"].as_str().unwrap_or("open") == "open")
        .filter_map(|issue| {
            let number = issue["number"].as_u64()?;
            Some(Found {
                source: Some(Source::Issue { repo: repo.to_string(), number }),
                title: text(&issue["title"]),
                body: text(&issue["body"]),
                url: text(&issue["html_url"]),
                author: text(&issue["user"]["login"]),
                association: issue["author_association"].as_str().map(str::to_string),
                labels: issue["labels"].as_array().into_iter().flatten().filter_map(|l| l["name"].as_str().or(l.as_str()).map(str::to_string)).collect(),
                priority: 0,
                created: text(&issue["created_at"]),
            })
        })
        .collect()
}

/// A repository's open issues with `label`, through `gh api`. It runs `gh`: never on the main
/// thread.
pub fn labeled_issues(repo: &str, label: &str, env: &std::collections::HashMap<String, String>) -> anyhow::Result<Vec<Found>> {
    let label: String = label.chars().map(|c| if c.is_ascii_alphanumeric() || "-_.~".contains(c) { c.to_string() } else { format!("%{:02X}", c as u32 & 0xff) }).collect();
    let path = format!("repos/{repo}/issues?state=open&per_page=50&labels={label}");
    let list = crate::github::gh_json(repo, env, &["api", &path])?;
    Ok(issues_from_json(repo, &list))
}

/// How loaded a project is now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Load {
    /// Its sessions that run and that automation started (not the person).
    pub automation_sessions: u32,
    /// Tokens its sessions used today.
    pub tokens_today: u64,
    /// Its pull requests that wait for a review (open, not a draft, not approved).
    pub waiting_review: u32,
    /// Its pull requests with red CI (`acme/app#12`).
    pub red: Vec<String>,
}

/// Why intake took nothing (for the whole project), or not this item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Held {
    NotAutopilot,
    Untrusted { author: String },
    Cap { running: u32, max: u32 },
    Budget { used: u64, max: u64 },
    Waiting { waiting: u32, max: u32 },
    Red(String),
}

impl Held {
    pub fn words(&self) -> String {
        match self {
            Held::NotAutopilot => "the project is not on Autopilot".into(),
            Held::Untrusted { author } => format!("{author} is not a trusted author (D27): their text does not go to a session"),
            Held::Cap { running, max } => format!("{running} of its sessions run already (at most {max})"),
            Held::Budget { used, max } => format!("its sessions used {used} tokens today (at most {max})"),
            Held::Waiting { waiting, max } => format!("{waiting} of its pull requests wait for review (new work waits below {max})"),
            Held::Red(pr) => format!("CI is red on {pr}"),
        }
    }
}

/// A piece of work intake takes: its work item and the session that does it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pick {
    pub found: Found,
    pub key: WorkKey,
    /// Typed: when the work counts as done.
    pub done: Vec<DoneCondition>,
    /// The first message of its session.
    pub message: String,
}

/// What one round of intake does for a project.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub picks: Vec<Pick>,
    /// Items left out, and why.
    pub held: Vec<(String, Held)>,
    /// Why nothing was taken at all, when nothing could be.
    pub blocked: Option<Held>,
}

/// The work an item is: a Linear ticket's (`Ticket(ENG-42)`, as `ticket.start` makes it), or an
/// issue's branch (`Branch(12-crash-on-start)`, the session `session.new` starts on it).
pub fn work_key(project: &ProjectKey, found: &Found) -> WorkKey {
    match &found.source {
        Some(Source::Issue { number, .. }) => WorkKey { project: project.clone(), kind: WorkKind::Branch, id: branch(found).unwrap_or_else(|| number.to_string()) },
        _ => WorkKey { project: project.clone(), kind: WorkKind::Ticket, id: found.id() },
    }
}

/// The branch an issue's session starts on (None for a ticket: `ticket.start` names its own).
pub fn branch(found: &Found) -> Option<String> {
    match &found.source {
        Some(Source::Issue { number, .. }) => Some(crate::github_tickets::branch_for_issue(*number, &found.title)),
        _ => None,
    }
}

/// The done conditions of work intake picked.
pub fn done_conditions() -> Vec<DoneCondition> {
    vec![DoneCondition::PrReadyForReview, DoneCondition::CiGreen]
}

/// The longest stretch of an item's text that goes into its session's first message.
const BODY_MAX: usize = 4_000;

/// The first message of an item's session.
pub fn message(found: &Found) -> String {
    let body = found.body.trim();
    let body = if body.chars().count() > BODY_MAX { format!("{}…", body.chars().take(BODY_MAX).collect::<String>()) } else { body.to_string() };
    let mut out = format!("Work on {}: {}", found.id(), found.title.trim());
    if !found.url.is_empty() {
        out.push_str(&format!(" ({})", found.url));
    }
    out.push_str(".\n\n");
    if !body.is_empty() {
        out.push_str(&body);
        out.push_str("\n\n");
    }
    out.push_str("Glasscode picked this up by itself (the project is on Autopilot). It is done when its pull request is ready for review and CI is green: open one, mark it ready, and make CI pass.");
    out
}

/// What intake takes for `project` this round. `taken` says whether work is known already (the
/// work ledger, or an earlier pick's effect); `person` and `confirmed` are the person's logins and
/// their confirmed teammates' (D27).
pub fn plan(project: &ProjectKey, policy: &ProjectPolicy, found: &[Found], load: &Load, taken: &dyn Fn(&WorkKey) -> bool, person: &[String], confirmed: &[String]) -> Plan {
    let mut plan = Plan::default();
    let Some(rule) = policy.intake.as_ref() else { return plan };
    let key_of = |f: &Found| work_key(project, f);
    // The project's own: with its label, and not known yet.
    let mut wanted: Vec<&Found> = found
        .iter()
        .filter(|f| match &f.source {
            Some(Source::Linear { .. }) => rule.linear_label.as_deref().is_some_and(|label| f.labelled(label)),
            Some(Source::Issue { .. }) => rule.github_label.as_deref().is_some_and(|label| f.labelled(label)),
            None => false,
        })
        .filter(|f| !taken(&key_of(f)))
        .collect();
    // An issue's text reaches a session only from a trusted author (D27).
    wanted.retain(|f| {
        let trusted = !matches!(f.source, Some(Source::Issue { .. })) || crate::gate::trusted_author(&f.author, f.association.as_deref(), person, confirmed);
        if !trusted {
            plan.held.push((f.id(), Held::Untrusted { author: f.author.clone() }));
        }
        trusted
    });
    if wanted.is_empty() {
        return plan;
    }
    wanted.sort_by_key(|f| f.order());
    let blocked = if policy.level != Level::Autopilot {
        Some(Held::NotAutopilot)
    } else if let Some(red) = load.red.first().filter(|_| rule.stop_on_red) {
        Some(Held::Red(red.clone()))
    } else if load.waiting_review >= rule.max_waiting {
        Some(Held::Waiting { waiting: load.waiting_review, max: rule.max_waiting })
    } else if load.tokens_today >= rule.daily_tokens {
        Some(Held::Budget { used: load.tokens_today, max: rule.daily_tokens })
    } else if load.automation_sessions >= rule.max_sessions {
        Some(Held::Cap { running: load.automation_sessions, max: rule.max_sessions })
    } else {
        None
    };
    if let Some(why) = blocked {
        plan.held.extend(wanted.iter().map(|f| (f.id(), why.clone())));
        plan.blocked = Some(why);
        return plan;
    }
    let free = (rule.max_sessions - load.automation_sessions) as usize;
    for (n, f) in wanted.into_iter().enumerate() {
        if n < free {
            plan.picks.push(Pick { found: f.clone(), key: key_of(f), done: done_conditions(), message: message(f) });
        } else {
            plan.held.push((f.id(), Held::Cap { running: rule.max_sessions, max: rule.max_sessions }));
        }
    }
    plan
}

/// The effect key of picking an item: one per project and item, ever.
pub fn effect(project: &ProjectKey, found: &Found) -> crate::effects::EffectKey {
    crate::effects::effect_key("intake", &found.id(), "session.new", project.as_str())
}

/// A pick's effect: claimed before its session starts, so an item is picked once, also across a
/// restart or a move of home.
pub fn intent(pick: &Pick, op: &str, launch: &str) -> crate::effects::Intent {
    let target = pick.key.project.as_str().to_string();
    crate::effects::Intent {
        origin: "intake".into(),
        subject: pick.found.id(),
        target: target.clone(),
        caller: "Glasscode's intake".into(),
        reason: crate::words::scrub(&pick.found.title.chars().take(80).collect::<String>()),
        project: Some(pick.key.project.clone()),
        launch: launch.to_string(),
        ..crate::effects::Intent::new(effect(&pick.key.project, &pick.found), op, crate::ops::Retry::AtMostOnce)
    }
}

/// The tokens a project's sessions used on `day` (`usage::local_day`): its sessions are those whose
/// folder is in one of `roots` (the project's folder and its worktrees).
pub fn tokens_on(report: &crate::usage::Report, day: &str, roots: &[std::path::PathBuf]) -> u64 {
    let ours = |cwd: &str| {
        let cwd = std::path::Path::new(cwd);
        roots.iter().any(|root| cwd.starts_with(root))
    };
    report.accounts.iter().flat_map(|a| a.sessions.iter()).filter(|s| ours(&s.cwd)).filter_map(|s| s.days.get(day)).map(|t| t.total()).sum()
}

/// A pull request that waits for a review: open (not a draft), and nobody approved it.
pub fn waits_for_review(pr: &crate::manager::PrInfo) -> bool {
    pr.state == "open" && !pr.reviewers.iter().any(|r| r.state.eq_ignore_ascii_case("APPROVED"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> ProjectKey {
        ProjectKey::parse("app-3f9a1c").unwrap()
    }

    fn policy(level: Level) -> ProjectPolicy {
        ProjectPolicy { level, intake: Some(Intake { linear_label: Some("glasscode".into()), github_label: Some("ready".into()), ..Intake::default() }), ..ProjectPolicy::default() }
    }

    fn ticket(id: &str, labels: &[&str], priority: u8, created: &str) -> Found {
        Found { source: Some(Source::Linear { id: format!("uuid-{id}"), identifier: id.into() }), title: format!("Fix {id}"), labels: labels.iter().map(|l| l.to_string()).collect(), priority, created: created.into(), ..Found::default() }
    }

    fn issue(number: u64, author: &str, association: &str, labels: &[&str]) -> Found {
        Found { source: Some(Source::Issue { repo: "acme/app".into(), number }), title: format!("Issue {number}"), body: "Steps to reproduce.".into(), author: author.into(), association: Some(association.into()), labels: labels.iter().map(|l| l.to_string()).collect(), created: "2026-11-01".into(), ..Found::default() }
    }

    #[test]
    fn autopilot_picks_labelled_work_from_trusted_authors() {
        let found = vec![
            ticket("ENG-2", &["glasscode"], 3, "2026-11-02"),
            ticket("ENG-1", &["Glasscode"], 1, "2026-11-05"),
            ticket("ENG-3", &["other"], 1, "2026-11-01"),
            issue(7, "stranger", "NONE", &["ready"]),
            issue(8, "teammate", "MEMBER", &["ready"]),
        ];
        let plan = plan(&project(), &policy(Level::Autopilot), &found, &Load::default(), &|_| false, &["robin".into()], &[]);
        // Two at most (the cap), the urgent one first; the issue waits for a free place.
        assert_eq!(plan.picks.iter().map(|p| p.key.id.as_str()).collect::<Vec<_>>(), ["ENG-1", "ENG-2"]);
        assert_eq!(plan.picks[0].done, vec![DoneCondition::PrReadyForReview, DoneCondition::CiGreen]);
        assert!(plan.picks[0].message.starts_with("Work on ENG-1: Fix ENG-1."));
        assert!(plan.held.contains(&("acme/app#7".to_string(), Held::Untrusted { author: "stranger".into() })), "{:?}", plan.held);
        assert!(plan.held.iter().any(|(id, why)| id == "acme/app#8" && matches!(why, Held::Cap { .. })));
        assert!(plan.held.iter().all(|(id, _)| id != "ENG-3"), "not the project's");
        // Known work is not picked again.
        let again = super::plan(&project(), &policy(Level::Autopilot), &found, &Load::default(), &|key| key.id == "ENG-1", &[], &[]);
        assert_eq!(again.picks.iter().map(|p| p.found.id()).collect::<Vec<_>>(), ["ENG-2", "acme/app#8"]);
        // An issue's work is the branch its session starts on.
        assert_eq!((again.picks[1].key.kind, again.picks[1].key.id.as_str()), (WorkKind::Branch, "8-issue-8"));
        assert_eq!(intent(&again.picks[1], "session.new", "run").key, effect(&project(), &again.picks[1].found));
    }

    #[test]
    fn nothing_without_autopilot_room_budget_or_green_ci() {
        let found = vec![ticket("ENG-1", &["glasscode"], 0, "2026-11-01")];
        let plan_with = |level: Level, load: Load| super::plan(&project(), &policy(level), &found, &load, &|_| false, &[], &[]);
        assert_eq!(plan_with(Level::Standard, Load::default()).blocked, Some(Held::NotAutopilot));
        assert_eq!(plan_with(Level::Autopilot, Load { red: vec!["acme/app#4".into()], ..Load::default() }).blocked, Some(Held::Red("acme/app#4".into())));
        assert_eq!(plan_with(Level::Autopilot, Load { waiting_review: 3, ..Load::default() }).blocked, Some(Held::Waiting { waiting: 3, max: 3 }));
        assert_eq!(plan_with(Level::Autopilot, Load { tokens_today: 3_000_000, ..Load::default() }).blocked, Some(Held::Budget { used: 3_000_000, max: 3_000_000 }));
        assert_eq!(plan_with(Level::Autopilot, Load { automation_sessions: 2, ..Load::default() }).blocked, Some(Held::Cap { running: 2, max: 2 }));
        let one_left = plan_with(Level::Autopilot, Load { automation_sessions: 1, waiting_review: 2, tokens_today: 10, ..Load::default() });
        assert_eq!((one_left.picks.len(), one_left.blocked), (1, None));
        // No rule, no intake; red CI does not stop it when the rule says so.
        let no_rule = super::plan(&project(), &ProjectPolicy { level: Level::Autopilot, ..ProjectPolicy::default() }, &found, &Load::default(), &|_| false, &[], &[]);
        assert_eq!(no_rule, Plan::default());
        let mut lenient = policy(Level::Autopilot);
        lenient.intake.as_mut().unwrap().stop_on_red = false;
        let red = super::plan(&project(), &lenient, &found, &Load { red: vec!["acme/app#4".into()], ..Load::default() }, &|_| false, &[], &[]);
        assert_eq!(red.picks.len(), 1);
    }

    #[test]
    fn issues_and_tickets_are_read() {
        let list = serde_json::json!([
            { "number": 12, "title": "Crash on start", "body": "It crashes.", "html_url": "https://github.com/acme/app/issues/12", "state": "open", "user": { "login": "sam" }, "author_association": "COLLABORATOR", "labels": [{ "name": "ready" }], "created_at": "2026-11-01T10:00:00Z" },
            { "number": 13, "title": "A pull request", "state": "open", "pull_request": {}, "user": { "login": "sam" }, "labels": [] }
        ]);
        let found = issues_from_json("acme/app", &list);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].id(), found[0].association.as_deref(), found[0].labels.clone()), ("acme/app#12".to_string(), Some("COLLABORATOR"), vec!["ready".to_string()]));
        let mut t = crate::linear::Ticket { id: "u1".into(), identifier: "ENG-9".into(), title: "Do it".into(), labels: vec!["glasscode".into()], ..Default::default() };
        assert_eq!(from_ticket(&t).map(|f| f.id()), Some("ENG-9".to_string()));
        t.provider = crate::model::Provider::GitHub;
        assert!(from_ticket(&t).is_none(), "an issue on the board is found as an issue");
        // The project's sessions' tokens today, by their folder.
        let mut days = std::collections::BTreeMap::new();
        days.insert("2026-11-20".to_string(), crate::usage::Tokens { input: 100, output: 50, ..Default::default() });
        let session = |cwd: &str| crate::usage::SessionUsage { id: "x".into(), cwd: cwd.into(), title: None, days: days.clone() };
        let report = crate::usage::Report { accounts: vec![crate::usage::AccountReport { sessions: vec![session("/work/app"), session("/work/app-wt/feat"), session("/work/other")], ..Default::default() }], ..Default::default() };
        assert_eq!(tokens_on(&report, "2026-11-20", &["/work/app".into(), "/work/app-wt".into()]), 300);
        assert!(Intake::default().describe().contains("nothing (no label set)"));
    }
}
