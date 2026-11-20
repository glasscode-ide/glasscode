//! The app's side of the brain runtime (M50; D18): `AppHost`, the `runtime::Host` the runtime's
//! threads call. Its calls go to the main thread over a channel, where the workspace runs an op
//! for the manager (by the same gate as the `glasscode` command), shows a notification once per
//! matter, or marks a brain's MCP server as checked. It is never called on the main thread
//! (`run_op` waits for its answer).

use std::sync::mpsc;
use std::time::Duration;

use gpui::{Context, Window};
use glasscode_core::caller::{self, Lane};
use glasscode_core::control::{Reply, code};
use glasscode_core::ops::{ErrorCode, Op, OpError, OpHost, OpReply};
use glasscode_core::paths::MachineId;

use crate::control_ui::{Caller, Sink, WakeAct};
use crate::workspace::Workspace;

/// What the runtime asks the main thread. An op's first reply comes back on `reply`: its result,
/// or "waiting" with its card at once when it asks the person (M51: no turn waits on a card).
pub enum HostCall {
    RunOp { op: Op, caller: caller::Caller, reply: mpsc::Sender<Reply> },
    /// A wake's proposal: a card, whatever the gate would allow (M51).
    Propose { op: Op, caller: caller::Caller, reason: String, reply: mpsc::Sender<Reply> },
    /// A wake's op under the playbook that fired it, which the person approved to act (M51).
    RunUnder { op: Op, caller: caller::Caller, playbook: String, reply: mpsc::Sender<Reply> },
    Notify { title: String, body: String, matter: String },
    Checked { lane: Lane, wake: Option<String> },
}

/// The Mac's host: it has every kind of op (the window, the sessions, the network).
pub struct AppHost {
    machine: MachineId,
    calls: async_channel::Sender<HostCall>,
}

/// How long an op the runtime asks for may take before it is given up.
const OP_WAIT: Duration = Duration::from_secs(600);
/// How long a proposal's card may take (its targets and its check are read first).
const PROPOSE_WAIT: Duration = Duration::from_secs(60);

impl AppHost {
    pub fn new(machine: MachineId, calls: async_channel::Sender<HostCall>) -> AppHost {
        AppHost { machine, calls }
    }
}

impl glasscode_core::runtime::Host for AppHost {
    fn machine(&self) -> &MachineId {
        &self.machine
    }

    fn run_op(&self, op: &Op, caller: &caller::Caller) -> OpReply {
        let (reply, answer) = mpsc::channel();
        if self.calls.send_blocking(HostCall::RunOp { op: op.clone(), caller: caller.clone(), reply }).is_err() {
            return OpReply::Failed(OpError::new(ErrorCode::Unavailable { host: OpHost::Window, queued: false }, "Glasscode is closing."));
        }
        answer.recv_timeout(OP_WAIT).map(|reply| op_reply(&reply)).unwrap_or_else(|_| OpReply::Failed(OpError::new(ErrorCode::Failed, format!("{} gave no answer in {} minutes.", op.name(), OP_WAIT.as_secs() / 60))))
    }

    fn run_under(&self, op: &Op, caller: &caller::Caller, playbook: &str) -> OpReply {
        let (reply, answer) = mpsc::channel();
        if self.calls.send_blocking(HostCall::RunUnder { op: op.clone(), caller: caller.clone(), playbook: playbook.to_string(), reply }).is_err() {
            return OpReply::Failed(OpError::new(ErrorCode::Unavailable { host: OpHost::Window, queued: false }, "Glasscode is closing."));
        }
        answer.recv_timeout(OP_WAIT).map(|reply| op_reply(&reply)).unwrap_or_else(|_| OpReply::Failed(OpError::new(ErrorCode::Failed, format!("{} gave no answer in {} minutes.", op.name(), OP_WAIT.as_secs() / 60))))
    }

    fn propose(&self, op: &Op, reason: &str, lane: Lane, wake: Option<&str>) -> Result<String, String> {
        let (reply, answer) = mpsc::channel();
        let caller = caller::Caller::Manager { lane, wake: wake.map(str::to_string), project: None };
        if self.calls.send_blocking(HostCall::Propose { op: op.clone(), caller, reason: reason.to_string(), reply }).is_err() {
            return Err("Glasscode is closing.".into());
        }
        match answer.recv_timeout(PROPOSE_WAIT) {
            Ok(Reply::Waiting { approval, .. }) => Ok(approval.to_string()),
            Ok(Reply::Done { text, .. }) => Err(text),
            Err(_) => Err(format!("no card was made for {} in {} seconds", op.name(), PROPOSE_WAIT.as_secs())),
        }
    }

    fn has(&self, _host: OpHost) -> bool {
        true
    }

    fn notify(&self, title: &str, body: &str, matter: &str) {
        let _ = self.calls.send_blocking(HostCall::Notify { title: title.into(), body: body.into(), matter: matter.into() });
    }

    fn brain_checked(&self, lane: Lane, wake: Option<&str>) {
        let _ = self.calls.send_blocking(HostCall::Checked { lane, wake: wake.map(str::to_string) });
    }
}

/// An app reply as the runtime reads it.
pub fn op_reply(reply: &Reply) -> OpReply {
    match reply {
        Reply::Waiting { approval, summary } => OpReply::Waiting { pending: approval.to_string(), summary: summary.clone() },
        Reply::Done { code: code::OK, text, data } => OpReply::Done { text: text.clone(), data: data.clone() },
        Reply::Done { code, text, .. } => {
            let class = match *code {
                code::USAGE => ErrorCode::Usage,
                code::DENIED => ErrorCode::Denied,
                code::NOT_RUNNING => ErrorCode::Unavailable { host: OpHost::Window, queued: false },
                _ => ErrorCode::Failed,
            };
            OpReply::Failed(OpError::new(class, text.clone()))
        }
    }
}

impl Workspace {
    /// The host's calls, answered on the main thread: the runtime's `AppHost` sends them.
    pub fn brain_host_start(&mut self, window: &mut Window, cx: &mut Context<Self>) -> async_channel::Sender<HostCall> {
        let (tx, rx) = async_channel::unbounded::<HostCall>();
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(call) = rx.recv().await {
                if this.update_in(cx, |ws, window, cx| ws.brain_host_call(call, window, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        tx
    }

    /// The manager, as the brain runtime names it (its lane and wake): never the person, whatever
    /// channel it is handed (with v1's `Trust` gone, the channel says who it is, M56 row 7).
    pub(crate) fn brain_caller(&self, channel: caller::Caller) -> Caller {
        let channel = match channel {
            channel @ caller::Caller::Manager { .. } => channel,
            other => {
                log::warn!("manager: the brain's runtime named {other:?} as its caller; it is the manager's chat");
                caller::Caller::Manager { lane: caller::Lane::Chat, wake: None, project: None }
            }
        };
        Caller { who: self.state.manager.name.clone(), channel, session: None, session_title: None, agent: Some(self.state.manager.brain), cwd: None, project: None, drive: None }
    }

    fn brain_host_call(&mut self, call: HostCall, window: &mut Window, cx: &mut Context<Self>) {
        match call {
            // The manager, in no session, by the channel the runtime is (never the person); through
            // the gate, as the brain host is only made with the v2 engine.
            // What it did is an action line in the conversation, with Undo when the op has an
            // inverse (M52.2, ux-14).
            HostCall::RunOp { op, caller, reply } => {
                let who = self.brain_caller(caller.clone());
                let line = op.clone();
                self.control_gated(op, who, vec![Sink::Socket(reply)], String::new(), Some(Box::new(move |ws: &mut Workspace, answer: &Reply| ws.chat_action_line(&line, &caller, None, answer))), window, cx);
            }
            HostCall::Propose { op, caller, reason, reply } => {
                let who = self.brain_caller(caller);
                self.control_gated_as(op, who, vec![Sink::Socket(reply)], String::new(), None, crate::control_ui::Gated { act: Some(WakeAct::Propose(reason)), ..Default::default() }, window, cx);
            }
            HostCall::RunUnder { op, caller, playbook, reply } => {
                let who = self.brain_caller(caller.clone());
                let (line, by) = (op.clone(), playbook.clone());
                let done: Box<dyn FnOnce(&mut Workspace, &Reply)> = Box::new(move |ws, answer| ws.chat_action_line(&line, &caller, Some(&by), answer));
                self.control_gated_as(op, who, vec![Sink::Socket(reply)], String::new(), Some(done), crate::control_ui::Gated { act: Some(WakeAct::Under(playbook)), ..Default::default() }, window, cx);
            }
            // One notification a change of state (`runtime::health::change`); `matter` names it in
            // the log.
            HostCall::Notify { title, body, matter } => {
                log::info!("manager ({matter}): {body}");
                self.notify_manager(title, body, cx);
            }
            HostCall::Checked { lane, wake } => self.control_brain_checked(lane, wake),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glasscode_core::runtime::Host;

    #[test]
    fn replies_reach_the_runtime_as_op_replies() {
        assert!(matches!(op_reply(&Reply::ok("Done.", serde_json::Value::Null)), OpReply::Done { .. }));
        assert!(matches!(op_reply(&Reply::Waiting { approval: 3, summary: "x".into() }), OpReply::Waiting { pending, .. } if pending == "3"));
        assert!(matches!(op_reply(&Reply::fail(code::DENIED, "no")), OpReply::Failed(OpError { code: ErrorCode::Denied, .. })));
    }

    /// The host's calls go over the channel; with the app gone, an op says so at once.
    #[test]
    fn the_host_talks_to_the_main_thread() {
        let (tx, rx) = async_channel::unbounded();
        let host = AppHost::new(MachineId::parse("0123456789abcdef").unwrap(), tx);
        host.brain_checked(Lane::Chat, None);
        host.notify("Manager", "Signed out.", "health");
        assert!(matches!(rx.try_recv(), Ok(HostCall::Checked { lane: Lane::Chat, wake: None })));
        assert!(matches!(rx.try_recv(), Ok(HostCall::Notify { .. })));
        drop(rx);
        let op = glasscode_core::ops::typed("ui.state", serde_json::Value::Null).unwrap();
        assert!(matches!(host.run_op(&op, &caller::Caller::Manager { lane: Lane::Chat, wake: None, project: None }), OpReply::Failed(_)));
        assert!(host.has(OpHost::Window));
    }
}
