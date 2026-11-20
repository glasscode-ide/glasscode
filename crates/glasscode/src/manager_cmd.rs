//! `glasscode manager …`: what the manager's own brain, a session's agent or the person can
//! ask of the manager from a terminal. Reading is free; what changes the manager's own data
//! (its memory, follow-ups, drafts, checks) counts as the Today board does in Settings ›
//! Automation; showing, drawing and speaking count as changing what the window shows. A
//! message to a teammate is only ever drafted here: allowing one is the person's alone.

use std::path::{Path, PathBuf};
use std::time::Instant;

use gpui::{AppContext as _, Context, Window};
use glasscode_core::control::{Args, Kind, Reply, code};
use glasscode_core::manager::{Action, Check, FollowState, FollowUp, MsgState, OutMessage, PanelTab, Settings, Show, Tier, brief, followups, memory, messages, scene, sync, voice};
use glasscode_core::model::{new_id, now};
use serde_json::{Value, json};

use crate::control_ui::{Caller, Plan, Responder, Trust};
use crate::workspace::Workspace;

type Planned = Result<Plan, Reply>;

const OFF: &str = "The manager is off. Switch it on in Settings › Manager.";

/// The server's copy runs the manager while its last round is no older than this.
const SERVER_FRESH_SECS: u64 = 90;

/// A voice token is good for this long.
const TOKEN_SECS: u64 = 3600;

/// The screens `show --view` knows.
const VIEWS: &[&str] = &["manager", "sessions", "today", "tickets", "usage", "settings"];

/// The parts of a session's right panel `show --section` can point at.
const SECTIONS: &[&str] = &["checks", "preview", "reviews", "comments", "changes", "files", "reply"];

fn usage(text: impl Into<String>) -> Reply {
    Reply::fail(code::USAGE, text)
}

fn failed(text: impl Into<String>) -> Reply {
    Reply::fail(code::FAILED, text)
}

fn denied(text: impl Into<String>) -> Reply {
    Reply::fail(code::DENIED, text)
}

fn plan(kind: Kind, what: impl Into<String>, run: impl FnOnce(&mut Workspace, Responder, &mut Window, &mut Context<Workspace>) + 'static) -> Planned {
    Ok(Plan { kind, what: what.into(), run: Box::new(run) })
}

fn answer(reply: Reply) -> Planned {
    plan(Kind::Read, "read", move |ws, r, _, _| ws.reply(r, reply))
}

fn check(args: &Args, known: &[&str]) -> Result<(), Reply> {
    let unknown = args.unknown(known);
    if unknown.is_empty() {
        return Ok(());
    }
    Err(usage(format!("Unknown option {}. This command takes: {}.", unknown.join(", "), if known.is_empty() { "no options".to_string() } else { known.iter().map(|k| format!("--{k}")).collect::<Vec<_>>().join(", ") })))
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// At most `max` characters of one line, with "…" when something was cut.
fn clip(text: &str, max: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max {
        return line;
    }
    let mut out: String = line.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn to_json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or_default()
}

/// "12 Nov" of a Unix time.
fn day_of(unix: u64) -> String {
    glasscode_core::day::short(glasscode_core::day::local(unix as i64))
}

/// A relative path is from where the command was run.
fn expand(path: &str, base: Option<&Path>) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        return glasscode_core::paths::home().join(rest);
    }
    let p = PathBuf::from(path);
    match base {
        Some(base) if p.is_relative() => base.join(p),
        _ => p,
    }
}

/// "30", "30m", "2h", "1d" as minutes.
fn minutes(text: &str) -> Option<u64> {
    let t = text.trim().to_lowercase();
    let (number, times) = match t.chars().last()? {
        'm' => (&t[..t.len() - 1], 1),
        'h' => (&t[..t.len() - 1], 60),
        'd' => (&t[..t.len() - 1], 1440),
        _ => (t.as_str(), 1),
    };
    number.trim().parse::<u64>().ok().filter(|n| *n > 0).map(|n| n * times)
}

/// "every 30 minutes", "every 2 hours".
fn every(secs: u64) -> String {
    match secs / 60 {
        m if m % 1440 == 0 => format!("every {}", plural((m / 1440) as usize, "day", "days")),
        m if m % 60 == 0 => format!("every {}", plural((m / 60) as usize, "hour", "hours")),
        m => format!("every {}", plural(m as usize, "minute", "minutes")),
    }
}

/// An option that is on or off but was followed by the text ("--voice what is blocked"):
/// the text it took, when it is not just yes or no.
fn swallowed<'a>(args: &'a Args, name: &str) -> Option<&'a str> {
    args.flag(name).filter(|v| !matches!(v.to_lowercase().as_str(), "true" | "false" | "yes" | "no" | "1" | "0" | "on" | "off"))
}

/// Who a note or a follow-up comes from: "you", "manager" or "session:<id>".
fn source_of(caller: &Caller) -> String {
    match (&caller.session, caller.trust) {
        (_, Trust::You) => "you".into(),
        (Some(id), _) => format!("session:{id}"),
        (None, Trust::Session) => "manager".into(),
        (None, Trust::Outside) => "outside".into(),
    }
}

fn tab_of(word: &str) -> Result<PanelTab, Reply> {
    match word.to_lowercase().replace(['-', '_', ' '], "").as_str() {
        "pr" | "pullrequest" | "pull" => Ok(PanelTab::PullRequest),
        "changes" | "diff" => Ok(PanelTab::Changes),
        "files" => Ok(PanelTab::Files),
        _ => Err(usage(format!("“{word}” is not a tab: pr, changes or files."))),
    }
}

/// The tab a part of the panel is on.
fn tab_for(section: &str) -> Option<PanelTab> {
    match section {
        "checks" | "preview" | "reviews" | "comments" => Some(PanelTab::PullRequest),
        "changes" => Some(PanelTab::Changes),
        "files" => Some(PanelTab::Files),
        _ => None,
    }
}

fn follow_up_line(f: &FollowUp) -> String {
    let state = match f.state {
        FollowState::Open => "open",
        FollowState::Fixed => "fixed?",
        FollowState::Done => "done",
        FollowState::Dropped => "dropped",
    };
    let what = if f.number > 0 { format!("{}#{} “{}”", f.repo, f.number, clip(&f.title, 60)) } else { format!("“{}”", clip(if f.title.is_empty() { &f.note } else { &f.title }, 90)) };
    let who = match (f.person.trim(), f.login.trim()) {
        ("", "") => String::new(),
        ("", login) => format!("  · @{login}"),
        (person, _) => format!("  · {person}"),
    };
    let blockers = if f.blockers.is_empty() { String::new() } else { format!("  · {} of {} still open", f.open_blockers(), f.blockers.len()) };
    format!("{}  [{state}] {what}{who}{blockers}", short(&f.id))
}

fn message_state(m: &OutMessage) -> &'static str {
    match (m.state, m.tier) {
        (MsgState::Draft, Tier::Never) => "never sent",
        (MsgState::Draft, _) => "waits for the person",
        (MsgState::Approved, _) => "allowed, going out",
        (MsgState::Sent, _) => "sent",
        (MsgState::Rejected, _) => "rejected",
        (MsgState::Failed, _) => "failed",
    }
}

fn message_line(m: &OutMessage) -> String {
    let why = if m.why.trim().is_empty() { String::new() } else { format!("  (why: {})", clip(&m.why, 70)) };
    let error = m.error.as_deref().filter(|e| !e.is_empty()).map(|e| format!("  ({})", clip(e, 70))).unwrap_or_default();
    format!("{}  [{}] to {}: “{}”{why}{error}", short(&m.id), message_state(m), m.to, clip(&m.body, 110))
}

fn check_line(c: &Check) -> String {
    let when = if c.once { format!("once, {} after it was added", every(c.every_secs).trim_start_matches("every ")) } else { every(c.every_secs) };
    format!("{}  {}  · {when}", short(&c.id), clip(&c.what, 100))
}

/// The one thing in a list that `which` names: its id, the start of its id, or words from
/// its text. `Err` says why not (nothing, or several).
fn pick<'a, T>(items: &'a [T], which: &str, id: impl Fn(&T) -> &str, text: impl Fn(&T) -> String, noun: &str) -> Result<&'a T, Reply> {
    let w = which.trim();
    if let Some(exact) = items.iter().find(|i| id(i) == w) {
        return Ok(exact);
    }
    let starts: Vec<&T> = items.iter().filter(|i| w.len() >= 4 && id(i).starts_with(w)).collect();
    if let [one] = starts.as_slice() {
        return Ok(one);
    }
    let words: Vec<String> = w.to_lowercase().split_whitespace().map(str::to_string).collect();
    let hits: Vec<&T> = items
        .iter()
        .filter(|i| {
            let t = text(i).to_lowercase();
            !words.is_empty() && words.iter().all(|x| t.contains(x))
        })
        .collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(failed(format!("No {noun} “{w}”."))),
        many => Err(usage(format!("“{w}” matches {} of them: {}. Use its id.", many.len(), many.iter().take(6).map(|i| short(id(i))).collect::<Vec<_>>().join(", ")))),
    }
}

impl Workspace {
    pub fn manager_plan(&mut self, caller: &Caller, args: &Args, verb: Option<&str>) -> Result<Plan, Reply> {
        let settings = self.state.manager.clone();
        let name = settings.name.clone();
        if !settings.enabled && !matches!(verb, None | Some("status")) {
            return Err(failed(OFF));
        }
        match verb {
            None | Some("status") => {
                check(args, &[])?;
                answer(self.manager_status_reply())
            }
            Some("ask") => {
                check(args, &["voice"])?;
                if caller.is_manager() {
                    return Err(denied(format!("{name} cannot ask itself. Answer from what you know, or look with the glasscode command.")));
                }
                let text = [swallowed(args, "voice").unwrap_or_default(), args.rest(2).as_str()].join(" ").trim().to_string();
                if text.is_empty() {
                    return Err(usage("manager ask \"<question>\" [--voice]"));
                }
                let voice = args.has("voice");
                // Whose words the question is comes from who sent the command, never a composer's
                // (D1): an agent's is a peer's, a terminal's is outside (ux-m1).
                glasscode_core::words::record(glasscode_core::words::command_origin(&caller.channel), &text, caller.session.as_deref(), None);
                // The manager takes a question as the person's. One from a session's agent says so.
                let text = if caller.trust == Trust::Session { format!("(Asked by {}, an agent, not the person.) {text}", caller.describe()) } else { text };
                // Asking changes nothing by itself (what the manager then does passes Settings ›
                // Automation as its own), so the person's voice and phone can ask without an OK.
                plan(Kind::Read, format!("ask {name}: “{}”", clip(&text, 80)), move |ws, r, _, cx| {
                    ws.manager_ask_then(
                        text,
                        voice,
                        Box::new(move |ws, say, _cx| {
                            let reply = match say {
                                Ok(say) => Reply::ok(say.clone(), json!({ "say": say })),
                                Err(why) => failed(why),
                            };
                            ws.reply(r, reply);
                        }),
                        cx,
                    );
                })
            }
            Some("say") => {
                check(args, &[])?;
                let text = args.rest(2).trim().to_string();
                if text.is_empty() {
                    return Err(usage("manager say \"<text>\""));
                }
                plan(Kind::Show, format!("have {name} say “{}”", clip(&text, 80)), move |ws, r, _, cx| {
                    ws.manager_say(&text, None, cx);
                    ws.reply(r, Reply::ok(format!("{} said it.", ws.state.manager.name), json!({ "say": text })));
                })
            }
            Some("brief" | "update") => {
                check(args, &[])?;
                let made = brief::build(&settings, &self.manager.harness.data, &self.manager_snapshot(), now());
                let text = match (made.text.trim().is_empty(), made.lines.is_empty()) {
                    (false, _) => made.text.clone(),
                    (true, false) => made.lines.iter().map(|l| format!("- {}", l.text)).collect::<Vec<_>>().join("\n"),
                    (true, true) => "Nothing to report today.".to_string(),
                };
                answer(Reply::ok(text, to_json(&made)))
            }
            Some("tour" | "walk") => {
                check(args, &[])?;
                plan(Kind::Show, "walk you through today's update", move |ws, r, _, cx| {
                    let stops = brief::tour(&ws.state.manager, &ws.manager.harness.data, &ws.manager_snapshot());
                    if stops.is_empty() {
                        ws.reply(r, Reply::ok("Nothing to walk through right now.", json!({ "stops": [] })));
                        return;
                    }
                    let (count, data) = (stops.len(), to_json(&stops));
                    ws.manager_start_tour(stops, cx);
                    ws.reply(r, Reply::ok(format!("Started the walk through today's update: {}.", plural(count, "stop", "stops")), json!({ "stops": data })));
                })
            }
            Some("remember") => {
                check(args, &[])?;
                let text = args.rest(2).trim().to_string();
                if text.is_empty() {
                    return Err(usage("manager remember \"<fact>\""));
                }
                let source = source_of(caller);
                plan(Kind::Board, format!("have {name} remember “{}”", clip(&text, 80)), move |ws, r, _, cx| {
                    let id = memory::remember(&mut ws.manager.harness.data, &text, &source, now());
                    ws.manager.harness.dirty = true;
                    cx.notify();
                    ws.reply(r, Reply::ok(format!("Remembered: {text}"), json!({ "id": id, "text": text })));
                })
            }
            Some("forget") => {
                check(args, &[])?;
                let which = args.rest(2).trim().to_string();
                if which.is_empty() {
                    return Err(usage("manager forget \"<id or words>\""));
                }
                plan(Kind::Board, format!("have {name} forget “{}”", clip(&which, 80)), move |ws, r, _, cx| {
                    let reply = match memory::forget(&mut ws.manager.harness.data, &which, now()) {
                        Some(text) => {
                            ws.manager.harness.dirty = true;
                            cx.notify();
                            Reply::ok(format!("Forgot: {text}"), json!({ "text": text }))
                        }
                        None => failed(format!("Nothing in memory matches “{which}”. `glasscode manager memory` lists what is kept.")),
                    };
                    ws.reply(r, reply);
                })
            }
            Some("memory" | "notes") => {
                check(args, &["limit"])?;
                let query = args.rest(2).trim().to_string();
                let limit: usize = args.flag("limit").and_then(|n| n.parse().ok()).unwrap_or(20);
                let data = &self.manager.harness.data;
                let notes = if query.is_empty() { memory::all(data) } else { memory::recall(data, &query, limit) };
                let text = match (notes.is_empty(), query.is_empty()) {
                    (true, true) => "Nothing remembered yet. `glasscode manager remember \"<fact>\"` keeps one.".to_string(),
                    (true, false) => format!("Nothing in memory matches “{query}”."),
                    _ => notes.iter().take(limit.max(1)).map(|n| format!("{}  {}  ({}, {})", short(&n.id), n.text, n.source, day_of(n.updated.max(n.created)))).collect::<Vec<_>>().join("\n"),
                };
                answer(Reply::ok(text, Value::Array(notes.iter().take(limit.max(1)).map(|n| to_json(*n)).collect())))
            }
            Some("follow-up" | "follow-ups" | "followup" | "followups" | "follow") => self.manager_follow_up_plan(caller, args, &name),
            Some("message" | "messages" | "msg") => self.manager_message_plan(caller, args, &settings),
            Some("show") => self.manager_show_plan(caller, args, &settings),
            Some("scene" | "draw") => {
                check(args, &["file"])?;
                let text = match args.flag("file") {
                    Some(path) => {
                        let path = expand(path, caller.cwd.as_deref());
                        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 512 * 1024) {
                            return Err(failed(format!("{} is too large for a scene.", path.display())));
                        }
                        std::fs::read_to_string(&path).map_err(|e| failed(format!("Could not read {}: {e}", path.display())))?
                    }
                    None => args.rest(2),
                };
                if text.trim().is_empty() {
                    return Err(usage("manager scene '<json>'  (or --file <path>)"));
                }
                // Read here, before anyone is asked: a mistake in the JSON goes straight back.
                let scene = scene::parse(&text).map_err(|e| usage(format!("That scene cannot be drawn: {e:#}")))?;
                let title = if scene.title.trim().is_empty() { "a picture".to_string() } else { format!("“{}”", clip(&scene.title, 60)) };
                plan(Kind::Show, format!("show {title}, drawn by {name}"), move |ws, r, _, cx| {
                    let (steps, data) = (scene.steps.len(), to_json(&scene));
                    ws.manager_apply(vec![Action::Scene { scene }], cx);
                    ws.reply(r, Reply::ok(format!("Drawing {title} ({}).", plural(steps, "step", "steps")), data));
                })
            }
            Some("check" | "checks") => self.manager_check_plan(args, &name),
            Some("voice-token") => {
                check(args, &["identity"])?;
                // A token lets a device listen and speak in the manager's room as the person: for
                // the person and their own devices (the voice server asks for the glasses and the
                // phone), never for an agent in a session or the manager's own.
                if caller.trust == Trust::Session {
                    return Err(denied("A voice token is for the person's own devices, not for an agent."));
                }
                let url = settings.voice.livekit_url.trim().to_string();
                if url.is_empty() {
                    return Err(failed("Voice is not set up: there is no LiveKit server in Settings › Manager."));
                }
                let room = voice::room(&settings);
                let identity = args.flag("identity").map(str::to_string).unwrap_or_else(|| format!("cli-{}", short(&new_id())));
                plan(Kind::Read, "read", move |_ws, r, _, cx| {
                    // The Keychain may take a moment: not on the main thread.
                    let (room_for, identity_for) = (room.clone(), identity.clone());
                    let task = cx.background_spawn(async move {
                        let (key, secret) = voice::keys().ok_or_else(|| "Voice is not set up: the LiveKit key and secret are missing in Settings › Manager.".to_string())?;
                        voice::token(&key, &secret, &identity_for, &room_for, TOKEN_SECS).map_err(|e| format!("Could not make a voice token: {e:#}"))
                    });
                    cx.spawn(async move |this, cx| {
                        let result = task.await;
                        let _ = this.update(cx, |ws, _| {
                            let reply = match result {
                                Ok(token) => Reply::ok(format!("Room {room} at {url} as {identity}, good for an hour:\n{token}"), json!({ "url": url, "token": token, "room": room, "identity": identity })),
                                Err(why) => failed(why),
                            };
                            ws.reply(r, reply);
                        });
                    })
                    .detach();
                })
            }
            Some(other) => Err(usage(format!("Unknown manager command “{other}”. `glasscode help manager` lists them."))),
        }
    }

    /// `manager status`: also answers while the manager is off.
    fn manager_status_reply(&self) -> Reply {
        let settings = &self.state.manager;
        let ui = &self.manager;
        let data = &ui.harness.data;
        let home = match settings.home {
            glasscode_core::manager::Home::Mac => "mac",
            glasscode_core::manager::Home::Server => "server",
        };
        if !settings.enabled {
            let data = json!({ "enabled": false, "name": settings.name, "initials": settings.initials(), "state": "idle", "caption": "", "needs_you": 0, "follow_ups_open": 0, "messages_waiting": 0, "home": home, "on_server": false });
            return Reply::ok(OFF, data);
        }
        let state = if ui.thinking {
            "thinking"
        } else if ui.speaking_until.is_some_and(|until| until > Instant::now()) {
            "speaking"
        } else {
            "idle"
        };
        let needs_you = ui.harness.needs_you(&self.manager_snapshot());
        let follow_ups = followups::open(data).len();
        let waiting = messages::waiting(data).len();
        let on_server = sync::runs_on_server(settings, ui.server_heartbeat, now(), SERVER_FRESH_SECS);
        let mut text = format!(
            "{} is on ({state}), running on {}.\nNeeds you: {needs_you} · follow-ups open: {follow_ups} · messages waiting for your OK: {waiting}.",
            settings.name,
            if on_server { "the server" } else { "this Mac" }
        );
        if !ui.caption.trim().is_empty() {
            text.push_str(&format!("\nLast said: “{}”", clip(&ui.caption, 160)));
        }
        Reply::ok(
            text,
            json!({
                "enabled": true,
                "name": settings.name,
                "initials": settings.initials(),
                "state": state,
                "caption": ui.caption,
                "needs_you": needs_you,
                "follow_ups_open": follow_ups,
                "messages_waiting": waiting,
                "home": home,
                "on_server": on_server,
            }),
        )
    }

    /// `manager follow-up list | add | done | drop`.
    fn manager_follow_up_plan(&mut self, caller: &Caller, args: &Args, name: &str) -> Planned {
        const USAGE: &str = "manager follow-up list [--all] | add \"<what>\" [--person <name>] | done <which> | drop <which>";
        match args.word(2).map(str::to_lowercase).as_deref() {
            None | Some("list" | "ls") => {
                check(args, &["all"])?;
                let data = &self.manager.harness.data;
                let list: Vec<&FollowUp> = if args.has("all") { data.followups.iter().collect() } else { followups::open(data) };
                let text = if list.is_empty() { "No follow-ups are open.".to_string() } else { list.iter().map(|f| follow_up_line(f)).collect::<Vec<_>>().join("\n") };
                answer(Reply::ok(text, Value::Array(list.iter().map(|f| to_json(*f)).collect())))
            }
            Some("add" | "new") => {
                check(args, &["person"])?;
                let what = args.rest(3).trim().to_string();
                if what.is_empty() {
                    return Err(usage(USAGE));
                }
                let person = args.flag("person").unwrap_or_default().to_string();
                let from = source_of(caller);
                plan(Kind::Board, format!("have {name} keep an eye on “{}”", clip(&what, 80)), move |ws, r, _, cx| {
                    let id = followups::add_custom(&mut ws.manager.harness.data, &what, &person, now());
                    ws.manager.harness.dirty = true;
                    cx.notify();
                    ws.reply(r, Reply::ok(format!("{} keeps an eye on it: {what}", ws.state.manager.name), json!({ "id": id, "what": what, "person": person, "from": from })));
                })
            }
            Some(verb @ ("done" | "close" | "drop" | "dropped")) => {
                check(args, &[])?;
                let which = args.rest(3).trim().to_string();
                if which.is_empty() {
                    return Err(usage(USAGE));
                }
                let dropped = verb.starts_with("drop");
                plan(Kind::Board, format!("{} the follow-up “{}”", if dropped { "drop" } else { "close" }, clip(&which, 80)), move |ws, r, _, cx| {
                    let reply = match followups::close(&mut ws.manager.harness.data, &which, dropped, now()) {
                        Some(f) => {
                            ws.manager.harness.dirty = true;
                            cx.notify();
                            let what = if f.number > 0 { format!("{}#{} {}", f.repo, f.number, f.title) } else if f.title.is_empty() { f.note.clone() } else { f.title.clone() };
                            Reply::ok(format!("{}: {}", if dropped { "Dropped" } else { "Done" }, clip(&what, 120)), to_json(&f))
                        }
                        None => failed(format!("No open follow-up matches “{which}”. `glasscode manager follow-up list` shows them.")),
                    };
                    ws.reply(r, reply);
                })
            }
            Some(_) => Err(usage(USAGE)),
        }
    }

    /// `manager message list | draft | approve | reject`. Drafting is all an agent can do.
    fn manager_message_plan(&mut self, caller: &Caller, args: &Args, settings: &Settings) -> Planned {
        const USAGE: &str = "manager message list [--all] | draft --to <person> \"<text>\" [--why \"<why>\"] [--about <ref>] | approve <id> [--text \"<new text>\"] | reject <id>";
        let name = settings.name.clone();
        match args.word(2).map(str::to_lowercase).as_deref() {
            None | Some("list" | "ls") => {
                check(args, &["all"])?;
                let all = args.has("all");
                let list: Vec<&OutMessage> = self.manager.harness.data.outbox.iter().filter(|m| all || !matches!(m.state, MsgState::Sent | MsgState::Rejected)).collect();
                let text = if list.is_empty() { "No messages wait.".to_string() } else { list.iter().map(|m| message_line(m)).collect::<Vec<_>>().join("\n") };
                answer(Reply::ok(text, Value::Array(list.iter().map(|m| to_json(*m)).collect())))
            }
            Some("draft" | "new") => {
                check(args, &["to", "why", "about"])?;
                let body = args.rest(3).trim().to_string();
                let Some(to) = args.flag("to") else { return Err(usage(USAGE)) };
                if body.is_empty() {
                    return Err(usage(USAGE));
                }
                let Some(person) = settings.person(to) else {
                    let known: Vec<&str> = settings.people.iter().map(|p| p.name.as_str()).collect();
                    return Err(failed(format!(
                        "{to} is not one of the teammates in Settings › Manager{}.",
                        if known.is_empty() { " (there are none yet)".to_string() } else { format!(": {}", known.join(", ")) }
                    )));
                };
                let to = person.name.clone();
                let why = args.flag("why").unwrap_or_default().to_string();
                let about = args.flag("about").map(str::to_string);
                plan(Kind::Board, format!("draft a message to {to}: “{}”", clip(&body, 80)), move |ws, r, _, cx| {
                    // Only ever a draft from here: whether it goes is the person's (or their send
                    // setting's) to decide.
                    let settings = ws.state.manager.clone();
                    let message = messages::draft(&mut ws.manager.harness.data, &settings, &to, &body, &why, about.as_deref(), now());
                    ws.manager.harness.dirty = true;
                    cx.notify();
                    let text = match (message.state, message.tier) {
                        (_, Tier::Never) => format!("Kept as a draft for {to}, but {} never sends a message like this one. The person has to write it themselves.", settings.name),
                        (MsgState::Approved | MsgState::Sent, _) => format!("Drafted for {to}. It is a routine ping, so it goes out by itself."),
                        _ => format!("Drafted for {to}. It waits for the person's OK in Glasscode ({}).", short(&message.id)),
                    };
                    ws.reply(r, Reply::ok(text, to_json(&message)));
                })
            }
            Some(verb @ ("approve" | "allow" | "send" | "reject" | "deny")) => {
                let allow = matches!(verb, "approve" | "allow" | "send");
                // Never an agent, never the manager's own brain: the person at a terminal.
                if caller.trust != Trust::You {
                    return Err(denied(if allow { "Only the person can allow a message. It waits for them in Glasscode." } else { "Only the person can reject a message. It waits for them in Glasscode." }));
                }
                check(args, &["text"])?;
                let Some(which) = args.word(3) else { return Err(usage(USAGE)) };
                let message = pick(&self.manager.harness.data.outbox, which, |m| &m.id, |m| format!("{} {}", m.to, m.body), "message")?.clone();
                if !matches!(message.state, MsgState::Draft | MsgState::Failed) {
                    return Err(failed(format!("That message to {} is {} already.", message.to, message_state(&message))));
                }
                if allow && message.tier == Tier::Never {
                    return Err(failed(format!("{name} never sends a message like this one. Write to {} yourself.", message.to)));
                }
                let edited = args.flag("text").map(str::to_string).filter(|_| allow);
                plan(Kind::Board, format!("{} the message to {}", if allow { "allow" } else { "reject" }, message.to), move |ws, r, _, cx| {
                    ws.manager_decide_message(&message.id, allow, edited, cx);
                    let after = ws.manager.harness.data.outbox.iter().find(|m| m.id == message.id).cloned().unwrap_or(message);
                    let text = if allow { format!("Allowed. The message to {} goes out.", after.to) } else { format!("Rejected the message to {}.", after.to) };
                    ws.reply(r, Reply::ok(text, to_json(&after)));
                })
            }
            Some(_) => Err(usage(USAGE)),
        }
    }

    /// `manager show <session> […]` and `manager show --view <screen>`: take the person there.
    fn manager_show_plan(&mut self, caller: &Caller, args: &Args, settings: &Settings) -> Planned {
        const USAGE: &str = "manager show <session> [--tab pr|changes|files] [--section <name>] [--say \"<text>\"]  |  manager show --view manager|sessions|today|tickets|usage|settings [--say \"<text>\"]";
        check(args, &["tab", "section", "say", "view"])?;
        let word = args.rest(2).trim().to_string();
        let say = args.flag("say").map(str::to_string);
        let section = match args.flag("section").map(str::to_lowercase) {
            Some(s) if SECTIONS.contains(&s.as_str()) => Some(s),
            Some(s) => return Err(usage(format!("“{s}” is not a part of the panel: {}.", SECTIONS.join(", ")))),
            None => None,
        };
        let tab = match args.flag("tab") {
            Some(t) => Some(tab_of(t)?),
            None => section.as_deref().and_then(tab_for),
        };
        let (show, what) = match (args.flag("view"), word.is_empty()) {
            (Some(view), true) => {
                let view = view.to_lowercase();
                if !VIEWS.contains(&view.as_str()) {
                    return Err(usage(format!("“{view}” is not a screen: {}.", VIEWS.join(", "))));
                }
                if tab.is_some() || section.is_some() {
                    return Err(usage("--tab and --section point into a session's panel: name a session, not --view."));
                }
                let what = format!("show you {}", if view == "manager" { settings.name.clone() } else { crate::control_ui::capitalize(&view) });
                (Show { session: None, view: Some(view), tab: None, section: None }, what)
            }
            (None, false) => {
                let (id, title) = self.manager_find_session(&word, caller, settings)?;
                (Show { session: Some(id), view: None, tab, section }, format!("show the session “{}”", clip(&title, 60)))
            }
            _ => return Err(usage(USAGE)),
        };
        plan(Kind::Show, what.clone(), move |ws, r, _, cx| {
            let mut actions = vec![Action::Show { show: show.clone() }];
            if let Some(text) = say.clone().filter(|t| !t.trim().is_empty()) {
                actions.push(Action::Say { text, show: Some(show.clone()) });
            }
            ws.manager_apply(actions, cx);
            let text = crate::control_ui::capitalize(&what.replacen("show", "showing", 1));
            ws.reply(r, Reply::ok(format!("{text}."), json!({ "show": to_json(&show), "say": say })));
        })
    }

    /// A session the manager may show: (its id, its title). One on this Mac, or one it sees on
    /// the server; never one in a project it does not manage.
    fn manager_find_session(&self, word: &str, caller: &Caller, settings: &Settings) -> Result<(String, String), Reply> {
        match self.find_session(word, caller) {
            Ok(id) => {
                let Some(session) = self.state.session(&id) else { return Err(failed("That session is gone.")) };
                let title = self.session_title(session);
                if !settings.manages(&session.project_id) {
                    return Err(denied(format!("“{title}” is in a project {} does not manage (Settings › Manager).", settings.name)));
                }
                Ok((id, title))
            }
            Err(not_here) => {
                let snap = self.manager_snapshot();
                let w = word.trim();
                let words: Vec<String> = w.to_lowercase().split_whitespace().map(str::to_string).collect();
                let hits: Vec<_> = snap
                    .sessions
                    .iter()
                    .filter(|s| {
                        let title = s.title.to_lowercase();
                        s.id == w || (w.len() >= 4 && s.id.starts_with(w)) || (!words.is_empty() && words.iter().all(|x| title.contains(x)))
                    })
                    .collect();
                match hits.as_slice() {
                    [one] => Ok((one.id.clone(), one.title.clone())),
                    _ => Err(not_here),
                }
            }
        }
    }

    /// `manager check list | add | remove`: what it looks at by itself, on a schedule.
    fn manager_check_plan(&mut self, args: &Args, name: &str) -> Planned {
        const USAGE: &str = "manager check list | add \"<what>\" --every <minutes> (or --in <minutes>, once) | remove <id>";
        match args.word(2).map(str::to_lowercase).as_deref() {
            None | Some("list" | "ls") => {
                check(args, &[])?;
                let checks = &self.manager.harness.data.checks;
                let text = if checks.is_empty() { format!("{name} checks nothing on a schedule.") } else { checks.iter().map(check_line).collect::<Vec<_>>().join("\n") };
                answer(Reply::ok(text, to_json(checks)))
            }
            Some("add" | "new") => {
                check(args, &["every", "in"])?;
                let what = args.rest(3).trim().to_string();
                if what.is_empty() {
                    return Err(usage(USAGE));
                }
                let (once, word) = match (args.flag("every"), args.flag("in")) {
                    (Some(every), None) => (false, every),
                    (None, Some(after)) => (true, after),
                    _ => return Err(usage(USAGE)),
                };
                let Some(mins) = minutes(word) else { return Err(usage(format!("“{word}” is not a time: a number of minutes, or 30m, 2h, 1d."))) };
                // Each check is a question to the agent: not more often than every five minutes.
                if !once && mins < 5 {
                    return Err(usage("A check runs every 5 minutes at the most."));
                }
                if self.manager.harness.data.checks.len() >= 50 {
                    return Err(failed(format!("{name} has 50 checks already. Remove one first (`glasscode manager check list`).")));
                }
                let when = if once { format!("once, in {}", every(mins * 60).trim_start_matches("every ")) } else { every(mins * 60) };
                plan(Kind::Board, format!("have {name} check “{}” {when}", clip(&what, 80)), move |ws, r, _, cx| {
                    // `last` is now: the first look is one interval from here.
                    let added = Check { id: new_id(), what: what.clone(), every_secs: mins * 60, last: now(), once };
                    let data = to_json(&added);
                    ws.manager.harness.data.checks.push(added);
                    ws.manager.harness.dirty = true;
                    cx.notify();
                    ws.reply(r, Reply::ok(format!("{} checks {when}: {what}", ws.state.manager.name), data));
                })
            }
            Some("remove" | "rm" | "delete") => {
                check(args, &[])?;
                let which = args.rest(3).trim().to_string();
                if which.is_empty() {
                    return Err(usage(USAGE));
                }
                let found = pick(&self.manager.harness.data.checks, &which, |c| &c.id, |c| c.what.clone(), "check")?.clone();
                plan(Kind::Board, format!("stop {name} checking “{}”", clip(&found.what, 80)), move |ws, r, _, cx| {
                    // Dropped for good, so a sync with the server's copy does not bring it back.
                    glasscode_core::manager::store::drop_check(&mut ws.manager.harness.data, &found.id);
                    ws.manager.harness.dirty = true;
                    cx.notify();
                    ws.reply(r, Reply::ok(format!("No longer checked: {}", clip(&found.what, 120)), to_json(&found)));
                })
            }
            Some(_) => Err(usage(USAGE)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Args {
        Args::parse(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>())
    }

    fn caller(trust: Trust, session: Option<&str>) -> Caller {
        Caller { who: "x".into(), trust, channel: glasscode_core::caller::Caller::Ui, session: session.map(str::to_string), session_title: None, agent: None, cwd: None, project: None }
    }

    #[test]
    fn times_read_as_minutes() {
        assert_eq!(minutes("30"), Some(30));
        assert_eq!(minutes("45m"), Some(45));
        assert_eq!(minutes("2h"), Some(120));
        assert_eq!(minutes("1d"), Some(1440));
        assert_eq!(minutes("0"), None);
        assert_eq!(minutes("soon"), None);
        assert_eq!(every(30 * 60), "every 30 minutes");
        assert_eq!(every(3600), "every 1 hour");
        assert_eq!(every(2 * 86_400), "every 2 days");
    }

    #[test]
    fn voice_before_the_question_still_asks_it() {
        let a = args(&["manager", "ask", "--voice", "what is blocked?"]);
        assert_eq!(swallowed(&a, "voice"), Some("what is blocked?"));
        assert!(a.has("voice"));
        let b = args(&["manager", "ask", "what is blocked?", "--voice"]);
        assert_eq!(swallowed(&b, "voice"), None);
        assert!(b.has("voice") && b.rest(2) == "what is blocked?");
        let c = args(&["manager", "ask", "--voice=false", "what is blocked?"]);
        assert_eq!(swallowed(&c, "voice"), None);
        assert!(!c.has("voice"));
    }

    #[test]
    fn who_a_note_comes_from() {
        assert_eq!(source_of(&caller(Trust::You, None)), "you");
        assert_eq!(source_of(&caller(Trust::Session, Some("s1"))), "session:s1");
        assert_eq!(source_of(&caller(Trust::Session, None)), "manager");
        assert!(caller(Trust::Session, None).is_manager());
        assert!(!caller(Trust::Session, Some("s1")).is_manager() && !caller(Trust::You, None).is_manager() && !caller(Trust::Outside, None).is_manager());
    }

    #[test]
    fn tabs_and_sections() {
        assert_eq!(tab_of("pr").ok(), Some(PanelTab::PullRequest));
        assert_eq!(tab_of("Pull-Request").ok(), Some(PanelTab::PullRequest));
        assert_eq!(tab_of("diff").ok(), Some(PanelTab::Changes));
        assert!(tab_of("terminal").is_err());
        assert_eq!(tab_for("checks"), Some(PanelTab::PullRequest));
        assert_eq!(tab_for("files"), Some(PanelTab::Files));
        assert_eq!(tab_for("reply"), None);
    }

    #[test]
    fn picking_by_id_start_or_words() {
        let checks = vec![
            Check { id: "aaaa1111-x".into(), what: "see whether 1111 was merged".into(), ..Check::default() },
            Check { id: "bbbb2222-x".into(), what: "see whether the deploy went out".into(), ..Check::default() },
        ];
        let by = |which: &str| pick(&checks, which, |c| &c.id, |c| c.what.clone(), "check").map(|c| c.id.clone());
        assert_eq!(by("aaaa1111-x").ok().as_deref(), Some("aaaa1111-x"));
        assert_eq!(by("bbbb").ok().as_deref(), Some("bbbb2222-x"));
        assert_eq!(by("deploy").ok().as_deref(), Some("bbbb2222-x"));
        assert!(by("see whether").is_err());
        assert!(by("nothing like it").is_err());
    }

    #[test]
    fn lines_say_where_things_stand() {
        let draft = OutMessage { id: "m1m1m1m1-x".into(), to: "Sam".into(), body: "The fix is pushed.".into(), why: "he asked".into(), ..OutMessage::default() };
        assert_eq!(message_line(&draft), "m1m1m1m1  [waits for the person] to Sam: “The fix is pushed.”  (why: he asked)");
        assert_eq!(message_state(&OutMessage { tier: Tier::Never, ..OutMessage::default() }), "never sent");
        let follow = FollowUp { id: "f1f1f1f1-x".into(), repo: "acme/shop".into(), number: 12, title: "Retry queue".into(), person: "Mira".into(), ..FollowUp::default() };
        assert_eq!(follow_up_line(&follow), "f1f1f1f1  [open] acme/shop#12 “Retry queue”  · Mira");
        let once = Check { id: "c1c1c1c1-x".into(), what: "look at the deploy".into(), every_secs: 1800, once: true, ..Check::default() };
        assert_eq!(check_line(&once), "c1c1c1c1  look at the deploy  · once, 30 minutes after it was added");
    }
}
