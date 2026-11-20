//! A local reviewer for pull requests that ask for your review. Opt in per repository
//! (Settings › Automation › Auto-review): for each open PR on an opted-in repo that GitHub says asks for
//! your review, Glasscode runs a headless review with your default agent — reading the PR's
//! diff through `gh` — and posts the overview back as a comment, once per head commit.
//!
//! It only ever comments; it never approves, requests changes, or merges. Off by default, and
//! the per-repository opt-in is the consent gate for posting to GitHub.
//!
//! What was posted is kept in `~/.glasscode/auto-review.json` (`repo#number` → head commit),
//! so a restart does not review the same commit again. A new commit on the PR is a new head,
//! so it is reviewed afresh; a new comment is not (the head is unchanged).
//!
//! The review instructions live in one place (`review_instructions`) and can be overridden by
//! `~/.glasscode/review-prompt.md` — the first step toward user-defined review plugins.

use std::collections::{HashMap, HashSet};

use gpui::{AppContext, Context};
use glasscode_core::model::AgentKind;
use glasscode_core::{agents, github};

use crate::workspace::Workspace;

#[derive(Default)]
pub struct AutoReview {
    /// `repo#number` → the head commit we last posted a review for.
    posted: HashMap<String, String>,
    /// Reviews running right now (at most one), so a tick does not start a second.
    in_flight: HashSet<String>,
    /// `repo#number` → the PR's `updated_at` we last examined, so an unchanged PR is not
    /// re-fetched every tick. In memory only.
    checked: HashMap<String, String>,
}

fn path() -> std::path::PathBuf {
    glasscode_core::paths::root().join("auto-review.json")
}

impl AutoReview {
    pub fn load() -> AutoReview {
        AutoReview {
            posted: std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default(),
            ..Default::default()
        }
    }

    fn save(&self) {
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.posted) {
            let _ = std::fs::write(path(), bytes);
        }
    }
}

fn key(repo: &str, number: u64) -> String {
    format!("{repo}#{number}")
}

fn short_repo(repo: &str) -> &str {
    repo.rsplit('/').next().unwrap_or(repo)
}

/// What the agent is asked to do. A `~/.glasscode/review-prompt.md` file, if present and not
/// empty, replaces the default (with `{repo}` and `{number}` substituted).
fn review_instructions(repo: &str, number: u64) -> String {
    let custom = glasscode_core::paths::root().join("review-prompt.md");
    if let Ok(text) = std::fs::read_to_string(&custom) {
        let text = text.trim();
        if !text.is_empty() {
            return text.replace("{repo}", repo).replace("{number}", &number.to_string());
        }
    }
    format!(
        "You are reviewing GitHub pull request #{number} in {repo}. \
         Run `gh pr view {number} --repo {repo}` for its title and description and \
         `gh pr diff {number} --repo {repo}` to read the full diff. Then write a short review \
         for the author in Markdown: one or two sentences on what the pull request does, then a \
         bulleted list of what is worth a close look — likely bugs, risky changes, missing \
         tests, and anything unclear — each pointing at the file it concerns. Be specific and \
         concise. Do not approve, request changes, or merge; this is only a comment. Output \
         only the review, with no preamble."
    )
}

/// The brief of a review run in project `key` when v2's memory brief is on: the project's, with
/// the review as its work. None with v1, or when memory cannot be read. It runs git.
fn headless_brief(key: &glasscode_core::paths::ProjectKey, repo: &str, number: u64) -> Option<String> {
    use glasscode_core::memstore::{Memory, brief};
    if !glasscode_core::store::EngineSwitch::current().is_on("brief") {
        return None;
    }
    let work = glasscode_core::identity::WorkKey { project: key.clone(), kind: glasscode_core::identity::WorkKind::Review, id: format!("{}#{number}", repo.to_lowercase()) };
    let snap = Memory::open().snapshot().ok()?;
    let what = brief::BriefFor { project: Some(key), work: Some(&work), today: glasscode_core::day::iso(glasscode_core::day::local((glasscode_core::clock::now_ms() / 1000) as i64)), ..brief::BriefFor::default() };
    brief::build(&snap, &what, &|_| true).ok().map(|brief| brief.text)
}

/// The comment body: the overview, clearly marked as an automated local comment.
fn review_body(overview: &str) -> String {
    format!(
        "🔭 **Glasscode auto-review**\n\n{overview}\n\n---\n<sub>Local, automated overview from \
         Glasscode — a comment, not an approval. Turn it off for this repository in Glasscode → \
         Settings › Automation › Auto-review.</sub>"
    )
}

impl Workspace {
    /// Whether Glasscode may auto-review pull requests in this repository.
    pub fn auto_review_on(&self, repo: &str) -> bool {
        self.state.auto_review_repos.contains(repo)
    }

    /// The repositories to offer in Settings: where you currently have a review request, plus
    /// any already opted in (so you can turn one off even with no open PR).
    pub fn auto_review_repos(&self) -> Vec<String> {
        let mut repos: Vec<String> = self.tickets.reviews.iter().map(|r| r.repo.clone()).collect();
        repos.extend(self.state.auto_review_repos.iter().cloned());
        repos.sort();
        repos.dedup();
        repos
    }

    pub fn auto_review_tick(&mut self, cx: &mut Context<Self>) {
        // A review runs Claude or Codex on its own (`agents::headless`): not in a test copy with
        // GLASSCODE_DEBUG_NO_AGENTS (debug builds).
        if self.state.auto_review_repos.is_empty() || crate::workspace::no_agents() {
            return;
        }
        // One review at a time: cheap, and it avoids a burst of comments.
        if !self.auto_review.in_flight.is_empty() {
            return;
        }
        // The first opted-in, non-draft review request not already examined at this update.
        let candidate = self.tickets.reviews.iter().find(|r| {
            self.state.auto_review_repos.contains(&r.repo)
                && !r.hit.draft
                && self.auto_review.checked.get(&key(&r.repo, r.hit.number)).map(String::as_str) != Some(r.hit.updated_at.as_str())
        });
        let Some(r) = candidate.cloned() else { return };

        // The account to review as: the default Claude, else the default Codex. None means no
        // agent is signed in yet — leave it unexamined and try again once one is.
        let (agent, config_dir) = if let Some(account) = self.state.default_account(AgentKind::Claude) {
            (AgentKind::Claude, account.config_dir())
        } else if let Some(account) = self.state.default_account(AgentKind::Codex) {
            (AgentKind::Codex, account.config_dir())
        } else {
            return;
        };

        let repo = r.repo.clone();
        let number = r.hit.number;
        let id = key(&repo, number);
        let updated_at = r.hit.updated_at.clone();
        let posted = self.auto_review.posted.get(&id).cloned();
        let instructions = review_instructions(&repo, number);
        // v2's memory (mem-m5): a headless run gets its project's brief too, read-only, ahead of
        // its task. With v1, or the brief off, nothing changes.
        let memory_key = self.state.projects.iter().find(|p| self.tickets_repo_of(&p.root).as_deref() == Some(repo.as_str())).and_then(|p| self.control.project_keys.get(&p.id).cloned());
        let env = self.env.clone();
        self.auto_review.in_flight.insert(id.clone());

        let task = {
            let repo = repo.clone();
            cx.background_spawn(async move {
                let head = github::head_oid(&repo, number, &env)?;
                if posted.as_deref() == Some(head.as_str()) {
                    // Already reviewed this commit (the PR only changed in some other way).
                    return Ok::<Option<String>, anyhow::Error>(None);
                }
                let instructions = match memory_key.as_ref().and_then(|key| headless_brief(key, &repo, number)) {
                    Some(brief) => format!("{brief}\n\nMemory is read-only for this run.\n\n{instructions}"),
                    None => instructions,
                };
                let overview = agents::headless(agent, config_dir.as_deref(), &instructions, &env)?;
                if overview.trim().len() < 20 {
                    anyhow::bail!("the review came back empty");
                }
                github::comment(&repo, number, &review_body(&overview), &env)?;
                Ok(Some(head))
            })
        };

        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.auto_review.in_flight.remove(&id);
                // Mark examined so we don't re-fetch until the PR changes again (even on a
                // failure — the toast says why, and a new commit will clear the gate).
                this.auto_review.checked.insert(id.clone(), updated_at);
                let short = short_repo(&repo).to_string();
                match result {
                    Ok(Some(head)) => {
                        this.auto_review.posted.insert(id, head);
                        this.auto_review.save();
                        this.show_toast(format!("Reviewed PR #{number} in {short}: posted an overview on GitHub."), cx);
                        cx.notify();
                    }
                    Ok(None) => {}
                    Err(err) => {
                        this.show_toast(format!("Auto-review of #{number} in {short} did not post: {err:#}"), cx);
                    }
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_marks_it_automated_and_not_an_approval() {
        let body = review_body("Looks good overall.");
        assert!(body.contains("Glasscode auto-review"));
        assert!(body.contains("not an approval"));
        assert!(body.contains("Looks good overall."));
    }

    #[test]
    fn default_instructions_name_the_pr_and_forbid_approving() {
        // It reads `review-prompt.md` of the Glasscode folder: the test's own, never the person's.
        glasscode_core::paths::use_test_root();
        let text = review_instructions("octo/repo", 42);
        assert!(text.contains("#42"));
        assert!(text.contains("octo/repo"));
        assert!(text.contains("Do not approve"));
    }

    #[test]
    fn key_and_short_repo() {
        assert_eq!(key("octo/repo", 7), "octo/repo#7");
        assert_eq!(short_repo("octo/repo"), "repo");
        assert_eq!(short_repo("repo"), "repo");
    }
}
