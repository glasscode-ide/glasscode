//! glasscoded's side of the brain runtime (M50; D18): `DaemonHost`, the `runtime::Host` of the
//! server, which the wake lane's worker runs against while the server is the manager's home
//! (M57, `crate::brain`). It has the server's own kinds of ops (Core, and SessionHost for its tmux
//! sessions) and never the window's. Every step checks that the server is still home at the
//! generation the worker started at: a wake that outlives its home does nothing more.
//!
//! - **Ops.** The Core reads that need no window run here (`ops::core_reads`: work, journal,
//!   memory), each within the projects the asking wake holds (trust-9): a project named outside
//!   them is refused, and memory is read for one of them. Every other op is the Mac's
//!   (`Unavailable`, not queued).
//! - **Cards.** A proposal is a pending action in the server's store, asked on the server; the
//!   sync shows it on the Mac as a card, and the person's answer comes back (no authority).
//! - **Notices** are kept, one a matter, until the next sync takes them to the Mac.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use glasscode_core::caller::{self, Lane};
use glasscode_core::control::{Reply, code};
use glasscode_core::ops::core_reads;
use glasscode_core::paths::ProjectKey;
use glasscode_core::ops::{ErrorCode, Op, OpError, OpHost, OpReply};
use glasscode_core::paths::MachineId;
use glasscode_core::remote::ServerNotice;

pub struct DaemonHost {
    machine: MachineId,
    /// The home generation the worker started at; none: not tied to home (tests).
    generation: Option<u64>,
    /// The projects each running wake holds (the worker's, by wake id), and the projects as the
    /// Mac's gate sees them (their keys and names).
    wakes: Arc<Mutex<HashMap<String, Vec<String>>>>,
    projects: Vec<glasscode_core::runtime::situation::Project>,
    notices: Mutex<Vec<ServerNotice>>,
    checked: Mutex<Vec<(Lane, Option<String>)>>,
}

impl DaemonHost {
    pub fn new(machine: MachineId) -> DaemonHost {
        DaemonHost { machine, generation: None, wakes: Arc::default(), projects: Vec::new(), notices: Mutex::new(Vec::new()), checked: Mutex::new(Vec::new()) }
    }

    /// A host for a worker started while the server is home at `generation`, sharing its wakes'
    /// projects.
    pub fn at_home(machine: MachineId, generation: u64, wakes: Arc<Mutex<HashMap<String, Vec<String>>>>, projects: Vec<glasscode_core::runtime::situation::Project>) -> DaemonHost {
        DaemonHost { generation: Some(generation), wakes, projects, ..DaemonHost::new(machine) }
    }

    /// The projects `caller` may reach: a wake's own (trust-9); nothing for anyone else here.
    fn held(&self, caller: &caller::Caller) -> Vec<ProjectKey> {
        let caller::Caller::Manager { lane: Lane::Wake, wake: Some(wake), .. } = caller else { return Vec::new() };
        self.wakes.lock().unwrap_or_else(|p| p.into_inner()).get(wake).map(|ids| ids.iter().filter_map(|id| ProjectKey::parse(id)).collect()).unwrap_or_default()
    }

    /// A project an op names (by its key or its name), when the caller holds it.
    fn named(&self, word: Option<&str>, held: &[ProjectKey]) -> Result<Option<ProjectKey>, Reply> {
        let Some(word) = word.map(str::trim).filter(|w| !w.is_empty()) else { return Ok(None) };
        let key = self.projects.iter().find(|p| p.id == word || p.name.eq_ignore_ascii_case(word)).and_then(|p| ProjectKey::parse(&p.id)).or_else(|| ProjectKey::parse(word));
        match key {
            Some(key) if held.contains(&key) => Ok(Some(key)),
            _ => Err(Reply::fail(code::DENIED, format!("“{word}” is not a project this wake is about."))),
        }
    }

    /// The Core reads this server answers itself; none for any other op.
    fn read(&self, op: &Op, caller: &caller::Caller) -> Option<Reply> {
        use glasscode_core::store::Store as _;
        if !matches!(op.name(), "work.list" | "work.show" | "journal.events" | "journal.query" | "memory.search" | "memory.show") {
            return None;
        }
        let held = self.held(caller);
        let answer = || -> Result<Reply, Reply> {
            let project = self.named(op.text("project"), &held)?;
            match op.name() {
                "work.list" | "work.show" => {
                    let ledger = glasscode_core::work::WorkLedger::load().map_err(|err| Reply::fail(code::FAILED, format!("The work ledger cannot be read here: {err}")))?;
                    let seen = |item: &glasscode_core::work::WorkItem| held.contains(&item.key.project);
                    if op.name() == "work.list" {
                        core_reads::work_list(&ledger, &seen, project.as_ref(), op.text("state"), op.on("all"))
                    } else {
                        let word = op.text("work").unwrap_or_default().trim();
                        core_reads::work_named(&ledger, &seen, word, None).map(|item| core_reads::work_show(&item))
                    }
                }
                "journal.events" | "journal.query" => {
                    let filter = core_reads::journal_filter(project, op.text("since"), op.text("session"), op.text("kind"))?;
                    let scope = glasscode_core::journal::Scope::Projects(held.clone());
                    let limit = op.number("limit").unwrap_or(50) as usize;
                    if op.name() == "journal.query" {
                        return Ok(core_reads::journal_query(&filter, &scope, limit));
                    }
                    let after = match op.text("after") {
                        Some(text) => Some(glasscode_core::journal::Cursor::parse(text).ok_or_else(|| Reply::fail(code::USAGE, format!("“{text}” is not a cursor of the journal.")))?),
                        None => None,
                    };
                    Ok(core_reads::journal_events(&filter, &scope, limit, after.as_ref(), false))
                }
                // Memory is read for one of the wake's projects: the one it names, or its only one.
                _ => {
                    let key = match (project, held.as_slice()) {
                        (Some(key), _) => key,
                        (None, [only]) => only.clone(),
                        _ => return Err(Reply::fail(code::USAGE, "Name the project to read the memory of (--project).")),
                    };
                    if op.name() == "memory.search" {
                        let limit = op.number("limit").unwrap_or(5).clamp(1, 50) as usize;
                        core_reads::memory_search(Some(&key), None, None, op.text("query").unwrap_or_default(), limit, op.on("history"))
                    } else {
                        core_reads::memory_show(Some(&key), op.text("what").unwrap_or_default().trim())
                    }
                }
            }
        };
        Some(answer().unwrap_or_else(|reply| reply))
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
        if let Some(reply) = self.read(op, _caller) {
            return op_reply(&reply);
        }
        // No handler of its own here for anything else: it is the Mac's.
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

/// A reply as the runtime reads it.
fn op_reply(reply: &Reply) -> OpReply {
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
        let host = DaemonHost::at_home(machine.clone(), generation, Default::default(), Vec::new());
        let number = host.propose(&op, "the release is tomorrow", Lane::Wake, Some("w1")).unwrap();
        let pending = glasscode_core::gate::Pending::load().unwrap();
        let card = pending.actions.iter().find(|a| a.number.to_string() == number).unwrap();
        assert_eq!((card.machine.clone(), card.reason.as_str()), (None, "the release is tomorrow"), "asked here");
        let past = DaemonHost::at_home(machine, generation + 1, Default::default(), Vec::new());
        assert!(past.propose(&op, "x", Lane::Wake, None).is_err());
        past.notify("Manager", "Ready.", "health");
        assert!(past.take_notices().is_empty());
        assert!(matches!(past.run_op(&op, &caller::Caller::Manager { lane: Lane::Wake, wake: None, project: None }), OpReply::Failed(OpError { code: ErrorCode::Failed, .. })));
    }

    /// The server answers the Core reads itself, within the projects the asking wake holds
    /// (trust-9): another project's work is not seen, and naming it is refused (M57).
    #[test]
    fn reads_here_reach_only_the_wakes_projects() {
        use glasscode_core::identity::{Hlc, WorkKey, WorkKind};
        use glasscode_core::runtime::situation::Project;
        use glasscode_core::work::{WorkItem, WorkLedger};
        let (_home, _scratch) = crate::daemon::tests::scratch_home();
        let machine = crate::owners::machine().unwrap();
        let generation = crate::home::server_generation().unwrap();
        let (app, web) = (ProjectKey::parse("app-3f9a1c").unwrap(), ProjectKey::parse("web-8b2d10").unwrap());
        let item = |project: &ProjectKey, id: &str| WorkItem::new(WorkKey { project: project.clone(), kind: WorkKind::Branch, id: id.into() }, Hlc::zero(machine.clone()));
        glasscode_core::store::update::<WorkLedger, ()>(|l| l.items = vec![item(&app, "feat/login"), item(&web, "feat/landing")]).unwrap();
        let wakes = Arc::new(Mutex::new(HashMap::from([("w1".to_string(), vec![app.to_string()])])));
        let projects = vec![Project { id: app.to_string(), name: "app".into(), ..Project::default() }, Project { id: web.to_string(), name: "web".into(), ..Project::default() }];
        let host = DaemonHost::at_home(machine, generation, wakes, projects);
        let wake = caller::Caller::Manager { lane: Lane::Wake, wake: Some("w1".into()), project: None };
        let list = glasscode_core::ops::typed("work.list", serde_json::json!({})).unwrap();
        let OpReply::Done { text, .. } = host.run_op(&list, &wake) else { panic!("work.list is answered here") };
        assert!(text.contains("feat/login") && !text.contains("feat/landing"), "{text}");
        let other = glasscode_core::ops::typed("work.list", serde_json::json!({ "project": "web" })).unwrap();
        assert!(matches!(host.run_op(&other, &wake), OpReply::Failed(OpError { code: ErrorCode::Denied, .. })));
        let chat = caller::Caller::Manager { lane: Lane::Chat, wake: None, project: None };
        let OpReply::Done { text, .. } = host.run_op(&list, &chat) else { panic!() };
        assert!(!text.contains("feat/"), "no wake, no projects: {text}");
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
