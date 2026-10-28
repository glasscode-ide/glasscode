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
            out
        });
        cx.spawn(async move |this, cx| {
            let results = task.await;
            let _ = this.update(cx, |this, cx| {
                this.prs.running = false;
                for (id, status) in results {
                    this.prs.map.insert(id, status);
                }
                cx.notify();
            });
        })
        .detach();
    }
}
