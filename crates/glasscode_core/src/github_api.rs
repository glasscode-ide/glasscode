//! GitHub's API over long-lived HTTPS connections, for Glasscode's own reads (pull requests,
//! their checks and reviews). Asking `gh` costs a new process and a new TLS connection for each
//! question (about half a second), one question at a time; here the connections stay open and
//! many questions go in one GraphQL query (`github_query`).
//!
//! - **Accounts.** A repository's token is its account's (`github_accounts::account_for_repo`),
//!   else the environment's `GH_TOKEN`/`GITHUB_TOKEN`, else `gh`'s active account: the choice
//!   `github::gh_as` makes. Tokens stay in memory (`github_accounts` keeps them); none is logged
//!   or written. A token GitHub refuses is forgotten and asked for again once.
//! - **Per account.** Kept answers, ETags and limits belong to one account and are never used
//!   for another; all of them go when an account route changes (`github_accounts::generation`).
//! - **Conditional requests.** A REST answer is kept with its ETag and asked for again with
//!   `If-None-Match`: GitHub answers 304, which is quick and not counted against the hourly
//!   limit.
//! - **Limits.** An account GitHub limits (403 or 429 with `retry-after`, none left before
//!   `x-ratelimit-reset`, a secondary limit) is not asked again until then: the call fails at
//!   once, without a request. Nothing falls back to `gh` then, as `gh` spends the same limit.
//! - **Shared answers.** [`shared`] gives callers that ask the same thing at the same time one
//!   request, and a fresh answer to those that ask again soon after.
//!
//! - **Reaching GitHub.** A proxy in `HTTPS_PROXY`/`ALL_PROXY` is used, as `gh` does. Trust
//!   comes from the roots built in (webpki), not the system's store: where only the system
//!   trusts the way out (a proxy that inspects TLS), requests fail, and after two such failures
//!   in a row Glasscode asks `gh` for five minutes, then tries again.
//!
//! `GLASSCODE_GITHUB_API=off` turns all this off (Glasscode then asks `gh`, as before); a URL
//! there is another API base.

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

const API: &str = "https://api.github.com";
/// Kept REST answers (with their ETags) and shared answers, at most.
const KEEP_MAX: usize = 512;

/// Who a request is made as. `account` names it for caches and logs (a login, `@env` for a token
/// from the environment, `@active` for gh's active account); the token never leaves this module.
#[derive(Clone)]
pub struct Auth {
    pub account: String,
    token: String,
    source: Source,
}

#[derive(Clone, PartialEq)]
enum Source {
    Login(String),
    Env,
    Active,
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Auth({})", self.account)
    }
}

impl Auth {
    /// For tests: a made-up token for `account`.
    #[cfg(test)]
    pub(crate) fn test(account: &str) -> Auth {
        Auth { account: account.into(), token: "test_token_0123456789abcdef".into(), source: Source::Env }
    }

    /// A new token after GitHub refused this one (None: none to be had).
    fn renewed(&self, env: &HashMap<String, String>) -> Option<Auth> {
        match &self.source {
            Source::Login(login) => {
                crate::github_accounts::forget_token(Some(login));
                crate::github_accounts::token(login).map(|token| Auth { token, ..self.clone() })
            }
            Source::Active => {
                crate::github_accounts::forget_token(None);
                crate::github_accounts::active_token(env).map(|token| Auth { token, ..self.clone() })
            }
            Source::Env => None,
        }
    }
}

/// Whether Glasscode asks GitHub itself (else it asks `gh`): not switched off, and GitHub was
/// not out of reach a moment ago.
pub fn enabled() -> bool {
    std::env::var("GLASSCODE_GITHUB_API").map_or(true, |v| v.trim() != "off") && reachable()
}

/// Requests in a row that got no answer at all, and until when `gh` is asked instead.
static NO_ANSWER: AtomicU64 = AtomicU64::new(0);
static OUT_OF_REACH: Mutex<Option<Instant>> = Mutex::new(None);
/// No answer this many times in a row: `gh` is asked for [`OUT_OF_REACH_FOR`].
const NO_ANSWER_MAX: u64 = 2;
const OUT_OF_REACH_FOR: Duration = Duration::from_secs(300);

/// GitHub answered the last requests (or the pause after it did not is over).
pub fn reachable() -> bool {
    OUT_OF_REACH.lock().unwrap().is_none_or(|until| Instant::now() >= until)
}

fn answered() {
    NO_ANSWER.store(0, Ordering::Relaxed);
}

fn no_answer(err: &str) {
    if NO_ANSWER.fetch_add(1, Ordering::Relaxed) + 1 >= NO_ANSWER_MAX {
        NO_ANSWER.store(0, Ordering::Relaxed);
        log::warn!("GitHub cannot be reached directly ({err}); asking through gh for {} minutes", OUT_OF_REACH_FOR.as_secs() / 60);
        *OUT_OF_REACH.lock().unwrap() = Some(Instant::now() + OUT_OF_REACH_FOR);
    }
}

fn base() -> String {
    match std::env::var("GLASSCODE_GITHUB_API") {
        Ok(v) if v.starts_with("http://") || v.starts_with("https://") => v.trim_end_matches('/').to_string(),
        _ => API.to_string(),
    }
}

/// The account and token a repository (`owner/name`, or "" for no repository) is read as, or
/// None when Glasscode cannot ask GitHub itself (switched off, or no token to be had).
pub fn auth_for(repo: &str, env: &HashMap<String, String>) -> Option<Auth> {
    if !enabled() {
        return None;
    }
    if let Some(login) = crate::github_accounts::account_for_repo(repo) {
        if let Some(token) = crate::github_accounts::token(&login) {
            return Some(Auth { account: login.clone(), token, source: Source::Login(login) });
        }
        // As `gh_as`: an account with no token leaves the environment's and gh's own.
    }
    auth_default(env)
}

/// The account `login` (signed in to `gh`), or gh's active account for None.
pub fn auth_as(login: Option<&str>, env: &HashMap<String, String>) -> Option<Auth> {
    if !enabled() {
        return None;
    }
    match login {
        Some(login) => crate::github_accounts::token(login).map(|token| Auth { account: login.to_string(), token, source: Source::Login(login.to_string()) }),
        None => auth_default(env),
    }
}

fn auth_default(env: &HashMap<String, String>) -> Option<Auth> {
    for key in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Some(token) = env.get(key).map(|t| t.trim()).filter(|t| crate::github_accounts::looks_like_token(t)) {
            return Some(Auth { account: "@env".into(), token: token.to_string(), source: Source::Env });
        }
    }
    let token = crate::github_accounts::active_token(env)?;
    Some(Auth { account: "@active".into(), token, source: Source::Active })
}

// ----- errors -------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum ApiError {
    /// 404: no such thing, or this account cannot see it.
    NotFound,
    /// GitHub limits this account until then.
    Limited { account: String, for_secs: u64 },
    /// The token was refused, and no new one could be had.
    Unauthorized,
    Status(u16, String),
    /// GraphQL answered with errors and no data.
    Query(String),
    /// No answer (network, timeout, TLS).
    Transport(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::NotFound => write!(f, "GitHub: not found"),
            ApiError::Limited { account, for_secs } => write!(f, "GitHub limits {account} for {for_secs} s more; asked again then"),
            ApiError::Unauthorized => write!(f, "GitHub refused the token (run `gh auth login` again)"),
            ApiError::Status(code, message) => write!(f, "GitHub answered {code}: {message}"),
            ApiError::Query(message) => write!(f, "GitHub: {message}"),
            ApiError::Transport(message) => write!(f, "could not reach GitHub: {message}"),
        }
    }
}

impl std::error::Error for ApiError {}

// ----- what is kept, per account -----------------------------------------------------------------

#[derive(Default)]
struct Kept {
    /// The routes' generation this was kept under.
    generation: u64,
    /// (account, URL) → (ETag, answer, when it was kept).
    etags: HashMap<(String, String), (String, Value, Instant)>,
    /// Account → not asked again before then.
    limited: HashMap<String, Instant>,
    /// Key → a shared answer (see [`shared`]).
    shared: HashMap<String, Arc<Mutex<Option<(Instant, Arc<dyn Any + Send + Sync>)>>>>,
}

static KEPT: Mutex<Option<Kept>> = Mutex::new(None);

/// What is kept, emptied first when the routes changed.
fn kept<R>(f: impl FnOnce(&mut Kept) -> R) -> R {
    let mut guard = KEPT.lock().unwrap();
    let kept = guard.get_or_insert_with(Kept::default);
    let generation = crate::github_accounts::generation();
    if kept.generation != generation {
        *kept = Kept { generation, ..Kept::default() };
    }
    f(kept)
}

/// Requests made, of them answered 304, and the GraphQL points they cost (`rateLimit.cost`),
/// since the start: for the log of each refresh.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub requests: u64,
    pub not_modified: u64,
    pub points: u64,
}

static REQUESTS: AtomicU64 = AtomicU64::new(0);
static NOT_MODIFIED: AtomicU64 = AtomicU64::new(0);
static POINTS: AtomicU64 = AtomicU64::new(0);

pub fn stats() -> Stats {
    Stats { requests: REQUESTS.load(Ordering::Relaxed), not_modified: NOT_MODIFIED.load(Ordering::Relaxed), points: POINTS.load(Ordering::Relaxed) }
}

/// How long GitHub still limits `account` (None: not limited).
pub fn limited_for(account: &str) -> Option<Duration> {
    kept(|k| k.limited.get(account).and_then(|until| until.checked_duration_since(Instant::now())))
}

fn limit(account: &str, for_secs: u64) {
    let for_secs = for_secs.clamp(1, 3600);
    log::warn!("GitHub limits {account}: not asked again for {for_secs} s");
    kept(|k| k.limited.insert(account.to_string(), Instant::now() + Duration::from_secs(for_secs)));
}

fn check_limit(account: &str) -> Result<(), ApiError> {
    match limited_for(account) {
        Some(left) => Err(ApiError::Limited { account: account.to_string(), for_secs: left.as_secs().max(1) }),
        None => Ok(()),
    }
}

/// Forget what is kept about a pull request (`owner/name#number`, any account): after a comment,
/// a label or a re-run, the next read asks GitHub.
pub fn forget_pr(repo: &str, number: u64) {
    let end = format!(" {}#{number}", repo.to_lowercase());
    kept(|k| k.shared.retain(|key, _| !key.ends_with(&end)));
}

/// Forget what is kept about every pull request of a repository.
pub fn forget_repo(repo: &str) {
    let part = format!(" {}#", repo.to_lowercase());
    kept(|k| k.shared.retain(|key, _| !key.contains(&part)));
}

/// The key of a pull request's shared answer: `kind`, the account, then `owner/name#number`
/// (which [`forget_pr`] finds).
pub fn pr_key(kind: &str, auth: &Auth, repo: &str, number: u64) -> String {
    format!("{kind} {} {}#{number}", auth.account, repo.to_lowercase())
}

/// One request for callers that ask for `key` at the same time, and the same answer for those
/// that ask again within `fresh_for`. A failure is not kept.
pub fn shared<T: Clone + Send + Sync + 'static>(key: &str, fresh_for: Duration, fetch: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
    let slot = kept(|k| {
        if k.shared.len() >= KEEP_MAX {
            // Bounded: drop the answers nobody is waiting on.
            k.shared.retain(|_, slot| Arc::strong_count(slot) > 1);
        }
        k.shared.entry(key.to_string()).or_default().clone()
    });
    // Callers of the same key wait here for the first one's answer.
    let mut slot = slot.lock().unwrap();
    if let Some((at, value)) = slot.as_ref() {
        if at.elapsed() < fresh_for {
            if let Some(value) = value.downcast_ref::<T>() {
                return Ok(value.clone());
            }
        }
    }
    let value = fetch()?;
    *slot = Some((Instant::now(), Arc::new(value.clone())));
    Ok(value)
}

// ----- requests ----------------------------------------------------------------------------------

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(30))
            .timeout_write(Duration::from_secs(30))
            .max_idle_connections_per_host(4)
            .try_proxy_from_env(true)
            .user_agent(concat!("Glasscode/", env!("CARGO_PKG_VERSION")))
            .build()
    })
}

struct Reply {
    status: u16,
    etag: Option<String>,
    body: String,
}

/// Seconds since the epoch, now.
fn epoch_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// How long GitHub asks to wait, from the headers of a refused request: `retry-after`, else the
/// reset of a used-up limit, else a minute (a secondary limit says neither).
fn wait_secs(retry_after: Option<&str>, remaining: Option<&str>, reset: Option<&str>) -> u64 {
    if let Some(secs) = retry_after.and_then(|v| v.trim().parse::<u64>().ok()) {
        return secs;
    }
    if remaining.map(str::trim) == Some("0") {
        if let Some(reset) = reset.and_then(|v| v.trim().parse::<u64>().ok()) {
            return reset.saturating_sub(epoch_now()) + 1;
        }
    }
    60
}

/// One request as `auth`, with a new token once if GitHub refused this one.
fn send(base: &str, auth: &Auth, env: &HashMap<String, String>, method: &str, path: &str, body: Option<&str>, etag: Option<&str>) -> Result<Reply, ApiError> {
    match send_once(base, auth, method, path, body, etag) {
        Err(ApiError::Unauthorized) => match auth.renewed(env) {
            Some(again) => send_once(base, &again, method, path, body, etag),
            None => Err(ApiError::Unauthorized),
        },
        other => other,
    }
}

fn send_once(base: &str, auth: &Auth, method: &str, path: &str, body: Option<&str>, etag: Option<&str>) -> Result<Reply, ApiError> {
    check_limit(&auth.account)?;
    let url = format!("{base}{path}");
    let mut request = agent()
        .request(method, &url)
        .set("Authorization", &format!("Bearer {}", auth.token))
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", "2022-11-28");
    if let Some(etag) = etag {
        request = request.set("If-None-Match", etag);
    }
    REQUESTS.fetch_add(1, Ordering::Relaxed);
    let result = match body {
        Some(body) => request.set("Content-Type", "application/json").send_string(body),
        None => request.call(),
    };
    let response = match result {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(ureq::Error::Transport(err)) => {
            let err = err.to_string();
            no_answer(&err);
            return Err(ApiError::Transport(err));
        }
    };
    answered();
    let status = response.status();
    let header = |name: &str| response.header(name).map(str::to_string);
    let (retry_after, remaining, reset) = (header("retry-after"), header("x-ratelimit-remaining"), header("x-ratelimit-reset"));
    let etag = header("etag");
    let body = response.into_string().unwrap_or_default();
    match status {
        401 => Err(ApiError::Unauthorized),
        403 | 429 if status == 429 || retry_after.is_some() || remaining.as_deref() == Some("0") || body.to_lowercase().contains("rate limit") => {
            let secs = wait_secs(retry_after.as_deref(), remaining.as_deref(), reset.as_deref());
            limit(&auth.account, secs);
            Err(ApiError::Limited { account: auth.account.clone(), for_secs: secs })
        }
        404 => Err(ApiError::NotFound),
        200..=299 | 304 => {
            if status == 304 {
                NOT_MODIFIED.fetch_add(1, Ordering::Relaxed);
            }
            // The last request of the hour may succeed: the next would not.
            if remaining.as_deref().map(str::trim) == Some("0") {
                limit(&auth.account, wait_secs(None, remaining.as_deref(), reset.as_deref()));
            }
            Ok(Reply { status, etag, body })
        }
        _ => {
            let message = serde_json::from_str::<Value>(&body).ok().and_then(|v| v["message"].as_str().map(str::to_string)).unwrap_or_else(|| body.chars().take(200).collect());
            Err(ApiError::Status(status, message))
        }
    }
}

/// A GraphQL query as `auth`. Its `data`, also when some parts of it failed (a pull request
/// that is gone answers null and an error): only a query with no data at all is an error.
/// A query that asks for `rateLimit { cost }` adds its cost to [`stats`].
pub fn graphql(auth: &Auth, env: &HashMap<String, String>, query: &str, variables: Value) -> Result<Value, ApiError> {
    graphql_at(&base(), auth, env, query, variables)
}

pub(crate) fn graphql_at(base: &str, auth: &Auth, env: &HashMap<String, String>, query: &str, variables: Value) -> Result<Value, ApiError> {
    let body = serde_json::json!({ "query": query, "variables": variables }).to_string();
    let reply = send(base, auth, env, "POST", "/graphql", Some(&body), None)?;
    let value: Value = serde_json::from_str(&reply.body).map_err(|e| ApiError::Transport(format!("not JSON: {e}")))?;
    let errors: Vec<&Value> = value["errors"].as_array().map(|e| e.iter().collect()).unwrap_or_default();
    if errors.iter().any(|e| e["type"] == "RATE_LIMITED") {
        limit(&auth.account, 60);
        return Err(ApiError::Limited { account: auth.account.clone(), for_secs: 60 });
    }
    if let Some(cost) = value["data"]["rateLimit"]["cost"].as_u64() {
        POINTS.fetch_add(cost, Ordering::Relaxed);
    }
    if value["data"].is_null() || !value["data"].is_object() {
        let message = errors.iter().filter_map(|e| e["message"].as_str()).collect::<Vec<_>>().join("; ");
        return Err(ApiError::Query(if message.is_empty() { "no data in the answer".into() } else { message }));
    }
    if !errors.is_empty() {
        log::debug!("GitHub answered in part: {}", errors.iter().filter_map(|e| e["message"].as_str()).collect::<Vec<_>>().join("; "));
    }
    Ok(value["data"].clone())
}

/// A REST read as `auth` (`path` like `/repos/o/n/labels/x`), asked with the ETag of the last
/// answer: an unchanged one comes back as 304, quickly and without counting against the limit.
pub fn rest_get(auth: &Auth, env: &HashMap<String, String>, path: &str) -> Result<Value, ApiError> {
    rest_get_at(&base(), auth, env, path)
}

pub(crate) fn rest_get_at(base: &str, auth: &Auth, env: &HashMap<String, String>, path: &str) -> Result<Value, ApiError> {
    let key = (auth.account.clone(), path.to_string());
    let known = kept(|k| k.etags.get(&key).map(|(etag, value, _)| (etag.clone(), value.clone())));
    let reply = send(base, auth, env, "GET", path, None, known.as_ref().map(|(etag, _)| etag.as_str()))?;
    if reply.status == 304 {
        if let Some((_, value)) = known {
            return Ok(value);
        }
    }
    let value: Value = serde_json::from_str(&reply.body).map_err(|e| ApiError::Transport(format!("not JSON: {e}")))?;
    if let Some(etag) = reply.etag {
        kept(|k| {
            if k.etags.len() >= KEEP_MAX {
                // Bounded: the older half goes.
                let mut ages: Vec<Instant> = k.etags.values().map(|(_, _, at)| *at).collect();
                ages.sort();
                let cut = ages[ages.len() / 2];
                k.etags.retain(|_, (_, _, at)| *at > cut);
            }
            k.etags.insert(key, (etag, value.clone(), Instant::now()));
        });
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    /// A request the stand-in server saw: its first line and headers (lower case).
    #[derive(Clone, Debug)]
    struct Seen {
        line: String,
        headers: HashMap<String, String>,
        body: String,
    }

    /// A server on 127.0.0.1 that answers each request with the next of `replies` (status,
    /// extra headers, body) and records what it was asked.
    fn server(replies: Vec<(u16, Vec<(&'static str, String)>, String)>) -> (String, Arc<Mutex<Vec<Seen>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        std::thread::spawn(move || {
            for (status, headers, body) in replies {
                let Ok((stream, _)) = listener.accept() else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut seen_headers = HashMap::new();
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    let header = header.trim_end();
                    if header.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = header.split_once(':') {
                        seen_headers.insert(k.trim().to_lowercase(), v.trim().to_string());
                    }
                }
                let length: usize = seen_headers.get("content-length").and_then(|l| l.parse().ok()).unwrap_or(0);
                let mut request_body = vec![0; length];
                reader.read_exact(&mut request_body).unwrap();
                record.lock().unwrap().push(Seen { line: line.trim_end().to_string(), headers: seen_headers, body: String::from_utf8_lossy(&request_body).into_owned() });
                let mut stream = stream;
                let mut reply = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
                for (k, v) in headers {
                    reply.push_str(&format!("{k}: {v}\r\n"));
                }
                reply.push_str("\r\n");
                reply.push_str(&body);
                stream.write_all(reply.as_bytes()).unwrap();
            }
        });
        (url, seen)
    }

    #[test]
    fn a_rest_read_is_asked_again_with_its_etag_and_a_304_gives_the_kept_answer() {
        let (url, seen) = server(vec![
            (200, vec![("ETag", "\"abc\"".into())], r#"{"name":"preview"}"#.into()),
            (304, vec![("ETag", "\"abc\"".into())], String::new()),
        ]);
        let auth = Auth::test("etag-account");
        let env = HashMap::new();
        let first = rest_get_at(&url, &auth, &env, "/repos/o/n/labels/preview").unwrap();
        let again = rest_get_at(&url, &auth, &env, "/repos/o/n/labels/preview").unwrap();
        assert_eq!(first, again);
        assert_eq!(again["name"], "preview");
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].headers.get("if-none-match"), None);
        assert_eq!(seen[1].headers.get("if-none-match").map(String::as_str), Some("\"abc\""));
        // The token goes as a bearer token, and the version GitHub documents.
        assert_eq!(seen[0].headers.get("authorization").map(String::as_str), Some("Bearer test_token_0123456789abcdef"));
        assert_eq!(seen[0].headers.get("x-github-api-version").map(String::as_str), Some("2022-11-28"));
    }

    #[test]
    fn an_etag_of_one_account_is_not_sent_for_another() {
        let (url, seen) = server(vec![
            (200, vec![("ETag", "\"one\"".into())], "{}".into()),
            (200, vec![("ETag", "\"two\"".into())], "{}".into()),
        ]);
        let env = HashMap::new();
        rest_get_at(&url, &Auth::test("account-a"), &env, "/repos/o/n/deployments").unwrap();
        rest_get_at(&url, &Auth::test("account-b"), &env, "/repos/o/n/deployments").unwrap();
        assert_eq!(seen.lock().unwrap()[1].headers.get("if-none-match"), None);
    }

    #[test]
    fn a_limited_account_is_not_asked_until_github_says() {
        let (url, seen) = server(vec![(403, vec![("Retry-After", "120".into()), ("x-ratelimit-remaining", "0".into())], r#"{"message":"API rate limit exceeded"}"#.into())]);
        let auth = Auth::test("limited-account");
        let env = HashMap::new();
        let first = rest_get_at(&url, &auth, &env, "/repos/o/n/labels/x").unwrap_err();
        assert!(matches!(first, ApiError::Limited { for_secs: 120, .. }), "{first:?}");
        // The next call fails at once, with no request.
        let second = graphql_at(&url, &auth, &env, "query { viewer { login } }", Value::Null).unwrap_err();
        assert!(matches!(second, ApiError::Limited { .. }), "{second:?}");
        assert_eq!(seen.lock().unwrap().len(), 1);
        // Another account is not limited by it.
        assert!(limited_for("another-account").is_none());
        assert!(limited_for("limited-account").is_some_and(|d| d.as_secs() > 100));
    }

    #[test]
    fn graphql_gives_partial_data_and_counts_its_cost() {
        let (url, seen) = server(vec![
            (200, vec![], r#"{"data":{"rateLimit":{"cost":1},"repository":{"a":null,"b":{"number":7}}},"errors":[{"message":"Could not resolve to a PullRequest with the number of 1."}]}"#.into()),
            (200, vec![], r#"{"data":null,"errors":[{"message":"Field 'x' doesn't exist"}]}"#.into()),
        ]);
        let auth = Auth::test("graphql-account");
        let env = HashMap::new();
        let before = stats().points;
        let data = graphql_at(&url, &auth, &env, "query($o:String!){x}", serde_json::json!({"o":"me"})).unwrap();
        assert_eq!(data["repository"]["b"]["number"], 7);
        assert!(data["repository"]["a"].is_null());
        assert!(stats().points > before);
        let err = graphql_at(&url, &auth, &env, "query { x }", Value::Null).unwrap_err();
        assert_eq!(err, ApiError::Query("Field 'x' doesn't exist".into()));
        // The query and its variables go as JSON.
        let sent: Value = serde_json::from_str(&seen.lock().unwrap()[0].body).unwrap();
        assert_eq!(sent["variables"]["o"], "me");
        assert!(seen.lock().unwrap()[0].line.starts_with("POST /graphql "));
    }

    #[test]
    fn not_found_and_other_answers_are_told_apart() {
        let (url, _) = server(vec![(404, vec![], r#"{"message":"Not Found"}"#.into()), (500, vec![], r#"{"message":"boom"}"#.into())]);
        let auth = Auth::test("status-account");
        let env = HashMap::new();
        assert_eq!(rest_get_at(&url, &auth, &env, "/x").unwrap_err(), ApiError::NotFound);
        assert_eq!(rest_get_at(&url, &auth, &env, "/y").unwrap_err(), ApiError::Status(500, "boom".into()));
    }

    #[test]
    fn a_refused_token_from_the_environment_is_not_retried() {
        let (url, seen) = server(vec![(401, vec![], r#"{"message":"Bad credentials"}"#.into())]);
        let err = rest_get_at(&url, &Auth::test("refused-account"), &HashMap::new(), "/x").unwrap_err();
        assert_eq!(err, ApiError::Unauthorized);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn shared_answers_one_request_for_callers_at_the_same_time() {
        let calls = Arc::new(AtomicU64::new(0));
        let threads: Vec<_> = (0..6)
            .map(|_| {
                let calls = calls.clone();
                std::thread::spawn(move || {
                    shared("details test o/n#1", Duration::from_secs(5), || {
                        calls.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(50));
                        Ok(42u32)
                    })
                    .unwrap()
                })
            })
            .collect();
        for thread in threads {
            assert_eq!(thread.join().unwrap(), 42);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Forgotten after a change on GitHub: asked again.
        forget_pr("O/N", 1);
        shared("details test o/n#1", Duration::from_secs(5), || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(43u32)
        })
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // A failure is not kept.
        assert!(shared::<u32>("details test o/n#2", Duration::from_secs(5), || anyhow::bail!("down")).is_err());
        assert_eq!(shared("details test o/n#2", Duration::from_secs(5), || Ok(1u32)).unwrap(), 1);
    }

    #[test]
    fn the_wait_comes_from_retry_after_then_the_reset() {
        assert_eq!(wait_secs(Some("30"), Some("0"), Some("1")), 30);
        let reset = (epoch_now() + 100).to_string();
        let secs = wait_secs(None, Some("0"), Some(&reset));
        assert!((99..=102).contains(&secs), "{secs}");
        assert_eq!(wait_secs(None, Some("12"), Some(&reset)), 60);
    }

    #[test]
    fn no_answer_twice_in_a_row_hands_over_to_gh_for_a_while() {
        // A port nobody listens on: no answer at all.
        let closed = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", listener.local_addr().unwrap())
        };
        let auth = Auth::test("reach-account");
        let env = HashMap::new();
        *OUT_OF_REACH.lock().unwrap() = None;
        answered();
        assert!(matches!(rest_get_at(&closed, &auth, &env, "/x"), Err(ApiError::Transport(_))));
        assert!(reachable(), "one failure is not enough");
        // An answer in between starts the count again.
        let (url, _) = server(vec![(200, vec![], "{}".into())]);
        rest_get_at(&url, &auth, &env, "/y").unwrap();
        assert!(matches!(rest_get_at(&closed, &auth, &env, "/x"), Err(ApiError::Transport(_))));
        assert!(reachable());
        assert!(matches!(rest_get_at(&closed, &auth, &env, "/x"), Err(ApiError::Transport(_))));
        assert!(!reachable());
        assert!(auth_as(None, &env).is_none(), "asked through gh now");
        *OUT_OF_REACH.lock().unwrap() = None;
    }

    #[test]
    fn a_token_is_never_in_debug_output() {
        let auth = Auth::test("someone");
        assert_eq!(format!("{auth:?}"), "Auth(someone)");
    }
}
