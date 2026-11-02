//! GitHub Issues as a Tickets-board provider, alongside Linear. Reads the person's issues in a
//! repository through the `gh` CLI (reusing `github`'s helpers and auth), and shapes them into
//! the same `linear::Ticket`/`Status`/`Snapshot`/`Detail` the board already draws — so the
//! kanban renders identically whichever provider is in effect. GitHub has no statuses, so a
//! column is a `status:…` label (plus open/closed), and "In Review" also lights up when an
//! issue has an open linked pull request.

use std::collections::HashMap;

use anyhow::Context as _;
use serde_json::Value;

use crate::github::{gh_json, gh_run, parse_time};
use crate::linear::{Comment, Detail, Person, ProjectOption, Snapshot, Status, Ticket};
use crate::model::Provider;

/// Closed issues stay on the board this long after they close (matches Linear's two weeks).
const DONE_FOR_SECS: i64 = 14 * 86400;
/// The label that marks an issue's column, e.g. `status:in-progress`. Overridable per repo with
/// the `glasscode.ticketStatusPrefix` git setting.
const DEFAULT_PREFIX: &str = "status:";

/// The board's columns, as (column name, the `status:` suffix that puts an issue there).
/// "Todo" is the default (no status label); "Done" is closed, handled separately.
const COLUMNS: [(&str, &str); 4] = [("In Progress", "in-progress"), ("In Review", "in-review"), ("Triage", "triage"), ("Backlog", "backlog")];

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

/// The `status:` label prefix for a repository (its `glasscode.ticketStatusPrefix`, else the
/// default). A repo slug has no path to read git config from, so this is best-effort global.
pub fn status_prefix() -> String {
    std::env::var("GLASSCODE_TICKET_STATUS_PREFIX").ok().filter(|p| !p.is_empty()).unwrap_or_else(|| DEFAULT_PREFIX.to_string())
}

/// Build the board's `Status` for an issue from whether it is closed, its labels, and whether it
/// has an open linked pull request. The synthesized kind/position reuse Linear's so the board's
/// ordering (Asking for your review → In Progress → In Review → Todo → Triage → Backlog) and
/// colours come out identical.
pub fn status_for(closed: bool, labels: &[String], has_open_pr: bool, prefix: &str) -> Status {
    if closed {
        return Status { name: "Done".into(), kind: "completed".into(), position: 0. };
    }
    let has = |suffix: &str| labels.iter().any(|l| l.eq_ignore_ascii_case(&format!("{prefix}{suffix}")));
    // In Progress and In Review are both "started" (position orders them); the rest map to their
    // own kinds. A label wins; otherwise an open linked PR means In Review, else Todo.
    if has("in-progress") {
        Status { name: "In Progress".into(), kind: "started".into(), position: 0. }
    } else if has("in-review") || has_open_pr {
        Status { name: "In Review".into(), kind: "started".into(), position: 1. }
    } else if has("triage") {
        Status { name: "Triage".into(), kind: "triage".into(), position: 0. }
    } else if has("backlog") {
        Status { name: "Backlog".into(), kind: "backlog".into(), position: 0. }
    } else {
        Status { name: "Todo".into(), kind: "unstarted".into(), position: 0. }
    }
}

/// Issue numbers a branch, a title or a sentence names: `#742`, `issue-742`, `gh-742`, or a
/// branch that starts `742-…` (GitHub's own branch-from-issue format). Never matches a Linear
/// id (letters-dash-digits) or a `review/7` branch. In order, once each.
pub fn issue_refs(text: &str) -> Vec<u64> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<u64> = Vec::new();
    let mut push = |n: u64, out: &mut Vec<u64>| {
        if n > 0 && !out.contains(&n) {
            out.push(n);
        }
    };
    let lower = text.to_lowercase();
    // #NNN, issue-NNN, gh-NNN (each at a word boundary).
    for (marker, keep_hash) in [("#", true), ("issue-", false), ("gh-", false)] {
        let mut from = 0;
        while let Some(rel) = lower[from..].find(marker) {
            let at = from + rel;
            let before_ok = at == 0 || !lower.as_bytes()[at - 1].is_ascii_alphanumeric();
            let digits_at = at + marker.len();
            let end = digits_at + lower[digits_at..].chars().take_while(|c| c.is_ascii_digit()).count();
            if before_ok && end > digits_at {
                if let Ok(n) = lower[digits_at..end].parse::<u64>() {
                    let _ = keep_hash;
                    push(n, &mut out);
                }
            }
            from = at + marker.len();
        }
    }
    // A leading `NNN-…` segment: start of string, or just after a `/`.
    let mut i = 0;
    while i < chars.len() {
        let seg_start = i == 0 || chars[i - 1] == '/';
        if seg_start && chars[i].is_ascii_digit() {
            let mut j = i;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            if chars.get(j) == Some(&'-') {
                if let Ok(n) = chars[i..j].iter().collect::<String>().parse::<u64>() {
                    push(n, &mut out);
                }
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

/// Whether a branch (or title) is about this issue number.
pub fn mentions(text: &str, number: u64) -> bool {
    issue_refs(text).contains(&number)
}

/// A branch for work on an issue: its number and three words of its title, matching GitHub's own
/// "create a branch" default so a merged PR auto-closes the issue ("742-calendar-sync").
pub fn branch_for_issue(number: u64, title: &str) -> String {
    const SKIP: [&str; 14] = ["a", "an", "the", "and", "of", "to", "for", "in", "on", "with", "from", "by", "do", "is"];
    let words: Vec<String> = crate::linear::short_title(title, 80)
        .to_lowercase()
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .map(|w| w.trim_matches('-').to_string())
        .filter(|w| !w.is_empty() && !SKIP.contains(&w.as_str()))
        .take(3)
        .collect();
    let mut branch = number.to_string();
    if !words.is_empty() {
        branch.push('-');
        branch.push_str(&words.join("-"));
    }
    branch.truncate(60);
    branch.trim_end_matches('-').to_string()
}

fn person_from_login(login: &str, name: &str) -> Option<Person> {
    (!login.is_empty()).then(|| Person { id: login.to_string(), name: if name.is_empty() { login.to_string() } else { name.to_string() }, email: String::new() })
}

const ISSUES_QUERY: &str = "query { search(query: \"QUERY\", type: ISSUE, first: 100) { nodes { ... on Issue { \
number title body url state createdAt updatedAt closedAt author { login } \
assignees(first: 10) { nodes { login name } } labels(first: 30) { nodes { name } } milestone { dueOn } \
timelineItems(itemTypes: [CONNECTED_EVENT, CROSS_REFERENCED_EVENT], first: 30) { nodes { __typename \
... on ConnectedEvent { subject { __typename ... on PullRequest { number url state } } } \
... on CrossReferencedEvent { source { __typename ... on PullRequest { number url state } } } } } } } } }";

/// Who `gh` is signed in as, and the repository owner, as the snapshot's viewer/org.
fn viewer(repo: &str, env: &HashMap<String, String>) -> anyhow::Result<(Person, String)> {
    let user = gh_json(repo, env, &["api", "user"]).context("gh could not say who you are")?;
    let login = text(&user["login"]);
    let name = text(&user["name"]);
    let org = repo.split('/').next().unwrap_or(repo).to_string();
    Ok((person_from_login(&login, &name).unwrap_or_default(), org))
}

/// The person's open issues in a repository, and the ones closed in the last two weeks, as the
/// board's `mine`. ("Asking for your review" stays GitHub's PR review-request column, which is
/// provider-independent, so `reviewing` is left empty here.)
pub fn snapshot(repo: &str, env: &HashMap<String, String>, now: u64) -> anyhow::Result<Snapshot> {
    let (viewer, org) = viewer(repo, env)?;
    let prefix = status_prefix();
    let search = format!("repo:{repo} is:issue assignee:@me sort:updated-desc");
    let query = ISSUES_QUERY.replace("QUERY", &search);
    let data = gh_json(repo, env, &["api", "graphql", "-f", &format!("query={query}")])?;
    let nodes = data["data"]["search"]["nodes"].as_array().cloned().unwrap_or_default();
    let mut mine: Vec<Ticket> = Vec::new();
    for node in &nodes {
        if let Some(t) = ticket_from_issue(node, repo, &prefix, now) {
            mine.push(t);
        }
    }
    Ok(Snapshot { viewer, org, me: repo.to_string(), label: None, mine, reviewing: Vec::new(), fetched_at: now })
}

fn ticket_from_issue(node: &Value, repo: &str, prefix: &str, now: u64) -> Option<Ticket> {
    let number = node["number"].as_u64()?;
    let closed = node["state"].as_str().map(|s| s.eq_ignore_ascii_case("closed")).unwrap_or(false);
    let closed_at = node["closedAt"].as_str().map(str::to_string);
    // Drop issues closed more than two weeks ago.
    if closed {
        if let Some(at) = closed_at.as_deref().and_then(parse_time) {
            if now as i64 - at > DONE_FOR_SECS {
                return None;
            }
        }
    }
    let labels: Vec<String> = node["labels"]["nodes"].as_array().map(|ns| ns.iter().map(|l| text(&l["name"])).filter(|n| !n.is_empty()).collect()).unwrap_or_default();
    let linked = linked_prs(node);
    let has_open_pr = linked.iter().any(|(_, _, state)| state.eq_ignore_ascii_case("open"));
    let status = status_for(closed, &labels, has_open_pr, prefix);
    let assignee = node["assignees"]["nodes"].as_array().and_then(|a| a.first()).and_then(|a| person_from_login(&text(&a["login"]), &text(&a["name"])));
    let attachments = linked
        .iter()
        .map(|(n, url, _)| crate::linear::Attachment { title: format!("PR #{n}"), subtitle: String::new(), url: url.clone(), source: "github".into() })
        .collect();
    // The visible labels leave out the status: ones (they are the column, not a chip).
    let chip_labels: Vec<String> = labels.iter().filter(|l| !l.to_lowercase().starts_with(prefix)).cloned().collect();
    Some(Ticket {
        id: number.to_string(),
        identifier: format!("#{number}"),
        title: text(&node["title"]),
        description: text(&node["body"]),
        priority: 0,
        url: text(&node["url"]),
        branch: branch_for_issue(number, &text(&node["title"])),
        due: node["milestone"]["dueOn"].as_str().map(|d| d.get(..10).unwrap_or(d).to_string()),
        created_at: text(&node["createdAt"]),
        updated_at: text(&node["updatedAt"]),
        completed_at: closed_at,
        status,
        assignee,
        creator: node["author"]["login"].as_str().map(str::to_string).filter(|s| !s.is_empty()),
        labels: chip_labels,
        parent: None,
        team: repo.to_string(),
        attachments,
        provider: Provider::GitHub,
    })
}

/// Linked pull requests from an issue's timeline: (number, url, state).
fn linked_prs(node: &Value) -> Vec<(u64, String, String)> {
    let mut out: Vec<(u64, String, String)> = Vec::new();
    for item in node["timelineItems"]["nodes"].as_array().into_iter().flatten() {
        let pr = if !item["subject"].is_null() { &item["subject"] } else { &item["source"] };
        if pr["__typename"].as_str() == Some("PullRequest") {
            if let Some(n) = pr["number"].as_u64() {
                if !out.iter().any(|(m, _, _)| *m == n) {
                    out.push((n, text(&pr["url"]), text(&pr["state"])));
                }
            }
        }
    }
    out
}

/// An issue's body and comments, plus the six board columns as its "states" so the reader and
/// the drag-to-move menu have targets.
pub fn detail(repo: &str, number: u64, env: &HashMap<String, String>) -> anyhow::Result<Detail> {
    let data = gh_json(repo, env, &["issue", "view", &number.to_string(), "--repo", repo, "--json", "body,comments"])?;
    let mut comments: Vec<Comment> = data["comments"]
        .as_array()
        .map(|ns| {
            ns.iter()
                .map(|c| Comment {
                    author: {
                        let login = text(&c["author"]["login"]);
                        if login.is_empty() { "GitHub".into() } else { login }
                    },
                    body: text(&c["body"]),
                    created_at: text(&c["createdAt"]),
                    reply: false,
                })
                .collect()
        })
        .unwrap_or_default();
    comments.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    Ok(Detail { description: text(&data["body"]), comments, states: synthetic_states() })
}

/// The board's six columns as (move-target id, Status). The id a GitHub column carries is its
/// own name, since `set_status` moves by name, not by a remote state id.
fn synthetic_states() -> Vec<(String, Status)> {
    let mut states = vec![("In Progress".to_string(), Status { name: "In Progress".into(), kind: "started".into(), position: 0. })];
    states.push(("In Review".into(), Status { name: "In Review".into(), kind: "started".into(), position: 1. }));
    states.push(("Todo".into(), Status { name: "Todo".into(), kind: "unstarted".into(), position: 0. }));
    states.push(("Triage".into(), Status { name: "Triage".into(), kind: "triage".into(), position: 0. }));
    states.push(("Backlog".into(), Status { name: "Backlog".into(), kind: "backlog".into(), position: 0. }));
    states.push(("Done".into(), Status { name: "Done".into(), kind: "completed".into(), position: 0. }));
    states
}

/// The `Status` a board column carries, for the optimistic update when a card is dragged (and
/// for moving a ticket to In Progress on session start). Unknown names fall back to Todo.
pub fn column_status(column: &str) -> Status {
    synthetic_states().into_iter().find(|(name, _)| name.eq_ignore_ascii_case(column)).map(|(_, s)| s).unwrap_or(Status { name: "Todo".into(), kind: "unstarted".into(), position: 0. })
}

pub fn comment(repo: &str, number: u64, env: &HashMap<String, String>, body: &str) -> anyhow::Result<()> {
    gh_run(repo, env, &["issue", "comment", &number.to_string(), "--repo", repo, "--body", body])
}

/// Move an issue to a board column: swap its `status:` label, and open or close it. GitHub has
/// no statuses, so a column is the label plus open/closed — a move cannot touch a linked PR.
pub fn set_status(repo: &str, number: u64, env: &HashMap<String, String>, column: &str, prefix: &str) -> anyhow::Result<()> {
    let n = number.to_string();
    if column.eq_ignore_ascii_case("Done") {
        return gh_run(repo, env, &["issue", "close", &n, "--repo", repo]);
    }
    // Reopen if it was closed (moving out of Done).
    let _ = gh_run(repo, env, &["issue", "reopen", &n, "--repo", repo]);
    let add = COLUMNS.iter().find(|(name, _)| name.eq_ignore_ascii_case(column)).map(|(_, suffix)| format!("{prefix}{suffix}"));
    // Todo is the absence of a status label. Add the target label first (this is the move that
    // matters and must report an error), then drop the other status labels best-effort — a
    // `--remove-label` for a label the issue does not carry fails, which is fine.
    if let Some(add) = &add {
        gh_run(repo, env, &["issue", "edit", &n, "--repo", repo, "--add-label", add])?;
    }
    for (_, suffix) in COLUMNS {
        let label = format!("{prefix}{suffix}");
        if add.as_deref() != Some(label.as_str()) {
            let _ = gh_run(repo, env, &["issue", "edit", &n, "--repo", repo, "--remove-label", &label]);
        }
    }
    Ok(())
}

/// Repositories to offer in the project selector: `gh repo list` for the signed-in account,
/// newest first. Best-effort; the caller merges in the known project repos first.
pub fn repos(env: &HashMap<String, String>, limit: u32) -> anyhow::Result<Vec<ProjectOption>> {
    let data = gh_json("", env, &["repo", "list", "--limit", &limit.to_string(), "--json", "nameWithOwner,description"])?;
    Ok(data
        .as_array()
        .map(|rs| {
            rs.iter()
                .filter_map(|r| {
                    let slug = r["nameWithOwner"].as_str()?.to_string();
                    Some(ProjectOption { label: slug.clone(), detail: text(&r["description"]), id: slug })
                })
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_issue_numbers() {
        assert_eq!(issue_refs("742-calendar-sync"), vec![742]);
        assert_eq!(issue_refs("fix #742 and gh-13"), vec![742, 13]);
        assert_eq!(issue_refs("issue-9: the thing"), vec![9]);
        assert_eq!(issue_refs("wip/88-rework"), vec![88]);
        assert!(issue_refs("review/7").is_empty());
        assert!(issue_refs("eng-787-workspace").is_empty());
        assert!(mentions("742-calendar", 742));
        assert!(!mentions("7420-calendar", 742));
    }

    #[test]
    fn names_branches() {
        assert_eq!(branch_for_issue(742, "Calendar sync integrations: events and tasks"), "742-calendar-sync-integrations");
        assert_eq!(branch_for_issue(9, "Fix the bug"), "9-fix-bug");
    }

    #[test]
    fn synthesizes_columns() {
        let prefix = "status:";
        let none: Vec<String> = vec![];
        assert_eq!(status_for(true, &none, false, prefix).name, "Done");
        assert_eq!(status_for(true, &none, false, prefix).kind, "completed");
        // Default open issue is Todo.
        let todo = status_for(false, &none, false, prefix);
        assert_eq!((todo.name.as_str(), todo.kind.as_str()), ("Todo", "unstarted"));
        // A label wins.
        let ip = status_for(false, &["status:in-progress".into()], false, prefix);
        assert_eq!((ip.name.as_str(), ip.kind.as_str(), ip.position), ("In Progress", "started", 0.));
        // In-review label, or an open linked PR, both land in In Review (ordered after In Progress).
        assert_eq!(status_for(false, &["status:in-review".into()], false, prefix).name, "In Review");
        let by_pr = status_for(false, &none, true, prefix);
        assert_eq!((by_pr.name.as_str(), by_pr.position), ("In Review", 1.));
        // In Progress label beats an open PR.
        assert_eq!(status_for(false, &["status:in-progress".into()], true, prefix).name, "In Progress");
        assert_eq!(status_for(false, &["status:triage".into()], false, prefix).name, "Triage");
        assert_eq!(status_for(false, &["status:backlog".into()], false, prefix).name, "Backlog");
    }

    #[test]
    fn board_order_matches_linear() {
        // The synthesized kinds+position must sort the GitHub columns the way the board does:
        // it sorts cards by (status.order(), position). In Progress and In Review share the
        // "started" order and are split by position (0 vs 1), exactly as Linear's are.
        let prefix = "status:";
        let key = |labels: &[String], pr: bool| {
            let s = super::status_for(false, labels, pr, prefix);
            (s.order(), (s.position * 100.) as i64)
        };
        let in_progress = key(&["status:in-progress".into()], false);
        let in_review = key(&["status:in-review".into()], false);
        let todo = key(&[], false);
        let triage = key(&["status:triage".into()], false);
        let backlog = key(&["status:backlog".into()], false);
        assert!(in_progress < in_review, "In Progress before In Review");
        assert!(in_review < todo, "In Review before Todo");
        assert!(todo < triage, "Todo before Triage");
        assert!(triage < backlog, "Triage before Backlog");
    }
}
