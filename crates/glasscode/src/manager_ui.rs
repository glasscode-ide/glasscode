//! The manager on screen. It has no page of its own: Today is the one home, and the manager is
//! part of it.
//!
//! - **The assistant's panel** (`render_assistant`): its mark, name and state, the conversation
//!   (`manager_chat.rs` draws the thread) and a box to type in. On Today it is the "Manager" tab
//!   of the right panel, beside Inbox and Activity, and there it also holds what the manager
//!   keeps: follow-ups, messages and memory, one row of tabs under its header. On every other
//!   page the same panel opens over the window, from the bubble or its key.
//! - **The bubble** (`render_manager_bubble`): the manager's mark in the bottom right corner of
//!   the pages that do not show the panel, with a dot for its state, a count of what is new, and
//!   its caption while it talks.
//! - **What it adds to Today's cards** (`manager_waits`, `manager_next`, `manager_session_lines`,
//!   `manager_sentence`): the things only it knows about, in the cards that are already there.
//! - **The picture it draws** (`render_scene_card`): a card of Today's centre, while it explains.
//!
//! Few buttons, on purpose: a row opens what it describes, and things are done by asking. Only
//! what must be decided by hand has a button (allow or reject a message, a follow-up that is
//! done, a note to forget).

use std::time::Instant;

use gpui::{AnyElement, ClickEvent, Context, Div, ElementId, FontWeight, Hsla, MouseButton, MouseDownEvent, MouseUpEvent, SharedString, Stateful, Window, deferred, div, prelude::*, px};
use glasscode_core::manager::{FollowKind, FollowState, FollowUp, InMessage, Listen, MsgState, OutMessage, SendPolicy, SessionInfo, SessionState, Show, Snapshot, Tier, followups};
use glasscode_core::model::now;

use crate::icons;
use crate::manager_chat::ThreadView;
use crate::manager_view::{Mode, Stage};
use crate::scenes_ui::{gold, gold_bg, gold_line, text_lines};
use crate::settings_ui::Section;
use crate::shell::View;
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::today_ui::TodayTab;
use crate::ui;
use crate::workspace::{Side, ToggleManagerChat, Workspace};

/// What the panel and Settings › Manager keep between frames.
#[derive(Default)]
pub struct PageUi {
    /// Where the conversation is scrolled to.
    pub chat_scroll: gpui::ScrollHandle,
    /// The text fields of Settings › Manager, made the first time that section is drawn.
    pub form: std::cell::OnceCell<crate::manager_settings::ManagerForm>,
    /// Where follow-ups, messages or memory are scrolled to in the panel.
    pub stage_scroll: gpui::ScrollHandle,
    /// How many items more than the newest [`DOCKED_ITEMS`] the panel on Today shows: the person
    /// asked for earlier ones.
    pub earlier: usize,
}

/// Things to say to it, one click each.
pub(crate) const TRY: [&str; 5] = ["Give me today's update", "What needs me?", "What is blocked?", "Which follow-ups are open?", "Explain what my last session changed"];

/// How many items of the conversation the panel lays out: Today draws ten times a second while
/// its backdrop moves, and each item is laid out on each frame (measured in a debug build: about
/// 2 ms an item). The earlier ones are one click away ("Show earlier messages").
const DOCKED_ITEMS: usize = 12;
const FLOATING_ITEMS: usize = 12;
/// The panel over the window, at 100 %: no wider and no higher than this, and never more than
/// the window has room for.
const FLOAT_WIDTH: f32 = 400.;
const FLOAT_HEIGHT: f32 = 620.;
/// The bubble.
const BUBBLE: f32 = 44.;

// ----- what it is doing --------------------------------------------------------------------

/// How the manager looks right now: its orb, its bubble and the words in its panel's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Presence {
    Asleep,
    Listening,
    Thinking,
    Speaking,
}

fn presence(live: bool, thinking: bool, speaking: bool) -> Presence {
    if thinking {
        Presence::Thinking
    } else if speaking {
        Presence::Speaking
    } else if live {
        Presence::Listening
    } else {
        Presence::Asleep
    }
}

/// The words under the orb's hint in Talk mode, and for a screen reader.
fn state_text(look: Presence, mode: Mode, listen: Listen, name: &str) -> String {
    match (look, mode) {
        (Presence::Thinking, _) => "thinking".into(),
        (Presence::Speaking, Mode::Talk) => "speaking".into(),
        (Presence::Speaking, Mode::Chat) => "answering".into(),
        (Presence::Listening, Mode::Talk) => "listening".into(),
        (Presence::Asleep, Mode::Talk) if listen == Listen::Wake => format!("asleep, say “{name}”"),
        (Presence::Asleep, Mode::Talk) => "asleep".into(),
        (_, Mode::Chat) => "ready".into(),
    }
}

/// The state a dot can have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Signal {
    Idle,
    Busy,
    NeedsYou,
}

impl Signal {
    fn color(self) -> Hsla {
        match self {
            Signal::Idle => theme::added(),
            Signal::Busy => theme::link(),
            Signal::NeedsYou => theme::attention(),
        }
    }
}

/// The one word of the panel's header, and its dot: what it does now comes before what waits
/// (`waiting`: the cards that wait for the person's answer).
fn state_word(look: Presence, mode: Mode, waiting: usize) -> (&'static str, Signal) {
    match look {
        Presence::Thinking => ("thinking", Signal::Busy),
        Presence::Speaking if mode == Mode::Talk => ("speaking", Signal::Busy),
        Presence::Speaking => ("answering", Signal::Busy),
        _ if waiting > 0 => ("needs you", Signal::NeedsYou),
        Presence::Listening => ("listening", Signal::Idle),
        Presence::Asleep => ("idle", Signal::Idle),
    }
}

// ----- small pieces of text ----------------------------------------------------------------

fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max { text.to_string() } else { format!("{}…", text.chars().take(max).collect::<String>().trim_end()) }
}

/// The first line with something on it.
fn first_line(text: &str) -> String {
    clip(text.lines().find(|l| !l.trim().is_empty()).unwrap_or(""), 180)
}

/// The last line with something on it (where a reply ends, or the question it stopped on).
fn last_line(text: &str) -> String {
    clip(text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or(""), 180)
}

/// "just now", "5 min ago", "3 h ago", "2 d ago".
fn ago(at: u64, now: u64) -> String {
    let secs = now.saturating_sub(at);
    match secs {
        0..=89 => "just now".into(),
        90..=3299 => format!("{} min ago", (secs + 30) / 60),
        3300..=129_599 => format!("{} h ago", (secs + 1800) / 3600),
        _ => format!("{} d ago", (secs + 43_200) / 86_400),
    }
}

/// How often a check runs: "once", "every 10 min", "every 2 h", "every day".
#[cfg_attr(not(test), allow(dead_code))]
fn every(secs: u64, once: bool) -> String {
    if once {
        return "once".into();
    }
    match secs {
        0 => "when something changes".into(),
        1..=89 => "every minute".into(),
        90..=3299 => format!("every {} min", (secs + 30) / 60),
        3300..=5399 => "every hour".into(),
        5400..=82_799 => format!("every {} h", (secs + 1800) / 3600),
        82_800..=129_599 => "every day".into(),
        _ => format!("every {} d", (secs + 43_200) / 86_400),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The other person of a follow-up: their name, else their GitHub login.
fn who(f: &FollowUp) -> String {
    if f.person.trim().is_empty() { f.login.trim().to_string() } else { f.person.trim().to_string() }
}

fn follow_title(f: &FollowUp) -> String {
    let who = who(f);
    let with = |text: String| if who.is_empty() { text.clone() } else { format!("{text} · {who}") };
    match f.kind {
        FollowKind::BlockersGiven => with(format!("Your review on #{}", f.number)),
        FollowKind::ReviewReceived => with(format!("Review on your #{}", f.number)),
        FollowKind::Custom => with(first_line(if f.note.trim().is_empty() { &f.title } else { &f.note })),
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn follow_sub(f: &FollowUp, now: u64) -> String {
    let total = f.blockers.len();
    let done = total - f.open_blockers();
    let fixed = if f.state == FollowState::Fixed { "Says it is fixed. " } else { "" };
    match f.kind {
        FollowKind::BlockersGiven if total > 0 => format!("{fixed}{done} of {} done", plural(total, "blocker", "blockers")),
        FollowKind::ReviewReceived if total > 0 => format!("{fixed}{done} of {} addressed", plural(total, "comment", "comments")),
        FollowKind::Custom => format!("{fixed}Kept since {}", ago(f.created, now)),
        _ if !f.title.trim().is_empty() => format!("{fixed}{}", first_line(&f.title)),
        _ => format!("{fixed}Open since {}", ago(f.created, now)),
    }
}

// ----- what needs the person ---------------------------------------------------------------

/// Where a row leads.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Go {
    Show(Show),
    Stage(Stage),
}

/// One thing that waits for the person.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Need {
    pub title: String,
    pub detail: String,
    pub go: Go,
    pub urgent: bool,
}

fn to_session(id: &str) -> Show {
    Show { session: Some(id.to_string()), ..Show::default() }
}

fn to_view(view: &str) -> Show {
    Show { view: Some(view.to_string()), ..Show::default() }
}

/// Everything that waits for the person, most pressing first: an urgent message, sessions
/// that stopped on a question, commands waiting for an OK, messages waiting to be allowed,
/// follow-ups the other side says are fixed, and reviews asked of them.
fn needs(inbox: &[InMessage], outbox: &[OutMessage], follow_ups: &[FollowUp], snap: &Snapshot) -> Vec<Need> {
    let mut out = Vec::new();
    for m in inbox.iter().filter(|m| m.urgent && !m.handled) {
        out.push(Need { title: format!("Urgent from {}", m.from), detail: first_line(&m.text), go: Go::Stage(Stage::Messages), urgent: true });
    }
    for s in snap.sessions.iter().filter(|s| s.state == SessionState::NeedsYou) {
        let detail = s.last_reply.as_deref().map(last_line).filter(|l| !l.is_empty()).unwrap_or_else(|| "Stopped on a question".into());
        out.push(Need { title: s.title.clone(), detail, go: Go::Show(to_session(&s.id)), urgent: false });
    }
    if snap.approvals > 0 {
        out.push(Need {
            title: format!("{} for your OK", if snap.approvals == 1 { "1 command waits".to_string() } else { format!("{} commands wait", snap.approvals) }),
            detail: "What an agent asked to do with the glasscode command, on Today".into(),
            go: Go::Show(to_view("today")),
            urgent: false,
        });
    }
    for m in outbox.iter().filter(|m| m.state == MsgState::Draft) {
        out.push(Need { title: format!("A message to {} waits for your OK", m.to), detail: first_line(&m.body), go: Go::Stage(Stage::Messages), urgent: m.urgent });
    }
    for f in follow_ups.iter().filter(|f| f.state == FollowState::Fixed) {
        let who = who(f);
        let title = if who.is_empty() { format!("#{} is said to be fixed", f.number) } else { format!("{who} says #{} is fixed", f.number) };
        out.push(Need { title, detail: format!("Look again: {}", first_line(&f.title)), go: Go::Stage(Stage::FollowUps), urgent: false });
    }
    for r in &snap.review_asks {
        let title = if r.author.trim().is_empty() { format!("A review is asked of you on #{}", r.number) } else { format!("{} asks for your review", r.author) };
        out.push(Need { title, detail: format!("#{} {}", r.number, first_line(&r.title)), go: Go::Show(to_view("tickets")), urgent: false });
    }
    out
}

/// Of what waits, the part only the manager knows about: what it holds itself (a message, a
/// follow-up). Sessions, commands and reviews are in Today's cards from the app's own lists.
fn held_by_the_manager(needs: Vec<Need>) -> Vec<Need> {
    needs.into_iter().filter(|need| matches!(need.go, Go::Stage(_))).collect()
}

/// What a session reports in one line.
fn session_line(s: &SessionInfo) -> String {
    let word = match s.state {
        SessionState::NotRunning => "Not running",
        SessionState::Idle => "Idle",
        SessionState::Working => "Working",
        SessionState::NeedsYou => "Needs you",
    };
    match s.last_reply.as_deref().map(last_line).filter(|l| !l.is_empty()) {
        Some(line) => format!("{word} · {line}"),
        None => word.to_string(),
    }
}

/// The day's sentence of the manager, after the board's: what moves without the person, and
/// what waits on other people. Empty when there is nothing of either.
fn sentence(moving: usize, with_others: usize) -> String {
    let mut parts = Vec::new();
    if moving > 0 {
        parts.push(format!("{} moving without you.", if moving == 1 { "1 session is".to_string() } else { format!("{moving} sessions are") }));
    }
    if with_others > 0 {
        parts.push(format!("{} on other people.", if with_others == 1 { "1 thing waits".to_string() } else { format!("{with_others} things wait") }));
    }
    parts.join(" ")
}

// ----- building blocks ---------------------------------------------------------------------

/// The manager's mark: a filled circle in the accent with its sign. `muted`: it is off.
pub(crate) fn mark(size: f32, muted: bool) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(theme::sz(size))
        .rounded_full()
        .bg(if muted { theme::hover() } else { theme::accent() })
        .child(icons::icon(icons::ASSISTANT, theme::sz(size * 0.56), theme::sz(size * 0.56), if muted { theme::muted() } else { theme::on_accent() }))
}

/// A state dot with a ring in the colour of what is under it, so it reads on the mark.
fn signal_dot(signal: Signal, size: f32, ground: Hsla) -> Div {
    div().flex_none().size(theme::sz(size)).rounded_full().border(theme::sz(2.)).border_color(ground).bg(signal.color())
}

#[derive(Clone, Copy)]
enum Tone {
    Plain,
    Ok,
    Warn,
    Info,
    Err,
    Gold,
}

fn chip(text: impl Into<SharedString>, tone: Tone) -> Div {
    let (bg, color) = match tone {
        Tone::Plain => (None, theme::muted()),
        Tone::Ok => (Some(theme::success_bg()), theme::success_text()),
        Tone::Warn => (Some(theme::warn_bg()), theme::modified()),
        Tone::Info => (Some(theme::info_bg()), theme::link()),
        Tone::Err => (Some(theme::error_bg()), theme::error_text()),
        Tone::Gold => (Some(gold_bg()), gold()),
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(theme::sz(20.))
        .px(theme::sz(8.))
        .rounded_full()
        .text_size(theme::sz(11.))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .text_color(color)
        .map(|d| match bg {
            Some(bg) => d.bg(bg),
            None => d.border_1().border_color(theme::border()),
        })
        .child(text.into())
}

/// A small button for a row: the strong one decides, the other declines.
fn small_button(id: impl Into<ElementId>, label: &'static str, strong: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .h(theme::sz(26.))
        .px(theme::sz(10.))
        .rounded(theme::sz(7.))
        .text_size(theme::sz(12.))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .cursor_pointer()
        .map(|d| {
            if strong {
                d.bg(theme::accent()).text_color(theme::on_accent()).hover(|s| s.opacity(0.88))
            } else {
                d.border_1().border_color(theme::border_strong()).bg(theme::raised()).text_color(theme::text()).hover(|s| s.bg(theme::hover()))
            }
        })
        .child(label)
}

/// Words that lead somewhere.
fn link(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    div().id(id).flex_none().text_size(theme::sz(12.5)).font_weight(FontWeight::MEDIUM).text_color(theme::link()).cursor_pointer().hover(|s| s.text_color(theme::text_strong())).child(label.into())
}

/// A square button of the panel's header: an icon, and its words when the pointer rests on it.
fn header_button(id: &'static str, glyph: &'static [u8], words: impl Into<SharedString>) -> Stateful<Div> {
    let words: SharedString = words.into();
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(words.clone())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(theme::sz(28.))
        .rounded(theme::sz(7.))
        .cursor_pointer()
        .hover(|s| s.bg(theme::hover()))
        .tooltip(ui::tip(words))
        .child(icons::icon(glyph, theme::sz(15.), theme::sz(15.), theme::muted()))
}

/// One bar per blocker: green when it is done.
fn blocker_bars(f: &FollowUp) -> Div {
    div().flex().gap(theme::sz(4.)).children(f.blockers.iter().map(|b| div().flex_1().h(theme::sz(4.)).rounded(theme::sz(2.)).bg(if b.resolved { theme::added() } else { theme::border_strong() })))
}

/// A card of the panel: a follow-up, a message, what memory keeps.
fn stage_card() -> Div {
    div().flex().flex_col().min_w_0().gap(theme::sz(8.)).p(theme::sz(12.)).rounded(theme::sz(12.)).border_1().border_color(theme::border()).bg(theme::raised())
}

/// The sentence at the top of a part of the panel.
fn lead(text: impl Into<SharedString>) -> Div {
    div().text_size(theme::sz(12.5)).line_height(theme::sz(18.)).text_color(theme::muted()).child(text.into())
}

/// A small heading in the panel.
fn section(title: impl Into<SharedString>) -> Div {
    div().pt(theme::sz(4.)).text_size(theme::sz(12.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_soft()).child(title.into())
}

/// The heights of the orb's bars while it speaks: a still wave. They do not move: an
/// animation draws the whole window on each display frame (the owner's rule since U25).
const SPEAKING_BARS: [f32; 5] = [12., 22., 30., 19., 10.];

/// The orb of Talk mode: quiet bars at rest, a wave of bars while it speaks, a soft ring while
/// it listens, a dashed edge while it thinks. Nothing in it moves.
fn orb(look: Presence) -> Stateful<Div> {
    let asleep = look == Presence::Asleep;
    let ink = if asleep { theme::border_strong() } else { theme::accent() };
    let mut fill = theme::accent();
    fill.a = 0.12;
    let mut line = theme::accent();
    line.a = 0.45;
    let bars = div().flex().items_center().gap(theme::sz(5.)).h(theme::sz(36.)).children((0..5usize).map(|i| {
        let bar = div().flex_none().w(theme::sz(5.)).rounded(theme::sz(3.)).bg(ink);
        bar.h(theme::sz(if look == Presence::Speaking { SPEAKING_BARS[i] } else { 8. }))
    }));
    let disc = div()
        .absolute()
        .top(theme::sz(10.))
        .left(theme::sz(10.))
        .size(theme::sz(112.))
        .rounded_full()
        .border_2()
        .border_color(ink)
        .when(!asleep, |d| d.bg(fill))
        .when(look == Presence::Thinking, |d| d.border_dashed())
        .flex()
        .items_center()
        .justify_center()
        .child(bars);
    let ring = move |inset: f32| div().absolute().top(theme::sz(inset)).left(theme::sz(inset)).size(theme::sz(132. - 2. * inset)).rounded_full().border_1().border_color(line);
    div()
        .id("manager-orb")
        .relative()
        .flex_none()
        .size(theme::sz(132.))
        .rounded_full()
        .cursor_pointer()
        .when(look == Presence::Speaking, |d| d.child(ring(0.).opacity(0.9)).child(ring(5.).opacity(0.45)))
        .when(look == Presence::Listening, |d| d.child(ring(0.).opacity(0.4)))
        .child(disc)
}

/// The dots of a walk or a picture: one per step, lit up to the one it is on.
fn step_dot(id: impl Into<ElementId>, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .w(theme::sz(24.))
        .h(theme::sz(14.))
        .cursor_pointer()
        .child(div().w_full().h(theme::sz(4.)).rounded(theme::sz(2.)).bg(if on { theme::accent() } else { theme::border_strong() }))
}

// ----- where it shows ----------------------------------------------------------------------

impl Workspace {
    /// Whether the assistant's panel is on screen: over the window, or as the tab of Today's
    /// right panel. What arrives in the conversation while it is, is seen.
    pub(crate) fn assistant_shows(&self) -> bool {
        self.mchat.overlay || self.assistant_docked()
    }

    /// The panel shows as the "Manager" tab of Today's right panel.
    pub(crate) fn assistant_docked(&self) -> bool {
        self.view == View::Today && self.show_right && self.today.tab == TodayTab::Manager && (self.state.manager.enabled || self.manager.stage == Stage::Memory)
    }

    /// Show Today with the assistant's tab in its right panel (which comes back when it was
    /// hidden). The manager has no page of its own: this is where each way to it leads.
    pub(crate) fn assistant_dock(&mut self, cx: &mut Context<Self>) {
        self.manager_ensure_input(cx);
        self.mchat.overlay = false;
        if self.view != View::Today {
            self.show_today(cx);
        }
        self.today.tab = TodayTab::Manager;
        if !self.show_right {
            self.set_hidden(Side::Right, false);
            self.save();
        }
        self.manager.page.chat_scroll.scroll_to_bottom();
        if self.manager.stage == Stage::Memory {
            self.memory_load(cx);
        }
        cx.notify();
    }

    /// Go to the manager: Today, its conversation in the right panel, the cursor in its box
    /// (`manager show`, Jump's "Go to the manager", a notification of its own).
    pub fn show_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.assistant_dock(cx);
        if self.state.manager.enabled
            && self.manager.stage == Stage::Chat
            && let Some(input) = self.manager.input.clone()
        {
            crate::text_input::focus(&input, window, cx);
        }
    }

    /// The box to type to it in, made the first time its panel is opened or drawn.
    pub(crate) fn manager_ensure_input(&mut self, cx: &mut Context<Self>) {
        if self.manager.input.is_some() {
            return;
        }
        let name = self.state.manager.name.clone();
        let input = cx.new(|cx| TextInput::new(format!("Ask {name}"), cx).bare());
        cx.subscribe(&input, |this: &mut Workspace, input, event: &InputEvent, cx| match event {
            InputEvent::Submit => this.manager_send_typed(cx),
            // With the v2 chat: a menu or an edit goes first (M52).
            InputEvent::Cancel if (this.mchat.editing.is_some() || !this.mchat.menu.is_empty()) => this.chat_box_cancel(cx),
            // Esc stops an answer that is being written (ux-9).
            InputEvent::Cancel if this.mchat.turn.is_some() => this.chat_stop(cx),
            InputEvent::Cancel => input.update(cx, |i, cx| i.set_text("", cx)),
            InputEvent::Changed => this.chat_menu_update(&input, cx),
        })
        .detach();
        self.manager.input = Some(input);
    }

    /// Send what is in the panel's box on Today.
    fn manager_send_typed(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.manager.input.clone() else { return };
        // With the v2 chat: Enter on a line of the "/" or "@" menu puts it in the box (M52).
        if self.chat_menu_enter(&input, cx) {
            return;
        }
        let (text, untyped) = {
            let input = input.read(cx);
            let all = input.text();
            let (text, untyped) = glasscode_core::words::trimmed(&all, input.typing());
            (text.to_string(), untyped)
        };
        if text.is_empty() {
            return;
        }
        // A queued message being edited takes the text in its place (the v2 chat).
        if self.mchat.editing.is_some() {
            input.update(cx, |i, cx| i.set_text("", cx));
            if self.chat_save_edit(&text, cx) {
                return;
            }
        }
        // Typed in the Manager chat: the person's words, where they typed them (D1).
        let words = glasscode_core::words::record_with(glasscode_core::words::Origin::Composer(glasscode_core::words::ComposerKind::ManagerChat), &text, &untyped, None, None);
        input.update(cx, |i, cx| i.set_text("", cx));
        // With the v2 engine, a typed "yes" approves the one card that waits, when exactly one does
        // (M21.2); with two or more it binds to none, and a paste or a spoken one never binds.
        {
            let origin = if untyped.is_empty() { glasscode_core::words::Origin::Composer(glasscode_core::words::ComposerKind::ManagerChat) } else { glasscode_core::words::Origin::Pasted };
            let open: Vec<u64> = self.control.approvals.iter().map(|a| a.id).collect();
            let one = self.control.approvals.first().filter(|a| a.pending.is_some()).map(|a| (a.id, a.what.clone()));
            if glasscode_core::gate::yes_binds(&origin, &text, open.len())
                && let Some((id, what)) = one
            {
                self.with_main_window(cx, move |ws, window, cx| ws.run_op_named("approvals.decide", serde_json::json!({ "id": id.to_string(), "answer": "allow" }), window, cx));
                self.show_toast(format!("Approved: {what}."), cx);
                cx.notify();
                return;
            }
        }
        self.manager_ask_as(text, false, Some(words), None, None, cx);
        cx.notify();
    }

    /// What the panel on Today shows: the conversation, or one of the things the manager holds.
    pub(crate) fn manager_set_stage(&mut self, stage: Stage, cx: &mut Context<Self>) {
        self.manager.stage = stage;
        self.manager.page.stage_scroll.set_offset(gpui::point(px(0.), px(0.)));
        // v2's memory is read for the panel when it opens (and after each change there).
        if stage == Stage::Memory {
            self.memory_load(cx);
        }
        if stage == Stage::Chat {
            self.manager.page.chat_scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    /// A row of Today that leads to something the manager holds: the panel, at that part.
    pub(crate) fn manager_open_stage(&mut self, stage: Stage, cx: &mut Context<Self>) {
        self.assistant_dock(cx);
        self.manager_set_stage(stage, cx);
    }

    /// The person says a follow-up is done.
    fn manager_follow_done(&mut self, id: &str, cx: &mut Context<Self>) {
        // Where the engine keeps them (the work ledger at v2, M56.2).
        self.follow_ups_edit(|list| followups::close(list, id, false, now()));
        cx.notify();
    }

    /// Jump to a step of the picture (it then stays there instead of moving on by itself).
    fn manager_scene_go(&mut self, step: usize, cx: &mut Context<Self>) {
        if let Some(play) = self.manager.scene.as_mut() {
            play.step = step.min(play.scene.steps.len().saturating_sub(1));
            play.next_at = None;
        }
        cx.notify();
    }

    /// How it looks right now.
    pub(crate) fn manager_presence(&self) -> Presence {
        let m = &self.manager;
        let speaking = m.speaking_until.is_some_and(|until| until > Instant::now()) || (m.live && m.voice.state == "speaking");
        let thinking = m.thinking || (m.live && m.voice.state == "thinking");
        presence(m.live, thinking, speaking)
    }

    // ----- what it adds to Today's cards ---------------------------------------------------

    /// What waits for the person that only the manager knows about: a message it wrote that
    /// needs an OK, an urgent one that came in, a follow-up the other side says is fixed. For
    /// Today's "Needs you" card, its Inbox and the count beside "Today"; none while it is off.
    pub(crate) fn manager_waits(&self) -> Vec<Need> {
        if !self.state.manager.enabled {
            return Vec::new();
        }
        // The sessions and commands are Today's own rows: only the manager's stores are read.
        held_by_the_manager(needs(self.manager_inbox(), self.manager_outbox(), self.follow_ups(), &Snapshot::default()))
    }

    /// What the manager does next, by itself: (what, when). For Today's "Coming up" card.
    pub(crate) fn manager_next(&self) -> Vec<(String, String)> {
        if !self.state.manager.enabled {
            return Vec::new();
        }
        let mut out: Vec<(String, String)> = Vec::new();
        if let Some(brief) = self.v2_last_brief() {
            out.extend(brief.lines.iter().filter(|l| l.kind == "next").map(|line| (line.text.clone(), String::new())));
        }
        if let Some(at) = self.state.manager.brief_at.as_ref() {
            out.push(("Your daily brief".to_string(), at.clone()));
        }
        out
    }

    /// The sessions the manager manages, as it sees them now (its state for each, and its last
    /// reply), with those on the server. For Today's "Recent sessions" card and the day's
    /// sentence; Today asks once for each frame it draws.
    pub(crate) fn manager_session_lines(&self) -> Vec<SessionInfo> {
        if !self.state.manager.enabled {
            return Vec::new();
        }
        self.v2_snapshot().sessions
    }

    /// The line a managed session has in "Recent sessions".
    pub(crate) fn manager_line(info: &SessionInfo) -> String {
        session_line(info)
    }

    /// The manager's part of the day's sentence under the greeting (`managed`: the sessions it
    /// manages, as `manager_session_lines` gave them for this frame).
    pub(crate) fn manager_sentence(&self, managed: &[SessionInfo]) -> String {
        if !self.state.manager.enabled {
            return String::new();
        }
        let moving = managed.iter().filter(|s| s.state == SessionState::Working).count();
        sentence(moving, followups::open(self.follow_ups()).len())
    }

    // ----- the bubble ------------------------------------------------------------------------

    /// The bubble: the manager's mark in the bottom right corner of each page that does not show
    /// its panel, with a dot for its state, a count of what is new, and its caption while it
    /// talks. A click opens the panel (on Today: its tab). None while the manager is off, and on
    /// Today while the right panel shows: the manager's tab is there, with its count, and the
    /// bubble would lie on the panel's own rows.
    pub fn render_manager_bubble(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.state.manager.enabled || (self.view == View::Today && self.show_right) {
            return None;
        }
        // Above the status bar, at any scale.
        let bottom = theme::sz(crate::status_bar::BAR_HEIGHT) + theme::sz(12.);
        if self.mchat.overlay {
            // The panel takes the bubble's corner: as wide and high as the window has room for.
            let viewport = window.viewport_size();
            let width = theme::sz(FLOAT_WIDTH).min(viewport.width - theme::sz(24.));
            let height = theme::sz(FLOAT_HEIGHT).min(viewport.height - bottom - theme::sz(56.));
            let panel = div()
                .id("chat-overlay")
                .role(gpui::Role::Dialog)
                .aria_label(format!("Chat with {}", self.state.manager.name))
                .occlude()
                .flex()
                .flex_col()
                .w(width)
                .h(height)
                .rounded(theme::sz(16.))
                .border_1()
                .border_color(theme::border_strong())
                .bg(theme::panel())
                .shadow_lg()
                .overflow_hidden()
                .child(self.render_assistant(false, window, cx));
            return Some(deferred(div().absolute().right(theme::sz(12.)).bottom(bottom).child(crate::motion::fade_in("assistant-panel", panel))).with_priority(2).into_any_element());
        }
        let m = &self.manager;
        let look = self.manager_presence();
        let name = self.state.manager.name.clone();
        let caption = if look == Presence::Thinking {
            "Thinking…".to_string()
        } else if m.caption.trim().is_empty() {
            m.tour.as_ref().and_then(|t| t.stops.get(t.index)).map(|s| caption_line(&s.say)).unwrap_or_default()
        } else {
            caption_line(&m.caption)
        };
        let talking = m.tour.is_some() || look == Presence::Thinking || look == Presence::Speaking || m.caption_until.is_some_and(|until| until > Instant::now());
        let dots = m.tour.as_ref().filter(|t| t.stops.len() > 1).map(|tour| {
            div().flex().flex_wrap().items_center().gap(theme::sz(6.)).children((0..tour.stops.len()).map(|k| {
                step_dot(("manager-tour-dot", k), k <= tour.index).occlude().on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.manager_tour_go(k, cx)))
            }))
        });
        let (unread, waiting) = self.chat_unread();
        let (_, signal) = state_word(look, m.mode, waiting);
        // The caption takes no clicks: what is under it stays reachable. Only the dots and
        // the mark do. A toast has this corner while it shows.
        let card = (talking && self.toast.is_none() && (!caption.trim().is_empty() || dots.is_some())).then(|| {
            div()
                .flex()
                .flex_col()
                .gap(theme::sz(6.))
                .max_w(theme::sz(280.))
                .px(theme::sz(12.))
                .py(theme::sz(9.))
                .rounded(theme::sz(14.))
                .rounded_br(theme::sz(4.))
                .border_1()
                .border_color(theme::border_strong())
                .bg(theme::dialog())
                .shadow_md()
                .when(!caption.trim().is_empty(), |d| d.child(div().text_size(theme::sz(12.5)).line_height(theme::sz(18.)).text_color(theme::text()).line_clamp(4).child(caption.trim().to_string())))
                .when_some(dots, |d, dots| d.child(dots))
        });
        let key = crate::keymap::label(&ToggleManagerChat);
        let words = match (waiting, unread) {
            (w, _) if w > 0 => format!("Chat with {name}: {w} waiting for you ({key})"),
            (_, u) if u > 0 => format!("Chat with {name}: {u} new ({key})"),
            _ => format!("Chat with {name} ({key})"),
        };
        let face = div()
            .relative()
            .flex_none()
            .size(theme::sz(BUBBLE))
            .child(
                div()
                    .id("manager-bubble")
                    .role(gpui::Role::Button)
                    .aria_label(words.clone())
                    .size_full()
                    .rounded_full()
                    .shadow_md()
                    .cursor_pointer()
                    .occlude()
                    .hover(|s| s.opacity(0.9))
                    .tooltip(ui::tip(words))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.run_op_named("ui.panel", serde_json::json!({ "side": "chat", "hide": false }), window, cx);
                    }))
                    .child(mark(BUBBLE, false)),
            )
            // Its state, at the foot of the mark: not colour only, the tooltip says it in words.
            .child(div().absolute().right(theme::sz(-1.)).bottom(theme::sz(-1.)).child(signal_dot(signal, 13., theme::bg())))
            // Not colour only: the count is a number, and what waits has its mark.
            .when(unread > 0 || waiting > 0, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(theme::sz(-4.))
                        .right(theme::sz(-4.))
                        .min_w(theme::sz(18.))
                        .h(theme::sz(18.))
                        .px(theme::sz(4.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .border(theme::sz(2.))
                        .border_color(theme::bg())
                        .bg(theme::attention())
                        .text_size(theme::sz(10.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme::on_attention())
                        .child(if waiting > 0 { format!("{waiting}!") } else { unread.min(99).to_string() }),
                )
            });
        Some(
            deferred(div().absolute().right(theme::sz(14.)).bottom(bottom).flex().flex_col().items_end().gap(theme::sz(8.)).when_some(card, |d, card| d.child(card)).child(face))
                .with_priority(2)
                .into_any_element(),
        )
    }

    // ----- the panel -------------------------------------------------------------------------

    /// The assistant's panel: header, then the conversation and its box, or (on Today) one of
    /// the things the manager holds. `docked`: it is the tab of Today's right panel; else it is
    /// over the window. It fills what holds it.
    pub fn render_assistant(&mut self, docked: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // Memory is v2's, not the manager's: with the manager off the tab shows it alone.
        if !self.state.manager.enabled {
            let content = self.manager_memory(cx);
            return div().flex().flex_col().flex_1().min_h_0().child(self.stage_scroll(content)).into_any_element();
        }
        if docked {
            self.manager_ensure_input(cx);
        } else {
            self.chat_overlay_input(cx);
        }
        let stage = if docked { self.manager.stage } else { Stage::Chat };
        // What is on screen is seen: the count on the bubble and on the tab starts again.
        self.chat_seen();
        let talk = docked && self.manager.voice_helper && self.manager.mode == Mode::Talk;
        let input = if docked { self.manager.input.clone() } else { self.mchat.overlay_input.clone() };
        let typing = input.as_ref().is_some_and(|input| input.read(cx).focus_handle_clone().is_focused(window));
        let body = match stage {
            Stage::Chat if talk => div().flex().flex_col().flex_1().min_h_0().child(self.manager_talk(cx)).child(self.assistant_composer(docked, input, typing, cx)).into_any_element(),
            Stage::Chat => self.assistant_chat(docked, input, typing, cx),
            Stage::FollowUps => {
                let content = self.manager_followups(cx);
                self.stage_scroll(content)
            }
            Stage::Messages => {
                let content = self.manager_messages(cx);
                self.stage_scroll(content)
            }
            Stage::Memory => {
                let content = self.manager_memory(cx);
                self.stage_scroll(content)
            }
        };
        let root = div().flex().flex_col().flex_1().min_h_0().min_w_0().child(self.assistant_header(docked, cx)).when(docked, |d| d.child(self.assistant_stages(cx))).child(body);
        // Up and Down move in the box's "/" or "@" menu while it is open.
        self.chat_menu_keys(root, cx).into_any_element()
    }

    /// A part of the panel that scrolls: follow-ups, messages, memory.
    fn stage_scroll(&self, content: Vec<AnyElement>) -> AnyElement {
        ui::scroll_area("assistant-stage", &self.manager.page.stage_scroll, div().flex().flex_col().gap(theme::sz(12.)).px(theme::sz(14.)).pt(theme::sz(12.)).pb(theme::sz(20.)).children(content))
    }

    /// The panel's header: the mark with its state, the name, what it is doing in a word, what
    /// the next message is about, and the few buttons of the panel.
    fn assistant_header(&self, docked: bool, cx: &mut Context<Self>) -> Div {
        let settings = &self.state.manager;
        let m = &self.manager;
        let (_, waiting) = self.chat_unread();
        let (word, signal) = state_word(self.manager_presence(), m.mode, waiting);
        let all = self.state.projects.len();
        let managed = self.state.projects.iter().filter(|p| settings.manages(&p.id)).count();
        // Where it acts: the home's lease says (D5, M57).
        let home = if self.acts_as_home() { format!("this {}", crate::platform::computer()) } else { "the server".to_string() };
        let scope = format!("Manages {managed} of {} · runs on {home}", plural(all, "project", "projects"));
        // What the next message is about (ux-5): names only; the person can take it off.
        let about = (!docked).then(|| self.chat_about()).flatten().map(|about| {
            // A page by the name the sidebar gives it, not the word `ui.open` knows it by.
            let label = match about.strip_prefix("About: ").unwrap_or(&about) {
                "tickets" => "the Board",
                "pull-requests" => "Pull requests",
                "usage" => "Usage",
                "settings" => "Settings",
                other => other,
            }
            .to_string();
            div()
                .flex()
                .min_w_0()
                .items_center()
                .gap(theme::sz(4.))
                .h(theme::sz(20.))
                .pl(theme::sz(7.))
                .pr(theme::sz(2.))
                .rounded_full()
                .border_1()
                .border_color(theme::border())
                .text_size(theme::sz(11.))
                .text_color(theme::text_soft())
                .child(div().flex_none().text_color(theme::muted()).child("about"))
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))
                .child(
                    div()
                        .id("chat-overlay-about-off")
                        .role(gpui::Role::Button)
                        .aria_label(format!("Send without: {about}"))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(theme::sz(16.))
                        .rounded_full()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .tooltip(ui::tip("Send the next message without where you are"))
                        .child(icons::icon(icons::CLOSE, theme::sz(9.), theme::sz(9.), theme::muted()))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.mchat.focus_off = true;
                            cx.notify();
                        })),
                )
        });
        let state = div()
            .flex()
            .min_w_0()
            .items_center()
            .gap(theme::sz(6.))
            .text_size(theme::sz(12.))
            .text_color(theme::muted())
            .child(div().flex_none().size(theme::sz(7.)).rounded_full().bg(signal.color()))
            .child(div().flex_none().child(word))
            // On Today there is room for what it manages; over the window, for what it is about.
            .when(docked, |d| d.child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme::faint()).child(format!("· {}", plural(managed, "project", "projects")))))
            .children(about);
        let mode = |id: &'static str, mode: Mode, label: &'static str, cx: &mut Context<Self>| {
            ui::segment(id, m.mode == mode, label).h(theme::sz(24.)).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.manager.mode = mode;
                this.manager.mode_chosen = true;
                this.manager.page.chat_scroll.scroll_to_bottom();
                cx.notify();
            }))
        };
        let actions = if docked {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(theme::sz(2.))
                // Talk needs the manager's voice helper: the choice shows only while this build
                // has one (looked for while the app runs, not decided by the system).
                .when(m.voice_helper, |d| d.child(ui::segmented().child(mode("manager-mode-talk", Mode::Talk, "Talk", cx)).child(mode("manager-mode-chat", Mode::Chat, "Chat", cx))))
                .child(header_button("manager-settings", icons::GEAR, "Manager settings").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_settings(Section::Manager, window, cx))))
        } else {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(theme::sz(2.))
                .child(header_button("chat-overlay-full", icons::TODAY, "Open in Today: the whole conversation, follow-ups, messages and memory").on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.mchat.overlay = false;
                    this.show_manager(window, cx);
                })))
                .child(
                    header_button("chat-overlay-close", icons::CLOSE, format!("Close ({})", crate::keymap::label(&ToggleManagerChat)))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.run_op_named("ui.panel", serde_json::json!({ "side": "chat", "hide": true }), window, cx))),
                )
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(theme::sz(10.))
            .pl(theme::sz(14.))
            .pr(theme::sz(8.))
            .py(theme::sz(10.))
            .when(!docked, |d| d.border_b_1().border_color(theme::border()))
            .child(mark(30., false))
            .child(
                div()
                    .id("assistant-name")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(theme::sz(1.))
                    .tooltip(ui::tip(scope))
                    .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(theme::sz(14.)).line_height(theme::sz(19.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(settings.name.clone()))
                    .child(state),
            )
            .child(actions)
    }

    /// Under the header on Today: the conversation, and what the manager holds, as tabs with
    /// how much each has.
    fn assistant_stages(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let stage = self.manager.stage;
        let open = followups::open(self.follow_ups()).len();
        let drafts = self.manager_outbox().iter().filter(|m| m.state == MsgState::Draft).count();
        let memory = self.memory_page.view.as_ref().map(|view| view.waiting()).unwrap_or(0);
        let tab = |id: &'static str, shown: Stage, label: &'static str, count: usize, hot: bool, cx: &mut Context<Self>| {
            let on = stage == shown;
            div()
                .id(id)
                .role(gpui::Role::Tab)
                .aria_label(if count > 0 { format!("{label}: {count}") } else { label.to_string() })
                .aria_selected(on)
                .flex()
                .flex_none()
                .items_center()
                .gap(theme::sz(5.))
                .h(theme::sz(30.))
                .px(theme::sz(2.))
                .border_b_2()
                .border_color(if on { theme::accent() } else { gpui::transparent_black() })
                .text_size(theme::sz(12.5))
                .cursor_pointer()
                .when(on, |d| d.text_color(theme::text_strong()).font_weight(FontWeight::MEDIUM))
                .when(!on, |d| d.text_color(theme::muted()).hover(|s| s.text_color(theme::text())))
                .child(label)
                .when(count > 0, |d| {
                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .min_w(theme::sz(16.))
                            .h(theme::sz(16.))
                            .px(theme::sz(4.))
                            .rounded_full()
                            .text_size(theme::sz(10.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .map(|d| if hot { d.bg(theme::attention()).text_color(theme::on_attention()) } else { d.bg(theme::hover()).text_color(theme::text_soft()) })
                            .child(count.to_string()),
                    )
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.manager_set_stage(shown, cx)))
        };
        div()
            .id("assistant-stages")
            .role(gpui::Role::TabList)
            .aria_label("What the manager holds")
            .flex()
            .flex_none()
            .flex_wrap()
            .gap_x(theme::sz(14.))
            .px(theme::sz(14.))
            .border_b_1()
            .border_color(theme::border())
            .child(tab("assistant-stage-chat", Stage::Chat, "Chat", 0, false, cx))
            .child(tab("assistant-stage-followups", Stage::FollowUps, "Follow-ups", open, false, cx))
            .child(tab("assistant-stage-messages", Stage::Messages, "Messages", drafts, true, cx))
            .child(tab("assistant-stage-memory", Stage::Memory, "Memory", memory, true, cx))
    }

    /// The conversation and its box.
    fn assistant_chat(&self, docked: bool, input: Option<gpui::Entity<TextInput>>, typing: bool, cx: &mut Context<Self>) -> AnyElement {
        let limit = DOCKED_ITEMS + self.manager.page.earlier;
        let view = if docked { ThreadView { id: "manager-thread", limit, compact: false, fold: true, topics: true } } else { ThreadView { id: "overlay-thread", limit: FLOATING_ITEMS, compact: true, fold: true, topics: false } };
        // The conversation is longer than what is laid out: the earlier part is one click away.
        let earlier = (docked && self.chat_items_shown() > limit).then(|| {
            div().flex().justify_center().child(crate::manager_chat::quiet_action("manager-earlier".into(), icons::CHEVRON_DOWN, "Show earlier messages").role(gpui::Role::Button).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.manager.page.earlier += DOCKED_ITEMS;
                cx.notify();
            })))
        });
        let picture = docked.then(|| self.manager.scene.as_ref().filter(|_| !self.manager.scene_open)).flatten().map(|play| {
            div().flex().child(link("manager-to-scene", format!("Back to the picture: {} ›", if play.scene.title.trim().is_empty() { "what it drew" } else { play.scene.title.trim() })).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.manager.scene_open = true;
                cx.notify();
            })))
        });
        // Nothing said yet, and nothing wrong: what the panel is for, in the middle of it.
        let quiet = self.mchat.items.is_empty() && self.mchat.turn.is_none() && self.mchat.queue.is_empty() && !self.chat_fails();
        let body = if quiet {
            let name = self.state.manager.name.clone();
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap(theme::sz(10.))
                .px(theme::sz(24.))
                .child(mark(40., true))
                .child(div().text_size(theme::sz(14.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(format!("Ask {name}")))
                .child(
                    div()
                        .max_w(theme::sz(280.))
                        .text_center()
                        .text_size(theme::sz(12.5))
                        .line_height(theme::sz(18.))
                        .text_color(theme::muted())
                        .child("About your sessions, reviews and follow-ups, or to start work for you. “What can you do?” lists it all."),
                )
                .into_any_element()
        } else {
            let thread = div()
                .flex()
                .flex_col()
                .gap(theme::sz(12.))
                .px(theme::sz(14.))
                .pt(theme::sz(12.))
                .pb(theme::sz(14.))
                .children(docked.then(|| self.render_chat_topics(cx)).flatten())
                .children(picture)
                .children(earlier)
                .child(self.render_chat_thread(view, cx));
            ui::scroll_area(if docked { "manager-chat" } else { "chat-overlay-scroll" }, &self.manager.page.chat_scroll, thread)
        };
        div().flex().flex_col().flex_1().min_h_0().child(body).child(self.assistant_composer(docked, input, typing, cx)).into_any_element()
    }

    /// The box of the panel: the field of Today's start box at a smaller size (the same corners,
    /// the same edge that takes the accent, the same round Send), with its menu and chips above
    /// and its keys below.
    fn assistant_composer(&self, docked: bool, input: Option<gpui::Entity<TextInput>>, typing: bool, cx: &mut Context<Self>) -> Div {
        let name = self.state.manager.name.clone();
        let key = if docked { "manager-box" } else { "chat-overlay" };
        let empty = self.mchat.items.is_empty() && self.mchat.turn.is_none();
        let typed = input.as_ref().is_some_and(|input| !input.read(cx).text().trim().is_empty());
        // Things to say, one click each, until there is a conversation. On Today they are under
        // the start box, one row of them, so the panel beside it does not say them again.
        let tries = (empty && !docked).then(|| {
            div().flex().flex_wrap().gap(theme::sz(6.)).children(TRY.iter().enumerate().map(|(i, phrase)| {
                try_chip(SharedString::from(format!("{key}-try-{i}")), phrase).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.manager_ask(phrase.to_string(), false, cx);
                    cx.notify();
                }))
            }))
        });
        let mut send_fill = theme::accent();
        if !typed {
            send_fill.a *= 0.38;
        }
        let focus = input.clone();
        let field = div()
            .id(if docked { "manager-input" } else { "chat-overlay-input" })
            .role(gpui::Role::TextInput)
            .aria_label(format!("Ask {name}"))
            .flex()
            .items_center()
            .gap(theme::sz(8.))
            .min_h(theme::sz(44.))
            .pl(theme::sz(14.))
            .pr(theme::sz(6.))
            .rounded(theme::sz(14.))
            .border(theme::sz(1.5))
            .border_color(if typing { theme::accent() } else { theme::border_strong() })
            .bg(theme::input())
            .cursor(gpui::CursorStyle::IBeam)
            .on_click(cx.listener(move |_, _: &ClickEvent, window, cx| {
                if let Some(input) = &focus {
                    crate::text_input::focus(input, window, cx);
                }
            }))
            .child(div().flex_1().min_w_0().children(input))
            .child(
                div()
                    .id(if docked { "manager-send" } else { "chat-overlay-send" })
                    .role(gpui::Role::Button)
                    .aria_label(format!("Send to {name}"))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(theme::sz(32.))
                    .rounded_full()
                    .bg(send_fill)
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.88))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        if docked { this.manager_send_typed(cx) } else { this.chat_overlay_send(cx) }
                    }))
                    .child(icons::icon(icons::SEND, theme::sz(15.), theme::sz(15.), theme::on_accent())),
            );
        let keys = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_x(theme::sz(8.))
            .gap_y(theme::sz(2.))
            .px(theme::sz(4.))
            .text_size(theme::sz(11.))
            .text_color(theme::faint())
            .child(div().flex().items_center().gap(theme::sz(5.)).child(ui::kbd("/")).child("actions").child(ui::kbd("@")).child("mention"))
            .child(div().flex().items_center().gap(theme::sz(5.)).child(if docked { "focus" } else { "open or close" }).child(ui::kbd(crate::keymap::label(&ToggleManagerChat))));
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(theme::sz(8.))
            .px(theme::sz(12.))
            .pt(theme::sz(8.))
            .pb(theme::sz(10.))
            .children(tries)
            // With the v2 chat: the box's menu, attached text and editing note (M52).
            .children(self.render_chat_box_extras(if docked { self.manager.input.as_ref() } else { self.mchat.overlay_input.as_ref() }, key, cx))
            .child(field)
            .child(keys)
    }

    /// Talk mode: the orb, what it says, and how to wake it.
    fn manager_talk(&self, cx: &mut Context<Self>) -> AnyElement {
        let m = &self.manager;
        let settings = &self.state.manager;
        let look = self.manager_presence();
        let caption = if look == Presence::Thinking { "Thinking…".to_string() } else { m.caption.trim().to_string() };
        let hint = match (&m.voice.problem, settings.listen) {
            (Some(problem), _) => problem.clone(),
            (None, Listen::Wake) => format!("Tap the orb to sleep or wake, or say “{}”.", settings.name),
            (None, Listen::Hold) => "Hold the orb to talk.".to_string(),
            (None, Listen::Never) => "It does not listen by itself (Settings › Manager). Tap the orb for this once, or type below.".to_string(),
        };
        // "While you hold": it listens while the orb is pressed (its voice is started first when
        // it is not on), and stops when it is let go, on the orb or anywhere else. Otherwise a
        // click wakes it or lets it sleep.
        let orb = if settings.listen == Listen::Hold {
            orb(look)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        if !this.manager.live {
                            this.manager_voice_toggle(cx);
                        }
                        this.manager_voice_hold(true, cx);
                        cx.notify();
                    }),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, cx| {
                        this.manager_voice_hold(false, cx);
                        cx.notify();
                    }),
                )
                .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, cx| this.manager_voice_hold(false, cx)))
        } else {
            orb(look).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.manager_voice_toggle(cx);
                cx.notify();
            }))
        };
        div()
            .id("manager-talk")
            .role(gpui::Role::Group)
            .aria_label(format!("{}: {}", settings.name, state_text(look, Mode::Talk, settings.listen, &settings.name)))
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .items_center()
            .justify_center()
            .gap(theme::sz(16.))
            .p(theme::sz(16.))
            .child(orb)
            .when(!caption.is_empty(), |d| d.child(div().flex_none().max_w(theme::sz(300.)).text_size(theme::sz(15.)).line_height(theme::sz(22.)).text_color(theme::text()).text_center().line_clamp(9).child(caption)))
            .child(div().flex_none().max_w(theme::sz(300.)).text_size(theme::sz(12.)).line_height(theme::sz(17.)).text_color(if m.voice.problem.is_some() { theme::attention() } else { theme::muted() }).text_center().child(hint))
            .into_any_element()
    }

    /// Allow and Reject for a message that waits. A message it must never send has no Allow.
    fn manager_decide_buttons(&self, msg: &OutMessage, allow_id: impl Into<ElementId>, reject_id: impl Into<ElementId>, cx: &mut Context<Self>) -> Div {
        let allow = msg.id.clone();
        let reject = msg.id.clone();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(theme::sz(6.))
            .when(msg.tier != Tier::Never, |d| {
                d.child(small_button(allow_id, "Allow", true).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.v2_decide_message(&allow, true, None);
                    cx.notify();
                })))
            })
            .child(small_button(reject_id, "Reject", false).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.v2_decide_message(&reject, false, None);
                cx.notify();
            })))
    }

    fn manager_followups(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let time = now();
        let mut out: Vec<AnyElement> = vec![lead("Things that are open with someone else. The manager keeps them until they close, so nobody has to tell you by hand.").into_any_element()];
        let open = followups::open(self.follow_ups());
        if open.is_empty() {
            out.push(stage_card().child(lead("Nothing is open with anyone. A review you leave with blockers, or one left on your pull request, shows up here.")).into_any_element());
        }
        for (i, f) in open.iter().enumerate() {
            let total = f.blockers.len();
            let done = total - f.open_blockers();
            let who = who(f);
            let mut small: Vec<String> = Vec::new();
            if !f.repo.is_empty() {
                small.push(f.repo.clone());
            }
            if !who.is_empty() {
                small.push(match f.kind {
                    FollowKind::BlockersGiven => format!("author {who}"),
                    FollowKind::ReviewReceived => format!("reviewer {who}"),
                    FollowKind::Custom => who.clone(),
                });
            }
            if total > 0 {
                small.push(format!("{done} of {total} done"));
            }
            small.push(format!("since {}", ago(f.created, time)));
            let id = f.id.clone();
            let session = f.session.clone();
            let url = f.url.clone();
            let items = f.blockers.iter().enumerate().map(|(k, b)| {
                div()
                    .flex()
                    .items_start()
                    .gap(theme::sz(8.))
                    .py(theme::sz(7.))
                    .border_t_1()
                    .border_color(theme::border())
                    .text_size(theme::sz(12.5))
                    .line_height(theme::sz(18.))
                    .child(div().flex_none().w(theme::sz(16.)).font_family(theme::MONO_FONT).text_size(theme::sz(11.5)).text_color(theme::muted()).child((k + 1).to_string()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(theme::sz(2.))
                            .child(text_lines(&b.text).text_color(if b.resolved { theme::muted() } else { theme::text() }))
                            .when(!b.path.trim().is_empty(), |d| d.child(div().truncate().font_family(theme::MONO_FONT).text_size(theme::sz(11.5)).text_color(theme::muted()).child(b.path.clone()))),
                    )
                    .child(if b.resolved { chip("Done", Tone::Ok) } else { chip("Open", Tone::Plain) })
            });
            out.push(
                stage_card()
                    .child(div().text_size(theme::sz(13.5)).line_height(theme::sz(19.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(follow_title(f)))
                    .child(div().text_size(theme::sz(12.)).text_color(theme::muted()).child(small.join(" · ")))
                    .when(!f.title.trim().is_empty() && f.kind != FollowKind::Custom, |d| d.child(div().text_size(theme::sz(12.5)).line_height(theme::sz(18.)).text_color(theme::text_soft()).child(f.title.clone())))
                    .when(total > 0, |d| d.child(blocker_bars(f)).child(div().flex().flex_col().children(items)))
                    .when(!f.note.trim().is_empty() && f.kind != FollowKind::Custom, |d| d.child(text_lines(&f.note).text_size(theme::sz(12.5)).text_color(theme::muted())))
                    .when(f.state == FollowState::Fixed, |d| d.child(div().flex().child(chip("Says it is fixed. Look again.", Tone::Gold))))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_x(theme::sz(12.))
                            .gap_y(theme::sz(8.))
                            .pt(theme::sz(2.))
                            .child(small_button(("manager-follow-done", i), "Done", false).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.manager_follow_done(&id, cx))))
                            .when_some(session, |d, session| {
                                d.child(link(("manager-follow-session", i), "Open the session ›").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.manager_show(&to_session(&session), window, cx))))
                            })
                            .when(!url.trim().is_empty(), |d| d.child(link(("manager-follow-pr", i), "Open the pull request ›").on_click(cx.listener(move |_, _: &ClickEvent, _, cx| crate::center::open_url(&url, cx))))),
                    )
                    .into_any_element(),
            );
        }
        out
    }

    fn manager_messages(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let settings = &self.state.manager;
        let time = now();
        let mut out: Vec<AnyElement> = vec![
            lead(match settings.send {
                SendPolicy::AskAlways => "Every message the manager writes waits here for your OK. It never approves or merges a pull request, and never decides for you.",
                SendPolicy::RoutineAuto => "Routine pings go out by themselves. Anything with a promise, a date or an opinion waits here for your OK. It never approves or merges a pull request, and never decides for you.",
            })
            .into_any_element(),
        ];
        // Without a messenger nothing can go out and nothing comes in: say where to set one.
        if settings.messenger.trim().is_empty() {
            out.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme::sz(6.))
                    .px(theme::sz(12.))
                    .py(theme::sz(10.))
                    .rounded(theme::sz(12.))
                    .bg(theme::warn_bg())
                    .text_size(theme::sz(12.5))
                    .line_height(theme::sz(18.))
                    .text_color(theme::text())
                    .child("No messenger is set up, so no message can go out and none is read.")
                    .child(div().flex().child(link("manager-messenger-settings", "Choose one in Settings › Manager ›").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_settings(Section::Manager, window, cx)))))
                    .into_any_element(),
            );
        }
        let head = |name: String, when: String| div().flex().flex_wrap().items_baseline().gap_x(theme::sz(8.)).child(div().text_size(theme::sz(13.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(name)).child(div().text_size(theme::sz(11.5)).text_color(theme::muted()).child(when));
        let body = |text: &str| text_lines(text).text_size(theme::sz(12.5)).line_height(theme::sz(18.)).text_color(theme::text());
        let small = |text: String| div().text_size(theme::sz(12.)).line_height(theme::sz(17.)).text_color(theme::muted()).child(text);

        let mut outbox: Vec<&OutMessage> = self.manager_outbox().iter().collect();
        outbox.sort_by_key(|m| (m.state != MsgState::Draft, std::cmp::Reverse(m.updated.max(m.created))));
        out.push(section("Written for you").into_any_element());
        if outbox.is_empty() {
            out.push(lead("Nothing yet. Ask the manager to tell a teammate something, and the draft lands here.").into_any_element());
        }
        for (i, msg) in outbox.iter().take(40).enumerate() {
            let (label, tone) = message_state(msg);
            let (tier, tier_tone) = match msg.tier {
                Tier::Routine => ("Routine", Tone::Plain),
                Tier::NeedsOk => ("Needs your OK", Tone::Warn),
                Tier::Never => ("Never sent", Tone::Err),
            };
            out.push(
                stage_card()
                    .when(msg.state == MsgState::Draft, |d| d.border_color(gold_line()))
                    .child(head(format!("To {}", msg.to), ago(msg.sent.unwrap_or(msg.updated.max(msg.created)), time)))
                    .child(div().flex().flex_wrap().gap(theme::sz(6.)).child(chip(label, tone)).child(chip(tier, tier_tone)).when(msg.urgent, |d| d.child(chip("Holding reply", Tone::Gold))))
                    .child(body(&msg.body))
                    .when(!msg.why.trim().is_empty(), |d| d.child(small(format!("Why: {}", msg.why.trim()))))
                    .when_some(msg.about.clone().filter(|a| !a.trim().is_empty()), |d, about| d.child(small(format!("About: {about}"))))
                    .when_some(msg.error.clone().filter(|_| msg.state == MsgState::Failed), |d, error| d.child(div().text_size(theme::sz(12.)).text_color(theme::error_text()).child(error)))
                    .when(msg.state == MsgState::Draft, |d| {
                        d.child(self.manager_decide_buttons(msg, ("manager-out-allow", i), ("manager-out-reject", i), cx))
                            .when(msg.tier == Tier::Never, |d| d.child(small("The manager never sends a message like this. Send it yourself if you mean it.".to_string())))
                    })
                    .into_any_element(),
            );
        }

        let mut inbox: Vec<&InMessage> = self.manager_inbox().iter().collect();
        inbox.sort_by_key(|m| std::cmp::Reverse(m.at));
        out.push(section("What came in").into_any_element());
        if inbox.is_empty() {
            out.push(lead("Nothing came in.").into_any_element());
        }
        for msg in inbox.iter().take(40) {
            out.push(
                stage_card()
                    .when(msg.urgent && !msg.handled, |d| d.border_color(theme::error_border()))
                    .child(head(msg.from.clone(), ago(msg.at, time)))
                    .when(msg.urgent || msg.handled, |d| d.child(div().flex().flex_wrap().gap(theme::sz(6.)).when(msg.urgent, |d| d.child(chip("Urgent", Tone::Err))).when(msg.handled, |d| d.child(chip("Dealt with", Tone::Plain)))))
                    .child(body(&msg.text))
                    .into_any_element(),
            );
        }
        out
    }

    /// Memory with the v2 engine: what waits for the person, then what memory keeps.
    fn manager_memory(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        use crate::memory_ui::MemoryChange;
        let mut out: Vec<AnyElement> = vec![lead("What memory keeps for you and your sessions, in plain Markdown, file by file. Nothing anyone proposes becomes memory until you accept it here.").into_any_element()];
        let page = &self.memory_page;
        if let Some(error) = &page.error {
            out.push(stage_card().child(div().text_size(theme::sz(12.5)).text_color(theme::error_text()).child(error.clone())).into_any_element());
        }
        let Some(view) = &page.view else {
            out.push(crate::motion::skeleton_rows("assistant-memory-wait", 3).into_any_element());
            return out;
        };
        let card = || div().flex().flex_col().rounded(theme::sz(12.)).border_1().border_color(theme::border()).bg(theme::raised()).overflow_hidden();
        // A line of memory: its text and where it is from, then what can be done with it, under
        // the text (the panel is narrow).
        let line = |i: usize, text: String, meta: String, buttons: Div| {
            div()
                .flex()
                .flex_col()
                .gap(theme::sz(6.))
                .px(theme::sz(12.))
                .py(theme::sz(10.))
                .when(i > 0, |d| d.border_t_1().border_color(theme::border()))
                .child(text_lines(&text).text_size(theme::sz(12.5)).line_height(theme::sz(18.)).text_color(theme::text()))
                .child(div().text_size(theme::sz(11.5)).line_height(theme::sz(16.)).text_color(theme::muted()).child(meta))
                .child(buttons.flex().flex_wrap().gap(theme::sz(6.)))
        };
        // What waits for the person.
        if view.waiting() > 0 {
            let all: Vec<String> = view.proposals.iter().map(|p| p.id.clone()).collect();
            out.push(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(theme::sz(8.))
                    .child(section(format!("Waiting for you ({})", view.waiting())).pt(theme::sz(0.)))
                    .when(all.len() > 1, |d| d.child(small_button("memory-accept-all", "Accept all", true).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.memory_change(MemoryChange::AcceptAll(all.clone()), cx)))))
                    .into_any_element(),
            );
            let mut rows: Vec<AnyElement> = Vec::new();
            for (i, proposal) in view.proposals.iter().enumerate() {
                let (yes, no) = (proposal.id.clone(), proposal.id.clone());
                let mut meta = vec![format!("to {}", proposal.to), proposal.from.clone()];
                if let Some(why) = &proposal.why {
                    meta.push(why.clone());
                }
                let buttons = div()
                    .child(small_button(("memory-accept", i), "Accept", true).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.memory_change(MemoryChange::Accept(yes.clone()), cx))))
                    .child(small_button(("memory-reject", i), "Reject", false).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.memory_change(MemoryChange::Reject(no.clone()), cx))));
                rows.push(line(i, proposal.text.clone(), meta.join(" · "), buttons).into_any_element());
            }
            for (i, held) in view.held.iter().enumerate() {
                let (keep, drop) = (held.path.clone(), held.path.clone());
                let buttons = div()
                    .child(small_button(("memory-keep", i), "Keep", true).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.memory_change(MemoryChange::KeepEdit(keep.clone()), cx))))
                    .child(small_button(("memory-discard", i), "Discard", false).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.memory_change(MemoryChange::DiscardEdit(drop.clone()), cx))));
                rows.push(line(view.proposals.len() + i, format!("{} was {} by hand in the memory folder", held.path, held.change), "held back: sessions do not see it until you keep it".into(), buttons).into_any_element());
            }
            out.push(card().children(rows).into_any_element());
        }
        // What memory keeps.
        if view.files.is_empty() {
            out.push(stage_card().child(lead("Memory keeps nothing yet. Move in (Settings › Manager), or tell the manager “remember …”.")).into_any_element());
            return out;
        }
        let mut n: usize = 0;
        for file in &view.files {
            let let_go = if file.let_go > 0 { format!(" · {} let go", file.let_go) } else { String::new() };
            out.push(section(format!("{}{let_go}", file.label)).into_any_element());
            let rows: Vec<AnyElement> = file
                .rows
                .iter()
                .enumerate()
                .map(|(i, row)| {
                    n += 1;
                    let id = row.id.clone();
                    let buttons = div().child(small_button(("memory-forget", n), "Forget", false).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.memory_change(MemoryChange::Forget(id.clone()), cx))));
                    line(i, row.text.clone(), format!("{} · {}", row.status, row.from), buttons).into_any_element()
                })
                .collect();
            if !rows.is_empty() {
                out.push(card().children(rows).into_any_element());
            }
        }
        out
    }

    /// The picture the manager explains, as a card of Today's centre: its title, the picture at
    /// the step it is on, a dot for each step, and a way to put it away. None while it shows none.
    pub(crate) fn render_scene_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let play = self.manager.scene.as_ref().filter(|_| self.state.manager.enabled && self.manager.scene_open)?;
        let steps = play.scene.steps.len();
        let at = play.step.min(steps.saturating_sub(1));
        let title = if play.scene.title.trim().is_empty() { format!("What {} drew", self.state.manager.name) } else { play.scene.title.trim().to_string() };
        let head = div()
            .flex()
            .items_center()
            .gap(theme::sz(8.))
            .child(icons::icon(icons::ASSISTANT, theme::sz(15.), theme::sz(15.), theme::accent()))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(theme::sz(13.)).font_weight(FontWeight::MEDIUM).text_color(theme::text_soft()).child(title))
            .child(header_button("manager-scene-close", icons::CLOSE, "Put the picture away").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.manager.scene_open = false;
                cx.notify();
            })));
        let dots = (steps > 1).then(|| {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_x(theme::sz(12.))
                .gap_y(theme::sz(6.))
                .child(div().flex().flex_wrap().items_center().gap(theme::sz(6.)).children((0..steps).map(|k| step_dot(("manager-scene-dot", k), k <= at).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.manager_scene_go(k, cx))))))
                .child(div().text_size(theme::sz(12.)).text_color(theme::card_soft_text()).child("One step per sentence. Click a step to jump."))
        });
        Some(
            crate::backdrop::glass_strong(theme::sz(14.))
                .flex()
                .flex_col()
                .gap(theme::sz(12.))
                .p(theme::sz(16.))
                .pt(theme::sz(12.))
                .when(theme::is_light(), |d| d.shadow_md())
                .child(head)
                .child(crate::scenes_ui::render_scene(&play.scene, at))
                .children(dots)
                .into_any_element(),
        )
    }
}

impl Workspace {
    /// Debug builds, a screen check (`GLASSCODE_DEBUG_TODAY=scene`): a made-up picture, as the
    /// manager would explain one. No name in it is a real one.
    pub(crate) fn manager_debug_scene(&mut self, cx: &mut Context<Self>) {
        use glasscode_core::manager::{Scene, SceneEdge, SceneKind, SceneNode, SceneStep};
        if !cfg!(debug_assertions) || !self.state.manager.enabled {
            return;
        }
        let node = |id: &str, label: &str, note: &str| SceneNode { id: id.into(), label: label.into(), note: note.into() };
        let edge = |from: &str, to: &str, label: &str, bad: bool| SceneEdge { from: from.into(), to: to.into(), label: label.into(), bad };
        let step = |say: &str, nodes: &[&str], edges: &[usize]| SceneStep { say: say.into(), nodes: nodes.iter().map(|n| n.to_string()).collect(), edges: edges.to_vec(), ..SceneStep::default() };
        let scene = Scene {
            title: "How the import test fails".into(),
            kind: SceneKind::Flow,
            nodes: vec![node("test", "import test", "Windows only"), node("cache", "build cache", "old key"), node("file", "large file", "2 GB"), node("fix", "retry with backoff", "acme/app#48")],
            edges: vec![edge("test", "cache", "reads", false), edge("cache", "file", "times out", true), edge("fix", "cache", "replaces", false)],
            steps: vec![
                step("The import test reads the build cache.", &["test", "cache"], &[0]),
                step("With the old key the cache asks for the large file again, and that times out.", &["file"], &[1]),
                step("The fix in acme/app#48 retries with a backoff in place of the old read.", &["fix"], &[2]),
            ],
            ..Scene::default()
        };
        self.manager_play(scene, cx);
        // The second step, and no moving on by itself: the picture stands still for the check.
        if let Some(play) = self.manager.scene.as_mut() {
            play.step = 1;
            play.next_at = None;
        }
    }

    /// Debug builds, a screen check (`GLASSCODE_DEBUG_TODAY=tour`): a made-up walk of three
    /// stops, standing at its second.
    pub(crate) fn manager_debug_tour(&mut self, cx: &mut Context<Self>) {
        use glasscode_core::manager::{Show, TourStop};
        if !cfg!(debug_assertions) || !self.state.manager.enabled {
            return;
        }
        let stop = |say: &str| TourStop { show: Show::default(), say: say.into() };
        let far = Instant::now() + std::time::Duration::from_secs(3600);
        self.manager.tour = Some(crate::manager_view::Tour { stops: vec![stop("Two sessions moved on by themselves."), stop("The checks of the settings page fail on one test."), stop("Nothing else needs you today.")], index: 1, next_at: far });
        self.manager.caption = "The checks of the settings page fail on one test.".into();
        self.manager.caption_until = Some(far);
        cx.notify();
    }
}

/// What the bubble's caption shows of what the manager said: its first paragraph in plain words
/// (an answer can be a page of Markdown; the whole of it is in the conversation).
fn caption_line(text: &str) -> String {
    let first = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
    let plain: String = first.trim_start_matches(['#', '-', '*', ' ']).chars().filter(|c| !matches!(c, '*' | '`')).collect();
    clip(&plain, 200)
}

/// A thing to say to the manager, one click: a quiet chip.
pub(crate) fn try_chip(id: SharedString, phrase: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(format!("Ask: {phrase}"))
        .flex()
        .flex_none()
        .items_center()
        .max_w_full()
        .h(theme::sz(26.))
        .px(theme::sz(10.))
        .rounded_full()
        .border_1()
        .border_color(theme::border())
        .text_size(theme::sz(12.))
        .text_color(theme::text_soft())
        .cursor_pointer()
        .hover(|s| s.bg(theme::tint()).text_color(theme::text()))
        .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(phrase))
}

/// How an outbox message stands, in a chip.
fn message_state(msg: &OutMessage) -> (&'static str, Tone) {
    match msg.state {
        MsgState::Draft => ("Waits for your OK", Tone::Warn),
        MsgState::Approved => ("Goes out next", Tone::Info),
        MsgState::Sent => ("Sent", Tone::Ok),
        MsgState::Rejected => ("Rejected", Tone::Plain),
        MsgState::Failed => ("Not sent", Tone::Err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glasscode_core::manager::{Blocker, ReviewAsk};

    #[test]
    fn thinking_shows_before_speaking_and_listening_only_while_live() {
        assert_eq!(presence(false, false, false), Presence::Asleep);
        assert_eq!(presence(true, false, false), Presence::Listening);
        assert_eq!(presence(true, false, true), Presence::Speaking);
        assert_eq!(presence(false, false, true), Presence::Speaking);
        assert_eq!(presence(true, true, true), Presence::Thinking);
    }

    #[test]
    fn the_header_says_what_it_is_doing() {
        assert_eq!(state_text(Presence::Listening, Mode::Talk, Listen::Wake, "Mira"), "listening");
        assert_eq!(state_text(Presence::Asleep, Mode::Talk, Listen::Wake, "Mira"), "asleep, say “Mira”");
        assert_eq!(state_text(Presence::Asleep, Mode::Talk, Listen::Never, "Mira"), "asleep");
        assert_eq!(state_text(Presence::Speaking, Mode::Talk, Listen::Wake, "Mira"), "speaking");
        assert_eq!(state_text(Presence::Speaking, Mode::Chat, Listen::Wake, "Mira"), "answering");
        assert_eq!(state_text(Presence::Thinking, Mode::Chat, Listen::Wake, "Mira"), "thinking");
        assert_eq!(state_text(Presence::Asleep, Mode::Chat, Listen::Wake, "Mira"), "ready");
        assert_eq!(state_text(Presence::Listening, Mode::Chat, Listen::Wake, "Mira"), "ready");
    }

    /// The panel's one word: idle, thinking or needs you; what it does now comes first.
    #[test]
    fn the_panel_says_idle_thinking_or_needs_you() {
        assert_eq!(state_word(Presence::Asleep, Mode::Chat, 0), ("idle", Signal::Idle));
        assert_eq!(state_word(Presence::Asleep, Mode::Chat, 2), ("needs you", Signal::NeedsYou));
        assert_eq!(state_word(Presence::Thinking, Mode::Chat, 2), ("thinking", Signal::Busy));
        assert_eq!(state_word(Presence::Speaking, Mode::Chat, 0), ("answering", Signal::Busy));
        assert_eq!(state_word(Presence::Speaking, Mode::Talk, 0), ("speaking", Signal::Busy));
        assert_eq!(state_word(Presence::Listening, Mode::Talk, 0), ("listening", Signal::Idle));
        assert_eq!(state_word(Presence::Listening, Mode::Talk, 1), ("needs you", Signal::NeedsYou));
    }

    #[test]
    fn times_read_as_words() {
        assert_eq!(ago(1000, 1030), "just now");
        assert_eq!(ago(1000, 1000 + 5 * 60), "5 min ago");
        assert_eq!(ago(1000, 1000 + 3600), "1 h ago");
        assert_eq!(ago(1000, 1000 + 3 * 3600), "3 h ago");
        assert_eq!(ago(1000, 1000 + 2 * 86_400), "2 d ago");
        assert_eq!(ago(2000, 1000), "just now", "a clock that is a little ahead");
        assert_eq!(every(600, false), "every 10 min");
        assert_eq!(every(3600, false), "every hour");
        assert_eq!(every(4 * 3600, false), "every 4 h");
        assert_eq!(every(86_400, false), "every day");
        assert_eq!(every(600, true), "once");
    }

    #[test]
    fn one_line_of_a_longer_text() {
        assert_eq!(first_line("\n  \nFirst\nSecond"), "First");
        assert_eq!(last_line("First\nShall I push?\n\n"), "Shall I push?");
        assert_eq!(first_line(""), "");
        let long = "x".repeat(400);
        assert_eq!(first_line(&long).chars().count(), 181);
    }

    fn waiting() -> (Vec<InMessage>, Vec<OutMessage>, Vec<FollowUp>, Snapshot) {
        let inbox = vec![
            InMessage { id: "i1".into(), from: "Sam".into(), text: "The release is stuck".into(), urgent: true, ..InMessage::default() },
            InMessage { id: "i2".into(), from: "Alex".into(), text: "Lunch?".into(), ..InMessage::default() },
            InMessage { id: "i3".into(), from: "Mira".into(), text: "Seen already".into(), urgent: true, handled: true, ..InMessage::default() },
        ];
        let outbox = vec![
            OutMessage { id: "o1".into(), to: "Alex".into(), body: "The fix is pushed.\n\nMira's manager".into(), state: MsgState::Draft, ..OutMessage::default() },
            OutMessage { id: "o2".into(), to: "Sam".into(), body: "Sent before".into(), state: MsgState::Sent, ..OutMessage::default() },
        ];
        let followups = vec![
            FollowUp { id: "f1".into(), state: FollowState::Fixed, number: 41, person: "Alex".into(), title: "Retry with backoff".into(), ..FollowUp::default() },
            FollowUp { id: "f2".into(), state: FollowState::Open, number: 42, ..FollowUp::default() },
        ];
        let snap = Snapshot {
            sessions: vec![
                SessionInfo { id: "s1".into(), title: "Backoff".into(), state: SessionState::NeedsYou, last_reply: Some("Done with the tests.\nShall I push?".into()), ..SessionInfo::default() },
                SessionInfo { id: "s2".into(), title: "Docs".into(), state: SessionState::Working, ..SessionInfo::default() },
            ],
            review_asks: vec![ReviewAsk { number: 7, title: "Quiz order".into(), author: "Sam".into(), ..ReviewAsk::default() }],
            approvals: 2,
            ..Snapshot::default()
        };
        (inbox, outbox, followups, snap)
    }

    #[test]
    fn what_needs_the_person_is_listed_most_pressing_first() {
        let (inbox, outbox, followups, snap) = waiting();
        let list = needs(&inbox, &outbox, &followups, &snap);
        let titles: Vec<&str> = list.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(titles, vec!["Urgent from Sam", "Backoff", "2 commands wait for your OK", "A message to Alex waits for your OK", "Alex says #41 is fixed", "Sam asks for your review"]);
        assert!(list[0].urgent && !list[1].urgent);
        assert_eq!(list[1].detail, "Shall I push?");
        assert_eq!(list[1].go, Go::Show(to_session("s1")));
        assert_eq!(list[2].go, Go::Show(to_view("today")));
        assert_eq!(list[3].detail, "The fix is pushed.");
        assert_eq!(list[3].go, Go::Stage(Stage::Messages));
        assert_eq!(list[4].go, Go::Stage(Stage::FollowUps));
        assert_eq!(list[5].detail, "#7 Quiz order");
    }

    /// Today's cards list sessions, commands and reviews from the app's own lists: the manager
    /// adds only what it holds itself, and each of those rows opens its part of the panel.
    #[test]
    fn today_gets_from_the_manager_only_what_it_holds() {
        let (inbox, outbox, followups, snap) = waiting();
        let held = held_by_the_manager(needs(&inbox, &outbox, &followups, &snap));
        let titles: Vec<&str> = held.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(titles, vec!["Urgent from Sam", "A message to Alex waits for your OK", "Alex says #41 is fixed"]);
        assert!(held.iter().all(|need| matches!(need.go, Go::Stage(Stage::Messages | Stage::FollowUps))));
    }

    #[test]
    fn nothing_waits_when_all_is_quiet() {
        assert!(needs(&[], &[], &[], &Snapshot::default()).is_empty());
        assert_eq!(sentence(0, 0), "");
    }

    #[test]
    fn the_days_sentence_counts_what_moves_and_what_waits_on_others() {
        assert_eq!(sentence(1, 0), "1 session is moving without you.");
        assert_eq!(sentence(3, 1), "3 sessions are moving without you. 1 thing waits on other people.");
        assert_eq!(sentence(0, 2), "2 things wait on other people.");
    }

    #[test]
    fn a_follow_up_reads_as_who_and_how_far() {
        let mut f = FollowUp {
            kind: FollowKind::BlockersGiven,
            number: 41,
            login: "alex-dev".into(),
            blockers: vec![Blocker { resolved: true, ..Blocker::default() }, Blocker::default(), Blocker::default()],
            ..FollowUp::default()
        };
        assert_eq!(follow_title(&f), "Your review on #41 · alex-dev");
        assert_eq!(follow_sub(&f, 0), "1 of 3 blockers done");
        f.person = "Alex".into();
        f.state = FollowState::Fixed;
        assert_eq!(follow_title(&f), "Your review on #41 · Alex");
        assert_eq!(follow_sub(&f, 0), "Says it is fixed. 1 of 3 blockers done");
        let received = FollowUp { kind: FollowKind::ReviewReceived, number: 9, title: "Relay reconnect".into(), ..FollowUp::default() };
        assert_eq!(follow_title(&received), "Review on your #9");
        assert_eq!(follow_sub(&received, 0), "Relay reconnect");
        let custom = FollowUp { kind: FollowKind::Custom, note: "Ask Sam about the token".into(), person: "Sam".into(), created: 100, ..FollowUp::default() };
        assert_eq!(follow_title(&custom), "Ask Sam about the token · Sam");
        assert_eq!(follow_sub(&custom, 100 + 7200), "Kept since 2 h ago");
    }

    #[test]
    fn the_caption_is_the_first_paragraph_in_plain_words() {
        assert_eq!(caption_line("Here is what **Mira** can do.

**Sessions**
- The `sessions`"), "Here is what Mira can do.");
        assert_eq!(caption_line("
## Today
- one"), "Today");
        assert_eq!(caption_line(""), "");
        assert_eq!(caption_line(&"x".repeat(500)).chars().count(), 201);
    }

    #[test]
    fn a_session_reports_its_state_and_its_last_line() {
        let s = SessionInfo { state: SessionState::Working, last_reply: Some("Reading the code.\nRunning the tests now".into()), ..SessionInfo::default() };
        assert_eq!(session_line(&s), "Working · Running the tests now");
        assert_eq!(session_line(&SessionInfo::default()), "Not running");
    }
}
