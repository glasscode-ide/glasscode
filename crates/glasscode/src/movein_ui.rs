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
#[derive(Clone, Debug, PartialEq, Eq)]
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
}

impl Workspace {
    /// Open moving in: list what there is to read (names and paths only), then ask (v2 only).
    pub fn open_move_in(&mut self, cx: &mut Context<Self>) {
        if !self.control.engine_v2 {
            self.show_toast("Moving in fills Glasscode's memory, which comes with the new manager.", cx);
            return;
        }
        self.movein = Some(MoveIn { stage: Stage::Looking, found: Vec::new(), run: None, waiting: Vec::new(), counts: (0, 0), accepted: 0, busy: true, error: None, projects: Vec::new(), progress: None, answer: None, studying: false });
        self.modal = Some(Modal::MoveIn);
        cx.notify();
        // What looking needs, from the window: no IO here.
        let projects: Vec<(String, String, std::path::PathBuf, Vec<std::path::PathBuf>, Option<String>, Option<glasscode_core::paths::ProjectKey>)> = self
            .state
            .projects
            .iter()
            .filter(|p| !self.state.is_hidden(&p.id))
            .map(|p| {
                let worktrees = self.worktrees.get(&p.id).map(|list| list.iter().map(|w| w.path.clone()).collect()).unwrap_or_default();
                let owner = self.tickets_repo_of(&p.root).and_then(|repo| repo.split('/').next().map(str::to_string));
                (p.id.clone(), p.name.clone(), p.root.clone(), worktrees, owner, self.control.project_keys.get(&p.id).cloned())
            })
            .collect();
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
            // A study left paused resumes where it was.
            let progress = movein::Progress::load().ok().filter(|p| p.run.is_some() && (!p.studied() || !p.open().is_empty()));
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
    fn move_in_study_start(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.movein.as_mut() else { return };
        let run = form.run.clone().unwrap_or_else(|| movein::run_id(glasscode_core::clock::now_ms()));
        let mut progress = movein::Progress::start(&run, &form.projects);
        let levels = movein::level_questions(&progress, 3);
        progress.ask(levels);
        form.run = Some(run);
        form.progress = Some(progress);
        form.stage = Stage::Study;
        self.move_in_answer_box(cx);
        self.move_in_study_step(cx);
        cx.notify();
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
    fn move_in_study_step(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.movein.as_mut() else { return };
        let Some(mut progress) = form.progress.clone() else { return };
        if form.studying || progress.studied() {
            return;
        }
        form.studying = true;
        let projects = form.projects.clone();
        let agent = self.state.manager.brain;
        let config_dir = self.state.default_account(agent).and_then(|a| a.config_dir());
        let env = self.env.clone();
        cx.notify();
        let task = cx.background_spawn(async move {
            let memory = Memory::open();
            let mut ledger = glasscode_core::runtime::budget::Ledger::load().unwrap_or_default();
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
            post_questions(&mut progress);
            let _ = ledger.save();
            let _ = progress.save();
            let waiting = progress.run.as_deref().map(|run| movein::waiting(&memory, run).unwrap_or_default()).unwrap_or_default();
            (progress, step, waiting)
        });
        cx.spawn(async move |this, cx| {
            let (progress, step, waiting) = task.await;
            let _ = this.update(cx, |ws, cx| {
                let go_on = matches!(step, movein::Step::Studied { .. } | movein::Step::Failed { .. });
                if let Some(form) = ws.movein.as_mut() {
                    form.studying = false;
                    form.progress = Some(progress);
                    form.waiting = waiting;
                    form.error = match &step {
                        movein::Step::Failed { key, why } => Some(format!("The study of {key} did not work: {why}")),
                        _ => None,
                    };
                }
                if go_on {
                    ws.move_in_study_step(cx);
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
        let (text, untyped) = {
            let input = input.read(cx);
            let all = if skip { "skip".to_string() } else { input.text() };
            let (text, untyped) = glasscode_core::words::trimmed(&all, input.typing());
            (text.to_string(), if skip { Vec::new() } else { untyped })
        };
        if text.is_empty() {
            return;
        }
        input.update(cx, |i, cx| i.set_text("", cx));
        // The answer box is the composer of the chat's topic "Moving in": the person's own words.
        let id = glasscode_core::words::record_with(glasscode_core::words::Origin::Composer(glasscode_core::words::ComposerKind::ManagerChat), &text, &untyped, None, question.project.as_ref());
        let task = cx.background_spawn(async move {
            let memory = Memory::open();
            let record = glasscode_core::words::find(&id).ok_or_else(|| "the answer was not recorded".to_string())?;
            let done = movein::answer(&mut progress, &question.id, &record, &memory)?;
            post_answer(&progress, &question, &record);
            let _ = progress.save();
            Ok::<_, String>((progress, done))
        });
        let _ = window;
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                match result {
                    Ok((progress, done)) => {
                        if let Some(form) = ws.movein.as_mut() {
                            form.progress = Some(progress);
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
    fn move_in_read(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.movein.as_mut() else { return };
        if form.busy {
            return;
        }
        form.busy = true;
        form.stage = Stage::Reading;
        form.error = None;
        let found = form.found.clone();
        let run = movein::run_id(glasscode_core::clock::now_ms());
        cx.notify();
        let task = cx.background_spawn(async move {
            let memory = Memory::open();
            let proposals = movein::harvest(&found);
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

    /// Accept proposals of the run: one, all from one source, or all (the person's click).
    fn move_in_accept(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        self.move_in_change(cx, move |memory, run| {
            memory.accept_proposals(&ids).map_err(|e| e.to_string())?;
            let _ = run;
            Ok(ids.len())
        });
    }

    /// Drop one proposal.
    fn move_in_drop(&mut self, id: String, cx: &mut Context<Self>) {
        self.move_in_change(cx, move |memory, _| memory.reject_proposal(&id).map(|_| 0).map_err(|e| e.to_string()));
    }

    /// Undo the whole run: what waits leaves, what was accepted is forgotten.
    fn move_in_undo(&mut self, cx: &mut Context<Self>) {
        self.move_in_change(cx, move |memory, run| {
            movein::undo(memory, run).map_err(|e| e.to_string())?;
            Ok(0)
        });
        if let Some(form) = self.movein.as_mut() {
            form.accepted = 0;
        }
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
                        .child(ui::chip(SharedString::from(format!("movein-all-{n}")), false, format!("Accept all ({})", ids.len())).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.move_in_accept(ids.clone(), cx))))
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
                            .child(ui::chip(SharedString::from(format!("movein-ok-{id}")), false, "Accept").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.move_in_accept(vec![accept.clone()], cx))))
                            .child(ui::chip(SharedString::from(format!("movein-drop-{id}")), false, "Drop").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.move_in_drop(drop.clone(), cx))))
                            .into_any_element(),
                    );
                }
            }
            body.child(note(summary)).child(div().flex().flex_col().gap_1().children(groups)).into_any_element()
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
        rows.push(line(format!("Paused: {why} It goes on by itself when the budget allows."), theme::modified()));
    }
    for project in &progress.projects {
        let state = match &project.state {
            movein::StudyState::Waiting if form.studying => "studying or waiting".to_string(),
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
            rows.push(div().text_size(theme::sz(11.5)).text_color(theme::faint()).child(format!("If you skip it: {}. {} more after this one.", question.default, open.len() - 1)).into_any_element());
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
            d.child(ui::button("movein-undo", "Undo this move-in").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.move_in_undo(cx))))
        })
        .child(ui::button("movein-close", if matches!(form.stage, Stage::Review | Stage::Study) { "Done" } else { "Not now" }).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_modal(window, cx))))
        .when(form.stage == Stage::Review && !form.projects.is_empty(), |d| {
            d.child(ui::primary_button("movein-study", "Study my projects").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.move_in_study_start(cx))))
        })
        .when(form.stage == Stage::Study && form.progress.as_ref().is_some_and(|p| p.paused.is_some()), |d| {
            d.child(ui::button("movein-resume", "Try again now").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.move_in_study_step(cx))))
        })
        .when(form.stage == Stage::Consent && ticked > 0, |d| {
            d.child(ui::primary_button("movein-read", format!("Read {}", plural(ticked, "source", "sources"))).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.move_in_read(cx))))
        })
        .into_any_element()
}
