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
    if let Some(value) = repo_override(dir) {
        return Some(value);
    }
    let output = crate::sys::command("git")
        .arg("-C")
        .arg(dir)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    parse_slug(String::from_utf8_lossy(&output.stdout).trim())
}

/// The `glasscode.repo` git setting of a checkout (`owner/name`), which names its GitHub
/// repository over its `origin`: for mirrors and non-GitHub remotes.
pub fn repo_override(dir: &Path) -> Option<String> {
    let output = crate::sys::command("git").arg("-C").arg(dir).args(["config", "--get", "glasscode.repo"]).output().ok()?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (value.split('/').count() == 2 && !value.starts_with('/')).then_some(value)
}

/// A remote in one form, so that the forms of one repository compare equal (Manager v2's project
/// keys, `identity`): `host/path` in lower case, without the scheme, the user, the port and a
/// `.git` suffix. A GitHub remote is `github.com/owner/name`: `github.com`, its subdomains
/// (`ssh.github.com`), and the per-account SSH aliases that have no dot of their own
/// (`github.com-work`, `github-work`, `gh-work`). A host with more after such a prefix
/// (`github-enterprise.corp.example`, `gh-mirror.example.org`) is a server of its own, unlike
/// `github_host`, which v1 reads (`repo_slug`). An alias of the SSH config that looks like no
/// GitHub host (`Host work`) keeps its own name, as the SSH config is not read. None for a local
/// path (`/srv/app.git`, `../app`, `C:\repos\app`, `file://…`), which names a folder of one
/// machine rather than a repository.
pub fn normalize_remote(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let (authority, path) = match remote.split_once("://") {
        Some((scheme, rest)) => {
            if !matches!(scheme.to_ascii_lowercase().as_str(), "ssh" | "git" | "http" | "https" | "git+ssh" | "ssh+git") {
                return None;
            }
            rest.split_once('/')?
        }
        None => {
            // The scp form, `[user@]host:path`. A colon after a slash, and a drive letter, are
            // parts of a local path.
            let (authority, path) = remote.split_once(':')?;
            if authority.len() < 2 || authority.contains(['/', '\\']) {
                return None;
            }
            (authority, path)
        }
    };
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    // Without the port: `[::1]:22` keeps its address, `host:2222` loses the port.
    let host = match host.strip_prefix('[') {
        Some(inside) => inside.split_once(']').map_or(host, |(address, _)| address),
        None => host.split(':').next().unwrap_or(host),
    }
    // A name written with its root dot (`github.com.`) is the same host.
    .trim_end_matches('.')
    .to_lowercase();
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).trim_end_matches('/').to_lowercase();
    if host.is_empty() || path.is_empty() {
        return None;
    }
    if !github_remote_host(&host) {
        return Some(format!("{host}/{path}"));
    }
    let mut parts = path.split('/');
    let (owner, name) = (parts.next()?, parts.next()?.trim_end_matches(".git"));
    (!owner.is_empty() && !name.is_empty()).then(|| format!("github.com/{owner}/{name}"))
}

/// Whether a host is GitHub for [`normalize_remote`]: `github.com`, a subdomain of it, or a
/// per-account alias with no dot after its prefix.
fn github_remote_host(host: &str) -> bool {
    host == "github.com"
        || host.ends_with(".github.com")
        || ["github.com-", "github.com.", "github-", "gh-"].iter().any(|prefix| host.strip_prefix(prefix).is_some_and(|alias| !alias.is_empty() && !alias.contains('.')))
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

#[derive(Clone, Debug, Default, PartialEq, Deserialize, serde::Serialize)]
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
    /// Who opened it (M32's review facts): `author { login }` from GitHub, a login once kept.
    #[serde(default, deserialize_with = "login_of")]
    pub author: String,
    /// How many reviews are asked for now, of people or teams: GitHub's `{ totalCount }`, `gh`'s
    /// list, a number once kept.
    #[serde(default, deserialize_with = "count_of")]
    pub review_requests: u64,
    /// The newest review of each reviewer (`latestReviews`): GitHub's `{ nodes }`, `gh`'s list.
    #[serde(default, rename = "latestReviews", deserialize_with = "reviews_of")]
    pub reviews: Vec<Review>,
}

/// One reviewer's newest review of a pull request.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Review {
    pub login: String,
    /// APPROVED, CHANGES_REQUESTED, COMMENTED or DISMISSED.
    pub state: String,
    /// RFC 3339, as GitHub says it.
    pub submitted_at: String,
}

fn login_of<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(match &value {
        serde_json::Value::String(login) => login.clone(),
        other => other["login"].as_str().unwrap_or_default().to_string(),
    })
}

fn count_of<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(match &value {
        serde_json::Value::Number(n) => n.as_u64().unwrap_or(0),
        serde_json::Value::Array(list) => list.len() as u64,
        other => other["totalCount"].as_u64().unwrap_or(0),
    })
}

fn reviews_of<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Review>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    let list = match &value {
        serde_json::Value::Array(list) => list.clone(),
        other => other["nodes"].as_array().cloned().unwrap_or_default(),
    };
    Ok(list
        .iter()
        .map(|r| Review {
            login: match &r["author"] {
                serde_json::Value::String(login) => login.clone(),
                author => author["login"].as_str().or_else(|| r["login"].as_str()).unwrap_or_default().to_string(),
            },
            state: r["state"].as_str().unwrap_or_default().to_string(),
            submitted_at: r["submittedAt"].as_str().or_else(|| r["submitted_at"].as_str()).unwrap_or_default().to_string(),
        })
        .collect())
}

/// The fields of a [`PullRequest`] for `gh pr list` and `gh pr view`.
pub(crate) const PR_FIELDS: &str = "number,title,url,baseRefName,headRefName,isDraft,state,author,reviewRequests,latestReviews";

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
            "--json", PR_FIELDS,
        ])
        .output()?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let list: Vec<PullRequest> = serde_json::from_slice(&output.stdout)?;
    Ok(list.into_iter().next())
}

/// The repository (`owner/name`) of a pull request's web address
/// (`https://github.com/owner/name/pull/42`, an Enterprise host too).
pub fn repo_of_pr_url(url: &str) -> Option<String> {
    let rest = url.trim().split_once("://").map_or(url.trim(), |(_, rest)| rest);
    let mut parts = rest.split('/');
    let (_host, owner, name, pull) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    (pull == "pull" && !owner.is_empty() && !name.is_empty()).then(|| format!("{owner}/{name}"))
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
    let value = match crate::github_api::auth_for("", env) {
        Some(auth) => crate::github_query::as_gh_graphql(&auth, env, REVIEW_REQUESTS)?,
        None => gh_json("", env, &["api", "graphql", "-f", &format!("query={REVIEW_REQUESTS}")])?,
    };
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

/// One of the person's own open pull requests, in any repository (Today, the Pull requests page).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MyPr {
    /// `owner/name`.
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub updated_at: String,
    pub draft: bool,
    /// Its checks and whether it merges; `None` when GitHub did not answer, and, when read
    /// through `gh`, for the ones after the newest [`MY_PR_DETAILS`].
    pub ci: Option<CiStatus>,
    /// APPROVED, CHANGES_REQUESTED, REVIEW_REQUIRED or empty.
    pub review: String,
}

/// How many of them, the most recently updated, get their checks and review when read through
/// `gh` (one call each; asked of GitHub itself, all of them come in the search).
pub const MY_PR_DETAILS: usize = 10;
/// The most open pull requests read for one account.
const MY_PR_LIMIT: u32 = 50;
const MY_PR_FIELDS: &str = "number,repository,title,url,updatedAt,isDraft";

/// The open pull requests the person wrote, newest first, for each GitHub account `gh` is
/// signed in to (the accounts Settings lists). Each account is asked as itself (its token from
/// `gh`, as sessions get it); when no token can be had, only `gh`'s own active account is asked.
/// Calls: one to list the accounts, one search for each account, and one `gh pr view` for each
/// of the newest [`MY_PR_DETAILS`]. An error only when every search failed.
pub fn my_open_prs(env: &HashMap<String, String>) -> anyhow::Result<Vec<MyPr>> {
    let accounts: Vec<Vec<(String, String)>> = match crate::github_accounts::accounts(env) {
        Ok(list) => list
            .iter()
            .filter(|account| account.ok)
            .filter_map(|account| {
                let as_it = crate::github_accounts::env_for(&account.login);
                (!as_it.is_empty() || account.active).then_some(as_it)
            })
            .collect(),
        Err(err) => {
            log::warn!("could not list the GitHub accounts of gh ({err:#}); asking as its active account");
            Vec::new()
        }
    };
    let accounts = if accounts.is_empty() { vec![Vec::new()] } else { accounts };
    open_prs_as(&accounts, env)
}

/// [`my_open_prs`] for these accounts (each an environment from `github_accounts::env_for`;
/// an empty one is `gh`'s active account).
fn open_prs_as(accounts: &[Vec<(String, String)>], env: &HashMap<String, String>) -> anyhow::Result<Vec<MyPr>> {
    let name = |account: &[(String, String)]| account.iter().find(|(k, _)| k == "GLASSCODE_GITHUB_ACCOUNT").map_or("gh's active account".to_string(), |(_, v)| v.clone());
    // Each pull request with the account that found it: that account can read its checks.
    let mut found: Vec<(MyPr, usize)> = Vec::new();
    let mut first_error = None;
    let mut failed = 0;
    for (index, account) in accounts.iter().enumerate() {
        // Asked of GitHub itself when there is a token: one search, with each PR's checks and
        // review in it.
        let login = account.iter().find(|(k, _)| k == "GLASSCODE_GITHUB_ACCOUNT").map(|(_, v)| v.as_str());
        let searched = match crate::github_api::auth_as(login, env) {
            Some(auth) => crate::github_query::my_open_prs(&auth, env, MY_PR_LIMIT),
            None => search_my_prs(account, env),
        };
        match searched {
            Ok(list) => {
                for pr in list {
                    if !found.iter().any(|(seen, _)| seen.url == pr.url) {
                        found.push((pr, index));
                    }
                }
            }
            Err(err) => {
                log::warn!("could not list the open pull requests of {}: {err:#}", name(account));
                failed += 1;
                first_error.get_or_insert(err);
            }
        }
    }
    if let (Some(err), true) = (first_error, failed == accounts.len()) {
        return Err(err);
    }
    found.sort_by(|a, b| b.0.updated_at.cmp(&a.0.updated_at));
    std::thread::scope(|scope| {
        // Those read through `gh` have no checks yet: a call each for the newest.
        for (pr, index) in found.iter_mut().take(MY_PR_DETAILS).filter(|(pr, _)| pr.ci.is_none()) {
            let account = &accounts[*index];
            scope.spawn(move || match checks_and_review(&pr.repo, pr.number, account, env) {
                Ok((ci, review)) => {
                    pr.ci = Some(ci);
                    pr.review = review;
                }
                Err(err) => log::warn!("no checks for {}#{}: {err:#}", pr.repo, pr.number),
            });
        }
    });
    Ok(found.into_iter().map(|(pr, _)| pr).collect())
}

fn search_my_prs(account: &[(String, String)], env: &HashMap<String, String>) -> anyhow::Result<Vec<MyPr>> {
    let mut cmd = gh(env)?;
    cmd.envs(account.iter().map(|(k, v)| (k, v)));
    cmd.args(["search", "prs", "--author", "@me", "--state", "open", "--sort", "updated", "--limit", &MY_PR_LIMIT.to_string(), "--json", MY_PR_FIELDS]);
    let output = output_within(cmd, GH_LIMIT)?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    parse_my_prs(&output.stdout)
}

/// What `gh search prs --json MY_PR_FIELDS` printed. No output at all is no pull requests.
fn parse_my_prs(stdout: &[u8]) -> anyhow::Result<Vec<MyPr>> {
    if stdout.trim_ascii().is_empty() {
        return Ok(Vec::new());
    }
    let list: Vec<serde_json::Value> = serde_json::from_slice(stdout)?;
    Ok(list
        .iter()
        .filter_map(|v| {
            Some(MyPr {
                repo: text(&v["repository"]["nameWithOwner"]),
                number: v["number"].as_u64()?,
                title: text(&v["title"]),
                url: text(&v["url"]),
                updated_at: text(&v["updatedAt"]),
                draft: v["isDraft"].as_bool().unwrap_or(false),
                ci: None,
                review: String::new(),
            })
        })
        .filter(|pr| !pr.repo.is_empty())
        .collect())
}

/// A pull request's checks (as [`pr_ci`]) and its review decision, asked as `account`.
fn checks_and_review(repo: &str, number: u64, account: &[(String, String)], env: &HashMap<String, String>) -> anyhow::Result<(CiStatus, String)> {
    let mut cmd = gh(env)?;
    cmd.envs(account.iter().map(|(k, v)| (k, v)));
    cmd.args(["pr", "view", &number.to_string(), "--repo", repo, "--json", &format!("{CI_FIELDS},reviewDecision")]);
    let output = output_within(cmd, GH_LIMIT)?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let pr: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    Ok((ci_from_json(&pr), text(&pr["reviewDecision"])))
}

/// A merged pull request: when it merged (Unix time) and the lines it changed (added and removed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Merged {
    pub at: i64,
    pub lines: u32,
}

/// Whether a pull request is merged (for Activity); `None` while it is open, and when it was
/// closed without a merge.
pub fn pr_merged(repo: &str, number: u64, env: &HashMap<String, String>) -> anyhow::Result<Option<Merged>> {
    match crate::github_api::auth_for(repo, env) {
        Some(auth) => crate::github_query::pr_merged(&auth, env, repo, number),
        None => pr_merged_gh(repo, number, env),
    }
}

pub(crate) fn pr_merged_gh(repo: &str, number: u64, env: &HashMap<String, String>) -> anyhow::Result<Option<Merged>> {
    let pr = gh_json(repo, env, &["pr", "view", &number.to_string(), "--repo", repo, "--json", "state,mergedAt,additions,deletions"])?;
    Ok(merged_from_json(&pr))
}

pub(crate) fn merged_from_json(pr: &serde_json::Value) -> Option<Merged> {
    if pr["state"].as_str() != Some("MERGED") {
        return None;
    }
    let lines = pr["additions"].as_u64().unwrap_or(0).saturating_add(pr["deletions"].as_u64().unwrap_or(0));
    let at = pr["mergedAt"].as_str().and_then(parse_time).unwrap_or_else(|| crate::model::now() as i64);
    Some(Merged { at, lines: u32::try_from(lines).unwrap_or(u32::MAX) })
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
    match crate::github_api::auth_for(repo, env) {
        Some(auth) => crate::github_query::pr_by_number(&auth, env, repo, number),
        None => pr_by_number_gh(repo, number, env),
    }
}

pub(crate) fn pr_by_number_gh(repo: &str, number: u64, env: &HashMap<String, String>) -> anyhow::Result<Option<PullRequest>> {
    let mut cmd = gh_as(repo, env)?;
    cmd.args(["pr", "view", &number.to_string(), "--repo", repo, "--json", PR_FIELDS]);
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
    let candidates = pr_branch_candidates(dir, branch);
    match crate::github_api::auth_for(repo, env) {
        Some(auth) => crate::github_query::pr_for_candidates(&auth, env, repo, &candidates),
        None => pr_for_candidates_gh(repo, &candidates, env),
    }
}

/// [`pr_for_checkout`] through `gh`: a call for each candidate until one has a pull request.
pub(crate) fn pr_for_candidates_gh(repo: &str, candidates: &[String], env: &HashMap<String, String>) -> anyhow::Result<Option<PullRequest>> {
    for candidate in candidates {
        if let Some(pr) = pr_for_branch(repo, candidate, env)? {
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
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, Deserialize)]
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
    match crate::github_api::auth_for(repo, env) {
        Some(auth) => crate::github_query::pr_ci(&auth, env, repo, number),
        None => pr_ci_gh(repo, number, env),
    }
}

pub(crate) fn pr_ci_gh(repo: &str, number: u64, env: &HashMap<String, String>) -> anyhow::Result<CiStatus> {
    let pr = gh_json(repo, env, &["pr", "view", &number.to_string(), "--repo", repo, "--json", CI_FIELDS])?;
    Ok(ci_from_json(&pr))
}

/// Everything the PR panel shows about a pull request. Read once for callers that ask at the
/// same time (the panel, the manager, a command), and kept for a few seconds.
pub fn pr_details(repo: &str, number: u64, preview: &PreviewConfig, env: &HashMap<String, String>) -> anyhow::Result<PrDetails> {
    match crate::github_api::auth_for(repo, env) {
        Some(auth) => {
            let key = crate::github_api::pr_key("details", &auth, repo, number);
            crate::github_api::shared(&key, std::time::Duration::from_secs(5), || crate::github_query::pr_details(&auth, env, repo, number, preview))
        }
        None => pr_details_gh(repo, number, preview, env),
    }
}

/// [`pr_details`] through `gh`: a call for the PR, one for its review threads, and up to three
/// for the preview.
fn pr_details_gh(repo: &str, number: u64, preview: &PreviewConfig, env: &HashMap<String, String>) -> anyhow::Result<PrDetails> {
    let n = number.to_string();
    let pr = gh_json(repo, env, &[
        "pr", "view", &n, "--repo", repo, "--json",
        "number,title,url,state,isDraft,headRefName,baseRefName,author,updatedAt,mergeable,mergeStateStatus,reviewDecision,labels,reviewRequests,latestReviews,statusCheckRollup,comments",
    ])?;
    // Review threads (file comments) and whether each is resolved: GraphQL only.
    let mut threads = serde_json::Value::Null;
    if let Some((owner, name)) = repo.split_once('/') {
        let query = "query($o:String!,$n:String!,$p:Int!){repository(owner:$o,name:$n){pullRequest(number:$p){reviewThreads(first:100){nodes{id isResolved isOutdated path line originalLine comments(first:50){nodes{author{login} body createdAt url diffHunk}}}}}}}";
        let result = gh_json(repo, env, &["api", "graphql", "-f", &format!("query={query}"), "-F", &format!("o={owner}"), "-F", &format!("n={name}"), "-F", &format!("p={number}")]);
        if let Ok(value) = result {
            threads = value["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"].clone();
        }
    }
    let mut details = details_from_json(number, &pr, &threads);
    // The preview deployment and its latest status.
    let environment = preview.environment_for(number);
    let deployments = gh_json(repo, env, &["api", &format!("repos/{repo}/deployments?environment={}&per_page=1", urlencoding::encode(&environment))]);
    let deployment = deployments.ok().and_then(|d| d.as_array().and_then(|a| a.first().cloned()));
    let status = deployment
        .as_ref()
        .and_then(|d| d["id"].as_u64())
        .and_then(|id| gh_json(repo, env, &["api", &format!("repos/{repo}/deployments/{id}/statuses?per_page=1")]).ok())
        .and_then(|s| s.as_array().and_then(|a| a.first().cloned()));
    let label = || gh_json(repo, env, &["api", &format!("repos/{repo}/labels/{}", urlencoding::encode(&preview.label))]).is_ok();
    finish_preview(&mut details, preview, deployment.as_ref(), status.as_ref(), label);
    Ok(details)
}

/// A PR's details from `gh pr view --json` fields (or the same shape made from GraphQL) and its
/// review threads (GraphQL `reviewThreads.nodes`; null for none).
pub(crate) fn details_from_json(number: u64, pr: &serde_json::Value, threads: &serde_json::Value) -> PrDetails {
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
    for thread in threads.as_array().into_iter().flatten() {
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
    details
}

/// The preview of a PR's details: its newest deployment and that one's latest status (as the
/// REST API lists them), whether the repository has the preview label (asked only when nothing
/// else shows a preview), its named links, and the review verdict.
pub(crate) fn finish_preview(details: &mut PrDetails, preview: &PreviewConfig, deployment: Option<&serde_json::Value>, status: Option<&serde_json::Value>, label_exists: impl FnOnce() -> bool) {
    details.preview.labeled = details.labels.iter().any(|l| l == &preview.label);
    if let Some(deployment) = deployment {
        details.preview.sha = deployment["sha"].as_str().map(|s| s.chars().take(7).collect());
    }
    if let Some(status) = status {
        details.preview.state = status["state"].as_str().map(str::to_string);
        details.preview.url = status["environment_url"].as_str().filter(|u| !u.is_empty()).map(str::to_string);
        details.preview.updated_at = status["created_at"].as_str().map(str::to_string);
    }
    details.preview.available = details.preview.labeled || details.preview.state.is_some() || label_exists();
    // Named links (Web, API, IAM…) from the newest comment with a link table to the preview.
    if let Some(url) = &details.preview.url {
        let host = url.trim_start_matches("https://").split('/').next().unwrap_or_default().to_string();
        let suffix = host.split_once('.').map(|(_, rest)| rest.to_string()).unwrap_or(host);
        if let Some(comment) = details.comments.iter().rev().find(|c| c.body.contains(&suffix)) {
            details.preview.links = table_links(&comment.body).into_iter().filter(|(_, u)| u.contains(&suffix)).collect();
        }
    }
    details.verdict = review_verdict(&details.comments);
}

/// Re-run the failed jobs of these Actions runs.
pub fn rerun_failed(repo: &str, runs: &[u64], env: &HashMap<String, String>) -> anyhow::Result<()> {
    crate::github_api::forget_repo(repo);
    for run in runs {
        gh_run(repo, env, &["run", "rerun", &run.to_string(), "--failed", "--repo", repo])?;
    }
    Ok(())
}

pub fn set_label(repo: &str, number: u64, label: &str, on: bool, env: &HashMap<String, String>) -> anyhow::Result<()> {
    crate::github_api::forget_pr(repo, number);
    gh_run(repo, env, &["pr", "edit", &number.to_string(), "--repo", repo, if on { "--add-label" } else { "--remove-label" }, label])
}

pub fn comment(repo: &str, number: u64, body: &str, env: &HashMap<String, String>) -> anyhow::Result<()> {
    crate::github_api::forget_pr(repo, number);
    gh_run(repo, env, &["pr", "comment", &number.to_string(), "--repo", repo, "--body", body])
}

/// The head commit (OID) of a pull request — what the auto-reviewer keys its "already
/// reviewed this commit" record on, so a new comment on the PR does not trigger a re-review.
pub fn head_oid(repo: &str, number: u64, env: &HashMap<String, String>) -> anyhow::Result<String> {
    if let Some(auth) = crate::github_api::auth_for(repo, env) {
        return crate::github_query::head_oid(&auth, env, repo, number);
    }
    let value = gh_json(repo, env, &["pr", "view", &number.to_string(), "--repo", repo, "--json", "headRefOid"])?;
    let oid = value["headRefOid"].as_str().unwrap_or_default().to_string();
    if oid.is_empty() {
        bail!("no head commit for {repo}#{number}");
    }
    Ok(oid)
}

pub fn reply_to_thread(repo: &str, thread_id: &str, body: &str, env: &HashMap<String, String>) -> anyhow::Result<()> {
    crate::github_api::forget_repo(repo);
    let query = "mutation($t:ID!,$b:String!){addPullRequestReviewThreadReply(input:{pullRequestReviewThreadId:$t,body:$b}){comment{id}}}";
    gh_json(repo, env, &["api", "graphql", "-f", &format!("query={query}"), "-f", &format!("t={thread_id}"), "-f", &format!("b={body}")]).map(|_| ())
}

pub fn resolve_thread(repo: &str, thread_id: &str, env: &HashMap<String, String>) -> anyhow::Result<()> {
    crate::github_api::forget_repo(repo);
    let query = "mutation($t:ID!){resolveReviewThread(input:{threadId:$t}){thread{id}}}";
    gh_json(repo, env, &["api", "graphql", "-f", &format!("query={query}"), "-f", &format!("t={thread_id}")]).map(|_| ())
}

/// Push the branch and open a draft pull request for it (title and body from its commits).
pub fn push_and_open_draft(dir: &Path, branch: &str, base: &str, env: &HashMap<String, String>) -> anyhow::Result<()> {
    push_and_open_draft_unless(dir, branch, base, env, None)
}

/// [`push_and_open_draft`] for an op its caller may stop (M33.2, ops-10): it says each step to
/// `job`, looks at it before each, and kills the push or `gh` it waits for when asked. A stop is
/// a [`crate::ops::job::Cancelled`] in the error, saying what ran and what may have.
pub fn push_and_open_draft_unless(dir: &Path, branch: &str, base: &str, env: &HashMap<String, String>, job: Option<&crate::ops::job::Job>) -> anyhow::Result<()> {
    use crate::ops::job;
    let account = crate::github_accounts::env_for_dir(dir);
    let pushing = format!("pushing {branch}");
    job::check(job)?;
    if let Some(job) = job {
        job.step(1, 2, &pushing);
    }
    let mut cmd = crate::sys::command("git");
    cmd.arg("-C").arg(dir).args(["push", "-u", "origin", branch]).envs(env).envs(account.clone()).env("GIT_TERMINAL_PROMPT", "0");
    let Some(push) = job::output_unless(cmd, job)? else { return Err(job.map(|j| j.stopped_midway(&pushing)).expect("stopped by its job").into()) };
    if !push.status.success() {
        bail!("git push failed: {}", String::from_utf8_lossy(&push.stderr).trim());
    }
    let opening = "opening the draft pull request".to_string();
    if let Some(job) = job {
        job.ran(&pushing);
        job.check()?;
        job.step(2, 2, &opening);
    }
    let mut cmd = gh(env)?;
    cmd.envs(account).current_dir(dir).args(["pr", "create", "--draft", "--fill", "--base", base, "--head", branch]);
    let Some(output) = job::output_unless(cmd, job)? else { return Err(job.map(|j| j.stopped_midway(&opening)).expect("stopped by its job").into()) };
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
    use std::collections::HashMap;

    use super::parse_slug;

    #[test]
    fn the_repository_of_a_pull_requests_address() {
        use super::repo_of_pr_url;
        assert_eq!(repo_of_pr_url("https://github.com/Acme/app/pull/42").as_deref(), Some("Acme/app"));
        assert_eq!(repo_of_pr_url("https://git.example.com/acme/app/pull/7/files").as_deref(), Some("acme/app"));
        assert_eq!(repo_of_pr_url("https://github.com/acme/app/issues/7"), None);
        assert_eq!(repo_of_pr_url(""), None);
    }

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

    /// `gh search prs --author @me --state open --sort updated --json number,repository,title,url,updatedAt,isDraft`
    /// as gh 2.88.1 printed it, with made-up repositories, titles and links.
    const MY_PRS: &str = r#"[{"isDraft":false,"number":28,"repository":{"name":"app","nameWithOwner":"acme/app"},"title":"Show the build number in the title bar","updatedAt":"2026-11-16T20:26:05Z","url":"https://github.com/acme/app/pull/28"},{"isDraft":false,"number":1,"repository":{"name":"tools","nameWithOwner":"acme/tools"},"title":"Fix the date picker on small screens","updatedAt":"2026-11-11T00:54:13Z","url":"https://github.com/acme/tools/pull/1"},{"isDraft":true,"number":48,"repository":{"name":"notes","nameWithOwner":"octocat/notes"},"title":"feat: sync notes over the relay","updatedAt":"2026-10-23T22:23:38Z","url":"https://github.com/octocat/notes/pull/48"}]"#;

    /// `gh pr view 28 --json headRefOid,state,statusCheckRollup,mergeable,baseRefName,baseRefOid,reviewDecision`,
    /// the same: one check still queued, one passed.
    const VIEW_28: &str = r#"{"baseRefName":"main","baseRefOid":"1111111111111111111111111111111111111111","headRefOid":"2222222222222222222222222222222222222222","mergeable":"MERGEABLE","reviewDecision":"","state":"OPEN","statusCheckRollup":[{"__typename":"CheckRun","completedAt":"0001-01-01T00:00:00Z","conclusion":"","detailsUrl":"https://github.com/acme/app/actions/runs/101/job/201","name":"macOS","startedAt":"2026-11-16T20:11:18Z","status":"QUEUED","workflowName":"CI"},{"__typename":"CheckRun","completedAt":"2026-11-16T20:13:37Z","conclusion":"SUCCESS","detailsUrl":"https://github.com/acme/app/actions/runs/101/job/202","name":"Windows","startedAt":"2026-11-16T20:11:21Z","status":"COMPLETED","workflowName":"CI"}]}"#;

    /// An environment variable set for one test, and as it was again at its end.
    struct SetVar(&'static str, Option<std::ffi::OsString>);

    impl SetVar {
        fn new(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> SetVar {
            let before = std::env::var_os(name);
            // SAFETY: the core tests run one at a time (`--test-threads=1`).
            unsafe { std::env::set_var(name, value) };
            SetVar(name, before)
        }
    }

    impl Drop for SetVar {
        fn drop(&mut self) {
            // SAFETY: as in `new`.
            match self.1.take() {
                Some(value) => unsafe { std::env::set_var(self.0, value) },
                None => unsafe { std::env::remove_var(self.0) },
            }
        }
    }

    /// A stand-in for `gh` in a new temporary folder, which `GLASSCODE_GH` names until dropped.
    /// It prints the reply files next to it: `auth.json`, `<account>search.json` and
    /// `<account>view-<number>.json`, where `<account>` is the account the call is made as
    /// (`GLASSCODE_GITHUB_ACCOUNT`, empty for gh's active account). A missing file is a failure.
    fn stand_in(replies: &[(&str, &str)]) -> (tempfile::TempDir, [SetVar; 3]) {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in replies {
            std::fs::write(dir.path().join(name), body).unwrap();
        }
        #[cfg(windows)]
        let program = {
            let program = dir.path().join("gh.cmd");
            let script = "@echo off\r\nif \"%~1\"==\"auth\" goto auth\r\nif \"%~1\"==\"search\" goto search\r\nif \"%~1\"==\"pr\" goto view\r\nexit /b 1\r\n\
                :auth\r\ntype \"%~dp0auth.json\"\r\nexit /b %errorlevel%\r\n\
                :search\r\ntype \"%~dp0%GLASSCODE_GITHUB_ACCOUNT%search.json\"\r\nexit /b %errorlevel%\r\n\
                :view\r\ntype \"%~dp0%GLASSCODE_GITHUB_ACCOUNT%view-%~3.json\"\r\nexit /b %errorlevel%\r\n";
            std::fs::write(&program, script).unwrap();
            program
        };
        #[cfg(unix)]
        let program = {
            use std::os::unix::fs::PermissionsExt;
            let program = dir.path().join("gh");
            let script = "#!/bin/sh\ndir=$(dirname \"$0\")\ncase \"$1\" in\n\
                auth) exec cat \"$dir/auth.json\" ;;\n\
                search) exec cat \"$dir/${GLASSCODE_GITHUB_ACCOUNT}search.json\" ;;\n\
                pr) exec cat \"$dir/${GLASSCODE_GITHUB_ACCOUNT}view-$3.json\" ;;\n\
                esac\nexit 1\n";
            std::fs::write(&program, script).unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            program
        };
        // In a Glasscode session, this variable names the session's account: a call made as gh's
        // active account would see it, so it is empty while the stand-in is in use.
        let account = SetVar::new("GLASSCODE_GITHUB_ACCOUNT", "");
        let gh = SetVar::new("GLASSCODE_GH", &program);
        // The stand-in answers for gh: Glasscode asks it, not GitHub.
        let api = SetVar::new("GLASSCODE_GITHUB_API", "off");
        (dir, [account, gh, api])
    }

    #[test]
    fn reads_a_recorded_search_of_my_open_prs() {
        let list = super::parse_my_prs(MY_PRS.as_bytes()).unwrap();
        assert_eq!(list.len(), 3);
        let first = &list[0];
        assert_eq!((first.repo.as_str(), first.number, first.title.as_str()), ("acme/app", 28, "Show the build number in the title bar"));
        assert_eq!((first.url.as_str(), first.updated_at.as_str(), first.draft), ("https://github.com/acme/app/pull/28", "2026-11-16T20:26:05Z", false));
        assert!(list[2].draft);
        assert!(list.iter().all(|pr| pr.ci.is_none() && pr.review.is_empty()));
        // An empty reply is no pull requests; a reply that is not JSON is an error.
        assert!(super::parse_my_prs(b"[]").unwrap().is_empty());
        assert!(super::parse_my_prs(b"\r\n").unwrap().is_empty());
        assert!(super::parse_my_prs(b"error connecting to api.github.com").is_err());
    }

    #[test]
    fn my_open_prs_asks_gh_and_reads_checks_and_reviews() {
        // Two accounts; the second is not gh's active one, and its token cannot be had here (no
        // GitHub settings in this process), so only the first is asked.
        let auth = r#"{"hosts":{"github.com":[{"login":"octocat","active":true,"state":"success"},{"login":"hubot","active":false,"state":"success"}]}}"#;
        let view_48 = r#"{"headRefOid":"3333333333333333333333333333333333333333","state":"OPEN","statusCheckRollup":[],"mergeable":"UNKNOWN","baseRefName":"main","baseRefOid":"4444444444444444444444444444444444444444","reviewDecision":"APPROVED"}"#;
        // No reply for #1: its checks are unknown, and the list still comes.
        let (_dir, _gh) = stand_in(&[("auth.json", auth), ("search.json", MY_PRS), ("view-28.json", VIEW_28), ("view-48.json", view_48)]);
        let list = super::my_open_prs(&HashMap::new()).unwrap();
        assert_eq!(list.iter().map(|pr| pr.number).collect::<Vec<_>>(), [28, 1, 48]);
        let ci = list[0].ci.as_ref().expect("the checks of #28");
        assert_eq!((ci.checks.len(), ci.running(), ci.merges_cleanly(), list[0].review.as_str()), (2, true, true, ""));
        assert_eq!(list[1].ci, None);
        assert_eq!((list[2].review.as_str(), list[2].ci.as_ref().map(|ci| ci.checks.len())), ("APPROVED", Some(0)));
    }

    #[test]
    fn my_open_prs_of_two_accounts_come_together_newest_first() {
        let other = r#"[{"isDraft":false,"number":5,"repository":{"name":"tools","nameWithOwner":"acme/tools"},"title":"Add a dark icon","updatedAt":"2026-11-17T08:00:00Z","url":"https://github.com/acme/tools/pull/5"},
            {"isDraft":false,"number":28,"repository":{"name":"app","nameWithOwner":"acme/app"},"title":"Show the build number in the title bar","updatedAt":"2026-11-16T20:26:05Z","url":"https://github.com/acme/app/pull/28"}]"#;
        let view_5 = r#"{"headRefOid":"5555555555555555555555555555555555555555","state":"OPEN","statusCheckRollup":[],"mergeable":"CONFLICTING","baseRefName":"main","baseRefOid":"6666666666666666666666666666666666666666","reviewDecision":"CHANGES_REQUESTED"}"#;
        let mine = r#"[{"isDraft":false,"number":28,"repository":{"name":"app","nameWithOwner":"acme/app"},"title":"Show the build number in the title bar","updatedAt":"2026-11-16T20:26:05Z","url":"https://github.com/acme/app/pull/28"}]"#;
        let (_dir, _gh) = stand_in(&[("search.json", mine), ("hubotsearch.json", other), ("view-28.json", VIEW_28), ("hubotview-5.json", view_5)]);
        let accounts = vec![Vec::new(), vec![("GLASSCODE_GITHUB_ACCOUNT".to_string(), "hubot".to_string())]];
        let list = super::open_prs_as(&accounts, &HashMap::new()).unwrap();
        // #28 is in both replies and shows once; #5 is newer. Each is asked about as the account
        // that found it (the reply for #5 is only there for hubot).
        assert_eq!(list.iter().map(|pr| (pr.repo.as_str(), pr.number)).collect::<Vec<_>>(), [("acme/tools", 5), ("acme/app", 28)]);
        assert_eq!((list[0].review.as_str(), list[0].ci.as_ref().map(|ci| ci.conflicting())), ("CHANGES_REQUESTED", Some(true)));
        assert!(list[1].ci.is_some());
        // One account fails: the other's pull requests still come.
        let (_dir, _gh) = stand_in(&[("hubotsearch.json", other)]);
        assert_eq!(super::open_prs_as(&accounts, &HashMap::new()).unwrap().len(), 2);
    }

    #[test]
    fn my_open_prs_is_empty_for_an_empty_reply_and_an_error_when_gh_fails() {
        let (_dir, _gh) = stand_in(&[("auth.json", r#"{"hosts":{"github.com":[{"login":"octocat","active":true,"state":"success"}]}}"#), ("search.json", "[]")]);
        assert_eq!(super::my_open_prs(&HashMap::new()).unwrap(), Vec::new());
        // gh fails: not signed in, so no accounts and no search reply.
        let (_dir, _gh) = stand_in(&[]);
        assert!(super::my_open_prs(&HashMap::new()).is_err());
        // gh is not there at all.
        let gone = tempfile::tempdir().unwrap();
        let _gh = SetVar::new("GLASSCODE_GH", gone.path().join("gh-is-not-here"));
        assert!(super::my_open_prs(&HashMap::new()).is_err());
    }

    #[test]
    fn reads_a_merge() {
        let merged = serde_json::json!({ "state": "MERGED", "mergedAt": "2026-11-16T10:00:00Z", "additions": 120, "deletions": 30 });
        assert_eq!(super::merged_from_json(&merged), Some(super::Merged { at: super::parse_time("2026-11-16T10:00:00Z").unwrap(), lines: 150 }));
        assert_eq!(super::merged_from_json(&serde_json::json!({ "state": "CLOSED", "mergedAt": null, "additions": 1, "deletions": 1 })), None);
        assert_eq!(super::merged_from_json(&serde_json::json!({ "state": "OPEN" })), None);
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

    /// Every form of one repository is one remote (the project keys of Manager v2): https, ssh,
    /// the scp form, per-account host aliases, a `.git` suffix, a port and letter case.
    #[test]
    fn normalizes_remotes_of_every_form() {
        use super::normalize_remote;
        for form in [
            "git@github.com:acme/app.git",
            "git@github.com:Acme/App",
            "https://github.com/acme/app",
            "https://github.com/acme/app.git/",
            "https://user@github.com/acme/app.git",
            "http://github.com/acme/app",
            "ssh://git@github.com/acme/app.git",
            "ssh://git@github.com:22/acme/app",
            "git@github.com-work:acme/app.git",
            "ssh://git@gh-work/acme/app",
            "github.com-work:acme/app.git",
            "deploy@github.com:acme/app.git",
            "ssh://git@ssh.github.com:443/acme/app.git",
            "git@github.com:acme/app.git/",
            "https://github.com/acme/app/tree/main",
            "  git@github.com:acme/app.git\n",
            "git@github.com.:acme/app",
        ] {
            assert_eq!(normalize_remote(form).as_deref(), Some("github.com/acme/app"), "{form}");
        }
        // Other hosts keep their host, without the user, the port and the suffix.
        for form in ["git@gitlab.example.com:team/app.git", "git@GitLab.example.com.:team/app.git", "https://gitlab.example.com/Team/App", "ssh://git@gitlab.example.com:2222/team/app.git", "git+ssh://gitlab.example.com/team/app/"] {
            assert_eq!(normalize_remote(form).as_deref(), Some("gitlab.example.com/team/app"), "{form}");
        }
        assert_eq!(normalize_remote("https://git.example.com/group/sub/app.git").as_deref(), Some("git.example.com/group/sub/app"));
        assert_eq!(normalize_remote("ssh://git@[::1]:2222/team/app").as_deref(), Some("::1/team/app"));
        // An SSH alias that does not look like GitHub's keeps its name (the config is not read).
        assert_eq!(normalize_remote("git@work:acme/app.git").as_deref(), Some("work/acme/app"));
        // A server whose name only starts like an alias is its own, not github.com (v1's
        // `parse_slug` still takes it as GitHub, as before).
        assert_eq!(normalize_remote("https://github-enterprise.corp.example/acme/app").as_deref(), Some("github-enterprise.corp.example/acme/app"));
        assert_eq!(normalize_remote("git@gh-mirror.example.org:acme/app.git").as_deref(), Some("gh-mirror.example.org/acme/app"));
        assert_eq!(normalize_remote("git@github.com.evil.example:acme/app.git").as_deref(), Some("github.com.evil.example/acme/app"));
        assert_eq!(parse_slug("git@gh-mirror.example.org:acme/app.git").as_deref(), Some("acme/app"));
        // Local paths name a folder, not a repository.
        for local in ["/srv/git/app.git", "../app", "./app", "app", "file:///srv/app.git", r"C:\repos\app", "C:/repos/app", r"\\server\share\app", "", "https://github.com", "git@host:"] {
            assert_eq!(normalize_remote(local), None, "{local:?}");
        }
    }
}
