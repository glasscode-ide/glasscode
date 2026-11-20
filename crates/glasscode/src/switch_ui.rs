//! "Server mode" / "Laptop mode": move every open session to the server or back. A worktree
//! moves as one unit with all its sessions (`glasscode_core::switch`), several at a time, each
//! when its agents are idle ("Move now" stops them first). The server keeps an owner record per
//! worktree, so a copy left behind never runs again.

use std::sync::Arc;
use std::time::Duration;

use glasscode_core::agents::AgentActivity;
use glasscode_core::model::AgentKind;
use glasscode_core::remote::RemoteClient;
use glasscode_core::switch::{self, Problem, Unit};
use glasscode_core::sync;
use gpui::{AnyElement, ClickEvent, Context, FontWeight, Hsla, SharedString, div, prelude::*};

use crate::center::Center;
use crate::dialogs::Modal;
use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

/// How many worktrees move at the same time.
const PARALLEL: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Direction {
    ToServer,
    ToLaptop,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ItemState {
    Ready,
    /// An agent in it is busy; it moves when the turn ends.
    Waiting(String),
    /// "Move now" was pressed for a busy one: its agents stop, then it moves.
    Forced,
    Moving,
    Done,
    Failed(String),
}

/// One worktree with its open sessions.
pub struct SwitchItem {
    pub unit: Unit,
    pub title: String,
    pub agent: AgentKind,
    pub detail: String,
    pub selected: bool,
    pub state: ItemState,
    /// What the check found (shown under it; the blocking ones keep it from moving).
    pub problems: Vec<String>,
    pub blocked: bool,
    /// A worktree with uncommitted work and no open session: unticked, its copy is parked on
    /// the server instead of moving it.
    pub idle: bool,
}

pub struct SwitchForm {
    pub direction: Direction,
    pub items: Vec<SwitchItem>,
    pub running: bool,
    /// Moves under way now.
    pub moving: usize,
    /// The copies of unticked worktrees left on the server: how many, or why not.
    pub parked: Option<Result<usize, String>>,
}

impl SwitchForm {
    fn finished(&self) -> bool {
        self.items
            .iter()
            .filter(|i| i.selected)
            .all(|i| matches!(i.state, ItemState::Done | ItemState::Failed(_)))
    }
}

fn with_switch(ws: &mut Workspace, f: impl FnOnce(&mut SwitchForm)) {
    if let Some(form) = ws.switch.as_mut() {
        f(form);
    }
}

impl Workspace {
    /// The units that move in `direction`, each with what the check found.
    fn switch_items(&self, direction: Direction) -> Vec<SwitchItem> {
        let server = self.server.state.as_ref();
        let features = self
            .server
            .client
            .as_ref()
            .map(|c| c.hello.features.clone())
            .unwrap_or_default();
        match direction {
            Direction::ToServer => switch::units_to_server(&self.state)
                .into_iter()
                .map(|unit| {
                    let problems = match server {
                        Some(server) => switch::check_to_server(
                            &self.state,
                            server,
                            &features,
                            &unit,
                            &|id: &str| {
                                self.terminals
                                    .contains_key(id)
                                    .then(|| self.activity.get(id).copied())
                                    .flatten()
                            },
                        ),
                        None => Vec::new(),
                    };
                    let first = unit.sessions.first().and_then(|id| self.state.session(id));
                    let agent = first.map_or(AgentKind::Shell, |s| s.agent);
                    let branch = first.map(|s| s.branch.clone()).unwrap_or_default();
                    self.switch_item(unit, agent, branch, problems)
                })
                .collect(),
            Direction::ToLaptop => {
                let Some(server) = server else {
                    return Vec::new();
                };
                let home = self
                    .server
                    .client
                    .as_ref()
                    .map(|c| c.hello.home.clone())
                    .unwrap_or_default();
                switch::units_to_mac(server, &home)
                    .into_iter()
                    .map(|unit| {
                        let problems = switch::check_to_mac(&self.state, server, &unit);
                        let first = unit.sessions.first().and_then(|id| self.server.session(id));
                        let agent = first.map_or(AgentKind::Shell, |r| r.session.agent);
                        let branch = first.map(|r| r.session.branch.clone()).unwrap_or_default();
                        self.switch_item(unit, agent, branch, problems)
                    })
                    .collect()
            }
        }
    }

    fn switch_item(
        &self,
        unit: Unit,
        agent: AgentKind,
        branch: String,
        problems: Vec<Problem>,
    ) -> SwitchItem {
        let title = match unit.titles.len() {
            0 => unit
                .worktree
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            1 => unit.titles[0].clone(),
            n => format!("{} and {} more", unit.titles[0], n - 1),
        };
        let folder = unit
            .worktree
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let sessions = match unit.sessions.len() {
            0 => "no session".to_string(),
            1 => "1 session".to_string(),
            n => format!("{n} sessions, together"),
        };
        let titles: std::collections::HashMap<String, String> = unit
            .sessions
            .iter()
            .cloned()
            .zip(unit.titles.iter().cloned())
            .collect();
        let blocked = problems.iter().any(Problem::blocks);
        let problems = problems
            .iter()
            .filter(|p| !matches!(p, Problem::Working { .. }))
            .map(|p| p.describe(|id| titles.get(id).cloned().unwrap_or_default()))
            .collect();
        SwitchItem {
            unit,
            title,
            agent,
            detail: format!("⎇ {branch} · {folder} · {sessions}"),
            selected: !blocked,
            state: ItemState::Ready,
            problems,
            blocked,
            idle: false,
        }
    }

    pub fn open_switch(&mut self, direction: Direction, cx: &mut Context<Self>) {
        if self.switch.as_ref().is_some_and(|f| f.running) {
            // A switch is under way: show it instead of starting another.
            self.modal = Some(Modal::Switch);
            cx.notify();
            return;
        }
        let items = self.switch_items(direction);
        self.switch = Some(SwitchForm {
            direction,
            items,
            running: false,
            moving: 0,
            parked: None,
        });
        self.modal = Some(Modal::Switch);
        if direction == Direction::ToServer {
            self.switch_find_idle(cx);
        }
        cx.notify();
    }

    /// Worktrees with uncommitted work and no open session join the list, unticked (git status
    /// runs in the background).
    fn switch_find_idle(&mut self, cx: &mut Context<Self>) {
        let idle = switch::idle_worktrees(&self.state, &self.worktrees);
        if idle.is_empty() {
            return;
        }
        let task = cx.background_spawn(async move { idle.into_iter().filter(|u| glasscode_core::git::is_dirty(&u.worktree)).collect::<Vec<_>>() });
        cx.spawn(async move |this, cx| {
            let dirty = task.await;
            let _ = this.update(cx, |this, cx| {
                let items: Vec<SwitchItem> = dirty
                    .into_iter()
                    .map(|unit| {
                        let branch = this.worktrees.get(&unit.project_id).and_then(|list| list.iter().find(|w| w.path == unit.worktree)).map(|w| w.branch.clone()).unwrap_or_default();
                        let mut item = this.switch_item(unit, AgentKind::Shell, branch, Vec::new());
                        item.detail = item.detail.replace("no session", "uncommitted work, no session: tick to move it, else a copy goes along");
                        item.selected = false;
                        item.idle = true;
                        item
                    })
                    .collect();
                with_switch(this, |form| {
                    if !form.running && form.direction == Direction::ToServer {
                        form.items.extend(items);
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// At "Move": the unticked worktrees with uncommitted work leave a copy on the server, so
    /// they can be opened there while this computer sleeps. Their owner does not change.
    fn switch_park_unticked(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.switch.as_ref().filter(|f| f.direction == Direction::ToServer) else { return };
        let units: Vec<Unit> = form.items.iter().filter(|i| i.idle && !i.selected).map(|i| i.unit.clone()).collect();
        let Some(config) = self.server.config.clone() else { return };
        if units.is_empty() {
            return;
        }
        let local = self.state.clone();
        let task = cx.background_spawn(async move {
            let client = RemoteClient::connect(&config)?;
            if !client.hello.features.iter().any(|f| f == "owners") {
                anyhow::bail!("the server's glasscoded is too old to keep copies");
            }
            let mac = glasscode_core::paths::MachineId::this().map_err(|e| anyhow::anyhow!("{e}"))?;
            for unit in &units {
                switch::park_on_server(&client, &config, &local, unit, &mac)?;
            }
            Ok(units.len())
        });
        cx.spawn(async move |this, cx| {
            let result: anyhow::Result<usize> = task.await;
            let _ = this.update(cx, |this, cx| {
                with_switch(this, |form| form.parked = Some(result.map_err(|e| format!("{e:#}"))));
                cx.notify();
            });
        })
        .detach();
    }

    /// Move one session's worktree, with every session in it (when its agents are idle), shown
    /// as progress in the title bar.
    pub fn move_one(&mut self, direction: Direction, id: String, cx: &mut Context<Self>) {
        let Some(mut item) = self
            .switch_items(direction)
            .into_iter()
            .find(|i| i.unit.sessions.contains(&id))
        else {
            return;
        };
        item.selected = !item.blocked;
        if item.blocked {
            self.show_toast(
                format!("It cannot move yet: {}", item.problems.join("; ")),
                cx,
            );
            return;
        }
        match self.switch.as_mut() {
            // Another move under way in the same direction: this one joins it.
            Some(form) if form.running && form.direction == direction => {
                if !form.items.iter().any(|i| i.unit.key == item.unit.key) {
                    form.items.push(item);
                }
            }
            Some(form) if form.running => {
                self.show_toast("Another move is under way; try again when it is done", cx);
                return;
            }
            _ => {
                self.switch = Some(SwitchForm {
                    direction,
                    items: vec![item],
                    running: true,
                    moving: 0,
                    parked: None,
                })
            }
        }
        cx.notify();
    }

    /// The other sessions that share `id`'s worktree (they move with it), by title.
    pub fn moves_with(&self, direction: Direction, id: &str) -> Vec<String> {
        let units = match direction {
            Direction::ToServer => switch::units_to_server(&self.state),
            Direction::ToLaptop => match (&self.server.state, &self.server.client) {
                (Some(server), Some(client)) => switch::units_to_mac(server, &client.hello.home),
                _ => Vec::new(),
            },
        };
        switch::unit_of(&units, id)
            .map(|u| {
                u.sessions
                    .iter()
                    .zip(&u.titles)
                    .filter(|(s, _)| *s != id)
                    .map(|(_, t)| t.clone())
                    .collect()
            })
            .unwrap_or_default()
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
            Direction::ToLaptop => self
                .server
                .session(id)
                .filter(|s| s.running)
                .and_then(|s| s.activity),
        };
        match activity {
            Some(AgentActivity::Working) => Some("finishing its reply".into()),
            Some(AgentActivity::NeedsInput) => Some("waiting for your answer".into()),
            _ => None,
        }
    }

    /// Why a unit cannot move yet: the first of its sessions that is busy.
    fn unit_busy(&self, direction: Direction, unit: &Unit) -> Option<String> {
        unit.sessions.iter().find_map(|id| {
            let reason = self.switch_busy(direction, id)?;
            let title = unit
                .sessions
                .iter()
                .position(|s| s == id)
                .and_then(|i| unit.titles.get(i))
                .cloned()
                .unwrap_or_default();
            Some(if unit.sessions.len() > 1 {
                format!("{title} is {reason}")
            } else {
                reason
            })
        })
    }

    /// Called from the workspace tick: start the next units that can move, up to [`PARALLEL`].
    pub fn switch_tick(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.switch.as_ref() else {
            return;
        };
        if !form.running {
            return;
        }
        let direction = form.direction;
        let mut free = PARALLEL.saturating_sub(form.moving);
        let candidates: Vec<(Unit, bool)> = form
            .items
            .iter()
            .filter(|i| {
                i.selected
                    && matches!(
                        i.state,
                        ItemState::Ready | ItemState::Waiting(_) | ItemState::Forced
                    )
            })
            .map(|i| (i.unit.clone(), i.state == ItemState::Forced))
            .collect();
        let mut start = Vec::new();
        let mut waiting: Vec<(Unit, String)> = Vec::new();
        for (unit, forced) in candidates {
            if free == 0 {
                break;
            }
            match self.unit_busy(direction, &unit) {
                Some(reason) if !forced => waiting.push((unit, reason)),
                _ => {
                    start.push((unit, forced));
                    free -= 1;
                }
            }
        }
        with_switch(self, |form| {
            for (unit, reason) in &waiting {
                if let Some(item) = form.items.iter_mut().find(|i| i.unit.key == unit.key) {
                    item.state = ItemState::Waiting(reason.clone());
                }
            }
            if form.finished() && form.moving == 0 {
                form.running = false;
            }
        });
        for (unit, forced) in start {
            self.switch_move(direction, unit, forced, cx);
        }
        cx.notify();
    }

    fn switch_move(
        &mut self,
        direction: Direction,
        unit: Unit,
        forced: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(config) = self
            .server
            .config
            .clone()
            .filter(|_| self.server.client.is_some())
        else {
            with_switch(self, |form| {
                set_state(
                    form,
                    &unit,
                    ItemState::Failed("not connected to the server".into()),
                )
            });
            return;
        };
        with_switch(self, |form| {
            form.moving += 1;
            set_state(form, &unit, ItemState::Moving);
        });
        if forced && direction == Direction::ToServer {
            // "Move now": the agents here stop first, so nothing changes after the snapshot.
            for id in &unit.sessions {
                self.stop_terminal(id);
            }
        }
        for id in &unit.sessions {
            if direction == Direction::ToLaptop {
                self.server.terminals.remove(id);
            }
        }
        let local = self.state.clone();
        let task_unit = unit.clone();
        // Its own connection, so several worktrees move at once (one connection carries one
        // call at a time).
        let task = cx.background_spawn(async move {
            let client = RemoteClient::connect(&config)?;
            let mac =
                glasscode_core::paths::MachineId::this().map_err(|e| anyhow::anyhow!("{e}"))?;
            let owners = client.hello.features.iter().any(|f| f == "owners");
            match direction {
                Direction::ToServer if owners => {
                    let moved =
                        switch::move_unit_to_server(&client, &config, &local, &task_unit, &mac)?;
                    Ok(Moved::ToServer(moved.sessions))
                }
                Direction::ToLaptop if owners => {
                    let moved =
                        switch::move_unit_to_mac(&client, &config, &local, &task_unit, &mac)?;
                    Ok(Moved::ToMac(moved, Arc::new(client)))
                }
                // An older glasscoded: one session at a time, as before.
                Direction::ToServer => {
                    let mut sessions = Vec::new();
                    for id in &task_unit.sessions {
                        let session = local
                            .session(id)
                            .ok_or_else(|| anyhow::anyhow!("the session is gone"))?;
                        sessions.push((
                            id.clone(),
                            sync::move_to_server(&client, &config, &local, session)?,
                        ));
                    }
                    Ok(Moved::ToServer(sessions))
                }
                Direction::ToLaptop => {
                    let state: glasscode_core::remote::ServerState =
                        client.call("state", serde_json::json!({}))?;
                    let mut sessions = Vec::new();
                    for id in &task_unit.sessions {
                        let remote = state
                            .sessions
                            .iter()
                            .find(|r| &r.session.id == id)
                            .ok_or_else(|| anyhow::anyhow!("the session is gone"))?;
                        sessions.push((
                            id.clone(),
                            sync::move_to_laptop(&client, &config, &local, remote)?,
                        ));
                    }
                    Ok(Moved::Legacy(sessions))
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let result: anyhow::Result<Moved> = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Moved::ToServer(sessions)) => {
                        for (local_id, remote) in sessions {
                            // One machine runs a conversation: the local session ends.
                            let was_open = this.state.active_session.as_deref()
                                == Some(local_id.as_str())
                                && this.center == Center::Session;
                            this.remove_session_quietly(&local_id);
                            let new_id = remote.session.id.clone();
                            if let Some(state) = this.server.state.as_mut() {
                                state.sessions.push(remote);
                            }
                            if was_open {
                                this.center = Center::Server(new_id);
                            }
                        }
                        with_switch(this, |form| set_state(form, &unit, ItemState::Done));
                    }
                    Ok(Moved::ToMac(moved, client)) => {
                        let ids: Vec<String> = moved
                            .sessions
                            .iter()
                            .map(|(server_id, _)| server_id.clone())
                            .collect();
                        this.bring_home(
                            ids,
                            moved.sessions.iter().map(|(_, s)| s.clone()).collect(),
                        );
                        // The sessions run here now: the server's copies end.
                        cx.background_spawn(async move {
                            if let Err(err) = switch::close_on_server(&client, &moved) {
                                log::warn!(
                                    "could not close the server's copies after a move: {err:#}"
                                );
                            }
                        })
                        .detach();
                        with_switch(this, |form| set_state(form, &unit, ItemState::Done));
                    }
                    Ok(Moved::Legacy(sessions)) => {
                        let (ids, sessions): (Vec<String>, Vec<_>) = sessions.into_iter().unzip();
                        this.bring_home(ids, sessions);
                        with_switch(this, |form| set_state(form, &unit, ItemState::Done));
                    }
                    Err(err) => {
                        // A one-session move has no dialog open: say why it did not move.
                        if !matches!(this.modal, Some(Modal::Switch)) {
                            this.show_toast(format!("Could not move it: {err:#}"), cx);
                        }
                        with_switch(this, |form| {
                            set_state(form, &unit, ItemState::Failed(format!("{err:#}")))
                        });
                    }
                }
                with_switch(this, |form| form.moving = form.moving.saturating_sub(1));
                this.server_refresh(cx);
                this.switch_tick(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Sessions that came back from the server: added here (they resume when opened).
    fn bring_home(
        &mut self,
        server_ids: Vec<String>,
        sessions: Vec<glasscode_core::model::Session>,
    ) {
        for (server_id, session) in server_ids.into_iter().zip(sessions) {
            let was_open = self.center == Center::Server(server_id.clone());
            let new_id = session.id.clone();
            if let Some(state) = self.server.state.as_mut() {
                state.sessions.retain(|r| r.session.id != server_id);
            }
            self.state.sessions.push(session);
            if was_open || self.state.active_session.is_none() {
                self.state.active_session = Some(new_id);
                self.center = Center::Session;
            }
        }
        self.save();
    }
}

/// What a finished move brings back to the window.
enum Moved {
    ToServer(Vec<(String, glasscode_core::remote::RemoteSession)>),
    ToMac(switch::MovedToMac, Arc<RemoteClient>),
    /// Moved back by an older glasscoded (its sessions are closed there already).
    Legacy(Vec<(String, glasscode_core::model::Session)>),
}

fn set_state(form: &mut SwitchForm, unit: &Unit, state: ItemState) {
    if let Some(item) = form.items.iter_mut().find(|i| i.unit.key == unit.key) {
        item.state = state;
    }
}

pub fn render_switch(ws: &Workspace, form: &SwitchForm, cx: &mut Context<Workspace>) -> AnyElement {
    let server = ws.server.name();
    let explain = match form.direction {
        Direction::ToServer => format!(
            "Each worktree (commits and uncommitted work, its .env files) goes to {server} with every session open in it, and the sessions resume there. Agents in the middle of a reply finish it first; \"Move now\" stops them. The {} copy of each session closes; its folder stays as it is.",
            crate::platform::computer()
        ),
        Direction::ToLaptop => format!(
            "Each worktree comes back from {server} with every session in it, and the sessions resume here (open one to continue). Edits made on {} meanwhile are kept in a git stash, never overwritten.",
            crate::platform::this_computer()
        ),
    };
    let mut rows: Vec<AnyElement> = Vec::new();
    if form.items.is_empty() {
        rows.push(div().text_color(theme::muted()).child("No open sessions to move.").into_any_element());
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
                    Direction::ToLaptop => format!("✓ on {}", crate::platform::this_computer()),
                },
                theme::added(),
            ),
            ItemState::Failed(err) => (format!("✕ {err}"), theme::removed()),
        };
        let waiting = matches!(item.state, ItemState::Waiting(_));
        let key = item.unit.key.clone();
        let force_unit = item.unit.clone();
        let locked = form.running || item.blocked || matches!(item.state, ItemState::Done);
        let problems = item.problems.join(" · ");
        rows.push(
            div()
                .id(SharedString::from(format!("switch-{index}")))
                .flex()
                .items_center()
                .gap_2p5()
                .min_h(theme::sz(34.))
                .px_2()
                .py_1()
                .rounded_md()
                .when(!locked, |d| d.cursor_pointer().hover(|s| s.bg(theme::hover())))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    with_switch(this, |form| {
                        if !form.running {
                            if let Some(item) = form.items.iter_mut().find(|i| i.unit.key == key) {
                                if item.state != ItemState::Done && !item.blocked {
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
                        .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().font_family(theme::MONO_FONT).text_size(theme::sz(11.5)).text_color(theme::muted()).child(item.detail.clone()))
                        .when(!problems.is_empty(), |d| d.child(div().text_size(theme::sz(11.5)).text_color(if item.blocked { theme::removed() } else { theme::modified() }).child(problems.clone()))),
                )
                .child(div().max_w(theme::sz(300.)).text_size(theme::sz(12.)).text_color(color).child(label))
                .when(waiting, |d| {
                    d.child(ui::button(SharedString::from(format!("switch-force-{index}")), "Move now").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        with_switch(this, |form| set_state(form, &force_unit, ItemState::Forced));
                        this.switch_tick(cx);
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
        .child(div().text_size(theme::sz(12.5)).text_color(theme::text_soft()).child(explain))
        .child(div().flex().flex_col().gap_0p5().children(rows))
        .when_some(form.parked.clone(), |d, parked| {
            let (text, color) = match parked {
                Ok(n) => (format!("A copy of {n} unticked worktree{} with uncommitted work is on {server} too; it moves there when opened there.", if n == 1 { "" } else { "s" }), theme::muted()),
                Err(err) => (format!("The unticked worktrees could not leave a copy on {server}: {err}"), theme::removed()),
            };
            d.child(div().text_size(theme::sz(12.)).text_color(color).child(text))
        })
        .when(done, |d| {
            let (text, color) = match (form.direction, failed) {
                (Direction::ToServer, 0) => (format!("Everything runs on {server} now. Safe to close the lid."), theme::added()),
                (Direction::ToLaptop, 0) => (format!("Everything is back on {}. Open a session to continue it.", crate::platform::this_computer()), theme::added()),
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
        Direction::ToLaptop => crate::platform::this_computer().to_string(),
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
            this.switch_park_unticked(cx);
            this.switch_tick(cx);
        }))
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().flex_1().text_size(theme::sz(12.)).text_color(theme::muted()).child(if form.running {
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
        // Before leaving: every open session to the server; back at the desk: everything home.
        let here = !self.state.sessions.is_empty();
        let there = self.server.state.as_ref().is_some_and(|s| !s.sessions.is_empty());
        let (label, direction) = match (here, there) {
            (true, _) => ("Server mode", Direction::ToServer),
            (false, true) => ("Laptop mode", Direction::ToLaptop),
            (false, false) => return None,
        };
        Some(
            ui::button("switch-mode", label)
                .text_color(theme::muted())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_switch(direction, cx)))
                .into_any_element(),
        )
    }
}
