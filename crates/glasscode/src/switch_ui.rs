//! "Server mode" / "Laptop mode": move every session (worktree, uncommitted work, conversation)
//! to the server or back, one at a time, each when its agent is idle.

use std::sync::Arc;
use std::time::Duration;

use gpui::{AnyElement, ClickEvent, Context, FontWeight, Hsla, SharedString, div, prelude::*, px};
use glasscode_core::agents::AgentActivity;
use glasscode_core::model::AgentKind;
use glasscode_core::sync;

use crate::dialogs::Modal;
use crate::center::Center;
use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Direction {
    ToServer,
    ToLaptop,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ItemState {
    Ready,
    /// The agent is busy; it moves when its turn ends.
    Waiting(String),
    /// "Move anyway" was pressed for a busy agent.
    Forced,
    Moving,
    Done,
    Failed(String),
}

pub struct SwitchItem {
    /// A local session id (to the server) or a server session id (to the laptop).
    pub id: String,
    pub title: String,
    pub agent: AgentKind,
    pub detail: String,
    pub selected: bool,
    pub state: ItemState,
}

pub struct SwitchForm {
    pub direction: Direction,
    pub items: Vec<SwitchItem>,
    pub running: bool,
    pub moving: bool,
}

impl SwitchForm {
    fn finished(&self) -> bool {
        self.items.iter().filter(|i| i.selected).all(|i| matches!(i.state, ItemState::Done | ItemState::Failed(_)))
    }
}

fn with_switch(ws: &mut Workspace, f: impl FnOnce(&mut SwitchForm)) {
    if let Some(form) = ws.switch.as_mut() {
        f(form);
    }
}

impl Workspace {
    pub fn open_switch(&mut self, direction: Direction, cx: &mut Context<Self>) {
        let server = self.server.name();
        let items: Vec<SwitchItem> = match direction {
            Direction::ToServer => self
                .state
                .sessions
                .iter()
                .map(|s| SwitchItem {
                    id: s.id.clone(),
                    title: s.title.clone(),
                    agent: s.agent,
                    detail: format!("⎇ {}", s.branch),
                    selected: true,
                    state: ItemState::Ready,
                })
                .collect(),
            Direction::ToLaptop => self
                .server
                .state
                .as_ref()
                .map(|state| {
                    state
                        .sessions
                        .iter()
                        .map(|r| SwitchItem {
                            id: r.session.id.clone(),
                            title: r.session.title.clone(),
                            agent: r.session.agent,
                            detail: format!("⎇ {} on {server}", r.session.branch),
                            selected: true,
                            state: ItemState::Ready,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        if self.switch.as_ref().is_some_and(|f| f.running) {
            // A switch is under way: show it instead of starting another.
            self.modal = Some(Modal::Switch);
            cx.notify();
            return;
        }
        self.switch = Some(SwitchForm { direction, items, running: false, moving: false });
        self.modal = Some(Modal::Switch);
        cx.notify();
    }

    /// Move one session (when its agent is idle), shown as progress in the title bar.
    pub fn move_one(&mut self, direction: Direction, id: String, cx: &mut Context<Self>) {
        let item = match direction {
            Direction::ToServer => self.state.session(&id).map(|s| SwitchItem {
                id: s.id.clone(),
                title: s.title.clone(),
                agent: s.agent,
                detail: format!("⎇ {}", s.branch),
                selected: true,
                state: ItemState::Ready,
            }),
            Direction::ToLaptop => self.server.session(&id).map(|r| SwitchItem {
                id: r.session.id.clone(),
                title: r.session.title.clone(),
                agent: r.session.agent,
                detail: format!("⎇ {}", r.session.branch),
                selected: true,
                state: ItemState::Ready,
            }),
        };
        let Some(item) = item else { return };
        match self.switch.as_mut() {
            // Another move under way in the same direction: this one joins it.
            Some(form) if form.running && form.direction == direction => {
                if !form.items.iter().any(|i| i.id == id) {
                    form.items.push(item);
                }
            }
            Some(form) if form.running => {
                self.show_toast("Another move is under way; try again when it is done", cx);
                return;
            }
            _ => self.switch = Some(SwitchForm { direction, items: vec![item], running: true, moving: false }),
        }
        cx.notify();
    }

    /// Why a session cannot move yet, if it cannot.
    fn switch_busy(&self, direction: Direction, id: &str) -> Option<String> {
        let activity = match direction {
            Direction::ToServer => {
                let session = self.state.session(id)?;
                if session.agent == AgentKind::Codex {
                    // Codex has no hooks: its conversation file still changing means it is working.
                    let writing = session
                        .transcript_path
                        .as_ref()
                        .and_then(|p| std::fs::metadata(p).ok())
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age < Duration::from_secs(6));
                    if writing && self.terminals.contains_key(id) {
                        return Some("finishing its reply".into());
                    }
                }
                if !self.terminals.contains_key(id) {
                    return None;
                }
                self.activity.get(id).copied()
            }
            Direction::ToLaptop => self.server.session(id).filter(|s| s.running).and_then(|s| s.activity),
        };
        match activity {
            Some(AgentActivity::Working) => Some("finishing its reply".into()),
            Some(AgentActivity::NeedsInput) => Some("waiting for your answer".into()),
            _ => None,
        }
    }

    /// Called from the workspace tick: start the next session that can move.
    pub fn switch_tick(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.switch.as_ref() else { return };
        if !form.running || form.moving {
            return;
        }
        let direction = form.direction;
        let candidates: Vec<(String, bool)> = form
            .items
            .iter()
            .filter(|i| i.selected && matches!(i.state, ItemState::Ready | ItemState::Waiting(_) | ItemState::Forced))
            .map(|i| (i.id.clone(), i.state == ItemState::Forced))
            .collect();
        let mut next = None;
        let mut waiting: Vec<(String, String)> = Vec::new();
        for (id, forced) in candidates {
            match self.switch_busy(direction, &id) {
                Some(reason) if !forced => waiting.push((id, reason)),
                _ => {
                    next = Some(id);
                    break;
                }
            }
        }
        with_switch(self, |form| {
            for (id, reason) in &waiting {
                if let Some(item) = form.items.iter_mut().find(|i| &i.id == id) {
                    item.state = ItemState::Waiting(reason.clone());
                }
            }
            if form.finished() {
                form.running = false;
            }
        });
        if let Some(id) = next {
            self.switch_move(direction, id, cx);
        }
        cx.notify();
    }

    fn switch_move(&mut self, direction: Direction, id: String, cx: &mut Context<Self>) {
        let (Some(client), Some(config)) = (self.server.client.clone(), self.server.config.clone()) else {
            with_switch(self, |form| {
                if let Some(item) = form.items.iter_mut().find(|i| i.id == id) {
                    item.state = ItemState::Failed("not connected to the server".into());
                }
            });
            return;
        };
        with_switch(self, |form| {
            form.moving = true;
            if let Some(item) = form.items.iter_mut().find(|i| i.id == id) {
                item.state = ItemState::Moving;
            }
        });
        let local = self.state.clone();
        match direction {
            Direction::ToServer => {
                let Some(session) = self.state.session(&id).cloned() else {
                    with_switch(self, |form| {
                        form.moving = false;
                        set_state(form, &id, ItemState::Failed("the session is gone".into()));
                    });
                    return;
                };
                let client = Arc::clone(&client);
                let task = cx.background_spawn(async move { sync::move_to_server(&client, &config, &local, &session) });
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |this, cx| {
                        match result {
                            Ok(remote) => {
                                // One machine runs a conversation: the local session ends.
                                let was_open = this.state.active_session.as_deref() == Some(id.as_str()) && this.center == Center::Session;
                                this.remove_session_quietly(&id);
                                let new_id = remote.session.id.clone();
                                if let Some(state) = this.server.state.as_mut() {
                                    state.sessions.push(remote);
                                }
                                if was_open {
                                    this.center = Center::Server(new_id);
                                }
                                with_switch(this, |form| set_state(form, &id, ItemState::Done));
                            }
                            Err(err) => {
                                // A one-session move has no dialog open: say why it did not move.
                                if !matches!(this.modal, Some(Modal::Switch)) {
                                    this.show_toast(format!("Could not move it: {err:#}"), cx);
                                }
                                with_switch(this, |form| set_state(form, &id, ItemState::Failed(format!("{err:#}"))))
                            }
                        }
                        with_switch(this, |form| form.moving = false);
                        this.server_refresh(cx);
                        cx.notify();
                    });
                })
                .detach();
            }
            Direction::ToLaptop => {
                let Some(remote) = self.server.session(&id).cloned() else {
                    with_switch(self, |form| {
                        form.moving = false;
                        set_state(form, &id, ItemState::Failed("the session is gone".into()));
                    });
                    return;
                };
                self.server.terminals.remove(&id);
                let task = cx.background_spawn(async move { sync::move_to_laptop(&client, &config, &local, &remote) });
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |this, cx| {
                        match result {
                            Ok(session) => {
                                let was_open = this.center == Center::Server(id.clone());
                                let new_id = session.id.clone();
                                this.state.sessions.push(session);
                                if was_open || this.state.active_session.is_none() {
                                    this.state.active_session = Some(new_id);
                                    this.center = Center::Session;
                                }
                                this.save();
                                this.refresh_worktrees(cx);
                                with_switch(this, |form| set_state(form, &id, ItemState::Done));
                            }
                            Err(err) => {
                                // A one-session move has no dialog open: say why it did not move.
                                if !matches!(this.modal, Some(Modal::Switch)) {
                                    this.show_toast(format!("Could not move it: {err:#}"), cx);
                                }
                                with_switch(this, |form| set_state(form, &id, ItemState::Failed(format!("{err:#}"))))
                            }
                        }
                        with_switch(this, |form| form.moving = false);
                        this.server_refresh(cx);
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }
}

fn set_state(form: &mut SwitchForm, id: &str, state: ItemState) {
    if let Some(item) = form.items.iter_mut().find(|i| i.id == id) {
        item.state = state;
    }
}

pub fn render_switch(ws: &Workspace, form: &SwitchForm, cx: &mut Context<Workspace>) -> AnyElement {
    let server = ws.server.name();
    let explain = match form.direction {
        Direction::ToServer => format!(
            "Each session's worktree (commits and uncommitted work), its .env files and its conversation go to {server}, and the session resumes there. Agents in the middle of a reply finish it first. The Mac copy of each session closes; its folder stays as it is."
        ),
        Direction::ToLaptop => format!(
            "Each session's worktree and conversation come back from {server} and the session resumes here (open it to continue). Edits made on this Mac meanwhile are kept in a git stash, never overwritten."
        ),
    };
    let mut rows: Vec<AnyElement> = Vec::new();
    if form.items.is_empty() {
        rows.push(div().text_color(theme::muted()).child("No sessions to move.").into_any_element());
    }
    for (index, item) in form.items.iter().enumerate() {
        let (label, color): (String, Hsla) = match &item.state {
            ItemState::Ready => (if form.running { "queued".into() } else { "ready".into() }, theme::muted()),
            ItemState::Waiting(reason) => (format!("waiting: {reason}"), theme::modified()),
            ItemState::Forced => ("moving next".into(), theme::modified()),
            ItemState::Moving => ("moving…".into(), theme::codex()),
            ItemState::Done => (
                match form.direction {
                    Direction::ToServer => format!("✓ on {server}"),
                    Direction::ToLaptop => "✓ on this Mac".into(),
                },
                theme::added(),
            ),
            ItemState::Failed(err) => (format!("✕ {err}"), theme::removed()),
        };
        let waiting = matches!(item.state, ItemState::Waiting(_));
        let id = item.id.clone();
        let force_id = item.id.clone();
        let locked = form.running || matches!(item.state, ItemState::Done);
        rows.push(
            div()
                .id(SharedString::from(format!("switch-{index}")))
                .flex()
                .items_center()
                .gap_2p5()
                .min_h(px(34.))
                .px_2()
                .py_1()
                .rounded_md()
                .when(!locked, |d| d.cursor_pointer().hover(|s| s.bg(theme::hover())))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    with_switch(this, |form| {
                        if !form.running {
                            if let Some(item) = form.items.iter_mut().find(|i| i.id == id) {
                                if item.state != ItemState::Done {
                                    item.selected = !item.selected;
                                }
                            }
                        }
                    });
                    cx.notify();
                }))
                .child(ui::checkbox(SharedString::from(format!("switch-check-{index}")), item.selected))
                .child(ui::dot(theme::agent_color(item.agent)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme::text_strong()).child(item.title.clone()))
                        .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().font_family(theme::MONO_FONT).text_size(px(11.5)).text_color(theme::muted()).child(item.detail.clone())),
                )
                .child(div().max_w(px(300.)).text_size(px(12.)).text_color(color).child(label))
                .when(waiting, |d| {
                    d.child(ui::button(SharedString::from(format!("switch-force-{index}")), "Move anyway").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        with_switch(this, |form| set_state(form, &force_id, ItemState::Forced));
                        cx.notify();
                    })))
                })
                .into_any_element(),
        );
    }
    let done = form.items.iter().any(|i| i.selected) && form.finished();
    let failed = form.items.iter().filter(|i| matches!(i.state, ItemState::Failed(_))).count();
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(div().text_size(px(12.5)).text_color(theme::text_soft()).child(explain))
        .child(div().flex().flex_col().gap_0p5().children(rows))
        .when(done, |d| {
            let (text, color) = match (form.direction, failed) {
                (Direction::ToServer, 0) => (format!("Everything runs on {server} now. You can close the lid."), theme::added()),
                (Direction::ToLaptop, 0) => ("Everything is back on this Mac. Open a session to continue it.".to_string(), theme::added()),
                (_, n) => (format!("{n} could not move; see above. The others moved."), theme::removed()),
            };
            d.child(div().mt_1().p_3().rounded_md().bg(theme::raised()).font_weight(FontWeight::SEMIBOLD).text_color(color).child(text))
        })
        .into_any_element()
}

pub fn switch_footer(ws: &Workspace, form: &SwitchForm, cx: &mut Context<Workspace>) -> AnyElement {
    let selected = form.items.iter().filter(|i| i.selected && i.state != ItemState::Done).count();
    let finished = form.items.iter().any(|i| i.selected) && form.finished();
    let target = match form.direction {
        Direction::ToServer => ws.server.name(),
        Direction::ToLaptop => "this Mac".to_string(),
    };
    let button = if finished {
        ui::primary_button("switch-done", "Done").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_modal(window, cx)))
    } else if form.running {
        let total = form.items.iter().filter(|i| i.selected).count();
        let moved = form.items.iter().filter(|i| i.selected && i.state == ItemState::Done).count();
        ui::disabled_button("switch-busy", format!("Moving… {moved} of {total}"))
    } else if selected == 0 || !ws.server.connected() {
        ui::disabled_button("switch-go", format!("Move to {target}"))
    } else {
        ui::primary_button("switch-go", format!("Move {selected} to {target}")).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            with_switch(this, |form| form.running = true);
            this.switch_tick(cx);
        }))
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().flex_1().text_size(px(12.)).text_color(theme::muted()).child(if form.running {
            "You can close this; moving continues in the background."
        } else {
            ""
        }))
        .when(!finished, |d| d.child(ui::button("switch-cancel", if form.running { "Hide" } else { "Cancel" }).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_modal(window, cx)))))
        .child(button)
        .into_any_element()
}

impl Workspace {
    /// Title bar: progress while sessions move.
    pub fn render_switch_buttons(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.server.connected() {
            return None;
        }
        if let Some(form) = self.switch.as_ref().filter(|f| f.running) {
            let total = form.items.iter().filter(|i| i.selected).count();
            let moved = form.items.iter().filter(|i| i.selected && i.state == ItemState::Done).count();
            return Some(
                ui::button("switch-progress", format!("Moving {moved} of {total}…"))
                    .text_color(theme::muted())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.modal = Some(Modal::Switch);
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        // No modes any more: this Mac's sessions sync to the server by themselves, and each
        // session moves on its own (its header's Move button).
        None
    }
}
