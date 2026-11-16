//! Per-session pull-request status: which PR a session's branch belongs to and its CI, read
//! from GitHub (the `gh` CLI). The PR panel, CI auto-fix and the Today board read this.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{Context, prelude::*};
use glasscode_core::git;
use glasscode_core::github::{self, CiStatus, PullRequest};

use crate::workspace::Workspace;

/// What GitHub says about the PR for a session's branch.
#[derive(Clone, Default)]
pub struct PrStatus {
    pub repo: Option<String>,
    pub branch: String,
    pub pr: Option<PullRequest>,
    pub error: Option<String>,
    /// CI on the PR's newest commit (not for review sessions: their PR is someone else's).
    pub ci: Option<CiStatus>,
}

#[derive(Default)]
pub struct PrStatuses {
    pub map: HashMap<String, PrStatus>,
    running: bool,
    last: Option<Instant>,
    /// The open PR (repository, number) last seen on each of the person's own sessions. When it
    /// is no longer open there, GitHub is asked once whether it merged (Activity on Today). It
    /// stays while GitHub does not answer, so that a merge is not missed.
    open: HashMap<String, (String, u64)>,
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

impl Workspace {
    /// Every few seconds: refresh each session's PR status in the background.
    pub fn pr_tick(&mut self, cx: &mut Context<Self>) {
        if self.prs.running || self.prs.last.is_some_and(|t| t.elapsed() < Duration::from_secs(20)) {
            return;
        }
        self.pr_refresh(cx);
    }

    pub fn pr_refresh(&mut self, cx: &mut Context<Self>) {
        if self.prs.running {
            return;
        }
        self.prs.last = Some(Instant::now());
        self.prs.running = true;
        let jobs: Vec<(String, PathBuf, Option<u64>)> = self
            .state
            .sessions
            .iter()
            .filter(|s| s.worktree.is_dir())
            .map(|s| (s.id.clone(), s.worktree.clone(), s.review.as_ref().map(|r| r.number)))
            .collect();
        let watched: Vec<(String, (String, u64))> = self.prs.open.iter().map(|(id, pr)| (id.clone(), pr.clone())).collect();
        let env = self.env.clone();
        let task = cx.background_spawn(async move {
            let mut out: Vec<(String, PrStatus)> = Vec::new();
            let mut cache: HashMap<(String, String), PrStatus> = HashMap::new();
            for (id, worktree, reviewing) in jobs {
                let branch = git::current_branch(&worktree);
                let repo = github::repo_slug(&worktree);
                // Per worktree: two detached checkouts share the branch name "HEAD".
                let key = (repo.clone().unwrap_or_default(), worktree.display().to_string());
                if let Some(status) = cache.get(&key) {
                    out.push((id, status.clone()));
                    continue;
                }
                let mut status = PrStatus { repo: repo.clone(), branch: branch.clone(), pr: None, error: None, ci: None };
                if let Some(repo) = &repo {
                    let found = match reviewing {
                        Some(number) => github::pr_by_number(repo, number, &env),
                        None => github::pr_for_checkout(repo, &worktree, &branch, &env),
                    };
                    match found {
                        Ok(pr) => status.pr = pr,
                        Err(err) => status.error = Some(err.to_string()),
                    }
                    if let (Some(pr), None) = (&status.pr, reviewing) {
                        status.ci = github::pr_ci(repo, pr.number, &env).ok();
                    }
                }
                cache.insert(key, status.clone());
                out.push((id, status));
            }
            // A PR that is no longer open on its session: merged, or closed without a merge?
            let mut gone: Vec<(String, (String, u64), MergeAnswer)> = Vec::new();
            let mut asked: HashMap<(String, u64), MergeAnswer> = HashMap::new();
            for (id, pr) in watched {
                if still_open(&pr, out.iter().find(|(i, _)| *i == id).map(|(_, s)| s)) {
                    continue;
                }
                let answer = asked.entry(pr.clone()).or_insert_with(|| github::pr_merged(&pr.0, pr.1, &env).map_err(|e| format!("{e:#}"))).clone();
                gone.push((id, pr, answer));
            }
            (out, gone)
        });
        cx.spawn(async move |this, cx| {
            let (results, gone) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.prs.running = false;
                for (id, pr, answer) in gone {
                    match answer {
                        Ok(merged) => {
                            if let Some(merged) = merged {
                                glasscode_core::activity::record_merge(merged.at, merged.lines, &format!("{}#{}", pr.0, pr.1));
                            }
                            if this.prs.open.get(&id) == Some(&pr) {
                                this.prs.open.remove(&id);
                            }
                        }
                        // Asked again at the next refresh.
                        Err(err) => log::warn!("could not see whether {}#{} merged: {err}", pr.0, pr.1),
                    }
                }
                for (id, status) in results {
                    let own = this.state.session(&id).is_some_and(|s| s.review.is_none());
                    if let (true, Some(repo), Some(pr)) = (own, &status.repo, &status.pr) {
                        this.prs.open.insert(id.clone(), (repo.clone(), pr.number));
                    }
                    this.prs.map.insert(id, status);
                }
                cx.notify();
            });
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
        PrStatus { repo: repo.map(str::to_string), branch: "build-number".into(), pr, error: error.map(str::to_string), ci: None }
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
}
