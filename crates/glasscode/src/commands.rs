//! The `glasscode` command's commands: what each one means, what kind of action it is (for
//! Settings › Automation), and what it does. Each command does what the matching button does;
//! nothing here takes the keyboard or brings the window forward unless asked (`--open`).

use std::path::{Path, PathBuf};

use gpui::{AppContext as _, Context, Window};
use glasscode_core::board::{self, Draft, List, Section};
use glasscode_core::control::{Args, Kind, Reply, code};
use glasscode_core::day;
use glasscode_core::git::{self, Worktree};
use glasscode_core::github;
use glasscode_core::linear;
use glasscode_core::agents::AgentActivity;
use glasscode_core::model::{AgentKind, Project, Session, new_id, now};
use serde_json::{Value, json};

use crate::control_ui::{Caller, Plan, Responder, Trust};
use crate::workspace::Workspace;

type Planned = Result<Plan, Reply>;

fn usage(text: impl Into<String>) -> Reply {
    Reply::fail(code::USAGE, text)
}

fn failed(text: impl Into<String>) -> Reply {
    Reply::fail(code::FAILED, text)
}

fn plan(kind: Kind, what: impl Into<String>, run: impl FnOnce(&mut Workspace, Responder, &mut Window, &mut Context<Workspace>) + 'static) -> Planned {
    Ok(Plan { kind, what: what.into(), run: Box::new(run) })
}

/// A command that only reads: its answer right away.
fn answer(reply: Reply) -> Planned {
    plan(Kind::Read, "read", move |ws, r, _, _| ws.reply(r, reply))
}

fn check(args: &Args, known: &[&str]) -> Result<(), Reply> {
    let unknown = args.unknown(known);
    if unknown.is_empty() {
        return Ok(());
    }
    let known: Vec<String> = known.iter().map(|k| format!("--{k}")).collect();
    Err(usage(format!("Unknown option {}. This command takes: {}.", unknown.join(", "), if known.is_empty() { "no options".into() } else { known.join(", ") })))
}

/// Where `session move` sends a session.
#[derive(Debug, PartialEq)]
enum MoveTo {
    Server,
    ThisComputer,
}

/// `--to-pc` is the same as `--to-mac`, for people on Windows.
fn move_to(args: &Args) -> Result<MoveTo, Reply> {
    check(args, &["to-server", "to-mac", "to-pc"])?;
    Ok(if args.has("to-mac") || args.has("to-pc") { MoveTo::ThisComputer } else { MoveTo::Server })
}

fn short(id: &str) -> &str {
    &id[..id.len().min(8)]
}

fn home_relative(path: &Path) -> String {
    let text = path.display().to_string();
    let home = glasscode_core::paths::home().display().to_string();
    match text.strip_prefix(&home) {
        Some(rest) => format!("~{rest}"),
        None => text,
    }
}

fn expand(path: &str, base: Option<&Path>) -> PathBuf {
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

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn agent_of(word: &str) -> Result<AgentKind, Reply> {
    match word.to_lowercase().as_str() {
        "claude" | "claude-code" | "cc" => Ok(AgentKind::Claude),
        "codex" => Ok(AgentKind::Codex),
        "shell" | "terminal" | "zsh" | "bash" => Ok(AgentKind::Shell),
        other => Err(usage(format!("--agent is claude, codex or shell (not {other})."))),
    }
}

fn pr_number(word: &str) -> Result<u64, Reply> {
    let w = word.trim().trim_start_matches('#');
    let w = w.rsplit('/').next().unwrap_or(w);
    w.parse().map_err(|_| usage(format!("“{word}” is not a pull request number (958, #958 or its URL).")))
}

/// A day from the command line ("fri", "tomorrow", "2026-11-03"), or `None` for "none".
fn when(word: &str) -> Result<Option<i64>, Reply> {
    if matches!(word.to_lowercase().as_str(), "none" | "no" | "never" | "-") {
        return Ok(None);
    }
    day::parse_when(word, day::today()).map(Some).ok_or_else(|| usage(format!("“{word}” is not a day: today, tomorrow, fri, next monday, in 3 days, 2026-11-03 or 30.10.")))
}

impl Workspace {
    // ----- finding things ------------------------------------------------------------------

    pub fn find_session(&self, word: &str, caller: &Caller) -> Result<String, Reply> {
        let w = word.trim();
        if matches!(w, "this" | "self" | "." | "me") {
            return caller.session.clone().ok_or_else(|| usage("“this” is the session the command runs in, and this command does not run in a Glasscode session."));
        }
        // A hidden project's sessions are unreachable by id or name (the caller's own "this"
        // session is handled above, so a hidden session can still refer to itself).
        if let Some(s) = self.state.sessions.iter().find(|s| s.id == w && !self.state.session_hidden(s)) {
            return Ok(s.id.clone());
        }
        let starts: Vec<&Session> = self.state.sessions.iter().filter(|s| w.len() >= 4 && s.id.starts_with(w) && !self.state.session_hidden(s)).collect();
        if let [one] = starts.as_slice() {
            return Ok(one.id.clone());
        }
        let words: Vec<String> = w.to_lowercase().split_whitespace().map(str::to_string).collect();
        let hits: Vec<&Session> = self
            .state
            .sessions
            .iter()
            .filter(|s| !self.state.session_hidden(s))
            .filter(|s| {
                let title = self.session_title(s).to_lowercase();
                let branch = s.branch.to_lowercase();
                let pr = s.review.as_ref().map(|r| format!("#{}", r.number)).unwrap_or_default();
                !words.is_empty() && words.iter().all(|x| title.contains(x) || branch.contains(x) || pr == *x)
            })
            .collect();
        match hits.as_slice() {
            [one] => Ok(one.id.clone()),
            [] => Err(failed(format!("No session “{w}”. `glasscode session list` shows them."))),
            many => Err(usage(format!(
                "“{w}” matches {}: {}. Use its id.",
                plural(many.len(), "session", "sessions"),
                many.iter().take(6).map(|s| format!("{} ({})", short(&s.id), self.session_title(s))).collect::<Vec<_>>().join(", ")
            ))),
        }
    }

    pub fn find_project(&self, word: &str) -> Result<String, Reply> {
        let w = word.trim();
        let path = expand(w, None);
        // Hidden projects are invisible to the CLI and Glasscode: they resolve to "no such project".
        let p = self.state.visible_projects().find(|p| p.id == w || p.name.eq_ignore_ascii_case(w) || p.root == path || p.root.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(w)));
        if let Some(p) = p {
            return Ok(p.id.clone());
        }
        let starts: Vec<_> = self.state.visible_projects().filter(|p| p.name.to_lowercase().starts_with(&w.to_lowercase())).collect();
        if let [one] = starts.as_slice() {
            return Ok(one.id.clone());
        }
        Err(failed(format!("No project “{w}”. Projects: {}.", self.project_names())))
    }

    pub(crate) fn project_names(&self) -> String {
        let names: Vec<String> = self.state.visible_projects().map(|p| p.name.clone()).collect();
        if names.is_empty() {
            return "none yet (`glasscode project import <folder>`)".into();
        }
        names.join(", ")
    }

    /// `--project`, else the calling session's project, else the one the command runs in,
    /// else the only one.
    pub(crate) fn project_for(&self, args: &Args, caller: &Caller) -> Result<String, Reply> {
        if let Some(p) = args.flag("project") {
            return self.find_project(p);
        }
        if let Some(s) = caller.session.as_deref().and_then(|id| self.state.session(id)) {
            return Ok(s.project_id.clone());
        }
        // A server session: its project by name (the Mac has its own copy under the same name).
        if let Some(name) = &caller.project {
            if let Some(p) = self.state.visible_projects().find(|p| p.name.eq_ignore_ascii_case(name)) {
                return Ok(p.id.clone());
            }
        }
        if let Some(cwd) = &caller.cwd {
            let by_root = self.state.visible_projects().filter(|p| cwd.starts_with(&p.root)).max_by_key(|p| p.root.as_os_str().len());
            let by_worktree = self
                .worktrees
                .iter()
                .find(|(id, list)| !self.state.is_hidden(id) && list.iter().any(|w| cwd.starts_with(&w.path)))
                .map(|(id, _)| id.clone());
            if let Some(id) = by_worktree.or_else(|| by_root.map(|p| p.id.clone())) {
                return Ok(id);
            }
        }
        let visible: Vec<&Project> = self.state.visible_projects().collect();
        match visible.as_slice() {
            [one] => Ok(one.id.clone()),
            _ => Err(usage(format!("Say which project: --project <name> ({}).", self.project_names()))),
        }
    }

    pub(crate) fn find_account(&self, agent: AgentKind, word: Option<&str>) -> Result<Option<String>, Reply> {
        if !agent.uses_accounts() {
            return Ok(None);
        }
        match word {
            None => self.state.default_account(agent).map(|a| Some(a.id.clone())).ok_or_else(|| failed(format!("No {} account yet: add one in Settings › Accounts.", agent.name()))),
            Some(w) => self
                .state
                .accounts_for(agent)
                .find(|a| a.id == w || a.label.eq_ignore_ascii_case(w) || a.display().eq_ignore_ascii_case(w))
                .map(|a| Some(a.id.clone()))
                .ok_or_else(|| failed(format!("No {} account “{w}”. Accounts: {}.", agent.name(), self.state.accounts_for(agent).map(|a| a.label.clone()).collect::<Vec<_>>().join(", ")))),
        }
    }

    /// A worktree by its folder, branch, or the end of its path.
    pub(crate) fn find_worktree(&self, word: &str, project: Option<&str>, base: Option<&Path>) -> Result<(String, Worktree), Reply> {
        let w = word.trim().trim_end_matches('/');
        let path = expand(w, base);
        let mut hits: Vec<(String, Worktree)> = Vec::new();
        for (project_id, list) in &self.worktrees {
            // Hidden projects' worktrees are unreachable from the CLI and Glasscode.
            if self.state.is_hidden(project_id) {
                continue;
            }
            if project.is_some_and(|p| p != project_id) {
                continue;
            }
            for wt in list {
                let name = wt.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if wt.path == path || wt.branch == w || name == w || wt.path.to_string_lossy().ends_with(&format!("/{w}")) {
                    hits.push((project_id.clone(), wt.clone()));
                }
            }
        }
        match hits.len() {
            1 => Ok(hits.remove(0)),
            0 => Err(failed(format!("No worktree “{w}”. `glasscode worktree list` shows them."))),
            _ => Err(usage(format!("“{w}” matches {} worktrees: {}. Use its path.", hits.len(), hits.iter().map(|(_, wt)| home_relative(&wt.path)).collect::<Vec<_>>().join(", ")))),
        }
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

    // ----- the commands --------------------------------------------------------------------

    pub fn control_plan(&mut self, caller: &Caller, args: &Args, _cx: &mut Context<Self>) -> Planned {
        let noun = args.word(0).unwrap_or_default().to_lowercase();
        let verb = args.word(1).map(str::to_lowercase);
        match noun.as_str() {
            "board" | "today" | "task" | "tasks" | "todo" => self.board_plan(caller, args, verb.as_deref()),
            "session" | "sessions" | "s" => self.session_plan(caller, args, verb.as_deref()),
            "pr" | "prs" | "pull" => self.pr_plan(caller, args, verb.as_deref()),
            "project" | "projects" => self.project_plan(caller, args, verb.as_deref()),
            "worktree" | "worktrees" | "wt" => self.worktree_plan(caller, args, verb.as_deref()),
            "account" | "accounts" => {
                check(args, &[])?;
                answer(self.accounts_reply())
            }
            "ticket" | "tickets" => self.ticket_plan(caller, args, verb.as_deref()),
            "status" => answer(self.status_reply()),
            "whoami" => answer(self.whoami_reply(caller)),
            "open" | "show" => self.open_plan(caller, args),
            "notify" => {
                check(args, &["title"])?;
                let message = args.rest(1);
                if message.trim().is_empty() {
                    return Err(usage("notify \"<message>\" [--title \"<title>\"]"));
                }
                let title = args.flag("title").map(str::to_string).unwrap_or_else(|| format!("{} says", caller.who));
                let session = caller.session.clone();
                let agent = caller.agent.unwrap_or(AgentKind::Claude);
                plan(Kind::Board, format!("send you a notification: “{message}”"), move |ws, r, _, cx| {
                    ws.notify_session(session.as_deref().unwrap_or("glasscode-notify"), agent, title, message.clone(), true, cx);
                    ws.reply(r, Reply::ok(format!("Notified the person: “{message}”."), Value::Null));
                })
            }
            "log" => {
                check(args, &["limit"])?;
                let limit: usize = args.flag("limit").and_then(|n| n.parse().ok()).unwrap_or(20);
                let entries = glasscode_core::control::read_log(limit);
                let text = if entries.is_empty() {
                    "Nothing yet.".to_string()
                } else {
                    entries
                        .iter()
                        .map(|e| {
                            let at = day::local(e.at as i64);
                            let when = if at == day::today() { clock(e.at) } else { format!("{} {}", day::short(at), clock(e.at)) };
                            let from = e.session_title.as_ref().map(|t| format!(" in “{t}”")).unwrap_or_default();
                            format!("{when}  {}{from}: {}", e.who, e.what)
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                answer(Reply::ok(text, serde_json::to_value(&entries).unwrap_or_default()))
            }
            "approvals" | "pending" => {
                let list = &self.control.approvals;
                let text = if list.is_empty() {
                    "Nothing waits for the person.".to_string()
                } else {
                    list.iter().map(|a| format!("#{}  {}  ({})", a.id, a.summary(), a.command)).collect::<Vec<_>>().join("\n")
                };
                answer(Reply::ok(text, self.control_approvals_json()))
            }
            "wait" => {
                let id: u64 = args.word(1).and_then(|w| w.trim_start_matches('#').parse().ok()).ok_or_else(|| usage("wait <id>: the number the waiting command printed."))?;
                plan(Kind::Read, "read", move |ws, r, _, _| ws.control_wait(id, r))
            }
            "" => Err(usage("Say what to do; `glasscode help` lists the commands.")),
            other => Err(usage(format!("Unknown command “{other}”. `glasscode help` lists them."))),
        }
    }

    fn status_reply(&self) -> Reply {
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

    fn whoami_reply(&self, caller: &Caller) -> Reply {
        let rules: Vec<(Kind, glasscode_core::control::Rule)> = Kind::ALL.iter().map(|k| (*k, self.control_rule(caller, *k))).collect();
        let lines: Vec<String> = rules
            .iter()
            .map(|(k, r)| format!("  {:<6} {}", match r { glasscode_core::control::Rule::Allow => "yes", glasscode_core::control::Rule::Ask => "asks", glasscode_core::control::Rule::Never => "never" }, k.label()))
            .collect();
        let who = match caller.trust {
            Trust::You => "You (at a terminal): everything is allowed.".to_string(),
            Trust::Session => format!("{} (session {}).", caller.describe(), caller.session.as_deref().map(short).unwrap_or_default()),
            Trust::Outside => "A program Glasscode did not start.".to_string(),
        };
        Reply::ok(
            format!("{who}\nWhat it may do:\n{}", lines.join("\n")),
            json!({
                "who": caller.who,
                "session": caller.session,
                "session_title": caller.session_title,
                "trust": match caller.trust { Trust::You => "you", Trust::Session => "session", Trust::Outside => "outside" },
                "rules": rules.iter().map(|(k, r)| (k.key().to_string(), json!(r))).collect::<serde_json::Map<_, _>>(),
            }),
        )
    }

    // ----- board ---------------------------------------------------------------------------

    fn board_plan(&mut self, caller: &Caller, args: &Args, verb: Option<&str>) -> Planned {
        let outside = self.today_outside();
        let today = day::today();
        let item = |ws: &Workspace, index: usize| -> Result<String, Reply> {
            let word = args.word(index).ok_or_else(|| usage(format!("Say which task: `glasscode board {} <id>` (ids are in `glasscode board`).", verb.unwrap_or("done"))))?;
            ws.today.board.resolve(&outside, word).map_err(failed)
        };
        let title_of = |ws: &Workspace, key: &str| -> String {
            ws.today_view().items().find(|i| i.key == key).map(|i| i.title.clone()).unwrap_or_else(|| key.trim_start_matches("task:").to_string())
        };
        match verb {
            None | Some("today" | "list" | "ls") => {
                check(args, &["all", "section"])?;
                let view = self.today_view();
                let needs = self.state.sessions.iter().filter(|s| self.session_state(s) == "needs you").count();
                let mut view = view;
                if let Some(section) = args.flag("section") {
                    let wanted = Section::parse(section).ok_or_else(|| usage(format!("No section “{section}”.")))?;
                    view.sections.retain(|s| s.section == Some(wanted));
                }
                let text = board::render_text(&view, &view.summary(needs), args.has("all") || args.has("section"));
                answer(Reply::ok(text, serde_json::to_value(&view).unwrap_or_default()))
            }
            Some("show") => {
                check(args, &[])?;
                let key = item(self, 2)?;
                let view = self.today_view();
                let found = view.items().find(|i| i.key == key).cloned().ok_or_else(|| failed("That item is not on today's board."))?;
                let mut lines = vec![format!("{} {}", found.id, found.title)];
                if let Some(s) = found.section {
                    lines.push(format!("Section: {}", s.title()));
                }
                for (label, value) in [("Due", found.due_label.clone()), ("Person", found.person.clone()), ("From", found.source.clone()), ("Link", found.url.clone()), ("Back", found.snoozed.clone()), ("", found.added_by.clone())] {
                    if let Some(v) = value {
                        lines.push(if label.is_empty() { v } else { format!("{label}: {v}") });
                    }
                }
                for note in &found.notes {
                    lines.push(format!("Note: {note}"));
                }
                answer(Reply::ok(lines.join("\n"), serde_json::to_value(&found).unwrap_or_default()))
            }
            Some("add" | "new" | "+") => {
                check(args, &["due", "asked-by", "waiting-on", "with", "via", "list", "section", "link", "note", "urgent", "every"])?;
                let text = args.rest(2);
                if text.trim().is_empty() {
                    return Err(usage("board add \"<what to do>\" [--due fri] [--asked-by <name>] … (`glasscode help board`)"));
                }
                let people = self.today_people();
                let structured = ["due", "asked-by", "waiting-on", "with", "list", "section", "every"].iter().any(|f| args.has(f));
                let mut draft = if structured { Draft { title: text.clone(), ..Default::default() } } else { board::parse_quick(&text, today, &people) };
                if structured {
                    draft.links = board::parse_quick(&text, today, &people).links;
                }
                if let Some(due) = args.flag("due") {
                    draft.due = when(due)?;
                }
                if let Some(name) = args.flag("asked-by") {
                    draft.person = Some(name.to_string());
                    draft.list = List::Asked;
                }
                if let Some(name) = args.flag("waiting-on") {
                    draft.person = Some(name.to_string());
                    draft.list = List::Waiting;
                }
                if let Some(name) = args.flag("with") {
                    draft.person = Some(name.to_string());
                }
                if let Some(list) = args.flag("list").or(args.flag("section")) {
                    draft.list = List::parse(list).ok_or_else(|| usage(format!("--list is todo, asked, waiting or later (not {list}).")))?;
                }
                if let Some(every) = args.flag("every") {
                    draft.every = Some(board::Every::parse(every).ok_or_else(|| usage(format!("--every is day, weekday, monday…sunday, month-start or month-end (not {every}).")))?);
                }
                draft.via = args.flag("via").map(str::to_string);
                draft.note = args.flag("note").map(str::to_string);
                draft.urgent |= args.has("urgent");
                for link in args.all("link") {
                    if !draft.links.contains(&link) {
                        draft.links.push(link);
                    }
                }
                let origin = caller.origin();
                let what = format!("add “{}” to Today", draft.title);
                plan(Kind::Board, what, move |ws, r, _, cx| {
                    let recurring = draft.every;
                    let title = draft.title.clone();
                    let id = ws.today.board.add(draft, origin, today);
                    ws.today_save(cx);
                    let view = ws.today_view();
                    let text = match recurring {
                        Some(every) => format!("Added the recurring task “{title}” ({}, id {id}).", every.label().to_lowercase()),
                        None => {
                            let placed = view.items().find(|i| i.id == id);
                            let section = placed.and_then(|i| i.section).map(|s| s.title()).unwrap_or("Today");
                            let due = placed.and_then(|i| i.due_label.clone()).map(|d| format!(", due {}", if matches!(d.as_str(), "Today" | "Tomorrow" | "Yesterday") { d.to_lowercase() } else { d })).unwrap_or_default();
                            format!("Added “{title}” to {section}{due} (id {id}).")
                        }
                    };
                    let item = view.items().find(|i| i.id == id).cloned();
                    ws.reply(r, Reply::ok(text, json!({ "id": id, "item": item })));
                })
            }
            Some(v @ ("done" | "undo" | "undone" | "reopen" | "check" | "tick")) => {
                check(args, &[])?;
                let done = matches!(v, "done" | "check" | "tick");
                let key = item(self, 2)?;
                let title = title_of(self, &key);
                plan(Kind::Board, if done { format!("tick off “{title}”") } else { format!("put “{title}” back on the board") }, move |ws, r, _, cx| {
                    let result = ws.today.board.set_done(&key, done, today);
                    ws.today_save(cx);
                    ws.reply(r, match result {
                        Ok(()) if done => Reply::ok(format!("Ticked off “{title}”."), json!({ "key": key })),
                        Ok(()) => Reply::ok(format!("Put “{title}” back on the board."), json!({ "key": key })),
                        Err(e) => failed(e),
                    });
                })
            }
            Some("snooze" | "later" | "tomorrow") => {
                check(args, &["until"])?;
                let key = item(self, 2)?;
                let until = match args.flag("until").or(args.word(3)) {
                    Some(w) => when(w)?.unwrap_or(today + 1),
                    None => today + 1,
                };
                if until <= today {
                    return Err(usage("Snooze until a day after today."));
                }
                let title = title_of(self, &key);
                let label = day::relative(until, today);
                let label = if label == "Tomorrow" { "tomorrow".to_string() } else { label };
                plan(Kind::Board, format!("snooze “{title}” until {label}"), move |ws, r, _, cx| {
                    let result = ws.today.board.snooze(&key, Some(until));
                    ws.today_save(cx);
                    ws.reply(r, match result {
                        Ok(()) => Reply::ok(format!("Snoozed “{title}”: back {label}."), json!({ "key": key, "until": day::iso(until) })),
                        Err(e) => failed(e),
                    });
                })
            }
            Some("unsnooze" | "back") => {
                check(args, &[])?;
                let key = item(self, 2)?;
                let title = title_of(self, &key);
                plan(Kind::Board, format!("bring “{title}” back to today"), move |ws, r, _, cx| {
                    let result = ws.today.board.snooze(&key, None);
                    ws.today_save(cx);
                    ws.reply(r, match result {
                        Ok(()) => Reply::ok(format!("“{title}” is back on today's board."), json!({ "key": key })),
                        Err(e) => failed(e),
                    });
                })
            }
            Some(v @ ("pin" | "unpin" | "focus" | "unfocus")) => {
                check(args, &[])?;
                let on = matches!(v, "pin" | "focus");
                let key = item(self, 2)?;
                let title = title_of(self, &key);
                plan(Kind::Board, if on { format!("put “{title}” in Focus") } else { format!("take “{title}” out of Focus") }, move |ws, r, _, cx| {
                    let result = ws.today.board.set_focus(&key, on);
                    ws.today_save(cx);
                    ws.reply(r, match result {
                        Ok(()) if on => Reply::ok(format!("“{title}” is in Focus."), json!({ "key": key })),
                        Ok(()) => Reply::ok(format!("Took “{title}” out of Focus."), json!({ "key": key })),
                        Err(e) => failed(e),
                    });
                })
            }
            Some("move") => {
                check(args, &["to", "list", "section"])?;
                let key = item(self, 2)?;
                let to = args.flag("to").or(args.flag("list")).or(args.flag("section")).or(args.word(3)).ok_or_else(|| usage("board move <task> <asked|todo|waiting|later>"))?;
                let section = Section::parse(to).filter(|s| s.movable()).ok_or_else(|| usage(format!("Tasks move to asked, todo, waiting or later (not {to}).")))?;
                let title = title_of(self, &key);
                plan(Kind::Board, format!("move “{title}” to {}", section.title()), move |ws, r, _, cx| {
                    let result = ws.today.board.move_to(&key, section);
                    ws.today_save(cx);
                    ws.reply(r, match result {
                        Ok(()) => Reply::ok(format!("Moved “{title}” to {}.", section.title()), json!({ "key": key, "section": section.key() })),
                        Err(e) => failed(e),
                    });
                })
            }
            Some("note") => {
                check(args, &[])?;
                let key = item(self, 2)?;
                let text = args.rest(3);
                if text.trim().is_empty() {
                    return Err(usage("board note <task> \"<text>\""));
                }
                let title = title_of(self, &key);
                let origin = caller.origin();
                plan(Kind::Board, format!("add a note to “{title}”"), move |ws, r, _, cx| {
                    let result = ws.today.board.add_note(&key, &text, &origin);
                    ws.today_save(cx);
                    ws.reply(r, match result {
                        Ok(()) => Reply::ok(format!("Added a note to “{title}”."), json!({ "key": key })),
                        Err(e) => failed(e),
                    });
                })
            }
            Some("edit") => {
                check(args, &["title", "due", "asked-by", "waiting-on", "with", "via", "link", "urgent", "not-urgent", "list"])?;
                let key = item(self, 2)?;
                let id = key.strip_prefix("task:").map(str::to_string).ok_or_else(|| failed("Only your own tasks can be edited; items from Linear or GitHub change there."))?;
                let due = match args.flag("due") {
                    Some(d) => Some(when(d)?),
                    None => None,
                };
                let list = match args.flag("list") {
                    Some(l) => Some(List::parse(l).ok_or_else(|| usage(format!("--list is todo, asked, waiting or later (not {l}).")))?),
                    None => None,
                };
                let (title, asked, waiting, with, via) = (args.flag("title").map(str::to_string), args.flag("asked-by").map(str::to_string), args.flag("waiting-on").map(str::to_string), args.flag("with").map(str::to_string), args.flag("via").map(str::to_string));
                let links = args.all("link");
                let urgent = if args.has("urgent") { Some(true) } else if args.has("not-urgent") { Some(false) } else { None };
                let name = title_of(self, &key);
                plan(Kind::Board, format!("change “{name}”"), move |ws, r, _, cx| {
                    let Some(task) = ws.today.board.task_mut(&id) else {
                        ws.reply(r, failed("That task is gone."));
                        return;
                    };
                    if let Some(t) = title {
                        task.title = t;
                    }
                    if let Some(d) = due {
                        task.due = d.map(day::iso);
                    }
                    if let Some(p) = asked {
                        task.person = Some(p);
                        task.list = List::Asked;
                    }
                    if let Some(p) = waiting {
                        task.person = Some(p);
                        task.list = List::Waiting;
                    }
                    if let Some(p) = with {
                        task.person = Some(p).filter(|p| !p.is_empty() && p != "none");
                    }
                    if let Some(l) = list {
                        task.list = l;
                    }
                    if via.is_some() {
                        task.via = via;
                    }
                    for link in links {
                        if !task.links.contains(&link) {
                            task.links.push(link);
                        }
                    }
                    if let Some(u) = urgent {
                        task.urgent = u;
                    }
                    task.updated_at = now();
                    let title = task.title.clone();
                    ws.today_save(cx);
                    ws.reply(r, Reply::ok(format!("Changed “{title}”."), json!({ "key": key })));
                })
            }
            Some("remove" | "rm" | "delete") => {
                check(args, &[])?;
                let key = item(self, 2)?;
                let title = title_of(self, &key);
                plan(Kind::Board, format!("take “{title}” off the board"), move |ws, r, _, cx| {
                    let result = ws.today.board.remove(&key);
                    ws.today_save(cx);
                    ws.reply(r, match result {
                        Ok(title) => Reply::ok(format!("Took “{title}” off the board."), json!({ "key": key })),
                        Err(e) => failed(e),
                    });
                })
            }
            Some("recurring" | "repeat") => {
                check(args, &[])?;
                let list = &self.today.board.recurring;
                let text = if list.is_empty() {
                    "No recurring tasks. `glasscode board add \"Review Renovate PRs\" --every monday` adds one.".to_string()
                } else {
                    list.iter().map(|r| format!("{}  {}  · {}", r.id, r.title, r.every.label())).collect::<Vec<_>>().join("\n")
                };
                answer(Reply::ok(text, serde_json::to_value(list).unwrap_or_default()))
            }
            Some(other) => Err(usage(format!("Unknown board command “{other}”. `glasscode help board` lists them."))),
        }
    }

    // ----- sessions --------------------------------------------------------------------------

    fn session_plan(&mut self, caller: &Caller, args: &Args, verb: Option<&str>) -> Planned {
        match verb {
            None | Some("list" | "ls") => {
                check(args, &["project", "needs-you", "server", "all"])?;
                let project = match args.flag("project") {
                    Some(p) => Some(self.find_project(p)?),
                    None => None,
                };
                let needs = args.has("needs-you");
                let mut rows = Vec::new();
                let mut data = Vec::new();
                for s in &self.state.sessions {
                    // Sessions of a hidden project are invisible to the CLI and Glasscode.
                    if self.state.session_hidden(s) {
                        continue;
                    }
                    if project.as_ref().is_some_and(|p| *p != s.project_id) {
                        continue;
                    }
                    let state = self.session_state(s);
                    if needs && state != "needs you" {
                        continue;
                    }
                    let project_name = self.state.project(&s.project_id).map(|p| p.name.clone()).unwrap_or_default();
                    let pr = self.prs.map.get(&s.id).and_then(|p| p.pr.as_ref()).map(|p| format!(" · PR #{}", p.number)).unwrap_or_default();
                    let here = if caller.session.as_deref() == Some(s.id.as_str()) { "  (this session)" } else { "" };
                    rows.push(format!("{}  {:<11} {:<6} {}  · {project_name}/{}{pr}{here}", short(&s.id), state, s.agent.name(), self.session_title(s), s.branch));
                    data.push(self.session_json(s));
                }
                if args.has("server") || args.has("all") {
                    for r in self.server.state.as_ref().map(|s| s.sessions.clone()).unwrap_or_default() {
                        let state = match r.activity {
                            Some(glasscode_core::agents::AgentActivity::NeedsInput) => "needs you",
                            Some(glasscode_core::agents::AgentActivity::Working) => "working",
                            _ if r.running => "idle",
                            _ => "not running",
                        };
                        if needs && state != "needs you" {
                            continue;
                        }
                        rows.push(format!("{}  {:<11} {:<6} {}  · on {}/{}", short(&r.session.id), state, r.session.agent.name(), r.session.title, self.server.name(), r.session.branch));
                        data.push(json!({ "id": r.session.id, "title": r.session.title, "agent": r.session.agent.name().to_lowercase(), "branch": r.session.branch, "state": state, "on_server": true, "last_reply": r.last_reply }));
                    }
                }
                let text = if rows.is_empty() { "No sessions.".to_string() } else { rows.join("\n") };
                answer(Reply::ok(text, Value::Array(data)))
            }
            Some("show" | "info") => {
                check(args, &[])?;
                let id = self.find_session(args.word(2).unwrap_or("this"), caller)?;
                let Some(s) = self.state.session(&id).cloned() else { return Err(failed("That session is gone.")) };
                let mut data = self.session_json(&s);
                let last = s.transcript_path.as_deref().and_then(|p| glasscode_core::transcript::last_reply(s.agent, p));
                data["last_reply"] = json!(last);
                let mut lines = vec![
                    format!("{} ({})", self.session_title(&s), s.id),
                    format!("{} · {} · {}", s.agent.name(), s.account_id.as_deref().and_then(|a| self.state.account(a)).map(|a| a.label.clone()).unwrap_or_else(|| "no account".into()), self.session_state(&s)),
                    format!("Project {} · branch {} · {}", self.state.project(&s.project_id).map(|p| p.name.clone()).unwrap_or_default(), s.branch, home_relative(&s.worktree)),
                ];
                if let Some(pr) = self.prs.map.get(&s.id).and_then(|p| p.pr.as_ref()) {
                    lines.push(format!("Pull request #{} {} {}", pr.number, pr.title, pr.url));
                }
                if let Some(t) = self.state.ticket_of.get(&s.id) {
                    lines.push(format!("Ticket {t}"));
                }
                if let Some(last) = &last {
                    lines.push(format!("Last reply:\n{last}"));
                }
                answer(Reply::ok(lines.join("\n"), data))
            }
            Some("read" | "transcript" | "log") => {
                check(args, &["last"])?;
                let id = self.find_session(args.word(2).unwrap_or("this"), caller)?;
                let Some(s) = self.state.session(&id).cloned() else { return Err(failed("That session is gone.")) };
                let last: usize = args.flag("last").and_then(|n| n.parse().ok()).unwrap_or(6);
                let path = s.transcript_path.clone().ok_or_else(|| failed("That session has no conversation yet."))?;
                let title = self.session_title(&s);
                plan(Kind::Read, "read", move |_ws, r, _, cx| {
                    let agent = s.agent;
                    let task = cx.background_spawn(async move { glasscode_core::transcript::read(agent, &path, None, 400) });
                    cx.spawn(async move |this, cx| {
                        let page = task.await;
                        let _ = this.update(cx, |ws, _| {
                            let reply = match page {
                                Ok(page) => {
                                    let items: Vec<_> = page.items.into_iter().filter(|i| i.kind != glasscode_core::transcript::ItemKind::Tool).collect();
                                    let items = &items[items.len().saturating_sub(last)..];
                                    let text = items
                                        .iter()
                                        .map(|i| format!("{}: {}", if i.kind == glasscode_core::transcript::ItemKind::User { "Person" } else { agent.name() }, i.text.trim()))
                                        .collect::<Vec<_>>()
                                        .join("\n\n");
                                    Reply::ok(format!("“{title}”, the last {}:\n\n{text}", plural(items.len(), "message", "messages")), serde_json::to_value(items).unwrap_or_default())
                                }
                                Err(e) => failed(format!("Could not read its conversation: {e}")),
                            };
                            ws.reply(r, reply);
                        });
                    })
                    .detach();
                })
            }
            Some("new" | "start" | "create") => {
                check(args, &["project", "agent", "account", "title", "branch", "worktree", "ticket", "pr", "review", "from", "model", "mode", "message", "open", "on-server"])?;
                let spec = self.new_spec(caller, args)?;
                self.new_session_plan(spec)
            }
            Some("open" | "focus" | "go") => {
                check(args, &[])?;
                let id = self.find_session(args.word(2).ok_or_else(|| usage("session open <session>"))?, caller)?;
                let title = self.state.session(&id).map(|s| self.session_title(s)).unwrap_or_default();
                plan(Kind::Show, format!("show the session “{title}” in the window"), move |ws, r, window, cx| {
                    ws.tickets.view = crate::tickets_ui::View::Sessions;
                    ws.select_session(&id, window, cx);
                    ws.reply(r, Reply::ok(format!("Showing “{title}”."), json!({ "id": id })));
                })
            }
            Some("rename" | "name" | "title") => {
                check(args, &[])?;
                let id = self.find_session(args.word(2).ok_or_else(|| usage("session rename <session> \"<new name>\""))?, caller)?;
                let name = args.rest(3).trim().to_string();
                if name.is_empty() {
                    return Err(usage("session rename <session> \"<new name>\""));
                }
                let old = self.state.session(&id).map(|s| self.session_title(s)).unwrap_or_default();
                plan(Kind::Rename, format!("rename the session “{old}” to “{name}”"), move |ws, r, _, cx| {
                    if let Some(s) = ws.state.session_mut(&id) {
                        s.title = name.clone();
                        ws.state.custom_titles.insert(id.clone());
                    }
                    ws.save();
                    cx.notify();
                    ws.reply(r, Reply::ok(format!("Renamed “{old}” to “{name}”."), json!({ "id": id, "title": name })));
                })
            }
            Some(v @ ("close" | "restart")) => {
                check(args, &[])?;
                let id = self.find_session(args.word(2).ok_or_else(|| usage(format!("session {v} <session>")))?, caller)?;
                let title = self.state.session(&id).map(|s| self.session_title(s)).unwrap_or_default();
                let restart = v == "restart";
                let own = caller.session.as_deref() == Some(id.as_str());
                plan(Kind::Close, if restart { format!("restart the session “{title}”") } else { format!("close the session “{title}”") }, move |ws, r, window, cx| {
                    if restart {
                        // Answer first: restarting this very session ends the command's own process.
                        ws.reply(r, Reply::ok(format!("Restarting “{title}”; it resumes its conversation."), json!({ "id": id })));
                        ws.restart_session(&id, window, cx);
                    } else {
                        ws.reply(r, Reply::ok(format!("Closed “{title}”. Its worktree stays; a new session there can resume the conversation."), json!({ "id": id })));
                        let _ = own;
                        ws.close_session(&id, window, cx);
                    }
                })
            }
            Some("send" | "message" | "tell") => {
                check(args, &["no-enter"])?;
                let id = self.find_session(args.word(2).ok_or_else(|| usage("session send <session> \"<message>\""))?, caller)?;
                let message = args.rest(3).trim().to_string();
                if message.is_empty() {
                    return Err(usage("session send <session> \"<message>\""));
                }
                if caller.session.as_deref() == Some(id.as_str()) {
                    return Err(usage("That is this session; say it in your reply instead."));
                }
                let title = self.state.session(&id).map(|s| self.session_title(s)).unwrap_or_default();
                let from = caller.describe();
                let short_text: String = message.chars().take(80).collect();
                plan(Kind::Send, format!("send “{short_text}” to the session “{title}”"), move |ws, r, window, cx| {
                    ws.session_send(&id, &title, &from, message, r, window, cx);
                })
            }
            Some("hand-off" | "handoff" | "switch") => {
                check(args, &["to", "account", "note", "new-tab"])?;
                let id = self.find_session(args.word(2).unwrap_or("this"), caller)?;
                let s = self.state.session(&id).cloned().ok_or_else(|| failed("That session is gone."))?;
                let to = match args.flag("to") {
                    Some(a) => agent_of(a)?,
                    None => s.agent.other(),
                };
                if to == AgentKind::Shell {
                    return Err(usage("--to is claude or codex."));
                }
                let account = self.find_account(to, args.flag("account"))?.ok_or_else(|| failed("No account to hand it to."))?;
                let note = args.flag("note").unwrap_or_default().to_string();
                let new_tab = args.has("new-tab");
                let title = self.session_title(&s);
                let label = self.state.lineage_tag(to, Some(&account));
                plan(Kind::Move, format!("hand the session “{title}” to {label}"), move |ws, r, window, cx| {
                    ws.reply(r, Reply::ok(format!("Handing “{title}” to {label}."), json!({ "id": id })));
                    ws.handoff_now(&id, to, account, note, new_tab, window, cx);
                })
            }
            Some("move") => {
                let to = move_to(args)?;
                let id = self.find_session(args.word(2).unwrap_or("this"), caller)?;
                let title = self.state.session(&id).map(|s| self.session_title(s)).unwrap_or_default();
                if to == MoveTo::ThisComputer {
                    let option = crate::platform::computer().to_lowercase();
                    return Err(usage(format!("`--to-{option}` brings a server session back; this one runs on {} already.", crate::platform::this_computer())));
                }
                if self.server.config.is_none() {
                    return Err(failed("No server is set up (Settings › Server & devices)."));
                }
                let server = self.server.name();
                plan(Kind::Move, format!("move the session “{title}” to {server}"), move |ws, r, _, cx| {
                    ws.move_one(crate::switch_ui::Direction::ToServer, id.clone(), cx);
                    ws.reply(r, Reply::ok(format!("Moving “{title}” to {server} once its agent is idle."), json!({ "id": id })));
                })
            }
            Some(other) => Err(usage(format!("Unknown session command “{other}”. `glasscode help session` lists them."))),
        }
    }

    /// Type a message into another session, like a person would (it starts the session first
    /// when it is not running).
    #[allow(clippy::too_many_arguments)]
    fn session_send(&mut self, id: &str, title: &str, from: &str, message: String, r: Responder, window: &mut Window, cx: &mut Context<Self>) {
        let started = !self.terminals.contains_key(id);
        if started {
            self.ensure_terminal(id, window, cx);
        }
        let Some(terminal) = self.terminals.get(id).cloned() else {
            self.reply(r, failed(format!("“{title}” could not be started.")));
            return;
        };
        // A shell session has no message box to paste into, and the "(Sent by …)" line would
        // run as a command there; a message to another agent gets that note so the Chat view can
        // show it.
        let agent = self.state.session(id).map(|s| s.agent).unwrap_or(AgentKind::Claude);
        if agent == AgentKind::Shell {
            self.reply(r, failed(format!("“{title}” is a shell, not an agent; there is nothing to send a message to.")));
            return;
        }
        let working = self.activity.get(id) == Some(&AgentActivity::Working);
        let text = format!("{message}\n\n(Sent by Glasscode because {from} asked it to.)");
        let (id, title) = (id.to_string(), title.to_string());
        cx.spawn(async move |this, cx| {
            // A session just started needs its prompt first.
            for _ in 0..if started { 40 } else { 1 } {
                let ready = this.update(cx, |_, cx| {
                    let tail = terminal.read(cx).text_tail(60);
                    crate::ci_fix::prompt_box(&tail).is_some() || tail.contains('›') || tail.contains("▌")
                });
                if ready.unwrap_or(false) {
                    break;
                }
                cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
            }
            let asking = this.update(cx, |_, cx| glasscode_core::prompt::parse(&terminal.read(cx).text_tail(60)).is_some()).unwrap_or(true);
            if asking {
                let _ = this.update(cx, |ws, _| ws.reply(r, failed(format!("“{title}” is asking the person something right now; answer that first, then send again."))));
                return;
            }
            // The person may be typing in it: do not send on top of their half-written line.
            let leftover = this.update(cx, |_, cx| if agent == AgentKind::Claude { crate::chat_ui::claude_box(terminal.read(cx)) } else { String::new() }).unwrap_or_default();
            if !leftover.trim().is_empty() {
                let _ = this.update(cx, |ws, _| ws.reply(r, failed(format!("“{title}” has text already typed in its box; it was left alone. Try again once it is sent or cleared."))));
                return;
            }
            let _ = terminal.update(cx, |t, cx| t.paste_text(&text, cx));
            cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
            let _ = terminal.update(cx, |t, cx| t.write(b"\r".to_vec(), cx));
            // Claude queues what is sent while it works; when it is idle, an Enter can be lost, so
            // press again if the message is still sitting in its box.
            if agent == AgentKind::Claude && !working {
                for wait in [600u64, 900, 1500] {
                    cx.background_executor().timer(std::time::Duration::from_millis(wait)).await;
                    let still = this.update(cx, |_, cx| crate::chat_ui::claude_box(terminal.read(cx)).trim() == text.trim() || crate::ci_fix::prompt_box(&terminal.read(cx).text_tail(60)).as_deref().map(str::trim) == Some(text.trim())).unwrap_or(false);
                    if !still {
                        break;
                    }
                    let _ = terminal.update(cx, |t, cx| t.write(b"\r".to_vec(), cx));
                }
            }
            let _ = this.update(cx, |ws, _| ws.reply(r, Reply::ok(format!("Sent to “{title}”."), json!({ "id": id }))));
        })
        .detach();
    }
}

fn clock(unix: u64) -> String {
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
    pub open: bool,
    pub on_server: bool,
    pub started_by: String,
}

impl Workspace {
    pub(crate) fn new_spec(&self, caller: &Caller, args: &Args) -> Result<NewSpec, Reply> {
        let project_id = self.project_for(args, caller)?;
        let agent = match args.flag("agent") {
            Some(a) => agent_of(a)?,
            None => AgentKind::Claude,
        };
        let account_id = self.find_account(agent, args.flag("account"))?;
        let worktree = match args.flag("worktree") {
            Some(w) => Some(self.find_worktree(w, Some(&project_id), caller.cwd.as_deref())?.1.path),
            None => None,
        };
        let ticket = match args.flag("ticket") {
            Some(t) => Some(self.tickets.ticket(&t.to_uppercase()).cloned().ok_or_else(|| failed(format!("No ticket {t} among yours in Linear (Tickets shows the ones Glasscode knows).")))?),
            None => None,
        };
        let pr = args.flag("pr").map(pr_number).transpose()?;
        let review = args.flag("review").map(pr_number).transpose()?;
        let choices = [worktree.is_some(), args.has("branch"), pr.is_some(), review.is_some()].iter().filter(|b| **b).count();
        if choices > 1 {
            return Err(usage("Pick one of --worktree, --branch, --pr and --review."));
        }
        if let Some(mode) = args.flag("mode") {
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
        let branch = args.flag("branch").map(|b| b.trim().replace(' ', "-"));
        if let Some(b) = &branch {
            let root = self.state.project(&project_id).map(|p| p.root.clone());
            if root.is_some_and(|root| git::branch_exists(&root, b)) {
                return Err(failed(format!("Branch {b} exists already; use --worktree {b} to work in it.")));
            }
        }
        Ok(NewSpec {
            project_id,
            agent,
            account_id,
            title: args.flag("title").map(str::to_string),
            branch,
            worktree,
            from: args.flag("from").map(str::to_string),
            ticket,
            pr,
            review,
            model: args.flag("model").map(str::to_string),
            mode: args.flag("mode").map(|m| match agent {
                AgentKind::Claude => glasscode_core::agents::claude_mode(m).unwrap_or("default").to_string(),
                _ => m.to_string(),
            }),
            message: args.flag("message").map(str::to_string).filter(|m| !m.trim().is_empty()),
            open: args.has("open"),
            on_server: args.has("on-server"),
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
        let known_worktrees: Vec<Worktree> = self.worktrees.get(&project.id).cloned().unwrap_or_default();
        let task = cx.background_spawn(async move {
            let repo = || github::repo_slug(&root).ok_or_else(|| anyhow::anyhow!("this project's origin is not on GitHub"));
            let find_pr = |n: u64| -> anyhow::Result<github::PrHit> {
                github::search_prs(&repo()?, &n.to_string(), &env)?.into_iter().find(|h| h.number == n).ok_or_else(|| anyhow::anyhow!("GitHub has no pull request #{n} in {}", repo().unwrap_or_default()))
            };
            let no_review: Option<glasscode_core::model::ReviewRef> = None;
            if let Some(n) = pr {
                let hit = find_pr(n)?;
                let checkout = git::checkout_pr_branch(&root, &name, hit.number, &hit.head, hit.same_repo)?;
                return Ok::<_, anyhow::Error>((checkout.path, checkout.branch, Some(checkout.note), no_review, Some(hit), None));
            }
            if let Some(path) = worktree {
                let branch = git::current_branch(&path);
                return Ok((path, branch, None, None, None, None));
            }
            if plain {
                return Ok((root.clone(), String::new(), None, None, None, None));
            }
            // A ticket: its branch (an existing worktree on it, else a new one), and its text.
            let detail = ticket.as_ref().and_then(|t| linear::Client::saved().and_then(|c| c.detail(&t.id).ok()));
            let wanted = branch.or_else(|| ticket.as_ref().map(|t| linear::branch_for(&t.identifier, &t.title))).unwrap_or_else(|| format!("glasscode/session-{}", &new_id()[..4]));
            if let Some(existing) = known_worktrees.iter().find(|w| w.branch == wanted) {
                return Ok((existing.path.clone(), wanted, None, None, None, detail));
            }
            let base = match from {
                Some(b) => git::FreshBase { base: b, ..git::start_base(&root, false) },
                None => git::start_base(&root, fetch),
            };
            let note = (fetch && !base.fetched).then(|| format!("Could not fetch origin (offline?); the branch starts from {} as it is on {}.", base.base, crate::platform::this_computer()));
            let path = git::add_worktree(&root, &name, &wanted, &base.base)?;
            Ok((path, wanted, note, None, None, detail))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |ws, window, cx| match result {
                Ok((worktree, branch, note, review, hit, detail)) => {
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
                    ws.state.sessions.push(session.clone());
                    glasscode_core::activity::record_session();
                    if spec.open {
                        ws.tickets.view = crate::tickets_ui::View::Sessions;
                        ws.center = crate::center::Center::Session;
                        ws.select_session(&id, window, cx);
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
            self.reply(r, failed("The server has no copy of this project (Settings › Server & devices › Copy to server)."));
            return;
        };
        if spec.review.is_some() || spec.ticket.is_some() || spec.worktree.is_some() {
            self.reply(r, failed("On the server, `session new` takes --branch, --pr, --message, --agent, --account and --title for now."));
            return;
        }
        let new_branch = spec.branch.clone().or_else(|| spec.pr.is_none().then(|| format!("glasscode/session-{}", &new_id()[..4])));
        let start = match spec.pr {
            Some(n) => {
                let root = self.state.project(&spec.project_id).map(|p| p.root.clone()).unwrap_or_default();
                let repo = github::repo_slug(&root).unwrap_or_default();
                match github::search_prs(&repo, &n.to_string(), &self.env).ok().and_then(|h| h.into_iter().find(|h| h.number == n)) {
                    Some(hit) => Some(glasscode_core::remote::StartFrom::PullRequest { number: n, head: hit.head, same_repo: hit.same_repo }),
                    None => {
                        self.reply(r, failed(format!("GitHub has no pull request #{n} here.")));
                        return;
                    }
                }
            }
            None => Some(glasscode_core::remote::StartFrom::Main),
        };
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
        };
        let server = self.server.name();
        self.create_server_session(create, window, cx);
        self.reply(r, Reply::ok(format!("Starting it on {server}."), Value::Null));
    }

    // ----- pull requests ---------------------------------------------------------------------

    fn pr_plan(&mut self, caller: &Caller, args: &Args, verb: Option<&str>) -> Planned {
        match verb {
            Some("review" | "build" | "work") => {
                check(args, &["project", "agent", "account", "message", "open", "model", "mode", "on-server", "title"])?;
                let n = pr_number(args.word(2).ok_or_else(|| usage(format!("pr {} <number>", verb.unwrap_or("build"))))?)?;
                let mut spec = self.new_spec(caller, args)?;
                if verb == Some("review") {
                    spec.review = Some(n);
                } else {
                    spec.pr = Some(n);
                }
                // Already reviewing it here: say so instead of starting a second one.
                if verb == Some("review") {
                    if let Some(s) = self.state.sessions.iter().find(|s| s.review.as_ref().is_some_and(|r| r.number == n)) {
                        return answer(Reply::ok(format!("Already reviewing #{n} in “{}” ({}).", self.session_title(s), short(&s.id)), self.session_json(s)));
                    }
                }
                self.new_session_plan(spec)
            }
            Some("status" | "show" | "view") => {
                check(args, &["project"])?;
                let n = pr_number(args.word(2).ok_or_else(|| usage("pr status <number>"))?)?;
                let project_id = self.project_for(args, caller)?;
                let root = self.state.project(&project_id).map(|p| p.root.clone()).unwrap_or_default();
                let env = self.env.clone();
                plan(Kind::Read, "read", move |_ws, r, _, cx| {
                    let task = cx.background_spawn(async move {
                        let repo = github::repo_slug(&root).ok_or_else(|| anyhow::anyhow!("this project's origin is not on GitHub"))?;
                        let preview = github::PreviewConfig::load(&root);
                        github::pr_details(&repo, n, &preview, &env)
                    });
                    cx.spawn(async move |this, cx| {
                        let result = task.await;
                        let _ = this.update(cx, |ws, _| ws.reply(r, match result {
                            Ok(d) => pr_status_reply(&d),
                            Err(e) => failed(format!("Could not read #{n}: {e:#}")),
                        }));
                    })
                    .detach();
                })
            }
            None | Some("list" | "ls") => {
                check(args, &["project"])?;
                let reviews = &self.tickets.reviews;
                let text = if reviews.is_empty() {
                    "No pull requests ask for your review.".to_string()
                } else {
                    reviews.iter().map(|r| format!("#{:<6} {}  · {} · by {}", r.hit.number, r.hit.title, r.repo, r.hit.author)).collect::<Vec<_>>().join("\n")
                };
                let data: Vec<Value> = reviews.iter().map(|r| json!({ "repo": r.repo, "number": r.hit.number, "title": r.hit.title, "author": r.hit.author, "url": r.hit.url })).collect();
                answer(Reply::ok(format!("Asking for your review:\n{text}"), Value::Array(data)))
            }
            Some(v @ ("rerun" | "comment" | "label" | "open-draft" | "draft")) => self.pr_change_plan(caller, args, v),
            Some(other) => Err(usage(format!("Unknown pr command “{other}”. `glasscode help pr` lists them."))),
        }
    }
}

fn pr_status_reply(d: &github::PrDetails) -> Reply {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &[&str]) -> Args {
        Args::parse(&line.iter().map(|w| w.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn to_pc_and_to_mac_are_the_same_move() {
        let mac = parse(&["session", "move", "--to-mac", "abc"]);
        let pc = parse(&["session", "move", "--to-pc", "abc"]);
        // A switch: the session after it stays a word.
        assert_eq!((mac.word(2), pc.word(2)), (Some("abc"), Some("abc")));
        assert_eq!(move_to(&mac).ok(), Some(MoveTo::ThisComputer));
        assert_eq!(move_to(&pc).ok(), move_to(&mac).ok());
        assert_eq!(move_to(&parse(&["session", "move", "abc", "--to-server"])).ok(), Some(MoveTo::Server));
        assert!(move_to(&parse(&["session", "move", "abc", "--to-laptop"])).is_err());
    }
}
