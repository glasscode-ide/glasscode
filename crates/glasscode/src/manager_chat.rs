//! The manager's chat with the engine at v2 (M52.1; ux-3, ux-4, ux-9, ux-16, ux-18, ux-20): one
//! conversation, the conversation store's (M43), answered by the chat lane's brain (M51), shown in
//! one thread component on the Manager page and under the Today box.
//!
//! - **The data path.** Every way to ask (the typed box, a Try phrase, the voice, `glasscode
//!   manager ask`) goes through `mtick_ask`, which at v2 comes here. Answers from code come back at
//!   once (v1's quick answers, "what can you do?" from the registry); anything else goes to the
//!   chat lane, on a thread of its own that owns the brain. A message sent while it answers is
//!   queued, shown as such, and goes next; Stop ends the answer, which stays, marked stopped.
//! - **One writer.** Every post goes through one thread that loads the conversation, writes, and
//!   hands the new state to the window; it also loads it again when another writer (a wake, a
//!   card) changed the files. Streaming text stays in memory until the answer ends.
//! - **The brain's conversation** is the newest rollover link's, loaded again after a restart; a
//!   fresh one starts from a summary on a new day, after an idle spell or past a size (D8).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, SystemTime};

use gpui::{AnyElement, ClickEvent, Context, Div, FontWeight, Role, SharedString, Window, div, prelude::*, px, relative};
use glasscode_core::caller::Lane;
use glasscode_core::model::AgentKind;
use glasscode_core::conversation::{Author, Conversation, Draft, Item, Kind, Ref, Stream};
use glasscode_core::runtime::chat::{self, Caps, ChatQueue, ChipTarget, Next, Queued, StartMode};
use glasscode_core::runtime::lanes::{ChatLane, Rolled};
use glasscode_core::runtime::{BrainSpec, Host, Ledger, Outcome, TurnEnd, Update};
use glasscode_core::store::Store as _;
use glasscode_core::words::WordsId;

use crate::manager_tick::OnAnswer;
use crate::theme;
use crate::workspace::Workspace;

/// What the chat keeps between frames.
#[derive(Default)]
pub struct ChatUi {
    /// The conversation as it was last loaded.
    pub conversation: Conversation,
    /// Its items in order, without summaries and rollover links: what the threads draw (sorted once
    /// when it arrives, not each frame).
    pub items: Vec<Item>,
    /// Whether the chat's brain can be started, and why not (read when the settings change).
    ready: Option<Result<(), String>>,
    /// The answer being written, if one is.
    pub turn: Option<Turn>,
    pub queue: ChatQueue,
    worker: Option<mpsc::Sender<Job>>,
    writer: Option<mpsc::Sender<Write>>,
    /// The chat over the window (ux-3): open, its box, and the newest item seen while it was.
    pub overlay: bool,
    overlay_input: Option<gpui::Entity<crate::text_input::TextInput>>,
    /// The newest item seen (none until the conversation first arrives: then all of it is).
    seen_ms: Option<u64>,
    /// The topic the Manager page's thread shows (none: all of them).
    pub topic: Option<glasscode_core::conversation::TopicId>,
    /// When the person last sent a message (the 30 s in which the chat may move the window).
    pub last_person_ms: Option<u64>,
    /// The person took the "About: …" chip off: the next message goes without where they are.
    pub focus_off: bool,
    /// Why the brain cannot answer, after its last turn (ux-15); none while it can.
    pub failure: Option<chat::Failure>,
    /// Whose failure that is: the manager's own agent, or the other one asked instead.
    failure_agent: Option<AgentKind>,
    /// The questions held until the limit resets (`manager/held.json`), read once.
    held: Option<chat::HeldFile>,
    /// The other agent's chat thread, started when the person first asks it instead (ux-15).
    other_worker: Option<(AgentKind, mpsc::Sender<Job>)>,
    /// The other agent the failure line offers, looked for when the failure came (none: not
    /// installed, no way out of this failure, or a project the manager's account alone may read).
    other_offer: Option<AgentKind>,
    /// The last question no answer came to and that is not held (the other agent's to answer).
    unanswered: Option<Asked>,
    /// The last question sent to the brain, for "Try again".
    last_question: Option<(String, Option<WordsId>)>,
    /// The Undo of each action line, by its key, while this app runs (ux-14).
    pub undos: std::collections::HashMap<String, chat::UndoEntry>,
    /// Text the person attached to the next message (the "+ N selected lines" chip, ux-5).
    pub attached: Option<chat::Attached>,
    /// The "/" or "@" menu of the box being typed in, and that box (ux-18).
    pub menu: Vec<chat::Suggestion>,
    menu_for: Option<gpui::Entity<crate::text_input::TextInput>>,
    /// A queued message being edited in a box (Enter saves it in its place).
    pub editing: Option<u64>,
    /// Long answers opened with "Read more" in the small surfaces (ux-20).
    pub expanded: std::collections::HashSet<glasscode_core::conversation::ItemId>,
    /// The last question was asked from the Today box: its exchange shows under it.
    pub today_asked: bool,
    /// The start box's placeholder as last set.
    pub placeholder: Option<String>,
}

/// The answer being written.
pub struct Turn {
    /// What it said so far.
    pub text: String,
    /// What it is doing (its last tool call).
    pub activity: Option<String>,
    pub caps: Caps,
    stop: Option<Arc<AtomicBool>>,
    pub stopping: bool,
    on_answer: Option<OnAnswer>,
    voice: bool,
    /// When it was sent, and when its first text came (the time budget, ux-m3).
    sent: std::time::Instant,
    first_text_ms: Option<u64>,
    /// The agent that answers it, and the question (held when the limit is used up).
    agent: AgentKind,
    question: Asked,
}

/// A question as the person asked it, and the turn's text as it went to the brain (where they were
/// then, what they attached: ux-5), so a held or re-asked one goes as it was.
#[derive(Clone, Debug)]
struct Asked {
    text: String,
    words: Option<WordsId>,
    composed: String,
}

/// What the chat's thread is asked.
enum Job {
    Ask { text: String, since: String, next: Next },
}

/// What the chat's thread tells the window.
enum Event {
    /// The running turn's Stop.
    Stop(Arc<AtomicBool>),
    Text(String),
    Tool(String),
    /// A fresh brain conversation began (the first, or after a summary).
    Began { brain: String, rolled: Option<Rolled> },
    /// The turn's end, and the brain's state after it (`chat::failure_of`).
    Done(Result<Outcome, (String, chat::Failure)>, Option<glasscode_core::runtime::BrainHealth>),
}

/// A change for the one writer of the conversation.
type Write = Box<dyn FnOnce(&mut Conversation) + Send>;

/// How often the writer looks for changes another writer made.
const WATCH: Duration = Duration::from_secs(2);

/// The newest change of the conversation's files.
fn changed_at() -> Option<SystemTime> {
    std::fs::read_dir(glasscode_core::conversation::dir()).ok()?.filter_map(|entry| entry.ok()?.metadata().ok()?.modified().ok()).max()
}

impl Workspace {
    /// Start the conversation's writer and watcher (with the v2 engine), once.
    pub(crate) fn chat_start(&mut self, cx: &mut Context<Self>) {
        if self.mchat.writer.is_some() || !self.control.engine_v2 {
            return;
        }
        let (writes, queue) = mpsc::channel::<Write>();
        let (loaded, arrivals) = async_channel::unbounded::<Conversation>();
        let _ = std::thread::Builder::new().name("manager-chat-store".into()).spawn(move || {
            let mut seen = changed_at();
            if loaded.send_blocking(Conversation::load()).is_err() {
                return;
            }
            loop {
                match queue.recv_timeout(WATCH) {
                    Ok(write) => {
                        let mut conversation = Conversation::load();
                        write(&mut conversation);
                        seen = changed_at();
                        if loaded.send_blocking(conversation).is_err() {
                            return;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let now = changed_at();
                        if now != seen {
                            seen = now;
                            if loaded.send_blocking(Conversation::load()).is_err() {
                                return;
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        });
        cx.spawn(async move |this, cx| {
            while let Ok(conversation) = arrivals.recv().await {
                if this.update(cx, |ws, cx| {
                    ws.mchat.items = conversation.items().into_iter().filter(|item| !matches!(item.kind, Kind::Summary { .. } | Kind::Rollover { .. })).cloned().collect();
                    ws.mchat.conversation = conversation;
                    // What was there when the app started, and what arrives while the chat is
                    // open or its page is shown, is seen.
                    if ws.mchat.seen_ms.is_none() || ws.mchat.overlay || ws.view == crate::shell::View::Manager {
                        ws.mchat.seen_ms = Some(newest_ms(&ws.mchat.conversation));
                    }
                    ws.manager.page.chat_scroll.scroll_to_bottom();
                    cx.notify();
                })
                .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        self.mchat.writer = Some(writes);
    }

    /// A line Glasscode says in the conversation (v1's rounds and `manager say` at v2).
    pub(crate) fn chat_post_line(&mut self, system: bool, text: String) {
        let refs = self.chat_refs(&text);
        let kind = if system { Kind::System } else { Kind::Manager { state: Stream::Done } };
        self.chat_post(Draft { author: Author::App, kind, text, refs, topic: None });
    }

    /// Post an item, through the one writer.
    fn chat_post(&mut self, draft: Draft) {
        self.chat_write(Box::new(move |conversation| {
            if let Err(err) = conversation.post(draft) {
                log::warn!("manager: a line of the conversation could not be kept: {err}");
            }
        }));
    }

    fn chat_write(&mut self, write: Write) {
        if let Some(writer) = &self.mchat.writer
            && writer.send(write).is_ok()
        {
            return;
        }
        log::warn!("manager: the conversation's writer is not running; a line was not kept");
    }

    /// The references a text makes, for its topic.
    fn chat_refs(&self, text: &str) -> Vec<Ref> {
        let (sessions, repos) = self.chat_known();
        chat::chips_from_references(text, &sessions, &repos)
            .into_iter()
            .map(|chip| match chip.target {
                ChipTarget::Session(id) => Ref::Session(id),
                ChipTarget::Pr { repo, number } => Ref::pr(&repo, number),
            })
            .collect()
    }

    /// The sessions (id, title) and the repositories the chips can name.
    fn chat_known(&self) -> (Vec<(String, String)>, Vec<String>) {
        let sessions = self.state.sessions.iter().map(|s| (s.id.clone(), s.title.clone())).collect();
        let mut repos: Vec<String> = self.prs.map.values().filter_map(|s| s.repo.clone()).collect();
        repos.sort();
        repos.dedup();
        (sessions, repos)
    }

    /// A line the manager says from code (no brain): kept, shown, spoken when the voice is on.
    pub(crate) fn chat_say_from_code(&mut self, text: String, voice: bool, cx: &mut Context<Self>) {
        let refs = self.chat_refs(&text);
        self.chat_post(Draft { author: Author::App, kind: Kind::Manager { state: Stream::Done }, text: text.clone(), refs, topic: None });
        let _ = voice;
        self.manager_caption(&text, true, cx);
    }

    /// A question to the manager with the v2 engine (`mtick_ask` comes here). `words`: the
    /// person's words record, when it was typed or said in one of the app's boxes; none for a
    /// question from a command (`asker` says whose).
    pub(crate) fn chat_ask(&mut self, text: String, words: Option<WordsId>, asker: Option<String>, voice: bool, on_answer: Option<OnAnswer>, cx: &mut Context<Self>) {
        self.chat_start(cx);
        // Typed in the topic "Moving in" while a question waits: the answer to it (M54), posted
        // beside it there, not a question for the brain.
        if asker.is_none()
            && on_answer.is_none()
            && let Some(id) = &words
            && self.move_in_chat_answer(id, cx)
        {
            cx.notify();
            return;
        }
        // The person's message opens the 30 s in which the chat may move the window (ux-5).
        if asker.is_none() {
            self.mchat.last_person_ms = Some(glasscode_core::clock::now_ms());
        }
        // "What did you do this week?": from the effect ledger, read off the main thread (ux-22).
        if chat::asks_week(&text) {
            self.chat_post_question(&text, words, asker.as_deref());
            let task = cx.background_spawn(async { glasscode_core::effects::load().map(|ledger| ledger.records.into_values().collect::<Vec<_>>()) });
            cx.spawn(async move |this, cx| {
                let records = task.await;
                let _ = this.update(cx, |ws, cx| {
                    let say = match records {
                        Ok(records) => chat::week_account(&records, glasscode_core::clock::now_ms()),
                        Err(err) => format!("The effect ledger could not be read ({err}), so I cannot say what ran this week."),
                    };
                    ws.chat_say_from_code(say.clone(), voice, cx);
                    if let Some(on_answer) = on_answer {
                        on_answer(ws, Ok(say), cx);
                    }
                    cx.notify();
                });
            })
            .detach();
            return;
        }
        // Answers from code, at once: v1's rules ("what needs me?", "status") and the registry.
        let snap = self.manager_snapshot();
        // On the follow-ups where the engine keeps them (the work ledger's at v2, M56.2).
        let quick = self.with_follow_ups_lent(|ws| {
            let settings = ws.state.manager.clone();
            ws.manager.harness.quick_answer(&settings, &snap, &text, glasscode_core::model::now())
        });
        let from_code = match quick {
            Some(answer) => Some((answer.say, answer.actions)),
            None if chat::asks_capabilities(&text) => Some((chat::capabilities_answer(&self.state.manager.name, true), Vec::new())),
            // M33.1: what it cannot do, printed from the inventory (design section 11).
            None if chat::asks_limits(&text) => Some((format!("Here is what {} cannot do, by screen: what only you do, and what waits for a later version.\n\n{}", self.state.manager.name, crate::capabilities::cannot_do()), Vec::new())),
            None => None,
        };
        if let Some((say, actions)) = from_code {
            self.chat_post_question(&text, words, asker.as_deref());
            self.chat_say_from_code(say.clone(), voice, cx);
            let actions = actions.into_iter().filter(|a| !matches!(a, glasscode_core::manager::Action::Say { text, .. } if text.trim() == say.trim())).collect();
            self.manager_apply(actions, cx);
            self.manager.harness.save();
            if let Some(on_answer) = on_answer {
                on_answer(self, Ok(say), cx);
            }
            cx.notify();
            return;
        }
        // While it answers, the next waits, shown as queued (ux-9); a command's question is not
        // queued behind the person's: it waits for its answer.
        if self.mchat.turn.is_some() || !self.mchat.queue.is_empty() {
            if on_answer.is_none() {
                self.mchat.queue.push(text, words);
                cx.notify();
                return;
            }
            if let Some(on_answer) = on_answer {
                on_answer(self, Err(format!("{} is answering another question; ask again in a moment.", self.state.manager.name)), cx);
            }
            return;
        }
        self.chat_send_now(Queued { id: 0, text, words }, asker, voice, on_answer, cx);
    }

    /// The question as the conversation keeps it: the person's message, or Glasscode's line that
    /// says who asked.
    fn chat_post_question(&mut self, text: &str, words: Option<WordsId>, asker: Option<&str>) {
        let refs = self.chat_refs(text);
        let draft = match (words, asker) {
            (Some(words), None) => Draft { author: Author::Person, kind: Kind::You { words, reply_to: None }, text: text.to_string(), refs, topic: None },
            (_, asker) => Draft { author: Author::App, kind: Kind::System, text: format!("{} asked: {text}", asker.unwrap_or("A program")), refs, topic: None },
        };
        self.chat_post(draft);
    }

    /// Send a message to the chat's brain now.
    fn chat_send_now(&mut self, message: Queued, asker: Option<String>, voice: bool, on_answer: Option<OnAnswer>, cx: &mut Context<Self>) {
        let agent = self.state.manager.brain;
        self.chat_send_to(message, asker, voice, on_answer, None, agent, cx);
    }

    /// Send a message to a chat brain: the manager's own agent, or the other one the person asked
    /// instead (ux-15). `composed`: the turn's text from when it was first asked (a held or
    /// re-asked question, already posted); none: posted now, with where the person is now.
    #[allow(clippy::too_many_arguments)]
    fn chat_send_to(&mut self, message: Queued, asker: Option<String>, voice: bool, on_answer: Option<OnAnswer>, composed: Option<String>, agent: AgentKind, cx: &mut Context<Self>) {
        if composed.is_none() {
            self.chat_post_question(&message.text, message.words.clone(), asker.as_deref());
        }
        // The manager works from the server now (M57): its chat lane there answers what is in the
        // conversation; the question goes with the next sync and the answer comes back the same way.
        if self.server.config.is_some() && !self.home_here() {
            if let Some(on_answer) = on_answer {
                on_answer(self, Err(format!("{} works from {} now: the answer comes in the conversation.", self.state.manager.name, self.server.name())), cx);
            }
            cx.notify();
            return;
        }
        let now = glasscode_core::clock::now_ms();
        let composed = match composed {
            Some(composed) => composed,
            None => {
                // Where the person is as they send it goes with it (ux-5): names and ids from
                // `ui.state`, never text from the screen. A command's question comes from away.
                let mut text = match self.chat_focus(asker.is_some(), now, cx) {
                    Some(block) => format!("{block}\n\n{}", message.text),
                    None => message.text.clone(),
                };
                // What the person attached with the chip (ux-5), as data after their words.
                if asker.is_none()
                    && let Some(attached) = self.mchat.attached.take()
                {
                    text = format!("{text}\n\n{}", attached.block());
                }
                self.mchat.focus_off = false;
                text
            }
        };
        let question = Asked { text: message.text.clone(), words: message.words.clone(), composed };
        self.mchat.last_question = Some((message.text.clone(), message.words.clone()));
        // While the limit is used up, a question to the manager's own agent waits with the held
        // ones, said so, and goes when it resets: no turn is spent on a plan that refuses (ux-15).
        // Whatever is queued behind it waits with it.
        if agent == self.state.manager.brain
            && let Some((until, known)) = self.chat_holding((now / 1000) as i64)
        {
            self.chat_hold_question(question, until, known, on_answer, cx);
            if let Some(next) = self.mchat.queue.take_next() {
                self.chat_send_now(next, None, false, None, cx);
            }
            cx.notify();
            return;
        }
        let text = question.composed.clone();
        let caps = Caps::of(agent);
        let refusal = match self.chat_worker(agent, cx) {
            Ok(worker) => {
                // The other agent starts a conversation of its own: the store's names the manager's.
                let next = if agent == self.state.manager.brain { chat::next_conversation(&self.mchat.conversation, now) } else { Next::First };
                let job = Job::Ask { text, since: chat::since_text(&self.mchat.conversation), next };
                worker.send(job).err().map(|_| "the manager's brain stopped; ask again".to_string())
            }
            Err(why) => Some(why),
        };
        if let Some(why) = refusal {
            self.chat_post(Draft { author: Author::App, kind: Kind::Error, text: why.clone(), refs: Vec::new(), topic: None });
            if let Some(on_answer) = on_answer {
                on_answer(self, Err(why), cx);
            }
            cx.notify();
            return;
        }
        self.manager.thinking = true;
        self.mchat.turn = Some(Turn { text: String::new(), activity: None, caps, stop: None, stopping: false, on_answer, voice, sent: std::time::Instant::now(), first_text_ms: None, agent, question });
        cx.notify();
    }

    /// The snapshot of where the person is, as a block for the brain's turn (none when they took
    /// the chip off). From away (a command), "here" is the last session they had open.
    fn chat_focus(&self, away: bool, now: u64, cx: &Context<Self>) -> Option<String> {
        if self.mchat.focus_off {
            return None;
        }
        let mut state = crate::ops_v2::state_of(&self.ui_seen(cx));
        if away && let Some(session) = self.state.active_session.as_deref().and_then(|id| self.state.session(id)) {
            state["session"] = serde_json::json!({ "id": session.id, "title": self.session_title(session) });
        }
        let surface = if away { chat::Surface::Away } else { chat::Surface::Mac };
        Some(chat::UiSnapshot::from_state(&state, surface, now).block())
    }

    /// What the "About: …" chip above a chat box says (none when nothing is open to be about).
    pub(crate) fn chat_about(&self) -> Option<String> {
        if self.mchat.focus_off {
            return None;
        }
        match self.view {
            crate::shell::View::Sessions => self.active_session().map(|s| format!("About: the session “{}”", self.session_title(s))),
            crate::shell::View::Manager | crate::shell::View::FirstRun => None,
            view => Some(format!("About: {}", crate::capabilities::Place::View(view).name())),
        }
    }

    /// The chat's thread for an agent, started with the first question that needs it: the
    /// manager's own, or the other one asked instead (its own thread, which never links its
    /// conversation into the store's: that stays the manager's own agent's).
    fn chat_worker(&mut self, agent: AgentKind, cx: &mut Context<Self>) -> Result<mpsc::Sender<Job>, String> {
        let own = agent == self.state.manager.brain;
        if own && let Some(worker) = &self.mchat.worker {
            return Ok(worker.clone());
        }
        if !own
            && let Some((started, worker)) = &self.mchat.other_worker
            && *started == agent
        {
            return Ok(worker.clone());
        }
        // A screen check (`GLASSCODE_DEBUG_NO_AGENTS`, debug builds) never starts Claude or Codex.
        if crate::workspace::no_agents() {
            return Err("its agent does not start in this test copy (GLASSCODE_DEBUG_NO_AGENTS is set)".into());
        }
        let host = self.brain_host.clone().ok_or("the manager's brain runs with the v2 engine only")?;
        let spec = self.brain_spec_for(Lane::Chat, None, agent).ok_or_else(|| if own { format!("{} is not installed: Settings › Manager chooses the agent that thinks for it.", agent.name()) } else { format!("{} is not installed, so it cannot answer instead.", agent.name()) })?;
        let (jobs, inbox) = mpsc::channel::<Job>();
        let (events, arrivals) = async_channel::unbounded::<Event>();
        let (hold, locks) = (self.lanes.hold.clone(), self.lanes.locks.clone());
        std::thread::Builder::new().name("manager-chat".into()).spawn(move || chat_thread(inbox, events, host, spec, hold, locks, own)).map_err(|e| format!("the manager's brain could not start ({e})"))?;
        cx.spawn(async move |this, cx| {
            while let Ok(event) = arrivals.recv().await {
                if this.update(cx, |ws, cx| ws.chat_event(event, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        if own {
            self.mchat.worker = Some(jobs.clone());
        } else {
            self.mchat.other_worker = Some((agent, jobs.clone()));
        }
        Ok(jobs)
    }

    /// The chat's brain starts again with the manager's settings (its agent, account or model
    /// changed): the next question starts it.
    pub(crate) fn chat_restart(&mut self) {
        self.mchat.worker = None;
        self.mchat.other_worker = None;
        // Another agent thinks for it now: the old one's failure line goes (its held questions are
        // due at once: they waited for that agent's limit, `Held::is_for`).
        if self.mchat.failure_agent.is_some_and(|a| a != self.state.manager.brain) {
            self.mchat.failure = None;
            self.mchat.failure_agent = None;
            self.mchat.other_offer = None;
        }
        let agent = self.state.manager.brain;
        let found = glasscode_core::agents::resolve_program(if agent == glasscode_core::model::AgentKind::Codex { "codex" } else { "claude" }, &self.env).is_some();
        self.mchat.ready = Some(if found || crate::workspace::no_agents() { Ok(()) } else { Err(format!("{} is not installed", agent.name())) });
    }

    fn chat_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Stop(handle) => {
                if let Some(turn) = &mut self.mchat.turn {
                    // A Stop pressed before the turn began is for it.
                    if turn.stopping {
                        handle.store(true, Ordering::SeqCst);
                    }
                    turn.stop = Some(handle);
                }
            }
            Event::Text(text) => {
                if let Some(turn) = &mut self.mchat.turn {
                    turn.text.push_str(&text);
                    turn.activity = None;
                    turn.first_text_ms.get_or_insert(turn.sent.elapsed().as_millis() as u64);
                }
                self.manager.page.chat_scroll.scroll_to_bottom();
            }
            Event::Tool(name) => {
                if let Some(turn) = &mut self.mchat.turn {
                    turn.activity = Some(chat::activity_line(&name));
                }
            }
            Event::Began { brain, rolled } => self.chat_write(Box::new(move |conversation| {
                let linked = match &rolled {
                    Some(rolled) => glasscode_core::runtime::lanes::link_rollover(conversation, &Rolled { to: Some(brain.clone()), ..rolled.clone() }, None).map(drop),
                    None => conversation.roll_over(&brain, None, None).map(drop),
                };
                if let Err(err) = linked {
                    log::warn!("manager: the brain's new conversation could not be linked: {err}");
                }
            })),
            Event::Done(result, health) => self.chat_done(result, health, cx),
        }
        cx.notify();
    }

    fn chat_done(&mut self, result: Result<Outcome, (String, chat::Failure)>, health: Option<glasscode_core::runtime::BrainHealth>, cx: &mut Context<Self>) {
        let Some(turn) = self.mchat.turn.take() else { return };
        self.manager.thinking = false;
        // Why it cannot answer, shown before the person types again (ux-15); gone once it answers.
        // An answer from the other agent leaves the manager's own agent's line as it was: that one
        // is still out.
        let failure = match &result {
            Ok(Outcome { end: TurnEnd::Answered(_), .. }) => None,
            Ok(Outcome { end: TurnEnd::Failed(why), .. }) => chat::failure_of(health.as_ref(), Some(why)),
            Ok(_) => chat::failure_of(health.as_ref(), None),
            Err((_, failure)) => Some(failure.clone()),
        };
        if turn.agent == self.state.manager.brain || failure.is_some() {
            self.mchat.failure = failure;
            self.mchat.failure_agent = Some(turn.agent);
        }
        // The time budget, measured (ux-m3).
        let ended = match &result {
            Ok(Outcome { end: TurnEnd::Answered(_), .. }) => "answered",
            Ok(Outcome { end: TurnEnd::Stopped, .. }) => "stopped",
            _ => "failed",
        };
        glasscode_core::journal::append(chat::turn_event(turn.first_text_ms, turn.sent.elapsed().as_millis() as u64, ended, turn.agent));
        // The plan's limit is used up: the question waits for it to reset and is asked again then,
        // by itself (ux-15). The person hears when, in place of the error.
        let now_s = (glasscode_core::clock::now_ms() / 1000) as i64;
        // A question the other agent could not answer either goes back with the held ones.
        let hold = match &result {
            Ok(Outcome { end: TurnEnd::Answered(_) | TurnEnd::Stopped, .. }) => None,
            _ if turn.agent == self.state.manager.brain => self.mchat.failure.as_ref().and_then(|failure| chat::hold_until(failure, now_s)),
            _ => self.chat_holding(now_s),
        };
        self.chat_note_other();
        if let Some((until, known)) = hold {
            self.chat_hold_question(turn.question.clone(), until, known, turn.on_answer, cx);
            if let Some(next) = self.mchat.queue.take_next() {
                self.chat_send_now(next, None, false, None, cx);
            }
            return;
        }
        // An answer clears the question left unanswered; a failure (not held) leaves this one so,
        // for the other agent to answer.
        match &result {
            Ok(Outcome { end: TurnEnd::Answered(_), .. }) => self.mchat.unanswered = None,
            Ok(Outcome { end: TurnEnd::Stopped, .. }) => {}
            _ => self.mchat.unanswered = Some(turn.question.clone()),
        }
        let result = result.map_err(|(why, _)| why);
        let answer = match result {
            Ok(Outcome { end: TurnEnd::Answered(text), .. }) => {
                let refs = self.chat_refs(&text);
                self.chat_post(Draft { author: Author::Chat, kind: Kind::Manager { state: Stream::Done }, text: text.clone(), refs, topic: None });
                // Said as v1 said its answers: in the caption, and aloud when the voice is on.
                let _ = turn.voice;
                self.manager_caption(&text, true, cx);
                Ok(text)
            }
            Ok(Outcome { end: TurnEnd::Stopped, .. }) => {
                let text = if turn.text.trim().is_empty() { "(stopped before it said anything)".to_string() } else { turn.text.clone() };
                self.chat_post(Draft { author: Author::Chat, kind: Kind::Manager { state: Stream::Stopped }, text, refs: Vec::new(), topic: None });
                Err("stopped".to_string())
            }
            Ok(Outcome { end: TurnEnd::Failed(why), .. }) => {
                let text = format!("The answer did not come: {why}.");
                self.chat_post(Draft { author: Author::App, kind: Kind::Error, text: text.clone(), refs: Vec::new(), topic: None });
                // Who asked from a command or by voice hears why in the person's words (M52.2's
                // failure states), as the chat's own line above its box says it: "signed out, sign
                // in again", not the CLI's "Please run /login".
                match self.mchat.failure.as_ref() {
                    Some(failure) => Err(chat::failure_view(failure, turn.agent).sentence),
                    None => Err(why),
                }
            }
            Err(why) => {
                self.chat_post(Draft { author: Author::App, kind: Kind::Error, text: why.clone(), refs: Vec::new(), topic: None });
                Err(why)
            }
        };
        if let Some(on_answer) = turn.on_answer {
            on_answer(self, answer, cx);
        }
        // The next queued message goes now.
        if let Some(next) = self.mchat.queue.take_next() {
            self.chat_send_now(next, None, false, None, cx);
        }
    }

    /// Above a chat box: its "/" or "@" menu, the chip of attached text, and the editing note.
    pub(crate) fn render_chat_box_extras(&self, input: Option<&gpui::Entity<crate::text_input::TextInput>>, key: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu_here = input.is_some_and(|input| self.mchat.menu_for.as_ref() == Some(input)) && !self.mchat.menu.is_empty();
        let attached = self.mchat.attached.clone();
        let editing = self.mchat.editing.is_some();
        if !menu_here && attached.is_none() && !editing {
            return None;
        }
        let menu = menu_here.then(|| {
            div()
                .id(SharedString::from(format!("{key}-menu")))
                .role(Role::ListBox)
                .aria_label("Suggestions")
                .flex()
                .flex_col()
                .rounded(px(8.))
                .border_1()
                .border_color(theme::border())
                .bg(theme::panel())
                .overflow_hidden()
                .children(self.mchat.menu.iter().enumerate().map(|(n, s)| {
                    div()
                        .id(SharedString::from(format!("{key}-menu-{n}")))
                        .role(Role::ListBoxOption)
                        .aria_label(format!("{}: {}", s.label, s.detail))
                        .flex()
                        .flex_col()
                        .px_2p5()
                        .py_1()
                        .cursor_pointer()
                        .hover(|d| d.bg(theme::tint()))
                        .child(div().text_size(px(12.5)).text_color(theme::text()).overflow_hidden().whitespace_nowrap().text_ellipsis().child(s.label.clone()))
                        .child(div().text_size(px(11.)).text_color(theme::muted()).overflow_hidden().whitespace_nowrap().text_ellipsis().child(s.detail.clone()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.chat_menu_take(n, window, cx)))
                }))
        });
        let chip = attached.map(|attached| {
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(px(11.5))
                .text_color(theme::text_soft())
                .child(div().px_1p5().rounded(px(5.)).bg(theme::tint()).child(attached.chip()))
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme::muted()).child(format!("from {}", attached.source)))
                .child(
                    div()
                        .id(SharedString::from(format!("{key}-unattach")))
                        .role(Role::Button)
                        .aria_label(format!("Send without the {}", attached.chip()))
                        .flex_none()
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::text()))
                        .child("×")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.mchat.attached = None;
                            cx.notify();
                        })),
                )
        });
        let note = editing.then(|| div().text_size(px(11.5)).text_color(theme::muted()).child("Editing a queued message: Enter keeps it in its place, Esc leaves it as it was."));
        Some(div().flex().flex_col().gap_1p5().px_3().pt_1p5().children(menu).children(chip).children(note).into_any_element())
    }

    /// The chat's failure line (ux-15), if the brain cannot think.
    fn render_chat_failure(&self, key: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        let failure = self.mchat.failure.clone().or_else(|| match &self.mchat.ready {
            Some(Err(_)) => Some(chat::Failure::Health(glasscode_core::runtime::BrainHealth::NotInstalled)),
            _ => None,
        })?;
        let failing = self.mchat.failure_agent.unwrap_or(self.state.manager.brain);
        let view = chat::failure_view(&failure, failing);
        let recovery = view.recovery;
        // The other CLI's account, one click away, when the manager's own agent is out (ux-15).
        let waiting = self.mchat.held.as_ref().is_some_and(|h| !h.held.is_empty()) || self.mchat.unanswered.is_some();
        let other = self.mchat.other_offer.filter(|_| waiting && failing == self.state.manager.brain && chat::other_helps(&failure));
        let other_button = other.map(|other| {
            let account = self.state.default_account(other).map(|a| a.label.clone()).filter(|l| !l.trim().is_empty());
            let aria = match &account {
                Some(label) => format!("{} (its account {label})", chat::other_label(other)),
                None => chat::other_label(other),
            };
            div()
                .id(SharedString::from(format!("{key}-other")))
                .role(Role::Button)
                .aria_label(aria)
                .flex_none()
                .px_2p5()
                .py_0p5()
                .rounded(px(6.))
                .border_1()
                .border_color(theme::border_strong())
                .text_size(px(12.))
                .text_color(theme::text())
                .cursor_pointer()
                .hover(|s| s.bg(theme::tint()))
                .child(chat::other_label(other))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.chat_ask_other(other, cx)))
        });
        let button = (recovery != chat::Recovery::Wait).then(|| {
            div()
                .id(SharedString::from(format!("{key}-recover")))
                .role(Role::Button)
                .aria_label(recovery.label())
                .flex_none()
                .px_2p5()
                .py_0p5()
                .rounded(px(6.))
                .border_1()
                .border_color(theme::border_strong())
                .text_size(px(12.))
                .text_color(theme::text())
                .cursor_pointer()
                .hover(|s| s.bg(theme::tint()))
                .child(recovery.label())
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.chat_recover(recovery, window, cx)))
        });
        Some(
            div()
                .id(SharedString::from(format!("{key}-failure")))
                .role(Role::Alert)
                .aria_label(view.sentence.clone())
                .flex()
                .flex_col()
                .gap_1()
                .px_3()
                .py_2()
                .rounded(px(10.))
                .border_1()
                .border_color(theme::attention())
                .text_size(px(12.5))
                .text_color(theme::text())
                .child(view.sentence.clone())
                .child(div().text_size(px(11.5)).text_color(theme::muted()).child(view.still_works))
                .when(button.is_some() || other_button.is_some(), |d| d.child(div().flex().gap_2().pt_1().children(button).children(other_button)))
                .into_any_element(),
        )
    }

    /// The failure line's way out.
    fn chat_recover(&mut self, recovery: chat::Recovery, window: &mut Window, cx: &mut Context<Self>) {
        use crate::settings_ui::Section;
        match recovery {
            chat::Recovery::SignIn => self.open_settings(Section::Accounts, window, cx),
            chat::Recovery::Updates => self.open_settings(Section::Updates, window, cx),
            chat::Recovery::ManagerSettings => self.open_settings(Section::Manager, window, cx),
            chat::Recovery::Retry => {
                self.mchat.failure = None;
                if let Some((text, words)) = self.mchat.last_question.clone() {
                    self.manager_ask_as(text, false, words, None, None, cx);
                }
            }
            chat::Recovery::Wait => {}
        }
        cx.notify();
    }

    /// Which other agent the failure line offers: looked for once, when a failure comes (the line
    /// is drawn every frame).
    fn chat_note_other(&mut self) {
        let main = self.state.manager.brain;
        self.mchat.other_offer = match &self.mchat.failure {
            Some(failure) if chat::other_helps(failure) && self.mchat.failure_agent.is_none_or(|a| a == main) && self.chat_other_may_read() => chat::other_agent(main).filter(|other| self.brain_spec_for(Lane::Chat, None, *other).is_some()),
            _ => None,
        };
    }

    /// Whether another account may think for the manager (D17): the gate lets the manager read a
    /// project bound to one account only when that is the manager's (`manager_account`, from
    /// Settings). The other agent's account is not, so with a project bound to the manager's own
    /// account the other agent is not offered: it would read what only that account may.
    fn chat_other_may_read(&self) -> bool {
        let set = self.control_policy_set();
        let Some(mine) = set.manager_account.as_deref() else { return true };
        !self.control.project_keys.values().any(|key| set.policy.of(key).model_account.as_deref() == Some(mine))
    }

    /// The questions held until the limit resets, read the first time they are needed.
    fn chat_held(&mut self) -> &mut chat::HeldFile {
        self.mchat.held.get_or_insert_with(|| chat::HeldFile::load().unwrap_or_default())
    }

    fn chat_keep_held(&self) {
        let Some(kept) = self.mchat.held.clone() else { return };
        std::thread::spawn(move || {
            if let Err(err) = kept.save() {
                log::warn!("manager: the held questions could not be kept ({err})");
            }
        });
    }

    /// Until when questions wait for the limit of the manager's agent and account, if any still do.
    fn chat_holding(&mut self, now_s: i64) -> Option<(i64, bool)> {
        let (agent, account) = (self.state.manager.brain, self.state.manager.account.clone());
        self.chat_held().holding(now_s, agent, account.as_deref())
    }

    /// Hold a question until the limit resets: kept across restarts, said so in the conversation,
    /// and a command that waits for the answer is told where it will come. Every held question
    /// waits for the same reset.
    fn chat_hold_question(&mut self, question: Asked, until: i64, known: bool, on_answer: Option<OnAnswer>, cx: &mut Context<Self>) {
        let now_ms = glasscode_core::clock::now_ms();
        let held = chat::Held { text: question.text.clone(), words: question.words, composed: Some(question.composed), asked_ms: now_ms, until, known, agent: Some(self.state.manager.brain), account: self.state.manager.account.clone(), ..chat::Held::default() };
        self.chat_held().hold(held);
        if self.mchat.unanswered.as_ref().is_some_and(|u| u.text.trim() == question.text.trim()) {
            self.mchat.unanswered = None;
        }
        self.chat_keep_held();
        // The failure line stays (with its way out to the other agent) while questions wait.
        if self.mchat.failure.is_none() {
            self.mchat.failure = Some(chat::Failure::Health(glasscode_core::runtime::BrainHealth::OutOfUsage { until: known.then_some(until) }));
            self.mchat.failure_agent = Some(self.state.manager.brain);
            self.chat_note_other();
        }
        let line = chat::hold_line(until, known, (now_ms / 1000) as i64);
        self.chat_post(Draft { author: Author::App, kind: Kind::Manager { state: Stream::Done }, text: line.clone(), refs: Vec::new(), topic: None });
        if let Some(on_answer) = on_answer {
            on_answer(self, Err(format!("{line} The answer comes in the conversation.")), cx);
        }
    }

    /// A held question whose limit has reset is asked again, one at a time, when the chat is free
    /// (v2's round calls this).
    pub(crate) fn chat_held_tick(&mut self, cx: &mut Context<Self>) {
        if !self.control.engine_v2 || self.mchat.turn.is_some() || !self.mchat.queue.is_empty() {
            return;
        }
        let now_s = (glasscode_core::clock::now_ms() / 1000) as i64;
        let (agent, account) = (self.state.manager.brain, self.state.manager.account.clone());
        let Some(due) = self.chat_held().due(now_s, agent, account.as_deref()).cloned() else { return };
        self.chat_held().take(&due.text);
        self.chat_keep_held();
        self.mchat.failure = None;
        self.chat_post_line(true, format!("The limit reset: answering what you asked at {}, “{}”.", glasscode_core::day::clock((due.asked_ms / 1000) as i64), short_line(&due.text)));
        let composed = due.composed.clone().unwrap_or_else(|| due.text.clone());
        self.chat_send_to(Queued { id: 0, text: due.text, words: due.words }, None, false, None, Some(composed), agent, cx);
        cx.notify();
    }

    /// "Ask Codex instead": the oldest held question (else the one no answer came to) goes to the
    /// other agent's account now, as it was first asked; the manager's own agent stays as it is
    /// (ux-15). Nothing waits: nothing is asked.
    fn chat_ask_other(&mut self, other: AgentKind, cx: &mut Context<Self>) {
        if self.mchat.turn.is_some() {
            return;
        }
        let held = self.chat_held().held.first().cloned();
        let question = match held {
            Some(held) => {
                self.chat_held().take(&held.text);
                self.chat_keep_held();
                let composed = held.composed.unwrap_or_else(|| held.text.clone());
                Asked { text: held.text, words: held.words, composed }
            }
            None => match self.mchat.unanswered.take() {
                Some(question) => question,
                None => return,
            },
        };
        self.chat_post_line(true, format!("Asked {} instead: “{}”.", other.name(), short_line(&question.text)));
        self.chat_send_to(Queued { id: 0, text: question.text, words: question.words }, None, false, None, Some(question.composed), other, cx);
        cx.notify();
    }

    /// An op the manager ran, as a line of the conversation (ux-14): who, what the op said it did,
    /// and Undo while its inverse can be built. Reads, and moves of the window, make none.
    pub(crate) fn chat_action_line(&mut self, op: &glasscode_core::ops::Op, caller: &glasscode_core::caller::Caller, playbook: Option<&str>, reply: &glasscode_core::control::Reply) {
        use glasscode_core::ops::EffectClass;
        let glasscode_core::control::Reply::Done { code, text, data } = reply else { return };
        if *code != glasscode_core::control::code::OK || matches!(op.spec.effect, EffectClass::Read | EffectClass::Navigate) || data.get("moved") == Some(&serde_json::Value::Bool(false)) {
            return;
        }
        let name = self.state.manager.name.clone();
        let (actor, author) = match (caller, playbook) {
            (_, Some(playbook)) => (format!("{name}, under the playbook {playbook}"), Author::Wake(String::new())),
            (glasscode_core::caller::Caller::Manager { lane: Lane::Wake, wake, .. }, None) => (format!("{name} (a wake)"), Author::Wake(wake.clone().unwrap_or_default())),
            _ => (name, Author::Chat),
        };
        let undo = chat::undo_op(op, data);
        let effect: String = glasscode_core::gate::op_hash(op).iter().map(|b| format!("{b:02x}")).collect();
        let what = text.lines().next().unwrap_or(op.spec.summary).trim_end_matches('.').to_string();
        let line = chat::action_line(&actor, &what, undo.is_some());
        if let Some(undo) = undo {
            // What it touched: a later change to any of it ends the Undo (ux-14).
            let targets = glasscode_core::ops::targets(op, &self.world(&crate::control_ui::Caller::you())).unwrap_or_default();
            let keys = chat::undo_keys(op.name(), &serde_json::to_value(op).ok().and_then(|v| v.get("args").cloned()).unwrap_or_default(), &targets);
            self.mchat.undos.insert(effect.clone(), chat::UndoEntry { op: undo, keys, of: glasscode_core::gate::op_hash(op), at_ms: glasscode_core::clock::now_ms() });
        }
        let refs = self.chat_refs(&line);
        self.chat_post(Draft { author, kind: Kind::Action { effect, event: None }, text: line, refs, topic: None });
    }

    /// A later change ends the Undo of an action that touched the same thing (ux-14): every op's
    /// record comes here (`journal_op`).
    pub(crate) fn chat_expire_undos(&mut self, action: &glasscode_core::ops::Action) {
        use glasscode_core::ops::EffectClass;
        if self.mchat.undos.is_empty() || action.code != glasscode_core::control::code::OK || matches!(action.effect, EffectClass::Read | EffectClass::Navigate) {
            return;
        }
        let Ok(op) = glasscode_core::ops::typed(&action.op, action.args.clone()) else { return };
        let (keys, hash) = (chat::undo_keys(&action.op, &action.args, &action.targets), glasscode_core::gate::op_hash(&op));
        self.mchat.undos.retain(|_, entry| !entry.ended_by(&keys, &hash));
    }

    /// The person's word on the nudges of a matter: they are done (closed) in the conversation.
    pub(crate) fn chat_close_nudges(&mut self, matter: &str) {
        let matter = matter.to_string();
        self.chat_write(Box::new(move |conversation| {
            let open: Vec<glasscode_core::conversation::ItemId> = conversation.items().iter().filter(|item| !item.closed && matches!(&item.kind, Kind::Nudge { matter: m } if *m == matter)).map(|item| item.id.clone()).collect();
            for id in open {
                let _ = conversation.close(&id);
            }
        }));
    }

    /// What the manager would have shown, kept as a "Show me" chip for the person (ux-5).
    pub(crate) fn chat_show_me(&mut self, op: &glasscode_core::ops::Op) {
        let text = format!("Show me {}", self.show_me_label(op));
        self.chat_post(Draft { author: Author::App, kind: Kind::Nudge { matter: chat::show_me_matter(op) }, text, refs: Vec::new(), topic: None });
    }

    /// What a "Show me" chip shows: the session by its title, or the place by its name (found in
    /// the journey run: the chip said the op's summary, "show a place in the window: a view, …").
    fn show_me_label(&self, op: &glasscode_core::ops::Op) -> String {
        let named = |word: &str| -> Option<String> {
            let word = word.trim();
            if word.is_empty() {
                return None;
            }
            if let Some(place) = crate::capabilities::Place::parse(word) {
                return Some(chat::show_me_place(&place.name()));
            }
            let id = self.find_session(word, &crate::control_ui::Caller::you()).ok()?;
            self.state.session(&id).map(|s| format!("the session “{}”", self.session_title(s)))
        };
        let label = match op.name() {
            "ui.open" => op.text("place").and_then(named),
            "ui.point" | "manager.show" => op.text("view").and_then(named).or_else(|| op.text("session").and_then(named)),
            _ => None,
        };
        label.unwrap_or_else(|| format!("where it would {}", op.spec.summary))
    }

    /// Stop the answer being written (the chat's Stop, Esc).
    pub(crate) fn chat_stop(&mut self, cx: &mut Context<Self>) {
        if let Some(turn) = &mut self.mchat.turn {
            turn.stopping = true;
            if let Some(stop) = &turn.stop {
                stop.store(true, Ordering::SeqCst);
            }
        }
        cx.notify();
    }

    /// The Today box's Session | Ask control, remembered: the person's `settings.set today.ask`.
    pub(crate) fn chat_set_today_mode(&mut self, mode: StartMode, window: &mut Window, cx: &mut Context<Self>) {
        self.run_op_named("settings.set", serde_json::json!({ "key": "today.ask", "value": (mode == StartMode::Ask).to_string() }), window, cx);
    }

    /// What Enter does in the Today box when the manager is on, as the person chose it.
    pub(crate) fn chat_today_mode(&self) -> StartMode {
        if self.state.prefs.today_ask { StartMode::Ask } else { StartMode::Session }
    }

    /// Whether the chat can answer now, and why not (for the Today box's control): its agent is
    /// looked for when the settings change (`chat_restart`), not each frame.
    pub(crate) fn chat_can_answer(&self) -> Result<(), String> {
        if !self.control.engine_v2 {
            return Err("the manager's chat here needs Manager v2's engine".into());
        }
        if self.brain_host.is_none() {
            return Err("the manager's brain is not ready yet".into());
        }
        self.mchat.ready.clone().unwrap_or(Ok(()))
    }

    /// What Enter does in the Today box now.
    pub(crate) fn chat_enter(&self) -> chat::Enter {
        chat::enter_meaning(self.state.manager.enabled && self.control.engine_v2, self.chat_can_answer(), self.chat_today_mode())
    }
}

/// The chat's thread: it owns the chat lane, and answers one question at a time.
/// `link`: its brain conversation is the store's (the manager's own agent); the other agent's,
/// asked instead, is never linked.
fn chat_thread(inbox: mpsc::Receiver<Job>, events: async_channel::Sender<Event>, host: Arc<crate::brain_host::AppHost>, spec: BrainSpec, hold: glasscode_core::runtime::lanes::Hold, locks: glasscode_core::runtime::lanes::Locks, link: bool) {
    // The events that link a brain conversation into the store; none from an unlinked thread.
    let began = |events: &async_channel::Sender<Event>, event: Event| {
        if link || !matches!(event, Event::Began { .. }) {
            let _ = events.send_blocking(event);
        }
    };
    let mut lane: Option<ChatLane> = None;
    // The brain conversation is linked in the store once its id is known (Codex's only after its
    // first turn); `resumed`: the lane loads one the store named, which may be gone.
    let (mut linked, mut resumed) = (false, false);
    while let Ok(Job::Ask { text, since, next }) = inbox.recv() {
        let host: &dyn Host = host.as_ref();
        let now = glasscode_core::clock::now_ms();
        let before = Ledger::load().unwrap_or_default();
        let mut ledger = before.clone();
        let fresh = |spec: &BrainSpec| {
            let mut spec = spec.clone();
            spec.resume = None;
            ChatLane::new(spec, hold.clone(), locks.clone())
        };
        let lane = lane.get_or_insert_with(|| {
            let mut spec = spec.clone();
            spec.resume = match &next {
                Next::Resume(brain) | Next::RollOver { brain, .. } => Some(brain.clone()),
                Next::First => None,
            };
            resumed = spec.resume.is_some();
            linked = resumed;
            ChatLane::new(spec, hold.clone(), locks.clone())
        });
        if !linked
            && let Some(brain) = lane.brain_id()
        {
            began(&events, Event::Began { brain, rolled: None });
            linked = true;
        }
        // A new day, an idle spell or a long conversation: a summary, then a fresh one.
        if matches!(next, Next::RollOver { .. })
            && let Some(rolled) = lane.roll_over(host, &mut ledger, now)
            && let Some(brain) = rolled.to.clone()
        {
            began(&events, Event::Began { brain, rolled: Some(rolled) });
        }
        let _ = events.send_blocking(Event::Stop(lane.stop_handle()));
        let sender = events.clone();
        let mut on_update = |update: &Update| match update {
            Update::Text(text) => {
                let _ = sender.send_blocking(Event::Text(text.clone()));
            }
            Update::ToolCall { name } => {
                let _ = sender.send_blocking(Event::Tool(name.clone()));
            }
            _ => {}
        };
        let mut result = lane.ask(host, &mut ledger, &text, &since, now, &mut on_update);
        // A conversation the store named that will not load (another account or agent since, or
        // its files gone) fails before its first line: a fresh one starts, linked, and answers.
        if resumed
            && matches!(&result, Ok((Outcome { end: TurnEnd::Failed(_), checked: false, .. }, _)))
        {
            lane.brain.stop();
            *lane = fresh(&spec);
            linked = false;
            if let Some(brain) = lane.brain_id() {
                began(&events, Event::Began { brain, rolled: None });
                linked = true;
            }
            let _ = events.send_blocking(Event::Stop(lane.stop_handle()));
            result = lane.ask(host, &mut ledger, &text, &since, now, &mut on_update);
        }
        resumed = false;
        if !linked
            && let Some(brain) = lane.brain_id()
        {
            began(&events, Event::Began { brain, rolled: None });
            linked = true;
        }
        if let Err(err) = glasscode_core::store::update::<Ledger, _>(|stored| stored.add_since(&before, &ledger)) {
            log::warn!("manager: the token ledger could not be kept: {err}");
        }
        let done = match result {
            Ok((outcome, rolled)) => {
                if let Some(rolled) = rolled
                    && let Some(brain) = rolled.to.clone()
                {
                    began(&events, Event::Began { brain, rolled: Some(rolled) });
                }
                Ok(outcome)
            }
            Err(glasscode_core::runtime::Refusal::Health(health)) => {
                let health = with_reset(health, &ledger, now);
                Err((health.sentence(spec.agent), chat::Failure::Health(health)))
            }
            Err(glasscode_core::runtime::Refusal::Budget(blocked)) => Err((blocked.sentence(), chat::Failure::Budget(blocked))),
        };
        if events.send_blocking(Event::Done(done, Some(with_reset(lane.brain.health.clone(), &ledger, now)))).is_err() {
            break;
        }
    }
    if let Some(mut lane) = lane {
        lane.brain.stop();
    }
}

/// A question shortened to one line for the chat's notes about it.
fn short_line(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(80) {
        Some((at, _)) => format!("{}…", line[..at].trim_end()),
        None => line,
    }
}

/// A used-up plan whose CLI did not say until when: the reset of the vendor's windows the ledger
/// read, when one is ahead (the hold's "at 14:20", ux-15).
fn with_reset(health: glasscode_core::runtime::BrainHealth, ledger: &Ledger, now_ms: u64) -> glasscode_core::runtime::BrainHealth {
    use glasscode_core::runtime::BrainHealth;
    match health {
        BrainHealth::OutOfUsage { until: None } => BrainHealth::OutOfUsage { until: chat::reset_from_windows(&ledger.windows, (now_ms / 1000) as i64) },
        other => other,
    }
}

// ----- the chat over the window (ux-3) ---------------------------------------------------------------

/// The newest item's time: what "seen" is measured against.
fn newest_ms(conversation: &Conversation) -> u64 {
    conversation.items().last().map(|item| item.posted.wall_ms).unwrap_or(0)
}

impl Workspace {
    /// Open or close the chat over the window: the view under it stays (`ui.state` names it), and
    /// its box takes the keys.
    /// `focus`: the person opened it, so its box takes the keys (a brain's `ui.panel chat` does
    /// not take them from wherever the person types). On the Manager page the page is the chat.
    pub(crate) fn chat_overlay_set(&mut self, open: bool, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.chat_start(cx);
        self.mchat.seen_ms = Some(newest_ms(&self.mchat.conversation));
        if open && self.view == crate::shell::View::Manager {
            self.mchat.overlay = false;
            if focus && let Some(input) = self.manager.input.clone() {
                crate::text_input::focus(&input, window, cx);
            }
            return cx.notify();
        }
        let was_focused = self.mchat.overlay_input.as_ref().is_some_and(|input| input.read(cx).focus_handle_clone().is_focused(window));
        self.mchat.overlay = open;
        if open {
            let input = self.chat_overlay_input(cx);
            if focus {
                // What the person had selected can go with their next message, as a chip they
                // see and can take off; never without it.
                if let Some(attached) = self.chat_take_selection(cx) {
                    self.mchat.attached = Some(attached);
                }
                crate::text_input::focus(&input, window, cx);
            }
            self.manager.page.chat_scroll.scroll_to_bottom();
        } else if was_focused {
            // Its box is gone: the keys go back to the window.
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    /// The chat's own box, made the first time it opens.
    fn chat_overlay_input(&mut self, cx: &mut Context<Self>) -> gpui::Entity<crate::text_input::TextInput> {
        if let Some(input) = &self.mchat.overlay_input {
            return input.clone();
        }
        let name = self.state.manager.name.clone();
        let input = cx.new(|cx| crate::text_input::TextInput::new(format!("Ask {name}"), cx).bare());
        cx.subscribe(&input, |this: &mut Workspace, _, event: &crate::text_input::InputEvent, cx| match event {
            crate::text_input::InputEvent::Submit => this.chat_overlay_send(cx),
            // Esc stops an answer being written, else closes the chat.
            crate::text_input::InputEvent::Cancel if this.mchat.editing.is_some() || !this.mchat.menu.is_empty() => this.chat_box_cancel(cx),
            crate::text_input::InputEvent::Cancel if this.mchat.turn.is_some() => this.chat_stop(cx),
            crate::text_input::InputEvent::Cancel => {
                this.mchat.overlay = false;
                cx.notify();
            }
            crate::text_input::InputEvent::Changed => {
                if let Some(input) = this.mchat.overlay_input.clone() {
                    this.chat_menu_update(&input, cx);
                }
            }
        })
        .detach();
        self.mchat.overlay_input = Some(input.clone());
        input
    }

    fn chat_overlay_send(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.mchat.overlay_input.clone() else { return };
        let (text, untyped) = {
            let input = input.read(cx);
            let all = input.text();
            let (text, untyped) = glasscode_core::words::trimmed(&all, input.typing());
            (text.to_string(), untyped)
        };
        if text.is_empty() {
            return;
        }
        input.update(cx, |i, cx| i.set_text("", cx));
        if self.chat_save_edit(&text, cx) {
            return;
        }
        // Typed in the manager's chat: the person's words, where they typed them (D1).
        let words = glasscode_core::words::record_with(glasscode_core::words::Origin::Composer(glasscode_core::words::ComposerKind::ManagerChat), &text, &untyped, None, None);
        self.manager_ask_as(text, false, Some(words), None, None, cx);
        cx.notify();
    }

    /// A box's text changed: its "/" or "@" menu (ux-18).
    pub(crate) fn chat_menu_update(&mut self, input: &gpui::Entity<crate::text_input::TextInput>, cx: &mut Context<Self>) {
        let text = input.read(cx).text().to_string();
        let menu = if text.starts_with('/') {
            chat::slash_suggestions(&text, self.control.engine_v2)
        } else if chat::at_query(&text).is_some() {
            chat::at_suggestions(&text, &self.chat_nameable())
        } else {
            Vec::new()
        };
        self.mchat.menu_for = (!menu.is_empty()).then(|| input.clone());
        self.mchat.menu = menu;
        cx.notify();
    }

    /// What "@" can name: the sessions (newest first), the pull requests the status rounds know,
    /// and the projects.
    fn chat_nameable(&self) -> Vec<chat::Named> {
        let mut sessions: Vec<&glasscode_core::model::Session> = self.state.sessions.iter().collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s.created_at));
        let mut named: Vec<chat::Named> = sessions.into_iter().map(|s| chat::Named::session(&s.id, &self.session_title(s))).collect();
        let mut prs: Vec<(String, u64, String)> = self.prs.map.values().filter_map(|status| Some((status.repo.clone()?, status.pr.as_ref()?.number, status.pr.as_ref()?.title.clone()))).collect();
        prs.sort();
        prs.dedup();
        named.extend(prs.into_iter().map(|(repo, number, title)| chat::Named::pr(&repo, number, &title)));
        named.extend(self.state.visible_projects().map(|p| chat::Named::project(&p.name)));
        named
    }

    /// A menu line chosen: its text in the box, the box keeps the keys.
    fn chat_menu_take(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(input), Some(choice)) = (self.mchat.menu_for.clone(), self.mchat.menu.get(n).cloned()) else { return };
        input.update(cx, |i, cx| i.set_text(choice.text.clone(), cx));
        crate::text_input::focus(&input, window, cx);
        self.mchat.menu.clear();
        cx.notify();
    }

    /// Esc in a box with a menu open or a queued message being edited: that goes, the box stays.
    pub(crate) fn chat_box_cancel(&mut self, cx: &mut Context<Self>) {
        if !self.mchat.menu.is_empty() {
            self.mchat.menu.clear();
        } else if self.mchat.editing.take().is_some() {
            for input in [self.mchat.overlay_input.clone(), self.manager.input.clone()].into_iter().flatten() {
                input.update(cx, |i, cx| i.set_text("", cx));
            }
        }
        cx.notify();
    }

    /// Edit a queued message in the box that is open (the chat over the window, else the page's).
    fn chat_edit_queued(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(queued) = self.mchat.queue.items().find(|q| q.id == id).cloned() else { return };
        let input = match (self.mchat.overlay, self.mchat.overlay_input.clone(), self.manager.input.clone()) {
            (true, Some(input), _) => input,
            (_, _, Some(input)) => input,
            (_, Some(input), _) => input,
            _ => return,
        };
        input.update(cx, |i, cx| i.set_text(queued.text.clone(), cx));
        crate::text_input::focus(&input, window, cx);
        self.mchat.editing = Some(id);
        cx.notify();
    }

    /// The text of a box sent while a queued message was being edited: it takes that message's
    /// place (true), unless it went meanwhile (then it is a new message).
    pub(crate) fn chat_save_edit(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let Some(id) = self.mchat.editing.take() else { return false };
        let saved = self.mchat.queue.edit(id, text.to_string());
        cx.notify();
        saved
    }

    /// The text selected where the person is (a session's terminal or its Chat view), as an
    /// attachment for the chip: taken only when they open the chat themselves (ux-5).
    fn chat_take_selection(&self, cx: &Context<Self>) -> Option<chat::Attached> {
        if self.view != crate::shell::View::Sessions {
            return None;
        }
        let session = self.active_session()?;
        let title = self.session_title(session);
        let (source, text) = match self.session_view(session) {
            glasscode_core::model::SessionView::Chat => (format!("the conversation of “{title}”"), self.chat_view_selection(&session.id)?),
            glasscode_core::model::SessionView::Terminal => (format!("the terminal of “{title}”"), self.terminals.get(&session.id)?.read(cx).selected_text()?),
        };
        chat::Attached::new(source, &text)
    }

    /// What the manager said since the chat was last open (its answers, its wakes', Glasscode's),
    /// and the cards that wait for the person: the bubble's count and mark.
    pub(crate) fn chat_unread(&self) -> (usize, usize) {
        let seen = self.mchat.seen_ms.unwrap_or(u64::MAX);
        let unread = self.mchat.items.iter().filter(|item| item.posted.wall_ms > seen && !matches!(item.author, Author::Person) && matches!(item.kind, Kind::Manager { .. } | Kind::Nudge { .. } | Kind::Card { .. } | Kind::Error)).count();
        // What waits for the person is the approvals that are open, not the cards' items.
        (unread, self.control.approvals.len())
    }

    /// A card's pending action still waits for the person.
    fn chat_card_waits(&self, pending: &str) -> bool {
        self.control.approvals.iter().any(|a| a.pending.as_ref().is_some_and(|p| p.id == pending))
    }

    /// The chat over the window: its newest exchanges, the answer being written, a box, and the
    /// way to the full conversation. Anchored above the bubble.
    pub(crate) fn render_chat_overlay(&self, cx: &mut Context<Self>) -> AnyElement {
        let name = self.state.manager.name.clone();
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme::border())
            .child(div().flex_1().min_w_0().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(name.clone()))
            .child(
                div()
                    .id("chat-overlay-full")
                    .role(Role::Link)
                    .aria_label("Open the full conversation")
                    .flex_none()
                    .text_size(px(11.5))
                    .text_color(theme::text_soft())
                    .underline()
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme::text()))
                    .child("Open the full conversation")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.mchat.overlay = false;
                        this.show_manager(window, cx);
                    })),
            )
            .child(
                div()
                    .id("chat-overlay-close")
                    .role(Role::Button)
                    .aria_label("Close the chat")
                    .flex_none()
                    .px_1()
                    .text_color(theme::muted())
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme::text()))
                    .child("×")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.run_op_named("ui.panel", serde_json::json!({ "side": "chat", "hide": true }), window, cx))),
            );
        let thread = div()
            .id("chat-overlay-scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.manager.page.chat_scroll)
            .p_3()
            .child(self.render_chat_thread(ThreadView { id: "overlay-thread", limit: 12, compact: true }, cx));
        // What the next message is about (ux-5): names only; the person can take it off.
        let about = self.chat_about().map(|about| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1p5()
                .px_3()
                .pt_1p5()
                .text_size(px(11.5))
                .text_color(theme::muted())
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(about.clone()))
                .child(
                    div()
                        .id("chat-overlay-about-off")
                        .role(Role::Button)
                        .aria_label(format!("Send without: {about}"))
                        .flex_none()
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::text()))
                        .child("×")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.mchat.focus_off = true;
                            cx.notify();
                        })),
                )
        });
        let composer = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .p_2()
            .border_t_1()
            .border_color(theme::border())
            .child(div().id("chat-overlay-input").role(Role::TextInput).aria_label(format!("Ask {name}")).flex().flex_1().min_w_0().items_center().h(px(32.)).px_3().rounded_full().border_1().border_color(theme::border_strong()).bg(theme::input()).children(self.mchat.overlay_input.clone()))
            .child(
                div()
                    .id("chat-overlay-send")
                    .role(Role::Button)
                    .aria_label(format!("Send to {name}"))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(30.))
                    .rounded_full()
                    .bg(theme::accent())
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.88))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.chat_overlay_send(cx)))
                    .child(crate::icons::icon(crate::icons::SEND, px(14.), px(14.), theme::on_accent())),
            );
        div()
            .id("chat-overlay")
            .role(Role::Dialog)
            .aria_label(format!("Chat with {name}"))
            .occlude()
            .flex()
            .flex_col()
            .w(px(380.))
            .h(px(480.))
            .rounded(px(14.))
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .shadow_lg()
            .overflow_hidden()
            .child(header)
            .child(thread)
            .children(self.render_chat_box_extras(self.mchat.overlay_input.as_ref(), "chat-overlay", cx))
            .children(about)
            .child(composer)
            .into_any_element()
    }

    /// The topics of the Manager page's thread: all of them, or one (M43's topics: a PR's, a
    /// session's, the general one), newest first.
    pub(crate) fn render_chat_topics(&self, cx: &mut Context<Self>) -> AnyElement {
        let conversation = &self.mchat.conversation;
        let mut topics: Vec<(glasscode_core::conversation::TopicId, String, u64)> = conversation
            .topics()
            .into_iter()
            .map(|topic| {
                let last = conversation.topic_items(&topic.id).iter().map(|item| item.posted.wall_ms).max().unwrap_or(0);
                (topic.id.clone(), topic.title.clone(), last)
            })
            .filter(|(_, _, last)| *last > 0)
            .collect();
        topics.sort_by(|a, b| b.2.cmp(&a.2));
        let chosen = self.mchat.topic.clone();
        let chip = |id: SharedString, label: String, on: bool, topic: Option<glasscode_core::conversation::TopicId>, cx: &mut Context<Self>| {
            div()
                .id(id)
                .role(Role::Tab)
                .aria_label(format!("Topic: {label}"))
                .aria_selected(on)
                .flex_none()
                .max_w(px(180.))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .px_2()
                .py_0p5()
                .rounded(px(6.))
                .text_size(px(12.))
                .when(on, |d| d.bg(theme::tint()).text_color(theme::text()).font_weight(FontWeight::MEDIUM))
                .when(!on, |d| d.text_color(theme::text_soft()).cursor_pointer().hover(|s| s.text_color(theme::text())))
                .child(label)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.mchat.topic = topic.clone();
                    this.manager.page.chat_scroll.scroll_to_bottom();
                    cx.notify();
                }))
        };
        let mut row: Vec<AnyElement> = vec![chip("chat-topic-all".into(), "All".into(), chosen.is_none(), None, cx).into_any_element()];
        if topics.len() > 1 {
            for (n, (id, title, _)) in topics.into_iter().take(12).enumerate() {
                let on = chosen.as_ref() == Some(&id);
                row.push(chip(SharedString::from(format!("chat-topic-{n}")), title, on, Some(id), cx).into_any_element());
            }
        }
        div().id("chat-topics").role(Role::TabList).aria_label("Topics").flex().flex_none().flex_wrap().gap_1().px_4().pt_3().children(row).into_any_element()
    }
}

// ----- the thread, drawn ---------------------------------------------------------------------------

/// What a thread shows: its newest items, and where it is mounted (ids stay unique per mount).
pub struct ThreadView<'a> {
    pub id: &'a str,
    pub limit: usize,
    /// Narrow (under the Today box): smaller type.
    pub compact: bool,
}

impl Workspace {
    /// The conversation's newest items, the answer being written, and the queued messages: one
    /// component, on the Manager page and under the Today box. A log for screen readers, whose
    /// finished messages are its children (ux-16).
    pub(crate) fn render_chat_thread(&self, view: ThreadView, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        // The Manager page shows the topic chosen there; the other mounts show them all.
        let topic = self.mchat.topic.as_ref().filter(|_| view.id == "manager-thread");
        let items: Vec<&Item> = match topic {
            Some(topic) => self.mchat.conversation.topic_items(topic).into_iter().filter(|item| !matches!(item.kind, Kind::Summary { .. } | Kind::Rollover { .. })).collect(),
            None => self.mchat.items.iter().collect(),
        };
        let from = items.len().saturating_sub(view.limit);
        let size = if view.compact { 13. } else { 13.5 };
        let name = self.state.manager.name.clone();
        let mut out: Vec<AnyElement> = Vec::new();
        // Why it cannot think, before the person types (ux-15): its sentence, what still works,
        // and one way out.
        if let Some(banner) = self.render_chat_failure(view.id, cx) {
            out.push(banner);
        }
        for (i, item) in items.iter().enumerate().skip(from) {
            out.push(self.render_chat_item(item, &format!("{}-{i}", view.id), size, view.compact, cx));
        }
        if out.is_empty() && self.mchat.turn.is_none() {
            out.push(div().text_size(px(12.5)).text_color(theme::muted()).child(format!("Ask {name} anything about your sessions, reviews and follow-ups; “what can you do?” lists it all.")).into_any_element());
        }
        if let Some(turn) = &self.mchat.turn {
            out.push(self.render_chat_turn(turn, view.id, size, cx));
        }
        for queued in self.mchat.queue.items() {
            let id = queued.id;
            out.push(
                div()
                    .id(SharedString::from(format!("{}-queued-{id}", view.id)))
                    .role(Role::ListItem)
                    .aria_label(format!("Queued: {}", queued.text))
                    .flex()
                    .self_end()
                    .items_center()
                    .gap_2()
                    .max_w(relative(0.92))
                    .px_3()
                    .py_1p5()
                    .rounded(px(12.))
                    .border_1()
                    .border_dashed()
                    .border_color(theme::border_strong())
                    .text_size(px(size))
                    .text_color(theme::text_soft())
                    .child(div().flex_none().text_size(px(11.)).text_color(theme::muted()).child("Queued"))
                    .child(div().flex_1().min_w_0().child(queued.text.clone()))
                    .child(
                        div()
                            .id(SharedString::from(format!("{}-edit-{id}", view.id)))
                            .role(Role::Button)
                            .aria_label("Edit this queued message")
                            .flex_none()
                            .text_size(px(11.5))
                            .underline()
                            .cursor_pointer()
                            .text_color(theme::muted())
                            .hover(|s| s.text_color(theme::text()))
                            .child(if self.mchat.editing == Some(id) { "Editing" } else { "Edit" })
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.chat_edit_queued(id, window, cx))),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("{}-unqueue-{id}", view.id)))
                            .role(Role::Button)
                            .aria_label("Remove this queued message")
                            .flex_none()
                            .cursor_pointer()
                            .text_color(theme::muted())
                            .hover(|s| s.text_color(theme::text()))
                            .child("×")
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.mchat.queue.remove(id);
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            );
        }
        div().id(SharedString::from(format!("{}-log", view.id))).role(Role::Log).aria_label(format!("Conversation with {name}")).flex().flex_col().gap(px(10.)).children(out)
    }

    fn render_chat_item(&self, item: &Item, key: &str, size: f32, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let bubble = div().flex().flex_col().flex_none().gap(px(6.)).px_3().py_2().rounded(px(12.)).text_size(px(size)).line_height(px(size * 1.45));
        match &item.kind {
            Kind::You { .. } | Kind::V1 { who: glasscode_core::manager::Who::You } => bubble.self_end().max_w(relative(0.92)).bg(theme::selected()).text_color(theme::text_strong()).child(item.text.clone()).into_any_element(),
            Kind::Manager { state } => {
                let mut sel = crate::chat_select::Selection::default();
                // A long answer in a small surface: its start, and "Read more" (ux-20).
                let clipped = compact.then(|| chat::clip_long(&item.text, chat::READ_MORE_AT)).flatten();
                let open = self.mchat.expanded.contains(&item.id);
                let shown = match (&clipped, open) {
                    (Some(start), false) => start.clone(),
                    _ => item.text.clone(),
                };
                let reply = crate::chat_ui::render_reply(&shown, key, &mut sel, cx.entity().downgrade());
                let more = clipped.map(|_| {
                    let id = item.id.clone();
                    div()
                        .id(SharedString::from(format!("{key}-more")))
                        .role(Role::Button)
                        .aria_label(if open { "Show less" } else { "Read more" })
                        .aria_expanded(open)
                        .text_size(px(11.5))
                        .underline()
                        .text_color(theme::text_soft())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::text()))
                        .child(if open { "Show less" } else { "Read more" })
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if !this.mchat.expanded.remove(&id) {
                                this.mchat.expanded.insert(id.clone());
                            }
                            cx.notify();
                        }))
                });
                bubble
                    .self_start()
                    .max_w(relative(0.92))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::panel())
                    .text_color(theme::text())
                    .child(reply)
                    .children(more)
                    .when(*state == Stream::Stopped, |d| d.child(div().text_size(px(11.)).text_color(theme::muted()).child("Stopped")))
                    .child(self.render_chips(&item.text, key, cx))
                    .into_any_element()
            }
            Kind::Error => bubble.self_stretch().bg(theme::error_bg()).text_color(theme::error_text()).text_size(px(size - 1.)).child(item.text.clone()).into_any_element(),
            // An approval card, inline (M21.2): Allow and Deny while it waits (ux-14, M52.2).
            Kind::Card { pending } if self.chat_card_waits(pending) => {
                let number = self.control.approvals.iter().find(|a| a.pending.as_ref().is_some_and(|p| &p.id == pending)).map(|a| a.id).unwrap_or_default();
                let decide = |answer: &'static str, label: &'static str, cx: &mut Context<Self>| {
                    div()
                        .id(SharedString::from(format!("{key}-{answer}")))
                        .role(Role::Button)
                        .aria_label(format!("{label}: {}", item.text))
                        .px_2p5()
                        .py_0p5()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(theme::border_strong())
                        .text_size(px(12.))
                        .when(answer == "allow", |d| d.bg(theme::accent()).text_color(theme::on_accent()).border_color(theme::accent()))
                        .when(answer != "allow", |d| d.text_color(theme::text_soft()))
                        .cursor_pointer()
                        .hover(|s| s.opacity(0.88))
                        .child(label)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.run_op_named("approvals.decide", serde_json::json!({ "id": number.to_string(), "answer": answer }), window, cx)))
                };
                bubble
                    .self_stretch()
                    .border_1()
                    .border_color(theme::attention())
                    .text_color(theme::text())
                    .child(div().text_size(px(11.)).text_color(theme::attention()).child("Waiting for you"))
                    .child(item.text.clone())
                    .child(div().flex().gap_2().child(decide("allow", "Allow", cx)).child(decide("deny", "Deny", cx)))
                    .into_any_element()
            }
            Kind::Card { .. } => bubble.self_stretch().border_1().border_color(theme::border()).text_color(theme::text_soft()).child(div().text_size(px(11.)).text_color(theme::muted()).child("Decided")).child(item.text.clone()).into_any_element(),
            // What the manager would have shown: the person's to take (ux-5).
            Kind::Nudge { matter } if matter.starts_with(chat::SHOW_ME) && !item.closed => {
                let (matter, id) = (matter.clone(), item.id.clone());
                bubble
                    .self_start()
                    .text_color(theme::text_soft())
                    .child(item.text.clone())
                    .child(
                        div().flex().child(
                            div()
                                .id(SharedString::from(format!("{key}-show")))
                                .role(Role::Button)
                                .aria_label(item.text.clone())
                                .px_2p5()
                                .py_0p5()
                                .rounded(px(6.))
                                .bg(theme::tint())
                                .text_size(px(12.))
                                .text_color(theme::text())
                                .cursor_pointer()
                                .hover(|s| s.opacity(0.88))
                                .child("Show me")
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    if let Some(op) = chat::show_me_op(&matter) {
                                        this.run_op_named(op.name(), serde_json::to_value(&op).ok().and_then(|v| v.get("args").cloned()).unwrap_or_default(), window, cx);
                                    }
                                    let id = id.clone();
                                    this.chat_write(Box::new(move |conversation| {
                                        let _ = conversation.close(&id);
                                    }));
                                })),
                        ),
                    )
                    .into_any_element()
            }
            // The person's word on a nudge (ux-22): counted for its family, as a threshold.
            Kind::Nudge { matter } if !item.closed => {
                let project = item.refs.iter().find_map(|r| match r {
                    glasscode_core::conversation::Ref::Project(key) => Some(key.as_str().to_string()),
                    _ => None,
                });
                let say = |answer: &'static str, label: &'static str, cx: &mut Context<Self>| {
                    let (matter, project) = (matter.clone(), project.clone());
                    div()
                        .id(SharedString::from(format!("{key}-{answer}")))
                        .role(Role::Button)
                        .aria_label(format!("{label}: {}", item.text))
                        .text_size(px(11.))
                        .underline()
                        .text_color(theme::muted())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::text()))
                        .child(label)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            let mut args = serde_json::json!({ "matter": matter, "answer": answer });
                            if let Some(project) = &project {
                                args["project"] = serde_json::json!(project);
                            }
                            this.run_op_named("manager.feedback", args, window, cx);
                        }))
                };
                bubble
                    .self_start()
                    .text_color(theme::text_soft())
                    .child(format!("• {}", item.text))
                    .child(div().flex().gap_3().child(say("useful", "Useful", cx)).child(say("not-useful", "Not useful", cx)).child(say("fewer", "Fewer like this", cx)))
                    .into_any_element()
            }
            Kind::Nudge { .. } => bubble.self_start().text_color(theme::muted()).child(format!("• {}", item.text)).into_any_element(),
            // What the manager did, with Undo while its inverse holds (ux-14).
            Kind::Action { effect, .. } => {
                let undo = self.mchat.undos.get(effect).filter(|entry| entry.holds(glasscode_core::clock::now_ms())).map(|entry| entry.op.clone());
                let effect = effect.clone();
                div()
                    .flex()
                    .flex_none()
                    .self_stretch()
                    .items_center()
                    .gap_2()
                    .text_size(px(size - 1.5))
                    .text_color(theme::muted())
                    .child(div().flex_1().min_w_0().child(item.text.clone()))
                    .when_some(undo, |d, undo| {
                        d.child(
                            div()
                                .id(SharedString::from(format!("{key}-undo")))
                                .role(Role::Button)
                                .aria_label(format!("Undo: {}", item.text))
                                .flex_none()
                                .underline()
                                .text_color(theme::text_soft())
                                .cursor_pointer()
                                .hover(|s| s.text_color(theme::text()))
                                .child("Undo")
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.mchat.undos.remove(&effect);
                                    let args = serde_json::to_value(&undo).ok().and_then(|v| v.get("args").cloned()).unwrap_or_default();
                                    this.run_op_named(undo.name(), args, window, cx);
                                })),
                        )
                    })
                    .into_any_element()
            }
            _ => div().flex_none().self_stretch().text_size(px(size - 1.5)).text_color(theme::muted()).child(item.text.clone()).into_any_element(),
        }
    }

    /// The answer being written: its text so far, what it is doing, and Stop.
    fn render_chat_turn(&self, turn: &Turn, key: &str, size: f32, cx: &mut Context<Self>) -> AnyElement {
        let name = self.state.manager.name.clone();
        let mut sel = crate::chat_select::Selection::default();
        let body: AnyElement = if turn.text.trim().is_empty() {
            div().text_color(theme::muted()).child(if turn.stopping { "Stopping…".to_string() } else { format!("{name} is thinking…") }).into_any_element()
        } else {
            crate::chat_ui::render_reply(&turn.text, &format!("{key}-turn"), &mut sel, cx.entity().downgrade())
        };
        let waiting_whole = !turn.caps.streams_text && turn.text.is_empty();
        div()
            .id(SharedString::from(format!("{key}-turn")))
            // Not a live region: the log announces the answer once, when it is posted, not each
            // part as it streams (ux-16).
            .role(Role::Group)
            .aria_label(format!("{name} is answering"))
            .flex()
            .flex_col()
            .flex_none()
            .self_start()
            .max_w(relative(0.92))
            .gap(px(6.))
            .px_3()
            .py_2()
            .rounded(px(12.))
            .border_1()
            .border_color(theme::border())
            .bg(theme::panel())
            .text_size(px(size))
            .line_height(px(size * 1.45))
            .text_color(theme::text())
            .child(body)
            .when_some(turn.activity.clone(), |d, activity| d.child(div().text_size(px(11.5)).text_color(theme::muted()).child(activity)))
            .when(waiting_whole, |d| d.child(div().text_size(px(11.5)).text_color(theme::muted()).child("Its answer comes all at once when it is done.")))
            .child(
                div().flex().child(
                    div()
                        .id(SharedString::from(format!("{key}-stop")))
                        .role(Role::Button)
                        .aria_label("Stop the answer")
                        .flex_none()
                        .px_2()
                        .py_0p5()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(theme::border_strong())
                        .text_size(px(11.5))
                        .text_color(theme::text_soft())
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::tint()).text_color(theme::text()))
                        .child(turn.caps.stop_words())
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.chat_stop(cx))),
                ),
            )
            .into_any_element()
    }

    /// The references of a message as chips that open their place (ux-20).
    fn render_chips(&self, text: &str, key: &str, cx: &mut Context<Self>) -> AnyElement {
        let (sessions, repos) = self.chat_known();
        let chips = chat::chips_from_references(text, &sessions, &repos);
        div()
            .flex()
            .flex_wrap()
            .gap_1p5()
            .children(chips.into_iter().enumerate().map(|(n, chip)| {
                let target = chip.target.clone();
                let label = match &chip.target {
                    ChipTarget::Session(_) => format!("Session: {}", chip.label),
                    ChipTarget::Pr { .. } => chip.label.clone(),
                };
                div()
                    .id(SharedString::from(format!("{key}-chip-{n}")))
                    .role(Role::Link)
                    .aria_label(format!("Open {label}"))
                    .flex_none()
                    .px_2()
                    .py_0p5()
                    .rounded(px(6.))
                    .bg(theme::tint())
                    .text_size(px(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text_soft())
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme::text()))
                    .child(label)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.chat_open(&target, window, cx)))
            }))
            .into_any_element()
    }

    /// A chip's place: the session, or the session on the pull request, else the PR on the forge.
    fn chat_open(&mut self, target: &ChipTarget, window: &mut Window, cx: &mut Context<Self>) {
        match target {
            ChipTarget::Session(id) => self.open_session(id, window, cx),
            ChipTarget::Pr { repo, number } => {
                let session = self.prs.map.iter().find(|(_, s)| s.repo.as_deref().is_some_and(|r| r.eq_ignore_ascii_case(repo)) && s.pr.as_ref().is_some_and(|p| p.number == *number)).map(|(id, _)| id.clone());
                match session {
                    Some(id) => self.open_session(&id, window, cx),
                    None => crate::center::open_url(&format!("https://github.com/{repo}/pull/{number}"), cx),
                }
            }
        }
    }
}
