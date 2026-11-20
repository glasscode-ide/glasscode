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
    /// Recovering moves a restart interrupted (`switch_recover`), at most one round at a time,
    /// and not again before this after a failure.
    pub recovering: bool,
    pub recover_at: Option<Instant>,
    /// The next sync is the last before sleep (`mirror_before_sleep`).
    asleep: bool,
    /// The projects that stay on this Mac, as the last sync read them (the switch leaves them
    /// out; the moves check again themselves).
    /// This Mac's clock went back (crit-9): its changes wait until this time (ms).
    pub clock_back: Option<u64>,
    /// The server's latest stamp, for "use the server's copy".
    pub server_stamp: Option<glasscode_core::identity::Hlc>,
    pub local_only: std::collections::HashSet<String>,
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
            recovering: false,
            recover_at: None,
            asleep: false,
            clock_back: None,
            server_stamp: None,
            local_only: Default::default(),
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
    /// "This Mac's clock went back; its changes wait until 14:30." (crit-9)
    pub(crate) fn clock_back_words(&self, until_ms: u64) -> String {
        let secs = (until_ms / 1000) as i64;
        let day = glasscode_core::day::local(secs);
        let when = if day == glasscode_core::day::today() { glasscode_core::day::clock(secs) } else { format!("{} {}", glasscode_core::day::short(day), glasscode_core::day::clock(secs)) };
        format!("This {}'s clock went back; its changes wait until {when}.", crate::platform::computer())
    }

    /// "Use the server's copy" (crit-9): this Mac's next stamps go on from the server's latest, so
    /// its next changes are in time; what it changed while its clock was wrong stays here.
    pub(crate) fn clock_use_server(&mut self, cx: &mut Context<Self>) {
        let Some(stamp) = self.mirror.server_stamp.clone() else {
            self.show_toast(format!("{} has not said its time yet; try again after the next sync.", self.server.name()), cx);
            return;
        };
        let task = cx.background_spawn(async move { glasscode_core::identity::take_peer_clock(&stamp) });
        cx.spawn(async move |this, cx| {
            let done = task.await;
            let _ = this.update(cx, |ws, cx| {
                match done {
                    Ok(()) => {
                        ws.mirror.clock_back = None;
                        ws.show_toast(format!("This {} goes on from {}'s time now.", crate::platform::computer(), ws.server.name()), cx);
                    }
                    Err(err) => ws.show_toast(format!("This {}'s clock could not be set from the server's: {err}", crate::platform::computer()), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

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
        let answers_waiting = self.control.remote_out.lock().is_ok_and(|o| !o.is_empty()) || self.mirror.asleep;
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
        let worktree_keys = glasscode_core::switch::idle_worktrees(&self.state, &self.worktrees).into_iter().filter(|u| !self.mirror.local_only.contains(&u.project_id)).map(|u| u.key).collect();
        let extras = glasscode_core::mirror::Extras {
            worktree_keys,
            board: serde_json::to_value(&self.today.board).ok(),
            asleep: std::mem::take(&mut self.mirror.asleep),
            today: serde_json::to_value(&snapshot).ok(),
            cli_replies: replies,
            today_acks: self.mirror.today_acks.clone(),
            cli_acks: self.mirror.cli_acks.clone(),
            skill_off: self.state.agents.skill_off,
            // The approved authority goes one way, to a server that keeps it (crit-5).
            authority: (self.control.engine_v2 && self.server.client.as_ref().is_some_and(|c| c.hello.features.iter().any(|f| f == "authority")))
                .then(|| glasscode_core::gate::FromMac::of(&self.control.authority, None)),
            journal_sent: self.journal_sent(),
            // What the server's manager needs to run its wakes while it is home (M57).
            manager: self.control.engine_v2.then(|| glasscode_core::remote::ManagerFromMac {
                enabled: self.state.manager.enabled,
                agent: Some(self.state.manager.brain),
                model: self.state.manager.model.clone(),
                account: self.state.manager.account.clone(),
                projects: self.lanes_projects(),
            }),
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
                        this.mirror.local_only = outcome.local_only.clone();
                        // crit-9: say it once when it starts; the title bar keeps saying it.
                        if outcome.clock_back.is_some() && this.mirror.clock_back.is_none() {
                            this.show_toast(this.clock_back_words(outcome.clock_back.unwrap_or_default()), cx);
                        }
                        this.mirror.clock_back = outcome.clock_back;
                        this.mirror.server_stamp = outcome.server_stamp.clone();
                        if outcome.pending_changed {
                            this.control_refresh_pending(cx);
                        }
                        if !outcome.work.is_empty() {
                            this.journal_work_take_theirs(&outcome.work, cx);
                        }
                        if let Some(sent) = outcome.journal_sent.clone() {
                            this.journal_set_sent(sent);
                        }
                        // Window ops the server's manager asked for while this Mac was away: each
                        // once, through this Mac's gate, as the manager's.
                        for queued in outcome.window_ops.clone() {
                            // A card the person approved here for an op only this Mac runs.
                            if let Some(card) = queued.card {
                                this.with_main_window(cx, move |ws, window, cx| ws.control_run_server_card(card, window, cx));
                                continue;
                            }
                            let Ok(op) = serde_json::from_value::<glasscode_core::ops::Op>(queued.op) else { continue };
                            this.with_main_window(cx, move |ws, window, cx| {
                                let who = ws.brain_caller(glasscode_core::caller::Caller::Manager { lane: glasscode_core::caller::Lane::Wake, wake: None, project: None });
                                ws.control_gated(op, who, Vec::new(), String::new(), None, window, cx);
                            });
                        }
                        for notice in outcome.notices.clone() {
                            log::info!("manager on the server ({}): {}", notice.matter, notice.body);
                            this.notify_manager(notice.title, notice.body, cx);
                        }
                        this.mirror.error = outcome.skipped.first().map(|(title, why)| format!("{title}: {why}"));
                        for (id, inputs) in outcome.inbox {
                            this.mirror_apply(&id, inputs, cx);
                        }
                        for (id, server_id) in outcome.moved {
                            this.mirror_moved(&id, server_id, cx);
                        }
                        // What changed on the server's board (a phone, the manager there) comes
                        // here; this Mac's own changes went there with this sync.
                        if let Some(server_board) = &outcome.board {
                            if this.today.board.merge(server_board) {
                                this.today_save(cx);
                            }
                        }
                        // Who owns this Mac's worktrees, as the server says (kept on disk, so a
                        // restart starts nothing the server owns). Unreadable there: keep what
                        // was known.
                        if let (Some(mac), false) = (&outcome.machine, outcome.owners_unknown) {
                            let mut elsewhere: Vec<glasscode_core::owners::WorktreeKey> = outcome.owners.iter().filter(|(_, record)| &record.owner != mac).map(|(key, _)| key.clone()).collect();
                            elsewhere.sort();
                            if elsewhere != this.state.worktrees_elsewhere {
                                this.state.worktrees_elsewhere = elsewhere;
                                this.save();
                            }
                            let stopping: Vec<String> = this.state.sessions.iter().filter(|s| this.terminals.contains_key(&s.id) && this.runs_elsewhere(&s.id)).map(|s| s.id.clone()).collect();
                            for id in stopping {
                                this.mirror_owned_elsewhere(&id, cx);
                            }
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
                MirrorInput::Text { text, enter, device } => {
                    // Typing into an open question would answer it.
                    if prompt::parse(&terminal.read(cx).text_tail(60)).is_some() {
                        self.show_toast(format!("\"{title}\" is asking something; answer it first (the iPhone's message was not sent)"), cx);
                        continue;
                    }
                    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    // Relayed by the server, which names the phone its connection declared
                    // (M13.2); from an older server, or not from a phone, it is nobody's (D1).
                    glasscode_core::words::record(relayed_origin(device), &line, Some(id), None);
                    later(line.into_bytes(), Duration::ZERO, cx);
                    if enter {
                        later(b"\r".to_vec(), Duration::from_millis(250), cx);
                    }
                }
                MirrorInput::Answer { option, text, device } => {
                    let Some(open) = prompt::parse(&terminal.read(cx).text_tail(60)) else { continue };
                    let Some(keys) = prompt::keys_for(&open, option) else { continue };
                    // One key at a time, as a person types: the menu reads each.
                    for bytes in keys.iter().filter_map(|k| mirror::key_bytes(k)) {
                        later(bytes.to_vec(), Duration::from_millis(80), cx);
                    }
                    if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
                        glasscode_core::words::record(relayed_origin(device), &text, Some(id), None);
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
        let Some(title) = self.state.session(id).map(|s| s.title.clone()) else { return };
        self.stop_terminal(id);
        self.server_refresh(cx);
        self.show_toast(format!("\"{title}\": its worktree runs on {} now, so it stopped here. Open it to bring the worktree back", self.server.name()), cx);
    }

    /// The system is going to sleep: one last sync at once, saying so, so the server may continue
    /// a session that was working without the usual wait. Best effort: GPUI's notice does not
    /// hold sleep (the manager plan's M58 adds a held window); the 4-second sync stays the floor.
    pub fn mirror_before_sleep(&mut self, cx: &mut Context<Self>) {
        self.mirror.asleep = true;
        self.mirror_tick(cx);
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

/// Whose words a phone's input relayed by the server is: the phone's, by the device its
/// connection declared, or nobody's. A device is tier two at most (D1), whatever the server says.
fn relayed_origin(device: Option<String>) -> glasscode_core::words::Origin {
    match device.filter(|device| glasscode_core::caller::device_id_ok(device)) {
        Some(device) => glasscode_core::words::Origin::Device(device),
        None => glasscode_core::words::Origin::Outside,
    }
}

#[cfg(test)]
mod tests {
    use glasscode_core::words::{Origin, Tier, tier_of};

    #[test]
    fn a_relayed_phone_input_is_the_phones_never_more() {
        assert_eq!(super::relayed_origin(Some("phone-1a2b".into())), Origin::Device("phone-1a2b".into()));
        assert_eq!(tier_of(&super::relayed_origin(Some("phone-1a2b".into()))), Tier::Two);
        for device in [None, Some(String::new()), Some("a b".into())] {
            assert_eq!(super::relayed_origin(device), Origin::Outside);
        }
    }
}
