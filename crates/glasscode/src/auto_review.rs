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
use glasscode_core::{agents, github, review};

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
        // v2's memory (mem-m5): a headless run gets its project's brief too, read-only, ahead of
        // its task. With v1, or the brief off, nothing changes.
        let project = self.state.projects.iter().find(|p| self.tickets_repo_of(&p.root).as_deref() == Some(repo.as_str())).cloned();
        let memory_key = project.as_ref().and_then(|p| self.control.project_keys.get(&p.id).cloned());
        let env = self.env.clone();
        self.auto_review.in_flight.insert(id.clone());

        // M23 (trust-m2, alt-m1): Glasscode reads the pull request itself and hands it to a run with
        // no tools as data; the run answers in a fixed shape; the comment is Glasscode's template.
        let task = {
            let repo = repo.clone();
            cx.background_spawn(async move {
                let pr = read_pull_request(&repo, number, &env)?;
                if posted.as_deref() == Some(pr.head.as_str()) {
                    // Already reviewed this commit (the PR only changed in some other way).
                    return Ok::<Option<(review::PullRequest, String, bool)>, anyhow::Error>(None);
                }
                let (instructions, prompt_waiting) = review::instructions(&repo, number);
                let instructions = match memory_key.as_ref().and_then(|key| headless_brief(key, &repo, number)) {
                    Some(brief) => format!("{brief}\n\nMemory is read-only for this run.\n\n{instructions}"),
                    None => instructions,
                };
                // The run's schema and answer: in Glasscode's cache, which sessions are denied.
                let dir = glasscode_core::paths::cache_dir().join("auto-review").join(format!("{}-{number}", repo.replace('/', "-")));
                std::fs::create_dir_all(&dir)?;
                let schema = review::schema();
                let (schema_file, answer_file) = (dir.join("schema.json"), dir.join("answer.json"));
                let _ = std::fs::remove_file(&answer_file);
                std::fs::write(&schema_file, schema.to_string())?;
                let profile = glasscode_core::profile::headless_profile(glasscode_core::profile::HeadlessKind::AutoReview, agent, &schema, &schema_file, &answer_file);
                let out = agents::headless_run(agent, config_dir.as_deref(), &profile, &review::input(&instructions, &pr), Some(&answer_file), &env)?;
                let found = review::parse(&out).ok_or_else(|| anyhow::anyhow!("the review did not come back in its shape"))?;
                Ok(Some((pr, review::comment(&found), prompt_waiting)))
            })
        };

        let opted_in = self.auto_review_on(&repo);
        let mine: Vec<String> = self.github.list.as_ref().map(|list| list.iter().map(|a| a.login.clone()).collect()).unwrap_or_default();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.auto_review.in_flight.remove(&id);
                // Mark examined so we don't re-fetch until the PR changes again (even on a
                // failure — the toast says why, and a new commit will clear the gate).
                this.auto_review.checked.insert(id.clone(), updated_at);
                let short = short_repo(&repo).to_string();
                match result {
                    Ok(Some((pr, body, prompt_waiting))) => {
                        if prompt_waiting {
                            this.show_toast("Your review prompt changed, so the built-in one was used: approve yours in Settings › Automation › Auto-review.", cx);
                        }
                        this.auto_review_post(pr, body, opted_in, &mine, project.as_ref().map(|p| p.name.clone()), cx);
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

    /// Post a review: at once when its author is trusted (D27) and the repository opted in; else
    /// as `pr.comment`, from auto-review, which waits on a card with the full comment.
    fn auto_review_post(&mut self, pr: review::PullRequest, body: String, opted_in: bool, mine: &[String], project: Option<String>, cx: &mut Context<Self>) {
        let (id, head, number, short) = (key(&pr.repo, pr.number), pr.head.clone(), pr.number, short_repo(&pr.repo).to_string());
        match review::post_rule(&pr, mine, &[], opted_in) {
            review::Post::Now => {
                let (repo, env) = (pr.repo.clone(), self.env.clone());
                let task = cx.background_spawn(async move { github::comment(&repo, number, &body, &env) });
                cx.spawn(async move |this, cx| {
                    let posted = task.await;
                    let _ = this.update(cx, |this, cx| match posted {
                        Ok(()) => {
                            this.auto_review.posted.insert(id, head);
                            this.auto_review.save();
                            this.show_toast(format!("Reviewed PR #{number} in {short}: posted an overview on GitHub."), cx);
                        }
                        Err(err) => this.show_toast(format!("Auto-review of #{number} in {short} did not post: {err:#}"), cx),
                    });
                })
                .detach();
            }
            review::Post::Card => {
                let Some(project) = project else {
                    self.show_toast(format!("The review of #{number} in {short} is ready, but no project here holds that repository, so it was not posted."), cx);
                    return;
                };
                let op = match glasscode_core::ops::typed("pr.comment", serde_json::json!({ "number": number, "text": body, "project": project })) {
                    Ok(op) => op,
                    Err(error) => return self.show_toast(error.text, cx),
                };
                let caller = crate::control_ui::Caller {
                    who: "Glasscode's auto-review".into(),
                    channel: glasscode_core::caller::Caller::Manager { lane: glasscode_core::caller::Lane::Wake, wake: Some("auto-review".into()), project: None },
                    session: None,
                    session_title: None,
                    agent: None,
                    cwd: None,
                    project: None,
                    drive: None,
                };
                let done = move |ws: &mut Workspace, reply: &glasscode_core::control::Reply| {
                    if let glasscode_core::control::Reply::Done { code, .. } = reply
                        && *code == glasscode_core::control::code::OK
                    {
                        ws.auto_review.posted.insert(id, head);
                        ws.auto_review.save();
                    }
                };
                self.with_main_window(cx, move |ws, window, cx| ws.run_op(&op, caller, done, window, cx));
            }
        }
    }
}

/// The pull request as the review reads it: title, description, author and head from GitHub's
/// API (else `gh`), and its diff. It talks to the network: never on the main thread.
pub(crate) fn read_pull_request(repo: &str, number: u64, env: &std::collections::HashMap<String, String>) -> anyhow::Result<review::PullRequest> {
    let rest = glasscode_core::github_api::auth_for(repo, env).and_then(|auth| glasscode_core::github_api::rest_get(&auth, env, &format!("/repos/{repo}/pulls/{number}")).ok());
    let diff = glasscode_core::reads::pr_diff(repo, number, env)?;
    match rest {
        Some(value) => Ok(review::PullRequest::from_rest(repo, number, &value, diff)),
        None => {
            let view = glasscode_core::reads::pr_view(repo, number, env)?;
            let head = github::head_oid(repo, number, env)?;
            Ok(review::PullRequest {
                repo: repo.to_string(),
                number,
                title: view["title"].as_str().unwrap_or_default().to_string(),
                body: view["body"].as_str().unwrap_or_default().to_string(),
                author: view["author"]["login"].as_str().unwrap_or_default().to_string(),
                // Unknown: never trusted, so the comment waits on a card.
                association: None,
                head,
                diff,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_and_short_repo() {
        assert_eq!(key("octo/repo", 7), "octo/repo#7");
        assert_eq!(short_repo("octo/repo"), "repo");
        assert_eq!(short_repo("repo"), "repo");
    }
}
