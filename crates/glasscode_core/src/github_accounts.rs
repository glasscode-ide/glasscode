//! Which GitHub account works on which repository. `gh` keeps the person's accounts (several
//! may be signed in); Glasscode has a default one and lets a project use another. Sessions in
//! a project get its account as `GH_TOKEN` (git pushes through `gh auth git-credential`, and
//! `gh` itself, both honour it) and, when set, that account's commit email; Glasscode's own
//! GitHub lookups and git fetches for the repository use it too.
//!
//! Tokens come from `gh auth token --user <login>` when needed and stay in memory: Glasscode
//! never writes them anywhere. Nothing is set when no account is chosen (then `gh`'s active
//! account is used, as before), and never on a server.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Glasscode's GitHub settings (in `state.json`; a project's own choice is on the project).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GithubSettings {
    /// The account every repository uses unless its project says otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// The email commits are made with, per account; none: git's own `user.email`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub emails: BTreeMap<String, String>,
}

/// An account `gh` is signed in to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GhAccount {
    pub login: String,
    /// `gh`'s own active account (what it uses outside Glasscode).
    pub active: bool,
    /// Signed in and working (false: the token was revoked or expired).
    pub ok: bool,
}

#[derive(Default)]
struct Routes {
    default: Option<String>,
    /// Repository (`owner/name`) → its project's account.
    by_repo: HashMap<String, String>,
    emails: BTreeMap<String, String>,
    gh: Option<PathBuf>,
}

static ROUTES: RwLock<Option<Routes>> = RwLock::new(None);
static TOKENS: Mutex<Option<HashMap<String, (String, Instant)>>> = Mutex::new(None);

/// Tell this process who works where (the Mac app, whenever settings or projects change).
pub fn configure(settings: &GithubSettings, by_repo: HashMap<String, String>, env: &HashMap<String, String>) {
    let routes = Routes { default: settings.default.clone(), by_repo, emails: settings.emails.clone(), gh: gh_program(env) };
    *ROUTES.write().unwrap() = Some(routes);
}

fn gh_program(env: &HashMap<String, String>) -> Option<PathBuf> {
    std::env::var("GLASSCODE_GH").ok().filter(|p| !p.is_empty()).map(PathBuf::from).or_else(|| crate::agents::resolve_program("gh", env))
}

/// The accounts `gh` is signed in to on github.com.
pub fn accounts(env: &HashMap<String, String>) -> anyhow::Result<Vec<GhAccount>> {
    let gh = gh_program(env).ok_or_else(|| anyhow::anyhow!("GitHub CLI (gh) is not installed"))?;
    let mut cmd = std::process::Command::new(gh);
    cmd.envs(env).env_remove("GH_TOKEN").env_remove("GITHUB_TOKEN").args(["auth", "status", "--hostname", "github.com", "--json", "hosts"]);
    let output = crate::github::output_within(cmd, Duration::from_secs(15))?;
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|_| anyhow::anyhow!("gh is not signed in to GitHub (run `gh auth login`)"))?;
    Ok(value["hosts"]["github.com"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| Some(GhAccount { login: a["login"].as_str()?.to_string(), active: a["active"].as_bool().unwrap_or(false), ok: a["state"] == "success" }))
        .collect())
}

/// The account for a repository (`owner/name`): its project's, else the default.
pub fn account_for_repo(repo: &str) -> Option<String> {
    let routes = ROUTES.read().unwrap();
    let routes = routes.as_ref()?;
    routes.by_repo.get(&repo.to_lowercase()).cloned().or_else(|| routes.default.clone())
}

/// The environment that makes git and `gh` act as `login`: its token, its commit email when
/// one is set, and its name for anyone curious (`GLASSCODE_GITHUB_ACCOUNT`). Empty when the
/// token cannot be had (then `gh`'s active account is used).
pub fn env_for(login: &str) -> Vec<(String, String)> {
    let Some(token) = token(login) else { return Vec::new() };
    let mut env = vec![("GH_TOKEN".to_string(), token), ("GLASSCODE_GITHUB_ACCOUNT".to_string(), login.to_string())];
    let email = ROUTES.read().unwrap().as_ref().and_then(|r| r.emails.get(login).cloned()).filter(|e| !e.trim().is_empty());
    if let Some(email) = email {
        env.push(("GIT_AUTHOR_EMAIL".into(), email.clone()));
        env.push(("GIT_COMMITTER_EMAIL".into(), email));
    }
    env
}

/// [`env_for`] the account of a repository (`owner/name`).
pub fn env_for_repo(repo: &str) -> Vec<(String, String)> {
    account_for_repo(repo).map(|login| env_for(&login)).unwrap_or_default()
}

/// [`env_for`] the account of the repository a checkout belongs to.
pub fn env_for_dir(dir: &Path) -> Vec<(String, String)> {
    if ROUTES.read().unwrap().as_ref().is_none_or(|r| r.default.is_none() && r.by_repo.is_empty()) {
        return Vec::new();
    }
    crate::github::repo_slug(dir).map(|repo| env_for_repo(&repo)).unwrap_or_default()
}

/// `login`'s token from `gh` (its keychain), kept in memory for ten minutes.
fn token(login: &str) -> Option<String> {
    let mut cache = TOKENS.lock().unwrap();
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some((token, at)) = cache.get(login) {
        if at.elapsed() < Duration::from_secs(600) {
            return Some(token.clone());
        }
    }
    let gh = ROUTES.read().unwrap().as_ref().and_then(|r| r.gh.clone())?;
    let mut cmd = std::process::Command::new(gh);
    cmd.env_remove("GH_TOKEN").env_remove("GITHUB_TOKEN").args(["auth", "token", "--hostname", "github.com", "--user", login]);
    let output = crate::github::output_within(cmd, Duration::from_secs(10)).ok()?;
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || token.is_empty() {
        log::warn!("gh has no token for GitHub account {login}");
        return None;
    }
    cache.insert(login.to_string(), (token.clone(), Instant::now()));
    Some(token)
}

/// GitHub's private address for an account (`<id>+<login>@users.noreply.github.com`), which
/// credits commits to it without showing a real address.
pub fn noreply_email(login: &str) -> Option<String> {
    let gh = ROUTES.read().unwrap().as_ref().and_then(|r| r.gh.clone())?;
    let mut cmd = std::process::Command::new(gh);
    cmd.envs(env_for(login)).args(["api", "user", "--jq", ".id"]);
    let output = crate::github::output_within(cmd, Duration::from_secs(15)).ok()?;
    let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && id.chars().all(|c| c.is_ascii_digit()) && !id.is_empty()).then(|| format!("{id}+{login}@users.noreply.github.com"))
}
