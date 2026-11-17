//! The Chat view: a Claude or Codex session read like the Claude app, while the official CLI
//! keeps running in its terminal underneath (the header's Chat | Terminal switch shows it).
//!
//! What is drawn comes from the agent's own session file (`glasscode_core::chat`). What is sent
//! is typed into the terminal, as a person would: messages, answers to its questions (the
//! keys for an option), Shift-Tab for the mode, Escape to stop it. The mic drives Claude's own
//! voice mode: Glasscode holds the push-to-talk key for as long as it listens (Claude
//! transcribes with the person's own login), then takes the words into the message box.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt as _, AnyElement, AppContext as _, ClickEvent, Context, Entity, FontStyle, FontWeight, HighlightStyle, Hsla, ImageFormat, InteractiveText,
    ObjectFit, ScrollHandle, SharedString, Transformation, UnderlineStyle, Window, div, img, percentage, prelude::*, px,
};
use glasscode_core::agents::AgentActivity;
use glasscode_core::chat::{self, ChatLog, Entry, Line, LineKind, Row, TodoState, Tool, VoiceMode};
use glasscode_core::markdown::{self, Block, Span};
use glasscode_core::model::{AgentKind, Session, SessionView};
use glasscode_core::{mirror, prompt};

use crate::chat_select::Selection;
use crate::composer::{Composer, ComposerEvent};
use crate::icons;
use crate::center::open_url;
use crate::theme;
use crate::workspace::Workspace;

gpui::actions!(chat, [CopySelection, CycleMode]);

pub fn bind_keys(cx: &mut gpui::App) {
    crate::keymap::bind(cx, Some("ChatView"));
}

/// Debug builds' UI tests (`GLASSCODE_DEBUG_CHAT` set): copying logs instead of touching the
/// clipboard of the Mac the test runs on.
pub fn debug_no_clipboard() -> bool {
    cfg!(debug_assertions) && std::env::var_os("GLASSCODE_DEBUG_CHAT").is_some()
}

thread_local! {
    /// Debug builds: the `GLASSCODE_DEBUG_CHAT` steps left, and when the next is due.
    static DEBUG_SCRIPT: std::cell::RefCell<Option<(Vec<String>, Instant)>> = const { std::cell::RefCell::new(None) };
}

/// The widest the conversation gets, as in the Claude app.
const COLUMN: f32 = 780.;
/// Rows drawn before "Show earlier".
const RECENT_ROWS: usize = 160;
/// How often Glasscode presses the push-to-talk key while it listens: faster than Claude's
/// release timer (200 ms), like a held key repeating.
const HOLD_EVERY: Duration = Duration::from_millis(60);

#[derive(Default)]
pub struct ChatUi {
    panes: HashMap<String, ChatPane>,
    /// Sessions shown the other way than Themes › "Sessions open as" says.
    pub views: HashMap<String, SessionView>,
}

pub struct ChatPane {
    log: Option<ChatLog>,
    /// When Glasscode last looked for the session file (it appears with the first message).
    looked_at: Option<Instant>,
    /// Tool rows and folded groups opened by the person.
    open: HashSet<String>,
    scroll: ScrollHandle,
    /// Kept at the newest message (until the person scrolls up).
    follow: bool,
    /// The log generation last drawn.
    seen: u64,
    pub composer: Entity<Composer>,
    /// Decoded images, by their data's address (`None`: could not be read).
    images: HashMap<usize, Option<Arc<gpui::Image>>>,
    /// Show the whole conversation, not only the latest rows.
    all: bool,
    /// Files and images typed into the agent's box, waiting to go with the message.
    attached: usize,
    /// Sent, not in the session file yet (and when).
    pending: Vec<(String, Instant)>,
    voice: Voice,
    /// Text selected in the conversation (the copy key copies it), and the focus that takes that key.
    sel: Selection,
    focus: gpui::FocusHandle,
    /// The terminal's output last drawn from.
    screen_seen: Option<Instant>,
}

#[derive(Clone, Copy)]
enum QuestionKey {
    Up,
    Down,
    Enter,
    Escape,
    Number(u32),
}

enum Voice {
    Off,
    /// Holding Claude's push-to-talk key; Claude writes what it hears into its box, and the
    /// words show in the message box after what was there (`base`) as they come.
    Listening { since: Instant, mode: VoiceMode, heard: String, base: String },
    /// Let go: Claude is writing down the last words.
    Finishing { since: Instant, send: bool, discard: bool, heard: String, steady: Instant, base: String },
}

impl Voice {
    fn active(&self) -> bool {
        !matches!(self, Voice::Off)
    }
}

impl Workspace {
    /// How a session shows now: its own switch, else Themes › "Sessions open as" (a shell is
    /// always its terminal).
    pub fn session_view(&self, session: &Session) -> SessionView {
        if session.agent == AgentKind::Shell {
            return SessionView::Terminal;
        }
        self.chat.views.get(&session.id).copied().unwrap_or(self.state.appearance.session_view)
    }

    pub fn set_session_view(&mut self, id: &str, view: SessionView, window: &mut Window, cx: &mut Context<Self>) {
        if view == self.state.appearance.session_view {
            self.chat.views.remove(id);
        } else {
            self.chat.views.insert(id.to_string(), view);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    /// The session has another agent now (a handoff in the same tab): its message box names it.
    pub fn chat_agent_changed(&mut self, id: &str, cx: &mut Context<Self>) {
        let agent = self.state.session(id).map(|s| s.agent.name()).unwrap_or("the agent");
        if let Some(pane) = self.chat.panes.get(id) {
            pane.composer.update(cx, |c, _| c.set_placeholder(format!("Reply to {agent}…")));
        }
    }

    /// The message box of a session's Chat view (made the first time).
    pub fn chat_composer(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<Composer> {
        self.chat_pane(id, window, cx).composer.clone()
    }

    fn chat_pane(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> &mut ChatPane {
        if !self.chat.panes.contains_key(id) {
            let agent = self.state.session(id).map(|s| s.agent.name()).unwrap_or("the agent");
            let composer = cx.new(|cx| Composer::new(format!("Reply to {agent}…"), cx));
            let session = id.to_string();
            cx.subscribe_in(&composer, window, move |this: &mut Workspace, composer, event: &ComposerEvent, _window, cx| match event {
                ComposerEvent::Submit | ComposerEvent::SubmitNow => {
                    if this.chat.panes.get(&session).is_some_and(|p| matches!(p.voice, Voice::Listening { .. })) {
                        this.voice_stop(&session, true, false, cx);
                    } else if !this.chat_question_key(&session, QuestionKey::Enter, cx) {
                        this.chat_send(&session, cx);
                    }
                }
                ComposerEvent::Cancel => {
                    if this.chat.panes.get(&session).is_some_and(|p| p.voice.active()) {
                        this.voice_stop(&session, false, true, cx);
                    } else if !this.chat_question_key(&session, QuestionKey::Escape, cx) && this.activity.get(&session) == Some(&AgentActivity::Working) {
                        this.chat_interrupt(&session, cx);
                    }
                }
                ComposerEvent::CycleMode => this.chat_cycle_mode(&session, cx),
                ComposerEvent::Up => {
                    this.chat_question_key(&session, QuestionKey::Up, cx);
                }
                ComposerEvent::Down => {
                    this.chat_question_key(&session, QuestionKey::Down, cx);
                }
                ComposerEvent::Changed => {
                    // A number typed while the agent asks picks that option, as in its terminal.
                    let text = composer.read(cx).text();
                    if let Some(n) = text.trim().parse::<u32>().ok().filter(|_| text.trim().len() == 1) {
                        if this.chat_question_key(&session, QuestionKey::Number(n), cx) {
                            composer.update(cx, |c, cx| c.set_text("", cx));
                        }
                    }
                    cx.notify()
                }
                ComposerEvent::PasteFiles(files) => this.chat_attach(&session, files.iter().map(clone_incoming).collect(), cx),
            })
            .detach();
            self.chat.panes.insert(
                id.to_string(),
                ChatPane {
                    log: None,
                    looked_at: None,
                    open: HashSet::new(),
                    scroll: ScrollHandle::new(),
                    follow: true,
                    seen: 0,
                    composer,
                    images: HashMap::new(),
                    all: false,
                    attached: 0,
                    pending: Vec::new(),
                    voice: Voice::Off,
                    sel: Selection::default(),
                    focus: cx.focus_handle(),
                    screen_seen: None,
                },
            );
        }
        self.chat.panes.get_mut(id).unwrap()
    }

    /// Debug builds: `GLASSCODE_DEBUG_CHAT` drives the Chat view of the active session for UI
    /// tests (no keystrokes needed): `wait:ms;type:text;send;voice;answer:N;open;view:terminal`.
    fn chat_debug_step(&mut self, cx: &mut Context<Self>) {
        if !cfg!(debug_assertions) {
            return;
        }
        let script = DEBUG_SCRIPT.with(|s| {
            let mut s = s.borrow_mut();
            if s.is_none() {
                let steps: Vec<String> = std::env::var("GLASSCODE_DEBUG_CHAT").unwrap_or_default().split(';').filter(|x| !x.is_empty()).map(str::to_string).collect();
                *s = Some((steps, Instant::now() + Duration::from_millis(1500)));
            }
            let (steps, due) = s.as_mut().unwrap();
            if steps.is_empty() || Instant::now() < *due {
                return None;
            }
            let step = steps.remove(0);
            if let Some(ms) = step.strip_prefix("wait:").and_then(|m| m.parse().ok()) {
                *due = Instant::now() + Duration::from_millis(ms);
                return None;
            }
            *due = Instant::now() + Duration::from_millis(300);
            Some(step)
        });
        let Some(step) = script else { return };
        let id = self.state.active_session.clone().unwrap_or_default();
        let composer = self.chat.panes.get(&id).map(|p| p.composer.clone());
        match step.split_once(':').unwrap_or((step.as_str(), "")) {
            ("type", text) => {
                if let Some(composer) = composer {
                    composer.update(cx, |c, cx| c.set_text(text.to_string(), cx));
                }
            }
            ("send", _) => self.chat_send(&id, cx),
            ("voice", _) => self.voice_toggle(&id, cx),
            ("drag", points) => {
                // A real mouse drag through the window, from x1,y1 to x2,y2 (window points).
                let n: Vec<f32> = points.split(',').filter_map(|v| v.trim().parse().ok()).collect();
                if let (Some(window), &[x1, y1, x2, y2]) = (self.main_window, n.as_slice()) {
                    cx.defer(move |cx| {
                        let at = |x: f32, y: f32| gpui::point(px(x), px(y));
                        let _ = window.update(cx, |_, window, cx| {
                            window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent { button: gpui::MouseButton::Left, position: at(x1, y1), modifiers: Default::default(), click_count: 1, first_mouse: false }), cx);
                            for i in 1..=10 {
                                let t = i as f32 / 10.;
                                window.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent { position: at(x1 + (x2 - x1) * t, y1 + (y2 - y1) * t), pressed_button: Some(gpui::MouseButton::Left), modifiers: Default::default() }), cx);
                            }
                            window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent { button: gpui::MouseButton::Left, position: at(x2, y2), modifiers: Default::default(), click_count: 1 }), cx);
                        });
                    });
                }
            }
            ("press", keys) => {
                // A real key press through the window, as GPUI writes it (see keymap.rs).
                if let (Some(window), Ok(keystroke)) = (self.main_window, gpui::Keystroke::parse(keys)) {
                    cx.defer(move |cx| {
                        let _ = window.update(cx, |_, window, cx| {
                            window.dispatch_event(gpui::PlatformInput::KeyDown(gpui::KeyDownEvent { keystroke, is_held: false, prefer_character_input: false }), cx);
                        });
                    });
                }
            }
            ("log-focus", _) => {
                if let (Some(window), Some(pane)) = (self.main_window, self.chat.panes.get(&id)) {
                    let (chat, composer) = (pane.focus.clone(), pane.composer.read(cx).focus_handle_clone());
                    cx.defer(move |cx| {
                        let _ = window.update(cx, |_, window, _| eprintln!("[chat-debug] focus: conversation {} · message box {}", chat.is_focused(window), composer.is_focused(window)));
                    });
                }
            }
            ("log-selection", _) => {
                if let Some(pane) = self.chat.panes.get(&id) {
                    eprintln!("[chat-debug] message box selected: {:?} · conversation selected: {:?}", pane.composer.read(cx).selected_text(), pane.sel.copied());
                }
            }
            ("mode", _) => self.chat_cycle_mode(&id, cx),
            ("key", k) => {
                let key = match k { "up" => QuestionKey::Up, "down" => QuestionKey::Down, "enter" => QuestionKey::Enter, "escape" => QuestionKey::Escape, n => QuestionKey::Number(n.parse().unwrap_or(1)) };
                self.chat_question_key(&id, key, cx);
            }
            ("voice-send", _) => self.voice_stop(&id, true, false, cx),
            ("answer", n) => self.chat_answer(&id, n.parse().unwrap_or(1), false, None, cx),
            ("select", range) => {
                // Pieces `a:b`, whole; what the copy key would copy goes to the log (not the clipboard).
                let (a, b) = range.split_once(':').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))).unwrap_or((0, 3));
                if let Some(pane) = self.chat.panes.get_mut(&id) {
                    pane.sel.select_pieces(a, b);
                    eprintln!("[chat-debug] copied: {:?}", pane.sel.copied());
                }
            }
            ("open", _) => {
                if let Some(pane) = self.chat.panes.get_mut(&id) {
                    let ids: Vec<String> = pane.log.iter().flat_map(|l| l.entries.iter()).filter_map(|e| if let Entry::Tool(t) = e { Some(t.id.clone()) } else { None }).collect();
                    pane.open.extend(ids);
                    pane.open.extend((0..400).map(|i| format!("group-{i}")));
                }
            }
            ("view", v) => {
                let view = if v == "terminal" { SessionView::Terminal } else { SessionView::Chat };
                if view == self.state.appearance.session_view { self.chat.views.remove(&id); } else { self.chat.views.insert(id.clone(), view); }
            }
            ("theme", t) => {
                let choice = if t == "dark" { glasscode_core::model::ThemeChoice::Dark } else { glasscode_core::model::ThemeChoice::Light };
                self.set_appearance(|a| a.theme = choice, cx);
            }
            ("font", f) => {
                let font = match f { "serif" => glasscode_core::model::ChatFont::Serif, "system" => glasscode_core::model::ChatFont::System, _ => glasscode_core::model::ChatFont::Ui };
                self.set_appearance(|a| a.chat_font = font, cx);
            }
            _ => {}
        }
        cx.notify();
    }

    /// Follow the session on screen (and any that is listening): read what its agent added.
    pub fn chat_tick(&mut self, cx: &mut Context<Self>) {
        self.chat_debug_step(cx);
        // The agent's live status ("Pondering… 12s") and questions come from its screen:
        // redraw when that changed.
        if let Some(id) = self.state.active_session.clone() {
            let drawn = self.terminals.get(&id).map(|t| t.read(cx).last_output);
            if let (Some(output), Some(pane)) = (drawn, self.chat.panes.get_mut(&id)) {
                if pane.screen_seen != Some(output) {
                    pane.screen_seen = Some(output);
                    cx.notify();
                }
            }
        }
        let Some(id) = self.state.active_session.clone() else { return };
        let Some(session) = self.state.session(&id).cloned() else { return };
        if self.session_view(&session) != SessionView::Chat {
            return;
        }
        let config_dir = session.account_id.as_deref().and_then(|a| self.state.account(a)).map(|a| a.effective_dir()).or_else(|| {
            let home = glasscode_core::paths::home();
            Some(if session.agent == AgentKind::Codex { home.join(".codex") } else { home.join(".claude") })
        });
        let Some(pane) = self.chat.panes.get_mut(&id) else { return };
        // The file to read: as the hooks reported it, or found by the conversation id. It can
        // change (a /clear starts a new conversation).
        let wanted = chat::locate(session.agent, config_dir.as_deref(), session.agent_session_id.as_deref(), session.transcript_path.as_deref());
        let stale = match (&pane.log, &wanted) {
            (Some(log), Some(path)) => &log.path != path,
            (None, Some(_)) => true,
            _ => false,
        };
        if stale && pane.looked_at.is_none_or(|at| at.elapsed() > Duration::from_millis(1500) || pane.log.is_none()) {
            pane.looked_at = Some(Instant::now());
            if let Some(path) = wanted {
                pane.log = Some(ChatLog::open(session.agent, &path));
                pane.follow = true;
            }
        } else if let Some(log) = pane.log.as_mut() {
            log.poll();
        }
        if let Some(log) = &pane.log {
            if log.generation != pane.seen {
                pane.seen = log.generation;
                // Sent messages that are now in the file (taken at once, or later from the queue),
                // or that Claude has accepted into its queue (not stuck — it will run them).
                pane.pending.retain(|(p, _)| {
                    let in_file = log.entries.iter().rev().take(24).any(|e| matches!(e, Entry::User { text, .. } if same_message(text, p)));
                    let queued = log.queued.iter().any(|q| same_message(q, p));
                    !(in_file || queued)
                });
                cx.notify();
            }
        }
    }

    // ----- sending ---------------------------------------------------------------------------

    /// Send what is in the message box: typed into the agent's terminal, then Enter.
    fn chat_send(&mut self, id: &str, cx: &mut Context<Self>) {
        // Talking: stop, let Claude write down the last words, then send it all (the voice
        // steps call this again once the words are in the box).
        if self.chat.panes.get(id).is_some_and(|p| p.voice.active()) {
            self.voice_stop(id, true, false, cx);
            return;
        }
        let Some(terminal) = self.terminals.get(id).cloned() else {
            self.show_toast("This session is not running. Resume it first.".to_string(), cx);
            return;
        };
        let Some(session) = self.state.session(id).cloned() else { return };
        let Some(pane) = self.chat.panes.get(id) else { return };
        let text = pane.composer.read(cx).text();
        let attached = pane.attached;
        if text.trim().is_empty() && attached == 0 {
            return;
        }
        let tail = terminal.read(cx).text_tail(60);
        if prompt::parse(&tail).is_some() {
            self.show_toast(format!("{} is asking something: answer it first (above the message box).", session.agent.name()), cx);
            return;
        }
        // Typed into that question, the message could answer it for the person.
        if self.waits_to_start(id) {
            self.show_toast(format!("{} is asking a question before it starts. Open the Terminal view to answer it.", session.agent.name()), cx);
            return;
        }
        // In the Chat view this box is the message; Claude's own box should be empty. What is
        // left there from dictation goes; anything else joins the message first.
        let in_box = if session.agent == AgentKind::Claude && attached == 0 { claude_box(terminal.read(cx)) } else { String::new() };
        let leftover = !in_box.is_empty();
        if leftover && !squash(&text).contains(&squash(&in_box)) {
            let joined = join_words(&in_box, &text);
            if let Some(pane) = self.chat.panes.get(id) {
                pane.composer.update(cx, |c, cx| c.set_text(joined, cx));
            }
            clear_claude_box(&terminal, cx);
            self.show_toast("Claude's own box had text typed in the Terminal view: it is in your message now. Check it and send.".to_string(), cx);
            return;
        }
        let message = text.trim_end().to_string();
        let claude = session.agent == AgentKind::Claude;
        let sid = id.to_string();
        cx.spawn(async move |this, cx| {
            if leftover {
                let _ = terminal.update(cx, |t, cx| t.write(b"\x0c".to_vec(), cx));
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let _ = terminal.update(cx, |t, cx| erase_box(t, cx));
                cx.background_executor().timer(Duration::from_millis(200)).await;
            }
            // Paste the message into the agent's box, then give it a head start to land (a long
            // one takes longer) before the first Enter.
            let settle = (250 + message.len() as u64 / 4).min(900);
            let _ = terminal.update(cx, |t, cx| {
                if !message.trim().is_empty() {
                    t.paste_text(&message, cx);
                }
            });
            cx.background_executor().timer(Duration::from_millis(settle)).await;
            if !claude {
                // Codex and shells submit on a single Enter.
                let _ = terminal.update(cx, |t, cx| t.write(b"\r".to_vec(), cx));
                return;
            }
            // Claude's TUI is the flaky one: an Enter sent while the paste is still landing, or
            // just as Claude finishes a turn, is silently dropped and the message is left sitting
            // in the box — the person then had to resend by hand. The old watchdog waited on the
            // activity/hook stream, which lags ~10s behind ("Worked for 15s" still reads as
            // Working), so the dropped Enter was not retried for ages. Instead, drive submission
            // off the box itself: wait for the message to show up in the box, press Enter while it
            // is there, and treat the box going empty as sent (or queued). A lost Enter recovers in
            // a fraction of a second, with no dependence on the activity stream.
            let mut seen_in_box = false;
            let mut submitted = false;
            for _ in 0..16 {
                let in_box = terminal.read_with(cx, |t, _| still_in_box(&claude_box(t), &message));
                if in_box {
                    seen_in_box = true;
                    let _ = terminal.update(cx, |t, cx| t.write(b"\r".to_vec(), cx));
                } else if seen_in_box {
                    // The box emptied. One read can come back blank mid-redraw, so confirm it is
                    // really gone before calling it sent; if it reappears, the Enter was lost — press
                    // again.
                    cx.background_executor().timer(Duration::from_millis(150)).await;
                    let still = terminal.read_with(cx, |t, _| still_in_box(&claude_box(t), &message));
                    if still {
                        let _ = terminal.update(cx, |t, cx| t.write(b"\r".to_vec(), cx));
                    } else {
                        submitted = true;
                        break;
                    }
                }
                cx.background_executor().timer(Duration::from_millis(180)).await;
            }
            // The box was never detected (an unusual screen): fall back to a plain Enter so the
            // message is not left unsent.
            if !seen_in_box {
                let _ = terminal.update(cx, |t, cx| t.write(b"\r".to_vec(), cx));
            }
            // Clear the grey "pending" bubble once the message shows up in the transcript or
            // Claude's queue.
            let mut confirmed = false;
            for _ in 0..16 {
                cx.background_executor().timer(Duration::from_millis(600)).await;
                let in_log = this
                    .update(cx, |this, cx| {
                        let Some(pane) = this.chat.panes.get_mut(&sid) else { return true };
                        if let Some(log) = pane.log.as_mut() {
                            log.poll();
                        }
                        let found = pane.log.as_ref().is_some_and(|l| {
                            l.entries.iter().rev().take(24).any(|e| matches!(e, Entry::User { text, .. } if same_message(text, &message)))
                                || l.queued.iter().any(|q| same_message(q, &message))
                        });
                        if found {
                            pane.pending.retain(|(t, _)| !same_message(t, &message));
                            cx.notify();
                        }
                        found
                    })
                    .unwrap_or(false);
                if in_log {
                    confirmed = true;
                    break;
                }
            }
            if confirmed {
                return;
            }
            // Not seen in the transcript. If the box emptied it did send (queued, or the transcript
            // just lags) — clear the grey bubble but leave the text out of the composer so it is not
            // sent twice. If it never left the box it is genuinely stuck: hand it back.
            let _ = this.update(cx, |this, cx| {
                let Some(pane) = this.chat.panes.get_mut(&sid) else { return };
                let Some(i) = pane.pending.iter().position(|(t, _)| same_message(t, &message)) else { return };
                pane.pending.remove(i);
                if !submitted {
                    pane.composer.update(cx, |c, cx| {
                        if c.text().trim().is_empty() {
                            c.set_text(message.clone(), cx);
                        }
                    });
                    this.show_toast("Couldn't confirm that message sent — it's back in the box. Press send again.".to_string(), cx);
                }
                cx.notify();
            });
        })
        .detach();
        let Some(pane) = self.chat.panes.get_mut(id) else { return };
        pane.composer.update(cx, |c, cx| c.set_text("", cx));
        if !text.trim().is_empty() {
            pane.pending.push((text.trim().to_string(), Instant::now()));
        }
        pane.attached = 0;
        pane.follow = true;
        pane.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// Images and files for the message: typed into the agent's box as paths (it attaches them).
    fn chat_attach(&mut self, id: &str, files: Vec<crate::terminal::files::Incoming>, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminals.get(id).cloned() else { return };
        if self.waits_to_start(id) {
            self.show_toast("Claude is asking a question before it starts. Open the Terminal view to answer it, then add the files.".to_string(), cx);
            return;
        }
        let count = files.len();
        terminal.update(cx, |t, cx| t.paste_files(files, cx));
        if let Some(pane) = self.chat.panes.get_mut(id) {
            pane.attached += count;
        }
        cx.notify();
    }

    /// The agent's next mode: Shift-Tab, as in its terminal (the mode pill follows).
    fn chat_cycle_mode(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminals.get(id).cloned() else { return };
        terminal.update(cx, |t, cx| t.write(b"\x1b[Z".to_vec(), cx));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(150)).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    /// Stop the agent (Escape, as in its terminal).
    fn chat_interrupt(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(terminal) = self.terminals.get(id).cloned() {
            terminal.update(cx, |t, cx| t.write(b"\x1b".to_vec(), cx));
        }
    }

    /// A key for the question the agent shows (↑ ↓ to move, Enter to pick, a number, Escape),
    /// sent to its terminal as typed there. False when no question is open.
    fn chat_question_key(&mut self, id: &str, key: QuestionKey, cx: &mut Context<Self>) -> bool {
        let Some(terminal) = self.terminals.get(id).cloned() else { return false };
        let Some(open) = prompt::parse(&terminal.read(cx).text_tail(60)) else { return false };
        let bytes: &'static [u8] = match key {
            QuestionKey::Up => b"\x1b[A",
            QuestionKey::Down => b"\x1b[B",
            QuestionKey::Enter => b"\r",
            QuestionKey::Escape => b"\x1b",
            QuestionKey::Number(n) => {
                let Some(option) = open.options.iter().find(|o| o.number == n) else { return false };
                let wants_text = option.wants_text;
                self.chat_answer(id, n, wants_text, None, cx);
                return true;
            }
        };
        terminal.update(cx, |t, cx| t.write(bytes, cx));
        // The card follows the agent's highlight once it has redrawn.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(120)).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
        true
    }

    /// Pick an option of the question on screen: the same keys a person presses.
    fn chat_answer(&mut self, id: &str, number: u32, wants_text: bool, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminals.get(id).cloned() else { return };
        let Some(open) = prompt::parse(&terminal.read(cx).text_tail(60)) else { return };
        let Some(keys) = prompt::keys_for(&open, number) else { return };
        let mut delay = Duration::ZERO;
        for bytes in keys.iter().filter_map(|k| mirror::key_bytes(k)) {
            let terminal = terminal.clone();
            let at = delay;
            cx.spawn(async move |_, cx| {
                cx.background_executor().timer(at).await;
                let _ = terminal.update(cx, |t, cx| t.write(bytes.to_vec(), cx));
            })
            .detach();
            delay += Duration::from_millis(80);
        }
        if wants_text {
            let agent = self.state.session(id).map(|s| s.agent.name()).unwrap_or("the agent");
            if let Some(pane) = self.chat.panes.get(id) {
                let composer = pane.composer.clone();
                composer.update(cx, |c, _| c.set_placeholder(format!("Tell {agent} what to do instead…")));
                if let Some(window) = window {
                    let handle = composer.read(cx).focus_handle_clone();
                    window.focus(&handle, cx);
                }
            }
        }
        cx.notify();
    }

    // ----- voice -----------------------------------------------------------------------------

    /// The mic: start listening, or stop and keep the words.
    fn voice_toggle(&mut self, id: &str, cx: &mut Context<Self>) {
        let listening = self.chat.panes.get(id).is_some_and(|p| matches!(p.voice, Voice::Listening { .. }));
        if listening {
            self.voice_stop(id, false, false, cx);
        } else if !self.chat.panes.get(id).is_some_and(|p| p.voice.active()) {
            self.voice_start(id, cx);
        }
    }

    fn voice_start(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(session) = self.state.session(id).cloned() else { return };
        let Some(terminal) = self.terminals.get(id).cloned() else { return };
        let config = session.account_id.as_deref().and_then(|a| self.state.account(a)).map(|a| a.effective_dir()).unwrap_or_else(|| glasscode_core::paths::home().join(".claude"));
        let Some(mode) = chat::claude_voice(&config) else {
            self.show_toast(format!("Claude's voice mode is off. Type /voice in this session to turn it on (it uses your Claude login and the {}'s microphone), then click the mic again.", crate::platform::computer()), cx);
            return;
        };
        let tail = terminal.read(cx).text_tail(60);
        if prompt::parse(&tail).is_some() {
            self.show_toast("Claude is asking something: answer it first.".to_string(), cx);
            return;
        }
        if self.waits_to_start(id) {
            self.show_toast("Claude is asking a question before it starts. Open the Terminal view to answer it.".to_string(), cx);
            return;
        }
        // Voice starts on an empty box: what is there moves into the message, then it starts.
        let in_box = claude_box(terminal.read(cx));
        if !in_box.is_empty() {
            if let Some(pane) = self.chat.panes.get(id) {
                let text = pane.composer.read(cx).text();
                if !squash(&text).contains(&squash(&in_box)) {
                    let joined = join_words(&text, &in_box);
                    pane.composer.update(cx, |c, cx| c.set_text(joined, cx));
                }
            }
            clear_claude_box(&terminal, cx);
            let id = id.to_string();
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(700)).await;
                let _ = this.update(cx, |this, cx| {
                    if this.terminals.get(&id).is_some_and(|t| claude_box(t.read(cx)).is_empty()) {
                        this.voice_start(&id, cx);
                    }
                });
            })
            .detach();
            return;
        }
        terminal.update(cx, |t, cx| {
            t.last_typed = Some(Instant::now());
            t.write(b" ".to_vec(), cx)
        });
        let Some(pane) = self.chat.panes.get_mut(id) else { return };
        let base = pane.composer.read(cx).text();
        pane.voice = Voice::Listening { since: Instant::now(), mode, heard: String::new(), base };
        // Hold the key (hold mode), and watch what Claude hears, until told to stop.
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(HOLD_EVERY).await;
                let go_on = this.update(cx, |this, cx| this.voice_step(&id, cx)).unwrap_or(false);
                if !go_on {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// One beat while listening or finishing; false when done.
    fn voice_step(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let Some(terminal) = self.terminals.get(id).cloned() else {
            if let Some(pane) = self.chat.panes.get_mut(id) {
                pane.voice = Voice::Off;
            }
            return false;
        };
        let in_box = claude_box(terminal.read(cx));
        let Some(pane) = self.chat.panes.get_mut(id) else { return false };
        // What to do once the pane is let go of.
        let mut stop = false;
        let mut done: Option<(String, bool, bool, String)> = None;
        // The words so far, shown in the message box as if typed.
        let mut show: Option<String> = None;
        match &mut pane.voice {
            Voice::Off => return false,
            Voice::Listening { since, mode, heard, base } => {
                if *mode == VoiceMode::Hold {
                    terminal.update(cx, |t, cx| t.write(b" ".to_vec(), cx));
                }
                if in_box != *heard {
                    *heard = in_box;
                    show = Some(join_words(base, heard));
                }
                // Claude's own cap is two minutes; stop holding a little before.
                stop = since.elapsed() > Duration::from_secs(115);
            }
            Voice::Finishing { since, send, discard, heard, steady, base } => {
                if in_box != *heard {
                    *heard = in_box;
                    *steady = Instant::now();
                    show = Some(join_words(base, heard));
                }
                // Done when the words have stopped changing for a moment (Claude transcribes
                // the last bit after the key is let go), or nothing came at all.
                let settled = !heard.is_empty() && since.elapsed() > Duration::from_millis(700) && steady.elapsed() > Duration::from_millis(900);
                if settled || since.elapsed() > Duration::from_secs(12) {
                    done = Some((heard.trim().to_string(), *send, *discard, base.clone()));
                }
            }
        }
        if let Some(text) = show {
            pane.composer.update(cx, |c, cx| c.set_text(text, cx));
        }
        if stop {
            self.voice_stop(id, false, false, cx);
        }
        let Some((words, send, discard, base)) = done else {
            cx.notify();
            return true;
        };
        let Some(pane) = self.chat.panes.get_mut(id) else { return false };
        pane.voice = Voice::Off;
        // The words stay in Glasscode's box (or go, when cancelled); Claude's own box is emptied.
        if !words.is_empty() {
            clear_claude_box(&terminal, cx);
        }
        let text = if discard { base } else { join_words(&base, &words) };
        pane.composer.update(cx, |c, cx| c.set_text(text, cx));
        if send && !discard && !words.is_empty() {
            self.chat_send(id, cx);
        }
        cx.notify();
        false
    }

    /// Stop listening: keep the words in the message box (`send`: and send them; `discard`:
    /// throw them away).
    fn voice_stop(&mut self, id: &str, send: bool, discard: bool, cx: &mut Context<Self>) {
        let Some(pane) = self.chat.panes.get_mut(id) else { return };
        if let Voice::Listening { mode, heard, base, .. } = &pane.voice {
            if *mode == VoiceMode::Tap {
                // Tap mode: a second tap stops it, and Claude sends the words itself.
                if let Some(terminal) = self.terminals.get(id).cloned() {
                    terminal.update(cx, |t, cx| t.write(b" ".to_vec(), cx));
                }
                let base = base.clone();
                pane.composer.update(cx, |c, cx| c.set_text(base, cx));
                pane.voice = Voice::Off;
                cx.notify();
                return;
            }
            pane.voice = Voice::Finishing { since: Instant::now(), send, discard, heard: heard.clone(), steady: Instant::now(), base: base.clone() };
        } else if let Voice::Finishing { send: s, discard: d, .. } = &mut pane.voice {
            *s |= send;
            *d |= discard;
        }
        cx.notify();
    }

    // ----- drawing ---------------------------------------------------------------------------

    /// The Chat view: the conversation in a column of strong glass with the session's header
    /// (`top`) above it and `ended` (the agent exited) below it; then what waits for an answer
    /// and the message box, on glass of its own.
    pub fn render_chat(&mut self, session: &Session, top: AnyElement, ended: Option<AnyElement>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let id = session.id.clone();
        let composer = self.chat_composer(&id, window, cx);
        let terminal = self.terminals.get(&id).cloned();
        let tail = terminal.as_ref().map(|t| t.read(cx).text_tail(60)).unwrap_or_default();
        let question = prompt::parse(&tail);
        // While the agent asks, ↑ ↓ ⏎ in the message box answer it, whatever is typed there;
        // and the keyboard comes back here from the hidden terminal (or from nowhere).
        let asking = question.is_some();
        if composer.read(cx).arrows_out != asking {
            composer.update(cx, |c, _| c.arrows_out = asking);
        }
        let focused = window.focused(cx);
        let lost = match &focused {
            None => true,
            Some(handle) => *handle == self.focus || terminal.as_ref().is_some_and(|t| t.read(cx).focus_handle_clone() == *handle),
        };
        if lost && self.palette.is_none() && self.modal.is_none() && window.is_window_active() {
            let handle = composer.read(cx).focus_handle_clone();
            window.focus(&handle, cx);
        }
        let working = self.activity.get(&id) == Some(&AgentActivity::Working) || (question.is_none() && status_line(&tail).is_some());
        // Claude asks something before it starts that the screen gives no card for.
        let starting = self.waits_to_start(&id) && question.is_none() && !prompt::has_live_menu(&tail);
        let status = status_line(&tail);
        let mode = mode_line(&tail);
        // Only Claude writes this file. A session handed from Claude to Codex still has it, and
        // would show Claude's model and context for Codex.
        let limits = (session.agent == AgentKind::Claude)
            .then(|| std::fs::read(glasscode_core::usage::limits_file(&id)).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()))
            .flatten();
        let ws = cx.entity().downgrade();
        let pane = self.chat.panes.get_mut(&id).unwrap();
        // Stay at the newest message unless the person scrolled up (back at the bottom, it
        // follows again).
        let (max, offset) = (pane.scroll.max_offset().y, -pane.scroll.offset().y);
        if !pane.follow && offset >= max - px(4.) {
            pane.follow = true;
        }
        if pane.follow {
            pane.scroll.scroll_to_bottom();
        }
        let following = pane.follow;
        let model = limits
            .as_ref()
            .and_then(|l| l["model"]["display_name"].as_str().map(str::to_string))
            .or_else(|| pane.log.as_ref().and_then(|l| l.model.as_deref()).map(chat::model_label));
        let context = limits.as_ref().and_then(|l| l["context_window"]["used_percentage"].as_f64()).or_else(|| pane.log.as_ref().and_then(|l| l.context_percent));
        let agent = session.agent;
        let mut items: Vec<AnyElement> = Vec::new();
        let ChatPane { log, open, images, all, pending, sel, .. } = pane;
        sel.begin();
        match log.as_ref() {
            None if starting => items.push(startup_notice(&id, agent, cx)),
            None => items.push(
                div()
                    .py(theme::sz(60.))
                    .flex()
                    .justify_center()
                    .text_color(theme::card_soft_text())
                    .child(if terminal.is_some() {
                        format!("Your conversation with {} shows here. Write below to start.", agent.name())
                    } else {
                        "Resume the session to continue the conversation.".to_string()
                    })
                    .into_any_element(),
            ),
            Some(log) => {
                let rows = chat::rows(&log.entries);
                let hidden = if *all { 0 } else { rows.len().saturating_sub(RECENT_ROWS) };
                if hidden > 0 || log.truncated {
                    let label = if hidden > 0 { format!("Show {hidden} earlier") } else { "Earlier messages: see the Terminal view".to_string() };
                    let sid = id.clone();
                    items.push(
                        div()
                            .flex()
                            .justify_center()
                            .child(
                                div()
                                    .id("chat-earlier")
                                    .px_3()
                                    .py_1()
                                    .rounded_full()
                                    .text_size(theme::sz(12.))
                                    .text_color(theme::card_soft_text())
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme::tint()).text_color(theme::text()))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                        if hidden > 0 {
                                            if let Some(p) = this.chat.panes.get_mut(&sid) {
                                                p.all = true;
                                            }
                                            cx.notify();
                                        } else {
                                            this.set_session_view(&sid, SessionView::Terminal, window, cx);
                                        }
                                    }))
                                    .child(label),
                            )
                            .into_any_element(),
                    );
                }
                // A Codex turn that ended without a reply: a line where the reply would be.
                let mut unanswered = None;
                if crate::platform::chat_status_fixes() && agent == AgentKind::Codex {
                    let shown_from = rows.get(hidden).map(first_entry);
                    unanswered = log.unanswered.filter(|at| shown_from.is_none_or(|from| *at >= from));
                }
                for row in rows.into_iter().skip(hidden) {
                    if unanswered.is_some_and(|at| first_entry(&row) >= at) {
                        items.push(unanswered_notice(&id, sel, cx));
                        unanswered = None;
                    }
                    items.push(render_row(&row, log, open, images, sel, &id, ws.clone(), cx));
                }
                if unanswered.is_some() {
                    items.push(unanswered_notice(&id, sel, cx));
                }
                if starting {
                    items.push(startup_notice(&id, agent, cx));
                }
            }
        }
        // Sent and not in the conversation yet: queued (Claude reads it after its current step),
        // on its way, or, after a while, apparently not taken: send again or let it go.
        let queued: Vec<String> = log.as_ref().map(|l| l.queued.clone()).unwrap_or_default();
        for (i, (text, at)) in pending.iter().enumerate() {
            let in_queue = queued.iter().any(|q| same_message(q, text));
            // While the agent works a message can wait (Codex keeps no record of its queue).
            let late = !in_queue && !working && at.elapsed() > Duration::from_secs(30);
            items.push(user_bubble(text, &[], None, images, sel, true));
            let note = if in_queue {
                Some(div().child(format!("Queued · {} reads it after its current step", agent.name())).into_any_element())
            } else if late {
                let (again_id, drop_id) = (id.clone(), id.clone());
                let again_text = text.clone();
                Some(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(format!("{} has not taken this yet", agent.name()))
                        .child(div().id(SharedString::from(format!("pending-again-{i}"))).text_color(theme::link()).cursor_pointer().child("Send again").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if let Some(pane) = this.chat.panes.get_mut(&again_id) {
                                pane.pending.retain(|(p, _)| p != &again_text);
                                let composer = pane.composer.clone();
                                composer.update(cx, |c, cx| c.set_text(again_text.clone(), cx));
                            }
                            this.chat_send(&again_id, cx);
                        })))
                        .child(div().id(SharedString::from(format!("pending-drop-{i}"))).text_color(theme::card_soft_text()).cursor_pointer().child("Dismiss").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if let Some(pane) = this.chat.panes.get_mut(&drop_id) {
                                if i < pane.pending.len() {
                                    pane.pending.remove(i);
                                }
                            }
                            cx.notify();
                        })))
                        .into_any_element(),
                )
            } else {
                None
            };
            if let Some(note) = note {
                items.push(div().flex().justify_end().mt(theme::sz(-8.)).font_family(theme::UI_FONT).text_size(theme::sz(12.)).text_color(theme::card_soft_text()).child(note).into_any_element());
            }
        }
        // Working: Claude's own words for it ("Pondering… 12s"), and a way to stop it.
        if working && question.is_none() {
            let sid = id.clone();
            items.push(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_color(theme::card_soft_text())
                    .child(spinner("chat-working", theme::sz(15.), theme::agent_color(agent)))
                    .child(div().child(status.clone().unwrap_or_else(|| "Working…".into())))
                    .child(
                        div()
                            .id("chat-stop")
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .text_size(theme::sz(12.))
                            .text_color(theme::card_soft_text())
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::tint()).text_color(theme::text()))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.chat_interrupt(&sid, cx)))
                            .child(icons::icon(icons::STOP, theme::sz(10.), theme::sz(10.), theme::card_soft_text()))
                            .child("Stop · esc"),
                    )
                    .into_any_element(),
            );
        }
        let pane = self.chat.panes.get(&id).unwrap();
        let card = question.map(|q| self.render_question(&id, agent, q, pane.log.as_ref().and_then(|l| l.pending_tool()).cloned(), cx));
        // Safety net: a selection menu is on screen that we could not turn into buttons.
        // Rather than look frozen, tell the person and give one click to the terminal.
        let waiting = (card.is_none() && !working && terminal.is_some() && prompt::has_live_menu(&tail))
            .then(|| self.render_needs_input(&id, agent, window, cx));
        // What this session's agent asked Glasscode to do and waits for the person to allow.
        let asks: Vec<AnyElement> = self.control.pending_for(&id).into_iter().map(|a| crate::today_ui::render_approval(a, true, cx)).collect();
        let footer = self.render_composer(&id, agent, &composer, mode, model, context, terminal.is_some(), cx);
        let scroll = pane.scroll.clone();
        let (wheel_id, latest_id) = (id.clone(), id.clone());
        let (down_id, move_id, up_id, copy_id, key_id) = (id.clone(), id.clone(), id.clone(), id.clone(), id.clone());
        let focus = pane.focus.clone();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(theme::sz(10.))
            .child(
                crate::backdrop::glass_strong(theme::sz(14.))
                    .when(theme::is_light(), |d| d.shadow_md())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(top)
                    .child(
                    div()
                        .id(SharedString::from(format!("chat-{id}")))
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&scroll)
                        // Select text like in a terminal: drag, double-click a word, the copy key.
                        .key_context("ChatView")
                        .track_focus(&focus)
                        .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            if let Some(pane) = this.chat.panes.get_mut(&down_id) {
                                pane.sel.press(event.position, event.click_count, event.modifiers.shift);
                                let handle = pane.focus.clone();
                                window.focus(&handle, cx);
                            }
                            cx.notify();
                        }))
                        .on_mouse_move(cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                            if event.pressed_button == Some(gpui::MouseButton::Left) {
                                if let Some(pane) = this.chat.panes.get_mut(&move_id) {
                                    if pane.sel.drag(event.position) {
                                        cx.notify();
                                    }
                                }
                            }
                        }))
                        .on_mouse_up(gpui::MouseButton::Left, cx.listener({
                            let id = up_id.clone();
                            move |this, _: &gpui::MouseUpEvent, _, _| {
                                if let Some(pane) = this.chat.panes.get_mut(&id) {
                                    pane.sel.dragging = false;
                                }
                            }
                        }))
                        .on_mouse_up_out(gpui::MouseButton::Left, cx.listener(move |this, _: &gpui::MouseUpEvent, _, _| {
                            if let Some(pane) = this.chat.panes.get_mut(&up_id) {
                                pane.sel.dragging = false;
                            }
                        }))
                        .on_action(cx.listener({
                            let id = copy_id.clone();
                            move |this, _: &CycleMode, _, cx| this.chat_cycle_mode(&id, cx)
                        }))
                        .on_action(cx.listener(move |this, _: &CopySelection, _, cx| {
                            if debug_no_clipboard() {
                                eprintln!("[chat-debug] conversation copies: {:?}", this.chat.panes.get(&copy_id).and_then(|p| p.sel.copied()));
                                return;
                            }
                            if let Some(text) = this.chat.panes.get(&copy_id).and_then(|p| p.sel.copied()) {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                            }
                        }))
                        // Typing while the conversation has the focus goes to the message box.
                        .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                            let keys = &event.keystroke;
                            // AltGr on Windows comes as Ctrl+Alt with a character of the layout, which is typed.
                            if !event.prefer_character_input && (keys.modifiers.platform || keys.modifiers.control || keys.modifiers.function) {
                                return;
                            }
                            let question = match keys.key.as_str() {
                                "up" => Some(QuestionKey::Up),
                                "down" => Some(QuestionKey::Down),
                                "enter" => Some(QuestionKey::Enter),
                                "escape" => Some(QuestionKey::Escape),
                                _ => None,
                            };
                            if let Some(key) = question {
                                this.chat_question_key(&key_id, key, cx);
                                return;
                            }
                            let Some(pane) = this.chat.panes.get_mut(&key_id) else { return };
                            let composer = pane.composer.clone();
                            let handle = composer.read(cx).focus_handle_clone();
                            window.focus(&handle, cx);
                            if let Some(text) = keys.key_char.clone().filter(|t| !t.chars().any(char::is_control)) {
                                composer.update(cx, |c, cx| c.insert(&text, window, cx));
                            }
                        }))
                        .on_scroll_wheel(cx.listener(move |this, event: &gpui::ScrollWheelEvent, _, cx| {
                            if event.delta.pixel_delta(px(20.)).y > px(0.) {
                                if let Some(pane) = this.chat.panes.get_mut(&wheel_id) {
                                    pane.follow = false;
                                }
                                cx.notify();
                            }
                        }))
                        .child(
                            div().flex().justify_center().px(theme::sz(16.)).child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .w_full()
                                    .max_w(theme::sz(COLUMN))
                                    .pt(theme::sz(22.))
                                    .pb(theme::sz(18.))
                                    .gap(theme::sz(14.))
                                    .font_family(SharedString::new_static(theme::chat_font_family()))
                                    .text_size(px(theme::chat_text_size()))
                                    .line_height(px(theme::chat_text_size() * 1.6))
                                    .text_color(theme::text())
                                    .children(items)
                                    // What waits for the person's answer ends the conversation,
                                    // as in the Claude app: under the message box it would take
                                    // the height of the column in a small window.
                                    .when(!asks.is_empty() || card.is_some() || waiting.is_some(), |d| {
                                        d.child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .gap_2()
                                                .font_family(theme::UI_FONT)
                                                .text_size(px(theme::ui_text_size()))
                                                .line_height(gpui::phi())
                                                .children(asks)
                                                .children(card)
                                                .children(waiting),
                                        )
                                    }),
                            ),
                        ),
                    )
                    .children(ended),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .justify_center()
                    // Scrolled up: a way back to the newest message.
                    .when(!following, |d| {
                        d.child(
                            div().absolute().top(theme::sz(-40.)).left_0().right_0().flex().justify_center().child(
                                div()
                                    .id("chat-latest")
                                    .px_3()
                                    .py_1()
                                    .rounded_full()
                                    .border_1()
                                    .border_color(theme::border_strong())
                                    .bg(theme::raised())
                                    .shadow_sm()
                                    .font_family(theme::UI_FONT)
                                    .text_size(theme::sz(12.5))
                                    .text_color(theme::text_soft())
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme::hover()))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        if let Some(pane) = this.chat.panes.get_mut(&latest_id) {
                                            pane.follow = true;
                                            pane.scroll.scroll_to_bottom();
                                        }
                                        cx.notify();
                                    }))
                                    .child("↓ Latest"),
                            ),
                        )
                    })
                    .child(div().w_full().max_w(theme::sz(COLUMN)).child(footer)),
            )
            .into_any_element()
    }

    /// The question the agent is asking on screen, as buttons.
    fn render_question(&self, id: &str, agent: AgentKind, q: prompt::PendingPrompt, tool: Option<Tool>, cx: &mut Context<Self>) -> AnyElement {
        let title = match (&q.title, &tool) {
            (Some(title), _) => format!("{} · {}", agent.name(), title),
            (None, Some(tool)) => format!("{} wants to {}", agent.name(), tool.verb.to_lowercase()),
            (None, None) => format!("{} is asking", agent.name()),
        };
        let detail: Vec<String> = if q.detail.is_empty() { tool.map(|t| vec![t.subject]).unwrap_or_default() } else { q.detail.clone() };
        let options = q.options.iter().enumerate().map(|(i, o)| {
            let (number, wants_text) = (o.number, o.wants_text);
            // The option the agent has highlighted (↑ and ↓ move it, Enter picks it).
            let first = i == q.selected;
            let sid = id.to_string();
            div()
                .id(SharedString::from(format!("chat-answer-{number}")))
                .flex()
                .items_center()
                .gap_2p5()
                .min_h(theme::sz(36.))
                .px_3()
                .py_1p5()
                .rounded(theme::sz(9.))
                .border_1()
                .border_color(if first { theme::accent() } else { theme::border() })
                .bg(if first { theme::accent() } else { theme::raised() })
                .text_color(if first { theme::on_accent() } else { theme::text() })
                .font_weight(if first { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                .cursor_pointer()
                .hover(move |s| if first { s.opacity(0.9) } else { s.bg(theme::hover()) })
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.chat_answer(&sid, number, wants_text, Some(window), cx)))
                .child(
                    div()
                        .flex_none()
                        .size(theme::sz(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(theme::sz(6.))
                        .border_1()
                        .border_color(if first { theme::on_accent() } else { theme::border() })
                        .font_family(theme::MONO_FONT)
                        .text_size(theme::sz(11.5))
                        .child(number.to_string()),
                )
                .child(div().flex_1().min_w_0().child(o.label.clone()))
        });
        div()
            .flex()
            .flex_col()
            .gap_2p5()
            .p_4()
            .rounded(theme::sz(14.))
            .border_2()
            .border_color(theme::attention())
            .bg(theme::raised())
            .font_family(theme::UI_FONT)
            .text_size(theme::sz(14.))
            .child(div().flex().items_center().gap_2().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(div().size(theme::sz(9.)).rounded_full().bg(theme::attention())).child(title))
            .when(!detail.is_empty(), |d| {
                d.child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded(theme::sz(9.))
                        .bg(theme::chat_code_bg())
                        .font_family(theme::MONO_FONT)
                        .text_size(theme::sz(12.5))
                        .text_color(theme::text_strong())
                        .children(detail.into_iter().map(|l| div().child(l))),
                )
            })
            .child(div().text_color(theme::text_soft()).child(q.question.clone()))
            .child(div().flex().flex_col().gap_1p5().children(options))
            .child(div().text_size(theme::sz(12.)).text_color(theme::faint()).child(format!("↑ ↓ to choose · ⏎ to pick · or press 1–{}", q.options.len())))
            .into_any_element()
    }

    /// A prompt is on screen that we could not render as buttons: say so plainly and give
    /// one click to the terminal, so chat never just looks frozen.
    fn render_needs_input(&self, id: &str, agent: AgentKind, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let sid = id.to_string();
        div()
            .flex()
            .flex_col()
            .gap_2p5()
            .p_4()
            .rounded(theme::sz(14.))
            .border_2()
            .border_color(theme::attention())
            .bg(theme::raised())
            .font_family(theme::UI_FONT)
            .text_size(theme::sz(14.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_strong())
                    .child(div().size(theme::sz(9.)).rounded_full().bg(theme::attention()))
                    .child(format!("{} is waiting for your answer", agent.name())),
            )
            .child(div().text_color(theme::text_soft()).child("This prompt needs the terminal. Open it to respond, then come back to chat."))
            .child(
                div()
                    .id("needs-input-open-terminal")
                    .self_start()
                    .px_3()
                    .py_1p5()
                    .rounded(theme::sz(9.))
                    .border_1()
                    .border_color(theme::accent())
                    .text_color(theme::accent())
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.set_session_view(&sid, SessionView::Terminal, window, cx);
                    }))
                    .child("Open Terminal"),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_composer(&self, id: &str, agent: AgentKind, composer: &Entity<Composer>, mode: Option<String>, model: Option<String>, context: Option<f64>, running: bool, cx: &mut Context<Self>) -> AnyElement {
        let pane = self.chat.panes.get(id);
        let empty = composer.read(cx).text().trim().is_empty() && pane.is_none_or(|p| p.attached == 0);
        let attached = pane.map(|p| p.attached).unwrap_or(0);
        let listening = pane.is_some_and(|p| matches!(p.voice, Voice::Listening { .. }));
        let (send_id, mic_id, attach_id, mode_id, model_id) = (id.to_string(), id.to_string(), id.to_string(), id.to_string(), id.to_string());
        let button = |key: &str| div().id(SharedString::from(format!("chat-{key}"))).flex_none().size(theme::sz(32.)).flex().items_center().justify_center().rounded(theme::sz(10.)).cursor_pointer().hover(|s| s.bg(theme::tint()));
        // The text on a line of its own, and under it a bar of the box's buttons and what tells
        // the mode, the context and the model, as in the prototype. The bar wraps in a narrow
        // centre, so the text keeps its width.
        let input = div()
            .flex()
            .flex_col()
            .min_w_0()
            .px(theme::sz(6.))
            .pt(theme::sz(4.))
            .font_family(theme::UI_FONT)
            .text_size(theme::sz(15.))
            .line_height(theme::sz(21.))
            .when(attached > 0, |d| d.child(div().pb_1().text_size(theme::sz(12.)).text_color(theme::card_soft_text()).child(format!("{} attached · goes with your message", plural(attached, "file")))))
            .child(composer.clone());
        let meter = context.map(|p| {
            let full = p >= 80.;
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(format!("Context {}%", p.round() as u32))
                .child(div().w(theme::sz(34.)).h(theme::sz(6.)).rounded_full().bg(theme::hover()).overflow_hidden().child(div().h_full().w(theme::sz(34. * (p as f32 / 100.).clamp(0., 1.))).bg(if full { theme::attention() } else { theme::added() })))
        });
        // The hints only where the bar has the room for them on one line.
        let roomy = crate::session_view::centre_fit(self.centre_width()).words;
        let bar = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(theme::sz(6.))
            .font_family(theme::UI_FONT)
            .text_size(theme::sz(12.5))
            .text_color(theme::card_soft_text())
            .child(
                button("attach")
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.chat_pick_files(&attach_id, window, cx)))
                    .child(icons::icon(icons::ATTACH, theme::sz(17.), theme::sz(17.), theme::text_soft())),
            )
            .when_some(mode.filter(|_| agent == AgentKind::Claude), |d, mode| {
                let plan = mode.starts_with("Plan");
                let auto = mode.starts_with("Auto") || mode.starts_with("Bypass");
                d.child(
                    div()
                        .id("chat-mode")
                        .h(theme::sz(26.))
                        .px_2()
                        .flex()
                        .items_center()
                        .rounded(theme::sz(8.))
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::raised())
                        .text_color(if plan { theme::codex() } else if auto { theme::attention() } else { theme::text_soft() })
                        .font_weight(FontWeight::MEDIUM)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.chat_cycle_mode(&mode_id, cx)))
                        .child(mode),
                )
            })
            .when(roomy, |d| d.child(div().child("⏎ send · ⇧⏎ new line · ⇧⇥ mode")))
            .child(div().flex_1())
            .children(meter)
            .child({
                // Always offer the model control (fresh sessions have no reported model yet).
                let label = model.unwrap_or_else(|| "Model".into());
                div()
                    .id("chat-model")
                    .h(theme::sz(26.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded(theme::sz(8.))
                    .text_color(theme::text_soft())
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::tint()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        // Open the agent's own /model picker; it is a selection menu, so it
                        // surfaces right here as buttons (the chat polls the screen) — no need
                        // to leave for the terminal.
                        if let Some(t) = this.terminals.get(&model_id).cloned() {
                            t.update(cx, |t, cx| t.paste_text("/model", cx));
                            cx.spawn(async move |_, cx| {
                                cx.background_executor().timer(Duration::from_millis(150)).await;
                                let _ = t.update(cx, |t, cx| t.write(b"\r".to_vec(), cx));
                            })
                            .detach();
                        }
                    }))
                    .child(label)
                    .child(icons::icon(icons::CHEVRON, theme::sz(10.), theme::sz(10.), theme::card_soft_text()).with_transformation(Transformation::rotate(percentage(0.25))))
            })
            .when(agent == AgentKind::Claude, |d| {
                d.child(
                    button("mic")
                        .when(listening, |b| b.bg(theme::removed()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.voice_toggle(&mic_id, cx);
                            this.focus_active(window, cx);
                        }))
                        .child(icons::icon(icons::MIC, theme::sz(16.), theme::sz(16.), if listening { theme::on_danger() } else { theme::text_soft() })),
                )
            })
            .child(
                div()
                    .id("chat-send")
                    .flex_none()
                    .size(theme::sz(32.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(if empty || !running { theme::hover() } else { theme::accent() })
                    .when(!empty && running, |d| d.cursor_pointer())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.chat_send(&send_id, cx)))
                    .child(icons::icon(icons::SEND, theme::sz(15.), theme::sz(15.), if empty || !running { theme::faint() } else { theme::on_accent() })),
            );
        crate::backdrop::glass_strong(theme::sz(18.))
            .when(theme::is_light(), |d| d.shadow_md())
            .flex()
            .flex_col()
            .gap(theme::sz(6.))
            .p(theme::sz(8.))
            .child(input)
            .child(bar)
            .into_any_element()
    }

    /// The + button: choose images or files for the message.
    fn chat_pick_files(&mut self, id: &str, _window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Attach".into()) });
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let _ = this.update(cx, |this, cx| this.chat_attach(&id, paths.into_iter().map(crate::terminal::files::Incoming::Path).collect(), cx));
        })
        .detach();
    }
}

fn clone_incoming(incoming: &crate::terminal::files::Incoming) -> crate::terminal::files::Incoming {
    match incoming {
        crate::terminal::files::Incoming::Path(p) => crate::terminal::files::Incoming::Path(p.clone()),
        crate::terminal::files::Incoming::Image(i) => crate::terminal::files::Incoming::Image(i.clone()),
    }
}

/// What is typed in Claude's own input box (its dimmed suggestions left out).
pub(crate) fn claude_box(terminal: &crate::terminal::TerminalView) -> String {
    crate::ci_fix::prompt_box(&terminal.text_tail_typed(60)).unwrap_or_default()
}

/// What Claude's screen tells before its first hook event (`agents::ClaudeStart`):
/// whether anything is drawn yet, and whether it is Claude's own empty message box (it has
/// started) rather than a question. A question in a box can look like the message box with text
/// in it, never like an empty one.
pub(crate) fn claude_start_screen(tail: &str, typed: &str) -> (bool, bool) {
    let drawn = !tail.trim().is_empty();
    let message_box = crate::ci_fix::prompt_box(typed).is_some_and(|text| text.is_empty()) && !prompt::has_live_menu(tail);
    (drawn, message_box)
}

/// Empty Claude's own box: Ctrl-L (Claude's "clear input"), then, if anything is still there
/// (its last words came after, or the key does something else), End and a backspace a letter.
fn clear_claude_box(terminal: &gpui::Entity<crate::terminal::TerminalView>, cx: &mut gpui::App) {
    terminal.update(cx, |t, cx| t.write(b"\x0c".to_vec(), cx));
    let terminal = terminal.clone();
    cx.spawn(async move |cx| {
        for _ in 0..3 {
            cx.background_executor().timer(Duration::from_millis(300)).await;
            let done = terminal.update(cx, |t, cx| erase_box(t, cx));
            if done {
                break;
            }
        }
    })
    .detach();
}

/// Backspace away what is left in Claude's box; true when it was already empty.
fn erase_box(t: &mut crate::terminal::TerminalView, cx: &mut Context<crate::terminal::TerminalView>) -> bool {
    let left = claude_box(t);
    if left.is_empty() {
        return true;
    }
    let mut keys = b"\x1b[F".to_vec();
    keys.extend(std::iter::repeat_n(0x7f, left.chars().count() + 2));
    t.write(keys, cx);
    false
}

/// Whether Claude's box still holds the message just sent (as typed, or folded into
/// "[Pasted text #1 +3 lines]").
fn still_in_box(in_box: &str, message: &str) -> bool {
    if in_box.is_empty() {
        return false;
    }
    let start: String = squash(message).chars().take(24).collect();
    in_box.contains("[Pasted text") || (!start.is_empty() && squash(in_box).contains(&start))
}

/// The same message, as sent and as the agent recorded it (spacing aside; a long one may be
/// recorded whole while the start was compared).
fn same_message(recorded: &str, sent: &str) -> bool {
    let (recorded, sent) = (squash(recorded), squash(sent));
    !sent.is_empty() && (recorded == sent || recorded.starts_with(&sent) || sent.starts_with(&recorded) && recorded.len() > 40)
}

/// Text compared without its spacing (a transcript and its copy wrap differently).
fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// What was in the message box, then the words heard.
fn join_words(base: &str, heard: &str) -> String {
    match (base.trim().is_empty(), heard.trim().is_empty()) {
        (true, _) => heard.trim().to_string(),
        (false, true) => base.to_string(),
        (false, false) => format!("{} {}", base.trim_end(), heard.trim()),
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 { format!("1 {word}") } else { format!("{n} {word}s") }
}

/// The agent's own line about what it is doing: "Pondering… (12s · ↓ 1.2k tokens · esc to
/// interrupt)" → "Pondering… 12s · ↓ 1.2k tokens".
fn status_line(tail: &str) -> Option<String> {
    let lines: Vec<&str> = tail.lines().filter(|l| !l.trim().is_empty()).collect();
    // Claude: "✻ Pondering… (12s · ↓ 1.2k tokens)", the line just above its input box.
    let top_rule = lines.iter().rposition(|l| l.chars().filter(|c| *c == '─').count() >= 20).and_then(|bottom| lines[..bottom].iter().rposition(|l| l.chars().filter(|c| *c == '─').count() >= 20));
    if let Some(top) = top_rule {
        for line in lines[top.saturating_sub(4)..top].iter().rev() {
            if let Some(status) = spinner_line(line) {
                return Some(status);
            }
        }
    }
    let line = lines.iter().rev().take(14).find(|l| l.contains("esc to interrupt"))?;
    let t = line.trim().trim_start_matches(|c: char| !c.is_alphanumeric()).trim();
    let t = t.replace("· esc to interrupt", "").replace("• esc to interrupt", "").replace("esc to interrupt", "");
    let t = t.replace(['(', ')'], " ").split_whitespace().collect::<Vec<_>>().join(" ");
    let t = t.trim_end_matches(['·', '•', ' ']).to_string();
    (!t.is_empty()).then_some(t)
}

/// "✻ Pondering… (12s · ↓ 1.2k tokens · esc to interrupt)" → "Pondering… 12s · ↓ 1.2k tokens":
/// a spinner, one word ending in "…", then what it has done so far.
fn spinner_line(line: &str) -> Option<String> {
    let t = line.trim().trim_start_matches(|c: char| !c.is_alphanumeric()).trim();
    let (word, rest) = t.split_once('…')?;
    let plain = |c: char| c.is_alphabetic() || c == '-' || c == '\'' || c == ' ';
    if word.is_empty() || word.len() > 32 || !word.chars().next()?.is_uppercase() || !word.chars().all(plain) || word.split_whitespace().count() > 3 {
        return None;
    }
    let rest = rest.trim();
    if !(rest.is_empty() || rest.starts_with('(')) {
        return None;
    }
    let detail = rest.trim_start_matches('(').trim_end_matches(')').replace("· esc to interrupt", "").replace("esc to interrupt", "");
    let detail = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    let detail = detail.trim_end_matches(['·', ' ']).to_string();
    Some(if detail.is_empty() { format!("{word}…") } else { format!("{word}… {detail}") })
}

/// Claude's mode, from its footer: "⏵⏵ auto mode on (shift+tab to cycle)" → "Auto mode".
fn mode_line(tail: &str) -> Option<String> {
    let lines: Vec<&str> = tail.lines().rev().filter(|l| !l.trim().is_empty()).take(8).collect();
    for line in &lines {
        let l = line.trim().trim_start_matches(|c: char| !c.is_alphanumeric());
        let Some(before) = l.find(" on (shift+tab").map(|i| &l[..i]).or_else(|| l.strip_suffix(" on")) else { continue };
        let name = before.trim();
        if name.is_empty() || name.len() > 24 {
            continue;
        }
        let mut chars = name.chars();
        return chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str());
    }
    lines.iter().any(|l| l.contains("? for shortcuts")).then(|| "Default".to_string())
}

fn spinner(key: &str, size: gpui::Pixels, color: Hsla) -> AnyElement {
    icons::icon(icons::SPINNER, size, size, color)
        .with_animation(SharedString::from(key.to_string()), Animation::new(Duration::from_millis(900)).repeat(), |svg, t| svg.with_transformation(Transformation::rotate(percentage(t))))
        .into_any_element()
}

fn image_for(images: &mut HashMap<usize, Option<Arc<gpui::Image>>>, image: &chat::Image) -> Option<Arc<gpui::Image>> {
    let key = Arc::as_ptr(&image.base64) as usize;
    images
        .entry(key)
        .or_insert_with(|| {
            let format = match image.media_type.as_str() {
                "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
                "image/gif" => ImageFormat::Gif,
                "image/webp" => ImageFormat::Webp,
                _ => ImageFormat::Png,
            };
            image.bytes().map(|bytes| Arc::new(gpui::Image::from_bytes(format, bytes)))
        })
        .clone()
}

fn picture(image: Arc<gpui::Image>, max_w: f32) -> AnyElement {
    div()
        .flex_none()
        .max_w(theme::sz(max_w))
        .rounded(theme::sz(12.))
        .border_1()
        .border_color(theme::border())
        .overflow_hidden()
        .child(img(image).max_w(theme::sz(max_w)).max_h(theme::sz(380.)).object_fit(ObjectFit::Contain))
        .into_any_element()
}

fn user_bubble(text: &str, images: &[chat::Image], from_glasscode: Option<&str>, cache: &mut HashMap<usize, Option<Arc<gpui::Image>>>, sel: &mut Selection, pending: bool) -> AnyElement {
    let body = (!text.trim().is_empty()).then(|| sel.plain(text.trim().to_string()));
    let pictures: Vec<AnyElement> = images.iter().filter_map(|i| image_for(cache, i)).map(|i| picture(i, 240.)).collect();
    div()
        .flex()
        .flex_col()
        .items_end()
        .gap_1()
        .child(div().flex().flex_wrap().justify_end().gap_2().children(pictures))
        .when_some(body, |d, body| {
            d.child(
                div()
                    .max_w(gpui::relative(0.84))
                    .px(theme::sz(15.))
                    .py(theme::sz(10.))
                    .rounded(theme::sz(18.))
                    .bg(if from_glasscode.is_some() { theme::warn_bg() } else { theme::hover() })
                    .font_family(theme::UI_FONT)
                    .text_color(theme::text_strong())
                    .when(pending, |d| d.opacity(0.6))
                    .child(body),
            )
        })
        .when_some(from_glasscode, |d, why| {
            d.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .font_family(theme::UI_FONT)
                    .text_size(theme::sz(12.))
                    .text_color(theme::card_soft_text())
                    .child(div().size(theme::sz(6.)).rounded_full().bg(theme::attention()))
                    .child(format!("Sent by Glasscode: {why}")),
            )
        })
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_row(row: &Row, log: &ChatLog, open_rows: &HashSet<String>, images: &mut HashMap<usize, Option<Arc<gpui::Image>>>, sel: &mut Selection, id: &str, ws: gpui::WeakEntity<Workspace>, cx: &mut Context<Workspace>) -> AnyElement {
    match row {
        Row::Group { entries, label, .. } => {
            let key = format!("group-{}", entries[0]);
            let open = open_rows.contains(&key);
            let header = tool_header(&key, None, label, sel, false, None, false, true, open, id, cx);
            let tools: Vec<AnyElement> = if open {
                entries
                    .iter()
                    .filter_map(|&i| match &log.entries[i] {
                        Entry::Tool(t) => Some(
                            div()
                                .flex()
                                .gap_1p5()
                                .font_family(theme::UI_FONT)
                                .text_size(theme::sz(13.))
                                .text_color(theme::card_soft_text())
                                .child(sel.plain(t.verb.clone()))
                                .child(div().font_family(theme::MONO_FONT).text_size(theme::sz(12.5)).text_color(theme::text_soft()).overflow_hidden().whitespace_nowrap().text_ellipsis().child(sel.plain(t.subject.clone())))
                                .when_some(t.meta.clone(), |d, m| d.child(div().text_color(theme::card_soft_text()).child(m)))
                                .into_any_element(),
                        ),
                        _ => None,
                    })
                    .collect()
            } else {
                Vec::new()
            };
            div().flex().flex_col().gap_1().child(header).when(open, |d| d.child(div().flex().flex_col().gap_1().pl_3().ml_1().border_l_1().border_color(theme::border()).children(tools))).into_any_element()
        }
        Row::One(i) => match &log.entries[*i] {
            Entry::User { text, images: pictures, from_glasscode } => user_bubble(text, pictures, from_glasscode.as_deref(), images, sel, false),
            Entry::Text(text) => render_reply(text, &format!("reply-{i}"), sel, ws),
            Entry::Tool(tool) => {
                let open = open_rows.contains(&tool.id);
                let header = tool_header(&tool.id, Some(&tool.verb), &tool.subject, sel, tool.code, tool.meta.as_deref(), tool.failed, tool.done, open, id, cx);
                let pictures: Vec<AnyElement> = tool.images.iter().filter_map(|img| image_for(images, img)).map(|img| picture(img, 560.)).collect();
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(header)
                    .when(open && !tool.detail.is_empty(), |d| d.child(detail_box(&tool.detail, sel)))
                    .children(pictures)
                    .into_any_element()
            }
            Entry::Todos { items, .. } => div()
                .flex()
                .flex_col()
                .gap_1p5()
                .px_3p5()
                .py_2p5()
                .rounded(theme::sz(12.))
                .border_1()
                .border_color(theme::border())
                .bg(theme::raised())
                .font_family(theme::UI_FONT)
                .text_size(theme::sz(13.5))
                .child(div().text_size(theme::sz(12.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::muted()).child("Plan"))
                .children(items.iter().map(|t| {
                    let done = t.state == TodoState::Done;
                    div()
                        .flex()
                        .items_center()
                        .gap_2p5()
                        .text_color(if done { theme::muted() } else { theme::text() })
                        .when(t.state == TodoState::Active, |d| d.font_weight(FontWeight::MEDIUM).text_color(theme::text_strong()))
                        .child(
                            div()
                                .flex_none()
                                .size(theme::sz(15.))
                                .rounded(theme::sz(4.))
                                .border_1()
                                .border_color(if done { theme::added() } else { theme::border_strong() })
                                .bg(if done { theme::added() } else { theme::raised() })
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(theme::sz(10.))
                                .text_color(theme::on_accent())
                                .when(done, |d| d.child("✓")),
                        )
                        .child(div().when(done, |d| d.line_through()).child(sel.plain(t.text.clone())))
                }))
                .into_any_element(),
            Entry::Plan(plan) => div()
                .flex()
                .flex_col()
                .gap_2()
                .p_4()
                .rounded(theme::sz(12.))
                .border_1()
                .border_color(theme::border_strong())
                .bg(theme::raised())
                .child(div().font_family(theme::UI_FONT).text_size(theme::sz(12.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::muted()).child("Plan"))
                .child(render_reply(plan, &format!("plan-{i}"), sel, ws))
                .into_any_element(),
            Entry::Notice(text) => div().flex().justify_center().font_family(theme::UI_FONT).text_size(theme::sz(12.)).text_color(theme::card_soft_text()).child(sel.plain(text.clone())).into_any_element(),
        },
    }
}

/// The agent asks something before it starts (trust this folder, a new MCP server, the sign-in)
/// that the Chat view cannot show: say so, and give one click to the terminal. Never answered
/// from here.
fn startup_notice(id: &str, agent: AgentKind, cx: &mut Context<Workspace>) -> AnyElement {
    let sid = id.to_string();
    div()
        .flex()
        .flex_col()
        .gap_2p5()
        .p_4()
        .rounded(theme::sz(14.))
        .border_2()
        .border_color(theme::attention())
        .bg(theme::raised())
        .font_family(theme::UI_FONT)
        .text_size(theme::sz(14.))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text_strong())
                .child(div().size(theme::sz(9.)).rounded_full().bg(theme::attention()))
                .child(format!("{} is asking a question before it starts", agent.name())),
        )
        .child(div().text_color(theme::text_soft()).child("Open the Terminal view to answer it."))
        .child(
            div()
                .id("startup-open-terminal")
                .self_start()
                .px_3()
                .py_1p5()
                .rounded(theme::sz(9.))
                .border_1()
                .border_color(theme::accent())
                .text_color(theme::accent())
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.set_session_view(&sid, SessionView::Terminal, window, cx)))
                .child("Open Terminal"),
        )
        .into_any_element()
}

/// The first entry a row shows.
fn first_entry(row: &Row) -> usize {
    match row {
        Row::One(i) => *i,
        Row::Group { entries, .. } => entries.first().copied().unwrap_or_default(),
    }
}

/// Where a Codex turn ended without a reply. Its rollout has no error to show; Codex's own
/// message about it is in its terminal.
fn unanswered_notice(id: &str, sel: &mut Selection, cx: &mut Context<Workspace>) -> AnyElement {
    let sid = id.to_string();
    div()
        .flex()
        .flex_wrap()
        .justify_center()
        .gap_x_2()
        .font_family(theme::UI_FONT)
        .text_size(theme::sz(12.))
        .text_color(theme::card_soft_text())
        .child(sel.plain("Codex ended this turn without a reply. The Terminal view shows what happened."))
        .child(
            div()
                .id("chat-unanswered-terminal")
                .text_color(theme::link())
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.set_session_view(&sid, SessionView::Terminal, window, cx)))
                .child("Open Terminal"),
        )
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn tool_header(key: &str, verb: Option<&str>, subject: &str, sel: &mut Selection, code: bool, meta: Option<&str>, failed: bool, done: bool, open: bool, id: &str, cx: &mut Context<Workspace>) -> AnyElement {
    let (key_owned, sid) = (key.to_string(), id.to_string());
    let verb = verb.map(|v| sel.plain(v.to_string()));
    let subject = (!subject.is_empty()).then(|| sel.plain(subject.to_string()));
    let meta_color = match meta {
        Some(m) if failed || m == "interrupted" => theme::removed(),
        Some(m) if m.starts_with('+') => theme::added(),
        _ => theme::card_soft_text(),
    };
    div()
        .id(SharedString::from(format!("tool-{key}")))
        .flex()
        .items_center()
        .gap_1p5()
        .max_w_full()
        .font_family(theme::UI_FONT)
        .text_size(theme::sz(14.))
        .text_color(theme::card_soft_text())
        .cursor_pointer()
        .hover(|s| s.text_color(theme::text()))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            if let Some(pane) = this.chat.panes.get_mut(&sid) {
                if !pane.open.remove(&key_owned) {
                    pane.open.insert(key_owned.clone());
                }
            }
            cx.notify();
        }))
        .when(failed, |d| d.child(div().size(theme::sz(7.)).rounded_full().bg(theme::removed())))
        .when(!done, |d| d.child(spinner(&format!("tool-spin-{key}"), theme::sz(13.), theme::card_soft_text())))
        .when_some(verb, |d, verb| d.child(div().flex_none().child(verb)))
        .when_some(subject, |d, subject| {
            d.child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .when(code, |d| d.font_family(theme::MONO_FONT).text_size(theme::sz(13.)).text_color(theme::text_soft()))
                    .child(subject),
            )
        })
        .child(icons::icon(icons::CHEVRON, theme::sz(11.), theme::sz(11.), theme::card_soft_text()).with_transformation(Transformation::rotate(percentage(if open { 0.25 } else { 0. }))))
        .when_some(meta.map(str::to_string), |d, m| d.child(div().flex_none().text_size(theme::sz(12.)).text_color(meta_color).child(m)))
        .into_any_element()
}

/// A tool's output or diff: numbered, tinted lines in the code font.
fn detail_box(lines: &[Line], sel: &mut Selection) -> AnyElement {
    let numbered = lines.iter().any(|l| l.number.is_some());
    div()
        .id("tool-detail")
        .flex()
        .flex_col()
        .max_h(theme::sz(380.))
        .overflow_y_scroll()
        .py_2()
        .rounded(theme::sz(10.))
        .border_1()
        .border_color(theme::border())
        .bg(theme::raised())
        .font_family(theme::MONO_FONT)
        .text_size(theme::sz(12.))
        .line_height(theme::sz(18.))
        .children(lines.iter().map(|line| {
            let body = sel.plain(if line.text.is_empty() { " ".to_string() } else { line.text.clone() });
            let (fg, bg) = match line.kind {
                LineKind::Added => (theme::text_strong(), Some(theme::added_bg())),
                LineKind::Removed => (theme::text_strong(), Some(theme::removed_bg())),
                LineKind::Hunk => (theme::faint(), None),
                LineKind::Error => (theme::removed(), None),
                LineKind::Plain => (theme::text_soft(), None),
            };
            div()
                .flex()
                .px_3()
                .when_some(bg, |d, bg| d.bg(bg))
                .when(numbered, |d| {
                    d.child(div().flex_none().w(theme::sz(38.)).pr_2().text_color(theme::faint()).flex().justify_end().child(line.number.map(|n| n.to_string()).unwrap_or_default()))
                })
                .child(div().flex_1().min_w_0().text_color(fg).child(body))
        }).collect::<Vec<_>>())
        .into_any_element()
}

/// An agent's reply: Markdown in the reading font, code in chips and boxes.
fn render_reply(text: &str, key: &str, sel: &mut Selection, ws: gpui::WeakEntity<Workspace>) -> AnyElement {
    let size = theme::chat_text_size();
    let blocks = markdown::parse(text);
    let _ = ws;
    div()
        .flex()
        .flex_col()
        .gap(theme::sz(10.))
        .children(blocks.into_iter().enumerate().map(|(i, block)| {
            let key = format!("{key}-{i}");
            match block {
                Block::Heading { level, text } => div().pt_1().font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(rich(sel, &text, key, size + if level <= 2 { 3. } else { 1. } * theme::scale())).into_any_element(),
                Block::Paragraph(text) => rich(sel, &text, key, size),
                Block::Item { number, depth, text } => div()
                    .flex()
                    .gap_2p5()
                    .pl(theme::sz(depth as f32 * 18.))
                    .child(div().flex_none().min_w(theme::sz(16.)).text_color(theme::card_soft_text()).child(number.map(|n| format!("{n}.")).unwrap_or_else(|| "•".into())))
                    .child(div().flex_1().min_w_0().child(rich(sel, &text, key, size)))
                    .into_any_element(),
                Block::Check { done, depth, text } => div()
                    .flex()
                    .gap_2p5()
                    .pl(theme::sz(depth as f32 * 18.))
                    // The box sits on the first line of the item.
                    .child(div().flex_none().h(px(size * 1.6)).flex().items_center().child(icons::icon(if done { icons::CHECKBOX_CHECKED } else { icons::CHECKBOX }, px(size), px(size), theme::text_soft())))
                    .child(div().flex_1().min_w_0().child(rich(sel, &text, key, size)))
                    .into_any_element(),
                Block::Code(code) => div()
                    .px_3p5()
                    .py_2p5()
                    .rounded(theme::sz(10.))
                    .bg(theme::chat_code_bg())
                    .border_1()
                    .border_color(theme::border())
                    .font_family(theme::MONO_FONT)
                    .text_size(theme::sz(12.5))
                    .line_height(theme::sz(19.))
                    .text_color(theme::text_strong())
                    .child(sel.plain(code.trim_end_matches('\n').to_string()))
                    .into_any_element(),
                Block::Quote(text) => div().flex().gap_2p5().child(div().w(theme::sz(3.)).flex_none().rounded_full().bg(theme::border_strong())).child(div().flex_1().min_w_0().text_color(theme::text_soft()).child(rich(sel, &text, key, size))).into_any_element(),
                Block::Table(rows) => {
                    let columns = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
                    div()
                        .flex()
                        .flex_col()
                        .rounded(theme::sz(10.))
                        .border_1()
                        .border_color(theme::border())
                        .overflow_hidden()
                        .font_family(theme::UI_FONT)
                        .text_size(theme::sz(13.))
                        .children(rows.into_iter().enumerate().map(|(r, row)| {
                            let key = format!("{key}-{r}");
                            div()
                                .flex()
                                .when(r == 0, |d| d.bg(theme::hover()).font_weight(FontWeight::SEMIBOLD))
                                .when(r > 0, |d| d.border_t_1().border_color(theme::border()))
                                .children((0..columns).map(|c| {
                                    let cell = row.get(c).cloned().unwrap_or_default();
                                    div().flex_1().min_w(theme::sz(90.)).px_2p5().py_1p5().child(rich(sel, &cell, format!("{key}-{c}"), 13. * theme::scale()))
                                }))
                        }))
                        .into_any_element()
                }
                Block::Rule => div().h(px(1.)).bg(theme::border()).into_any_element(),
            }
        }))
        .into_any_element()
}

/// One paragraph: bold, italic, `code` in red-brown chips, links that open in the browser.
fn rich(sel: &mut Selection, spans: &[Span], key: String, size: f32) -> AnyElement {
    let mut text = String::new();
    let mut highlights: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
    let mut mono: Vec<(std::ops::Range<usize>, SharedString)> = Vec::new();
    let mut links: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    for span in spans {
        let start = text.len();
        // A narrow no-break space either side of code, so the chip's tint has room and stays
        // on the code's line.
        if span.code {
            text.push('\u{202F}');
        }
        text.push_str(&span.text);
        if span.code {
            text.push('\u{202F}');
        }
        let range = start..text.len();
        if range.is_empty() {
            continue;
        }
        let mut style = HighlightStyle::default();
        if span.bold {
            style.font_weight = Some(FontWeight::SEMIBOLD);
            style.color = Some(theme::text());
        }
        if span.italic {
            style.font_style = Some(FontStyle::Italic);
        }
        if span.code {
            style.color = Some(theme::chat_code());
            style.background_color = Some(theme::chat_code_bg());
            mono.push((range.clone(), SharedString::from(theme::MONO_FONT)));
        }
        if let Some(link) = &span.link {
            style.color = Some(theme::link());
            style.underline = Some(UnderlineStyle { thickness: px(1.), color: Some(Hsla { a: 0.5, ..theme::link() }), wavy: false });
            links.push((range.clone(), link.clone()));
        }
        if style != HighlightStyle::default() {
            highlights.push((range, style));
        }
    }
    let styled = sel.text(text, highlights, mono);
    let body: AnyElement = if links.is_empty() {
        styled.into_any_element()
    } else {
        let ranges: Vec<std::ops::Range<usize>> = links.iter().map(|(r, _)| r.clone()).collect();
        let targets: Vec<String> = links.into_iter().map(|(_, url)| url).collect();
        InteractiveText::new(SharedString::from(key), styled)
            .on_click(ranges, move |index, _window, _cx| {
                if let Some(url) = targets.get(index) {
                    open_url(url);
                }
            })
            .into_any_element()
    };
    div().text_size(px(size)).child(body).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_sent_and_recorded_messages() {
        assert!(same_message("also please check  if we already have\nfix brnach", "also please check if we already have fix brnach"));
        assert!(!same_message("something else", "also please check"));
        assert!(!same_message("x", ""));
    }

    #[test]
    fn knows_a_message_still_waiting() {
        assert!(still_in_box("Please check again.", "Please check again."));
        assert!(still_in_box("[Pasted text #1 +4 lines]", "a\nb\nc\nd\ne"));
        assert!(!still_in_box("", "Please check again."));
        assert!(!still_in_box("something else", "Please check again."));
    }

    #[test]
    fn reads_the_status_and_mode() {
        assert_eq!(status_line("x\n✻ Pondering… (12s · ↓ 1.2k tokens · esc to interrupt)\n").as_deref(), Some("Pondering… 12s · ↓ 1.2k tokens"));
        assert_eq!(status_line("• Working (24s • esc to interrupt)").as_deref(), Some("Working 24s"));
        assert_eq!(status_line("all done"), None);
        let rule = "─".repeat(30);
        assert_eq!(status_line(&format!("⏺ Reading the file\n\n✢ Combobulating… (41s · ↑ 2.3k tokens · thought for 3s)\n\n{rule}\n❯ \n{rule}\n  ⏵⏵ auto mode on")).as_deref(), Some("Combobulating… 41s · ↑ 2.3k tokens · thought for 3s"));
        assert_eq!(status_line(&format!("✳ Thinking…\n{rule}\n❯ \n{rule}")).as_deref(), Some("Thinking…"));
        assert_eq!(status_line(&format!("⏺ Done… that was quick, here is the summary\n{rule}\n❯ \n{rule}")), None);
        assert_eq!(mode_line("──\n  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents").as_deref(), Some("Auto mode"));
        assert_eq!(mode_line("  ⏸ plan mode on (shift+tab to cycle)").as_deref(), Some("Plan mode"));
        assert_eq!(mode_line("  ⏵⏵ accept edits on (shift+tab to cycle)").as_deref(), Some("Accept edits"));
        assert_eq!(mode_line("  ? for shortcuts").as_deref(), Some("Default"));
    }

    #[test]
    fn tells_claudes_message_box_from_a_question_before_it_starts() {
        let rule = "─".repeat(40);
        // Nothing drawn yet.
        assert_eq!(claude_start_screen("\n\n  \n", "\n\n"), (false, false));
        // Claude has started: its empty message box (the hint in it is dim, so not typed).
        let started = format!(" ✻ Welcome to Claude Code\n\n{rule}\n❯ \n{rule}\n  ? for shortcuts\n");
        assert_eq!(claude_start_screen(&started, &started), (true, true));
        let older = "╭────────────────────────────────────────╮\n│ >                                      │\n╰────────────────────────────────────────╯\n";
        assert_eq!(claude_start_screen(older, older), (true, true));
        // Questions before it starts, with or without a box, numbered or not: not the message box.
        let trust = format!("{rule}\n Accessing workspace:\n\n /home/dev/app\n\n Quick safety check: Is this a project you created or one you trust?\n\n ❯ 1. Yes, I trust this folder\n   2. No, exit\n\n Enter to confirm · Esc to cancel\n");
        assert_eq!(claude_start_screen(&trust, &trust), (true, false));
        let boxed = format!("{rule}\n New MCP server found in .mcp.json: example\n\n ❯ 1. Use this and all future MCP servers in this project\n   2. Use this MCP server\n   3. Continue without using this MCP server\n{rule}\n");
        assert_eq!(claude_start_screen(&boxed, &boxed), (true, false));
        let plain = format!("{rule}\n Select login method:\n\n ❯ Claude account with subscription\n   Anthropic Console account\n{rule}\n");
        assert_eq!(claude_start_screen(&plain, &plain), (true, false));
        // A question while its message box is on screen is not "started" either.
        let asking = format!("{rule}\n❯ \n{rule}\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n");
        assert_eq!(claude_start_screen(&asking, &asking), (true, false));
    }
}
