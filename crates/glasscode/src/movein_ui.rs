//! Moving in (M35): the dialog that asks first, reads what the person ticked, and shows what it
//! proposes for memory, grouped by project and source, to accept in bulk or line by line, or to
//! undo as a whole. The work is `glasscode_core::movein`'s, off the main thread; this is the
//! dialog. It exists only with the v2 engine (memory is v2's): with v1 nothing of it shows.

use std::collections::BTreeMap;

use gpui::{AnyElement, AppContext as _, ClickEvent, Context, IntoElement, InteractiveElement as _, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _};
use glasscode_core::memstore::Memory;
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
}

impl Workspace {
    /// Open moving in: list what there is to read (names and paths only), then ask (v2 only).
    pub fn open_move_in(&mut self, cx: &mut Context<Self>) {
        if !self.control.engine_v2 {
            self.show_toast("Moving in fills Glasscode's memory, which comes with the new manager.", cx);
            return;
        }
        self.movein = Some(MoveIn { stage: Stage::Looking, found: Vec::new(), run: None, waiting: Vec::new(), counts: (0, 0), accepted: 0, busy: true, error: None });
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
            movein::look(&ins, &claude, &codex, &logins)
        });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |ws, cx| {
                if let Some(form) = ws.movein.as_mut() {
                    form.found = found;
                    form.stage = Stage::Consent;
                    form.busy = false;
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

/// The dialog's buttons.
pub fn footer(form: &MoveIn, cx: &mut Context<Workspace>) -> AnyElement {
    let ticked = form.found.iter().filter(|f| f.ticked).count();
    div()
        .flex()
        .justify_end()
        .gap_2()
        .when(form.stage == Stage::Review, |d| {
            d.child(ui::button("movein-undo", "Undo this move-in").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.move_in_undo(cx))))
        })
        .child(ui::button("movein-close", if form.stage == Stage::Review { "Done" } else { "Not now" }).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_modal(window, cx))))
        .when(form.stage == Stage::Consent && ticked > 0, |d| {
            d.child(ui::primary_button("movein-read", format!("Read {}", plural(ticked, "source", "sources"))).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.move_in_read(cx))))
        })
        .into_any_element()
}
