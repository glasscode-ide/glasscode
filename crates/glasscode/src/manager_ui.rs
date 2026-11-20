//! The manager's page (one full page: a place to talk or type on the left, everything it
//! holds on the right), its row at the top of the left sidebar, and the small bubble that stays in a
//! corner of every other screen while it talks.
//!
//! The page has few buttons on purpose: a row opens what it describes, and things are done by
//! asking. Only what must be decided by hand has a button (allow or reject a message, a
//! follow-up that is done, a note to forget).

use std::time::Instant;

use gpui::{AnyElement, ClickEvent, Context, Div, ElementId, FontWeight, HighlightStyle, Hsla, MouseButton, MouseDownEvent, MouseUpEvent, SharedString, Stateful, StyledText, Window, deferred, div, prelude::*, px, relative};
use glasscode_core::manager::{Data, FollowKind, FollowState, FollowUp, Home, InMessage, Listen, MsgState, OutMessage, SendPolicy, SessionInfo, SessionState, Show, Snapshot, Tier, Who, followups, memory};
use glasscode_core::model::now;

use crate::manager_state::{Mode, Stage};
use crate::scenes_ui::{gold, gold_bg, gold_line, text_lines};
use crate::settings_ui::Section;
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::shell::View;
use crate::ui;
use crate::workspace::Workspace;

/// What the page and Settings › Manager keep between frames.
#[derive(Default)]
pub struct PageUi {
    /// Where the conversation is scrolled to, and how many lines it had when last drawn (a
    /// new line scrolls it to the bottom).
    pub chat_scroll: gpui::ScrollHandle,
    pub chat_seen: usize,
    /// The text fields of Settings › Manager, made the first time that section is drawn.
    pub form: std::cell::OnceCell<crate::manager_settings::ManagerForm>,
}

/// Things to say to it, one click each.
const TRY: [&str; 5] = ["Give me today's update", "What needs me?", "What is blocked?", "Which follow-ups are open?", "Explain what my last session changed"];

/// The most lines of the conversation drawn at once (the newest ones).
const CHAT_LINES: usize = 200;

/// Below this window width the tiles sit in one column.
const NARROW: f32 = 1240.;

// ----- what it is doing --------------------------------------------------------------------

/// How the manager looks right now: its orb, its bubble and the words in its page's header.
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

/// The words after its name in the page's header.
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
enum Go {
    Show(Show),
    Stage(Stage),
}

/// One thing that waits for the person.
#[derive(Clone, Debug, PartialEq)]
struct Need {
    title: String,
    detail: String,
    go: Go,
    urgent: bool,
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
fn needs(data: &Data, snap: &Snapshot) -> Vec<Need> {
    let mut out = Vec::new();
    for m in data.inbox.iter().filter(|m| m.urgent && !m.handled) {
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
    for m in data.outbox.iter().filter(|m| m.state == MsgState::Draft) {
        out.push(Need { title: format!("A message to {} waits for your OK", m.to), detail: first_line(&m.body), go: Go::Stage(Stage::Messages), urgent: m.urgent });
    }
    for f in data.followups.iter().filter(|f| f.state == FollowState::Fixed) {
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

fn session_dot(state: SessionState) -> Hsla {
    match state {
        SessionState::NotRunning => theme::faint(),
        SessionState::Idle => theme::codex(),
        SessionState::Working => theme::added(),
        SessionState::NeedsYou => theme::attention(),
    }
}

// ----- building blocks ---------------------------------------------------------------------

/// Its avatar: a circle with its initials.
fn avatar(initials: &str, size: f32, muted: bool) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded_full()
        .border_1()
        .border_color(if muted { theme::border_strong() } else { gold() })
        .bg(if muted { theme::panel() } else { gold_bg() })
        .text_color(if muted { theme::muted() } else { gold() })
        .text_size(px((size * 0.37).max(8.5)))
        .line_height(px(size * 0.5))
        .font_weight(FontWeight::SEMIBOLD)
        .child(initials.to_string())
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
        .h(px(20.))
        .px_2()
        .rounded_full()
        .text_size(px(11.))
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
        .h(px(26.))
        .px_2p5()
        .rounded(px(7.))
        .text_size(px(12.5))
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
    div().id(id).flex_none().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(gold()).cursor_pointer().hover(|s| s.text_color(theme::text_strong())).child(label.into())
}

/// A tile of the overview: its title line, then its rows.
fn tile(head: impl IntoElement, rows: Vec<AnyElement>) -> Div {
    div().flex().flex_col().min_w_0().rounded(px(10.)).border_1().border_color(theme::border()).bg(theme::raised()).overflow_hidden().child(head).children(rows)
}

/// A tile's title line: its name on the left, a count or a chevron on the right.
fn tile_head(id: &'static str, title: &'static str, right: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .px_3()
        .py(px(9.))
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::muted())
        .child(title)
        .child(div().font_weight(FontWeight::MEDIUM).child(right.into()))
}

/// A row of a tile.
fn row(id: impl Into<ElementId>) -> Stateful<Div> {
    div().id(id).flex().items_center().gap(px(10.)).px_3().py(px(9.)).border_t_1().border_color(theme::border())
}

/// A row's two lines: what it is, and one line about it.
fn row_text(title: impl Into<SharedString>, sub: impl Into<SharedString>) -> Div {
    let sub: SharedString = sub.into();
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .child(div().truncate().font_weight(FontWeight::MEDIUM).text_color(theme::text_strong()).child(title.into()))
        .when(!sub.is_empty(), |d| d.child(div().truncate().text_size(px(12.5)).text_color(theme::muted()).child(sub)))
}

/// The row a tile shows when it has nothing.
fn quiet_row(text: &'static str) -> AnyElement {
    div().px_3().py(px(9.)).border_t_1().border_color(theme::border()).text_size(px(12.5)).text_color(theme::muted()).child(text).into_any_element()
}

fn chevron() -> Div {
    div().flex_none().text_color(theme::muted()).child("›")
}

/// One bar per blocker: green when it is done.
fn blocker_bars(f: &FollowUp) -> Div {
    div().flex().gap_1().children(f.blockers.iter().map(|b| div().flex_1().h(px(5.)).rounded(px(3.)).bg(if b.resolved { theme::added() } else { theme::border_strong() })))
}

/// A card of a detail stage.
fn stage_card() -> Div {
    div().flex().flex_col().min_w_0().gap(px(10.)).p(px(14.)).rounded(px(10.)).border_1().border_color(theme::border()).bg(theme::raised())
}

fn lead(text: impl Into<SharedString>) -> Div {
    div().max_w(px(640.)).text_size(px(15.)).line_height(px(22.)).text_color(theme::text_soft()).child(text.into())
}

/// The heights of the orb's bars while it speaks: a still wave. They do not move: an
/// animation draws the whole window on each display frame (the owner's rule since U25).
const SPEAKING_BARS: [f32; 5] = [14., 26., 34., 22., 12.];

/// The orb of Talk mode: quiet bars at rest, a wave of bars while it speaks, a soft ring while
/// it listens, a dashed edge while it thinks. Nothing in it moves.
fn orb(look: Presence) -> Stateful<Div> {
    let asleep = look == Presence::Asleep;
    let bars = div().flex().items_center().gap(px(5.)).h(px(40.)).children((0..5usize).map(|i| {
        let bar = div().flex_none().w(px(5.)).rounded(px(3.)).bg(if asleep { theme::border_strong() } else { gold() });
        bar.h(px(if look == Presence::Speaking { SPEAKING_BARS[i] } else { 8. }))
    }));
    let disc = div()
        .absolute()
        .top(px(10.))
        .left(px(10.))
        .size(px(132.))
        .rounded_full()
        .border_2()
        .border_color(if asleep { theme::border_strong() } else { gold() })
        .when(!asleep, |d| d.bg(gold_bg()))
        .when(look == Presence::Thinking, |d| d.border_dashed())
        .flex()
        .items_center()
        .justify_center()
        .child(bars);
    let ring = |inset: f32| div().absolute().top(px(inset)).left(px(inset)).size(px(152. - 2. * inset)).rounded_full().border_1().border_color(gold_line());
    div()
        .id("manager-orb")
        .relative()
        .flex_none()
        .size(px(152.))
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
        .w(px(26.))
        .h(px(14.))
        .cursor_pointer()
        .child(div().w_full().h(px(4.)).rounded(px(2.)).bg(if on { gold() } else { theme::border_strong() }))
}

// ----- the page, the rail icon and the bubble ----------------------------------------------

impl Workspace {
    /// Open its page.
    pub fn show_manager(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.manager_ensure_input(cx);
        self.manager.page.chat_scroll.scroll_to_bottom();
        self.show_view(View::Manager, cx);
    }

    /// The box to type to it in, made the first time its page is opened or drawn.
    fn manager_ensure_input(&mut self, cx: &mut Context<Self>) {
        if self.manager.input.is_some() {
            return;
        }
        let input = cx.new(|cx| TextInput::new("Ask your manager", cx).bare());
        cx.subscribe(&input, |this: &mut Workspace, input, event: &InputEvent, cx| match event {
            InputEvent::Submit => this.manager_send_typed(cx),
            InputEvent::Cancel => input.update(cx, |i, cx| i.set_text("", cx)),
            InputEvent::Changed => {}
        })
        .detach();
        self.manager.input = Some(input);
    }

    /// Send what is in the box.
    fn manager_send_typed(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.manager.input.clone() else { return };
        let (text, untyped) = {
            let input = input.read(cx);
            let all = input.text();
            let (text, untyped) = glasscode_core::words::trimmed(&all, input.typing());
            (text.to_string(), untyped)
        };
        if text.is_empty() {
            return;
        }
        // Typed in the Manager chat: the person's words, where they typed them (D1).
        let words = glasscode_core::words::record_with(glasscode_core::words::Origin::Composer(glasscode_core::words::ComposerKind::ManagerChat), &text, &untyped, None, None);
        input.update(cx, |i, cx| i.set_text("", cx));
        // With the v2 engine, a typed "yes" approves the one card that waits, when exactly one does
        // (M21.2); with two or more it binds to none, and a paste or a spoken one never binds.
        if self.control.engine_v2 {
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

    pub(crate) fn manager_set_stage(&mut self, stage: Stage, cx: &mut Context<Self>) {
        self.manager.stage = stage;
        // v2's memory is read for the page when it opens (and after each change there).
        if stage == Stage::Memory && self.control.engine_v2 {
            self.memory_load(cx);
        }
        cx.notify();
    }

    fn manager_go(&mut self, go: &Go, window: &mut Window, cx: &mut Context<Self>) {
        match go {
            Go::Show(show) => self.manager_show(show, window, cx),
            Go::Stage(stage) => self.manager_set_stage(*stage, cx),
        }
    }

    /// The person says a follow-up is done.
    fn manager_follow_done(&mut self, id: &str, cx: &mut Context<Self>) {
        if followups::close(&mut self.manager.harness.data, id, false, now()).is_some() {
            self.manager.harness.dirty = true;
        }
        cx.notify();
    }

    /// The person has it forget a note.
    fn manager_forget_note(&mut self, id: &str, cx: &mut Context<Self>) {
        if memory::forget(&mut self.manager.harness.data, id, now()).is_some() {
            self.manager.harness.dirty = true;
        }
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

    /// Its row at the top of the left sidebar, above Today (the rail that had its icon is
    /// gone): its avatar, "Manager" and a badge of what waits for the person, in the form of
    /// the sidebar's other rows (`shell::nav_row`). None while the manager is off.
    pub fn render_manager_nav(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.state.manager.enabled {
            return None;
        }
        let on = self.view == View::Manager;
        let waiting = self.manager.harness.needs_you(&self.manager_snapshot());
        let color = if on { theme::text_strong() } else { theme::text_soft() };
        Some(
            div()
                .id("nav-manager")
                .flex()
                .items_center()
                .gap_2p5()
                .h(theme::sz(30.))
                .px_2()
                .rounded_md()
                .cursor_pointer()
                .text_color(color)
                .when(on, |d| d.bg(theme::selected()).font_weight(FontWeight::MEDIUM))
                .hover(|s| s.bg(theme::hover()))
                .child(avatar(&self.state.manager.initials(), 18. * theme::scale(), false))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child("Manager"))
                .when(waiting > 0, |d| {
                    d.child(
                        div()
                            .flex_none()
                            .min_w(theme::sz(18.))
                            .h(theme::sz(18.))
                            .px(theme::sz(5.))
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(theme::attention())
                            .text_color(theme::on_attention())
                            .text_size(theme::sz(10.5))
                            .font_weight(FontWeight::BOLD)
                            .child(waiting.to_string()),
                    )
                })
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.go(View::Manager, window, cx)))
                .into_any_element(),
        )
    }

    /// The bubble on every screen but its own page: small, in the bottom right corner, with a
    /// caption only while it talks. None while the manager is off or on its page.
    pub fn render_manager_bubble(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.state.manager.enabled || self.view == View::Manager {
            return None;
        }
        let m = &self.manager;
        let look = self.manager_presence();
        let caption = if look == Presence::Thinking {
            "Thinking…".to_string()
        } else if m.caption.trim().is_empty() {
            m.tour.as_ref().and_then(|t| t.stops.get(t.index)).map(|s| s.say.clone()).unwrap_or_default()
        } else {
            m.caption.clone()
        };
        let talking = m.tour.is_some() || look == Presence::Thinking || look == Presence::Speaking || m.caption_until.is_some_and(|until| until > Instant::now());
        let dots = m.tour.as_ref().filter(|t| t.stops.len() > 1).map(|tour| {
            div().flex().flex_wrap().items_center().gap(px(6.)).children((0..tour.stops.len()).map(|k| {
                step_dot(("manager-tour-dot", k), k <= tour.index).occlude().on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.manager_tour_go(k, cx)))
            }))
        });
        // With the v2 engine the bubble is the chat's collapsed state (M52.1, ux-3): a click opens
        // the chat over the window; its count is what was said since, its mark the cards waiting.
        let v2 = self.control.engine_v2;
        let overlay = v2 && self.mchat.overlay;
        let (unread, waiting) = if v2 { self.chat_unread() } else { (0, 0) };
        // The caption takes no clicks: what is under it stays reachable. Only the dots and
        // the avatar do.
        let card = (!overlay && talking && (!caption.trim().is_empty() || dots.is_some())).then(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .max_w(px(280.))
                .px(px(11.))
                .py_2()
                .rounded(px(12.))
                .rounded_br(px(4.))
                .border_1()
                .border_color(theme::border())
                .bg(theme::panel())
                .shadow_md()
                .when(!caption.trim().is_empty(), |d| d.child(div().text_size(px(12.5)).line_height(px(17.5)).text_color(theme::text_soft()).line_clamp(7).child(caption.trim().to_string())))
                .when_some(dots, |d, dots| d.child(dots))
        });
        let asleep = look == Presence::Asleep;
        let ring = || div().absolute().top(px(0.)).left(px(0.)).size(px(50.)).rounded_full().border_1().border_color(gold());
        let face = div()
            .relative()
            .flex_none()
            .size(px(50.))
            .when(look == Presence::Listening, |d| d.child(ring().opacity(0.3)))
            // A still ring while it speaks: no animation (it would draw the window each frame).
            .when(look == Presence::Speaking, |d| d.child(ring().opacity(0.7)))
            .child(
                div()
                    .id("manager-bubble")
                    .absolute()
                    .top(px(5.))
                    .left(px(5.))
                    .rounded_full()
                    .shadow_md()
                    .cursor_pointer()
                    .occlude()
                    .hover(|s| s.opacity(0.9))
                    .role(gpui::Role::Button)
                    .aria_label(match (v2, overlay) {
                        (true, true) => format!("Close the chat with {}", self.state.manager.name),
                        (true, false) if waiting > 0 => format!("Chat with {}: {waiting} waiting for you", self.state.manager.name),
                        (true, false) if unread > 0 => format!("Chat with {}: {unread} new", self.state.manager.name),
                        (true, false) => format!("Chat with {}", self.state.manager.name),
                        _ => format!("Open {}'s page", self.state.manager.name),
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        if v2 {
                            this.run_op_named("ui.panel", serde_json::json!({ "side": "chat", "hide": overlay }), window, cx);
                        } else {
                            this.show_manager(window, cx);
                        }
                    }))
                    .child(avatar(&self.state.manager.initials(), 40., asleep).text_size(px(13.)).when(look == Presence::Thinking, |d| d.border_dashed())),
            )
            // Not colour only: the count is a number, the mark a ring with its own label above.
            .when(!overlay && (unread > 0 || waiting > 0), |d| {
                d.child(
                    div()
                        .absolute()
                        .top(px(0.))
                        .right(px(0.))
                        .min_w(px(17.))
                        .h(px(17.))
                        .px(px(4.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(if waiting > 0 { theme::attention() } else { theme::accent() })
                        .text_size(px(10.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::on_accent())
                        .child(if waiting > 0 { format!("{waiting}!") } else { unread.min(99).to_string() }),
                )
            });
        Some(
            deferred(
                div()
                    .absolute()
                    .right(px(9.))
                    // Above the status bar, at any scale.
                    .bottom(theme::sz(crate::status_bar::BAR_HEIGHT) + px(9.))
                    .w(px(if overlay { 390. } else { 290. }))
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap(px(6.))
                    .when(overlay, |d| d.child(self.render_chat_overlay(cx)))
                    .when_some(card, |d, card| d.child(div().pr(px(5.)).child(card)))
                    .child(face),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    /// Its page (the main area when the rail's manager icon is chosen).
    pub fn render_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if !self.state.manager.enabled {
            // Memory is v2's, not the manager's: its page opens with the manager off too.
            if self.control.engine_v2 && self.manager.stage == Stage::Memory {
                let snap = self.manager_snapshot();
                let narrow = window.viewport_size().width < px(NARROW);
                return div().flex().flex_1().min_w_0().h_full().bg(theme::bg()).child(self.manager_stage(&snap, narrow, cx)).into_any_element();
            }
            return self.manager_off_card(cx);
        }
        self.manager_ensure_input(cx);
        // A new line in the conversation scrolls it to the bottom.
        let lines = self.manager.harness.data.chat.len();
        if lines != self.manager.page.chat_seen {
            self.manager.page.chat_seen = lines;
            self.manager.page.chat_scroll.scroll_to_bottom();
        }
        let snap = self.manager_snapshot();
        let narrow = window.viewport_size().width < px(NARROW);
        let typing = self.manager.input.as_ref().is_some_and(|input| input.read(cx).focus_handle_clone().is_focused(window));
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme::bg())
            .child(self.manager_header(cx))
            .child(div().flex().flex_1().min_h_0().child(self.manager_talk_column(typing, cx)).child(self.manager_stage(&snap, narrow, cx)))
            .into_any_element()
    }

    /// What the page shows when the manager is off.
    fn manager_off_card(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .justify_center()
            .bg(theme::bg())
            .child(
                ui::card()
                    .w(px(420.))
                    .gap_3()
                    .p_6()
                    .child(div().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child("The manager is off"))
                    .child(div().text_color(theme::muted()).child("One agent above all your sessions. Nothing of it runs until you switch it on."))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(ui::primary_button("manager-off-settings", "Open Settings › Manager").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_settings(Section::Manager, window, cx))))
                            // Memory works with the manager off (v2): sessions read and write it.
                            .when(self.control.engine_v2, |d| d.child(ui::button("manager-off-memory", "Open memory").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.manager_set_stage(Stage::Memory, cx))))),
                    ),
            )
            .into_any_element()
    }

    fn manager_header(&self, cx: &mut Context<Self>) -> Div {
        let settings = &self.state.manager;
        let m = &self.manager;
        let all = self.state.projects.len();
        let managed = self.state.projects.iter().filter(|p| settings.manages(&p.id)).count();
        let home = match settings.home {
            Home::Server if self.manager_on_server() => "runs on the server",
            Home::Server => "runs on this Mac until the server answers",
            Home::Mac => "runs on this Mac",
        };
        let state = state_text(self.manager_presence(), m.mode, settings.listen, &settings.name);
        let mode = |id: &'static str, mode: Mode, label: &'static str| {
            ui::segment(id, m.mode == mode, label).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.manager.mode = mode;
                this.manager.page.chat_scroll.scroll_to_bottom();
                cx.notify();
            }))
        };
        div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap_x(px(14.))
            .gap_y(px(10.))
            .px_5()
            .py(px(14.))
            .border_b_1()
            .border_color(theme::border())
            .child(avatar(&settings.initials(), 40., false))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(div().truncate().text_size(px(18.)).line_height(px(24.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(settings.name.clone()))
                    .child(div().truncate().text_size(px(12.5)).text_color(theme::muted()).child(format!("Manages {managed} of {} · {home} · {state}", plural(all, "project", "projects")))),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .child(ui::segmented().child(mode("manager-mode-talk", Mode::Talk, "Talk")).child(mode("manager-mode-chat", Mode::Chat, "Chat")))
                    .child(
                        div()
                            .id("manager-settings")
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(32.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(theme::border_strong())
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_settings(Section::Manager, window, cx)))
                            .child(crate::icons::icon(crate::icons::SETTINGS, px(17.), px(17.), theme::muted())),
                    ),
            )
    }

    /// The left column: the one place to talk or type to it.
    fn manager_talk_column(&self, typing: bool, cx: &mut Context<Self>) -> Div {
        let m = &self.manager;
        let body = match m.mode {
            Mode::Talk => self.manager_talk(cx),
            // With the v2 engine: the conversation store's thread (M52.1), the left column of the
            // page (ux-3), its topics a row above it.
            Mode::Chat if self.control.engine_v2 => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(self.render_chat_topics(cx))
                .child(
                    div()
                        .id("manager-chat")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&m.page.chat_scroll)
                        .p_4()
                        .child(self.render_chat_thread(crate::manager_chat::ThreadView { id: "manager-thread", limit: 80, compact: false }, cx)),
                )
                .into_any_element(),
            Mode::Chat => self.manager_chat(cx),
        };
        let mut tries: Vec<AnyElement> = vec![div().flex_none().child("Try:").into_any_element()];
        for (i, phrase) in TRY.iter().enumerate() {
            if i > 0 {
                tries.push(div().flex_none().text_color(theme::faint()).child("·").into_any_element());
            }
            tries.push(
                div()
                    .id(("manager-try", i))
                    .flex_none()
                    .text_color(theme::text_soft())
                    .underline()
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme::text_strong()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.manager_ask(phrase.to_string(), false, cx);
                        cx.notify();
                    }))
                    .child(*phrase)
                    .into_any_element(),
            );
        }
        let composer = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .px_4()
            .pt(px(6.))
            .pb_4()
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .h(px(40.))
                    .px(px(15.))
                    .rounded_full()
                    .border_1()
                    .border_color(if typing { theme::accent() } else { theme::border_strong() })
                    .bg(theme::input())
                    .children(m.input.clone()),
            )
            .child(
                div()
                    .id("manager-send")
                    .role(gpui::Role::Button)
                    .aria_label(format!("Send to {}", self.state.manager.name))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(38.))
                    .rounded_full()
                    .bg(theme::accent())
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.88))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.manager_send_typed(cx)))
                    .child(crate::icons::icon(crate::icons::SEND, px(16.), px(16.), theme::on_accent())),
            );
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(relative(0.4))
            .min_w(px(300.))
            .max_w(px(560.))
            .h_full()
            .border_r_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .child(body)
            .child(div().flex().flex_none().flex_wrap().items_center().gap_x_1p5().gap_y_0p5().px(px(18.)).pb(px(6.)).text_size(px(12.)).text_color(theme::muted()).children(tries))
            .child(composer)
    }

    /// Talk mode: the orb, what it says, and how to wake it.
    fn manager_talk(&self, cx: &mut Context<Self>) -> AnyElement {
        let m = &self.manager;
        let settings = &self.state.manager;
        let look = self.manager_presence();
        let caption = if look == Presence::Thinking { "Thinking…".to_string() } else { m.caption.trim().to_string() };
        let hint = match (&m.voice.problem, settings.listen) {
            (Some(problem), _) => problem.clone(),
            (None, Listen::Wake) => format!("Tap him to sleep or wake, or say “{}”.", settings.name),
            (None, Listen::Hold) => "Hold him to talk.".to_string(),
            (None, Listen::Never) => "He does not listen by himself (Settings › Manager). Tap him for this once, or type below.".to_string(),
        };
        // "While you hold": he listens while the orb is pressed (his voice is started first when
        // it is not on), and stops when it is let go, on the orb or anywhere else. Otherwise a
        // click wakes him or lets him sleep.
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
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .items_center()
            .justify_center()
            .gap(px(18.))
            .p_4()
            .child(orb)
            .when(!caption.is_empty(), |d| d.child(div().flex_none().max_w(px(340.)).text_size(px(16.)).line_height(px(23.)).text_color(theme::text()).text_center().line_clamp(9).child(caption)))
            .child(div().flex_none().max_w(px(340.)).text_size(px(12.)).text_color(if m.voice.problem.is_some() { theme::attention() } else { theme::muted() }).text_center().child(hint))
            .into_any_element()
    }

    /// Chat mode: the conversation, newest at the bottom.
    fn manager_chat(&self, cx: &mut Context<Self>) -> AnyElement {
        let m = &self.manager;
        let chat = &m.harness.data.chat;
        let from = chat.len().saturating_sub(CHAT_LINES);
        let mut lines: Vec<AnyElement> = Vec::new();
        for (i, line) in chat.iter().enumerate().skip(from) {
            let bubble = div().flex().flex_col().flex_none().gap(px(6.)).px_3().py_2().rounded(px(12.)).text_size(px(13.5)).line_height(px(19.5));
            let bubble = match line.who {
                Who::You => bubble.self_end().max_w(relative(0.92)).bg(theme::selected()).text_color(theme::text_strong()),
                Who::Manager => bubble.self_start().max_w(relative(0.92)).border_1().border_color(theme::border()).bg(theme::panel()).text_color(theme::text()),
                Who::System => bubble.self_stretch().bg(theme::error_bg()).text_color(theme::error_text()).text_size(px(12.5)),
            };
            let show = line.show.clone();
            lines.push(
                bubble
                    .child(text_lines(&line.text))
                    .when_some(show, |d, show| {
                        d.child(div().flex().child(link(("manager-chat-open", i), "Open ›").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.manager_show(&show, window, cx)))))
                    })
                    .into_any_element(),
            );
        }
        if chat.is_empty() {
            lines.push(div().flex_none().text_size(px(12.5)).text_color(theme::muted()).child("Ask him anything about your sessions, reviews and follow-ups. What you say and what he answers stays here.").into_any_element());
        }
        if m.thinking {
            lines.push(div().flex_none().self_start().text_size(px(12.5)).text_color(theme::muted()).child(format!("{} is thinking…", self.state.manager.name)).into_any_element());
        }
        div()
            .id("manager-chat")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&m.page.chat_scroll)
            .gap(px(10.))
            .p_4()
            .children(lines)
            .into_any_element()
    }

    /// The right side: everything it holds, or one part of it, or the picture it explains.
    fn manager_stage(&self, snap: &Snapshot, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let content = match self.manager.stage {
            Stage::Overview => self.manager_overview(snap, narrow, cx),
            Stage::FollowUps => self.manager_followups(cx),
            Stage::Messages => self.manager_messages(narrow, cx),
            Stage::Memory => self.manager_memory(cx),
            Stage::Scene if self.manager.scene.is_some() => self.manager_scene(cx),
            Stage::Scene => self.manager_overview(snap, narrow, cx),
        };
        div()
            .id("manager-stage")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .child(div().flex().flex_col().gap(px(14.)).px_5().pt(px(18.)).pb(px(28.)).children(content))
            .into_any_element()
    }

    /// The way back to the overview and a detail stage's title.
    fn manager_stage_head(&self, title: impl Into<SharedString>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            div()
                .flex()
                .child(
                    div()
                        .id("manager-back")
                        .text_size(px(12.5))
                        .text_color(theme::muted())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::text_strong()))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.manager_set_stage(Stage::Overview, cx)))
                        .child("‹ Everything"),
                )
                .into_any_element(),
            div().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(title.into()).into_any_element(),
        ]
    }

    fn manager_overview(&self, snap: &Snapshot, narrow: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let m = &self.manager;
        let data = &m.harness.data;
        let time = now();
        let needs = needs(data, snap);
        let moving = snap.sessions.iter().filter(|s| s.state == SessionState::Working).count();
        let open = followups::open(data);
        let mut out: Vec<AnyElement> = Vec::new();

        // One sentence, its date in bold.
        let date = format!("{}.", glasscode_core::day::long(glasscode_core::day::today()));
        let sentence = format!(
            "{date} {} you. {} moving without you. {} on other people.",
            if needs.len() == 1 { "1 thing needs".to_string() } else { format!("{} things need", needs.len()) },
            if moving == 1 { "1 session is".to_string() } else { format!("{moving} sessions are") },
            if open.len() == 1 { "1 thing waits".to_string() } else { format!("{} things wait", open.len()) },
        );
        let bold = HighlightStyle { color: Some(theme::text_strong()), font_weight: Some(FontWeight::SEMIBOLD), ..HighlightStyle::default() };
        out.push(div().max_w(px(680.)).text_size(px(15.)).line_height(px(22.)).text_color(theme::text_soft()).child(StyledText::new(sentence).with_highlights([(0..date.len(), bold)])).into_any_element());
        if let Some(play) = m.scene.as_ref() {
            out.push(
                div()
                    .flex()
                    .child(link("manager-to-scene", format!("Back to the picture: {} ›", if play.scene.title.trim().is_empty() { "what he drew" } else { play.scene.title.trim() })).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.manager_set_stage(Stage::Scene, cx))))
                    .into_any_element(),
            );
        }

        // Needs you.
        let need_rows: Vec<AnyElement> = if needs.is_empty() {
            vec![quiet_row("Nothing needs you right now.")]
        } else {
            needs
                .iter()
                .enumerate()
                .map(|(i, need)| {
                    let go = need.go.clone();
                    row(("manager-need", i))
                        .cursor_pointer()
                        .when(need.urgent, |d| d.bg(theme::error_bg()))
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.manager_go(&go, window, cx)))
                        .child(ui::dot(if need.urgent { theme::error_text() } else { theme::attention() }))
                        .child(row_text(need.title.clone(), need.detail.clone()))
                        .child(chevron())
                        .into_any_element()
                })
                .collect()
        };
        out.push(tile(tile_head("manager-tile-needs", "Needs you", needs.len().to_string()), need_rows).into_any_element());

        // Sessions.
        let session_rows: Vec<AnyElement> = if snap.sessions.is_empty() {
            vec![quiet_row("No sessions in the projects he manages.")]
        } else {
            snap.sessions
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let show = to_session(&s.id);
                    row(("manager-session", i))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.manager_show(&show, window, cx)))
                        .child(div().flex_none().size(px(8.)).rounded_full().bg(session_dot(s.state)))
                        .child(row_text(if s.title.trim().is_empty() { s.branch.clone() } else { s.title.clone() }, session_line(s)))
                        .when(s.on_server, |d| {
                            d.child(div().flex_none().px_1().rounded(px(4.)).border_1().border_color(theme::border()).font_family(theme::MONO_FONT).text_size(px(10.5)).text_color(theme::muted()).child("srv"))
                        })
                        .child(chevron())
                        .into_any_element()
                })
                .collect()
        };
        let sessions = tile(tile_head("manager-tile-sessions", "Sessions", snap.sessions.len().to_string()), session_rows);

        // Follow-ups.
        let to_followups = cx.listener(|this: &mut Workspace, _: &ClickEvent, _, cx| this.manager_set_stage(Stage::FollowUps, cx));
        let follow_rows: Vec<AnyElement> = if open.is_empty() {
            vec![quiet_row("Nothing is open with anyone.")]
        } else {
            open.iter()
                .take(5)
                .enumerate()
                .map(|(i, f)| {
                    row(("manager-follow", i))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.manager_set_stage(Stage::FollowUps, cx)))
                        .child(row_text(follow_title(f), follow_sub(f, time)))
                        .when(!f.blockers.is_empty(), |d| d.child(blocker_bars(f).flex_none().w(px(70.))))
                        .when(f.state == FollowState::Fixed, |d| d.child(chip("Look again", Tone::Gold)))
                        .into_any_element()
                })
                .collect()
        };
        let follow = tile(tile_head("manager-tile-follow", "Follow-ups", "›").cursor_pointer().hover(|s| s.text_color(theme::text_strong())).on_click(to_followups), follow_rows);

        // What it does next, by itself.
        let mut next_rows: Vec<AnyElement> = Vec::new();
        let plain = |title: String, when: String| {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .px_3()
                .py(px(9.))
                .border_t_1()
                .border_color(theme::border())
                .child(div().flex_1().min_w_0().truncate().text_color(theme::text()).child(title))
                .when(!when.is_empty(), |d| d.child(div().flex_none().text_size(px(12.)).text_color(theme::muted()).child(when)))
                .into_any_element()
        };
        if let Some(brief) = data.brief.as_ref() {
            for line in brief.lines.iter().filter(|l| l.kind == "next") {
                next_rows.push(plain(line.text.clone(), String::new()));
            }
        }
        for check in &data.checks {
            next_rows.push(plain(check.what.clone(), every(check.every_secs, check.once)));
        }
        if let Some(at) = self.state.manager.brief_at.as_ref() {
            next_rows.push(plain("Your daily brief".into(), at.clone()));
        }
        if next_rows.is_empty() {
            next_rows.push(quiet_row("Nothing planned. Ask him to check on something."));
        }
        let next = tile(tile_head("manager-tile-next", "He does next, by himself", ""), next_rows);

        // Messages.
        let to_messages = cx.listener(|this: &mut Workspace, _: &ClickEvent, _, cx| this.manager_set_stage(Stage::Messages, cx));
        let mut outbox: Vec<&OutMessage> = data.outbox.iter().collect();
        outbox.sort_by_key(|m| (m.state != MsgState::Draft, std::cmp::Reverse(m.updated.max(m.created))));
        let message_rows: Vec<AnyElement> = if outbox.is_empty() {
            vec![quiet_row("No message waits for you.")]
        } else {
            outbox
                .iter()
                .take(4)
                .enumerate()
                .map(|(i, msg)| {
                    let base = row(("manager-msg", i))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.manager_set_stage(Stage::Messages, cx)))
                        .child(row_text(format!("To {}", msg.to), first_line(&msg.body)));
                    if msg.state == MsgState::Draft {
                        base.child(self.manager_decide_buttons(msg, ("manager-msg-allow", i), ("manager-msg-reject", i), cx)).into_any_element()
                    } else {
                        let (label, tone) = message_state(msg);
                        base.child(chip(label, tone)).into_any_element()
                    }
                })
                .collect()
        };
        let waiting = outbox.iter().filter(|m| m.state == MsgState::Draft).count();
        let messages = tile(
            tile_head("manager-tile-messages", "Messages", if waiting > 0 { format!("{waiting} wait for you  ›") } else { "›".to_string() }).cursor_pointer().hover(|s| s.text_color(theme::text_strong())).on_click(to_messages),
            message_rows,
        );

        // Memory.
        let to_memory = cx.listener(|this: &mut Workspace, _: &ClickEvent, _, cx| this.manager_set_stage(Stage::Memory, cx));
        let notes = memory::all(data);
        // With v2, memory's own count, and what waits for the person first.
        let (count, sub, first): (String, String, Vec<String>) = match (self.control.engine_v2, &self.memory_page.view) {
            (true, Some(view)) => (
                plural(view.entries(), "entry", "entries"),
                if view.waiting() > 0 { format!("{} waiting for you", view.waiting()) } else { "Kept on this Mac, synced to the server".to_string() },
                view.proposals.iter().map(|p| p.text.clone()).chain(view.files.iter().flat_map(|f| f.rows.iter().map(|r| r.text.clone()))).take(3).collect(),
            ),
            (true, None) => ("Memory".into(), "Open to read it".into(), Vec::new()),
            _ => (plural(notes.len(), "note", "notes"), "Kept on the server and on this Mac".into(), notes.iter().take(3).map(|n| n.text.clone()).collect()),
        };
        let mut memory_rows: Vec<AnyElement> = vec![
            row("manager-memory-count")
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.manager_set_stage(Stage::Memory, cx)))
                .child(row_text(count, sub))
                .child(chevron())
                .into_any_element(),
        ];
        for text in first {
            memory_rows.push(div().px_3().py(px(7.)).border_t_1().border_color(theme::border()).child(div().truncate().text_size(px(12.5)).text_color(theme::text_soft()).child(first_line(&text))).into_any_element());
        }
        let memory_tile = tile(tile_head("manager-tile-memory", "Memory", "›").cursor_pointer().hover(|s| s.text_color(theme::text_strong())).on_click(to_memory), memory_rows);

        let left = div().flex().flex_col().gap(px(14.)).min_w_0().child(sessions);
        let right = div().flex().flex_col().gap(px(14.)).min_w_0().child(follow).child(next).child(messages).child(memory_tile);
        out.push(if narrow {
            div().flex().flex_col().gap(px(14.)).child(left).child(right).into_any_element()
        } else {
            div().flex().items_start().gap(px(14.)).child(left.flex_1()).child(right.flex_1()).into_any_element()
        });
        out
    }

    /// Allow and Reject for a message that waits. A message it must never send has no Allow.
    fn manager_decide_buttons(&self, msg: &OutMessage, allow_id: impl Into<ElementId>, reject_id: impl Into<ElementId>, cx: &mut Context<Self>) -> Div {
        let allow = msg.id.clone();
        let reject = msg.id.clone();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1p5()
            .when(msg.tier != Tier::Never, |d| {
                d.child(small_button(allow_id, "Allow", true).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.manager_decide_message(&allow, true, None, cx);
                    cx.notify();
                })))
            })
            .child(small_button(reject_id, "Reject", false).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.manager_decide_message(&reject, false, None, cx);
                cx.notify();
            })))
    }

    fn manager_followups(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let data = &self.manager.harness.data;
        let time = now();
        let mut out = self.manager_stage_head("Follow-ups", cx);
        out.push(lead("Things that are open with someone else. He keeps them until they close, so nobody has to tell you by hand.").into_any_element());
        let open = followups::open(data);
        if open.is_empty() {
            out.push(stage_card().child(div().text_color(theme::muted()).child("Nothing is open with anyone. A review you leave with blockers, or one left on your pull request, shows up here.")).into_any_element());
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
                    .gap_2()
                    .py(px(7.))
                    .border_t_1()
                    .border_color(theme::border())
                    .child(div().flex_none().w(px(22.)).font_family(theme::MONO_FONT).text_size(px(12.)).text_color(theme::muted()).child((k + 1).to_string()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(text_lines(&b.text).text_color(if b.resolved { theme::muted() } else { theme::text() }))
                            .when(!b.path.trim().is_empty(), |d| d.child(div().truncate().font_family(theme::MONO_FONT).text_size(px(12.)).text_color(theme::muted()).child(b.path.clone()))),
                    )
                    .child(if b.resolved { chip("Done", Tone::Ok) } else { chip("Open", Tone::Plain) })
            });
            out.push(
                stage_card()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(div().min_w_0().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(follow_title(f)))
                            .child(div().text_size(px(12.5)).text_color(theme::muted()).child(small.join(" · "))),
                    )
                    .when(!f.title.trim().is_empty() && f.kind != FollowKind::Custom, |d| d.child(div().text_color(theme::text_soft()).child(f.title.clone())))
                    .when(total > 0, |d| d.child(blocker_bars(f)).child(div().flex().flex_col().children(items)))
                    .when(!f.note.trim().is_empty() && f.kind != FollowKind::Custom, |d| d.child(text_lines(&f.note).text_size(px(12.5)).text_color(theme::muted())))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_x_3()
                            .gap_y_2()
                            .child(small_button(("manager-follow-done", i), "Done", false).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.manager_follow_done(&id, cx))))
                            .when(f.state == FollowState::Fixed, |d| d.child(chip("Says it is fixed. Look again.", Tone::Gold)))
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

    fn manager_messages(&self, narrow: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let data = &self.manager.harness.data;
        let settings = &self.state.manager;
        let time = now();
        let mut out = self.manager_stage_head("Messages", cx);
        out.push(
            lead(match settings.send {
                SendPolicy::AskAlways => "Every message he writes waits here for your OK. He never approves or merges a pull request, and never decides for you.",
                SendPolicy::RoutineAuto => "Routine pings go out by themselves. Anything with a promise, a date or an opinion waits here for your OK. He never approves or merges a pull request, and never decides for you.",
            })
            .text_size(px(13.))
            .line_height(px(19.5))
            .into_any_element(),
        );
        // Without a messenger nothing can go out and nothing comes in: say where to set one.
        if settings.messenger.trim().is_empty() {
            out.push(div().text_size(px(12.5)).text_color(gold()).child("No messenger is set up, so no message can go out and none is read. Choose one in Settings › Manager.").into_any_element());
        }
        let section = |title: &'static str| div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::muted()).child(title);

        let mut outbox: Vec<&OutMessage> = data.outbox.iter().collect();
        outbox.sort_by_key(|m| (m.state != MsgState::Draft, std::cmp::Reverse(m.updated.max(m.created))));
        let mut sent = div().flex().flex_col().flex_1().min_w_0().gap(px(10.)).child(section("Written for you"));
        if outbox.is_empty() {
            sent = sent.child(div().text_size(px(12.5)).text_color(theme::muted()).child("Nothing yet. Ask him to tell a teammate something, and the draft lands here."));
        }
        for (i, msg) in outbox.iter().take(40).enumerate() {
            let (label, tone) = message_state(msg);
            let (tier, tier_tone) = match msg.tier {
                Tier::Routine => ("Routine", Tone::Plain),
                Tier::NeedsOk => ("Needs your OK", Tone::Warn),
                Tier::Never => ("Never sent", Tone::Err),
            };
            sent = sent.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .px_3()
                    .py(px(11.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(if msg.state == MsgState::Draft { gold_line() } else { theme::border() })
                    .bg(theme::raised())
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_x_2()
                            .gap_y_1p5()
                            .text_size(px(12.5))
                            .text_color(theme::muted())
                            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(format!("To {}", msg.to)))
                            .child(ago(msg.sent.unwrap_or(msg.updated.max(msg.created)), time))
                            .child(chip(label, tone))
                            .child(chip(tier, tier_tone))
                            .when(msg.urgent, |d| d.child(chip("Holding reply", Tone::Gold))),
                    )
                    .child(text_lines(&msg.body).text_color(theme::text()))
                    .when(!msg.why.trim().is_empty(), |d| d.child(div().text_size(px(12.5)).text_color(theme::muted()).child(format!("Why: {}", msg.why.trim()))))
                    .when_some(msg.about.clone().filter(|a| !a.trim().is_empty()), |d, about| d.child(div().text_size(px(12.5)).text_color(theme::muted()).child(format!("About: {about}"))))
                    .when_some(msg.error.clone().filter(|_| msg.state == MsgState::Failed), |d, error| d.child(div().text_size(px(12.5)).text_color(theme::error_text()).child(error)))
                    .when(msg.state == MsgState::Draft, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .child(self.manager_decide_buttons(msg, ("manager-out-allow", i), ("manager-out-reject", i), cx))
                                .when(msg.tier == Tier::Never, |d| d.child(div().text_size(px(12.5)).text_color(theme::muted()).child("He never sends a message like this. Send it yourself if you mean it."))),
                        )
                    }),
            );
        }

        let mut inbox: Vec<&InMessage> = data.inbox.iter().collect();
        inbox.sort_by_key(|m| std::cmp::Reverse(m.at));
        let mut came = div().flex().flex_col().flex_1().min_w_0().gap(px(10.)).child(section("What came in"));
        if inbox.is_empty() {
            came = came.child(div().text_size(px(12.5)).text_color(theme::muted()).child("Nothing came in."));
        }
        for msg in inbox.iter().take(40) {
            came = came.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .px_3()
                    .py(px(11.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(if msg.urgent && !msg.handled { theme::error_border() } else { theme::border() })
                    .bg(theme::raised())
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_x_2()
                            .gap_y_1p5()
                            .text_size(px(12.5))
                            .text_color(theme::muted())
                            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(msg.from.clone()))
                            .child(ago(msg.at, time))
                            .when(msg.urgent, |d| d.child(chip("Urgent", Tone::Err)))
                            .when(msg.handled, |d| d.child(chip("Dealt with", Tone::Plain))),
                    )
                    .child(text_lines(&msg.text).text_color(theme::text())),
            );
        }
        out.push(if narrow {
            div().flex().flex_col().gap(px(18.)).child(sent).child(came).into_any_element()
        } else {
            div().flex().items_start().gap(px(14.)).child(sent).child(came).into_any_element()
        });
        out
    }

    /// The Memory page with the v2 engine: what waits for the person, then what memory keeps.
    fn manager_memory_v2(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        use crate::memory_ui::MemoryChange;
        let mut out = self.manager_stage_head("Memory", cx);
        out.push(lead("What memory keeps for you and your sessions, in plain Markdown, file by file. Nothing anyone proposes becomes memory until you accept it here.").into_any_element());
        let page = &self.memory_page;
        if let Some(error) = &page.error {
            out.push(stage_card().child(div().text_color(theme::removed()).child(error.clone())).into_any_element());
        }
        let Some(view) = &page.view else {
            out.push(stage_card().child(div().text_color(theme::muted()).child("Reading memory…")).into_any_element());
            return out;
        };
        let card = || div().flex().flex_col().rounded(px(10.)).border_1().border_color(theme::border()).bg(theme::raised()).overflow_hidden();
        let line = |i: usize| div().flex().items_start().gap_3().px_3().py_2().when(i > 0, |d| d.border_t_1().border_color(theme::border()));
        let texts = |text: String, meta: String| div().flex().flex_col().flex_1().min_w_0().gap_0p5().child(text_lines(&text).text_size(px(13.5)).line_height(px(19.5)).text_color(theme::text())).child(div().text_size(px(12.)).text_color(theme::muted()).child(meta));
        // What waits for the person.
        if view.waiting() > 0 {
            let all: Vec<String> = view.proposals.iter().map(|p| p.id.clone()).collect();
            out.push(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(13.)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(format!("Waiting for you ({})", view.waiting())))
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
                rows.push(
                    line(i)
                        .child(texts(proposal.text.clone(), meta.join(" · ")))
                        .child(small_button(("memory-accept", i), "Accept", true).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.memory_change(MemoryChange::Accept(yes.clone()), cx))))
                        .child(small_button(("memory-reject", i), "Reject", false).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.memory_change(MemoryChange::Reject(no.clone()), cx))))
                        .into_any_element(),
                );
            }
            for (i, held) in view.held.iter().enumerate() {
                let (keep, drop) = (held.path.clone(), held.path.clone());
                rows.push(
                    line(view.proposals.len() + i)
                        .child(texts(format!("{} was {} by hand in the memory folder", held.path, held.change), "held back: sessions do not see it until you keep it".into()))
                        .child(small_button(("memory-keep", i), "Keep", true).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.memory_change(MemoryChange::KeepEdit(keep.clone()), cx))))
                        .child(small_button(("memory-discard", i), "Discard", false).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.memory_change(MemoryChange::DiscardEdit(drop.clone()), cx))))
                        .into_any_element(),
                );
            }
            out.push(card().children(rows).into_any_element());
        }
        // What memory keeps.
        if view.files.is_empty() {
            out.push(stage_card().child(div().text_color(theme::muted()).child("Memory keeps nothing yet. Move in (Settings › Manager), or tell the manager “remember …”.")).into_any_element());
            return out;
        }
        let mut n: usize = 0;
        for file in &view.files {
            let let_go = if file.let_go > 0 { format!(" · {} let go", file.let_go) } else { String::new() };
            out.push(div().pt_2().text_size(px(12.5)).text_color(theme::muted()).child(format!("{}{let_go}", file.label)).into_any_element());
            let rows: Vec<AnyElement> = file
                .rows
                .iter()
                .enumerate()
                .map(|(i, row)| {
                    n += 1;
                    let id = row.id.clone();
                    line(i)
                        .child(texts(row.text.clone(), format!("{} · {}", row.status, row.from)))
                        .child(small_button(("memory-forget", n), "Forget", false).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.memory_change(MemoryChange::Forget(id.clone()), cx))))
                        .into_any_element()
                })
                .collect();
            if !rows.is_empty() {
                out.push(card().children(rows).into_any_element());
            }
        }
        out
    }

    fn manager_memory(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if self.control.engine_v2 {
            return self.manager_memory_v2(cx);
        }
        let data = &self.manager.harness.data;
        let time = now();
        let mut out = self.manager_stage_head("Memory", cx);
        out.push(lead("What he knows and keeps: plain notes you can read and correct, kept on the server and on this Mac. Tell him “remember …” to add one.").into_any_element());
        let notes = memory::all(data);
        if notes.is_empty() {
            out.push(stage_card().child(div().text_color(theme::muted()).child("Nothing yet.")).into_any_element());
            return out;
        }
        out.push(
            div()
                .flex()
                .flex_col()
                .rounded(px(10.))
                .border_1()
                .border_color(theme::border())
                .bg(theme::raised())
                .overflow_hidden()
                .children(notes.iter().enumerate().map(|(i, note)| {
                    let id = note.id.clone();
                    let from = match note.source.as_str() {
                        "you" => "from you".to_string(),
                        "manager" => "noted by himself".to_string(),
                        source if source.starts_with("session:") => "from a session".to_string(),
                        _ => String::new(),
                    };
                    let mut meta: Vec<String> = note.tags.iter().filter(|t| !t.trim().is_empty()).cloned().collect();
                    if !from.is_empty() {
                        meta.push(from);
                    }
                    meta.push(ago(note.updated.max(note.created), time));
                    div()
                        .flex()
                        .items_start()
                        .gap_3()
                        .px_3()
                        .py_2()
                        .when(i > 0, |d| d.border_t_1().border_color(theme::border()))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .gap_0p5()
                                .child(text_lines(&note.text).text_size(px(13.5)).line_height(px(19.5)).text_color(theme::text()))
                                .child(div().text_size(px(12.)).text_color(theme::muted()).child(meta.join(" · "))),
                        )
                        .child(small_button(("manager-forget", i), "Forget", false).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.manager_forget_note(&id, cx))))
                }))
                .into_any_element(),
        );
        out
    }

    fn manager_scene(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(play) = self.manager.scene.as_ref() else { return Vec::new() };
        let steps = play.scene.steps.len();
        let at = play.step.min(steps.saturating_sub(1));
        let mut out = self.manager_stage_head(if play.scene.title.trim().is_empty() { "What he drew".to_string() } else { play.scene.title.trim().to_string() }, cx);
        out.push(crate::scenes_ui::render_scene(&play.scene, at));
        if steps > 1 {
            out.push(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x_3()
                    .gap_y_2()
                    .child(div().flex().flex_wrap().items_center().gap(px(6.)).children((0..steps).map(|k| step_dot(("manager-scene-dot", k), k <= at).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.manager_scene_go(k, cx))))))
                    .child(div().text_size(px(12.)).text_color(theme::muted()).child("He draws this while he talks, one step per sentence. Click a step to jump."))
                    .into_any_element(),
            );
        }
        out
    }
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

    #[test]
    fn what_needs_the_person_is_listed_most_pressing_first() {
        let data = Data {
            inbox: vec![
                InMessage { id: "i1".into(), from: "Sam".into(), text: "The release is stuck".into(), urgent: true, ..InMessage::default() },
                InMessage { id: "i2".into(), from: "Alex".into(), text: "Lunch?".into(), ..InMessage::default() },
                InMessage { id: "i3".into(), from: "Mira".into(), text: "Seen already".into(), urgent: true, handled: true, ..InMessage::default() },
            ],
            outbox: vec![
                OutMessage { id: "o1".into(), to: "Alex".into(), body: "The fix is pushed.\n\nMira's manager".into(), state: MsgState::Draft, ..OutMessage::default() },
                OutMessage { id: "o2".into(), to: "Sam".into(), body: "Sent before".into(), state: MsgState::Sent, ..OutMessage::default() },
            ],
            followups: vec![
                FollowUp { id: "f1".into(), state: FollowState::Fixed, number: 41, person: "Alex".into(), title: "Retry with backoff".into(), ..FollowUp::default() },
                FollowUp { id: "f2".into(), state: FollowState::Open, number: 42, ..FollowUp::default() },
            ],
            ..Data::default()
        };
        let snap = Snapshot {
            sessions: vec![
                SessionInfo { id: "s1".into(), title: "Backoff".into(), state: SessionState::NeedsYou, last_reply: Some("Done with the tests.\nShall I push?".into()), ..SessionInfo::default() },
                SessionInfo { id: "s2".into(), title: "Docs".into(), state: SessionState::Working, ..SessionInfo::default() },
            ],
            review_asks: vec![ReviewAsk { number: 7, title: "Quiz order".into(), author: "Sam".into(), ..ReviewAsk::default() }],
            approvals: 2,
            ..Snapshot::default()
        };
        let list = needs(&data, &snap);
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

    #[test]
    fn nothing_waits_when_all_is_quiet() {
        assert!(needs(&Data::default(), &Snapshot::default()).is_empty());
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
    fn a_session_reports_its_state_and_its_last_line() {
        let s = SessionInfo { state: SessionState::Working, last_reply: Some("Reading the code.\nRunning the tests now".into()), ..SessionInfo::default() };
        assert_eq!(session_line(&s), "Working · Running the tests now");
        assert_eq!(session_line(&SessionInfo::default()), "Not running");
    }
}
