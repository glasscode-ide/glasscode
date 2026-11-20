//! Moving in (M35): the dialog that asks first, reads what the person ticked, and shows what it
//! proposes for memory, grouped by project and source, to accept in bulk or line by line, or to
//! undo as a whole. The work is `glasscode_core::movein`'s, off the main thread; this is the
//! dialog. It exists only with the v2 engine (memory is v2's): with v1 nothing of it shows.

use std::collections::BTreeMap;

use gpui::{AnyElement, AppContext as _, ClickEvent, Context, IntoElement, InteractiveElement as _, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _};
use glasscode_core::memstore::Memory;
use glasscode_core::store::Store as _;
use glasscode_core::movein::{self, Found, ProjectIn};

use crate::commands::plural;
use crate::dialogs::Modal;
use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

/// Where the dialog is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Listing names and paths.
    Looking,
    /// The person ticks what may be read.
    Consent,
    /// Reading the ticked sources and proposing.
    Reading,
    /// What was proposed, to accept or drop.
    Review,
    /// Part 2 (M54): each project studied, one at a time within the budget, and the questions.
    Study,
}

/// The dialog's state (`Workspace::movein`), kept while it is hidden.
pub struct MoveIn {
    pub stage: Stage,
    pub found: Vec<Found>,
    /// This run's id, once it read.
    pub run: Option<String>,
    /// Waiting proposals of the run: id, text, target file, source.
    pub waiting: Vec<(String, String, String, String)>,
    /// How many it proposed and how many it left out.
    pub counts: (usize, usize),
    pub accepted: usize,
    pub busy: bool,
    pub error: Option<String>,
    /// The projects as moving in sees them (with their memory keys).
    pub projects: Vec<ProjectIn>,
    /// Part 2: the study and its questions (`manager/movein.json`).
    pub progress: Option<movein::Progress>,
    /// The answer box of the question asked now.
    pub answer: Option<gpui::Entity<crate::text_input::TextInput>>,
    /// A study runs now.
    pub studying: bool,
    /// The wait of a paused study; dropping it stops the wait.
    pub resume: Option<gpui::Task<()>>,
    /// The dialog is not shown: a paused study resumed at start.
    pub quiet: bool,
    /// Each project's study estimated, after the review shows (M35).
    pub estimates: Vec<(glasscode_core::paths::ProjectKey, movein::Estimate)>,
    pub estimate_line: Option<String>,
    /// What an import of an export did ("From an export").
    pub note: Option<String>,
    /// Each organisation's repository, to read its GitHub teams as the account of its projects.
    pub org_repos: Vec<(String, String)>,
}

impl Workspace {
    /// Open moving in: list what there is to read (names and paths only), then ask (v2 only).
    pub fn open_move_in(&mut self, cx: &mut Context<Self>) {
        // A study under way is shown as it is, not started again.
        if let Some(form) = self.movein.as_mut()
            && (form.stage == Stage::Study || form.studying)
        {
            form.quiet = false;
            self.modal = Some(Modal::MoveIn);
            cx.notify();
            return;
        }
        self.load_move_in(false, cx);
    }

    /// At start (v2): a study left paused or questions left open go on by themselves, without the
    /// dialog. It reads the progress in the background; nothing shows unless there is something.
    pub fn resume_move_in(&mut self, cx: &mut Context<Self>) {
        if self.movein.is_some() {
            return;
        }
        // A study left paused, or questions left open (answered in the chat's topic too).
        let task = cx.background_spawn(async move { movein::Progress::load().map(|p| p.unfinished()).unwrap_or(false) });
        cx.spawn(async move |this, cx| {
            if task.await {
                let _ = this.update(cx, |ws, cx| {
                    if ws.movein.is_none() {
                        ws.load_move_in(true, cx);
                    }
                });
            }
        })
        .detach();
    }

    fn load_move_in(&mut self, quiet: bool, cx: &mut Context<Self>) {
        self.movein = Some(MoveIn { stage: Stage::Looking, found: Vec::new(), run: None, waiting: Vec::new(), counts: (0, 0), accepted: 0, busy: true, error: None, projects: Vec::new(), progress: None, answer: None, studying: false, resume: None, quiet, estimates: Vec::new(), estimate_line: None, note: None, org_repos: Vec::new() });
        if !quiet {
            self.modal = Some(Modal::MoveIn);
        }
        cx.notify();
        // The projects used most recently first: their levels are asked, and they are studied first.
        let mut last_used: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
        for session in &self.state.sessions {
            let at = last_used.entry(session.project_id.as_str()).or_insert(0);
            *at = (*at).max(session.created_at);
        }
        let mut order: Vec<&glasscode_core::model::Project> = self.state.projects.iter().collect();
        order.sort_by_key(|p| std::cmp::Reverse(last_used.get(p.id.as_str()).copied().unwrap_or(0)));
        let order: Vec<String> = order.into_iter().map(|p| p.id.clone()).collect();
        // What looking needs, from the window: no IO here.
        let projects: Vec<(String, String, std::path::PathBuf, Vec<std::path::PathBuf>, Option<String>, Option<glasscode_core::paths::ProjectKey>)> = order
            .iter()
            .filter_map(|id| self.state.project(id))
            .filter(|p| !self.state.is_hidden(&p.id))
            .map(|p| {
                let worktrees = self.worktrees.get(&p.id).map(|list| list.iter().map(|w| w.path.clone()).collect()).unwrap_or_default();
                let owner = self.tickets_repo_of(&p.root).and_then(|repo| repo.split('/').next().map(str::to_string));
                (p.id.clone(), p.name.clone(), p.root.clone(), worktrees, owner, self.control.project_keys.get(&p.id).cloned())
            })
            .collect();
        let mut org_repos: Vec<(String, String)> = Vec::new();
        for p in self.state.projects.iter().filter(|p| !self.state.is_hidden(&p.id)) {
            if let Some(repo) = self.tickets_repo_of(&p.root)
                && let Some(owner) = repo.split('/').next().map(str::to_string)
                && !org_repos.iter().any(|(o, _)| o.eq_ignore_ascii_case(&owner))
            {
                org_repos.push((owner, repo));
            }
        }
        if let Some(form) = self.movein.as_mut() {
            form.org_repos = org_repos;
        }
        let claude: Vec<std::path::PathBuf> = self.state.accounts.iter().filter(|a| a.agent == glasscode_core::model::AgentKind::Claude).map(|a| a.effective_dir()).collect();
        let codex: Vec<std::path::PathBuf> = self.state.accounts.iter().filter(|a| a.agent == glasscode_core::model::AgentKind::Codex).map(|a| a.effective_dir()).collect();
        let logins: Vec<String> = self.github.list.as_ref().map(|list| list.iter().map(|a| a.login.clone()).collect()).unwrap_or_default();
        let task = cx.background_spawn(async move {
            let mut ins = Vec::new();
            for (id, name, root, worktrees, owner, key) in projects {
                // A project's key is bound only to a folder that is there (M11, M20.1).
                let key = match key {
                    Some(key) => Some(key),
                    None => match glasscode_core::memstore::bind_project(&id, &root) {
                        Ok(glasscode_core::identity::KeyMatch::Known(key) | glasscode_core::identity::KeyMatch::New(key)) => Some(key),
                        _ => None,
                    },
                };
                if let Some(key) = key {
                    ins.push(ProjectIn { key, name, root, worktrees, owner });
                }
            }
            let found = movein::look(&ins, &claude, &codex, &logins);
            // A study left paused, or questions left open, go on where they were.
            let progress = movein::Progress::load().ok().filter(movein::Progress::unfinished);
            (ins, found, progress)
        });
        cx.spawn(async move |this, cx| {
            let (ins, found, progress) = task.await;
            let _ = this.update(cx, |ws, cx| {
                if let Some(form) = ws.movein.as_mut() {
                    form.found = found;
                    form.projects = ins;
                    form.busy = false;
                    form.stage = Stage::Consent;
                    if let Some(progress) = progress {
                        form.run = progress.run.clone();
                        form.progress = Some(progress);
                        form.stage = Stage::Study;
                    }
                }
                if ws.movein.as_ref().is_some_and(|f| f.stage == Stage::Study) {
                    ws.move_in_answer_box(cx);
                    ws.move_in_study_step(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ----- part 2: the study and the questions (M54) ------------------------------------------

    /// Study the projects next: their levels asked first, then one study a project, within the
    /// budget (the manager's own token ledger).
    pub(crate) fn move_in_study_start(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.movein.as_mut() else { return };
        let run = form.run.clone().unwrap_or_else(|| movein::run_id(glasscode_core::clock::now_ms()));
        let mut progress = movein::Progress::start(&run, &form.projects).with_estimates(&form.estimates);
        let levels = movein::level_questions(&progress, 3);
        progress.ask(levels);
        form.run = Some(run);
        form.progress = Some(progress.clone());
        form.stage = Stage::Study;
        self.move_in_answer_box(cx);
        // Kept before the first step runs; the step's change lands on it.
        let task = cx.background_spawn(async move { movein::keep(&movein::Delta::Start(progress)) });
        cx.spawn(async move |this, cx| {
            if let Err(err) = task.await {
                log::warn!("movein: the study's progress could not be kept: {err}");
            }
            let _ = this.update(cx, |ws, cx| ws.move_in_study_step(cx));
        })
        .detach();
        cx.notify();
    }

    /// Apply a change to the dialog's progress (the store gets the same change in the background).
    fn move_in_apply(&mut self, delta: &movein::Delta) {
        if let Some(progress) = self.movein.as_mut().and_then(|f| f.progress.as_mut()) {
            progress.apply(delta);
        }
    }

    /// A paused study tries again by itself: when its budget opens again (checked at least every
    /// half hour, as a cap can be raised), until the dialog's state goes.
    fn move_in_wait(&mut self, until: u64, cx: &mut Context<Self>) {
        let wait = until.saturating_sub(glasscode_core::clock::now_ms()).clamp(60_000, 30 * 60_000);
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(wait)).await;
            let _ = this.update(cx, |ws, cx| ws.move_in_study_step(cx));
        });
        if let Some(form) = self.movein.as_mut() {
            form.resume = Some(task);
        }
    }

    /// The answer box for the question asked now (made once).
    fn move_in_answer_box(&mut self, cx: &mut Context<Self>) {
        if self.movein.as_ref().is_some_and(|f| f.answer.is_none()) {
            let input = cx.new(|cx| crate::text_input::TextInput::new("Your answer (or skip)", cx));
            if let Some(form) = self.movein.as_mut() {
                form.answer = Some(input);
            }
        }
    }

    /// One study step in the background: the next project, unless the budget says wait. Each step
    /// saves where it is, so a restart resumes; then the next step, until all are done or it pauses.
    pub(crate) fn move_in_study_step(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.movein.as_mut() else { return };
        let Some(mut progress) = form.progress.clone() else { return };
        if form.studying || progress.studied() {
            return;
        }
        form.studying = true;
        form.resume = None;
        let projects = form.projects.clone();
        let agent = self.state.manager.brain;
        let config_dir = self.state.default_account(agent).and_then(|a| a.config_dir());
        // D17: the run is on this account; a project bound to another is not given to it.
        let account = self.state.default_account(agent).map(|a| a.id.clone());
        let set = self.control_policy_set();
        movein::skip_unreadable(&mut progress, |key| set.account_may_read(key, account.as_deref()));
        let env = self.env.clone();
        cx.notify();
        let task = cx.background_spawn(async move {
            use glasscode_core::runtime::budget::Ledger;
            let memory = Memory::open();
            // The step's own copy of the ledger; what it records joins the stored one after, as
            // the manager's lanes do.
            let before = Ledger::load().unwrap_or_default();
            let mut ledger = before.clone();
            let mut run = |project: &ProjectIn, input: &str| -> Result<String, String> {
                let dir = glasscode_core::paths::cache_dir().join("movein").join(project.key.as_str());
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let (schema_file, answer_file) = (dir.join("schema.json"), dir.join("answer.json"));
                let _ = std::fs::remove_file(&answer_file);
                std::fs::write(&schema_file, glasscode_core::runtime::study::schema().to_string()).map_err(|e| e.to_string())?;
                let profile = movein::study_profile(agent, &schema_file, &answer_file);
                glasscode_core::agents::headless_run(agent, config_dir.as_deref(), &profile, input, Some(&answer_file), &env).map_err(|e| format!("{e:#}"))
            };
            let step = movein::study_next(&mut progress, &projects, &mut ledger, glasscode_core::clock::now_ms(), &memory, &mut run);
            if let Err(err) = glasscode_core::store::update::<Ledger, _>(|stored| stored.add_since(&before, &ledger)) {
                log::warn!("movein: the token ledger could not be kept: {err}");
            }
            post_questions(&mut progress);
            let delta = movein::Delta::Study(progress);
            if let Err(err) = movein::keep(&delta) {
                log::warn!("movein: the study's progress could not be kept: {err}");
            }
            let run = match &delta {
                movein::Delta::Study(p) => p.run.clone(),
                _ => None,
            };
            let waiting = run.as_deref().map(|run| movein::waiting(&memory, run).unwrap_or_default()).unwrap_or_default();
            (delta, run, step, waiting)
        });
        cx.spawn(async move |this, cx| {
            let (delta, run, step, waiting) = task.await;
            let _ = this.update(cx, |ws, cx| {
                let Some(form) = ws.movein.as_mut() else { return };
                form.studying = false;
                // The run was undone while it studied: what this step proposed goes too.
                if form.progress.as_ref().and_then(|p| p.run.clone()) != run {
                    if let Some(run) = run {
                        cx.background_spawn(async move {
                            if let Err(err) = movein::undo(&Memory::open(), &run) {
                                log::warn!("movein: a late step of an undone run could not be undone: {err}");
                            }
                        })
                        .detach();
                    }
                    cx.notify();
                    return;
                }
                form.waiting = waiting;
                form.error = match &step {
                    movein::Step::Failed { key, why } => Some(format!("The study of {key} did not work: {why}")),
                    _ => None,
                };
                ws.move_in_apply(&delta);
                match step {
                    movein::Step::Studied { .. } | movein::Step::Failed { .. } => ws.move_in_study_step(cx),
                    movein::Step::Paused { until, .. } => ws.move_in_wait(until, cx),
                    movein::Step::Done => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The person's answer to the question asked now: their words, typed here (tier one, D1).
    fn move_in_answer(&mut self, skip: bool, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let Some(form) = self.movein.as_mut() else { return };
        let Some(mut progress) = form.progress.clone() else { return };
        let Some(question) = progress.open().first().map(|q| (*q).clone()) else { return };
        let Some(input) = form.answer.clone() else { return };
        let _ = window;
        // "Skip" is a click, not words: nothing goes to the words ledger or the conversation.
        if skip {
            input.update(cx, |i, cx| i.set_text("", cx));
            let Ok(delta) = movein::skip(&mut progress, &question.id) else { return };
            self.move_in_apply(&delta);
            cx.background_spawn(async move {
                if let Err(err) = movein::keep(&delta) {
                    log::warn!("movein: a skipped question could not be kept: {err}");
                }
            })
            .detach();
            cx.notify();
            return;
        }
        let (text, untyped) = {
            let input = input.read(cx);
            let all = input.text();
            let (text, untyped) = glasscode_core::words::trimmed(&all, input.typing());
            (text.to_string(), untyped)
        };
        if text.is_empty() {
            return;
        }
        input.update(cx, |i, cx| i.set_text("", cx));
        // The answer box is the composer of the chat's topic "Moving in": the person's own words.
        let id = glasscode_core::words::record_with(glasscode_core::words::Origin::Composer(glasscode_core::words::ComposerKind::ManagerChat), &text, &untyped, None, question.project.as_ref());
        self.move_in_answer_with(progress, question, id, false, cx);
    }

    /// Whether the chat's message belongs to moving in: the Manager page shows the topic "Moving
    /// in" and a question there is open. Then it answers that question (`words`: the chat's own
    /// record of what the person typed) instead of going to the brain. True when it took it.
    pub(crate) fn move_in_chat_answer(&mut self, words: &glasscode_core::words::WordsId, cx: &mut Context<Self>) -> bool {
        let Some(progress) = self.movein.as_ref().and_then(|f| f.progress.clone()) else { return false };
        let shown = self.mchat.topic.as_ref().map(|t| t.as_str().to_string());
        if shown.is_none() || progress.topic != shown {
            return false;
        }
        let Some(question) = progress.open().first().map(|q| (*q).clone()) else { return false };
        self.move_in_answer_with(progress, question, words.clone(), true, cx);
        true
    }

    /// The answer of `words` to `question`, in the background: the fact or the level, then its
    /// change applied here and kept.
    fn move_in_answer_with(&mut self, mut progress: movein::Progress, question: movein::Question, id: glasscode_core::words::WordsId, from_chat: bool, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move {
            let memory = Memory::open();
            let record = glasscode_core::words::find(&id).ok_or_else(|| "the answer was not recorded".to_string())?;
            let done = match movein::answer(&mut progress, &question.id, &record, &memory) {
                Ok(done) => done,
                Err(why) => {
                    // In the chat, what was typed shows, and why it is no answer, beside the question.
                    if from_chat {
                        post_answer(&progress, &question, &record);
                        post_note(&progress, &why);
                    }
                    return Err(why);
                }
            };
            post_answer(&progress, &question, &record);
            let delta = done.delta(&question.id, &record.text);
            if let Err(err) = movein::keep(&delta) {
                log::warn!("movein: an answer could not be kept: {err}");
            }
            Ok::<_, String>((delta, done))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                match result {
                    Ok((delta, done)) => {
                        ws.move_in_apply(&delta);
                        if let Some(form) = ws.movein.as_mut() {
                            form.error = None;
                        }
                        // A level is a policy change: its preview, confirmed in Settings' own dialog.
                        if let movein::Answered::Level(project, level) = done {
                            ws.trust_change(glasscode_core::trust::Change::Level { project, level }, cx);
                        }
                    }
                    Err(why) => {
                        if let Some(form) = ws.movein.as_mut() {
                            form.error = Some(why);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Read what is ticked and propose it to memory (one commit per project).
    pub(crate) fn move_in_read(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.movein.as_mut() else { return };
        if form.busy {
            return;
        }
        form.busy = true;
        form.stage = Stage::Reading;
        form.error = None;
        let found = form.found.clone();
        let org_repos = form.org_repos.clone();
        let env = self.env.clone();
        let run = movein::run_id(glasscode_core::clock::now_ms());
        cx.notify();
        let task = cx.background_spawn(async move {
            let memory = Memory::open();
            // An organisation's GitHub teams, read only, as the account of its projects (the person
            // ticked it: it is the one online source).
            let teams = |org: &str| -> Result<Vec<(String, Vec<String>)>, String> {
                use glasscode_core::github_api::{auth_for, rest_get};
                let repo = org_repos.iter().find(|(o, _)| o.eq_ignore_ascii_case(org)).map(|(_, r)| r.clone()).unwrap_or_default();
                let auth = auth_for(&repo, &env).ok_or_else(|| "no GitHub account to ask with".to_string())?;
                let list = rest_get(&auth, &env, &format!("/orgs/{org}/teams?per_page=30")).map_err(|e| e.to_string())?;
                let mut out = Vec::new();
                for team in list.as_array().into_iter().flatten().take(15) {
                    let (Some(slug), Some(name)) = (team["slug"].as_str(), team["name"].as_str()) else { continue };
                    let members = rest_get(&auth, &env, &format!("/orgs/{org}/teams/{slug}/members?per_page=50")).map_err(|e| e.to_string())?;
                    let logins = members.as_array().into_iter().flatten().filter_map(|m| m["login"].as_str().map(str::to_string)).collect();
                    out.push((name.to_string(), logins));
                }
                Ok(out)
            };
            let proposals = movein::harvest_with(&found, &teams);
            let counts = movein::propose(&memory, &run, &proposals).map_err(|e| e.to_string())?;
            let waiting = movein::waiting(&memory, &run).map_err(|e| e.to_string())?;
            Ok::<_, String>((run, counts, waiting))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                if let Some(form) = ws.movein.as_mut() {
                    form.busy = false;
                    match result {
                        Ok((run, counts, waiting)) => {
                            form.run = Some(run);
                            form.counts = counts;
                            form.waiting = waiting;
                            form.stage = Stage::Review;
                            let projects = form.projects.clone();
                            ws.move_in_estimate(projects, cx);
                        }
                        Err(error) => {
                            form.error = Some(error);
                            form.stage = Stage::Consent;
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// What the study of each project would use, before the person starts it (no model): from what
    /// it would read, with today's budget beside it.
    fn move_in_estimate(&mut self, projects: Vec<ProjectIn>, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move {
            let estimates: Vec<(glasscode_core::paths::ProjectKey, movein::Estimate)> = projects.iter().map(|p| (p.key.clone(), movein::estimate(p))).collect();
            let left = glasscode_core::runtime::budget::Ledger::load().ok().map(|l| l.left_today(glasscode_core::clock::now_ms()));
            let line = movein::estimate_line(&estimates, left);
            (estimates, line)
        });
        cx.spawn(async move |this, cx| {
            let (estimates, line) = task.await;
            let _ = this.update(cx, |ws, cx| {
                if let Some(form) = ws.movein.as_mut() {
                    form.estimates = estimates;
                    form.estimate_line = Some(line);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// "From an export": a Glasscode export (M25) moves in whole, merged entry by entry into memory.
    fn move_in_from_export(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Move in".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |ws, cx| {
                with_form(ws, |form| {
                    form.busy = true;
                    form.note = Some("Reading the export…".into());
                });
                cx.notify();
                let task = cx.background_spawn(async move { glasscode_core::portability::import(&path) });
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |ws, cx| {
                        with_form(ws, |form| {
                            form.busy = false;
                            match result {
                                Ok(done) => {
                                    form.error = None;
                                    form.note = Some(format!(
                                        "Moved in from the export: {} memory files, {} project keys and {} words records. {} policy files wait in Settings for you to approve again.",
                                        done.memory_files, done.project_keys, done.words_records, done.set_aside
                                    ));
                                }
                                Err(err) => {
                                    form.note = None;
                                    form.error = Some(format!("Could not move in from the export: {err:#}"));
                                }
                            }
                        });
                        cx.notify();
                    });
                })
                .detach();
            });
        })
        .detach();
    }

    /// Accept proposals of the run: one, all from one source, or all (the person's click).
    pub(crate) fn move_in_accept(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        self.move_in_change(cx, move |memory, run| {
            memory.accept_proposals(&ids).map_err(|e| e.to_string())?;
            let _ = run;
            Ok(ids.len())
        });
    }

    /// Drop one proposal.
    pub(crate) fn move_in_drop(&mut self, id: String, cx: &mut Context<Self>) {
        self.move_in_change(cx, move |memory, _| memory.reject_proposal(&id).map(|_| 0).map_err(|e| e.to_string()));
    }

    /// Undo the whole run: what waits leaves, what was accepted is forgotten.
    pub(crate) fn move_in_undo(&mut self, cx: &mut Context<Self>) {
        self.move_in_change(cx, move |memory, run| {
            movein::undo(memory, run).map_err(|e| e.to_string())?;
            // Its study and questions go too, so opening again does not resume an undone run.
            movein::keep(&movein::Delta::Reset).map_err(|e| e.to_string())?;
            Ok(0)
        });
        if let Some(form) = self.movein.as_mut() {
            form.accepted = 0;
            form.progress = None;
            form.resume = None;
            if form.stage == Stage::Study {
                form.stage = Stage::Review;
            }
        }
    }

    /// The person confirmed a level in its preview: the question it answered is answered.
    pub fn move_in_level_confirmed(&mut self, project: glasscode_core::paths::ProjectKey, level: glasscode_core::gate::Level, cx: &mut Context<Self>) {
        let asked = self.movein.as_ref().and_then(|f| f.progress.as_ref()).is_some_and(|p| p.questions.iter().any(|q| q.ask == movein::Ask::Level && q.project.as_ref() == Some(&project) && q.answer.is_none()));
        if !asked {
            return;
        }
        let delta = movein::Delta::Confirmed { project, level };
        self.move_in_apply(&delta);
        cx.background_spawn(async move {
            if let Err(err) = movein::keep(&delta) {
                log::warn!("movein: a confirmed level could not be kept: {err}");
            }
        })
        .detach();
        // Back to the questions, when the dialog was where the level was said.
        if self.movein.as_ref().is_some_and(|f| f.stage == Stage::Study && !f.quiet) && self.modal.is_none() {
            self.modal = Some(Modal::MoveIn);
        }
        cx.notify();
    }

    /// A change of the run's proposals, in the background; then the list again.
    fn move_in_change(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&Memory, &str) -> Result<usize, String> + Send + 'static) {
        let Some(form) = self.movein.as_mut() else { return };
        let Some(run) = form.run.clone() else { return };
        if form.busy {
            return;
        }
        form.busy = true;
        cx.notify();
        let task = cx.background_spawn(async move {
            let memory = Memory::open();
            let accepted = change(&memory, &run)?;
            let waiting = movein::waiting(&memory, &run).map_err(|e| e.to_string())?;
            Ok::<_, String>((accepted, waiting))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                if let Some(form) = ws.movein.as_mut() {
                    form.busy = false;
                    match result {
                        Ok((accepted, waiting)) => {
                            form.accepted += accepted;
                            form.waiting = waiting;
                            form.error = None;
                        }
                        Err(error) => form.error = Some(error),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// The questions not in the conversation yet, posted to its topic "Moving in" (made with the
/// first): so the chat shows what moving in asked. A conversation that cannot be written loses
/// nothing here: the questions are in `manager/movein.json`.
fn post_questions(progress: &mut movein::Progress) {
    use glasscode_core::conversation::{Author, Conversation, Draft, Kind, TopicId};
    let mut chat = Conversation::load();
    for at in 0..progress.questions.len() {
        if progress.questions[at].item.is_some() {
            continue;
        }
        let q = progress.questions[at].clone();
        let topic = progress.topic.as_deref().and_then(TopicId::parse);
        let draft = Draft { author: Author::App, kind: Kind::Nudge { matter: format!("movein:{}", q.id) }, text: format!("{} ({}; if unanswered: {})", q.text, q.project_name, q.default), refs: Vec::new(), topic: topic.clone() };
        match chat.post(draft) {
            Ok(item) => {
                if topic.is_none() {
                    match chat.split(std::slice::from_ref(&item.id), "Moving in") {
                        Ok(made) => progress.topic = Some(made.as_str().to_string()),
                        Err(err) => log::warn!("movein: the topic Moving in could not be made: {err}"),
                    }
                }
                progress.questions[at].item = Some(item.id.as_str().to_string());
            }
            Err(err) => {
                log::warn!("movein: a question could not be posted: {err}");
                return;
            }
        }
    }
}

/// The person's answer, in the topic beside its question.
fn post_answer(progress: &movein::Progress, question: &movein::Question, record: &glasscode_core::words::WordsRecord) {
    use glasscode_core::conversation::{Author, Conversation, Draft, ItemId, Kind, TopicId};
    let topic = progress.topic.as_deref().and_then(TopicId::parse);
    let reply_to = question.item.as_deref().and_then(|id| serde_json::from_value::<ItemId>(serde_json::json!(id)).ok());
    let draft = Draft { author: Author::Person, kind: Kind::You { words: record.id.clone(), reply_to }, text: record.text.clone(), refs: Vec::new(), topic };
    if let Err(err) = Conversation::load().post(draft) {
        log::warn!("movein: an answer could not be posted: {err}");
    }
}

/// A line of Glasscode's in the topic "Moving in" (why an answer was not taken).
fn post_note(progress: &movein::Progress, text: &str) {
    use glasscode_core::conversation::{Author, Conversation, Draft, Kind, TopicId};
    let topic = progress.topic.as_deref().and_then(TopicId::parse);
    if let Err(err) = Conversation::load().post(Draft { author: Author::App, kind: Kind::System, text: text.to_string(), refs: Vec::new(), topic }) {
        log::warn!("movein: a note could not be posted: {err}");
    }
}

fn with_form(ws: &mut Workspace, f: impl FnOnce(&mut MoveIn)) {
    if let Some(form) = ws.movein.as_mut() {
        f(form);
    }
}

/// The dialog's heading for its stage.
pub fn heading(form: &MoveIn) -> (&'static str, &'static str) {
    match form.stage {
        Stage::Looking | Stage::Consent => ("Move in", "Glasscode can read what your agents and editors already know about your projects, and propose it for memory. Nothing is read until you tick it."),
        Stage::Reading => ("Move in", "Reading what you ticked. Nothing becomes memory until you accept it."),
        Stage::Review => ("What moving in found", "Each line is a proposal. Accept what is right; what you accept becomes memory, and the whole run can be undone."),
        Stage::Study => ("Getting to know your projects", "Glasscode studies each project, one at a time and within the manager's budget, and asks what only you know. Your answers become memory; a level waits for your confirmation."),
    }
}

/// The dialog's body.
pub fn render(form: &MoveIn, cx: &mut Context<Workspace>) -> AnyElement {
    let note = |text: String| div().text_size(theme::sz(12.5)).text_color(theme::text_soft()).child(text);
    let mut body = div().flex().flex_col().gap_3();
    if let Some(error) = &form.error {
        body = body.child(div().text_size(theme::sz(12.5)).text_color(theme::removed()).child(error.clone()));
    }
    if let Some(done) = &form.note {
        body = body.child(div().text_size(theme::sz(12.5)).text_color(theme::added()).child(done.clone()));
    }
    match form.stage {
        Stage::Looking => body.child(note("Looking for memories and rules files…".into())).into_any_element(),
        Stage::Reading => body.child(note("Reading and proposing…".into())).into_any_element(),
        Stage::Consent => {
            if form.found.is_empty() {
                return body.child(note("Nothing to move in was found: no agent memories, rules files or CODEOWNERS in your projects.".into())).into_any_element();
            }
            let mut rows: Vec<AnyElement> = Vec::new();
            let mut last_project = String::new();
            for (index, found) in form.found.iter().enumerate() {
                if found.project_name != last_project {
                    last_project = found.project_name.clone();
                    rows.push(div().pt_2().text_size(theme::sz(12.)).text_color(theme::muted()).child(last_project.clone()).into_any_element());
                }
                let count = match found.files {
                    0 => String::new(),
                    1 => "1 file".to_string(),
                    n => plural(n, "file", "files"),
                };
                rows.push(
                    div()
                        .id(SharedString::from(format!("movein-{index}")))
                        .flex()
                        .items_center()
                        .gap_2p5()
                        .h(theme::sz(30.))
                        .px_2()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            with_form(this, |form| {
                                if let Some(found) = form.found.get_mut(index) {
                                    found.ticked = !found.ticked;
                                }
                            });
                            cx.notify();
                        }))
                        .child(ui::checkbox(SharedString::from(format!("movein-check-{index}")), found.ticked))
                        .child(div().flex_none().w(theme::sz(150.)).text_size(theme::sz(12.5)).text_color(theme::text_strong()).child(found.kind.label()))
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().font_family(theme::MONO_FONT).text_size(theme::sz(11.5)).text_color(theme::faint()).child(found.path.display().to_string()))
                        .child(div().flex_none().text_size(theme::sz(11.5)).text_color(theme::muted()).child(count))
                        .into_any_element(),
                );
            }
            body.child(note("An organisation's projects start with their agents' memories unticked. Every project starts Careful.".into())).child(div().flex().flex_col().gap_0p5().children(rows)).into_any_element()
        }
        Stage::Study => render_study(form, cx),
        Stage::Review => {
            let (proposed, left) = form.counts;
            let summary = format!(
                "{} proposed, {} accepted, {} waiting{}.",
                proposed,
                form.accepted,
                form.waiting.len(),
                if left > 0 { format!(", {left} left out (already in memory, refused by the scan, or no room)") } else { String::new() }
            );
            let mut by_source: BTreeMap<String, Vec<(String, String, String)>> = BTreeMap::new();
            for (id, text, to, source) in &form.waiting {
                by_source.entry(source.clone()).or_default().push((id.clone(), text.clone(), to.clone()));
            }
            let mut groups: Vec<AnyElement> = Vec::new();
            for (n, (source, lines)) in by_source.into_iter().enumerate() {
                let ids: Vec<String> = lines.iter().map(|(id, ..)| id.clone()).collect();
                groups.push(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .pt_2()
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().font_family(theme::MONO_FONT).text_size(theme::sz(11.5)).text_color(theme::muted()).child(source))
                        .child(ui::chip(SharedString::from(format!("movein-all-{n}")), false, format!("Accept all ({})", ids.len())).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.run_op_named("movein.accept", serde_json::json!({ "ids": ids.join(" ") }), window, cx))))
                        .into_any_element(),
                );
                for (id, text, to) in lines {
                    let (accept, drop) = (id.clone(), id.clone());
                    groups.push(
                        div()
                            .flex()
                            .items_start()
                            .gap_2()
                            .px_2()
                            .child(div().flex_1().min_w_0().flex().flex_col().child(div().text_size(theme::sz(12.5)).text_color(theme::text_strong()).child(text)).child(div().text_size(theme::sz(11.)).text_color(theme::faint()).child(to)))
                            .child(ui::chip(SharedString::from(format!("movein-ok-{id}")), false, "Accept").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.run_op_named("movein.accept", serde_json::json!({ "ids": accept.clone() }), window, cx))))
                            .child(ui::chip(SharedString::from(format!("movein-drop-{id}")), false, "Drop").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.run_op_named("movein.drop", serde_json::json!({ "id": drop.clone() }), window, cx))))
                            .into_any_element(),
                    );
                }
            }
            // Next: studying the projects, with what it would use.
            let study = form.estimate_line.clone().map(|line| format!("Next, “Study my projects” gets to know each one with the manager's model. {line}"));
            body.child(note(summary)).child(div().flex().flex_col().gap_1().children(groups)).children(study.map(note)).into_any_element()
        }
    }
}

/// Part 2: the studies, the question asked now with its answer box, and what was answered.
fn render_study(form: &MoveIn, cx: &mut Context<Workspace>) -> AnyElement {
    let Some(progress) = &form.progress else { return div().into_any_element() };
    let line = |text: String, color| div().text_size(theme::sz(12.5)).text_color(color).child(text).into_any_element();
    let mut rows: Vec<AnyElement> = Vec::new();
    if let Some(error) = &form.error {
        rows.push(line(error.clone(), theme::removed()));
    }
    if let Some(why) = &progress.paused {
        rows.push(line(format!("Paused: {why} It goes on by itself."), theme::modified()));
    }
    for project in &progress.projects {
        let state = match &project.state {
            movein::StudyState::Waiting if form.studying => "studying or waiting".to_string(),
            movein::StudyState::Waiting if project.estimate > 0 => format!("waiting (about {} tokens)", project.estimate),
            movein::StudyState::Waiting => "waiting".to_string(),
            movein::StudyState::Done => format!("studied: {}", plural(project.proposed, "proposal", "proposals")),
            movein::StudyState::Failed(why) => format!("not studied: {why}"),
        };
        rows.push(line(format!("{}: {state}", project.name), theme::text_soft()));
    }
    rows.push(line(format!("{} used by the studies. {} wait in the review list.", plural(progress.tokens as usize, "token", "tokens"), plural(form.waiting.len(), "proposal", "proposals")), theme::muted()));
    let open = progress.open();
    match (open.first(), &form.answer) {
        (Some(question), Some(input)) => {
            rows.push(div().pt_2().text_size(theme::sz(13.)).text_color(theme::text_strong()).child(format!("{} ({})", question.text, question.project_name)).into_any_element());
            let hint = match (&question.said, &question.ask) {
                (Some(level), _) => format!("You said {level}: it changes once you confirm it in the preview. Answer again to choose another."),
                (None, movein::Ask::Other) => format!("Your answer is kept as you write it, so make it a whole sentence. If you skip it: {}.", question.default),
                _ => format!("If you skip it: {}.", question.default),
            };
            rows.push(div().text_size(theme::sz(11.5)).text_color(theme::faint()).child(format!("{hint} {} more after this one.", open.len() - 1)).into_any_element());
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(input.clone()))
                    .child(ui::button("movein-skip", "Skip").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.move_in_answer(true, window, cx))))
                    .child(ui::primary_button("movein-answer", "Answer").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.move_in_answer(false, window, cx))))
                    .into_any_element(),
            );
        }
        _ if !progress.questions.is_empty() => rows.push(line("Every question is answered.".into(), theme::added())),
        _ => {}
    }
    for q in progress.questions.iter().filter(|q| q.answer.is_some()) {
        rows.push(line(format!("{} — {}", q.text, q.answer.clone().unwrap_or_default()), theme::faint()));
    }
    div().flex().flex_col().gap_1().children(rows).into_any_element()
}

/// The dialog's buttons.
pub fn footer(form: &MoveIn, cx: &mut Context<Workspace>) -> AnyElement {
    let ticked = form.found.iter().filter(|f| f.ticked).count();
    div()
        .flex()
        .justify_end()
        .gap_2()
        .when(matches!(form.stage, Stage::Review | Stage::Study), |d| {
            d.child(ui::button("movein-undo", "Undo this move-in").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.run_op_named("movein.undo", serde_json::json!({}), window, cx))))
        })
        .child(ui::button("movein-close", if matches!(form.stage, Stage::Review | Stage::Study) { "Done" } else { "Not now" }).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_modal(window, cx))))
        .when(form.stage == Stage::Review && !form.projects.is_empty(), |d| {
            d.child(ui::primary_button("movein-study", "Study my projects").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.run_op_named("movein.study", serde_json::json!({}), window, cx))))
        })
        .when(form.stage == Stage::Study && form.progress.as_ref().is_some_and(|p| p.paused.is_some()), |d| {
            d.child(ui::button("movein-resume", "Try again now").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.run_op_named("movein.retry", serde_json::json!({}), window, cx))))
        })
        .when(form.stage == Stage::Consent && !form.busy, |d| {
            d.child(ui::button("movein-export", "From an export…").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.move_in_from_export(cx))))
        })
        .when(form.stage == Stage::Consent && ticked > 0, |d| {
            d.child(ui::primary_button("movein-read", format!("Read {}", plural(ticked, "source", "sources"))).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.run_op_named("movein.read", serde_json::json!({}), window, cx))))
        })
        .into_any_element()
}
