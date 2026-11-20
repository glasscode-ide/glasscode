//! Keeping this Mac's sessions in sync with the server (`glasscode_core::mirror`): every few
//! seconds while connected, on a connection of its own. What the iPhone does to a Mac session
//! (answer, type, keys) comes back and is typed into its terminal here; a session someone
//! continued on the server closes here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context};
use glasscode_core::mirror::{self, Live, Syncer};
use glasscode_core::prompt;
use glasscode_core::remote::{MirrorInput, RemoteClient};

use crate::center::Center;
use crate::workspace::Workspace;

const EVERY: Duration = Duration::from_secs(4);

pub struct MirrorUi {
    syncer: Arc<Mutex<Syncer>>,
    client: Arc<Mutex<Option<Arc<RemoteClient>>>>,
    running: bool,
    last: Instant,
    /// Sessions synced in the last round, and when it was.
    pub synced: Option<(usize, Instant)>,
    pub error: Option<String>,
    /// The server it syncs to (a new one starts over).
    target: Option<String>,
    identity: Option<(String, String)>,
    /// What arrived last round, to acknowledge next round (so the server stops resending it).
    today_acks: Vec<u64>,
    cli_acks: Vec<u64>,
    /// What this Mac already did (the server resends until an ack): newest at the back, bounded.
    applied_ops: std::collections::VecDeque<u64>,
    applied_calls: std::collections::VecDeque<u64>,
    /// Sessions here whose worktree the server owns now: stopped, and opening one brings the
    /// worktree back (`bring_worktree_home`).
    pub elsewhere: std::collections::HashSet<String>,
    /// This Mac's worktrees with no open session that the server owns now (Laptop mode lists
    /// them).
    pub server_owned: Vec<glasscode_core::owners::WorktreeKey>,
}

impl MirrorUi {
    pub fn new() -> Self {
        MirrorUi {
            syncer: Arc::new(Mutex::new(Syncer::default())),
            client: Arc::new(Mutex::new(None)),
            running: false,
            last: Instant::now(),
            synced: None,
            error: None,
            target: None,
            identity: None,
            today_acks: Vec::new(),
            cli_acks: Vec::new(),
            applied_ops: std::collections::VecDeque::new(),
            applied_calls: std::collections::VecDeque::new(),
            elsewhere: Default::default(),
            server_owned: Vec::new(),
        }
    }

    fn remember(seen: &mut std::collections::VecDeque<u64>, id: u64) -> bool {
        if seen.contains(&id) {
            return false;
        }
        seen.push_back(id);
        while seen.len() > 512 {
            seen.pop_front();
        }
        true
    }
}

impl Workspace {
    /// What the app sees of each local session: running, activity, an open question.
    fn mirror_live(&self, cx: &mut Context<Self>) -> HashMap<String, Live> {
        self.state
            .sessions
            .iter()
            .map(|s| {
                let terminal = self.terminals.get(&s.id);
                let prompt = terminal.and_then(|t| prompt::parse(&t.read(cx).text_tail(60)));
                (s.id.clone(), Live { running: terminal.is_some(), activity: if prompt.is_some() { Some(glasscode_core::agents::AgentActivity::NeedsInput) } else { self.activity.get(&s.id).copied() }, prompt })
            })
            .collect()
    }

    pub fn mirror_tick(&mut self, cx: &mut Context<Self>) {
        let supported = self.server.client.as_ref().is_some_and(|c| c.hello.features.iter().any(|f| f == "mirror"));
        let Some(config) = self.server.config.clone().filter(|_| self.server.connected() && supported) else { return };
        // Answers for the server's sessions go at once, not after the usual wait.
        let answers_waiting = self.control.remote_out.lock().is_ok_and(|o| !o.is_empty());
        if self.mirror.running || (self.mirror.last.elapsed() < EVERY && !answers_waiting) {
            return;
        }
        let target = config.ssh_target.clone();
        if self.mirror.target.as_ref() != Some(&target) {
            self.mirror.syncer.lock().unwrap().reset();
            *self.mirror.client.lock().unwrap() = None;
            self.mirror.target = Some(target);
        }
        self.mirror.running = true;
        self.mirror.last = Instant::now();
        let identity = self.mirror.identity.get_or_insert_with(mirror::identity).clone();
        let live = self.mirror_live(cx);
        let local = self.state.clone();
        let snapshot = self.today_snapshot(&identity.0, &identity.1);
        let replies: Vec<glasscode_core::remote::RemoteCallReply> = self.control.remote_out.lock().map(|mut o| std::mem::take(&mut *o)).unwrap_or_default();
        let kept = replies.clone();
        let worktree_keys = glasscode_core::switch::idle_worktrees(&self.state, &self.worktrees).into_iter().map(|u| u.key).collect();
        let extras = glasscode_core::mirror::Extras {
            worktree_keys,
            today: serde_json::to_value(&snapshot).ok(),
            cli_replies: replies,
            today_acks: self.mirror.today_acks.clone(),
            cli_acks: self.mirror.cli_acks.clone(),
            skill_off: self.state.agents.skill_off,
        };
        let (syncer, client) = (self.mirror.syncer.clone(), self.mirror.client.clone());
        let task = cx.background_spawn(async move {
            let connection = {
                let mut slot = client.lock().unwrap();
                match slot.as_ref().filter(|c| c.is_alive()) {
                    Some(c) => c.clone(),
                    None => {
                        let c = Arc::new(RemoteClient::connect(&config)?);
                        *slot = Some(c.clone());
                        c
                    }
                }
            };
            let result = syncer.lock().unwrap().sync(&connection, &config, &local, &live, &identity.1, &identity.0, extras);
            if result.is_err() && !connection.is_alive() {
                *client.lock().unwrap() = None;
            }
            result
        });
        cx.spawn(async move |this, cx| {
            let result: anyhow::Result<glasscode_core::mirror::Outcome> = task.await;
            let _ = this.update(cx, |this, cx| {
                this.mirror.running = false;
                match result {
                    Ok(outcome) => {
                        this.mirror.synced = Some((outcome.synced, Instant::now()));
                        this.mirror.error = outcome.skipped.first().map(|(title, why)| format!("{title}: {why}"));
                        for (id, inputs) in outcome.inbox {
                            this.mirror_apply(&id, inputs, cx);
                        }
                        for (id, server_id) in outcome.moved {
                            this.mirror_moved(&id, server_id, cx);
                        }
                        // A worktree the server owns now (it continued it while this Mac was
                        // away): its sessions here must not run again.
                        if let Some(mac) = &outcome.machine {
                            let home = glasscode_core::paths::home();
                            let elsewhere: Vec<String> = this
                                .state
                                .sessions
                                .iter()
                                .filter(|s| {
                                    let project = this.state.project(&s.project_id).map(|p| p.name.clone()).unwrap_or_default();
                                    let key = glasscode_core::owners::WorktreeKey::of(&s.worktree, &home, &project, Some(&s.branch));
                                    !glasscode_core::switch::may_run_here(&outcome.owners, &key, mac)
                                })
                                .map(|s| s.id.clone())
                                .collect();
                            for id in &elsewhere {
                                this.mirror_owned_elsewhere(id, cx);
                            }
                            // The ones the server gave back run here again (when opened).
                            this.mirror.elsewhere.retain(|id| elsewhere.contains(id));
                            this.mirror.server_owned = outcome.owners.iter().filter(|(key, record)| &record.owner != mac && !this.state.sessions.iter().any(|s| {
                                let project = this.state.project(&s.project_id).map(|p| p.name.clone()).unwrap_or_default();
                                &glasscode_core::owners::WorktreeKey::of(&s.worktree, &home, &project, Some(&s.branch)) == *key
                            })).map(|(key, _)| key.clone()).collect();
                        }
                        // Acknowledge everything that arrived (so the server stops resending it),
                        // and do each thing once.
                        this.mirror.today_acks = outcome.today_seen;
                        this.mirror.cli_acks = outcome.calls_seen;
                        let ops: Vec<glasscode_core::remote::TodayOp> = outcome.today_ops.into_iter().filter(|(seq, _)| MirrorUi::remember(&mut this.mirror.applied_ops, *seq)).map(|(_, op)| op).collect();
                        let calls: Vec<glasscode_core::remote::RemoteCall> = outcome.cli_calls.into_iter().filter(|c| MirrorUi::remember(&mut this.mirror.applied_calls, c.id)).collect();
                        if !ops.is_empty() || !calls.is_empty() {
                            this.with_main_window(cx, move |ws, window, cx| {
                                for op in ops {
                                    ws.today_apply(op, window, cx);
                                }
                                for call in calls {
                                    ws.control_remote_call(call, window, cx);
                                }
                            });
                        }
                    }
                    Err(error) => {
                        this.mirror.error = Some(format!("{error:#}"));
                        // The answers go with the next sync.
                        if let Ok(mut out) = this.control.remote_out.lock() {
                            let mut kept = kept;
                            kept.append(&mut out);
                            *out = kept;
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Do here what the phone asked for, the way a person at the keyboard would.
    fn mirror_apply(&mut self, id: &str, inputs: Vec<MirrorInput>, cx: &mut Context<Self>) {
        let title = self.state.session(id).map(|s| s.title.clone()).unwrap_or_default();
        let Some(terminal) = self.terminals.get(id).cloned() else {
            self.show_toast(format!("Your iPhone sent something to \"{title}\", but it is not running on {}", crate::platform::this_computer()), cx);
            return;
        };
        let mut delay = Duration::ZERO;
        let mut later = |bytes: Vec<u8>, after: Duration, cx: &mut Context<Self>| {
            delay += after;
            let (terminal, at) = (terminal.clone(), delay);
            cx.spawn(async move |_, cx| {
                cx.background_executor().timer(at).await;
                let _ = terminal.update(cx, |t, cx| t.write(bytes, cx));
            })
            .detach();
        };
        for input in inputs {
            match input {
                MirrorInput::Text { text, enter } => {
                    // Typing into an open question would answer it.
                    if prompt::parse(&terminal.read(cx).text_tail(60)).is_some() {
                        self.show_toast(format!("\"{title}\" is asking something; answer it first (the iPhone's message was not sent)"), cx);
                        continue;
                    }
                    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    later(line.into_bytes(), Duration::ZERO, cx);
                    if enter {
                        later(b"\r".to_vec(), Duration::from_millis(250), cx);
                    }
                }
                MirrorInput::Answer { option, text } => {
                    let Some(open) = prompt::parse(&terminal.read(cx).text_tail(60)) else { continue };
                    let Some(keys) = prompt::keys_for(&open, option) else { continue };
                    // One key at a time, as a person types: the menu reads each.
                    for bytes in keys.iter().filter_map(|k| mirror::key_bytes(k)) {
                        later(bytes.to_vec(), Duration::from_millis(80), cx);
                    }
                    if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
                        later(text.into_bytes(), Duration::from_millis(500), cx);
                        later(b"\r".to_vec(), Duration::from_millis(250), cx);
                    }
                }
                MirrorInput::Keys { keys } => {
                    for bytes in keys.iter().filter_map(|k| mirror::key_bytes(k)) {
                        later(bytes.to_vec(), Duration::from_millis(80), cx);
                    }
                }
            }
        }
    }

    /// Someone continued this session on the server: it ends here (its folder stays).
    fn mirror_moved(&mut self, id: &str, server_id: String, cx: &mut Context<Self>) {
        let Some(title) = self.state.session(id).map(|s| s.title.clone()) else { return };
        let was_open = self.state.active_session.as_deref() == Some(id) && self.center == Center::Session;
        self.remove_session_quietly(id);
        if was_open {
            self.center = Center::Server(server_id);
        }
        self.server_refresh(cx);
        self.show_toast(format!("\"{title}\" continued on {}; it runs there now", self.server.name()), cx);
    }

    /// Its worktree runs on the server now: the agent here stops (the session stays listed), and
    /// opening it brings the worktree back first.
    fn mirror_owned_elsewhere(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.mirror.elsewhere.contains(id) {
            return;
        }
        let Some(title) = self.state.session(id).map(|s| s.title.clone()) else { return };
        self.mirror.elsewhere.insert(id.to_string());
        if self.terminals.contains_key(id) {
            self.stop_terminal(id);
        }
        self.server_refresh(cx);
        self.show_toast(format!("\"{title}\": its worktree runs on {} now, so it stopped here. Open it to bring the worktree back", self.server.name()), cx);
    }

    /// For the Server dialog: "3 sessions synced to devbox · 2 s ago".
    pub fn mirror_status(&self) -> Option<String> {
        if let Some(error) = &self.mirror.error {
            return Some(format!("Sync: {error}"));
        }
        let (n, at) = self.mirror.synced?;
        Some(format!("{n} session{} on {} synced to {} · {} s ago", if n == 1 { "" } else { "s" }, crate::platform::this_computer(), self.server.name(), at.elapsed().as_secs()))
    }
}
