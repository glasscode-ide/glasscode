//! The manager's voice in the app: the helper that joins its LiveKit room with the Mac's
//! microphone and speaker. What it hears goes to `manager_ask`; what the manager says goes
//! back to be spoken.
//!
//! The helper is `manager-voice` (`voice/helper`), a small headless process. The app talks
//! to it in JSON lines: it writes `{"say":{"id","text"}}`, `{"stop":true}`
//! and `{"config":{"name","listen"}}` to its stdin, and reads
//! `{"ready":true}`, `{"heard":"…"}`, `{"state":"…"}` and `{"error":"…"}` from its stdout.
//! The helper ends by itself when its stdin closes, so it never outlives the app.
//!
//! With a talker in the worker (`voice/manager/worker/talker.py`) the worker holds the
//! conversation: a sentence comes as `{"heard":"…","handled":true}` (shown, not asked), what the
//! talker says as `{"said":"…"}`, and the talker's tools as `{"tool":{"call","name","arguments"}}`,
//! answered with `{"result":{"call","text"}}`. `ask_manager` asks the manager as a spoken
//! question does, and its answer is spoken as before (`manager_voice_speak`).

use std::fs::File;
use std::io::{BufRead, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::Context;
use glasscode_core::manager::{Listen, Settings, voice};

use crate::workspace::Workspace;

/// How long a token lets the helper (re)join. The room only checks it when joining.
const TOKEN_SECS: u64 = 24 * 60 * 60;
/// After the connection is lost: how often it is tried again, and the first wait.
const RETRIES: u32 = 5;
const RETRY_FIRST: Duration = Duration::from_secs(5);
/// The helper's exit code when the worker reported a failure and closed the room.
const EXIT_WORKER_FAILED: i32 = 5;

/// A line the helper wrote.
enum Line {
    Ready,
    /// What was heard; `true`: the talker answers it, so it is only shown.
    Heard(String, bool),
    State(String),
    Error(String),
    /// What the talker itself said.
    Said(String),
    /// One of the talker's tools, to run here: its call id, name and arguments.
    Tool(String, String, serde_json::Value),
}

/// The helper process and what it last reported.
#[derive(Default)]
pub struct VoiceState {
    /// "idle", "listening", "thinking" or "speaking", as the worker reports it.
    pub state: String,
    /// Why voice is not available, in words for the person (None: it is).
    pub problem: Option<String>,
    /// Whether the Keychain holds the LiveKit key and secret (None: not looked up yet).
    /// Settings › Manager looks when it opens and after Save or Remove, never while drawing.
    pub keys_kept: Option<bool>,
    /// The same for the Soniox key and the talker's key (the voice on this Mac).
    pub soniox_kept: Option<bool>,
    pub talker_kept: Option<bool>,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    /// What the helper wrote, read by a thread; closed once the helper is gone.
    lines: Option<async_channel::Receiver<Line>>,
    /// The helper joined the room.
    ready: bool,
    /// The name, how it listens and where the person is, as last sent.
    sent: Option<(String, &'static str, String)>,
    /// Where the person is (`lens_now`), and when it was last looked at.
    now: String,
    now_at: Option<Instant>,
    /// The key is held (`Listen::Hold`).
    held: bool,
    /// The connection was lost: when to try again, and how often that was tried.
    retry_at: Option<Instant>,
    retries: u32,
    next_say: u64,
    /// The helper is ended with the app (hooked once).
    quit_hooked: bool,
}

impl VoiceState {
    /// The helper runs, or is about to be started again.
    pub fn on(&self) -> bool {
        self.child.is_some() || self.retry_at.is_some()
    }

    fn write(&mut self, line: serde_json::Value) -> bool {
        let Some(stdin) = self.stdin.as_mut() else {
            return false;
        };
        writeln!(stdin, "{line}").and_then(|_| stdin.flush()).is_ok()
    }

    /// Let the helper leave the room and end. Closing its stdin asks it to; it is killed if
    /// it has not gone a moment later.
    fn end(&mut self) {
        self.stdin = None;
        self.lines = None;
        self.ready = false;
        self.sent = None;
        self.held = false;
        self.state = "idle".into();
        if let Some(mut child) = self.child.take() {
            std::thread::spawn(move || {
                for _ in 0..20 {
                    if !matches!(child.try_wait(), Ok(None)) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                let _ = child.kill();
                let _ = child.wait();
            });
        }
    }
}

/// What the helper and the worker are told about listening: "wake" (after the manager's
/// name), "always" (everything counts) or "never" (the microphone is off). The room knows
/// no "hold": while the key is held it is "always", otherwise "never". `Listen::Never` only
/// gets here when the person tapped the orb, which means "listen now": everything counts
/// until it is tapped again. With the voice on this Mac the helper only runs while the person
/// tapped the orb to talk, so there too everything counts (a talk, as on the glasses): the name
/// is not needed in front of every sentence.
fn listen_mode(settings: &Settings, held: bool) -> &'static str {
    match settings.listen {
        Listen::Wake if settings.voice.local => "always",
        Listen::Wake => "wake",
        Listen::Hold if held => "always",
        Listen::Hold => "never",
        Listen::Never => "always",
    }
}

/// What is missing before voice can start, in words that say where to set it.
fn missing(settings: &Settings) -> Option<String> {
    let url = settings.voice.livekit_url.trim();
    if !settings.enabled {
        return Some("The manager is switched off. Switch it on in Settings › Manager.".into());
    }
    if settings.voice.local {
        // The address is this Mac's own (`voice_server.rs`).
        return None;
    }
    if url.is_empty() {
        return Some("No voice server yet. Enter its address in Settings › Manager › Voice.".into());
    }
    if !(url.starts_with("wss://") || url.starts_with("ws://")) {
        return Some("The voice server's address must start with wss://. Change it in Settings › Manager › Voice.".into());
    }
    None
}

/// The `manager-voice` helper: an explicit override, then inside the app bundle
/// (Contents/Resources), then this repository's own builds.
pub(crate) fn helper_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("GLASSCODE_MANAGER_VOICE") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(macos) = exe.parent() {
            let bundled = macos.join("../Resources/manager-voice");
            if bundled.exists() {
                return Some(bundled);
            }
        }
    }
    let build = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../voice/helper/.build");
    ["release", "debug"].iter().map(|profile| build.join(profile).join("manager-voice")).find(|path| path.exists())
}

/// A short id for this run of the helper: every device in the room needs its own.
fn short_id() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    format!("{:06x}", (nanos ^ std::process::id().rotate_left(12)) & 0xff_ffff)
}

/// One line of the helper's stdout.
fn parse(line: &str) -> Option<Line> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if let Some(text) = value.get("heard").and_then(|v| v.as_str()) {
        let text = text.trim();
        let handled = value.get("handled").and_then(|v| v.as_bool()) == Some(true);
        return (!text.is_empty()).then(|| Line::Heard(text.to_string(), handled));
    }
    if let Some(text) = value.get("said").and_then(|v| v.as_str()) {
        let text = text.trim();
        return (!text.is_empty()).then(|| Line::Said(text.to_string()));
    }
    if let Some(tool) = value.get("tool") {
        let call = tool.get("call").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        return (!call.is_empty() && !name.is_empty()).then(|| Line::Tool(call, name, tool.get("arguments").cloned().unwrap_or_default()));
    }
    if let Some(state) = value.get("state").and_then(|v| v.as_str()) {
        return matches!(state, "idle" | "listening" | "thinking" | "speaking").then(|| Line::State(state.to_string()));
    }
    if let Some(error) = value.get("error").and_then(|v| v.as_str()) {
        return Some(Line::Error(error.trim().to_string()));
    }
    (value.get("ready").and_then(|v| v.as_bool()) == Some(true)).then_some(Line::Ready)
}

/// Start the helper in the room at `url`. Err: why not, in words for the person.
fn spawn(settings: &Settings, url: &str) -> Result<(Child, ChildStdin, async_channel::Receiver<Line>), String> {
    if let Some(problem) = missing(settings) {
        return Err(problem);
    }
    let Some((key, secret)) = voice::keys() else {
        return Err("The voice server's key is missing. Enter the LiveKit key and secret in Settings › Manager › Voice.".into());
    };
    let Some(path) = helper_path() else {
        return Err("This build of Glasscode has no voice helper (manager-voice).".into());
    };
    let token = voice::token(&key, &secret, &format!("mac-{}", short_id()), &voice::room(settings), TOKEN_SECS)
        .map_err(|err| format!("The voice server's key cannot be used ({err}). Enter it again in Settings › Manager › Voice."))?;

    let mut cmd = Command::new(&path);
    // The token goes in the environment, not on the command line, where `ps` would show it.
    cmd.env("MANAGER_VOICE_URL", url)
        .env("MANAGER_VOICE_TOKEN", token)
        .env("MANAGER_VOICE_NAME", &settings.name)
        .env("MANAGER_VOICE_LISTEN", listen_mode(settings, false))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    // What the helper logs is kept for looking into a problem; the last run only.
    let dir = glasscode_core::paths::root().join("manager");
    let _ = std::fs::create_dir_all(&dir);
    match File::create(dir.join("voice-helper.log")) {
        Ok(file) => cmd.stderr(file),
        Err(_) => cmd.stderr(Stdio::null()),
    };
    // A dev (.build) binary has a broken @rpath because the repo path contains a space;
    // point the loader at the SwiftPM products dir so it finds the LiveKit frameworks.
    let shown = path.to_string_lossy();
    if shown.contains("/.build/debug/") || shown.contains("/.build/release/") {
        if let Some(build) = path.parent().and_then(|p| p.parent()) {
            let profile = if shown.contains("/release/") { "Release" } else { "Debug" };
            cmd.env("DYLD_FRAMEWORK_PATH", build.join("out/Products").join(profile));
        }
    }

    let mut child = cmd.spawn().map_err(|err| format!("The voice helper could not be started ({err})."))?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("The voice helper could not be started.".into());
    };
    let (tx, rx) = async_channel::unbounded();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(line) = parse(&line) {
                if tx.send_blocking(line).is_err() {
                    break;
                }
            }
        }
        // Dropping `tx` closes the channel: the tick then knows everything was read.
    });
    Ok((child, stdin, rx))
}

impl Workspace {
    /// Start or stop listening (the orb on its page). Sets `manager.live`.
    pub fn manager_voice_toggle(&mut self, cx: &mut Context<Self>) {
        if self.manager.voice.on() {
            self.manager_voice_end();
            self.manager.voice.problem = None;
        } else {
            self.manager.voice.retries = 0;
            self.manager_voice_start(cx);
        }
        cx.notify();
    }

    /// Start the helper. On failure `voice.problem` says what is missing and where to set it.
    fn manager_voice_start(&mut self, cx: &mut Context<Self>) {
        self.manager_voice_end();
        let settings = self.state.manager.clone();
        // The voice on this Mac: its address once it runs; until then, why not yet.
        let url = if settings.voice.local {
            match &self.vserver.status {
                crate::voice_server::Status::Running => self.vserver.url(),
                crate::voice_server::Status::Failed(why) => {
                    self.manager.voice.problem = Some(why.clone());
                    return;
                }
                crate::voice_server::Status::Preparing(what) => {
                    self.manager.voice.problem = Some(what.clone());
                    // Joined as soon as it runs.
                    self.manager.voice.retry_at = Some(Instant::now() + Duration::from_secs(2));
                    return;
                }
                crate::voice_server::Status::Off => {
                    self.manager.voice.problem = Some("The voice on this Mac is starting…".into());
                    self.manager.voice.retry_at = Some(Instant::now() + Duration::from_secs(2));
                    return;
                }
            }
        } else {
            settings.voice.livekit_url.trim().to_string()
        };
        match spawn(&settings, &url) {
            Ok((child, stdin, lines)) => {
                let voice = &mut self.manager.voice;
                voice.child = Some(child);
                voice.stdin = Some(stdin);
                voice.lines = Some(lines);
                voice.problem = None;
                self.manager.live = true;
                if !self.manager.voice.quit_hooked {
                    // However the app quits, the helper leaves the room.
                    self.manager.voice.quit_hooked = true;
                    cx.on_app_quit(|this: &mut Workspace, _| {
                        this.manager_voice_end();
                        async {}
                    })
                    .detach();
                }
                self.manager_voice_configure();
            }
            Err(problem) => {
                log::warn!("manager voice: {problem}");
                self.manager.voice.problem = Some(problem);
            }
        }
    }

    /// End the helper (the manager was switched off, the orb tapped, or the app quits).
    fn manager_voice_end(&mut self) {
        self.manager.voice.end();
        self.manager.voice.retry_at = None;
        self.manager.live = false;
    }

    /// Tell the helper the manager's name and how it listens, when that changed.
    fn manager_voice_configure(&mut self) {
        let want = (self.state.manager.name.clone(), listen_mode(&self.state.manager, self.manager.voice.held), self.manager.voice.now.clone());
        if self.manager.voice.sent.as_ref() == Some(&want) || self.manager.voice.stdin.is_none() {
            return;
        }
        // The conversation's last turns: a talk started again carries on from them (the worker
        // takes them once a talk).
        let recent = recent_turns(&self.mchat.items);
        let voice = &mut self.manager.voice;
        // This app runs the talker's tools (when the worker has a talker).
        voice.write(serde_json::json!({ "config": { "name": want.0, "listen": want.1, "talker": true, "place": "mac", "now": want.2, "recent": recent } }));
        voice.sent = Some(want);
    }

    /// Where the person is, for the talker: looked at each second while the voice is on.
    fn manager_voice_where(&mut self, cx: &mut Context<Self>) {
        if self.manager.voice.now_at.is_some_and(|at| at.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.manager.voice.now_at = Some(Instant::now());
        let now = format!("{} {}", self.lens_now(cx), self.projects_line());
        if now != self.manager.voice.now {
            self.manager.voice.now = now;
            self.manager_voice_configure();
        }
    }

    /// Called from the workspace tick: reads what the helper heard and reported.
    pub fn manager_voice_tick(&mut self, cx: &mut Context<Self>) {
        if !self.manager.voice.on() {
            return;
        }
        if self.manager.voice.stdin.is_some() {
            self.manager_voice_where(cx);
        }
        if !self.state.manager.enabled {
            self.manager_voice_end();
            self.manager.voice.problem = None;
            cx.notify();
            return;
        }
        if self.manager.voice.child.is_none() {
            // The connection was lost a while ago: try again.
            if self.manager.voice.retry_at.is_some_and(|at| at <= Instant::now()) {
                self.manager.voice.retry_at = None;
                self.manager_voice_start(cx);
                // Not when it waits for the voice on this Mac to start (it set its own time).
                if self.manager.voice.child.is_none() && self.manager.voice.retry_at.is_none() {
                    self.manager_voice_lost();
                }
                cx.notify();
            }
            return;
        }
        self.manager_voice_configure();

        let mut heard = Vec::new();
        let mut tools = Vec::new();
        let mut said = Vec::new();
        let mut changed = false;
        let mut all_read = false;
        if let Some(lines) = self.manager.voice.lines.clone() {
            loop {
                match lines.try_recv() {
                    Ok(Line::Ready) => {
                        let voice = &mut self.manager.voice;
                        voice.ready = true;
                        voice.retries = 0;
                        voice.problem = None;
                        changed = true;
                    }
                    Ok(Line::Heard(text, false)) => heard.push(text),
                    // The talker answers it; what it hands on comes as `ask_manager`.
                    Ok(Line::Heard(_, true)) => {}
                    Ok(Line::Said(text)) => said.push(text),
                    Ok(Line::Tool(call, name, arguments)) => tools.push((call, name, arguments)),
                    Ok(Line::State(state)) => {
                        let voice = &mut self.manager.voice;
                        // A state comes from the worker: whatever was wrong, it works again.
                        changed |= voice.state != state || voice.problem.is_some();
                        voice.state = state;
                        voice.problem = None;
                    }
                    Ok(Line::Error(problem)) => {
                        log::warn!("manager voice: {problem}");
                        // The room was made before the voice worker on this Mac signed in, so
                        // LiveKit never sent it in: leave, and join again a moment later (an
                        // empty room closes at once there, and the new one gets the worker).
                        let empty_room = self.state.manager.voice.local && problem.contains("did not join");
                        self.manager.voice.problem = Some(problem);
                        changed = true;
                        if empty_room {
                            self.manager_voice_end();
                            self.manager.voice.retry_at = Some(Instant::now() + Duration::from_secs(3));
                            break;
                        }
                    }
                    Err(async_channel::TryRecvError::Empty) => break,
                    Err(async_channel::TryRecvError::Closed) => {
                        all_read = true;
                        break;
                    }
                }
            }
        }

        // The helper is gone once its output is read to the end and the process has exited.
        let status = self.manager.voice.child.as_mut().and_then(|child| child.try_wait().ok().flatten());
        if let (true, Some(status)) = (all_read, status) {
            let was_ready = self.manager.voice.ready;
            let problem = self.manager.voice.problem.take();
            self.manager.voice.child = None;
            self.manager.voice.end();
            self.manager.live = false;
            self.manager.voice.problem = Some(problem.unwrap_or_else(|| "The voice connection ended.".into()));
            // Try again when a connection that had worked was lost (the Mac slept, the network
            // changed). Not when the worker said what is wrong: trying again changes nothing.
            if status.code() != Some(EXIT_WORKER_FAILED) && (was_ready || self.manager.voice.retries > 0) {
                self.manager_voice_lost();
            }
            changed = true;
        }

        for text in heard {
            self.manager_voice_ask(text, cx);
        }
        for text in said {
            // Shown in the bubble; it was spoken already.
            self.manager_caption(&text, false, cx);
        }
        for (call, name, arguments) in tools {
            let text = self.manager_voice_tool(&name, &arguments, cx);
            self.manager.voice.write(serde_json::json!({ "result": { "call": call, "text": text } }));
        }
        if changed {
            cx.notify();
        }
    }

    /// Ask the manager something said by voice here: its answer is spoken, so it is asked for a
    /// short, plain one (`mac_voice_block`), with where the person is in the window as usual.
    fn manager_voice_ask(&mut self, text: String, cx: &mut Context<Self>) {
        if self.mchat.turn.is_none() && self.mchat.device_focus.is_none() {
            let window = self.chat_focus(false, glasscode_core::clock::now_ms(), cx);
            // A project named by voice is often misheard ("class code"): the projects there are.
            let block = glasscode_core::runtime::chat::mac_voice_block(window.as_deref());
            self.mchat.device_focus = Some(format!("{block}\n[{}]", self.projects_line()));
        }
        self.manager_ask(text, true, cx);
    }

    /// One of the talker's tools, run here; what it found, in words for the talker.
    pub(crate) fn manager_voice_tool(&mut self, name: &str, arguments: &serde_json::Value, cx: &mut Context<Self>) -> String {
        let text = |key: &str| arguments.get(key).and_then(|v| v.as_str()).unwrap_or_default().trim().to_string();
        match name {
            "ask_manager" => {
                let request = text("request");
                if request.is_empty() {
                    return "There was nothing to ask.".into();
                }
                let busy = self.mchat.turn.is_some();
                self.manager_voice_ask(request, cx);
                if busy { "started: it is answered right after the question the manager is on now".into() } else { "started".into() }
            }
            "manager_progress" => match &self.mchat.turn {
                Some(turn) => {
                    let doing = turn.activity.as_deref().map(|a| format!(", now: {a}")).unwrap_or_default();
                    format!("The manager has been working on it for {} seconds{doing}.", turn.running_secs())
                }
                None => "The manager is not working on anything now.".into(),
            },
            "stop_manager" => {
                if self.mchat.turn.is_none() {
                    return "Nothing was running.".into();
                }
                self.chat_stop(cx);
                "Stopped.".into()
            }
            "list_sessions" => {
                // Only what the sidebar holds: a worktree left without a session is not one.
                let needs = arguments.get("needs_you").and_then(|v| v.as_bool()) == Some(true);
                self.voice_sessions(&text("project"), &text("search"), needs)
            }
            "open_place" => {
                // One place everywhere: it opens in the Mac's window, and the lens follows it
                // (`lens_pages`, the door's `place` event).
                let (kind, id) = (text("kind"), text("id"));
                let place = match kind.as_str() {
                    // Its id, the start of it, or the person's words of it (`voice_session_for`).
                    "session" => match self.voice_session_for(&id) {
                        Ok(id) => id,
                        Err(say) => return say,
                    },
                    "pr" => format!("pr:{}", id.trim().trim_start_matches('#').trim()),
                    // A key as heard: "eng 42", "ENG-42".
                    "ticket" => format!("ticket:{}", id.split_whitespace().collect::<Vec<_>>().join("-").to_uppercase()),
                    "board" => "tickets".into(),
                    "today" | "tickets" | "pull-requests" | "usage" | "sessions" | "settings" => kind.clone(),
                    _ => "today".into(),
                };
                if !self.lens_knows(&place) {
                    let what = place.replace("ticket:", "the ticket ").replace("pr:", "pull request #");
                    return format!("Nothing opened: {what} is not on this Mac's board or lists now.");
                }
                let page = self.lens_page(&place, cx);
                let lens = if self.door.presence.values().any(|p| p.glasses && p.lens) { " and the lens" } else { "" };
                self.with_main_window(cx, move |ws, window, cx| {
                    if let Err(why) = ws.lens_open(&place, window, cx) {
                        ws.show_toast(why, cx);
                    }
                });
                let what = if page.eyebrow.is_empty() { page.title.clone() } else { format!("{} \"{}\"", page.eyebrow, page.title) };
                let status = page.status.map(|s| format!(" ({})", s.label.to_lowercase())).unwrap_or_default();
                format!("Shown on the Mac's screen{lens}: {what}{status}.")
            }
            _ => format!("This Mac does not know the tool {name}."),
        }
    }

    /// The connection was lost after it had worked: plan the next try, or give up.
    fn manager_voice_lost(&mut self) {
        let voice = &mut self.manager.voice;
        if voice.retries >= RETRIES {
            voice.retry_at = None;
            return;
        }
        voice.retry_at = Some(Instant::now() + RETRY_FIRST * 2u32.pow(voice.retries));
        voice.retries += 1;
    }

    /// Speak this (no-op while not live). Returns whether it will be spoken.
    pub fn manager_voice_speak(&mut self, text: &str, _cx: &mut Context<Self>) -> bool {
        let text = text.trim();
        let voice = &mut self.manager.voice;
        if text.is_empty() || !voice.ready {
            return false;
        }
        voice.next_say += 1;
        let id = format!("say-{}", voice.next_say);
        voice.write(serde_json::json!({ "say": { "id": id, "text": text } }))
    }

    /// Stop speaking now (the person interrupted).
    pub fn manager_voice_stop(&mut self, _cx: &mut Context<Self>) {
        self.manager.voice.write(serde_json::json!({ "stop": true }));
    }

    /// The talk key went down or up (`Listen::Hold`): while it is held the microphone is on
    /// and everything said counts ("always"); otherwise it is off ("never").
    /// Nothing calls it yet: the key itself belongs to the manager's page.
    #[allow(dead_code)]
    pub fn manager_voice_hold(&mut self, held: bool, _cx: &mut Context<Self>) {
        self.manager.voice.held = held;
        self.manager_voice_configure();
    }
}

/// The conversation's last turns for the talker ([{"who": "person|manager", "text"}]): what the
/// person said and what the manager answered them, newest last.
fn recent_turns(items: &[glasscode_core::conversation::Item]) -> Vec<serde_json::Value> {
    use glasscode_core::conversation::{Author, Kind};
    let turns: Vec<serde_json::Value> = items
        .iter()
        .filter_map(|item| {
            let who = match (&item.author, &item.kind) {
                (Author::Person, Kind::You { .. }) => "person",
                (Author::Chat, Kind::Manager { .. }) => "manager",
                _ => return None,
            };
            let text = glasscode_core::lens::clip(&item.text, 400);
            (!text.is_empty()).then(|| serde_json::json!({ "who": who, "text": text }))
        })
        .collect();
    turns[turns.len().saturating_sub(8)..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(url: &str) -> Settings {
        let mut settings = Settings { enabled: true, name: "AI Mira".into(), ..Settings::default() };
        settings.voice.livekit_url = url.into();
        settings
    }

    #[test]
    fn says_what_is_missing_and_where() {
        let off = Settings::default();
        assert!(missing(&off).is_some_and(|p| p.contains("Settings › Manager")));
        assert!(missing(&settings("")).is_some_and(|p| p.contains("Settings › Manager › Voice")));
        assert!(missing(&settings("voice.example.test")).is_some_and(|p| p.contains("wss://")));
        assert_eq!(missing(&settings(" wss://voice.example.test ")), None);
        assert_eq!(missing(&settings("ws://10.0.0.5:7880")), None);
        // On this Mac: no address to enter.
        let mut local = settings("");
        local.voice.local = true;
        assert_eq!(missing(&local), None);
    }

    #[test]
    fn reads_the_helpers_lines() {
        assert!(matches!(parse(r#"{"ready":true}"#), Some(Line::Ready)));
        assert!(matches!(parse(r#"{"heard":" what is running? "}"#), Some(Line::Heard(text, false)) if text == "what is running?"));
        assert!(matches!(parse(r#"{"heard":"hi","handled":true}"#), Some(Line::Heard(text, true)) if text == "hi"));
        assert!(matches!(parse(r#"{"said":"I'll check."}"#), Some(Line::Said(text)) if text == "I'll check."));
        assert!(matches!(parse(r#"{"tool":{"call":"t1","name":"ask_manager","arguments":{"request":"what needs me"}}}"#), Some(Line::Tool(call, name, args)) if call == "t1" && name == "ask_manager" && args["request"] == "what needs me"));
        assert!(parse(r#"{"tool":{"name":"ask_manager"}}"#).is_none());
        assert!(matches!(parse(r#"{"state":"speaking"}"#), Some(Line::State(state)) if state == "speaking"));
        assert!(matches!(parse(r#"{"error":"The voice server could not be reached."}"#), Some(Line::Error(_))));
        assert!(parse(r#"{"heard":"  "}"#).is_none());
        assert!(parse(r#"{"state":"dancing"}"#).is_none());
        assert!(parse(r#"{"ready":false}"#).is_none());
        assert!(parse("not json").is_none());
    }

    #[test]
    fn how_it_listens() {
        let mut s = settings("wss://voice.example.test");
        assert_eq!(listen_mode(&s, false), "wake");
        assert_eq!(listen_mode(&s, true), "wake");
        // Hold: everything counts while the key is down, the microphone is off otherwise.
        s.listen = Listen::Hold;
        assert_eq!(listen_mode(&s, false), "never");
        assert_eq!(listen_mode(&s, true), "always");
        // Tapped although it never listens by itself: the microphone is open until tapped again.
        s.listen = Listen::Never;
        assert_eq!(listen_mode(&s, false), "always");
        // The voice on this Mac: a tap on the orb is a talk, no name needed.
        s.listen = Listen::Wake;
        s.voice.local = true;
        assert_eq!(listen_mode(&s, false), "always");
    }

    #[test]
    fn every_run_gets_a_short_id() {
        let id = short_id();
        assert_eq!(id.len(), 6);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn nothing_runs_until_started() {
        let mut voice = VoiceState::default();
        assert!(!voice.on());
        assert!(!voice.write(serde_json::json!({ "stop": true })));
        voice.end();
        assert_eq!(voice.state, "idle");
    }
}
