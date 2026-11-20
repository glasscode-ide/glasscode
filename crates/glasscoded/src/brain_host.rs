//! glasscoded's side of the brain runtime (M50; D18): `DaemonHost`, the `runtime::Host` of the
//! server, which the wake lane's worker runs against while the server is the manager's home
//! (M57, `crate::brain`). It has the server's own kinds of ops (Core, and SessionHost for its tmux
//! sessions) and never the window's. Every step checks that the server is still home at the
//! generation the worker started at: a wake that outlives its home does nothing more.
//!
//! - **Ops.** The server runs no op handler of its own yet: each op is the Mac's
//!   (`Unavailable`, not queued).
//! - **Cards.** A proposal is a pending action in the server's store, asked on the server; the
//!   sync shows it on the Mac as a card, and the person's answer comes back (no authority).
//! - **Notices** are kept, one a matter, until the next sync takes them to the Mac.

use std::sync::Mutex;

use glasscode_core::caller::{self, Lane};
use glasscode_core::ops::{ErrorCode, Op, OpError, OpHost, OpReply};
use glasscode_core::paths::MachineId;
use glasscode_core::remote::ServerNotice;

pub struct DaemonHost {
    machine: MachineId,
    /// The home generation the worker started at; none: not tied to home (tests).
    generation: Option<u64>,
    notices: Mutex<Vec<ServerNotice>>,
    checked: Mutex<Vec<(Lane, Option<String>)>>,
}

impl DaemonHost {
    pub fn new(machine: MachineId) -> DaemonHost {
        DaemonHost { machine, generation: None, notices: Mutex::new(Vec::new()), checked: Mutex::new(Vec::new()) }
    }

    /// A host for a worker started while the server is home at `generation`.
    pub fn at_home(machine: MachineId, generation: u64) -> DaemonHost {
        DaemonHost { generation: Some(generation), ..DaemonHost::new(machine) }
    }

    /// Why nothing may happen now: the server is not home at this host's generation any more.
    fn not_home(&self) -> Option<String> {
        let generation = self.generation?;
        (crate::home::server_generation() != Some(generation)).then(|| "the manager's home is not this server now; nothing was done".to_string())
    }

    /// The notices since the last call, for the sync.
    pub fn take_notices(&self) -> Vec<ServerNotice> {
        std::mem::take(&mut *self.notices.lock().unwrap_or_else(|p| p.into_inner()))
    }

    /// Whether a brain's tool list was checked (its MCP server's calls are served then).
    pub fn is_checked(&self, lane: Lane, wake: Option<&str>) -> bool {
        self.checked.lock().unwrap_or_else(|p| p.into_inner()).iter().any(|(l, w)| *l == lane && w.as_deref() == wake)
    }
}

impl glasscode_core::runtime::Host for DaemonHost {
    fn machine(&self) -> &MachineId {
        &self.machine
    }

    fn run_op(&self, op: &Op, _caller: &caller::Caller) -> OpReply {
        if let Some(why) = self.not_home() {
            return OpReply::Failed(OpError::new(ErrorCode::Failed, why));
        }
        // The server runs no op handler of its own yet: each op is the Mac's.
        OpReply::Failed(OpError::new(ErrorCode::Unavailable { host: op.spec.host, queued: false }, format!("{} runs on the Mac; the server has no handler for it yet.", op.name())))
    }

    fn has(&self, host: OpHost) -> bool {
        host != OpHost::Window
    }

    fn notify(&self, title: &str, body: &str, matter: &str) {
        if self.not_home().is_some() {
            return;
        }
        crate::daemon::log(format!("manager ({matter}): {title}: {body}"));
        let mut notices = self.notices.lock().unwrap_or_else(|p| p.into_inner());
        // One a matter: a newer one replaces it.
        notices.retain(|n| n.matter != matter);
        notices.push(ServerNotice { title: title.into(), body: body.into(), matter: matter.into() });
    }

    fn brain_checked(&self, lane: Lane, wake: Option<&str>) {
        self.checked.lock().unwrap_or_else(|p| p.into_inner()).push((lane, wake.map(str::to_string)));
    }

    /// A card: a pending action asked on this server, which the Mac shows and answers.
    fn propose(&self, op: &Op, reason: &str, lane: Lane, wake: Option<&str>) -> Result<String, String> {
        if let Some(why) = self.not_home() {
            return Err(why);
        }
        let now = glasscode_core::clock::now_ms();
        let caller = caller::Caller::Manager { lane, wake: wake.map(str::to_string), project: None };
        let mut action = glasscode_core::gate::PendingAction::new(op, Default::default(), Some(caller), now);
        action.who = "The manager on the server".into();
        action.what = op.spec.summary.to_string();
        action.reason = reason.to_string();
        let kept = glasscode_core::gate::add_pending(action).map_err(|err| format!("the card could not be kept: {err}"))?;
        Ok(kept.number.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glasscode_core::runtime::Host;

    /// A brain started here runs Glasscode's MCP server as `glasscoded cli mcp` (M57).
    #[test]
    fn a_brain_here_starts_glasscodeds_own_server() {
        glasscode_core::mcp::set_program_prefix(&["cli"]);
        assert_eq!(glasscode_core::mcp::server_args(), ["cli", "mcp", "--profile", "manager"]);
        let config = glasscode_core::mcp::claude_config(std::path::Path::new("/home/me/.glasscode/bin/glasscoded"), &[]);
        assert_eq!(config["mcpServers"]["glasscode"]["args"], serde_json::json!(["cli", "mcp", "--profile", "manager"]));
    }

    /// A wake's proposal is a pending action asked on the server (the Mac shows it as a card); a
    /// host whose home generation is past does nothing at all (M57).
    #[test]
    fn a_proposal_waits_on_the_server_and_a_past_home_does_nothing() {
        use glasscode_core::store::Store as _;
        let (_home, _scratch) = crate::daemon::tests::scratch_home();
        let machine = crate::owners::machine().unwrap();
        let generation = crate::home::server_generation().expect("the server is home with no Mac");
        let op = glasscode_core::ops::typed("board.add", serde_json::json!({ "title": "Check the release notes" })).unwrap();
        let host = DaemonHost::at_home(machine.clone(), generation);
        let number = host.propose(&op, "the release is tomorrow", Lane::Wake, Some("w1")).unwrap();
        let pending = glasscode_core::gate::Pending::load().unwrap();
        let card = pending.actions.iter().find(|a| a.number.to_string() == number).unwrap();
        assert_eq!((card.machine.clone(), card.reason.as_str()), (None, "the release is tomorrow"), "asked here");
        let past = DaemonHost::at_home(machine, generation + 1);
        assert!(past.propose(&op, "x", Lane::Wake, None).is_err());
        past.notify("Manager", "Ready.", "health");
        assert!(past.take_notices().is_empty());
        assert!(matches!(past.run_op(&op, &caller::Caller::Manager { lane: Lane::Wake, wake: None, project: None }), OpReply::Failed(OpError { code: ErrorCode::Failed, .. })));
    }

    #[test]
    fn the_server_host_has_no_window_and_keeps_one_notice_a_matter() {
        let host = DaemonHost::new(MachineId::parse("0123456789abcdef").unwrap());
        assert!(host.has(OpHost::Core) && host.has(OpHost::SessionHost) && !host.has(OpHost::Window));
        host.notify("Manager", "Signed out.", "health");
        host.notify("Manager", "Ready.", "health");
        assert_eq!(host.take_notices().iter().map(|n| n.body.as_str()).collect::<Vec<_>>(), ["Ready."]);
        assert!(host.take_notices().is_empty());
        let op = glasscode_core::ops::typed("ui.state", serde_json::Value::Null).unwrap();
        assert!(matches!(host.run_op(&op, &caller::Caller::Manager { lane: Lane::Wake, wake: None, project: None }), OpReply::Failed(OpError { code: ErrorCode::Unavailable { .. }, .. })));
        host.brain_checked(Lane::Wake, Some("w1"));
        assert!(host.is_checked(Lane::Wake, Some("w1")) && !host.is_checked(Lane::Chat, None));
    }
}
