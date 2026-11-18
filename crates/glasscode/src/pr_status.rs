//! Per-session pull-request status: which PR a session's branch belongs to and its CI. The PR
//! panel, CI auto-fix, the sidebar and Today read it.
//!
//! Each checkout is asked about when it is due, not all of them on one timer ([`next_check`]):
//! - the one on screen often (every 15 s while its checks run or GitHub has not worked out
//!   whether it merges, else every minute), the others with running checks every minute, those
//!   with an open PR every 3 minutes, the rest every 5; four times slower while Glasscode is not
//!   the active app;
//! - at once when it was switched, pushed or fetched (`git_info::moved`, which reads only file
//!   times), when the person opens one of its sessions, and when they come back to Glasscode
//!   after a minute or more;
//! - after a failure, in 30 s, then twice as long each time (at most 10 minutes).
//!
//! The checkouts that are due go to GitHub together (`github_query::look_up`: one query per
//! account and repository); git is asked only about checkouts whose files changed
//! (`git_info::info`). A lookup that fails keeps what was known. The statuses are kept on disk
//! (`github_cache`) and shown at the start, unconfirmed: CI auto-fix, the auto-review and the
//! manager act only on a status GitHub confirmed in this run.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{Context, prelude::*};
use glasscode_core::git_info::{self, GitInfo};
use glasscode_core::github::{self, CiStatus, PullRequest};
use glasscode_core::github_query::{self, Lookup};
use glasscode_core::{github_api, github_cache};

use crate::workspace::Workspace;

/// How often the checkouts' file times are looked at for a switch, a push or a fetch.
const MOVES_EVERY: Duration = Duration::from_secs(3);
/// After a push GitHub takes a moment to see it: the checkout is asked about this much later.
const AFTER_MOVE: Duration = Duration::from_secs(3);
/// Back in Glasscode: a checkout not asked about for this long is asked again.
const BACK_AFTER: Duration = Duration::from_secs(60);
/// A session the person opens is asked about again when its status is older than this.
const OPEN_AFTER: Duration = Duration::from_secs(10);
/// The copy on disk is written at most this often.
const SAVE_EVERY: Duration = Duration::from_secs(15);

/// What GitHub says about the PR for a session's branch.
#[derive(Clone, Default)]
pub struct PrStatus {
    pub repo: Option<String>,
    pub branch: String,
    pub pr: Option<PullRequest>,
    pub error: Option<String>,
    /// CI on the PR's newest commit (not for review sessions: their PR is someone else's).
    pub ci: Option<CiStatus>,
    /// GitHub said so in this run. False for a status from the copy on disk (shown until GitHub
    /// answers): nothing acts on it.
    pub confirmed: bool,
}

impl PrStatus {
    /// Whether CI auto-fix and the manager may act on it: GitHub confirmed it in this run, and
    /// the last lookup did not fail (then what is shown is what was known before).
    pub fn acts(&self) -> bool {
        self.confirmed && self.error.is_none()
    }

    /// Something the person sees changed from `before`.
    fn differs(&self, before: Option<&PrStatus>) -> bool {
        before.is_none_or(|b| !b.confirmed || b.repo != self.repo || b.branch != self.branch || b.pr != self.pr || b.ci != self.ci || b.error != self.error)
    }
}

#[derive(Default)]
pub struct PrStatuses {
    pub map: HashMap<String, PrStatus>,
    running: bool,
    /// The open PR (repository, number) last seen on each of the person's own sessions. When it
    /// is no longer open there, GitHub is asked once whether it merged (Activity on Today). It
    /// stays while GitHub does not answer, so that a merge is not missed.
    open: HashMap<String, (String, u64)>,
    /// Per checkout: when it is due, when GitHub last answered for it, failures in a row.
    due: HashMap<PathBuf, Instant>,
    checked: HashMap<PathBuf, Instant>,
    fails: HashMap<PathBuf, u32>,
    moving: bool,
    moved_at: Option<Instant>,
    was_active: bool,
    /// The copy on disk: read (and what it held, which a save keeps until GitHub confirms
    /// better), behind what is known, last written.
    loaded: bool,
    disk: github_cache::Cache,
    pub(crate) dirty: bool,
    saved_at: Option<Instant>,
}

/// What GitHub said about a PR that is no longer open on its session (an error as text).
type MergeAnswer = Result<Option<github::Merged>, String>;

/// Whether the PR a session had open still is, as far as this refresh knows. A failed lookup
/// knows nothing; a session that was not looked up (closed, or its folder gone) is asked about.
fn still_open(watched: &(String, u64), status: Option<&PrStatus>) -> bool {
    match status {
        None => false,
        Some(status) if status.error.is_some() => true,
        Some(status) => status.repo.as_deref() == Some(watched.0.as_str()) && status.pr.as_ref().is_some_and(|pr| pr.number == watched.1),
    }
}

/// When a checkout is asked about again, after an answer (`fails` 0) or a failure.
pub(crate) fn next_check(status: Option<&PrStatus>, on_screen: bool, window_active: bool, fails: u32) -> Duration {
    if fails > 0 {
        let backoff = 30u64.saturating_mul(1 << (fails - 1).min(10));
        return Duration::from_secs(backoff.min(600));
    }
    let has_pr = status.is_some_and(|s| s.pr.is_some());
    // Settled: checks done and GitHub knows whether it merges.
    let settled = status.and_then(|s| s.ci.as_ref()).is_none_or(|ci| !ci.running() && ci.mergeable != "UNKNOWN");
    let secs = match (on_screen, has_pr, settled) {
        (true, false, _) => 30,
        (true, true, false) => 15,
        (true, true, true) => 60,
        (false, true, false) => 60,
        (false, true, true) => 180,
        (false, false, _) => 300,
    };
    let secs = if window_active { secs } else { (secs * 4).min(1200) };
    Duration::from_secs(secs)
}

/// The key of a lookup: a checkout, and the PR number of a review session in it.
fn lookup_key(worktree: &Path, review: Option<u64>) -> String {
    format!("{}|{}", worktree.display(), review.map(|n| n.to_string()).unwrap_or_default())
}

/// What one refresh found for a session: its status, a failure, or None when its folder is gone.
type SessionAnswer = (String, PathBuf, Option<Result<PrStatus, String>>);

impl Workspace {
    /// Every tick: ask GitHub about the checkouts that are due (see the module's notes).
    pub fn pr_tick(&mut self, cx: &mut Context<Self>) {
        if !self.prs.loaded {
            self.prs.loaded = true;
            self.github_cache_load(cx);
        }
        let now = Instant::now();
        if self.window_active && !self.prs.was_active {
            for (path, at) in &self.prs.checked {
                if at.elapsed() > BACK_AFTER {
                    self.prs.due.insert(path.clone(), now);
                }
            }
        }
        self.prs.was_active = self.window_active;
        self.pr_watch_moves(cx);
        self.github_cache_save(cx);
        if self.prs.running {
            return;
        }
        let due: Vec<PathBuf> = self.session_worktrees().into_iter().filter(|w| self.prs.due.get(w).is_none_or(|at| *at <= now)).collect();
        if !due.is_empty() {
            self.pr_refresh_these(due, cx);
        }
    }

    /// Ask about every checkout now (after something changed on GitHub that Glasscode did).
    pub fn pr_refresh(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        for worktree in self.session_worktrees() {
            self.prs.due.insert(worktree, now);
        }
        if !self.prs.running {
            self.pr_tick(cx);
        }
    }

    /// The person opens a session: its checkout is asked about at once unless GitHub answered
    /// for it a moment ago.
    pub fn pr_session_opened(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let Some(worktree) = self.state.session(session_id).map(|s| s.worktree.clone()) else { return };
        let fresh = self.prs.map.get(session_id).is_some_and(|s| s.confirmed) && self.prs.checked.get(&worktree).is_some_and(|at| at.elapsed() < OPEN_AFTER);
        if !fresh {
            self.prs.due.insert(worktree, Instant::now());
            if !self.prs.running {
                self.pr_tick(cx);
            }
        }
    }

    /// The checkouts of the sessions, each once.
    fn session_worktrees(&self) -> Vec<PathBuf> {
        let mut seen = HashSet::new();
        self.state.sessions.iter().filter(|s| seen.insert(s.worktree.clone())).map(|s| s.worktree.clone()).collect()
    }

    /// The checkout of the session on screen.
    fn worktree_on_screen(&self) -> Option<PathBuf> {
        (self.center == crate::center::Center::Session).then(|| self.active_session().map(|s| s.worktree.clone())).flatten()
    }

    /// In the background, every few seconds: the checkouts that were switched, pushed or
    /// fetched become due.
    fn pr_watch_moves(&mut self, cx: &mut Context<Self>) {
        if self.prs.moving || self.prs.moved_at.is_some_and(|t| t.elapsed() < MOVES_EVERY) {
            return;
        }
        self.prs.moving = true;
        self.prs.moved_at = Some(Instant::now());
        let worktrees = self.session_worktrees();
        let task = cx.background_spawn(async move { git_info::moved(&worktrees) });
        cx.spawn(async move |this, cx| {
            let moved = task.await;
            let _ = this.update(cx, |this, _| {
                this.prs.moving = false;
                let at = Instant::now() + AFTER_MOVE;
                for worktree in moved {
                    log::debug!("{} was switched, pushed or fetched: its pull request is asked about", worktree.display());
                    let due = this.prs.due.entry(worktree).or_insert(at);
                    if *due > at {
                        *due = at;
                    }
                }
            });
        })
        .detach();
    }

    /// Ask GitHub about these checkouts, all in one go, in the background.
    fn pr_refresh_these(&mut self, worktrees: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.prs.running = true;
        for worktree in &worktrees {
            self.prs.due.remove(worktree);
        }
        let jobs: Vec<(String, PathBuf, Option<u64>)> =
            self.state.sessions.iter().filter(|s| worktrees.contains(&s.worktree)).map(|s| (s.id.clone(), s.worktree.clone(), s.review.as_ref().map(|r| r.number))).collect();
        // The open PRs watched for a merge: of the sessions asked about now, and of sessions
        // that were closed (asked about once, then forgotten).
        let in_round: HashSet<&str> = jobs.iter().map(|(id, _, _)| id.as_str()).collect();
        let watched: Vec<(String, (String, u64))> = self
            .prs
            .open
            .iter()
            .filter(|(id, _)| in_round.contains(id.as_str()) || self.state.session(id).is_none())
            .map(|(id, pr)| (id.clone(), pr.clone()))
            .collect();
        let env = self.env.clone();
        let task = cx.background_spawn(async move {
            let started = Instant::now();
            let before = github_api::stats();
            // git, only for the checkouts whose files changed; None for a folder that is gone.
            let mut infos: HashMap<PathBuf, Option<GitInfo>> = HashMap::new();
            for (_, worktree, _) in &jobs {
                infos.entry(worktree.clone()).or_insert_with(|| worktree.is_dir().then(|| git_info::info(worktree)));
            }
            let mut lookups: Vec<Lookup> = Vec::new();
            let mut asked: HashSet<String> = HashSet::new();
            for (_, worktree, review) in &jobs {
                let Some(Some(info)) = infos.get(worktree) else { continue };
                let Some(repo) = &info.repo else { continue };
                let key = lookup_key(worktree, *review);
                if asked.insert(key.clone()) {
                    let candidates = if review.is_none() { info.candidates.clone() } else { Vec::new() };
                    lookups.push(Lookup { key, repo: repo.clone(), candidates, number: *review });
                }
            }
            let mut merges: Vec<(String, u64)> = watched.iter().map(|(_, pr)| pr.clone()).collect();
            merges.sort();
            merges.dedup();
            let answers = github_query::look_up(&lookups, &merges, &env);
            let mut out: Vec<SessionAnswer> = Vec::new();
            for (id, worktree, review) in jobs {
                let answer = match infos.get(&worktree) {
                    Some(Some(info)) => Some(match &info.repo {
                        None => Ok(PrStatus { repo: None, branch: info.branch.clone(), confirmed: true, ..PrStatus::default() }),
                        Some(repo) => match answers.found.get(&lookup_key(&worktree, review)) {
                            Some(Ok(found)) => Ok(PrStatus {
                                repo: Some(repo.clone()),
                                branch: info.branch.clone(),
                                pr: found.pr.clone(),
                                error: None,
                                ci: if review.is_none() { found.ci.clone() } else { None },
                                confirmed: true,
                            }),
                            Some(Err(err)) => Err(err.clone()),
                            None => Err("not asked".to_string()),
                        },
                    }),
                    _ => None,
                };
                out.push((id, worktree, answer));
            }
            let after = github_api::stats();
            log::debug!(
                "pull requests: {} checkouts in {} ms, {} queries ({} requests, {} points)",
                infos.len(),
                started.elapsed().as_millis(),
                answers.queries,
                after.requests - before.requests,
                after.points - before.points
            );
            (out, answers.merged, watched)
        });
        cx.spawn(async move |this, cx| {
            let (out, merged, watched) = task.await;
            let _ = this.update(cx, |this, cx| this.pr_answers(out, merged, watched, cx));
        })
        .detach();
    }

    fn pr_answers(&mut self, out: Vec<SessionAnswer>, merged: HashMap<(String, u64), MergeAnswer>, watched: Vec<(String, (String, u64))>, cx: &mut Context<Self>) {
        self.prs.running = false;
        let now = Instant::now();
        // Per checkout: its folder is there, and every lookup in it answered.
        let mut checkouts: HashMap<PathBuf, (bool, bool)> = HashMap::new();
        let mut gone: HashSet<String> = HashSet::new();
        let mut changed = false;
        for (id, worktree, answer) in out {
            let entry = checkouts.entry(worktree).or_insert((true, true));
            match answer {
                None => {
                    entry.0 = false;
                    gone.insert(id);
                }
                Some(Ok(status)) => {
                    let own = self.state.session(&id).is_some_and(|s| s.review.is_none());
                    if let (true, Some(repo), Some(pr)) = (own, &status.repo, &status.pr) {
                        self.prs.open.insert(id.clone(), (repo.clone(), pr.number));
                    }
                    changed |= status.differs(self.prs.map.get(&id));
                    self.prs.map.insert(id, status);
                }
                Some(Err(err)) => {
                    entry.1 = false;
                    // What was known stays; the failure shows.
                    let known = self.prs.map.entry(id).or_default();
                    changed |= known.error.as_deref() != Some(err.as_str());
                    known.error = Some(err);
                }
            }
        }
        // A watched PR no longer open on its session (or its session closed, or its folder
        // removed): merged, or closed without a merge?
        for (id, pr) in watched {
            let status = if gone.contains(&id) { None } else { self.prs.map.get(&id) };
            if still_open(&pr, status) {
                continue;
            }
            match merged.get(&pr) {
                Some(Ok(answer)) => {
                    if let Some(merged) = answer {
                        glasscode_core::activity::record_merge(merged.at, merged.lines, &format!("{}#{}", pr.0, pr.1));
                    }
                    if self.prs.open.get(&id) == Some(&pr) {
                        self.prs.open.remove(&id);
                    }
                }
                // Asked again at the next refresh.
                Some(Err(err)) => log::warn!("could not see whether {}#{} merged: {err}", pr.0, pr.1),
                None => {}
            }
        }
        let on_screen = self.worktree_on_screen();
        for (worktree, (there, answered)) in checkouts {
            let status = self.state.sessions.iter().filter(|s| s.worktree == worktree && s.review.is_none()).find_map(|s| self.prs.map.get(&s.id));
            let wait = if !there {
                Duration::from_secs(300)
            } else if answered {
                self.prs.fails.remove(&worktree);
                self.prs.checked.insert(worktree.clone(), now);
                next_check(status, on_screen.as_ref() == Some(&worktree), self.window_active, 0)
            } else {
                let fails = self.prs.fails.entry(worktree.clone()).or_insert(0);
                *fails += 1;
                next_check(status, false, true, *fails)
            };
            // A move or a refresh asked for while it was being asked about keeps its earlier time.
            let due = self.prs.due.entry(worktree).or_insert(now + wait);
            if *due > now + wait {
                *due = now + wait;
            }
        }
        if changed {
            self.prs.dirty = true;
            cx.notify();
        }
    }

    // ----- the copy on disk ----------------------------------------------------------------

    /// At the start, in the background: show the statuses and PR details kept on disk, until
    /// GitHub answers.
    fn github_cache_load(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move { github_cache::load() });
        cx.spawn(async move |this, cx| {
            let cache = task.await;
            let _ = this.update(cx, |this, cx| {
                for session in &this.state.sessions {
                    if this.prs.map.contains_key(&session.id) {
                        continue;
                    }
                    let review = session.review.as_ref().map(|r| r.number);
                    if let Some(kept) = cache.statuses.get(&lookup_key(&session.worktree, review)) {
                        this.prs.map.insert(
                            session.id.clone(),
                            PrStatus { repo: kept.repo.clone(), branch: kept.branch.clone(), pr: kept.pr.clone(), error: None, ci: kept.ci.clone(), confirmed: false },
                        );
                    }
                }
                for kept in &cache.details {
                    this.gh.details.entry((kept.repo.clone(), kept.details.number)).or_insert_with(|| kept.details.clone());
                }
                log::debug!("showing {} kept pull-request statuses and {} PR details until GitHub answers", cache.statuses.len(), cache.details.len());
                this.prs.disk = cache;
                cx.notify();
            });
        })
        .detach();
    }

    /// Keep what GitHub confirmed on disk, at most every [`SAVE_EVERY`], in the background. What
    /// was on disk and is not confirmed again yet (GitHub did not answer since the start) stays
    /// as it was, with its old time.
    fn github_cache_save(&mut self, cx: &mut Context<Self>) {
        if !self.prs.dirty || self.prs.saved_at.is_some_and(|t| t.elapsed() < SAVE_EVERY) {
            return;
        }
        self.prs.dirty = false;
        self.prs.saved_at = Some(Instant::now());
        let now = glasscode_core::model::now();
        let mut cache = self.prs.disk.clone();
        for session in &self.state.sessions {
            let Some(status) = self.prs.map.get(&session.id).filter(|s| s.confirmed && s.error.is_none()) else { continue };
            let at = self.prs.checked.get(&session.worktree).map_or(now, |t| now.saturating_sub(t.elapsed().as_secs()));
            let review = session.review.as_ref().map(|r| r.number);
            cache.statuses.insert(
                lookup_key(&session.worktree, review),
                github_cache::KeptStatus {
                    account: status.repo.as_deref().map(github_cache::account_label).unwrap_or_default(),
                    repo: status.repo.clone(),
                    branch: status.branch.clone(),
                    pr: status.pr.clone(),
                    ci: status.ci.clone(),
                    at,
                },
            );
        }
        for ((repo, _), details) in &self.gh.details {
            let Some(fetched) = self.gh.fetched.get(&(repo.clone(), details.number)) else { continue };
            cache.details.retain(|d| !(d.repo == *repo && d.details.number == details.number));
            cache.details.push(github_cache::KeptDetails {
                account: github_cache::account_label(repo),
                repo: repo.clone(),
                at: now.saturating_sub(fetched.elapsed().as_secs()),
                details: details.clone(),
            });
        }
        self.prs.disk = cache.clone();
        cx.background_spawn(async move {
            if let Err(err) = github_cache::save(cache) {
                log::warn!("could not keep the pull requests on disk: {err}");
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(repo: Option<&str>, pr: Option<u64>, error: Option<&str>) -> PrStatus {
        let pr = pr.map(|number| PullRequest {
            number,
            title: "Show the build number".into(),
            url: String::new(),
            base_ref_name: "main".into(),
            head_ref_name: "build-number".into(),
            is_draft: false,
            state: "OPEN".into(),
        });
        PrStatus { repo: repo.map(str::to_string), branch: "build-number".into(), pr, error: error.map(str::to_string), ci: None, confirmed: true }
    }

    #[test]
    fn a_pr_is_gone_when_a_good_lookup_no_longer_finds_it() {
        let watched = ("acme/app".to_string(), 7);
        assert!(still_open(&watched, Some(&status(Some("acme/app"), Some(7), None))));
        // Merged or closed: the branch has no open PR now.
        assert!(!still_open(&watched, Some(&status(Some("acme/app"), None, None))));
        // A new PR on the same branch.
        assert!(!still_open(&watched, Some(&status(Some("acme/app"), Some(9), None))));
        // GitHub did not answer: nothing is known.
        assert!(still_open(&watched, Some(&status(Some("acme/app"), None, Some("GitHub did not answer within 30s")))));
        // The session is closed, or its worktree removed after the merge.
        assert!(!still_open(&watched, None));
    }

    fn with_ci(running: bool, mergeable: &str) -> PrStatus {
        let check = github::CheckRun { name: "test".into(), status: if running { "IN_PROGRESS".into() } else { "COMPLETED".into() }, conclusion: if running { String::new() } else { "SUCCESS".into() }, ..Default::default() };
        PrStatus { ci: Some(CiStatus { checks: vec![check], mergeable: mergeable.into(), ..Default::default() }), ..status(Some("acme/app"), Some(7), None) }
    }

    #[test]
    fn the_checkout_on_screen_with_running_checks_is_asked_about_most_often() {
        let secs = |s: Option<&PrStatus>, on_screen: bool, active: bool, fails: u32| next_check(s, on_screen, active, fails).as_secs();
        let running = with_ci(true, "MERGEABLE");
        let settled = with_ci(false, "MERGEABLE");
        let unknown = with_ci(false, "UNKNOWN");
        let no_pr = status(Some("acme/app"), None, None);
        assert_eq!(secs(Some(&running), true, true, 0), 15);
        assert_eq!(secs(Some(&unknown), true, true, 0), 15, "GitHub has not worked out the merge yet");
        assert_eq!(secs(Some(&settled), true, true, 0), 60);
        assert_eq!(secs(Some(&no_pr), true, true, 0), 30);
        assert_eq!(secs(Some(&running), false, true, 0), 60);
        assert_eq!(secs(Some(&settled), false, true, 0), 180);
        assert_eq!(secs(Some(&no_pr), false, true, 0), 300);
        assert_eq!(secs(None, false, true, 0), 300);
        // Glasscode in the background: four times slower, at most 20 minutes.
        assert_eq!(secs(Some(&settled), false, false, 0), 720);
        assert_eq!(secs(Some(&no_pr), false, false, 0), 1200);
        // After failures: 30 s, then twice as long, at most 10 minutes.
        assert_eq!([1, 2, 3, 4, 5, 6, 20].map(|f| secs(Some(&settled), true, true, f)), [30, 60, 120, 240, 480, 600, 600]);
    }

    #[test]
    fn a_lookup_is_named_by_its_checkout_and_review() {
        assert_eq!(lookup_key(Path::new("/code/app"), None), "/code/app|");
        assert_eq!(lookup_key(Path::new("/code/app"), Some(958)), "/code/app|958");
    }
}
