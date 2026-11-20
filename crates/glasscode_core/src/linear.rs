//! Linear, for the Tickets view: the tickets assigned to the person and the ones they review.
//! A team can mark a ticket's reviewer with a label ("Reviewer: Alex"), so "to review" is the
//! tickets with the person's reviewer label. Read through Linear's GraphQL API with a personal
//! API key the person pastes once: it stays in the macOS Keychain, and requests go through the
//! system curl with their headers on stdin (never on a command line). What was fetched last is
//! kept in `~/.glasscode/linear.json`, so the view opens at once and works offline.

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::AgentKind;
use crate::paths;

pub const API: &str = "https://api.linear.app/graphql";
/// Where a person makes a personal API key.
pub const KEY_PAGE: &str = "https://linear.app/settings/account/security";
const KEYCHAIN_SERVICE: &str = "Glasscode Linear";
pub(crate) const KEYCHAIN_ACCOUNT: &str = "api-key";
/// Done tickets stay on the board this long after they are done.
const DONE_FOR_SECS: u64 = 14 * 86400;
/// A session's first message carries the ticket, up to this much of it.
const MESSAGE_LIMIT: usize = 16_000;
pub const PRIORITY_NAMES: [&str; 5] = ["No priority", "Urgent", "High", "Medium", "Low"];

/// What the person chose in Settings › Integrations › Linear (in `state.json`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LinearSettings {
    /// Whose tickets are "Assigned to me" (a Linear user id); none: the key's own user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub me: Option<String>,
    /// That person's email (or name), for showing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub me_label: Option<String>,
    /// The label that marks tickets the person reviews; none: "Reviewer: <their first name>".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer_label: Option<String>,
    /// Pull requests that ask for the person's review are left out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub review_prs_off: bool,
    /// Starting a session leaves the ticket's status alone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub move_on_start_off: bool,
    /// No notification when a ticket is assigned or a review is asked for.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub notify_off: bool,
    /// Who drives sessions started from tickets (Claude unless chosen).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentKind>,
}

impl LinearSettings {
    /// The reviewer label in effect for a person called `name`.
    pub fn label_for(&self, name: &str) -> Option<String> {
        self.reviewer_label.clone().or_else(|| name.split_whitespace().next().filter(|n| !n.contains('@')).map(|first| format!("Reviewer: {first}")))
    }
}

// ----- the key -----------------------------------------------------------------------

pub fn api_url() -> String {
    std::env::var("GLASSCODE_LINEAR_API").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| API.to_string())
}

/// The saved API key (tests: `GLASSCODE_LINEAR_KEY_FILE`).
pub fn key() -> Option<String> {
    if let Ok(path) = std::env::var("GLASSCODE_LINEAR_KEY_FILE") {
        return std::fs::read_to_string(path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    }
    crate::keychain::read(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
}

pub fn save_key(key: &str) -> anyhow::Result<()> {
    let key = key.trim();
    if key.is_empty() || key.chars().any(char::is_whitespace) {
        bail!("that does not look like a Linear API key");
    }
    if let Ok(path) = std::env::var("GLASSCODE_LINEAR_KEY_FILE") {
        std::fs::write(path, key)?;
        return Ok(());
    }
    crate::keychain::write(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, key).with_context(|| format!("could not save the Linear key to {}", crate::keychain::NAME))
}

pub fn forget_key() {
    if let Ok(path) = std::env::var("GLASSCODE_LINEAR_KEY_FILE") {
        let _ = std::fs::remove_file(path);
        return;
    }
    crate::keychain::delete(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT);
}

// ----- what Linear says ----------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Person {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub email: String,
}

impl Person {
    /// Two letters for an avatar.
    pub fn initials(&self) -> String {
        let words: Vec<&str> = self.name.split(|c: char| c.is_whitespace() || c == '.' || c == '@').filter(|w| !w.is_empty()).collect();
        let letters: String = match words.as_slice() {
            [one] => one.chars().take(2).collect(),
            [first, second, ..] => first.chars().take(1).chain(second.chars().take(1)).collect(),
            [] => "?".into(),
        };
        letters.to_uppercase()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub name: String,
    /// Linear's kind: triage, backlog, unstarted, started, completed or canceled.
    pub kind: String,
    #[serde(default)]
    pub position: f64,
}

impl Status {
    /// Board order: what is being worked on first, done last.
    pub fn order(&self) -> u8 {
        match self.kind.as_str() {
            "started" => 0,
            "unstarted" => 1,
            "triage" => 2,
            "backlog" => 3,
            "completed" => 4,
            _ => 5,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    pub url: String,
    #[serde(default)]
    pub source: String,
}

impl Attachment {
    /// A GitHub pull request: `(owner/name, number)`.
    pub fn pull_request(&self) -> Option<(String, u64)> {
        let rest = self.url.strip_prefix("https://github.com/")?;
        let parts: Vec<&str> = rest.split('/').collect();
        match parts.as_slice() {
            [owner, name, "pull", number, ..] => Some((format!("{owner}/{name}"), number.parse().ok()?)),
            _ => None,
        }
    }

    /// A linked remote task's id. Nothing links one yet, so this is always None.
    pub fn team_task(&self) -> Option<String> {
        None
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Ticket {
    /// Linear's own id (for API calls).
    pub id: String,
    /// "ENG-787".
    pub identifier: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// 0 none, 1 urgent, 2 high, 3 medium, 4 low.
    #[serde(default)]
    pub priority: u8,
    pub url: String,
    #[serde(default)]
    pub branch: String,
    /// "2026-10-23".
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub completed_at: Option<String>,
    pub status: Status,
    #[serde(default)]
    pub assignee: Option<Person>,
    #[serde(default)]
    pub creator: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub parent: Option<(String, String)>,
    #[serde(default)]
    pub team: String,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// Which backend this ticket came from (Linear unless a GitHub issue).
    #[serde(default)]
    pub provider: crate::model::Provider,
}

/// One pickable project for the Tickets header's project selector: a Linear team or a GitHub
/// repository. `id` is what gets stored (a team key, or an `owner/name`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectOption {
    pub id: String,
    pub label: String,
    pub detail: String,
}

impl Ticket {
    pub fn priority_name(&self) -> &'static str {
        PRIORITY_NAMES.get(self.priority as usize).copied().unwrap_or(PRIORITY_NAMES[0])
    }

    /// Its GitHub pull requests, open or not.
    pub fn pull_requests(&self) -> Vec<(String, u64)> {
        let mut out: Vec<(String, u64)> = Vec::new();
        for pr in self.attachments.iter().filter_map(Attachment::pull_request) {
            if !out.contains(&pr) {
                out.push(pr);
            }
        }
        out
    }

    pub fn team_task(&self) -> Option<String> {
        self.attachments.iter().find_map(Attachment::team_task)
    }

    pub fn is_done(&self) -> bool {
        matches!(self.status.kind.as_str(), "completed" | "canceled")
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub author: String,
    pub body: String,
    pub created_at: String,
    /// An answer in a thread.
    #[serde(default)]
    pub reply: bool,
}

/// What a ticket's page adds to the board: its comments, and its team's statuses.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Detail {
    pub description: String,
    pub comments: Vec<Comment>,
    /// The team's statuses: (id, status).
    pub states: Vec<(String, Status)>,
}

impl Detail {
    /// The team's first "started" status (In Progress), to move a ticket to.
    pub fn started_state(&self) -> Option<&(String, Status)> {
        self.states.iter().filter(|(_, s)| s.kind == "started").min_by(|a, b| a.1.position.total_cmp(&b.1.position))
    }
}

/// Everything the Tickets view shows from Linear, as fetched at `fetched_at`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Who the key belongs to.
    pub viewer: Person,
    pub org: String,
    /// The person the tickets were fetched for, and the reviewer label used.
    pub me: String,
    #[serde(default)]
    pub label: Option<String>,
    pub mine: Vec<Ticket>,
    pub reviewing: Vec<Ticket>,
    pub fetched_at: u64,
}

impl Snapshot {
    pub fn ticket(&self, identifier: &str) -> Option<&Ticket> {
        self.mine.iter().chain(self.reviewing.iter()).find(|t| t.identifier.eq_ignore_ascii_case(identifier))
    }
}

fn cache_path() -> std::path::PathBuf {
    paths::root().join("linear.json")
}

pub fn load_cache() -> Option<Snapshot> {
    serde_json::from_slice(&std::fs::read(cache_path()).ok()?).ok()
}

pub fn save_cache(snapshot: &Snapshot) {
    let _ = paths::ensure_dirs();
    if let Ok(bytes) = serde_json::to_vec(snapshot) {
        let _ = std::fs::write(cache_path(), bytes);
    }
}

pub fn forget_cache() {
    let _ = std::fs::remove_file(cache_path());
}

// ----- the API -------------------------------------------------------------------------

const TICKET_FIELDS: &str = "id identifier title description priority url branchName dueDate createdAt updatedAt completedAt \
state { name type position } assignee { id name displayName email } creator { name displayName } \
labels(first: 20) { nodes { name } } parent { identifier title } team { key } \
attachments(first: 20) { nodes { title subtitle url sourceType } }";

pub struct Client {
    key: String,
    url: String,
}

impl Client {
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into(), url: api_url() }
    }

    /// The saved key's client, if there is a key.
    pub fn saved() -> Option<Self> {
        key().map(Self::new)
    }

    fn post(&self, query: &str, variables: Value) -> anyhow::Result<Value> {
        let body = serde_json::to_vec(&json!({ "query": query, "variables": variables }))?;
        let headers = [("Authorization", self.key.clone()), ("Content-Type", "application/json".to_string())];
        let (status, bytes) = crate::net::http("POST", &self.url, &headers, Some(body))?;
        let value: Value = serde_json::from_slice(&bytes).with_context(|| format!("Linear answered {status} with something that is not JSON"))?;
        let errors = value["errors"].as_array().cloned().unwrap_or_default();
        let refused = status == 401 || errors.iter().any(|e| e["extensions"]["code"].as_str() == Some("AUTHENTICATION_ERROR"));
        if refused {
            bail!("Linear refused the key: it may have been revoked. Paste a new one in Settings › Integrations › Linear.");
        }
        if let Some(first) = errors.first() {
            bail!("Linear: {}", first["message"].as_str().unwrap_or("unknown error"));
        }
        if status >= 400 {
            bail!("Linear answered {status}");
        }
        Ok(value["data"].clone())
    }

    /// Who the key belongs to, and their workspace's name.
    pub fn viewer(&self) -> anyhow::Result<(Person, String)> {
        let data = self.post("query Me { viewer { id name displayName email } organization { name } }", json!({}))?;
        Ok((person(&data["viewer"]).context("Linear did not say who the key belongs to")?, text(&data["organization"]["name"])))
    }

    /// The workspace's active members (for "You are").
    pub fn people(&self) -> anyhow::Result<Vec<Person>> {
        let query = "query People($after: String) { users(first: 250, after: $after, filter: { active: { eq: true } }) { nodes { id name displayName email } pageInfo { hasNextPage endCursor } } }";
        let mut out: Vec<Person> = self.pages(query, json!({}), "users", 4)?.iter().filter_map(person).collect();
        out.sort_by_key(|p| p.name.to_lowercase());
        Ok(out)
    }

    /// The workspace's reviewer labels ("Reviewer: Alex", …).
    pub fn reviewer_labels(&self) -> anyhow::Result<Vec<String>> {
        let query = "query Labels($after: String) { issueLabels(first: 250, after: $after, filter: { name: { startsWithIgnoreCase: \"reviewer\" } }) { nodes { name } pageInfo { hasNextPage endCursor } } }";
        let mut names: Vec<String> = self.pages(query, json!({}), "issueLabels", 4)?.iter().map(|n| text(&n["name"])).filter(|n| !n.is_empty()).collect();
        names.sort();
        names.dedup();
        Ok(names)
    }

    fn pages(&self, query: &str, mut variables: Value, field: &str, max: usize) -> anyhow::Result<Vec<Value>> {
        let mut out = Vec::new();
        for _ in 0..max {
            let data = self.post(query, variables.clone())?;
            out.extend(data[field]["nodes"].as_array().cloned().unwrap_or_default());
            let page = &data[field]["pageInfo"];
            if !page["hasNextPage"].as_bool().unwrap_or(false) {
                break;
            }
            variables["after"] = page["endCursor"].clone();
        }
        Ok(out)
    }

    fn tickets(&self, filter: Value) -> anyhow::Result<Vec<Ticket>> {
        let query = format!("query Tickets($filter: IssueFilter!, $after: String) {{ issues(first: 100, after: $after, filter: $filter, orderBy: updatedAt) {{ nodes {{ {TICKET_FIELDS} }} pageInfo {{ hasNextPage endCursor }} }} }}");
        Ok(self.pages(&query, json!({ "filter": filter }), "issues", 3)?.iter().filter_map(ticket).collect())
    }

    /// The person's tickets and the ones with their reviewer label: open ones, and the ones
    /// done in the last two weeks.
    pub fn snapshot(&self, settings: &LinearSettings, now: u64, team: Option<&str>) -> anyhow::Result<Snapshot> {
        let (viewer, org) = self.viewer()?;
        let me = settings.me.clone().unwrap_or_else(|| viewer.id.clone());
        let name = if settings.me.is_some() { settings.me_label.clone().unwrap_or_default() } else { viewer.name.clone() };
        let label = settings.label_for(&name);
        let mine = self.tickets(filters::assigned(&me, now, team))?;
        let reviewing = match &label {
            Some(label) => self.tickets(filters::labelled(label, now, team))?,
            None => Vec::new(),
        };
        Ok(Snapshot { viewer, org, me, label, mine, reviewing, fetched_at: now })
    }

    /// The workspace's teams, for the project selector: (key, name).
    pub fn teams(&self) -> anyhow::Result<Vec<ProjectOption>> {
        let query = "query Teams($after: String) { teams(first: 250, after: $after) { nodes { key name } pageInfo { hasNextPage endCursor } } }";
        let mut out: Vec<ProjectOption> = self
            .pages(query, json!({}), "teams", 4)?
            .iter()
            .filter_map(|t| {
                let key = t["key"].as_str()?.to_string();
                (!key.is_empty()).then(|| ProjectOption { label: format!("{} ({key})", text(&t["name"])), detail: String::new(), id: key })
            })
            .collect();
        out.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
        Ok(out)
    }

    /// A ticket's comments and its team's statuses.
    pub fn detail(&self, id: &str) -> anyhow::Result<Detail> {
        let query = "query TicketDetail($id: String!) { issue(id: $id) { id description \
comments(first: 100) { nodes { id body createdAt user { name displayName } botActor { name } parent { id } } } \
team { states { nodes { id name type position } } } } }";
        let data = self.post(query, json!({ "id": id }))?;
        let issue = &data["issue"];
        if issue.is_null() {
            bail!("Linear has no ticket {id}");
        }
        let mut comments: Vec<Comment> = issue["comments"]["nodes"]
            .as_array()
            .map(|nodes| {
                nodes
                    .iter()
                    .map(|c| Comment {
                        author: [&c["user"]["displayName"], &c["user"]["name"], &c["botActor"]["name"]].iter().map(|v| text(v)).find(|s| !s.is_empty()).unwrap_or_else(|| "Linear".into()),
                        body: text(&c["body"]),
                        created_at: text(&c["createdAt"]),
                        reply: !c["parent"].is_null(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        comments.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let states = issue["team"]["states"]["nodes"]
            .as_array()
            .map(|nodes| nodes.iter().filter_map(|s| Some((s["id"].as_str()?.to_string(), status(s)))).collect())
            .unwrap_or_default();
        Ok(Detail { description: text(&issue["description"]), comments, states })
    }

    pub fn comment(&self, issue_id: &str, body: &str) -> anyhow::Result<()> {
        let data = self.post("mutation Comment($issueId: String!, $body: String!) { commentCreate(input: { issueId: $issueId, body: $body }) { success } }", json!({ "issueId": issue_id, "body": body }))?;
        if !data["commentCreate"]["success"].as_bool().unwrap_or(false) {
            bail!("Linear did not take the comment");
        }
        Ok(())
    }

    pub fn set_status(&self, issue_id: &str, state_id: &str) -> anyhow::Result<()> {
        let data = self.post("mutation Move($id: String!, $stateId: String!) { issueUpdate(id: $id, input: { stateId: $stateId }) { success } }", json!({ "id": issue_id, "stateId": state_id }))?;
        if !data["issueUpdate"]["success"].as_bool().unwrap_or(false) {
            bail!("Linear did not change the status");
        }
        Ok(())
    }
}

/// The filters the board's two queries use.
pub mod filters {
    use serde_json::{Value, json};

    /// Open, or done in the last two weeks.
    fn recent(now: u64) -> Value {
        json!([
            { "state": { "type": { "nin": ["completed", "canceled"] } } },
            { "completedAt": { "gt": super::iso(now.saturating_sub(super::DONE_FOR_SECS)) } }
        ])
    }

    pub fn assigned(user_id: &str, now: u64, team: Option<&str>) -> Value {
        with_team(json!({ "assignee": { "id": { "eq": user_id } }, "or": recent(now) }), team)
    }

    pub fn labelled(label: &str, now: u64, team: Option<&str>) -> Value {
        with_team(json!({ "labels": { "name": { "eq": label } }, "or": recent(now) }), team)
    }

    /// Narrow a filter to one team (by its key, e.g. "ENG"), when one is chosen.
    fn with_team(mut filter: Value, team: Option<&str>) -> Value {
        if let Some(team) = team.filter(|t| !t.is_empty()) {
            filter["team"] = json!({ "key": { "eq": team } });
        }
        filter
    }
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

fn person(value: &Value) -> Option<Person> {
    let id = value["id"].as_str()?.to_string();
    let display = text(&value["displayName"]);
    let name = text(&value["name"]);
    // Linear's `name` is often the email for people who never set one; `displayName` is short.
    let name = if name.is_empty() || name.contains('@') { if display.is_empty() { name } else { display } } else { name };
    Some(Person { id, name, email: text(&value["email"]) })
}

fn status(value: &Value) -> Status {
    Status { name: text(&value["name"]), kind: text(&value["type"]), position: value["position"].as_f64().unwrap_or(0.) }
}

fn ticket(value: &Value) -> Option<Ticket> {
    let nodes = |field: &str| value[field]["nodes"].as_array().cloned().unwrap_or_default();
    let parent = (!value["parent"].is_null()).then(|| (text(&value["parent"]["identifier"]), text(&value["parent"]["title"])));
    let creator = [&value["creator"]["displayName"], &value["creator"]["name"]].iter().map(|v| text(v)).find(|s| !s.is_empty());
    Some(Ticket {
        id: value["id"].as_str()?.to_string(),
        identifier: value["identifier"].as_str()?.to_string(),
        title: text(&value["title"]),
        description: text(&value["description"]),
        priority: value["priority"].as_f64().unwrap_or(0.) as u8,
        url: text(&value["url"]),
        branch: text(&value["branchName"]),
        due: value["dueDate"].as_str().map(str::to_string),
        created_at: text(&value["createdAt"]),
        updated_at: text(&value["updatedAt"]),
        completed_at: value["completedAt"].as_str().map(str::to_string),
        status: status(&value["state"]),
        assignee: person(&value["assignee"]),
        creator,
        labels: nodes("labels").iter().map(|l| text(&l["name"])).filter(|n| !n.is_empty()).collect(),
        parent,
        team: text(&value["team"]["key"]),
        attachments: nodes("attachments")
            .iter()
            .map(|a| Attachment { title: text(&a["title"]), subtitle: text(&a["subtitle"]), url: text(&a["url"]), source: text(&a["sourceType"]) })
            .filter(|a| !a.url.is_empty())
            .collect(),
        provider: crate::model::Provider::Linear,
    })
}

/// `2026-10-06T00:00:00Z` for a Unix time.
pub fn iso(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rest = secs % 86400;
    // Civil from days (Howard Hinnant).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest / 60 % 60, rest % 60)
}

/// "Oct 8" for an ISO date or time.
pub fn short_date(stamp: &str) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let mut parts = stamp.get(..10).unwrap_or(stamp).split('-');
    let (_, month, day) = (parts.next(), parts.next().and_then(|m| m.parse::<usize>().ok()), parts.next().and_then(|d| d.parse::<u32>().ok()));
    match (month, day) {
        (Some(m @ 1..=12), Some(d)) => format!("{} {d}", MONTHS[m - 1]),
        _ => stamp.to_string(),
    }
}

/// Days from `now` until an ISO date (negative once it is past).
pub fn days_until(date: &str, now: u64) -> Option<i64> {
    let at = crate::github::parse_time(&format!("{}T00:00:00Z", date.get(..10)?))?;
    Some((at - now as i64).div_euclid(86400) + i64::from((at - now as i64).rem_euclid(86400) > 0))
}

/// Linear's Markdown as plain Markdown: its `<issue href=…>ENG-1</issue>` and
/// `<pull-request href=…>acme/app#7</pull-request>` mentions become links (a pull
/// request's to GitHub), and inline `<linear-image>` data goes.
pub fn markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let tag = ["issue", "pull-request", "linear-image"].into_iter().find(|t| tail.starts_with(&format!("<{t}")) && tail[t.len() + 1..].starts_with([' ', '>']));
        let Some(tag) = tag else {
            out.push('<');
            rest = &tail[1..];
            continue;
        };
        let close = format!("</{tag}>");
        let (Some(open_end), Some(close_at)) = (tail.find('>'), tail.find(&close)) else {
            out.push('<');
            rest = &tail[1..];
            continue;
        };
        if close_at > open_end && tag != "linear-image" {
            let label = &tail[open_end + 1..close_at];
            let href = tail[..open_end].split("href=\"").nth(1).and_then(|h| h.split('"').next()).unwrap_or_default();
            let href = match label.split_once('#') {
                Some((repo, number)) if tag == "pull-request" && repo.contains('/') && number.chars().all(|c| c.is_ascii_digit()) => format!("https://github.com/{repo}/pull/{number}"),
                _ => href.to_string(),
            };
            // Show the mention as written (e.g. "owner/app#701").
            out.push_str(&format!("[{label}]({href})"));
        }
        rest = &tail[(close_at + close.len()).max(open_end + 1)..];
    }
    out.push_str(rest);
    out
}

// ----- tickets and work ------------------------------------------------------------------

/// Ticket identifiers in a branch, a title or a sentence: "feat/eng-787-workspace" and
/// "add team boards (ENG-725)" both name one. Upper case, in order, once each.
pub fn identifiers_in(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let starts_word = i == 0 || !chars[i - 1].is_ascii_alphanumeric();
        if !(starts_word && chars[i].is_ascii_alphabetic()) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() && chars[j].is_ascii_alphanumeric() && j - i < 10 {
            j += 1;
        }
        let key_len = j - i;
        if key_len >= 2 && chars.get(j) == Some(&'-') && chars.get(j + 1).is_some_and(char::is_ascii_digit) {
            let mut k = j + 1;
            while k < chars.len() && chars[k].is_ascii_digit() {
                k += 1;
            }
            let ends_word = k == chars.len() || !chars[k].is_ascii_alphanumeric() || chars[k] == '-';
            let key: String = chars[i..j].iter().collect();
            if ends_word && key.starts_with(|c: char| c.is_ascii_alphabetic()) && k - j - 1 <= 6 {
                let id = format!("{}-{}", key.to_uppercase(), chars[j + 1..k].iter().collect::<String>());
                if !out.contains(&id) {
                    out.push(id);
                }
            }
            i = k;
            continue;
        }
        i = j.max(i + 1);
    }
    out
}

/// Whether a branch (or title) is about this ticket.
pub fn mentions(text: &str, identifier: &str) -> bool {
    identifiers_in(text).iter().any(|id| id.eq_ignore_ascii_case(identifier))
}

/// A branch for work on a ticket: its ID and three words of its title, so Linear links the
/// pull request to it ("eng-826-calendar-integrations").
pub fn branch_for(identifier: &str, title: &str) -> String {
    const SKIP: [&str; 14] = ["a", "an", "the", "and", "of", "to", "for", "in", "on", "with", "from", "by", "do", "is"];
    let words: Vec<String> = short_title(title, 80)
        .to_lowercase()
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .map(|w| w.trim_matches('-').to_string())
        .filter(|w| !w.is_empty() && !SKIP.contains(&w.as_str()))
        .take(3)
        .collect();
    let mut branch = identifier.to_lowercase();
    if !words.is_empty() {
        branch.push('-');
        branch.push_str(&words.join("-"));
    }
    branch.truncate(60);
    branch.trim_end_matches('-').to_string()
}

/// A ticket's title as a session's name: no "(…)" tail or "— …" second half, cut at a word.
pub fn short_title(title: &str, max: usize) -> String {
    let mut t = title.trim().to_string();
    if t.ends_with(')') {
        if let Some(open) = t.rfind(" (") {
            t.truncate(open);
        }
    }
    if let Some((head, _)) = t.split_once(" — ") {
        t = head.to_string();
    }
    crate::naming::tidy_title(&t, max)
}

/// A session's first message: what to do, then the ticket itself (so the agent needs no
/// Linear access), cut to a size a terminal paste takes well.
pub fn first_message(instruction: &str, ticket: &Ticket, detail: Option<&Detail>) -> String {
    let mut ticket_text = format!("{}: {}\n{}\nStatus: {} · Priority: {}", ticket.identifier, ticket.title, ticket.url, ticket.status.name, ticket.priority_name());
    if !ticket.labels.is_empty() {
        ticket_text.push_str(&format!(" · Labels: {}", ticket.labels.join(", ")));
    }
    if let Some(due) = &ticket.due {
        ticket_text.push_str(&format!(" · Due {due}"));
    }
    for (repo, number) in ticket.pull_requests() {
        ticket_text.push_str(&format!("\nPull request: {repo}#{number}"));
    }
    let description = detail.map(|d| d.description.as_str()).filter(|d| !d.is_empty()).unwrap_or(&ticket.description);
    if !description.trim().is_empty() {
        ticket_text.push_str("\n\n");
        ticket_text.push_str(description.trim());
    }
    if let Some(detail) = detail.filter(|d| !d.comments.is_empty()) {
        ticket_text.push_str("\n\nComments, oldest first:");
        for comment in &detail.comments {
            ticket_text.push_str(&format!("\n\n{} ({}):\n{}", comment.author, short_date(&comment.created_at), comment.body.trim()));
        }
    }
    let head = format!("{}\n\n--- The ticket, from Linear ---\n", instruction.trim());
    let room = MESSAGE_LIMIT.saturating_sub(head.len() + 80);
    if ticket_text.len() > room {
        let mut cut = room;
        while !ticket_text.is_char_boundary(cut) {
            cut -= 1;
        }
        ticket_text.truncate(cut);
        ticket_text.push_str(&format!("\n\n… (cut here: the rest is at {})", ticket.url));
    }
    format!("{head}{ticket_text}\n--- End of the ticket ---")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_ticket_ids() {
        assert_eq!(identifiers_in("feat/eng-787-workspace-admin-consent"), vec!["ENG-787"]);
        assert_eq!(identifiers_in("feat: add team boards (ENG-725), see ENG-724"), vec!["ENG-725", "ENG-724"]);
        assert_eq!(identifiers_in("eng-782-export-connector-fixes"), vec!["ENG-782"]);
        assert!(identifiers_in("glasscode/session-9ade").is_empty());
        assert!(identifiers_in("review/658").is_empty());
        assert!(identifiers_in("x-1 and 2-3").is_empty());
        assert!(mentions("fix/ENG-713-api-keys", "eng-713"));
        assert!(!mentions("eng-7130-x", "ENG-713"));
    }

    #[test]
    fn names_branches_and_sessions() {
        assert_eq!(branch_for("ENG-826", "Calendar sync integrations: events, tasks and reminders (feasibility and effort assessment first)"), "eng-826-calendar-sync-integrations");
        assert_eq!(branch_for("ENG-787", "Settings panel: per-site \"Show Acme button\""), "eng-787-settings-panel-per-site");
        assert_eq!(short_title("Data pipeline IQ MCP server cannot connect — SSO rejects the resource parameter (AADSTS9010010)", 46), "Data pipeline IQ MCP server cannot connect");
    }

    #[test]
    fn dates() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_792_454_400), "2026-10-20T00:00:00Z");
        assert_eq!(short_date("2026-10-08T10:00:00.000Z"), "Oct 8");
        assert_eq!(short_date("2026-12-31"), "Dec 31");
        assert_eq!(days_until("2026-10-23", 1_792_454_400), Some(3));
        assert_eq!(days_until("2026-10-20", 1_792_454_400 + 3600), Some(0));
    }

    #[test]
    fn reads_tickets() {
        let node = json!({
            "id": "u1", "identifier": "ENG-787", "title": "Settings panel", "description": "Body", "priority": 2,
            "url": "https://linear.app/acme/issue/ENG-787", "branchName": "eng-787", "dueDate": null,
            "createdAt": "2026-10-08T10:00:00Z", "updatedAt": "2026-10-20T11:00:00Z", "completedAt": null,
            "state": { "name": "In Progress", "type": "started", "position": 2 },
            "assignee": { "id": "me", "name": "alex@glasscode.invalid", "displayName": "alex", "email": "alex@glasscode.invalid" },
            "creator": { "name": "Alex", "displayName": "alex" },
            "labels": { "nodes": [{ "name": "Reviewer: Jordan" }] }, "parent": null, "team": { "key": "ENG" },
            "attachments": { "nodes": [
                { "title": "PR #961", "subtitle": null, "url": "https://github.com/acme/app/pull/961", "sourceType": "github" }
            ] }
        });
        let t = ticket(&node).unwrap();
        assert_eq!((t.identifier.as_str(), t.priority_name(), t.status.order()), ("ENG-787", "High", 0));
        assert_eq!(t.assignee.as_ref().unwrap().name, "alex");
        assert_eq!(t.pull_requests(), vec![("acme/app".to_string(), 961)]);
        assert_eq!(t.team_task(), None);
        assert_eq!(Person { id: "x".into(), name: "Alex Rivera".into(), email: String::new() }.initials(), "AR");
    }

    #[test]
    fn filters_and_labels() {
        let f = filters::labelled("Reviewer: Alex", 1_792_454_400, None);
        assert_eq!(f["labels"]["name"]["eq"], "Reviewer: Alex");
        assert_eq!(f["or"][1]["completedAt"]["gt"], "2026-10-06T00:00:00Z");
        assert_eq!(filters::assigned("me", 0, None)["assignee"]["id"]["eq"], "me");
        assert_eq!(filters::assigned("me", 0, Some("ENG"))["team"]["key"]["eq"], "ENG");
        let settings = LinearSettings::default();
        assert_eq!(settings.label_for("Alex Rivera").as_deref(), Some("Reviewer: Alex"));
        assert_eq!(settings.label_for("alex@glasscode.invalid"), None);
    }

    #[test]
    fn turns_mentions_into_links() {
        let text = "Split out of <issue id=\"a4\" href=\"https://linear.app/acme/issue/ENG-577/x\">ENG-577</issue>, shipped in <pull-request id=\"3b\" href=\"https://linear.app/acme/review/y\">acme/app#701</pull-request>. a < b <linear-image>{\"src\":1}</linear-image>end";
        assert_eq!(markdown(text), "Split out of [ENG-577](https://linear.app/acme/issue/ENG-577/x), shipped in [acme/app#701](https://github.com/acme/app/pull/701). a < b end");
    }

    #[test]
    fn first_message_carries_the_ticket() {
        let t = Ticket { identifier: "ENG-826".into(), title: "Calendar".into(), url: "https://linear.app/x".into(), description: "x".repeat(40_000), ..Default::default() };
        let m = first_message("Work on ENG-826.", &t, None);
        assert!(m.starts_with("Work on ENG-826.\n\n--- The ticket, from Linear ---\nENG-826: Calendar"));
        assert!(m.len() <= MESSAGE_LIMIT + 200 && m.contains("the rest is at https://linear.app/x") && m.ends_with("--- End of the ticket ---"));
    }
}
