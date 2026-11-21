//! The manager on screen: what its panel, its bubble and its commands share (`ManagerUi`), and how
//! it shows itself to the person. It says things (a line in the conversation, the caption, its
//! voice), points at places in the app, walks through them and plays pictures, and reads the pull
//! requests of managed sessions in full for what it sees. Its thinking is the chat and wake
//! lanes' (`manager_chat.rs`, `manager_lanes.rs`), its round `manager_round.rs`'s.
//!
//! It has no page of its own: Today is the one home (`manager_ui.rs` says where each part is).
//!
//! SHARED FILE. `ManagerUi` is used by `manager_ui.rs` (the panel and the bubble), the round, the
//! chat, `manager_settings.rs` and the ops. Add fields at the end of the struct (with a value in
//! `new`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{AnyElement, AppContext as _, Context, Div, Entity, FontWeight, IntoElement, ParentElement, Styled, Window, div, px};
use glasscode_core::github::{self, PreviewConfig};
use glasscode_core::control::Reply;
use glasscode_core::manager::{PanelTab, Scene, Settings, Show, TestLine, TourStop, Who, followups, messages, scene};
use glasscode_core::model::AgentKind;
use serde_json::json;

use crate::center::Center;
use crate::control_ui::Caller;
use crate::scenes_ui::{gold, gold_bg};
use crate::shell::View;
use crate::workspace::{RightTab, Workspace};


/// How the person talks to it in its panel on Today.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Talk,
    Chat,
}

/// What its panel on Today shows: the conversation, or one of the things it holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage {
    #[default]
    Chat,
    FollowUps,
    Messages,
    Memory,
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

pub struct ManagerUi {
    pub mode: Mode,
    pub stage: Stage,
    /// The box to type to it in, in its panel on Today (made the first time that opens).
    pub input: Option<Entity<crate::text_input::TextInput>>,
    /// It listens (voice).
    pub live: bool,
    /// The chat is working on an answer.
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
    /// The server test in Settings: running, and its lines.
    pub testing: bool,
    pub test: Option<Vec<TestLine>>,
    /// Its voice: the helper in its LiveKit room (`manager_voice.rs`).
    pub voice: crate::manager_voice::VoiceState,
    /// What its panel and Settings › Manager keep between frames (`manager_ui.rs`).
    pub page: crate::manager_ui::PageUi,
    /// When the pull requests of managed sessions are read in full next, whether a read runs, and
    /// where the next starts (they take turns, a few at a time).
    pub next_prs: Option<Instant>,
    pub prs_running: bool,
    pub prs_from: usize,
    /// Whether it was speaking and had a caption at the last tick (to redraw when that ends).
    pub shown: (bool, bool),
    /// A walk was started while it was still speaking: its first stop comes when that is over.
    pub tour_waits: bool,
    /// The picture it explains shows on Today (the person can put it away and bring it back).
    pub scene_open: bool,
    /// Whether this build has the manager's voice helper (`manager_voice::helper_path`), looked
    /// for in the background now and then, so Talk shows by itself once there is one; and when
    /// it was last looked for. Without a helper there is no Talk: only the box to type in.
    pub voice_helper: bool,
    pub voice_looked: Option<Instant>,
    /// The person chose Talk or Chat themselves (a helper found later does not change it).
    pub mode_chosen: bool,
}

impl ManagerUi {
    pub fn new() -> Self {
        ManagerUi {
            // Typing works everywhere: Chat, until a voice helper is found (`manager_view_tick`).
            mode: Mode::Chat,
            stage: Stage::default(),
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
            testing: false,
            test: None,
            voice: crate::manager_voice::VoiceState::default(),
            page: crate::manager_ui::PageUi::default(),
            next_prs: None,
            prs_running: false,
            prs_from: 0,
            shown: (false, false),
            tour_waits: false,
            scene_open: false,
            voice_helper: false,
            voice_looked: None,
            mode_chosen: false,
        }
    }
}

/// How often the voice helper is looked for.
const VOICE_LOOK_EVERY: Duration = Duration::from_secs(30);

/// How often the pull requests of managed sessions are read in full (reviewers, threads).
const PRS_EVERY: Duration = Duration::from_secs(3 * 60);
/// At most this many pull requests are read in one go.
const PRS_AT_ONCE: usize = 12;
/// How long it points at something when no walk says otherwise.
const POINT_FOR: Duration = Duration::from_secs(8);
/// The caption stays this long after it stopped speaking.
const CAPTION_STAYS: Duration = Duration::from_secs(6);
/// A walk waits this long at most for the voice to finish a sentence.
const VOICE_WAIT: Duration = Duration::from_secs(20);

// ----- the view's tick --------------------------------------------------------------------------

impl Workspace {
    /// From the workspace tick (about every 700 ms), while the manager is on: a walk, a highlight
    /// and a picture move on, the caption ends, and the pull requests of managed sessions are read
    /// in full now and then (their reviewers and threads, for the review feed).
    pub fn manager_view_tick(&mut self, cx: &mut Context<Self>) {
        if !self.state.manager.enabled {
            return;
        }
        let at = Instant::now();
        let mut changed = false;
        if !self.manager.prs_running && self.manager.next_prs.is_none_or(|t| at >= t) {
            self.manager_fetch_prs(cx);
        }
        // Is there a voice helper? A look at a few paths, off the main thread; Talk shows only
        // while there is one, and comes by itself once one exists.
        if self.manager.voice_looked.is_none_or(|looked| at.duration_since(looked) >= VOICE_LOOK_EVERY) {
            self.manager.voice_looked = Some(at);
            let task = cx.background_spawn(async { crate::manager_voice::helper_path().is_some() });
            cx.spawn(async move |this, cx| {
                let found = task.await;
                let _ = this.update(cx, |ws, cx| {
                    if ws.manager.voice_helper != found {
                        ws.manager.voice_helper = found;
                        // With a helper the panel opens on Talk, as its page did; without one
                        // there is only Chat.
                        if !found || !ws.manager.mode_chosen {
                            ws.manager.mode = if found { Mode::Talk } else { Mode::Chat };
                        }
                        cx.notify();
                    }
                });
            })
            .detach();
        }

        // A walk moves on when its sentence is over (and the voice has finished saying it).
        let voice_busy = self.manager.live && self.manager.voice.state == "speaking";
        let held = |next_at: Instant| at < next_at || (voice_busy && at < next_at + VOICE_WAIT);
        let waits = self.manager.tour_waits;
        if let Some((next_at, next, stops)) = self.manager.tour.as_ref().map(|t| (t.next_at, if waits { t.index } else { t.index + 1 }, t.stops.len())) {
            if !held(next_at) {
                if next < stops {
                    self.manager_tour_go(next, cx);
                } else {
                    self.manager.tour = None;
                    changed = true;
                }
            }
        }
        if self.manager.highlight.as_ref().is_some_and(|h| at >= h.until) {
            self.manager.highlight = None;
            changed = true;
        }
        // A picture moves to its next step the same way.
        let step = self.manager.scene.as_mut().and_then(|play| {
            let next_at = play.next_at?;
            if held(next_at) || play.step + 1 >= play.scene.steps.len() {
                return None;
            }
            play.step += 1;
            Some((play.scene.steps[play.step].say.clone(), play.step + 1 < play.scene.steps.len()))
        });
        if let Some((say, more)) = step {
            let spoken = self.mtick_say(&say, None, cx);
            if let Some(play) = self.manager.scene.as_mut() {
                play.next_at = more.then(|| Instant::now() + stop_time(&say, spoken));
            }
            changed = true;
        }

        // It stopped speaking, or its caption has been up long enough: draw that.
        let shown = (self.manager.speaking_until.is_some_and(|t| at < t), self.manager.caption_until.is_some_and(|t| at < t));
        if shown != self.manager.shown {
            self.manager.shown = shown;
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    /// Who reviewed the pull requests of managed sessions, and what is unresolved: the app
    /// only reads that for the pull request on screen, so the manager keeps the rest fresh
    /// (read-only, into the same cache the Pull request tab uses).
    fn manager_fetch_prs(&mut self, cx: &mut Context<Self>) {
        let mut open: Vec<(String, u64, PathBuf)> = Vec::new();
        for s in &self.state.sessions {
            if s.agent == AgentKind::Shell || s.review.is_some() || !self.manager_manages(&s.project_id) {
                continue;
            }
            // Only what GitHub confirmed in this run and still says (see `PrStatus::acts`).
            let Some(status) = self.prs.map.get(&s.id).filter(|s| s.acts()) else { continue };
            let (Some(repo), Some(pr)) = (&status.repo, &status.pr) else { continue };
            let over = |state: &str| matches!(state.to_uppercase().as_str(), "MERGED" | "CLOSED");
            if over(&pr.state) || status.ci.as_ref().is_some_and(|c| over(&c.state)) || open.iter().any(|(r, n, _)| r == repo && *n == pr.number) {
                continue;
            }
            open.push((repo.clone(), pr.number, s.worktree.clone()));
        }
        // A few at a time, taking turns: the next read goes on where this one stops.
        let (from, count) = turn(open.len(), self.manager.prs_from, PRS_AT_ONCE);
        self.manager.prs_from = from + count;
        let jobs: Vec<(String, u64, PathBuf)> = open.iter().cycle().skip(from).take(count).cloned().collect();
        // Nothing yet (the app is still finding the pull requests): look again soon.
        // More than one turn's worth: the rest follows a minute later, not three.
        let wait = if jobs.is_empty() { Duration::from_secs(20) } else if open.len() > PRS_AT_ONCE { Duration::from_secs(60) } else { PRS_EVERY };
        self.manager.next_prs = Some(Instant::now() + wait);
        if jobs.is_empty() {
            return;
        }
        self.manager.prs_running = true;
        let env = (*self.env).clone();
        let task = cx.background_spawn(async move {
            jobs.into_iter()
                .filter_map(|(repo, number, worktree)| {
                    let preview = PreviewConfig::load(&worktree);
                    github::pr_details(&repo, number, &preview, &env).ok().map(|details| ((repo, number), details))
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |ws, _| {
                ws.manager.prs_running = false;
                if ws.state.manager.enabled {
                    ws.gh.details.extend(found);
                }
            });
        })
        .detach();
    }

    /// A project the manager looks at: managed, and not switched off in Settings › Projects.
    pub(crate) fn manager_manages(&self, project_id: &str) -> bool {
        self.state.manager.manages(project_id) && !self.state.is_hidden(project_id)
    }
}

// ----- saying things ------------------------------------------------------------------------------

impl Workspace {
    /// It says something: the conversation, the caption, and its voice when that is on.
    pub fn manager_say(&mut self, text: &str, show: Option<Show>, cx: &mut Context<Self>) {
        self.mtick_say(text, show, cx);
    }

    /// The same; tells whether its voice speaks it.
    fn mtick_say(&mut self, text: &str, show: Option<Show>, cx: &mut Context<Self>) -> bool {
        let text = text.trim();
        if text.is_empty() {
            return false;
        }
        self.mtick_line(Who::Manager, text, show);
        self.manager_caption(text, true, cx)
    }

    /// What it says, in the bubble's caption and in its voice (when `voice` and the voice is on);
    /// tells whether it was spoken. The v2 chat keeps its lines in the conversation store.
    pub(crate) fn manager_caption(&mut self, text: &str, voice: bool, cx: &mut Context<Self>) -> bool {
        let text = text.trim();
        if text.is_empty() {
            return false;
        }
        let spoken = voice && self.manager_voice_speak(text, cx);
        let until = Instant::now() + say_time(text, spoken);
        self.manager.caption = text.to_string();
        self.manager.speaking_until = Some(until);
        self.manager.caption_until = Some(until + CAPTION_STAYS);
        cx.notify();
        spoken
    }

    /// Add a line to the conversation.
    pub(crate) fn mtick_line(&mut self, who: Who, text: impl Into<String>, _show: Option<Show>) {
        // The conversation store is the chat: what the manager says goes there (the person's own
        // lines are posted by the chat, M52.1).
        if who != Who::You {
            self.chat_post_line(who == Who::System, text.into());
        }
    }

    /// Show a picture on Today and say its first step.
    pub(crate) fn manager_play(&mut self, scene: Scene, cx: &mut Context<Self>) {
        // A picture that cannot be drawn (a step names something that is not there) is not shown.
        if let Err(why) = scene::check(&scene) {
            let line = format!("{} could not draw that. {}", self.state.manager.name, sentence(&format!("{why:#}")));
            self.mtick_line(Who::System, line, None);
            cx.notify();
            return;
        }
        let Some(first) = scene.steps.first().map(|s| s.say.clone()) else { return };
        let more = scene.steps.len() > 1;
        self.manager.scene = Some(ScenePlay { scene, step: 0, next_at: None });
        self.manager.scene_open = true;
        // The picture is a card of Today's centre; its sentences are in the panel beside it.
        self.manager.stage = Stage::Chat;
        self.assistant_dock(cx);
        let spoken = self.mtick_say(&first, None, cx);
        if let Some(play) = self.manager.scene.as_mut() {
            play.next_at = more.then(|| Instant::now() + stop_time(&first, spoken));
        }
        cx.notify();
    }

    /// Switch the manager on or off, or change its settings: save, and start or stop its parts.
    pub fn manager_settings_changed(&mut self, cx: &mut Context<Self>) {
        self.save();
        if !self.state.manager.enabled {
            if self.manager.live {
                self.manager_voice_toggle(cx);
            }
            let manager = &mut self.manager;
            manager.tour = None;
            manager.tour_waits = false;
            manager.highlight = None;
            manager.scene = None;
            manager.scene_open = false;
            // Memory is v2's own: its part of the panel stays with the manager off.
            if manager.stage != Stage::Memory {
                manager.stage = Stage::Chat;
            }
            manager.thinking = false;
            manager.caption.clear();
            manager.speaking_until = None;
            manager.caption_until = None;
        }
        cx.notify();
    }
}

// ----- taking the person through the app --------------------------------------------------------

impl Workspace {
    /// Go where `show` says and point at what it names.
    pub fn manager_show(&mut self, show: &Show, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.manager.enabled {
            return;
        }
        match show.view.as_deref().map(|v| v.trim().to_lowercase()).as_deref() {
            // Its own place is its panel on Today; the keyboard stays where the person has it.
            Some("manager") => self.assistant_dock(cx),
            Some("sessions" | "session") => self.show_view(View::Sessions, cx),
            Some("today" | "board") => self.show_today(cx),
            Some("tickets") => self.show_tickets(cx),
            Some("usage") => self.show_view(View::Usage, cx),
            Some("themes") => self.open_appearance(cx),
            Some("settings") => self.open_settings(self.settings.section, window, cx),
            _ => {}
        }
        // In its own panel a section is one of the things it holds, not a part of a session's.
        if show.session.is_none() && show.view.as_deref().is_some_and(|v| v.trim().eq_ignore_ascii_case("manager")) {
            if let Some(stage) = show.section.as_deref().and_then(stage_of) {
                self.manager_set_stage(stage, cx);
            }
            self.manager.highlight = None;
            cx.notify();
            return;
        }
        if let Some(id) = show.session.as_deref() {
            if self.state.session(id).is_some_and(|s| !self.state.session_hidden(s)) {
                self.menu_open = false;
                // Taken there at the person's asking: `open_session` shows Sessions and puts the
                // place left on the back list once.
                if self.view != View::Sessions || self.state.active_session.as_deref() != Some(id) || self.center != Center::Session {
                    self.open_session(id, window, cx);
                }
            } else if self.server.session(id).is_some() {
                self.enter(View::Sessions);
                self.menu_open = false;
                self.open_server_session(id, window, cx);
            } else {
                // The session is gone: there is nothing to show.
                self.manager.highlight = None;
                cx.notify();
                return;
            }
        }
        let section = show.section.as_deref().map(section_key).filter(|s| !s.is_empty());
        if let Some(tab) = show.tab.or_else(|| section.as_deref().and_then(tab_of_section)) {
            // The panel belongs to a session: make sure one is on screen.
            if show.view.is_none() && show.session.is_none() {
                self.enter(View::Sessions);
            }
            self.right_tab = match tab {
                // The checks have a tab of their own since the redesign.
                PanelTab::PullRequest if section.as_deref() == Some("checks") => RightTab::Checks,
                PanelTab::PullRequest => RightTab::PullRequest,
                PanelTab::Changes => RightTab::Changes,
                PanelTab::Files => RightTab::Files,
            };
            self.show_right = true;
            if tab == PanelTab::Files {
                self.files_tick(cx);
            }
        }
        self.manager.highlight = section.map(|section| {
            // At a stop of a walk it points for as long as the stop lasts.
            let until = (Instant::now() + POINT_FOR).max(self.manager.tour.as_ref().map(|t| t.next_at).unwrap_or_else(Instant::now));
            Highlight { session: show.session.clone().or_else(|| self.state.active_session.clone()), section, until }
        });
        cx.notify();
    }

    /// Start a walk through the app: go to its first stop.
    pub fn manager_start_tour(&mut self, stops: Vec<TourStop>, cx: &mut Context<Self>) {
        if stops.is_empty() || !self.state.manager.enabled {
            return;
        }
        // It finishes the sentence it is saying (the answer that announced the walk) first.
        let now = Instant::now();
        match self.manager.speaking_until.filter(|until| *until > now) {
            Some(until) => {
                self.manager.tour = Some(Tour { stops, index: 0, next_at: until });
                self.manager.tour_waits = true;
                cx.notify();
            }
            None => {
                self.manager.tour = Some(Tour { stops, index: 0, next_at: now });
                self.manager_tour_go(0, cx);
            }
        }
    }

    /// Go to a stop of the walk: show it and say its sentence. The walk moves on by itself
    /// when the sentence is over, and ends after the last stop.
    pub fn manager_tour_go(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(stop) = self.manager.tour.as_ref().map(|t| t.stops.get(index).cloned()) else { return };
        let Some(stop) = stop else {
            self.manager_stop_tour(cx);
            return;
        };
        self.manager.tour_waits = false;
        let spoken = self.mtick_say(&stop.say, Some(stop.show.clone()), cx);
        if let Some(tour) = self.manager.tour.as_mut() {
            tour.index = index;
            tour.next_at = Instant::now() + stop_time(&stop.say, spoken);
        }
        let show = stop.show;
        self.with_main_window(cx, move |ws, window, cx| ws.manager_show(&show, window, cx));
        cx.notify();
    }

    /// End the walk (the person asked something, clicked, or it is over).
    pub fn manager_stop_tour(&mut self, cx: &mut Context<Self>) {
        self.manager.tour_waits = false;
        if self.manager.tour.take().is_some() {
            self.manager.highlight = None;
            if self.manager.live {
                self.manager_voice_stop(cx);
            }
        }
        cx.notify();
    }

    /// Whether it is pointing at this part of the active session's right panel now
    /// ("checks", "preview", "reviews", "comments", "changes", "files", "reply").
    pub fn manager_points_at(&self, section: &str) -> bool {
        let Some(highlight) = self.manager.highlight.as_ref() else { return false };
        self.state.manager.enabled
            && highlight.section == section
            && highlight.until > Instant::now()
            && self.view == View::Sessions
            && self.center == Center::Session
            && highlight.session.as_deref().is_none_or(|id| self.state.active_session.as_deref() == Some(id))
    }

    /// Mark a row (a section's heading) while the manager points at `section`: a warm outline,
    /// a tint, and "<name> is pointing here" at its end. Otherwise `el` comes back as it is.
    pub fn manager_mark<E: Styled + ParentElement>(&self, section: &str, el: E) -> E {
        if !self.manager_points_at(section) {
            return el;
        }
        el.mt_2().px_2().pt_1p5().pb_1p5().rounded(px(8.)).border(px(1.5)).border_color(gold()).bg(gold_bg()).child(pointing_tag(&self.state.manager.name))
    }

    /// The same for a whole tab of the panel: the mark goes around it, the caption on top.
    pub fn manager_mark_any(&self, section: &str, el: AnyElement) -> AnyElement {
        if !self.manager_points_at(section) {
            return el;
        }
        pointed(&self.state.manager.name, el, true)
    }
}

// ----- pointing, and small things ------------------------------------------------------------------

/// "<name> is pointing here", in the manager's own colour.
fn pointing_tag(name: &str) -> Div {
    div().flex_none().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(gold()).child(format!("{name} is pointing here"))
}

/// `el` with the mark around it and "<name> is pointing here" above. `fill`: it takes the
/// room its place in a column gives it (a tab of the panel); else it is as tall as `el`.
pub(crate) fn pointed(name: &str, el: AnyElement, fill: bool) -> AnyElement {
    let frame = div().flex().flex_col().rounded(px(8.)).border(px(1.5)).border_color(gold()).bg(gold_bg());
    let frame = if fill { frame.flex_1().min_h_0() } else { frame.p_2() };
    frame.child(pointing_tag(name).px_2().py_1()).child(el).into_any_element()
}

/// How long it takes to say a sentence: about a second and 22 ms a character on screen (at
/// most six seconds), and the pace of speech when its voice says it.
fn say_time(text: &str, spoken: bool) -> Duration {
    let chars = text.chars().count() as u64;
    if spoken { Duration::from_millis((1000 + 65 * chars).min(90_000)) } else { Duration::from_millis((1000 + 22 * chars).min(6000)) }
}

/// How long a stop of a walk (or a step of a picture) lasts: its sentence, said or read at
/// an easy pace, and two seconds to look.
fn stop_time(text: &str, spoken: bool) -> Duration {
    let chars = text.chars().count() as u64;
    let sentence = if spoken { say_time(text, true) } else { Duration::from_millis((1000 + 45 * chars).clamp(3000, 20_000)) };
    sentence + Duration::from_secs(2)
}

/// Whose turn it is: where to start among `len` things when the last turn stopped at `from`,
/// and how many to take (at most `at_once`, never one twice).
fn turn(len: usize, from: usize, at_once: usize) -> (usize, usize) {
    if len == 0 {
        return (0, 0);
    }
    (from % len, len.min(at_once))
}

/// A section of the right panel as the app names it, whatever the manager called it.
fn section_key(word: &str) -> String {
    let word = word.trim().to_lowercase();
    match word.as_str() {
        "check" | "ci" => "checks",
        "review" => "reviews",
        "comment" | "threads" => "comments",
        "change" | "diff" => "changes",
        "file" | "worktree" => "files",
        _ => return word,
    }
    .to_string()
}

/// What its panel on Today shows for a section named in a `Show`.
fn stage_of(section: &str) -> Option<Stage> {
    match section.trim().to_lowercase().as_str() {
        "overview" | "home" | "chat" | "conversation" => Some(Stage::Chat),
        "followups" | "follow-ups" | "follow_ups" | "followup" | "follow-up" => Some(Stage::FollowUps),
        "messages" | "message" | "outbox" => Some(Stage::Messages),
        "memory" | "notes" => Some(Stage::Memory),
        _ => None,
    }
}

/// The tab of the right panel a section is on (the last reply is on none).
fn tab_of_section(section: &str) -> Option<PanelTab> {
    match section {
        "checks" | "preview" | "reviews" | "comments" => Some(PanelTab::PullRequest),
        "changes" => Some(PanelTab::Changes),
        "files" => Some(PanelTab::Files),
        _ => None,
    }
}

// ----- `manager status`, and a session by its words -----------------------------------------------

impl Workspace {
    /// `manager status`: also answers while the manager is off.
    pub(crate) fn manager_status_reply(&self) -> Reply {
        let settings = &self.state.manager;
        let ui = &self.manager;
        let home = match settings.home {
            glasscode_core::manager::Home::Mac => "mac",
            glasscode_core::manager::Home::Server => "server",
        };
        if !settings.enabled {
            let data = json!({ "enabled": false, "name": settings.name, "initials": settings.initials(), "state": "idle", "caption": "", "needs_you": 0, "follow_ups_open": 0, "messages_waiting": 0, "home": home, "on_server": false });
            return Reply::ok(crate::ops_run::OFF, data);
        }
        let state = if ui.thinking {
            "thinking"
        } else if ui.speaking_until.is_some_and(|until| until > Instant::now()) {
            "speaking"
        } else {
            "idle"
        };
        let needs_you = glasscode_core::manager::brief::needs_you(&self.v2_snapshot(), self.follow_ups(), self.manager_outbox());
        let follow_ups = followups::open(self.follow_ups()).len();
        let waiting = messages::waiting_in(self.manager_outbox()).len();
        // Where it acts: the home's lease says (D5, M57).
        let on_server = !self.acts_as_home();
        let mut text = format!(
            "{} is on ({state}), running on {}.\nNeeds you: {needs_you} · follow-ups open: {follow_ups} · messages waiting for your OK: {waiting}.",
            settings.name,
            if on_server { "the server" } else { "this Mac" }
        );
        if !ui.caption.trim().is_empty() {
            text.push_str(&format!("\nLast said: “{}”", crate::ops_run::clip(&ui.caption, 160)));
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
    /// A session the manager may show: (its id, its title). One on this Mac, or one it sees on
    /// the server; never one in a project it does not manage.
    pub(crate) fn manager_find_session(&self, word: &str, caller: &Caller, settings: &Settings) -> Result<(String, String), Reply> {
        match self.find_session(word, caller) {
            Ok(id) => {
                let Some(session) = self.state.session(&id) else { return Err(crate::ops_run::failed("That session is gone.")) };
                let title = self.session_title(session);
                if !settings.manages(&session.project_id) {
                    return Err(crate::ops_run::denied(format!("“{title}” is in a project {} does not manage (Settings › Manager).", settings.name)));
                }
                Ok((id, title))
            }
            Err(not_here) => {
                let snap = self.v2_snapshot();
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
}

/// The first line of a reason, ending like a sentence.
fn sentence(why: &str) -> String {
    let line = why.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("Something went wrong");
    if line.ends_with(['.', '!', '?']) { line.to_string() } else { format!("{line}.") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caption_is_short_and_speech_takes_its_time() {
        assert_eq!(say_time("Hi", false), Duration::from_millis(1044));
        assert_eq!(say_time(&"a".repeat(1000), false), Duration::from_secs(6));
        assert!(say_time(&"a".repeat(200), true) > Duration::from_secs(12));
        // A stop lasts as long as its sentence and two seconds more, never a blink.
        assert_eq!(stop_time("Hi", false), Duration::from_secs(5));
        assert_eq!(stop_time(&"a".repeat(200), true), say_time(&"a".repeat(200), true) + Duration::from_secs(2));
        assert_eq!(stop_time(&"a".repeat(5000), false), Duration::from_secs(22));
    }

    #[test]
    fn states_and_sections_get_the_manager_s_words() {
        assert_eq!(section_key(" Checks "), "checks");
        assert_eq!(section_key("diff"), "changes");
        assert_eq!(tab_of_section("reviews"), Some(PanelTab::PullRequest));
        assert_eq!(tab_of_section("changes"), Some(PanelTab::Changes));
        assert_eq!(tab_of_section("files"), Some(PanelTab::Files));
        assert_eq!(tab_of_section("reply"), None);
        assert_eq!(stage_of("followups"), Some(Stage::FollowUps));
        assert_eq!(stage_of("overview"), Some(Stage::Chat));
        assert_eq!(stage_of("Messages"), Some(Stage::Messages));
        assert_eq!(stage_of("memory"), Some(Stage::Memory));
        assert_eq!(stage_of("checks"), None);
    }

    #[test]
    fn pull_requests_take_turns() {
        // 30 open ones, 12 at a time: every one is read within three turns.
        let mut seen = std::collections::HashSet::new();
        let mut from = 0;
        for _ in 0..3 {
            let (start, count) = turn(30, from, 12);
            seen.extend((0..count).map(|k| (start + k) % 30));
            from = start + count;
        }
        assert_eq!(seen.len(), 30);
        // Fewer than a turn takes: each once, not twice.
        assert_eq!(turn(5, 7, 12), (2, 5));
        assert_eq!(turn(0, 3, 12), (0, 0));
    }
}
