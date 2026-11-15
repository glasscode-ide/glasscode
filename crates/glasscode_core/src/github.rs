//! GitHub lookups through the user's `gh` CLI (already signed in on developer machines).

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::bail;
use serde::Deserialize;

use crate::agents::resolve_program;

/// `owner/name` of the GitHub repository a checkout belongs to: the `glasscode.repo` git
/// setting when present (for mirrors or non-GitHub remotes), else the `origin` remote.
pub fn repo_slug(dir: &Path) -> Option<String> {
    if let Ok(output) = crate::sys::command("git").arg("-C").arg(dir).args(["config", "--get", "glasscode.repo"]).output() {
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if value.split('/').count() == 2 && !value.starts_with('/') {
            return Some(value);
        }
    }
    let output = crate::sys::command("git")
        .arg("-C")
        .arg(dir)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    parse_slug(String::from_utf8_lossy(&output.stdout).trim())
}

pub fn parse_slug(remote: &str) -> Option<String> {
    let remote = remote.trim();
    // The host may be a per-account SSH alias (`git@github.com-work:owner/name.git`,
    // `ssh://git@gh-work/owner/name`): the owner/name after it is what matters, and whether the
    // host looks like a GitHub one. `glasscode.repo` (handled by the caller) overrides all this.
    let after_host = if let Some(rest) = remote.strip_prefix("git@") {
        // scp form: host:owner/name
        let (host, path) = rest.split_once(':')?;
        github_host(host).then_some(path)?
    } else if let Some(rest) = remote.strip_prefix("ssh://") {
        let rest = rest.strip_prefix("git@").unwrap_or(rest);
        let (host, path) = rest.split_once('/')?;
        let host = host.split(':').next().unwrap_or(host);
        github_host(host).then_some(path)?
    } else if let Some(rest) = remote.strip_prefix("https://").or_else(|| remote.strip_prefix("http://")) {
        let rest = rest.split_once('@').map(|(_, r)| r).unwrap_or(rest);
        let (host, path) = rest.split_once('/')?;
        let host = host.split(':').next().unwrap_or(host);
        github_host(host).then_some(path)?
    } else {
        return None;
    };
    let rest = after_host.trim_start_matches('/').trim_end_matches('/').trim_end_matches(".git");
    let mut parts = rest.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    (!owner.is_empty() && !name.is_empty() && !name.contains('/')).then(|| format!("{owner}/{name}"))
}

/// Whether a git host is GitHub: `github.com`, a subdomain, or a per-account SSH alias such as
/// `github.com-work` or `gh-work` (common for juggling several GitHub logins).
fn github_host(host: &str) -> bool {
    let host = host.to_lowercase();
    host == "github.com"
        || host.ends_with(".github.com")
        || host.starts_with("github.com-")
        || host.starts_with("github-")
        || host.starts_with("gh-")
        || host.starts_with("github.com.")
}

#[derive(Clone, Debug, PartialEq, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub base_ref_name: String,
    #[serde(default)]
    pub head_ref_name: String,
    #[serde(default)]
    pub is_draft: bool,
    #[serde(default)]
    pub state: String,
}

fn gh(env: &HashMap<String, String>) -> anyhow::Result<Command> {
    // GLASSCODE_GH points at another gh binary (used by tests and for custom installs).
    let program = std::env::var("GLASSCODE_GH").ok().filter(|p| !p.is_empty()).map(std::path::PathBuf::from);
    let Some(program) = program.or_else(|| resolve_program("gh", env)) else {
        bail!("GitHub CLI (gh) is not installed");
    };
    let mut cmd = crate::sys::command(program);
    cmd.envs(env);
    Ok(cmd)
}

/// `gh` acting as the repository's GitHub account (its project's, else Glasscode's default).
pub(crate) fn gh_as(repo: &str, env: &HashMap<String, String>) -> anyhow::Result<Command> {
    let mut cmd = gh(env)?;
    cmd.envs(crate::github_accounts::env_for_repo(repo));
    Ok(cmd)
}

/// The open pull request whose head is `branch`, if any.
pub fn pr_for_branch(
    repo: &str,
    branch: &str,
    env: &HashMap<String, String>,
) -> anyhow::Result<Option<PullRequest>> {
    let output = gh_as(repo, env)?
        .args([
            "pr", "list", "--repo", repo, "--head", branch, "--state", "open", "--limit", "1",
            "--json", "number,title,url,baseRefName,headRefName,isDraft,state",
        ])
        .output()?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let list: Vec<PullRequest> = serde_json::from_slice(&output.stdout)?;
    Ok(list.into_iter().next())
}

/// One pull request as the New session dialog lists it.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, Deserialize)]
pub struct PrHit {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub head: String,
    pub base: String,
    /// The head branch is in this repository (not a fork), so pushing to it updates the PR.
    pub same_repo: bool,
    pub draft: bool,
    /// OPEN, CLOSED or MERGED.
    pub state: String,
    /// APPROVED, CHANGES_REQUESTED, REVIEW_REQUIRED or empty.
    pub review_decision: String,
    pub updated_at: String,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
}

const HIT_FIELDS: &str = "number,title,url,author,headRefName,baseRefName,isCrossRepository,isDraft,state,reviewDecision,updatedAt,additions,deletions,changedFiles";

fn parse_hit(v: &serde_json::Value) -> Option<PrHit> {
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    Some(PrHit {
        number: v["number"].as_u64()?,
        title: s("title"),
        url: s("url"),
        author: v["author"]["login"].as_str().unwrap_or_default().to_string(),
        head: s("headRefName"),
        base: s("baseRefName"),
        same_repo: !v["isCrossRepository"].as_bool().unwrap_or(false),
        draft: v["isDraft"].as_bool().unwrap_or(false),
        state: s("state"),
        review_decision: s("reviewDecision"),
        updated_at: s("updatedAt"),
        additions: v["additions"].as_u64().unwrap_or(0),
        deletions: v["deletions"].as_u64().unwrap_or(0),
        changed_files: v["changedFiles"].as_u64().unwrap_or(0),
    })
}

/// Pull requests matching what someone typed: a number ("929", "#929") finds that PR (open or
/// not); words search open PRs by title, branch and author, newest first.
pub fn search_prs(repo: &str, query: &str, env: &HashMap<String, String>) -> anyhow::Result<Vec<PrHit>> {
    let query = query.trim();
    let number = query.trim_start_matches('#');
    if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) {
        let mut cmd = gh_as(repo, env)?;
        cmd.args(["pr", "view", number, "--repo", repo, "--json", HIT_FIELDS]);
        let output = output_within(cmd, GH_LIMIT)?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            if err.contains("Could not resolve") || err.contains("not found") || err.contains("no pull requests") {
                return Ok(Vec::new());
            }
            bail!("{}", err.trim());
        }
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        return Ok(parse_hit(&value).into_iter().collect());
    }
    // Recent open PRs, matched here on title, branch and author (GitHub's search cannot match
    // part of a branch name); GitHub's text search adds PRs whose description matches.
    let mut hits: Vec<PrHit> = list_hits(repo, "sort:updated-desc", 100, env)?;
    if query.is_empty() {
        hits.truncate(12);
        return Ok(hits);
    }
    let words: Vec<String> = query.to_lowercase().split_whitespace().map(str::to_string).collect();
    hits.retain(|h| {
        let text = format!("{} {} {}", h.title, h.head, h.author).to_lowercase();
        words.iter().all(|w| text.contains(w.as_str()))
    });
    if hits.len() < 3 {
        for hit in list_hits(repo, &format!("{query} sort:updated-desc"), 12, env).unwrap_or_default() {
            if !hits.iter().any(|h| h.number == hit.number) {
                hits.push(hit);
            }
        }
    }
    hits.truncate(12);
    Ok(hits)
}

fn list_hits(repo: &str, search: &str, limit: u32, env: &HashMap<String, String>) -> anyhow::Result<Vec<PrHit>> {
    let mut cmd = gh_as(repo, env)?;
    cmd.args(["pr", "list", "--repo", repo, "--state", "open", "--limit", &limit.to_string(), "--search", search, "--json", HIT_FIELDS]);
    let output = output_within(cmd, GH_LIMIT)?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let list: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout)?;
    Ok(list.iter().filter_map(parse_hit).collect())
}

/// Open pull requests that ask for your review.
pub fn review_requests(repo: &str, env: &HashMap<String, String>) -> anyhow::Result<Vec<PrHit>> {
    list_hits(repo, "review-requested:@me sort:updated-desc", 12, env)
}

/// A pull request anywhere on GitHub that asks for the person's review (the Tickets view).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, Deserialize)]
pub struct ReviewRequest {
    /// `owner/name`.
    pub repo: String,
    pub hit: PrHit,
    pub created_at: String,
    /// GitHub's merge state: CLEAN, BEHIND, DIRTY (conflicts), BLOCKED, UNKNOWN, …
    pub merge_state: String,
    /// The newest review by each reviewer: (login, APPROVED / CHANGES_REQUESTED / COMMENTED).
    pub reviews: Vec<(String, String)>,
}

impl ReviewRequest {
    /// The newest review state left by an automated reviewer (a bot), if any reviewed.
    pub fn bot_review(&self) -> Option<&str> {
        self.reviews.iter().find(|(who, _)| is_bot_login(who)).map(|(_, state)| state.as_str())
    }
}

const REVIEW_REQUESTS: &str = "query { search(query: \"is:pr is:open review-requested:@me archived:false sort:updated-desc\", type: ISSUE, first: 50) { nodes { ... on PullRequest { \
number title url isDraft state createdAt updatedAt headRefName baseRefName additions deletions changedFiles mergeStateStatus reviewDecision isCrossRepository \
repository { nameWithOwner } author { login } latestReviews(first: 20) { nodes { author { login } state } } } } } }";

/// Open pull requests in any repository that ask for the review of Glasscode's default
/// GitHub account, newest first.
pub fn review_requested(env: &HashMap<String, String>) -> anyhow::Result<Vec<ReviewRequest>> {
    let value = gh_json("", env, &["api", "graphql", "-f", &format!("query={REVIEW_REQUESTS}")])?;
    Ok(parse_review_requests(&value))
}

fn parse_review_requests(value: &serde_json::Value) -> Vec<ReviewRequest> {
    value["data"]["search"]["nodes"]
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| {
                    let mut hit = parse_hit(n)?;
                    hit.changed_files = n["changedFiles"].as_u64().unwrap_or(0);
                    let reviews = n["latestReviews"]["nodes"]
                        .as_array()
                        .map(|r| r.iter().map(|r| (login(&r["author"]), text(&r["state"]))).filter(|(who, _)| !who.is_empty()).collect())
                        .unwrap_or_default();
                    Some(ReviewRequest { repo: text(&n["repository"]["nameWithOwner"]), hit, created_at: text(&n["createdAt"]), merge_state: text(&n["mergeStateStatus"]), reviews })
                })
                .filter(|r| !r.repo.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// What a PR says about itself: its description, files (+/−) and commit headlines.
pub fn pr_brief(repo: &str, number: u64, env: &HashMap<String, String>) -> anyhow::Result<(String, Vec<(String, u64, u64)>, Vec<String>)> {
    let mut cmd = gh_as(repo, env)?;
    cmd.args(["pr", "view", &number.to_string(), "--repo", repo, "--json", "body,files,commits"]);
    let output = output_within(cmd, GH_LIMIT)?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let v: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let files = v["files"]
        .as_array()
        .map(|l| l.iter().map(|f| (f["path"].as_str().unwrap_or_default().to_string(), f["additions"].as_u64().unwrap_or(0), f["deletions"].as_u64().unwrap_or(0))).collect())
        .unwrap_or_default();
    let commits = v["commits"]
        .as_array()
        .map(|l| l.iter().filter_map(|c| c["messageHeadline"].as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    Ok((v["body"].as_str().unwrap_or_default().to_string(), files, commits))
}

/// A pull request by number, as the PR panel shows it (review sessions know their number).
pub fn pr_by_number(repo: &str, number: u64, env: &HashMap<String, String>) -> anyhow::Result<Option<PullRequest>> {
    let mut cmd = gh_as(repo, env)?;
    cmd.args(["pr", "view", &number.to_string(), "--repo", repo, "--json", "number,title,url,baseRefName,headRefName,isDraft,state"]);
    let output = output_within(cmd, GH_LIMIT)?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(Some(serde_json::from_slice(&output.stdout)?))
}

/// Branches on GitHub whose pull request belongs to a checkout, most likely first:
/// 1. the branch's upstream (a local name can differ from the pushed one),
/// 2. the local branch name,
/// 3. branches on `origin` the checkout builds on: their tip is in HEAD but not in the default
///    branch (a local branch made from a PR's commit, or a merge into it not pushed yet),
///    newest first.
pub fn pr_branch_candidates(dir: &Path, branch: &str) -> Vec<String> {
    let git = |args: &[&str]| -> String {
        crate::sys::command("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let default = git(&["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"]);
    let default = default.trim();
    let default = if !default.is_empty() {
        default.to_string()
    } else {
        ["refs/remotes/origin/main", "refs/remotes/origin/master"]
            .into_iter()
            .find(|r| !git(&["rev-parse", "--verify", "--quiet", r]).is_empty())
            .unwrap_or_default()
            .to_string()
    };
    // The default branch is where PRs go, not where they come from.
    let default_name = default.strip_prefix("refs/remotes/origin/").unwrap_or_default().to_string();
    let mut list: Vec<String> = Vec::new();
    let mut push = |name: &str| {
        let name = name.trim();
        if !name.is_empty()
            && name != "HEAD"
            && name != "detached"
            && name != default_name
            && !list.iter().any(|n| n == name)
        {
            list.push(name.to_string());
        }
    };
    let on_branch = !branch.is_empty() && branch != "HEAD" && branch != "detached";
    if on_branch {
        push(&git(&["for-each-ref", "--format=%(upstream:lstrip=3)", &format!("refs/heads/{branch}")]));
        push(branch);
    }
    if !default.is_empty() {
        let built_on = git(&[
            "for-each-ref",
            "--merged",
            "HEAD",
            "--no-merged",
            &default,
            "--sort=-committerdate",
            "--count=3",
            "--format=%(refname:lstrip=3)",
            "refs/remotes/origin",
        ]);
        for name in built_on.lines() {
            push(name);
        }
    }
    list
}

/// The open pull request of a checkout: the first of `pr_branch_candidates` that has one.
pub fn pr_for_checkout(
    repo: &str,
    dir: &Path,
    branch: &str,
    env: &HashMap<String, String>,
) -> anyhow::Result<Option<PullRequest>> {
    for candidate in pr_branch_candidates(dir, branch) {
        if let Some(pr) = pr_for_branch(repo, &candidate, env)? {
            return Ok(Some(pr));
        }
    }
    Ok(None)
}

/// Head branch of a pull request (used to check out someone else's work locally).
pub fn pr_head_branch(repo: &str, number: u64, env: &HashMap<String, String>) -> anyhow::Result<String> {
    let output = gh_as(repo, env)?
        .args(["pr", "view", &number.to_string(), "--repo", repo, "--json", "headRefName", "-q", ".headRefName"])
        .output()?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Find a remote branch by prefix, e.g. `team/T-57864f-…`.
pub fn remote_branch_with_prefix(dir: &Path, prefix: &str) -> Option<String> {
    let output = crate::sys::command("git")
        .arg("-C")
        .arg(dir)
        .envs(crate::github_accounts::env_for_dir(dir))
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["ls-remote", "--heads", "origin", &format!("refs/heads/{prefix}*")])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split('\t').nth(1))
        .filter_map(|r| r.strip_prefix("refs/heads/"))
        .map(str::to_string)
        .next()
}

// ----- pull request details: checks, reviews, comments, preview ----------------------------------

/// Everything the Git tab shows about a pull request.
#[derive(Clone, Debug, Default, serde::Serialize, Deserialize)]
pub struct PrDetails {
    pub number: u64,
    pub title: String,
    pub url: String,
    /// OPEN, MERGED or CLOSED.
    pub state: String,
    pub is_draft: bool,
    pub head: String,
    pub base: String,
    pub author: String,
    pub updated_at: String,
    /// MERGEABLE, CONFLICTING or UNKNOWN.
    pub mergeable: String,
    /// CLEAN, BEHIND, BLOCKED, DIRTY, UNSTABLE, …
    pub merge_state: String,
    /// APPROVED, CHANGES_REQUESTED, REVIEW_REQUIRED, or none.
    pub review_decision: Option<String>,
    pub labels: Vec<String>,
    pub checks: Vec<CheckRun>,
    pub reviewers: Vec<Reviewer>,
    pub comments: Vec<IssueComment>,
    pub threads: Vec<ReviewThread>,
    pub preview: Preview,
    /// A review bot's verdict, from the review thread it keeps on the PR.
    #[serde(default)]
    pub verdict: Option<ReviewVerdict>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Failed,
    Running,
    Passed,
    Skipped,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, Deserialize)]
pub struct CheckRun {
    pub name: String,
    pub workflow: String,
    pub status: String,
    pub conclusion: String,
    pub url: String,
    pub started_at: String,
    pub completed_at: String,
}

impl CheckRun {
    pub fn state(&self) -> CheckState {
        match (self.status.as_str(), self.conclusion.as_str()) {
            (_, "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE" | "ERROR") => CheckState::Failed,
            (_, "SKIPPED" | "NEUTRAL" | "STALE") => CheckState::Skipped,
            ("COMPLETED", _) | (_, "SUCCESS") => CheckState::Passed,
            _ => CheckState::Running,
        }
    }

    /// The Actions run behind a check, from its URL (`…/actions/runs/<id>/job/<job>`).
    pub fn run_id(&self) -> Option<u64> {
        self.url.split("/actions/runs/").nth(1)?.split('/').next()?.parse().ok()
    }

    pub fn label(&self) -> String {
        if self.workflow.is_empty() || self.workflow == self.name { self.name.clone() } else { format!("{} / {}", self.workflow, self.name) }
    }
}

/// One run per check: GitHub keeps every re-run (a cancelled run stays next to the one that
/// replaced it), but only the newest counts, as on the PR page and in `gh pr checks`.
fn latest_checks(checks: Vec<CheckRun>) -> Vec<CheckRun> {
    // Not started yet means queued after everything that has a start time.
    let started = |c: &CheckRun| if c.started_at.is_empty() && c.status != "COMPLETED" { "~".to_string() } else { c.started_at.clone() };
    let mut out: Vec<CheckRun> = Vec::new();
    for check in checks {
        match out.iter_mut().find(|c| c.label() == check.label()) {
            Some(seen) if started(&check) >= started(seen) => *seen = check,
            Some(_) => {}
            None => out.push(check),
        }
    }
    out
}

#[derive(Clone, Debug, Default, serde::Serialize, Deserialize)]
pub struct Reviewer {
    pub login: String,
    /// APPROVED, CHANGES_REQUESTED, COMMENTED, DISMISSED, or REQUESTED (not reviewed yet).
    pub state: String,
    pub submitted_at: String,
    pub is_bot: bool,
}

#[derive(Clone, Debug, Default, serde::Serialize, Deserialize)]
pub struct IssueComment {
    pub author: String,
    pub body: String,
    pub created_at: String,
    pub url: String,
    pub is_bot: bool,
}

#[derive(Clone, Debug, Default, serde::Serialize, Deserialize)]
pub struct ThreadComment {
    pub author: String,
    pub body: String,
    pub created_at: String,
    pub url: String,
    pub diff_hunk: String,
}

#[derive(Clone, Debug, Default, serde::Serialize, Deserialize)]
pub struct ReviewThread {
    pub id: String,
    pub resolved: bool,
    pub outdated: bool,
    pub path: String,
    pub line: Option<u64>,
    pub comments: Vec<ThreadComment>,
}

/// The PR's preview deployment (GitHub Deployments in the environment `preview/pr-<n>`).
#[derive(Clone, Debug, Default, serde::Serialize, Deserialize)]
pub struct Preview {
    /// The repository uses previews (it has the label, or this PR has a preview deployment).
    pub available: bool,
    /// The PR carries the preview label (a preview is wanted).
    pub labeled: bool,
    /// success, in_progress, queued, pending, failure, error or inactive.
    pub state: Option<String>,
    pub url: Option<String>,
    pub sha: Option<String>,
    pub updated_at: Option<String>,
    /// Named links from the bot's "Ready" comment (Web, API, …).
    pub links: Vec<(String, String)>,
}

/// Per-repository preview settings (git config), with app's conventions as defaults:
/// `glasscode.previewLabel` (preview), `glasscode.previewEnvironment` (preview/pr-{n}),
/// `glasscode.previewRedeploy` (/preview redeploy).
#[derive(Clone, Debug)]
pub struct PreviewConfig {
    pub label: String,
    pub environment: String,
    pub redeploy_comment: String,
}

impl PreviewConfig {
    pub fn load(dir: &Path) -> PreviewConfig {
        let get = |key: &str, default: &str| {
            crate::sys::command("git")
                .arg("-C")
                .arg(dir)
                .args(["config", "--get", key])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| default.to_string())
        };
        PreviewConfig {
            label: get("glasscode.previewLabel", "preview"),
            environment: get("glasscode.previewEnvironment", "preview/pr-{n}"),
            redeploy_comment: get("glasscode.previewRedeploy", "/preview redeploy"),
        }
    }

    pub fn environment_for(&self, number: u64) -> String {
        self.environment.replace("{n}", &number.to_string())
    }
}

/// Run a command, giving up after `limit` (a hung network call must not stall the panel).
pub(crate) fn output_within(mut cmd: Command, limit: std::time::Duration) -> anyhow::Result<std::process::Output> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stdout.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let err = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stderr.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let deadline = std::time::Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("GitHub did not answer within {}s", limit.as_secs());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    Ok(std::process::Output { status, stdout: out.join().unwrap_or_default(), stderr: err.join().unwrap_or_default() })
}

const GH_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

/// What an automated review bot says about a PR: "## ✅ Approved · Review v3 · …" in a
/// managed conversation comment it keeps up to date (marked `glasscode-review-thread`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, Deserialize)]
pub struct ReviewVerdict {
    pub approved: bool,
    /// "Approved", "Changes requested", …
    pub verdict: String,
    pub version: u32,
    pub updated_at: String,
    pub url: String,
}

/// The latest verdict from a review bot's managed review comment, if one reviewed the PR.
pub fn review_verdict(comments: &[IssueComment]) -> Option<ReviewVerdict> {
    let comment = comments.iter().rev().find(|c| c.body.contains("glasscode-review-thread"))?;
    let header = comment.body.lines().map(str::trim).find(|l| l.starts_with("## ") && l.contains("Review v"))?;
    let parts: Vec<&str> = header.trim_start_matches("## ").split(" · ").map(str::trim).collect();
    let verdict = parts.first()?.trim_start_matches(|c: char| !c.is_alphanumeric()).trim().to_string();
    let version = parts.iter().find_map(|p| p.strip_prefix("Review v")).and_then(|v| v.parse().ok()).unwrap_or(0);
    Some(ReviewVerdict { approved: verdict.eq_ignore_ascii_case("approved"), verdict, version, updated_at: comment.created_at.clone(), url: comment.url.clone() })
}

/// The few answers a phone list needs about a PR: approved? CI? preview? bot verdict?
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, Deserialize)]
pub struct PrSummary {
    pub number: u64,
    pub title: String,
    pub url: String,
    /// OPEN, MERGED, CLOSED.
    pub state: String,
    pub draft: bool,
    /// approved, changes_requested, waiting.
    pub review: String,
    /// passed, failed, running, none.
    pub ci: String,
    pub checks_passed: u32,
    pub checks_failed: u32,
    pub checks_running: u32,
    /// ready, deploying, failed, none, or unavailable (the repo has no previews).
    pub preview: String,
    #[serde(default)]
    pub preview_url: Option<String>,
    #[serde(default)]
    pub verdict: Option<ReviewVerdict>,
    /// GitHub's merge state: CLEAN, BEHIND, DIRTY (conflicts), BLOCKED, …
    pub merge_state: String,
    pub unresolved: u32,
}

impl PrSummary {
    pub fn from_details(d: &PrDetails) -> PrSummary {
        let count = |state: CheckState| d.checks.iter().filter(|c| c.state() == state).count() as u32;
        let (failed, running, passed) = (count(CheckState::Failed), count(CheckState::Running), count(CheckState::Passed));
        let ci = if failed > 0 {
            "failed"
        } else if running > 0 {
            "running"
        } else if passed > 0 {
            "passed"
        } else {
            "none"
        };
        let review = match d.review_decision.as_deref() {
            Some("APPROVED") => "approved",
            Some("CHANGES_REQUESTED") => "changes_requested",
            _ => "waiting",
        };
        let preview = match (d.preview.available, d.preview.labeled, d.preview.state.as_deref()) {
            (false, _, _) => "unavailable",
            (_, false, _) => "none",
            (_, true, Some("success")) => "ready",
            (_, true, Some("failure" | "error")) => "failed",
            (_, true, _) => "deploying",
        };
        PrSummary {
            number: d.number,
            title: d.title.clone(),
            url: d.url.clone(),
            state: d.state.clone(),
            draft: d.is_draft,
            review: review.into(),
            ci: ci.into(),
            checks_passed: passed,
            checks_failed: failed,
            checks_running: running,
            preview: preview.into(),
            preview_url: d.preview.links.first().map(|(_, u)| u.clone()).or_else(|| d.preview.url.clone()).filter(|_| preview == "ready"),
            verdict: d.verdict.clone(),
            merge_state: d.merge_state.clone(),
            unresolved: d.threads.iter().filter(|t| !t.resolved).count() as u32,
        }
    }
}

/// Whether `gh` is installed and signed in here (for the server to say why PRs are missing).
pub fn gh_ready(env: &HashMap<String, String>) -> Result<(), String> {
    let Ok(mut cmd) = gh(env) else { return Err("GitHub CLI (gh) is not installed".into()) };
    cmd.args(["auth", "status"]);
    match output_within(cmd, std::time::Duration::from_secs(15)) {
        Ok(out) if out.status.success() => Ok(()),
        Ok(_) => Err("gh is not signed in (run `gh auth login`)".into()),
        Err(err) => Err(err.to_string()),
    }
}

pub(crate) fn gh_json(repo: &str, env: &HashMap<String, String>, args: &[&str]) -> anyhow::Result<serde_json::Value> {
    let mut cmd = gh_as(repo, env)?;
    cmd.args(args);
    let output = output_within(cmd, GH_LIMIT)?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

pub(crate) fn gh_run(repo: &str, env: &HashMap<String, String>, args: &[&str]) -> anyhow::Result<()> {
    let mut cmd = gh_as(repo, env)?;
    cmd.args(args);
    let output = output_within(cmd, GH_LIMIT)?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

fn text(value: &serde_json::Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

fn login(value: &serde_json::Value) -> String {
    text(&value["login"])
}

fn is_bot_login(name: &str) -> bool {
    // gh prints GitHub Apps without their "[bot]" suffix (github-actions, dependabot, …).
    const APPS: [&str; 8] = ["github-actions", "dependabot", "copilot", "renovate", "vercel", "netlify", "codecov", "sonarcloud"];
    name.ends_with("[bot]") || name.ends_with("-bot") || name.starts_with("app/") || APPS.contains(&name.to_lowercase().as_str())
}

/// Labelled links from a markdown table in a comment: `| Web | https://… |`.
fn table_links(body: &str) -> Vec<(String, String)> {
    body.lines()
        .filter_map(|line| {
            let cells: Vec<&str> = line.trim().trim_matches('|').split('|').map(str::trim).collect();
            match cells.as_slice() {
                [label, url] if url.starts_with("https://") && !label.is_empty() => Some((label.to_string(), url.to_string())),
                _ => None,
            }
        })
        .collect()
}

/// A PR's checks from `statusCheckRollup` (newest run of each).
fn parse_checks(rollup: &serde_json::Value) -> Vec<CheckRun> {
    let mut checks: Vec<CheckRun> = Vec::new();
    for check in rollup.as_array().into_iter().flatten() {
        checks.push(if check["__typename"] == "StatusContext" {
            // Commit statuses: one state, SUCCESS / FAILURE / ERROR / PENDING / EXPECTED.
            let state = text(&check["state"]);
            CheckRun {
                name: text(&check["context"]),
                workflow: String::new(),
                status: if matches!(state.as_str(), "PENDING" | "EXPECTED") { "IN_PROGRESS".into() } else { "COMPLETED".into() },
                conclusion: state,
                url: text(&check["targetUrl"]),
                started_at: text(&check["startedAt"]),
                completed_at: String::new(),
            }
        } else {
            CheckRun {
                name: text(&check["name"]),
                workflow: text(&check["workflowName"]),
                status: text(&check["status"]),
                conclusion: text(&check["conclusion"]),
                url: text(&check["detailsUrl"]),
                started_at: text(&check["startedAt"]),
                completed_at: text(&check["completedAt"]),
            }
        });
    }
    latest_checks(checks)
}

/// A pull request's CI on its newest commit, and whether it merges, for watching it in the
/// background.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CiStatus {
    /// The commit the checks ran on.
    pub head: String,
    /// OPEN, MERGED or CLOSED.
    pub state: String,
    pub checks: Vec<CheckRun>,
    /// MERGEABLE, CONFLICTING or UNKNOWN (GitHub has not worked it out yet; ask again later).
    pub mergeable: String,
    /// The branch it merges into, and that branch's commit GitHub compared against.
    pub base: String,
    pub base_head: String,
}

impl CiStatus {
    pub fn failed(&self) -> Vec<&CheckRun> {
        self.checks.iter().filter(|c| c.state() == CheckState::Failed).collect()
    }

    pub fn running(&self) -> bool {
        self.checks.iter().any(|c| c.state() == CheckState::Running)
    }

    /// GitHub says the branch conflicts with its base.
    pub fn conflicting(&self) -> bool {
        self.mergeable == "CONFLICTING"
    }

    /// GitHub says it merges cleanly.
    pub fn merges_cleanly(&self) -> bool {
        self.mergeable == "MERGEABLE"
    }

    /// The pair of commits a conflict is about: a push or a new commit on the base changes it.
    pub fn merge_key(&self) -> String {
        format!("{}:{}", self.head, self.base_head)
    }
}

const CI_FIELDS: &str = "headRefOid,state,statusCheckRollup,mergeable,baseRefName,baseRefOid";

/// A `CiStatus` from `gh pr view --json` with `CI_FIELDS`.
pub fn ci_from_json(pr: &serde_json::Value) -> CiStatus {
    CiStatus {
        head: text(&pr["headRefOid"]),
        state: text(&pr["state"]),
        checks: parse_checks(&pr["statusCheckRollup"]),
        mergeable: text(&pr["mergeable"]),
        base: text(&pr["baseRefName"]),
        base_head: text(&pr["baseRefOid"]),
    }
}

pub fn pr_ci(repo: &str, number: u64, env: &HashMap<String, String>) -> anyhow::Result<CiStatus> {
    let pr = gh_json(repo, env, &["pr", "view", &number.to_string(), "--repo", repo, "--json", CI_FIELDS])?;
    Ok(ci_from_json(&pr))
}

pub fn pr_details(repo: &str, number: u64, preview: &PreviewConfig, env: &HashMap<String, String>) -> anyhow::Result<PrDetails> {
    let n = number.to_string();
    let pr = gh_json(repo, env, &[
        "pr", "view", &n, "--repo", repo, "--json",
        "number,title,url,state,isDraft,headRefName,baseRefName,author,updatedAt,mergeable,mergeStateStatus,reviewDecision,labels,reviewRequests,latestReviews,statusCheckRollup,comments",
    ])?;
    let mut details = PrDetails {
        number,
        title: text(&pr["title"]),
        url: text(&pr["url"]),
        state: text(&pr["state"]),
        is_draft: pr["isDraft"].as_bool().unwrap_or(false),
        head: text(&pr["headRefName"]),
        base: text(&pr["baseRefName"]),
        author: login(&pr["author"]),
        updated_at: text(&pr["updatedAt"]),
        mergeable: text(&pr["mergeable"]),
        merge_state: text(&pr["mergeStateStatus"]),
        review_decision: pr["reviewDecision"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
        labels: pr["labels"].as_array().map(|l| l.iter().map(|x| text(&x["name"])).collect()).unwrap_or_default(),
        ..Default::default()
    };
    details.checks = parse_checks(&pr["statusCheckRollup"]);
    for review in pr["latestReviews"].as_array().into_iter().flatten() {
        let who = login(&review["author"]);
        details.reviewers.push(Reviewer { is_bot: is_bot_login(&who), login: who, state: text(&review["state"]), submitted_at: text(&review["submittedAt"]) });
    }
    for request in pr["reviewRequests"].as_array().into_iter().flatten() {
        let who = request["login"].as_str().or(request["name"].as_str()).unwrap_or_default().to_string();
        if !who.is_empty() && !details.reviewers.iter().any(|r| r.login == who) {
            details.reviewers.push(Reviewer { is_bot: is_bot_login(&who), login: who, state: "REQUESTED".into(), submitted_at: String::new() });
        }
    }
    for comment in pr["comments"].as_array().into_iter().flatten() {
        if comment["isMinimized"].as_bool() == Some(true) {
            continue;
        }
        let who = login(&comment["author"]);
        details.comments.push(IssueComment { is_bot: is_bot_login(&who), author: who, body: text(&comment["body"]), created_at: text(&comment["createdAt"]), url: text(&comment["url"]) });
    }

    // Review threads (file comments) and whether each is resolved: GraphQL only.
    if let Some((owner, name)) = repo.split_once('/') {
        let query = "query($o:String!,$n:String!,$p:Int!){repository(owner:$o,name:$n){pullRequest(number:$p){reviewThreads(first:100){nodes{id isResolved isOutdated path line originalLine comments(first:50){nodes{author{login} body createdAt url diffHunk}}}}}}}";
        let result = gh_json(repo, env, &["api", "graphql", "-f", &format!("query={query}"), "-F", &format!("o={owner}"), "-F", &format!("n={name}"), "-F", &format!("p={number}")]);
        if let Ok(value) = result {
            for thread in value["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"].as_array().into_iter().flatten() {
                details.threads.push(ReviewThread {
                    id: text(&thread["id"]),
                    resolved: thread["isResolved"].as_bool().unwrap_or(false),
                    outdated: thread["isOutdated"].as_bool().unwrap_or(false),
                    path: text(&thread["path"]),
                    line: thread["line"].as_u64().or(thread["originalLine"].as_u64()),
                    comments: thread["comments"]["nodes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|c| ThreadComment { author: login(&c["author"]), body: text(&c["body"]), created_at: text(&c["createdAt"]), url: text(&c["url"]), diff_hunk: text(&c["diffHunk"]) })
                        .collect(),
                });
            }
        }
    }

    // The preview deployment and its latest status.
    details.preview.labeled = details.labels.iter().any(|l| l == &preview.label);
    let environment = preview.environment_for(number);
    let deployments = gh_json(repo, env, &["api", &format!("repos/{repo}/deployments?environment={}&per_page=1", urlencoding::encode(&environment))]);
    if let Some(deployment) = deployments.ok().and_then(|d| d.as_array().and_then(|a| a.first().cloned())) {
        details.preview.sha = deployment["sha"].as_str().map(|s| s.chars().take(7).collect());
        if let Some(id) = deployment["id"].as_u64() {
            if let Ok(statuses) = gh_json(repo, env, &["api", &format!("repos/{repo}/deployments/{id}/statuses?per_page=1")]) {
                if let Some(status) = statuses.as_array().and_then(|a| a.first()) {
                    details.preview.state = status["state"].as_str().map(str::to_string);
                    details.preview.url = status["environment_url"].as_str().filter(|u| !u.is_empty()).map(str::to_string);
                    details.preview.updated_at = status["created_at"].as_str().map(str::to_string);
                }
            }
        }
    }
    details.preview.available = details.preview.labeled
        || details.preview.state.is_some()
        || gh_json(repo, env, &["api", &format!("repos/{repo}/labels/{}", urlencoding::encode(&preview.label))]).is_ok();
    // Named links (Web, API, IAM…) from the newest comment with a link table to the preview.
    if let Some(url) = &details.preview.url {
        let host = url.trim_start_matches("https://").split('/').next().unwrap_or_default().to_string();
        let suffix = host.split_once('.').map(|(_, rest)| rest.to_string()).unwrap_or(host);
        if let Some(comment) = details.comments.iter().rev().find(|c| c.body.contains(&suffix)) {
            details.preview.links = table_links(&comment.body).into_iter().filter(|(_, u)| u.contains(&suffix)).collect();
        }
    }
    details.verdict = review_verdict(&details.comments);
    Ok(details)
}

/// Re-run the failed jobs of these Actions runs.
pub fn rerun_failed(repo: &str, runs: &[u64], env: &HashMap<String, String>) -> anyhow::Result<()> {
    for run in runs {
        gh_run(repo, env, &["run", "rerun", &run.to_string(), "--failed", "--repo", repo])?;
    }
    Ok(())
}

pub fn set_label(repo: &str, number: u64, label: &str, on: bool, env: &HashMap<String, String>) -> anyhow::Result<()> {
    gh_run(repo, env, &["pr", "edit", &number.to_string(), "--repo", repo, if on { "--add-label" } else { "--remove-label" }, label])
}

pub fn comment(repo: &str, number: u64, body: &str, env: &HashMap<String, String>) -> anyhow::Result<()> {
    gh_run(repo, env, &["pr", "comment", &number.to_string(), "--repo", repo, "--body", body])
}

/// The head commit (OID) of a pull request — what the auto-reviewer keys its "already
/// reviewed this commit" record on, so a new comment on the PR does not trigger a re-review.
pub fn head_oid(repo: &str, number: u64, env: &HashMap<String, String>) -> anyhow::Result<String> {
    let value = gh_json(repo, env, &["pr", "view", &number.to_string(), "--repo", repo, "--json", "headRefOid"])?;
    let oid = value["headRefOid"].as_str().unwrap_or_default().to_string();
    if oid.is_empty() {
        bail!("no head commit for {repo}#{number}");
    }
    Ok(oid)
}

pub fn reply_to_thread(repo: &str, thread_id: &str, body: &str, env: &HashMap<String, String>) -> anyhow::Result<()> {
    let query = "mutation($t:ID!,$b:String!){addPullRequestReviewThreadReply(input:{pullRequestReviewThreadId:$t,body:$b}){comment{id}}}";
    gh_json(repo, env, &["api", "graphql", "-f", &format!("query={query}"), "-f", &format!("t={thread_id}"), "-f", &format!("b={body}")]).map(|_| ())
}

pub fn resolve_thread(repo: &str, thread_id: &str, env: &HashMap<String, String>) -> anyhow::Result<()> {
    let query = "mutation($t:ID!){resolveReviewThread(input:{threadId:$t}){thread{id}}}";
    gh_json(repo, env, &["api", "graphql", "-f", &format!("query={query}"), "-f", &format!("t={thread_id}")]).map(|_| ())
}

/// Push the branch and open a draft pull request for it (title and body from its commits).
pub fn push_and_open_draft(dir: &Path, branch: &str, base: &str, env: &HashMap<String, String>) -> anyhow::Result<()> {
    let account = crate::github_accounts::env_for_dir(dir);
    let push = crate::sys::command("git").arg("-C").arg(dir).args(["push", "-u", "origin", branch]).envs(env).envs(account.clone()).env("GIT_TERMINAL_PROMPT", "0").output()?;
    if !push.status.success() {
        bail!("git push failed: {}", String::from_utf8_lossy(&push.stderr).trim());
    }
    let output = gh(env)?.envs(account).current_dir(dir).args(["pr", "create", "--draft", "--fill", "--base", base, "--head", branch]).output()?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

/// Seconds since the Unix epoch for an RFC 3339 UTC time (`2026-10-15T12:37:25Z`).
pub fn parse_time(stamp: &str) -> Option<i64> {
    let (date, time) = stamp.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let time = time.trim_end_matches('Z');
    let time = time.split(['+', '.']).next()?;
    let mut t = time.split(':').map(|p| p.parse::<i64>().ok());
    let (hh, mm, ss) = (t.next()??, t.next()??, t.next().flatten().unwrap_or(0));
    // Days from civil (Howard Hinnant).
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

#[cfg(test)]
mod tests {
    use super::parse_slug;

    #[test]
    fn reads_review_requests() {
        let value = serde_json::json!({ "data": { "search": { "nodes": [
            { "number": 784, "title": "feat: add team boards (ENG-725)", "url": "https://github.com/acme/app/pull/784", "isDraft": false, "state": "OPEN",
              "createdAt": "2026-09-30T21:24:47Z", "updatedAt": "2026-10-20T13:04:25Z", "headRefName": "feat/eng-725-team-boards", "baseRefName": "main",
              "additions": 106054, "deletions": 3681, "changedFiles": 774, "mergeStateStatus": "DIRTY", "reviewDecision": "CHANGES_REQUESTED", "isCrossRepository": false,
              "repository": { "nameWithOwner": "acme/app" }, "author": { "login": "octocat" },
              "latestReviews": { "nodes": [{ "author": { "login": "review-bot" }, "state": "COMMENTED" }, { "author": null, "state": "APPROVED" }] } },
            {}
        ] } } });
        let list = super::parse_review_requests(&value);
        assert_eq!(list.len(), 1);
        let r = &list[0];
        assert_eq!((r.repo.as_str(), r.hit.number, r.hit.changed_files, r.merge_state.as_str()), ("acme/app", 784, 774, "DIRTY"));
        assert_eq!((r.hit.head.as_str(), r.hit.author.as_str(), r.bot_review()), ("feat/eng-725-team-boards", "octocat", Some("COMMENTED")));
        assert_eq!(r.reviews.len(), 1);
    }

    #[test]
    fn reads_check_runs_times_and_links() {
        let run = super::CheckRun { url: "https://github.com/o/r/actions/runs/123/job/9".into(), status: "COMPLETED".into(), conclusion: "FAILURE".into(), ..Default::default() };
        assert_eq!(run.run_id(), Some(123));
        assert_eq!(run.state(), super::CheckState::Failed);
        assert_eq!(super::parse_time("1970-01-02T00:00:01Z"), Some(86401));
        let check = |conclusion: &str, started: &str| super::CheckRun { name: "Title".into(), workflow: "PR".into(), status: if conclusion.is_empty() { "QUEUED".into() } else { "COMPLETED".into() }, conclusion: conclusion.into(), started_at: started.into(), ..Default::default() };
        let latest = super::latest_checks(vec![check("SUCCESS", "2026-10-15T10:00:00Z"), check("CANCELLED", "2026-10-15T09:00:00Z")]);
        assert_eq!((latest.len(), latest[0].state()), (1, super::CheckState::Passed));
        let latest = super::latest_checks(vec![check("FAILURE", "2026-10-15T10:00:00Z"), check("", "")]);
        assert_eq!(latest[0].state(), super::CheckState::Running);
        assert_eq!(super::parse_time("2026-10-15T12:37:25Z"), Some(1_792_067_845));
        let body = "**Status:** Ready\n| Service | URL |\n|---|---|\n| Web | https://glasscode.invalid |\n| API (core) | https://glasscode.invalid |";
        assert_eq!(super::table_links(body), vec![("Web".to_string(), "https://glasscode.invalid".to_string()), ("API (core)".to_string(), "https://glasscode.invalid".to_string())]);
    }

    #[test]
    fn reads_review_verdict() {
        let comment = |body: &str| super::IssueComment { author: "review-bot[bot]".into(), body: body.into(), created_at: "2026-10-14T13:15:43Z".into(), url: "u".into(), is_bot: true };
        let verdict = super::review_verdict(&[
            comment("thanks"),
            comment("<!-- glasscode-review-thread:codex -->\n<!-- Managed by the review bot; do not edit by hand. -->\n\n\n## ✅ Approved · Review v3 · delta · Oct 14, 2026, 1:15 PM · ✅ Approved · Codex (shared)\n"),
        ])
        .unwrap();
        assert!(verdict.approved);
        assert_eq!((verdict.verdict.as_str(), verdict.version), ("Approved", 3));
        let changes = super::review_verdict(&[comment("<!-- glasscode-review-thread:codex -->\n## ❌ Changes requested · Review v2 · full")]).unwrap();
        assert!(!changes.approved && changes.verdict == "Changes requested" && changes.version == 2);
        assert_eq!(super::review_verdict(&[comment("lgtm")]), None);
    }

    #[test]
    fn parses_remotes() {
        assert_eq!(parse_slug("git@github.com:acme/app.git").as_deref(), Some("acme/app"));
        assert_eq!(parse_slug("https://github.com/acme/tools").as_deref(), Some("acme/tools"));
        assert_eq!(parse_slug("https://gitlab.com/a/b.git"), None);
        // Per-account SSH host aliases (several GitHub logins on one machine).
        assert_eq!(parse_slug("git@github.com-work:acme/app.git").as_deref(), Some("acme/app"));
        assert_eq!(parse_slug("ssh://git@github.com-work/acme/app").as_deref(), Some("acme/app"));
        assert_eq!(parse_slug("git@gh-work:acme/app.git").as_deref(), Some("acme/app"));
        assert_eq!(parse_slug("https://user@github.com/acme/app.git").as_deref(), Some("acme/app"));
        // Non-GitHub hosts are still rejected, so Glasscode never makes wrong gh calls.
        assert_eq!(parse_slug("git@gitlab.com-work:team/app.git"), None);
        assert_eq!(parse_slug("https://bitbucket.org/team/app.git"), None);
        assert_eq!(parse_slug("git@github.com:owner"), None);
    }
}
