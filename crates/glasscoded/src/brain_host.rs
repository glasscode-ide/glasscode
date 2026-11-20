//! glasscoded's side of the brain runtime (M50; D18): `DaemonHost`, the `runtime::Host` of the
//! server. It has the server's own kinds of ops (Core, and SessionHost for its tmux sessions) and
//! never the window's; what it has no handler for here it says is the Mac's. Its notices are kept
//! for the next sync (the Mac shows them) and logged.
//!
//! The server's brain is not in release 1 (D5): this is the seam the runtime is written against,
//! so that it runs here unchanged once it does.
#![cfg_attr(not(test), allow(dead_code))]

use std::sync::Mutex;

use glasscode_core::caller::{self, Lane};
use glasscode_core::ops::{ErrorCode, Op, OpError, OpHost, OpReply};
use glasscode_core::paths::MachineId;

/// A notice for the person, kept until the Mac takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
    pub matter: String,
}

pub struct DaemonHost {
    machine: MachineId,
    notices: Mutex<Vec<Notice>>,
    checked: Mutex<Vec<(Lane, Option<String>)>>,
}

impl DaemonHost {
    pub fn new(machine: MachineId) -> DaemonHost {
        DaemonHost { machine, notices: Mutex::new(Vec::new()), checked: Mutex::new(Vec::new()) }
    }

    /// The notices since the last call, for the sync.
    pub fn take_notices(&self) -> Vec<Notice> {
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
        // The server runs no op handler of its own yet: each op is the Mac's.
        OpReply::Failed(OpError::new(ErrorCode::Unavailable { host: op.spec.host, queued: false }, format!("{} runs on the Mac; the server has no handler for it yet.", op.name())))
    }

    fn has(&self, host: OpHost) -> bool {
        host != OpHost::Window
    }

    fn notify(&self, title: &str, body: &str, matter: &str) {
        eprintln!("glasscoded: manager ({matter}): {title}: {body}");
        let mut notices = self.notices.lock().unwrap_or_else(|p| p.into_inner());
        // One a matter: a newer one replaces it.
        notices.retain(|n| n.matter != matter);
        notices.push(Notice { title: title.into(), body: body.into(), matter: matter.into() });
    }

    fn brain_checked(&self, lane: Lane, wake: Option<&str>) {
        self.checked.lock().unwrap_or_else(|p| p.into_inner()).push((lane, wake.map(str::to_string)));
    }

    /// A card is the Mac's to show: the server makes none (its wakes' proposals wait for the Mac).
    fn propose(&self, op: &Op, _reason: &str, _lane: Lane, _wake: Option<&str>) -> Result<String, String> {
        Err(format!("the server shows no cards; {} waits for the Mac", op.name()))
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
