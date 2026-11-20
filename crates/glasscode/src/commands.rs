//! What the `glasscode` command's handlers (`ops_run.rs`) share: finding sessions, projects,
//! accounts and worktrees, what a session looks like to the command, the app's status, and starting
//! sessions (`session new`, `pr build`, `pr review`, `ticket start`). Each command does what the
//! matching button does; nothing here takes the keyboard or brings the window forward unless asked
//! (`--open`).

use std::path::{Path, PathBuf};

use gpui::{AppContext as _, Context, Window};
use glasscode_core::control::{Kind, Reply, code};
use glasscode_core::day;
use glasscode_core::git::{self, Worktree};
use glasscode_core::github;
use glasscode_core::linear;
use glasscode_core::model::{AgentKind, Session, new_id, now};
use glasscode_core::ops::{self, Op};
use glasscode_core::project_settings;
use serde_json::{Value, json};

use crate::control_ui::{Caller, Responder, Trust};
use crate::ops_run::{Planned, error_reply, failed, plan, usage};
use crate::workspace::Workspace;

/// Where `session move` sends a session.
#[derive(Debug, PartialEq)]
pub(crate) enum MoveTo {
    Server,
    ThisComputer,
}

pub(crate) fn short(id: &str) -> &str {
    &id[..id.len().min(8)]
}

pub(crate) fn home_relative(path: &Path) -> String {
    let text = path.display().to_string();
    let home = glasscode_core::paths::home().display().to_string();
    match text.strip_prefix(&home) {
        Some(rest) => format!("~{rest}"),
        None => text,
    }
}

pub(crate) fn expand(path: &str, base: Option<&Path>) -> PathBuf {
    if path == "~" {
        return glasscode_core::paths::home();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return glasscode_core::paths::home().join(rest);
    }
    let p = PathBuf::from(path);
    // A relative path is from where the command was run (the agent's shell), not the app's folder.
    match base {
        Some(base) if p.is_relative() => base.join(p),
        _ => p,
    }
}

pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

pub(crate) fn agent_of(word: &str) -> Result<AgentKind, Reply> {
    match word.to_lowercase().as_str() {
        "claude" | "claude-code" | "cc" => Ok(AgentKind::Claude),
        "codex" => Ok(AgentKind::Codex),
        "shell" | "terminal" | "zsh" | "bash" => Ok(AgentKind::Shell),
        other => Err(usage(format!("--agent is claude, codex or shell (not {other})."))),
    }
}

impl Workspace {
    // ----- finding things ------------------------------------------------------------------

    /// A session by its id, its start, or words from its title (`ops::find_session`: only the
    /// person names one by words with the v2 engine).
    pub fn find_session(&self, word: &str, caller: &Caller) -> Result<String, Reply> {
        ops::find_session(word, &self.parse_ctx(caller)).map_err(|e| error_reply(&e))
    }

    /// A project by its id, name, folder or the start of its name, for the app itself.
    pub fn find_project(&self, word: &str) -> Result<String, Reply> {
        self.find_project_as(&Caller::you(), word)
    }

    /// A project the caller names (`ops::find_project`).
    pub(crate) fn find_project_as(&self, caller: &Caller, word: &str) -> Result<String, Reply> {
        ops::find_project(&self.world(caller), word).map_err(|e| error_reply(&e))
    }

    /// `--project`, else the calling session's project, else the one the command runs in, else
    /// the only one (`ops::project_for`).
    pub(crate) fn project_for(&self, caller: &Caller, given: Option<&str>) -> Result<String, Reply> {
        ops::project_for(&self.world(caller), given).map_err(|e| error_reply(&e))
    }

    pub(crate) fn find_account(&self, agent: AgentKind, word: Option<&str>) -> Result<Option<String>, Reply> {
        if !agent.uses_accounts() {
            return Ok(None);
        }
        match word {
            None => self.state.default_account(agent).map(|a| Some(a.id.clone())).ok_or_else(|| failed(format!("No {} account yet: add one in Settings › Agents and accounts.", agent.name()))),
            Some(w) => self
                .state
                .accounts_for(agent)
                .find(|a| a.id == w || a.label.eq_ignore_ascii_case(w) || a.display().eq_ignore_ascii_case(w))
                .map(|a| Some(a.id.clone()))
                .ok_or_else(|| failed(format!("No {} account “{w}”. Accounts: {}.", agent.name(), self.state.accounts_for(agent).map(|a| a.label.clone()).collect::<Vec<_>>().join(", ")))),
        }
    }

    /// A worktree by its folder, branch, or the end of its path (`ops::find_worktree`), with its
    /// project.
    pub(crate) fn find_worktree_as(&self, caller: &Caller, word: &str, project: Option<&str>) -> Result<(String, Worktree), Reply> {
        let world = self.world(caller);
        let found = ops::find_worktree(&world, word, project).map_err(|e| error_reply(&e))?;
        let wt = self.worktrees.get(&found.project).and_then(|list| list.iter().find(|w| w.path == found.path)).cloned().ok_or_else(|| failed("That worktree is gone."))?;
        Ok((found.project.clone(), wt))
    }

    pub(crate) fn session_state(&self, s: &Session) -> &'static str {
        if !self.terminals.contains_key(&s.id) {
            return "not running";
        }
        if !self.control.pending_for(&s.id).is_empty() {
            return "needs you";
        }
        match self.row_state(s) {
            crate::sidebar::RowState::NeedsYou => "needs you",
            crate::sidebar::RowState::Working => "working",
            _ => "idle",
        }
    }

    pub(crate) fn session_json(&self, s: &Session) -> Value {
        let pr = self.prs.map.get(&s.id).and_then(|p| p.pr.as_ref()).map(|p| json!({ "number": p.number, "title": p.title, "url": p.url }));
        json!({
            "id": s.id,
            "title": self.session_title(s),
            "agent": s.agent.name().to_lowercase(),
            "account": s.account_id.as_deref().and_then(|a| self.state.account(a)).map(|a| a.label.clone()),
            "project": self.state.project(&s.project_id).map(|p| p.name.clone()),
            "worktree": s.worktree,
            "branch": s.branch,
            "state": self.session_state(s),
            "pull_request": pr,
            "ticket": self.state.ticket_of.get(&s.id),
            "review": s.review.as_ref().map(|r| json!({ "repo": r.repo, "number": r.number })),
            "model": s.model,
            "mode": s.mode,
            "on_server": false,
        })
    }


    pub(crate) fn status_reply(&self) -> Reply {
        // Counts exclude hidden projects' sessions, so the CLI and Glasscode see the same set.
        let visible = || self.state.sessions.iter().filter(|s| !self.state.session_hidden(s));
        let total = visible().count();
        let running = visible().filter(|s| self.terminals.contains_key(&s.id)).count();
        let working = visible().filter(|s| self.session_state(s) == "working").count();
        let needs = visible().filter(|s| self.session_state(s) == "needs you").count();
        let view = self.today_view();
        let server = match (&self.server.config, self.server.connected()) {
            (None, _) => "no server".to_string(),
            (Some(_), true) => format!("{} connected, {} there", self.server.name(), plural(self.server.state.as_ref().map(|s| s.sessions.len()).unwrap_or(0), "session", "sessions")),
            (Some(_), false) => format!("{} not connected", self.server.name()),
        };
        let build = if glasscode_core::update::BUILD == 0 { "development build".to_string() } else { format!("build {}", glasscode_core::update::BUILD) };
        let text = format!(
            "Glasscode is running ({build}).\nSessions: {} on {}, {running} running ({working} working, {needs} need you). Server: {server}.\nToday: {}\nWaiting for the person: {}.",
            total,
            crate::platform::this_computer(),
            view.summary(needs),
            plural(self.control.approvals.len(), "request", "requests"),
        );
        Reply::ok(
            text,
            json!({
                "build": glasscode_core::update::BUILD,
                "sessions": total,
                "running": running,
                "working": working,
                "needs_you": needs,
                "server": server,
                "today": { "due": view.due, "asked": view.asked, "open": view.open, "done": view.done },
                "approvals": self.control.approvals.len(),
            }),
        )
    }

    pub(crate) fn whoami_reply(&self, caller: &Caller) -> Reply {
        let rules: Vec<(Kind, glasscode_core::control::Rule)> = Kind::ALL.iter().map(|k| (*k, self.control_rule(caller, *k))).collect();
        let lines: Vec<String> = rules
            .iter()
            .map(|(k, r)| format!("  {:<6} {}", match r { glasscode_core::control::Rule::Allow => "yes", glasscode_core::control::Rule::Ask => "asks", glasscode_core::control::Rule::Never => "never" }, k.label()))
            .collect();
        let mut who = match caller.trust {
            Trust::You => "You (at a terminal): everything is allowed.".to_string(),
            Trust::Session => format!("{} (session {}).", caller.describe(), caller.session.as_deref().map(short).unwrap_or_default()),
            Trust::Outside => "A program Glasscode did not start.".to_string(),
        };
        // The person by v1's rule only: the command says it was typed at a terminal, which any
        // program can say (owner decision O13; with the engine at v2 it may only read).
        let taken_at_its_word = caller.trust == Trust::You && !caller.channel.is_person();
        if taken_at_its_word {
            who.push_str("\nWarning: Glasscode takes this for you because the command says it was typed at a terminal, and any program can say that. Once this Mac runs Manager v2, commands from terminals outside Glasscode may only read; anything else waits for your OK in the app. Commands you type in a Glasscode shell tab keep full rights.");
        }
        Reply::ok(
            format!("{who}\nWhat it may do:\n{}", lines.join("\n")),
            json!({
                "who": caller.who,
                "session": caller.session,
                "session_title": caller.session_title,
                "trust": match caller.trust { Trust::You => "you", Trust::Session => "session", Trust::Outside => "outside" },
                "caller": caller.channel,
                "taken_at_its_word": taken_at_its_word,
                "rules": rules.iter().map(|(k, r)| (k.key().to_string(), json!(r))).collect::<serde_json::Map<_, _>>(),
            }),
        )
    }
}

/// One line a record, newest first: when, where it came from, its session, and its text.
pub(crate) fn words_reply(mode: glasscode_core::words::Mode, records: &[glasscode_core::words::WordsRecord]) -> Reply {
    use glasscode_core::words::Mode;
    let state = match mode {
        Mode::Measure => "The words ledger only counts (GLASSCODE_WORDS_MEASURE=1): no text is kept.",
        Mode::Ledger => "",
    };
    let lines: Vec<String> = records
        .iter()
        .map(|record| {
            let session = record.session.as_deref().map(|s| format!(" in {}", short(s))).unwrap_or_default();
            let text = if record.measured { format!("({} bytes, not kept)", record.spans.last().map_or(0, |span| span.1)) } else { record.text.chars().take(80).collect::<String>().replace('\n', " ") };
            format!("{}  {} ({:?}){session}: {text}", clock(record.at.wall_ms / 1000), record.origin, record.tier())
        })
        .collect();
    let text = match (state.is_empty(), lines.is_empty()) {
        (_, false) => [state, &lines.join("\n")].iter().filter(|part| !part.is_empty()).copied().collect::<Vec<_>>().join("\n"),
        (true, true) => "Nothing yet.".to_string(),
        (false, true) => state.to_string(),
    };
    Reply::ok(text, serde_json::to_value(records).unwrap_or_default())
}

pub(crate) fn clock(unix: u64) -> String {
    day::clock(unix as i64)
}

pub fn clock_of(unix: u64) -> String {
    clock(unix)
}

/// What `session new` (and `pr build`, `pr review`, `ticket start`) is asked to start.
pub struct NewSpec {
    pub project_id: String,
    pub agent: AgentKind,
    pub account_id: Option<String>,
    pub title: Option<String>,
    pub branch: Option<String>,
    pub worktree: Option<PathBuf>,
    pub from: Option<String>,
    pub ticket: Option<linear::Ticket>,
    pub pr: Option<u64>,
    pub review: Option<u64>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub message: Option<String>,
    /// The person typed `message` in one of the app's composers: its origin, and what of it they
    /// did not type (for the words ledger). None: Glasscode or a caller made it.
    pub message_origin: Option<(glasscode_core::words::Origin, Vec<std::ops::Range<usize>>)>,
    pub open: bool,
    pub on_server: bool,
    pub started_by: String,
}

impl Workspace {
    /// What `session new`, `pr build`, `pr review` and `ticket start` are asked to start, from
    /// the op's words.
    pub(crate) fn new_spec(&self, caller: &Caller, op: &Op) -> Result<NewSpec, Reply> {
        let project_id = self.project_for(caller, op.text("project"))?;
        // None chosen: the project's `default_agent`, else Claude, as before (also for a ticket:
        // Linear's agent is the Board's choice, not this command's).
        let agent = match op.text("agent") {
            Some(a) => agent_of(a)?,
            None => self.default_agent(&project_id, false).0,
        };
        let account_id = self.find_account(agent, op.text("account"))?;
        let worktree = match op.text("worktree") {
            Some(w) => Some(self.find_worktree_as(caller, w, Some(&project_id))?.1.path),
            None => None,
        };
        let ticket = match op.text("ticket") {
            Some(t) => Some(self.tickets.ticket(&t.to_uppercase()).cloned().ok_or_else(|| failed(format!("No ticket {t} among yours in Linear (Tickets shows the ones Glasscode knows).")))?),
            None => None,
        };
        let (pr, review) = (op.number("pr"), op.number("review"));
        let choices = [worktree.is_some(), op.text("branch").is_some(), pr.is_some(), review.is_some()].iter().filter(|b| **b).count();
        if choices > 1 {
            return Err(usage("Pick one of --worktree, --branch, --pr and --review."));
        }
        if let Some(mode) = op.text("mode") {
            let ok = match agent {
                AgentKind::Claude => glasscode_core::agents::claude_mode(mode).is_some(),
                AgentKind::Codex => matches!(mode, "full-auto" | "auto" | "read-only" | "default"),
                AgentKind::Shell => false,
            };
            if !ok {
                return Err(usage(format!("--mode for {} is {} (not {mode}).", agent.name(), if agent == AgentKind::Codex { "full-auto, read-only or default" } else { "default, auto, plan, accept-edits or bypass" })));
            }
            // A session that never asks the person is not something another agent may start.
            let bypasses = glasscode_core::agents::claude_mode(mode) == Some("bypassPermissions");
            if bypasses && caller.trust != Trust::You {
                return Err(Reply::fail(code::DENIED, "Only you, at a terminal, can start a session in bypass mode. Start it yourself, or choose auto, plan or accept-edits."));
            }
        }
        // Whether the branch exists already is git's to say, off the main thread: the handler's
        // plan checks it (`ops_run::new_branch_free`) before anyone is asked.
        let branch = op.text("branch").map(|b| b.trim().replace(' ', "-"));
        Ok(NewSpec {
            project_id,
            agent,
            account_id,
            title: op.text("title").map(str::to_string),
            branch,
            worktree,
            from: op.text("from").map(str::to_string),
            ticket,
            pr,
            review,
            model: op.text("model").map(str::to_string),
            mode: op.text("mode").map(|m| match agent {
                AgentKind::Claude => glasscode_core::agents::claude_mode(m).unwrap_or("default").to_string(),
                _ => m.to_string(),
            }),
            message: op.text("message").map(str::to_string).filter(|m| !m.trim().is_empty()),
            message_origin: None,
            open: op.on("open"),
            on_server: op.on("on_server"),
            started_by: caller.describe(),
        })
    }


    pub(crate) fn new_session_plan(&mut self, spec: NewSpec) -> Planned {
        let project = self.state.project(&spec.project_id).map(|p| p.name.clone()).unwrap_or_default();
        let agent = spec.agent.name();
        let what = match (&spec.review, &spec.pr, &spec.ticket, &spec.worktree, &spec.branch) {
            (Some(n), ..) => format!("start a {agent} review of #{n} in {project}"),
            (_, Some(n), ..) => format!("start a {agent} session on pull request #{n} in {project}"),
            (_, _, Some(t), ..) => format!("start a {agent} session for {} in {project}", t.identifier),
            (_, _, _, Some(w), _) => format!("start a {agent} session in {}", home_relative(w)),
            (_, _, _, _, Some(b)) => format!("start a {agent} session in {project} on a new branch {b}"),
            _ => format!("start a {agent} session in {project}"),
        };
        let what = if spec.on_server { format!("{what} on {}", self.server.name()) } else { what };
        plan(Kind::Start, what, move |ws, r, window, cx| ws.start_session(spec, r, window, cx))
    }

    pub fn start_session(&mut self, spec: NewSpec, r: Responder, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.state.project(&spec.project_id).cloned() else {
            self.reply(r, failed("That project is gone."));
            return;
        };
        if spec.on_server {
            self.start_server_session(spec, r, window, cx);
            return;
        }
        let root = project.root.clone();
        let name = project.name.clone();
        let env = self.env.clone();
        let fetch = !self.state.prefs.fresh_main_off;
        let plain = !root.join(".git").exists();
        let (pr, ticket, worktree, branch, from) = (spec.pr, spec.ticket.clone(), spec.worktree.clone(), spec.branch.clone(), spec.from.clone());
        // A title names a branch from the project's template (the ticket's title comes first).
        let title = spec.title.clone().or_else(|| spec.message.as_deref().map(|m| linear::short_title(m, 46)));
        let known_worktrees: Vec<Worktree> = self.worktrees.get(&project.id).cloned().unwrap_or_default();
        let file_root = root.clone();
        let task = cx.background_spawn(async move {
            let repo = || github::repo_slug(&root).ok_or_else(|| anyhow::anyhow!("this project's origin is not on GitHub"));
            let find_pr = |n: u64| -> anyhow::Result<github::PrHit> {
                github::search_prs(&repo()?, &n.to_string(), &env)?.into_iter().find(|h| h.number == n).ok_or_else(|| anyhow::anyhow!("GitHub has no pull request #{n} in {}", repo().unwrap_or_default()))
            };
            let no_review: Option<glasscode_core::model::ReviewRef> = None;
            if let Some(n) = pr {
                let hit = find_pr(n)?;
                let checkout = git::checkout_pr_branch(&root, &name, hit.number, &hit.head, hit.same_repo)?;
                return Ok::<_, anyhow::Error>((checkout.path, checkout.branch, Some(checkout.note), no_review, Some(hit), None, None));
            }
            if let Some(path) = worktree {
                let branch = git::current_branch(&path);
                return Ok((path, branch, None, None, None, None, None));
            }
            if plain {
                return Ok((root.clone(), String::new(), None, None, None, None, None));
            }
            // The project's own file, read once: the branch's name, the worktree's folder and
            // the setup all come from this one read.
            let file = project_settings::load(&root);
            // A ticket: its branch (an existing worktree on it, else a new one), and its text.
            let detail = ticket.as_ref().and_then(|t| linear::Client::saved().and_then(|c| c.detail(&t.id).ok()));
            let ticket_branch = ticket.as_ref().map(|t| linear::branch_for(&t.identifier, &t.title));
            // A branch the person named stays as it is; else the template, when it can tell this
            // work apart from other work.
            let templated = match (&branch, file.settings.branch_template.as_deref()) {
                (None, Some(template)) => project_settings::branch_name(
                    template,
                    &project_settings::BranchFor {
                        project_name: &name,
                        default: ticket_branch.as_deref().unwrap_or_default(),
                        issue: ticket.as_ref().map(|t| t.identifier.as_str()),
                        title: ticket.as_ref().map(|t| t.title.as_str()).or(title.as_deref()),
                        labels: ticket.as_ref().map(|t| t.labels.as_slice()).unwrap_or_default(),
                    },
                ),
                _ => None,
            };
            // The worktree there is already for this branch, or this ticket (by the template's
            // name, or by the name it had before the template). A title's name is not looked for:
            // other work can have the same words.
            let reuse: Vec<&String> = match (&branch, &ticket) {
                (Some(b), _) => vec![b],
                (None, Some(_)) => templated.iter().chain(ticket_branch.iter()).collect(),
                (None, None) => Vec::new(),
            };
            if let Some(existing) = known_worktrees.iter().find(|w| reuse.contains(&&w.branch)) {
                return Ok((existing.path.clone(), existing.branch.clone(), None, None, None, detail, Some(file)));
            }
            let wanted = match (branch, templated) {
                (Some(b), _) => b,
                // A title's name may be taken by other work: the next free one.
                (None, Some(t)) if ticket.is_none() => std::iter::once(t.clone()).chain((2..10).map(|n| format!("{t}-{n}"))).find(|b| !git::branch_exists(&root, b)).unwrap_or(t),
                (None, Some(t)) => t,
                (None, None) => ticket_branch.unwrap_or_else(|| format!("glasscode/session-{}", &new_id()[..4])),
            };
            let base = match from {
                Some(b) => git::FreshBase { base: b, ..git::start_base(&root, false) },
                None => git::start_base(&root, fetch),
            };
            let note = (fetch && !base.fetched).then(|| format!("Could not fetch origin (offline?); the branch starts from {} as it is on {}.", base.base, crate::platform::this_computer()));
            let path = git::add_worktree_with(&root, &name, &wanted, &base.base, file.clone())?;
            Ok((path, wanted, note, None, None, detail, Some(file)))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |ws, window, cx| match result {
                Ok((worktree, branch, note, review, hit, detail, file)) => {
                    if let Some(file) = file {
                        ws.project_file_read(&file_root, file, cx);
                    }
                    let agent = spec.agent;
                    let first = match (&spec.ticket, &spec.message) {
                        (Some(t), message) => Some(linear::first_message(message.as_deref().unwrap_or("Work on this ticket. Start by reading the code it is about, then make a short plan and tell me before you change much."), t, detail.as_ref())),
                        (None, message) => message.clone(),
                    };
                    let title = match (&spec.title, &review, &hit, &spec.ticket, &spec.message) {
                        (Some(t), ..) => t.clone(),
                        (None, Some(r), ..) => format!("#{} {}", r.number, r.title.chars().take(56).collect::<String>()),
                        (None, None, Some(h), ..) => h.title.chars().take(60).collect(),
                        (None, None, None, Some(t), _) => linear::short_title(&t.title, 46),
                        (None, None, None, None, Some(m)) => linear::short_title(m, 46),
                        _ if branch.is_empty() => format!("{} · {}", agent.name(), worktree.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
                        _ => format!("{} · {branch}", agent.name()),
                    };
                    let id = if spec.title.is_some() { ws.claim_custom_title(new_id()) } else { new_id() };
                    let session = Session {
                        id: id.clone(),
                        project_id: spec.project_id.clone(),
                        worktree: worktree.clone(),
                        branch: branch.clone(),
                        title: title.clone(),
                        agent,
                        account_id: spec.account_id.clone(),
                        agent_session_id: Workspace::new_session_id_for(agent),
                        transcript_path: None,
                        lineage: vec![ws.state.lineage_tag(agent, spec.account_id.as_deref())],
                        initial_prompt: first,
                        team_task: None,
                        review: review.clone(),
                        model: spec.model.clone(),
                        mode: spec.mode.clone(),
                        created_at: now(),
                    };
                    if let Some(t) = &spec.ticket {
                        ws.state.ticket_of.insert(id.clone(), t.identifier.clone());
                    }
                    // Started to carry on with a pull request: its work is that pull request's (M32).
                    if let (None, Some(hit)) = (&review, &hit)
                        && let Some(repo) = github::repo_of_pr_url(&hit.url)
                    {
                        ws.state.pr_of.insert(id.clone(), glasscode_core::model::pr_ref(&repo, hit.number));
                    }
                    // The person's own first message (a ticket's wraps it in Glasscode's words).
                    if let (None, Some(origin)) = (&spec.ticket, spec.message_origin.clone()) {
                        ws.control.first_words.insert(id.clone(), origin);
                    }
                    ws.state.sessions.push(session.clone());
                    // Who started it decides its profile from its first start (M22): before the
                    // terminal starts, never after the reply.
                    ws.control_mark_driven(&r.caller, std::slice::from_ref(&id));
                    glasscode_core::activity::record_session();
                    if spec.open {
                        ws.center = crate::center::Center::Session;
                        ws.open_session(&id, window, cx);
                    } else {
                        // In the background: it starts working, the person carries on.
                        ws.ensure_terminal(&id, window, cx);
                        if !spec.started_by.starts_with("You") {
                            ws.show_toast(format!("{} started “{title}”.", spec.started_by), cx);
                        }
                    }
                    ws.save();
                    ws.refresh_worktrees(cx);
                    let project = ws.state.project(&spec.project_id).map(|p| p.name.clone()).unwrap_or_default();
                    let mut text = format!(
                        "Started “{title}” ({}): {} in {project}{}, {}.",
                        short(&id),
                        agent.name(),
                        if branch.is_empty() { String::new() } else { format!(" on {branch}") },
                        home_relative(&worktree)
                    );
                    if !spec.open {
                        text.push_str(&format!(" It runs in the background; `glasscode session open {}` shows it.", short(&id)));
                    }
                    if let Some(note) = note {
                        text.push_str(&format!("\n{note}"));
                    }
                    let data = ws.session_json(&session);
                    ws.reply(r, Reply::ok(text, data));
                    cx.notify();
                }
                Err(e) => ws.reply(r, failed(format!("Could not start the session: {e:#}"))),
            });
        })
        .detach();
    }

    fn start_server_session(&mut self, spec: NewSpec, r: Responder, window: &mut Window, cx: &mut Context<Self>) {
        if !self.server.connected() {
            self.reply(r, failed("The server is not connected."));
            return;
        }
        let Some(server_project) = self.server.state.as_ref().and_then(|s| {
            let mac = self.state.project(&spec.project_id)?;
            s.projects.iter().find(|p| p.name == mac.name || p.id == mac.id).map(|p| p.id.clone())
        }) else {
            self.reply(r, failed("The server has no copy of this project (Settings › Server and devices › Server › Copy to server)."));
            return;
        };
        if spec.review.is_some() || spec.ticket.is_some() || spec.worktree.is_some() {
            self.reply(r, failed("On the server, `session new` takes --branch, --pr, --message, --agent, --account and --title for now."));
            return;
        }
        // A pull request is GitHub's to find: off the main thread, then the session starts.
        if let Some(n) = spec.pr {
            let root = self.state.project(&spec.project_id).map(|p| p.root.clone()).unwrap_or_default();
            let env = self.env.clone();
            let task = cx.background_spawn(async move {
                let repo = github::repo_slug(&root).unwrap_or_default();
                github::search_prs(&repo, &n.to_string(), &env).ok().and_then(|h| h.into_iter().find(|h| h.number == n))
            });
            cx.spawn_in(window, async move |this, cx| {
                let hit = task.await;
                let _ = this.update_in(cx, |ws, window, cx| match hit {
                    Some(hit) => ws.start_server_session_from(spec, server_project, Some(glasscode_core::remote::StartFrom::PullRequest { number: n, head: hit.head, same_repo: hit.same_repo }), r, window, cx),
                    None => ws.reply(r, failed(format!("GitHub has no pull request #{n} here."))),
                });
            })
            .detach();
            return;
        }
        self.start_server_session_from(spec, server_project, Some(glasscode_core::remote::StartFrom::Main), r, window, cx);
    }

    /// The rest of [`Workspace::start_server_session`], once where the session starts from is known.
    fn start_server_session_from(&mut self, spec: NewSpec, server_project: String, start: Option<glasscode_core::remote::StartFrom>, r: Responder, window: &mut Window, cx: &mut Context<Self>) {
        let new_branch = spec.branch.clone().or_else(|| spec.pr.is_none().then(|| format!("glasscode/session-{}", &new_id()[..4])));
        let create = glasscode_core::remote::CreateSession {
            project_id: server_project,
            worktree: None,
            new_branch: if spec.pr.is_some() { None } else { new_branch },
            agent: spec.agent,
            account_id: spec.account_id.clone(),
            title: spec.title.clone().or_else(|| spec.message.as_deref().map(|m| linear::short_title(m, 46))),
            resume: None,
            initial_prompt: spec.message.clone(),
            start,
            review: None,
            review_files: Default::default(),
            model: spec.model.clone(),
            mode: spec.mode.clone(),
            held: None,
        };
        let server = self.server.name();
        self.create_server_session(create, window, cx);
        self.reply(r, Reply::ok(format!("Starting it on {server}."), Value::Null));
    }
}

pub(crate) fn pr_status_reply(d: &github::PrDetails) -> Reply {
    use github::CheckState;
    let failed_checks: Vec<&str> = d.checks.iter().filter(|c| c.state() == CheckState::Failed).map(|c| c.name.as_str()).collect();
    let running = d.checks.iter().filter(|c| c.state() == CheckState::Running).count();
    let passed = d.checks.iter().filter(|c| c.state() == CheckState::Passed).count();
    let ci = if !failed_checks.is_empty() {
        format!("{} failing ({}), {passed} passed", failed_checks.len(), failed_checks.join(", "))
    } else if running > 0 {
        format!("{running} running, {passed} passed")
    } else if passed > 0 {
        format!("all {passed} passed")
    } else {
        "no checks".into()
    };
    let approved: Vec<&str> = d.reviewers.iter().filter(|r| r.state == "APPROVED").map(|r| r.login.as_str()).collect();
    let changes: Vec<&str> = d.reviewers.iter().filter(|r| r.state == "CHANGES_REQUESTED").map(|r| r.login.as_str()).collect();
    let waiting: Vec<&str> = d.reviewers.iter().filter(|r| r.state == "REQUESTED").map(|r| r.login.as_str()).collect();
    let mut reviews = Vec::new();
    if !approved.is_empty() {
        reviews.push(format!("approved by {}", approved.join(", ")));
    }
    if !changes.is_empty() {
        reviews.push(format!("changes requested by {}", changes.join(", ")));
    }
    if !waiting.is_empty() {
        reviews.push(format!("waiting for {}", waiting.join(", ")));
    }
    let conflicts = match d.mergeable.as_str() {
        "CONFLICTING" => "conflicts with its base",
        "MERGEABLE" => "no conflicts",
        _ => "GitHub is still checking for conflicts",
    };
    let verdict = d.verdict.as_ref().map(|v| format!("{} (review v{})", v.verdict, v.version)).unwrap_or_else(|| "not reviewed".into());
    let preview = match (&d.preview.state, &d.preview.url) {
        (Some(s), Some(u)) => format!("{s} at {u}"),
        (Some(s), None) => s.clone(),
        _ if d.preview.available => "none yet".into(),
        _ => "not used here".into(),
    };
    let text = format!(
        "#{} {} ({}{}, by {})\nCI: {ci}\nReviews: {}\nMerge: {conflicts}\nPreview: {preview}\nReview: {verdict}\n{}",
        d.number,
        d.title,
        d.state.to_lowercase(),
        if d.is_draft { ", draft" } else { "" },
        d.author,
        if reviews.is_empty() { "none yet".to_string() } else { reviews.join("; ") },
        d.url
    );
    Reply::ok(
        text,
        json!({
            "number": d.number, "title": d.title, "url": d.url, "state": d.state, "draft": d.is_draft, "author": d.author,
            "head": d.head, "base": d.base, "mergeable": d.mergeable, "review_decision": d.review_decision,
            "checks": { "failed": failed_checks, "running": running, "passed": passed },
            "approved_by": approved, "changes_requested_by": changes, "waiting_for": waiting,
            "review": d.verdict.as_ref().map(|v| json!({ "verdict": v.verdict, "version": v.version, "approved": v.approved })),
            "preview": { "state": d.preview.state, "url": d.preview.url },
        }),
    )
}
