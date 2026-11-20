//! The manager's copy on the server: keeping the two copies in step, and the test in
//! Settings › Manager.
//!
//! While the manager's home is the server and the server is connected, this Mac calls
//! `manager.sync` every few seconds, off the main thread: its settings, what it sees of its
//! own sessions and its data (only when the server does not hold the same) go there; the
//! server's data and what the server's manager wants done here come back. The server's rounds
//! count as running while its heartbeat is fresh (`manager_on_server`); when it goes stale, this
//! Mac's own rounds take over. Questions are the chat lane's (M52.1), never the server copy's.
//!
//! Nothing is lost when a call fails: the server sends an action again until this Mac named
//! it; each is done once here.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context};
use glasscode_core::control::{Kind, Rule};
use glasscode_core::manager::sync::{self, SyncReply, SyncRequest, TestLine};
use glasscode_core::manager::{Home, store};
use glasscode_core::model::now;
use glasscode_core::remote::Hello;
use serde_json::json;

use crate::manager_state::Done;
use crate::workspace::Workspace;

const EVERY: Duration = Duration::from_secs(4);
/// The server runs the manager while its last round is at most this old.
const FRESH_SECS: u64 = 30;

/// What the sync keeps between rounds (`ManagerUi::home`).
#[derive(Default)]
pub struct HomeUi {
    /// The actions that arrived last time, to name next time (the server then forgets them).
    acks: Vec<u64>,
    /// Actions already done here: the server sends one again until it hears this Mac has it.
    applied: VecDeque<u64>,
    /// The fingerprint of the server's data after the last sync (0: not known).
    server_has: u64,
    /// The server was told to run the manager, so it must hear when that ends (None: not
    /// read from disk yet).
    told: Option<bool>,
    /// Why the last sync failed, and when one last worked.
    pub error: Option<String>,
    pub synced: Option<Instant>,
}

/// A mark that outlives the app: the server's copy was told to run. Without it this Mac never
/// calls the server about the manager while the manager is off or at home here.
fn told_file() -> PathBuf {
    store::dir().join("on-server")
}

impl HomeUi {
    fn told(&mut self) -> bool {
        *self.told.get_or_insert_with(|| told_file().exists())
    }

    fn set_told(&mut self, told: bool) {
        if self.told == Some(told) {
            return;
        }
        self.told = Some(told);
        if told {
            let _ = std::fs::create_dir_all(store::dir());
            let _ = std::fs::write(told_file(), b"");
        } else {
            let _ = std::fs::remove_file(told_file());
        }
    }
}

/// True the first time `id` is seen (the list is bounded: the newest stay).
fn first_time<T: PartialEq>(seen: &mut VecDeque<T>, id: T) -> bool {
    if seen.contains(&id) {
        return false;
    }
    seen.push_back(id);
    while seen.len() > 512 {
        seen.pop_front();
    }
    true
}

fn line(what: &str, ok: bool, detail: impl Into<String>) -> TestLine {
    TestLine { what: what.into(), ok, detail: detail.into() }
}

/// Whether the server's daemon is the build this app brings.
fn version_line(hello: &Hello, name: &str) -> TestLine {
    let what = "Same version";
    let mine = glasscode_core::update::BUILD;
    let theirs = glasscode_core::update::describe(&hello.version, hello.build);
    if mine == 0 || hello.build == 0 {
        line(what, true, format!("glasscoded {theirs} on {name}; a development build, so not compared"))
    } else if hello.build == mine {
        line(what, true, format!("glasscoded {theirs}, the same as this app"))
    } else if hello.build < mine {
        line(what, false, format!("glasscoded {theirs} on {name} is older than this app ({mine}). Update it in the Server dialog."))
    } else {
        line(what, true, format!("glasscoded {theirs} on {name} is newer than this app ({mine}). Update Glasscode when you can."))
    }
}

impl Workspace {
    /// Called from the workspace tick. Syncs with the server every few seconds when the
    /// manager's home is the server and the server is connected.
    pub fn manager_home_tick(&mut self, cx: &mut Context<Self>) {
        for done in std::mem::take(&mut self.manager.home_inbox) {
            self.manager_home_done(done, cx);
        }
        let settings = self.state.manager.clone();
        let wanted = settings.enabled && settings.home == Home::Server;
        if !wanted {
            // Its home is this Mac, or it is off: nothing waits for the server any more.
            self.manager.server_heartbeat = 0;
            // The server only needs to hear it once, when it was running the manager.
            if !self.manager.home.told() {
                return;
            }
        }
        let client = self.server.client.clone().filter(|client| self.server.connected() && client.hello.features.iter().any(|f| f == "manager"));
        let Some(client) = client else { return };
        if self.manager.sync_running || self.manager.last_sync.is_some_and(|at| at.elapsed() < EVERY) {
            return;
        }

        let mut request = SyncRequest {
            settings: Some(settings.clone()),
            acked: self.manager.home.acks.clone(),
            utc_offset: Some(sync::utc_offset(now())),
            ..SyncRequest::default()
        };
        if wanted {
            request.snapshot = Some(self.manager_snapshot());
            // The data only travels when the server does not hold the same.
            // v1's data.json: with v2's outbox (`handover`), v1's in its place.
            let data = self.manager.harness.v1_data();
            request.fingerprint = sync::fingerprint(&data);
            if request.fingerprint != self.manager.home.server_has {
                request.data = data;
            }
            // Projects it leaves alone, by name: the server has its own project ids.
            let off = self.state.projects.iter().filter(|p| settings.off_projects.contains(&p.id) || self.state.hidden_projects.contains(&p.id));
            request.off_projects = off.flat_map(|p| [Some(p.name.clone()), p.root.file_name().map(|n| n.to_string_lossy().into_owned())]).flatten().collect();
            request.off_projects.sort();
            request.off_projects.dedup();
            request.never_tell = self.state.agents.rule(Kind::Send) == Rule::Never;
        }
        self.manager.sync_running = true;
        self.manager.last_sync = Some(Instant::now());
        let task = cx.background_spawn(async move { client.call::<SyncReply>("manager.sync", serde_json::to_value(&request)?) });
        cx.spawn(async move |this, cx| {
            let result: anyhow::Result<SyncReply> = task.await;
            let _ = this.update(cx, |this, cx| this.manager_home_done(Done::Server(result.map_err(|error| format!("{error:#}"))), cx));
        })
        .detach();
    }

    /// Whether the server's copy runs the manager right now (so the Mac's own rounds rest).
    pub fn manager_on_server(&self) -> bool {
        sync::runs_on_server(&self.state.manager, self.manager.server_heartbeat, now(), FRESH_SECS)
    }

    /// Run the server test (Settings › Manager): the lines land in `ManagerUi::test`.
    pub fn manager_test_server(&mut self, cx: &mut Context<Self>) {
        if self.manager.testing {
            return;
        }
        let name = self.server.name();
        let Some(client) = self.server.client.clone().filter(|_| self.server.connected()) else {
            let detail = if self.server.config.is_some() { format!("{name} cannot be reached right now.") } else { "No server is set up yet (Settings › Server & devices).".to_string() };
            self.manager.test = Some(vec![line("Server", false, detail)]);
            cx.notify();
            return;
        };
        let mut lines = vec![line("Server", true, format!("Connected to {name}")), version_line(&client.hello, &name)];
        if !client.hello.features.iter().any(|f| f == "manager") {
            lines.push(line("Manager", false, format!("glasscoded on {name} cannot run the manager yet. Update it in the Server dialog.")));
            self.manager.test = Some(lines);
            cx.notify();
            return;
        }
        self.manager.testing = true;
        let settings = self.state.manager.clone();
        let task = cx.background_spawn(async move { client.call::<Vec<TestLine>>("manager.test", json!({ "settings": settings })) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(more) => lines.extend(more),
                    Err(error) => lines.push(line("Manager", false, format!("{error:#}"))),
                }
                this.manager_home_done(Done::Test(lines), cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// A sync or the test came back.
    fn manager_home_done(&mut self, done: Done, cx: &mut Context<Self>) {
        match done {
            Done::Server(result) => {
                self.manager.sync_running = false;
                match result {
                    Ok(reply) => self.manager_home_reply(reply, cx),
                    // Quietly: the heartbeat goes stale by itself and this Mac takes over.
                    Err(error) => self.manager.home.error = Some(error),
                }
            }
            Done::Test(lines) => {
                self.manager.testing = false;
                self.manager.test = Some(lines);
            }
            _ => {}
        }
        cx.notify();
    }

    /// Take in what the server sent: its data, whether it runs the manager, and what to do here.
    fn manager_home_reply(&mut self, reply: SyncReply, cx: &mut Context<Self>) {
        let settings = self.state.manager.clone();
        let wanted = settings.enabled && settings.home == Home::Server;
        self.manager.home.error = None;
        self.manager.home.synced = Some(Instant::now());

        // The server left its data out when this Mac already held the same.
        if reply.fingerprint == 0 || reply.fingerprint != sync::fingerprint(&self.manager.harness.v1_data()) {
            if self.manager.harness.merge_theirs(&reply.data) {
                self.manager.harness.dirty = true;
            }
        }
        self.manager.home.server_has = reply.fingerprint;
        self.manager.server_heartbeat = if wanted { sync::heartbeat_here(&reply, now()) } else { 0 };
        if wanted {
            self.manager.home.set_told(true);
        } else if reply.heartbeat == 0 {
            // The server heard that its copy rests. What it still had is kept here.
            self.manager.home.set_told(false);
            self.manager.harness.save();
        }

        // Name everything that arrived next time, and do each thing once.
        self.manager.home.acks = reply.action_ids.clone();
        let mut actions = Vec::new();
        for (index, action) in reply.actions.into_iter().enumerate() {
            let new = match reply.action_ids.get(index) {
                Some(id) => first_time(&mut self.manager.home.applied, *id),
                None => true,
            };
            if new {
                actions.push(action);
            }
        }
        if settings.enabled && !actions.is_empty() {
            self.manager_apply(actions, cx);
        }
    }
}
