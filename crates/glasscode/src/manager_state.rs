//! The manager in the app: what its page, its bubble, its rounds and its commands share. The
//! thinking itself is `glasscode_core::manager`; this is only the app's side of it.
//!
//! SHARED FILE. `ManagerUi` is used by `manager_ui.rs` (the page and the bubble),
//! `manager_tick.rs` (the rounds, carrying out what it decides, walks through the app),
//! `manager_cmd.rs` (`glasscode manager …`), `manager_home.rs` (the copy on the server) and
//! `manager_settings.rs`. Add fields at the end of the struct (with a value in `new`); do not
//! rename or remove what is here.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use gpui::Entity;
use glasscode_core::manager::brain::Brain;
use glasscode_core::manager::gh::GivenReview;
use glasscode_core::manager::harness::Harness;
use glasscode_core::manager::sync::{Ask, SyncReply, TestLine};
use glasscode_core::manager::{InMessage, Scene, TourStop};

/// How the person talks to it on its page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Talk,
    Chat,
}

/// What the right side of its page shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage {
    #[default]
    Overview,
    FollowUps,
    Messages,
    Memory,
    /// A picture it is explaining (`ManagerUi::scene`).
    Scene,
}

/// A walk through the app in progress.
pub struct Tour {
    pub stops: Vec<TourStop>,
    pub index: usize,
    /// When to move on by itself.
    pub next_at: Instant,
}

/// What it points at in a session's right panel, until when.
pub struct Highlight {
    pub session: Option<String>,
    /// "checks", "preview", "reviews", "comments", "changes", "files", "reply".
    pub section: String,
    pub until: Instant,
}

/// A picture being explained, and the step it is on.
pub struct ScenePlay {
    pub scene: Scene,
    pub step: usize,
    pub next_at: Option<Instant>,
}

/// What a background job hands back to the main thread (through `ManagerUi::jobs`).
pub enum Done {
    /// The brain's answer to question `id`.
    Answer { id: u64, say: Result<String, String>, voice: bool },
    Reviews(Result<Vec<GivenReview>, String>),
    Inbox(Result<Vec<InMessage>, String>),
    /// An outbox message was sent, or not.
    Sent { id: String, result: Result<(), String> },
    /// The person's GitHub login.
    Me(Option<String>),
    /// A sync with the server's copy.
    Server(Result<SyncReply, String>),
    /// The server test in Settings.
    Test(Vec<TestLine>),
}

pub struct ManagerUi {
    /// Its data and its rules.
    pub harness: Harness,
    pub mode: Mode,
    pub stage: Stage,
    /// v2 (M20.3): memory's entries as the Memory stage lists them (`memstore::listing`), read off
    /// the main thread when the stage opens; None until read.
    pub v2_memory: Option<Vec<glasscode_core::memstore::Listed>>,
    /// The box to type to it in (made the first time its page opens).
    pub input: Option<Entity<crate::text_input::TextInput>>,
    /// It listens (voice).
    pub live: bool,
    /// The brain is working on an answer.
    pub thinking: bool,
    /// What it is saying, or said last (the bubble's caption).
    pub caption: String,
    /// It speaks until then (the bubble pulses), and the caption stays a little longer.
    pub speaking_until: Option<Instant>,
    pub caption_until: Option<Instant>,
    pub tour: Option<Tour>,
    pub highlight: Option<Highlight>,
    pub scene: Option<ScenePlay>,
    /// The person's GitHub login, once looked up.
    pub me: Option<String>,
    /// Background jobs report here; `manager_tick` reads it.
    pub jobs: (async_channel::Sender<Done>, async_channel::Receiver<Done>),
    /// The agent that thinks for it, started with the first question that needs it.
    pub brain: Option<Arc<Mutex<Brain>>>,
    pub next_ask: u64,
    pub last_round: Option<Instant>,
    pub last_reviews: Option<Instant>,
    pub last_inbox: Option<Instant>,
    pub last_sync: Option<Instant>,
    pub reviews_running: bool,
    pub inbox_running: bool,
    pub sync_running: bool,
    /// The server copy's last round (unix seconds), from the last sync.
    pub server_heartbeat: u64,
    /// Questions waiting to go to the server's copy.
    pub asks: Vec<Ask>,
    /// The server test in Settings: running, and its lines.
    pub testing: bool,
    pub test: Option<Vec<TestLine>>,
    /// Its voice: the helper in its LiveKit room (`manager_voice.rs`).
    pub voice: crate::manager_voice::VoiceState,
    /// What its page and Settings › Manager keep between frames (`manager_ui.rs`).
    pub page: crate::manager_ui::PageUi,
    /// Finished jobs that belong to the server's copy (`Done::Server`, `Done::Test`):
    /// `manager_tick` puts them here, `manager_home_tick` takes them out.
    pub home_inbox: Vec<Done>,
    /// What the rounds keep between ticks (`manager_tick.rs`).
    pub tick: crate::manager_tick::TickUi,
    /// What the sync with the server's copy keeps between rounds (`manager_home.rs`).
    pub home: crate::manager_home::HomeUi,
}

impl ManagerUi {
    pub fn new() -> Self {
        ManagerUi {
            harness: Harness::load(),
            // Without a voice helper (Windows) the page opens where typing works: Chat.
            mode: if crate::glasscode_ui::VOICE { Mode::default() } else { Mode::Chat },
            stage: Stage::default(),
            v2_memory: None,
            input: None,
            live: false,
            thinking: false,
            caption: String::new(),
            speaking_until: None,
            caption_until: None,
            tour: None,
            highlight: None,
            scene: None,
            me: None,
            jobs: async_channel::unbounded(),
            brain: None,
            next_ask: 1,
            last_round: None,
            last_reviews: None,
            last_inbox: None,
            last_sync: None,
            reviews_running: false,
            inbox_running: false,
            sync_running: false,
            server_heartbeat: 0,
            asks: Vec::new(),
            testing: false,
            test: None,
            voice: crate::manager_voice::VoiceState::default(),
            page: crate::manager_ui::PageUi::default(),
            home_inbox: Vec::new(),
            tick: crate::manager_tick::TickUi::default(),
            home: crate::manager_home::HomeUi::default(),
        }
    }
}
