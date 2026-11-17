//! GitHub, as far as the manager needs it beyond `crate::github`: the reviews the person left
//! on other people's pull requests. Read-only: nothing here writes to GitHub.

use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::bail;
use serde_json::Value;

use super::Blocker;

/// A pull request of someone else's on which the person asked for changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GivenReview {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    /// The author's login.
    pub author: String,
    /// The head commit now.
    pub head: String,
    /// The head commit the person's last review was on.
    pub reviewed_head: String,
    /// The person's last review: "CHANGES_REQUESTED", "COMMENTED", "APPROVED".
    pub my_state: String,
    /// The review threads the person started, with whether each is resolved.
    pub threads: Vec<Blocker>,
    /// "open", "merged", "closed".
    pub state: String,
    pub updated: u64,
}

impl GivenReview {
    /// Whether something the person asked for is still open on it: the pull request is open,
    /// is someone else's, and they asked for changes or started a thread nobody resolved.
    pub fn has_open_ask(&self, me: &str) -> bool {
        self.state == "open"
            && !self.author.eq_ignore_ascii_case(me)
            && self.my_state != "APPROVED"
            && (self.my_state == "CHANGES_REQUESTED" || self.threads.iter().any(|t| !t.resolved))
    }
}

/// How many reviewed pull requests are looked at in one go, how many tracked ones on top.
const MAX_CANDIDATES: usize = 40;
const MAX_KNOWN: usize = 40;
/// One `gh` call may take this long; a whole run of [`given_reviews`] that long.
const CALL_LIMIT: Duration = Duration::from_secs(30);
const RUN_LIMIT: Duration = Duration::from_secs(120);
/// How long a blocker's text may be.
const BLOCKER_CHARS: usize = 140;

/// One pull request: what it is, the person's own reviews on it and every review thread.
const PR_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!, $me: String!) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { \
number title url state isDraft updatedAt headRefOid author { login } \
reviews(last: 40, author: $me) { nodes { state submittedAt author { login } commit { oid } } } \
reviewThreads(first: 100) { nodes { id isResolved path comments(first: 1) { nodes { author { login } body } } } } } } }";

/// `gh` with the session environment, acting as Glasscode's default GitHub account (the one
/// review requests are read with), so the login and what it sees belong together.
fn gh(env: &HashMap<String, String>) -> anyhow::Result<Command> {
    // GLASSCODE_GH points at another gh binary (used by tests and for custom installs).
    let program = std::env::var("GLASSCODE_GH").ok().filter(|p| !p.is_empty()).map(std::path::PathBuf::from);
    let Some(program) = program.or_else(|| crate::agents::resolve_program("gh", env)) else {
        bail!("GitHub CLI (gh) is not installed");
    };
    let mut cmd = Command::new(program);
    cmd.envs(env);
    cmd.envs(crate::github_accounts::env_for_repo(""));
    Ok(cmd)
}

/// Run `gh` with these arguments and give back what it printed.
fn gh_text(env: &HashMap<String, String>, args: &[&str]) -> anyhow::Result<String> {
    let mut cmd = gh(env)?;
    cmd.args(args);
    let output = crate::github::output_within(cmd, CALL_LIMIT)?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn gh_json(env: &HashMap<String, String>, args: &[&str]) -> anyhow::Result<Value> {
    Ok(serde_json::from_str(&gh_text(env, args)?)?)
}

/// The login `gh` is signed in as.
pub fn my_login(env: &HashMap<String, String>) -> anyhow::Result<String> {
    let login = gh_text(env, &["api", "user", "--jq", ".login"])?.trim().to_string();
    if login.is_empty() {
        bail!("gh did not say who is signed in");
    }
    Ok(login)
}

/// Open pull requests by others that the person reviewed and that still have something open
/// from them (changes requested, or unresolved threads they started), plus the ones in
/// `known` (repo, number) so a follow-up learns that its pull request was merged or closed.
pub fn given_reviews(me: &str, known: &[(String, u64)], env: &HashMap<String, String>) -> anyhow::Result<Vec<GivenReview>> {
    let me = me.trim().trim_start_matches('@');
    if me.is_empty() {
        bail!("the GitHub login is not known yet");
    }
    let limit = MAX_CANDIDATES.to_string();
    let found = gh_json(env, &["search", "prs", "--reviewed-by", me, "--state", "open", "--limit", &limit, "--json", "number,repository,author,updatedAt"])?;
    let candidates = parse_search(&found, me);

    // The tracked ones first: learning that one was merged matters more than finding a new one.
    let known: Vec<(String, u64)> = known.iter().filter(|(repo, number)| repo.contains('/') && *number > 0).take(MAX_KNOWN).cloned().collect();
    let tracked: HashSet<(String, u64)> = known.iter().map(|(repo, number)| (repo.to_lowercase(), *number)).collect();
    let mut todo = known;
    for hit in candidates {
        if todo.len() >= MAX_KNOWN + MAX_CANDIDATES {
            break;
        }
        if !todo.iter().any(|(repo, number)| repo.eq_ignore_ascii_case(&hit.0) && *number == hit.1) {
            todo.push(hit);
        }
    }

    let started = Instant::now();
    let mut out = Vec::new();
    let mut failed: Option<anyhow::Error> = None;
    for (repo, number) in &todo {
        if started.elapsed() > RUN_LIMIT {
            break;
        }
        match fetch(repo, *number, me, env) {
            Ok(Some(review)) => {
                if tracked.contains(&(repo.to_lowercase(), *number)) || review.has_open_ask(me) {
                    out.push(review);
                }
            }
            Ok(None) => {}
            Err(err) => {
                failed.get_or_insert(err);
            }
        }
    }
    match failed {
        // Nothing could be read at all: say why, instead of "no reviews".
        Some(err) if out.is_empty() && !todo.is_empty() => Err(err),
        _ => Ok(out),
    }
}

/// One pull request with the person's reviews and threads. None when it is gone.
fn fetch(repo: &str, number: u64, me: &str, env: &HashMap<String, String>) -> anyhow::Result<Option<GivenReview>> {
    let Some((owner, name)) = repo.split_once('/') else { bail!("not a repository: {repo}") };
    let value = gh_json(
        env,
        &[
            "api",
            "graphql",
            "-f",
            &format!("query={PR_QUERY}"),
            "-f",
            &format!("owner={owner}"),
            "-f",
            &format!("name={name}"),
            "-F",
            &format!("number={number}"),
            "-f",
            &format!("me={me}"),
        ],
    )?;
    Ok(parse_pr(&value, repo, me))
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

/// The (repo, number) of every pull request `gh search prs --json number,repository,author`
/// found that is not the person's own, in the order given, each once.
pub(crate) fn parse_search(value: &Value, me: &str) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = Vec::new();
    for hit in value.as_array().map(Vec::as_slice).unwrap_or_default() {
        let repo = text(&hit["repository"]["nameWithOwner"]);
        let Some(number) = hit["number"].as_u64() else { continue };
        if repo.is_empty() || number == 0 || text(&hit["author"]["login"]).eq_ignore_ascii_case(me) {
            continue;
        }
        if !out.iter().any(|(r, n)| *r == repo && *n == number) {
            out.push((repo, number));
        }
    }
    out
}

/// A pull request from the answer to [`PR_QUERY`]: its state, the person's last review (the
/// last verdict when there is one, since a later plain comment does not take a "changes
/// requested" back) and the threads the person started.
pub(crate) fn parse_pr(value: &Value, repo: &str, me: &str) -> Option<GivenReview> {
    let pr = &value["data"]["repository"]["pullRequest"];
    let number = pr["number"].as_u64()?;
    let state = match text(&pr["state"]).to_uppercase().as_str() {
        "MERGED" => "merged",
        "CLOSED" => "closed",
        _ => "open",
    };

    // The person's submitted reviews, oldest first.
    let mut reviews: Vec<(String, String, String)> = pr["reviews"]["nodes"]
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .filter(|r| text(&r["author"]["login"]).eq_ignore_ascii_case(me))
                .map(|r| (text(&r["submittedAt"]), text(&r["state"]).to_uppercase(), text(&r["commit"]["oid"])))
                .filter(|(_, state, _)| !state.is_empty() && state != "PENDING")
                .collect()
        })
        .unwrap_or_default();
    reviews.sort_by(|a, b| a.0.cmp(&b.0));
    let verdict = reviews.iter().rev().find(|(_, state, _)| matches!(state.as_str(), "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED"));
    let my_state = verdict.or(reviews.last()).map(|(_, state, _)| state.clone()).unwrap_or_default();
    let reviewed_head = reviews.iter().rev().map(|(_, _, oid)| oid.clone()).find(|oid| !oid.is_empty()).unwrap_or_default();

    let threads = pr["reviewThreads"]["nodes"]
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|t| {
                    let first = t["comments"]["nodes"].as_array()?.first()?;
                    if !text(&first["author"]["login"]).eq_ignore_ascii_case(me) {
                        return None;
                    }
                    Some(Blocker { thread: text(&t["id"]), text: one_line(&text(&first["body"]), BLOCKER_CHARS), path: text(&t["path"]), resolved: t["isResolved"].as_bool().unwrap_or(false) })
                })
                .collect()
        })
        .unwrap_or_default();

    Some(GivenReview {
        repo: repo.to_string(),
        number,
        title: text(&pr["title"]),
        url: text(&pr["url"]),
        author: text(&pr["author"]["login"]),
        head: text(&pr["headRefOid"]),
        reviewed_head,
        my_state,
        threads,
        state: state.to_string(),
        updated: crate::github::parse_time(&text(&pr["updatedAt"])).map(|t| t.max(0) as u64).unwrap_or(0),
    })
}

/// A comment as one short line: its first line that says something (not a code fence, a
/// quote or a comment for machines), without markdown marks, cut to `max` characters.
pub(crate) fn one_line(body: &str, max: usize) -> String {
    let mut in_code = false;
    let mut picked = "";
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            in_code = !in_code;
            continue;
        }
        if in_code || line.is_empty() || line.starts_with('>') || line.starts_with("<!--") {
            continue;
        }
        picked = line;
        break;
    }
    let cleaned = picked.trim_start_matches(|c: char| matches!(c, '#' | '-' | '*' | '+') || c.is_whitespace()).replace("**", "").replace('`', "");
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    let line = words.join(" ");
    if line.chars().count() <= max {
        return line;
    }
    let mut cut: String = line.chars().take(max.saturating_sub(1)).collect();
    cut = cut.trim_end().to_string();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_pr() -> Value {
        json!({ "data": { "repository": { "pullRequest": {
            "number": 1111, "title": "Token refresh", "url": "https://github.com/acme/app/pull/1111", "state": "OPEN", "isDraft": false,
            "updatedAt": "2026-11-13T23:00:35Z", "headRefOid": "bbb222", "author": { "login": "sam" },
            "reviews": { "nodes": [
                { "state": "COMMENTED", "submittedAt": "2026-11-10T08:00:00Z", "author": { "login": "Alex" }, "commit": { "oid": "aaa000" } },
                { "state": "CHANGES_REQUESTED", "submittedAt": "2026-11-11T09:00:00Z", "author": { "login": "alex" }, "commit": { "oid": "aaa111" } },
                { "state": "COMMENTED", "submittedAt": "2026-11-12T10:00:00Z", "author": { "login": "alex" }, "commit": { "oid": "aaa111" } },
                { "state": "PENDING", "submittedAt": null, "author": { "login": "alex" }, "commit": { "oid": "bbb222" } },
                { "state": "APPROVED", "submittedAt": "2026-11-12T11:00:00Z", "author": { "login": "mira" }, "commit": { "oid": "bbb222" } }
            ] },
            "reviewThreads": { "nodes": [
                { "id": "T_1", "isResolved": true, "path": "auth/refresh.rs", "comments": { "nodes": [ { "author": { "login": "alex" }, "body": "**Blocker:** the refresh token is logged in plain text\n\nSee line 88." } ] } },
                { "id": "T_2", "isResolved": false, "path": "auth/session.rs", "comments": { "nodes": [ { "author": { "login": "alex" }, "body": "```suggestion\nlet x = 1;\n```\nThe error is swallowed here" } ] } },
                { "id": "T_3", "isResolved": false, "path": "client/http.rs", "comments": { "nodes": [ { "author": { "login": "mira" }, "body": "nit: naming" } ] } },
                { "id": "T_4", "isResolved": false, "path": "", "comments": { "nodes": [] } }
            ] }
        } } } })
    }

    #[test]
    fn reads_a_pull_request_with_my_reviews_and_threads() {
        let review = parse_pr(&sample_pr(), "acme/app", "alex").expect("a pull request");
        assert_eq!(review.repo, "acme/app");
        assert_eq!(review.number, 1111);
        assert_eq!(review.title, "Token refresh");
        assert_eq!(review.url, "https://github.com/acme/app/pull/1111");
        assert_eq!(review.author, "sam");
        assert_eq!(review.state, "open");
        assert_eq!(review.head, "bbb222");
        // The last verdict counts, not the plain comment after it, and never a pending review.
        assert_eq!(review.my_state, "CHANGES_REQUESTED");
        assert_eq!(review.reviewed_head, "aaa111");
        // Only the threads the person started.
        assert_eq!(review.threads.len(), 2);
        assert_eq!(review.threads[0], Blocker { thread: "T_1".into(), text: "Blocker: the refresh token is logged in plain text".into(), path: "auth/refresh.rs".into(), resolved: true });
        assert_eq!(review.threads[1].text, "The error is swallowed here");
        assert!(!review.threads[1].resolved);
        assert_eq!(review.updated, crate::github::parse_time("2026-11-13T23:00:35Z").unwrap() as u64);
        assert!(review.has_open_ask("alex"));
    }

    #[test]
    fn a_comment_only_review_and_an_approval() {
        let mut value = sample_pr();
        value["data"]["repository"]["pullRequest"]["reviews"]["nodes"] = json!([
            { "state": "COMMENTED", "submittedAt": "2026-11-10T08:00:00Z", "author": { "login": "alex" }, "commit": { "oid": "aaa000" } }
        ]);
        let review = parse_pr(&value, "acme/app", "alex").unwrap();
        assert_eq!(review.my_state, "COMMENTED");
        assert_eq!(review.reviewed_head, "aaa000");
        // An unresolved thread of the person's keeps it open even without "changes requested".
        assert!(review.has_open_ask("alex"));

        value["data"]["repository"]["pullRequest"]["reviews"]["nodes"] = json!([
            { "state": "CHANGES_REQUESTED", "submittedAt": "2026-11-10T08:00:00Z", "author": { "login": "alex" }, "commit": { "oid": "aaa000" } },
            { "state": "APPROVED", "submittedAt": "2026-11-12T08:00:00Z", "author": { "login": "alex" }, "commit": { "oid": "bbb222" } }
        ]);
        let review = parse_pr(&value, "acme/app", "alex").unwrap();
        assert_eq!(review.my_state, "APPROVED");
        assert_eq!(review.reviewed_head, "bbb222");
        assert!(!review.has_open_ask("alex"));
    }

    #[test]
    fn merged_closed_and_missing_pull_requests() {
        let mut value = sample_pr();
        value["data"]["repository"]["pullRequest"]["state"] = json!("MERGED");
        let review = parse_pr(&value, "acme/app", "alex").unwrap();
        assert_eq!(review.state, "merged");
        assert!(!review.has_open_ask("alex"));
        value["data"]["repository"]["pullRequest"]["state"] = json!("CLOSED");
        assert_eq!(parse_pr(&value, "acme/app", "alex").unwrap().state, "closed");
        assert_eq!(parse_pr(&json!({ "data": { "repository": { "pullRequest": null } } }), "acme/app", "alex"), None);
        assert_eq!(parse_pr(&json!({ "errors": [{ "message": "Could not resolve" }] }), "acme/app", "alex"), None);
    }

    #[test]
    fn my_own_pull_request_has_no_open_ask() {
        let review = parse_pr(&sample_pr(), "acme/app", "sam").unwrap();
        // Sam is the author: nothing of theirs counts as a review given.
        assert!(!review.has_open_ask("sam"));
        assert_eq!(review.my_state, "");
        assert!(review.threads.is_empty());
    }

    #[test]
    fn nothing_open_without_changes_requested_or_threads() {
        let mut value = sample_pr();
        value["data"]["repository"]["pullRequest"]["reviews"]["nodes"] = json!([
            { "state": "COMMENTED", "submittedAt": "2026-11-10T08:00:00Z", "author": { "login": "alex" }, "commit": { "oid": "aaa000" } }
        ]);
        value["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"] = json!([
            { "id": "T_1", "isResolved": true, "path": "a.rs", "comments": { "nodes": [ { "author": { "login": "alex" }, "body": "fix this" } ] } }
        ]);
        assert!(!parse_pr(&value, "acme/app", "alex").unwrap().has_open_ask("alex"));
    }

    #[test]
    fn reads_the_search_and_leaves_out_my_own() {
        let value = json!([
            { "number": 1139, "repository": { "name": "app", "nameWithOwner": "acme/app" }, "author": { "login": "sam", "is_bot": false }, "updatedAt": "2026-11-13T23:00:35Z" },
            { "number": 1064, "repository": { "name": "app", "nameWithOwner": "acme/app" }, "author": { "login": "Alex" }, "updatedAt": "2026-11-12T23:00:35Z" },
            { "number": 1139, "repository": { "name": "app", "nameWithOwner": "acme/app" }, "author": { "login": "sam" }, "updatedAt": "2026-11-13T23:00:35Z" },
            { "number": 7, "repository": { "name": "site", "nameWithOwner": "acme/site" }, "author": { "login": "mira" }, "updatedAt": "2026-11-11T23:00:35Z" },
            { "repository": { "nameWithOwner": "acme/site" } }
        ]);
        assert_eq!(parse_search(&value, "alex"), vec![("acme/app".to_string(), 1139), ("acme/site".to_string(), 7)]);
        assert!(parse_search(&json!({ "message": "bad" }), "alex").is_empty());
    }

    #[test]
    fn one_line_is_short_and_plain() {
        assert_eq!(one_line("  \n\n> quoted\n- **Please** rename `foo`  to   bar\nmore", 140), "Please rename foo to bar");
        assert_eq!(one_line("<!-- bot -->\n## Heading here", 140), "Heading here");
        assert_eq!(one_line("```rust\nlet a = 1;\n```", 140), "");
        let long = "word ".repeat(60);
        let cut = one_line(&long, 140);
        assert!(cut.chars().count() <= 140, "{}", cut.chars().count());
        assert!(cut.ends_with('…'));
        // Never cuts inside a character.
        assert_eq!(one_line("äöüäöüäöü", 5), "äöüä…");
    }

    #[test]
    fn no_login_no_query() {
        assert!(given_reviews("  ", &[], &HashMap::new()).is_err());
    }
}
