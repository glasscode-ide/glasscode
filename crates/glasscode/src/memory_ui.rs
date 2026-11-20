//! Memory on the Manager page with the v2 engine: what waits for the person (proposals from moving
//! in, sessions, the manager and learning; edits made by hand that are held back) and what memory
//! keeps, file by file, with each entry's standing. Reading and every change run in the
//! background (`memstore::overview`, `Memory`); the person's click is the person's (tier one).

use gpui::{AppContext as _, Context};
use glasscode_core::memstore::Memory;
use glasscode_core::memstore::overview::{self, Overview};

use crate::workspace::Workspace;

/// The page's state (`Workspace::memory_page`).
#[derive(Default)]
pub struct MemoryPage {
    pub view: Option<Overview>,
    pub busy: bool,
    pub error: Option<String>,
    /// When it was last read: the page reads again while it is shown.
    pub loaded_at: Option<std::time::Instant>,
}

/// How often the page reads memory again while it is shown (a session's note, an accepted move-in).
const AGAIN: std::time::Duration = std::time::Duration::from_secs(10);

/// A change the person asks for on the page.
pub enum MemoryChange {
    Accept(String),
    AcceptAll(Vec<String>),
    Reject(String),
    KeepEdit(String),
    DiscardEdit(String),
    Forget(String),
}

impl Workspace {
    /// Read memory for the page, off the main thread (v2 only).
    pub(crate) fn memory_load(&mut self, cx: &mut Context<Self>) {
        if !self.control.engine_v2 {
            return;
        }
        let names = self.memory_names();
        self.memory_page.loaded_at = Some(std::time::Instant::now());
        let task = cx.background_spawn(async move { overview::overview(&Memory::open(), &names).map_err(|e| e.to_string()) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                match result {
                    Ok(view) => {
                        ws.memory_page.view = Some(view);
                        ws.memory_page.error = None;
                    }
                    Err(why) => ws.memory_page.error = Some(why),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Settings › Manager › Memory: the Manager page's Memory, with the manager on or off.
    pub(crate) fn memory_open(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.show_manager(window, cx);
        self.manager_set_stage(crate::manager_view::Stage::Memory, cx);
    }

    /// Each tick: the page reads memory again while it is shown.
    pub(crate) fn memory_tick(&mut self, cx: &mut Context<Self>) {
        let shown = self.view == crate::shell::View::Manager && self.manager.stage == crate::manager_view::Stage::Memory;
        if shown && !self.memory_page.busy && self.memory_page.loaded_at.is_none_or(|at| at.elapsed() >= AGAIN) {
            self.memory_load(cx);
        }
    }

    /// Each project's name by its memory key.
    fn memory_names(&self) -> std::collections::BTreeMap<glasscode_core::paths::ProjectKey, String> {
        self.state.projects.iter().filter_map(|p| self.control.project_keys.get(&p.id).map(|key| (key.clone(), p.name.clone()))).collect()
    }

    /// The person's change, in the background; then the page again.
    pub(crate) fn memory_change(&mut self, change: MemoryChange, cx: &mut Context<Self>) {
        if self.memory_page.busy {
            return;
        }
        self.memory_page.busy = true;
        cx.notify();
        let task = cx.background_spawn(async move {
            let memory = Memory::open();
            let done = match &change {
                MemoryChange::Accept(id) => memory.accept_proposal(id).map(|_| ()),
                MemoryChange::AcceptAll(ids) => memory.accept_proposals(ids).map(|_| ()),
                MemoryChange::Reject(id) => memory.reject_proposal(id).map(|_| ()),
                MemoryChange::KeepEdit(path) => memory.accept(std::slice::from_ref(path)).map(|_| ()),
                MemoryChange::DiscardEdit(path) => memory.discard(std::slice::from_ref(path)),
                MemoryChange::Forget(id) => memory.forget(&[id.as_str()], false).map(|_| ()),
            };
            done.map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                ws.memory_page.busy = false;
                if let Err(why) = result {
                    ws.memory_page.error = Some(why);
                }
                ws.memory_load(cx);
                cx.notify();
            });
        })
        .detach();
    }
}
