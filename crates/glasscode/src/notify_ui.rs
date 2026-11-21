//! Telling the person a session is done or needs them, the way Zed does: a small window at
//! the top right of the screen (the bottom right on Windows; it takes no focus and stays above
//! other apps), with Open and Dismiss, and a short sound. Only when they are not looking at
//! that session already. It goes by itself after a while, or when the session is opened.
//!
//! The card is one calm surface (the owner, 2026-11-21): the agent's mark, what happened with
//! its state as a small dot, the session's title on up to two lines, and the actions as a row of
//! quiet words under it, with Dismiss as a cross in the corner. A request keeps Allow or Review
//! and Deny, and has no cross: it is answered, not put away.
//!
//! Debug builds: `GLASSCODE_DEBUG_NOTIFY=done|needs|approval|review|manager|ticket|all` shows
//! made-up ones at launch, past the delivery rules and with no sound, to look at the card.
//!
//! In a Manager v2 home every notice goes through `glasscode_core::delivery` first (M42): by
//! matter, the same words are never said twice, new evidence on a matter just told updates its
//! notification quietly, a burst or an hour's cap turns into Inbox lines, quiet hours hold all
//! but urgent ones for one digest, and opening or dismissing one acknowledges it. What is not a
//! popup is a line in the journal (Today's Activity).

use std::time::{Duration, Instant};

use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, ClickEvent, Context, DisplayId, Entity, FontWeight, IntoElement, Pixels, Render, SharedString,
    WeakEntity, Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, point, prelude::*, px, size,
};
use glasscode_core::model::AgentKind;
use glasscode_core::sys::Sound;

use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

/// The card, at 100 %.
const CARD_WIDTH: f32 = 368.;
const CARD_HEIGHT: f32 = 116.;
/// From the window's edge to the card. On the Mac the card draws its own edge and shadow, and
/// the window is larger by the shadow's room. Windows 11 draws an edge, round corners and a
/// shadow around each window itself, so there the card fills its window: a card inside that
/// frame would be a box in a box.
const INSET: f32 = if cfg!(windows) { 0. } else { 8. };
/// The window, at 100 %.
const WIDTH: f32 = CARD_WIDTH + 2. * INSET;
const HEIGHT: f32 = CARD_HEIGHT + 2. * INSET;
/// From the screen's edge (or the work area's) to the window: the card is 20 from the edge.
const MARGIN: f32 = 20. - INSET;
/// Between two windows: 12 between two cards at 100 % on Windows, 16 on the Mac (two windows
/// do not overlap).
const GAP: f32 = if cfg!(windows) { 12. } else { 0. };
const SHOWN_FOR: Duration = Duration::from_secs(45);
const MAX_SHOWN: usize = 4;

thread_local! {
    /// v2: the matters told, on the main thread (M42), as the last run left them
    /// (`manager/delivery.json`: a restart forgets no snooze and nothing the quiet hours hold).
    static DELIVERY: std::cell::RefCell<glasscode_core::delivery::Delivery> = std::cell::RefCell::new(load_delivery());
    /// It changed since it was last written.
    static DELIVERY_DIRTY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn load_delivery() -> glasscode_core::delivery::Delivery {
    use glasscode_core::store::Store as _;
    // Only a v2 home keeps one; elsewhere the file is not there and this is empty.
    glasscode_core::delivery::DeliveryFile::load().map(|file| file.delivery).unwrap_or_default()
}

/// A change to what was told: kept at the next tick.
fn delivery_mut<R>(change: impl FnOnce(&mut glasscode_core::delivery::Delivery) -> R) -> R {
    DELIVERY_DIRTY.with(|dirty| dirty.set(true));
    DELIVERY.with(|d| change(&mut d.borrow_mut()))
}

/// Write what was told, when it changed (off the main thread).
fn keep_delivery() {
    if !DELIVERY_DIRTY.with(|dirty| dirty.replace(false)) {
        return;
    }
    let kept = DELIVERY.with(|d| d.borrow().kept(glasscode_core::clock::now_ms()));
    std::thread::spawn(move || {
        use glasscode_core::store::Store as _;
        if let Err(err) = (glasscode_core::delivery::DeliveryFile { delivery: kept }).save() {
            log::warn!("notifications: what was told could not be kept ({err})");
        }
    });
}

/// The matter of a notification: its popup's id and what kind of notice it is, so a session's
/// "needs you" and its "finished" are two matters, and each manager notice its own.
fn matter_of(id: &str, kind: &str) -> String {
    format!("{id}#{kind}")
}

/// v2: the person opened or dismissed a popup: every matter it stood for is acknowledged.
pub(crate) fn acknowledge(id: &str) {
    let prefix = format!("{id}#");
    delivery_mut(|d| d.seen_all(&prefix));
}

/// v2: the person snoozed a popup for an hour: what it stands for comes as Inbox lines until then.
pub(crate) fn snooze(id: &str) {
    let prefix = format!("{id}#");
    let until = glasscode_core::clock::now_ms() + 3_600_000;
    delivery_mut(|d| d.snooze_all(&prefix, until));
}

/// v2: a session went on working: what it said before is over (its next stop is new).
pub(crate) fn session_moved_on(id: &str) {
    let prefix = format!("{id}#");
    delivery_mut(|d| d.close_all(&prefix));
}

/// v2: what is still waiting on the person, oldest first, with its age: for the brief.
pub(crate) fn still_waiting(now_ms: u64) -> Vec<(String, u64)> {
    DELIVERY.with(|d| d.borrow().waiting(now_ms).into_iter().map(|(m, age)| (m.title.clone(), age)).collect())
}

pub struct Popup {
    session_id: String,
    /// A ticket or a review request (Open goes to Tickets); else a session.
    ticket: Option<crate::tickets_ui::Key>,
    /// From the manager about no session (Open goes to its page): its initials, for the icon.
    manager: Option<SharedString>,
    agent: AgentKind,
    title: SharedString,
    body: SharedString,
    needs_you: bool,
    /// An agent's request waiting for the person: Allow and Deny instead of Open.
    approval: Option<u64>,
    /// The request leaves the Mac or removes something: Review (on its card, with the full
    /// payload) instead of Allow (M21.2).
    review: bool,
    workspace: WeakEntity<Workspace>,
    main: Option<AnyWindowHandle>,
    /// When it goes by itself.
    until: Instant,
    /// v2 (M42): Snooze beside Dismiss.
    snooze: bool,
    /// Not shown yet: it fades in. One that only moves up a place (Windows opens it again in a
    /// new window) is there at once.
    fresh: bool,
}

impl Popup {
    fn open_session(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let (workspace, id, ticket, to_manager) = (self.workspace.clone(), self.session_id.clone(), self.ticket.clone(), self.manager.is_some());
        acknowledge(&id);
        cx.activate(true);
        if let Some(main) = self.main {
            let _ = main.update(cx, |_, main_window, cx| {
                main_window.activate_window();
                let _ = workspace.update(cx, |ws, cx| match ticket {
                    _ if to_manager => ws.show_manager(main_window, cx),
                    Some(key) => ws.open_ticket(key, cx),
                    None => ws.open_session(&id, main_window, cx),
                });
            });
        }
        window.remove_window();
    }

    /// Review a request on its card in Today, where its full payload is.
    fn review(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        cx.activate(true);
        if let Some(main) = self.main {
            let _ = main.update(cx, |_, main_window, cx| {
                main_window.activate_window();
                let _ = workspace.update(cx, |ws, cx| ws.open_today(main_window, cx));
            });
        }
        window.remove_window();
    }

    fn decide(&mut self, allow: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (workspace, id) = (self.workspace.clone(), self.approval);
        if let (Some(main), Some(id)) = (self.main, id) {
            let _ = main.update(cx, |_, main_window, cx| {
                let _ = workspace.update(cx, |ws, cx| ws.control_decide(id, allow, false, main_window, cx));
            });
        }
        window.remove_window();
    }
}

/// The action the notification is for ("Open", "Allow", "Review"): the strongest word of the
/// row, on a faint tint, not a filled block.
fn main_action(id: &'static str, label: &'static str) -> gpui::Stateful<gpui::Div> {
    action_base(id, label).bg(theme::selected()).text_color(theme::text_strong()).font_weight(FontWeight::SEMIBOLD).hover(|s| s.bg(theme::border_strong()))
}

/// Another action of the row ("Snooze 1 h", "Deny"): a quiet word.
fn action(id: &'static str, label: &'static str) -> gpui::Stateful<gpui::Div> {
    action_base(id, label).text_color(theme::text_soft()).font_weight(FontWeight::MEDIUM).hover(|s| s.bg(theme::hover()).text_color(theme::text_strong()))
}

fn action_base(id: &'static str, label: &'static str) -> gpui::Stateful<gpui::Div> {
    div().id(id).flex().flex_none().items_center().h(theme::sz(26.)).px(theme::sz(10.)).rounded(theme::sz(7.)).text_size(theme::sz(12.5)).whitespace_nowrap().cursor_pointer().child(label)
}

impl Render for Popup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A window of its own: its rem follows the scale as the main window's does, so the short
        // forms grow with the `sz` sizes and the window (`popup_bounds`).
        window.set_rem_size(theme::rem_size());
        let state = if self.needs_you { crate::sidebar::attention() } else { theme::added() };
        let mark = if let Some(initials) = self.manager.clone() {
            div().size(theme::sz(28.)).flex_none().rounded_full().flex().items_center().justify_center().bg(crate::scenes_ui::gold_bg()).text_color(crate::scenes_ui::gold()).text_size(theme::sz(11.)).font_weight(FontWeight::SEMIBOLD).child(initials)
        } else if self.ticket.is_some() {
            div().size(theme::sz(28.)).flex_none().rounded(theme::sz(8.)).flex().items_center().justify_center().bg(theme::selected()).child(crate::icons::icon(crate::icons::TICKETS, theme::sz(15.), theme::sz(15.), theme::text_soft()))
        } else {
            crate::sidebar::agent_icon(self.agent, 28., true)
        };
        let actions = div()
            .flex()
            .items_center()
            .gap(theme::sz(4.))
            // The first action's words stand in line with the text over it.
            .ml(theme::sz(-10.))
            .when(self.approval.is_none(), |d| {
                let d = d.child(main_action("popup-open", "Open").ml(theme::sz(10.)).on_click(cx.listener(Self::open_session)));
                if !self.snooze {
                    return d;
                }
                // v2: what it stands for waits an hour as Inbox lines (M42).
                d.child(action("popup-snooze", "Snooze 1 h").on_click(cx.listener(|this, _: &ClickEvent, window, _| {
                    snooze(&this.session_id);
                    window.remove_window();
                })))
            })
            .when(self.approval.is_some() && !self.review, |d| {
                d.child(main_action("popup-allow", "Allow").ml(theme::sz(10.)).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.decide(true, window, cx))))
                    .child(action("popup-deny", "Deny").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.decide(false, window, cx))))
            })
            .when(self.approval.is_some() && self.review, |d| {
                d.child(main_action("popup-review", "Review").ml(theme::sz(10.)).on_click(cx.listener(Self::review)))
                    .child(action("popup-deny", "Deny").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.decide(false, window, cx))))
            });
        let card = div()
            .size_full()
            .flex()
            .items_start()
            .gap(theme::sz(12.))
            .pl(theme::sz(14.))
            .pr(theme::sz(10.))
            .py(theme::sz(12.))
            .bg(theme::dialog())
            // The system frames the window on Windows (see `INSET`).
            .when(!cfg!(windows), |d| d.rounded(theme::sz(14.)).border_1().border_color(theme::border_strong()).shadow_md())
            .child(div().pt(theme::sz(1.)).child(mark))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(theme::sz(7.))
                            .h(theme::sz(20.))
                            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(theme::sz(13.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(self.title.clone()))
                            .child(ui::dot(state)),
                    )
                    .child(div().mt(theme::sz(2.)).text_size(theme::sz(12.5)).line_height(theme::sz(17.)).text_color(theme::muted()).overflow_hidden().text_ellipsis().line_clamp(2).child(self.body.clone()))
                    .child(div().flex_1())
                    .child(actions),
            )
            .when(self.approval.is_none(), |d| {
                d.child(
                    div()
                        .id("popup-dismiss")
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(theme::sz(22.))
                        .mt(theme::sz(-2.))
                        .rounded(theme::sz(6.))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .child(crate::icons::icon(crate::icons::CLOSE, theme::sz(11.), theme::sz(11.), theme::muted()))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, _| {
                            acknowledge(&this.session_id);
                            window.remove_window();
                        })),
                )
            });
        // It comes in once, when it first shows (`motion`: short and once).
        let card = if self.fresh { crate::motion::fade_in("popup", card).into_any_element() } else { card.into_any_element() };
        div().size_full().p(theme::sz(INSET)).font_family(theme::UI_FONT).child(card)
    }
}

impl Workspace {
    /// Show a notification for a session (and play the sound, unless switched off).
    pub fn notify_session(&mut self, session_id: &str, agent: AgentKind, title: String, body: String, needs_you: bool, cx: &mut Context<Self>) {
        self.notify(session_id, None, agent, title, body, needs_you, None, None, false, cx);
    }

    /// The manager has something for the person that is about no session: Open goes to its page.
    pub fn notify_manager(&mut self, title: String, body: String, cx: &mut Context<Self>) {
        let (initials, agent) = (self.state.manager.initials(), self.state.manager.brain);
        self.notify("manager", None, agent, title, body, true, None, Some(initials.into()), false, cx);
    }

    /// An urgent message from a teammate the person allows to alarm them (v1's inbox rule): in a
    /// v2 home it passes quiet hours and the caps (M42).
    pub fn notify_manager_urgent(&mut self, title: String, body: String, cx: &mut Context<Self>) {
        let (initials, agent) = (self.state.manager.initials(), self.state.manager.brain);
        self.notify("manager", None, agent, title, body, true, None, Some(initials.into()), true, cx);
    }

    /// An agent asks to do something: Allow and Deny right in the notification; for what leaves the
    /// Mac or removes something (`review`), Review, which opens its card.
    pub fn notify_approval(&mut self, id: u64, agent: AgentKind, title: String, body: String, review: bool, cx: &mut Context<Self>) {
        self.notify(&format!("approval:{id}"), None, agent, title, body, true, Some((id, review)), None, false, cx);
    }

    /// A ticket assigned to the person, or a pull request that asks for their review.
    pub fn notify_ticket(&mut self, key: crate::tickets_ui::Key, title: String, body: String, cx: &mut Context<Self>) {
        let id = match &key {
            crate::tickets_ui::Key::Ticket(id) => format!("ticket:{id}"),
            crate::tickets_ui::Key::Pr(repo, n) => format!("pr:{repo}#{n}"),
        };
        self.notify(&id, Some(key), AgentKind::Claude, title, body, true, None, None, false, cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(&mut self, session_id: &str, ticket: Option<crate::tickets_ui::Key>, agent: AgentKind, title: String, body: String, needs_you: bool, approval: Option<(u64, bool)>, manager: Option<SharedString>, urgent: bool, cx: &mut Context<Self>) {
        // A test copy (debug, in the background) never puts windows on the person's screen.
        if cfg!(debug_assertions) && std::env::var_os("GLASSCODE_DEBUG_BACKGROUND").is_some() {
            log::warn!("notification (not shown in a test copy): {title} · {body}");
            return;
        }
        // v2 (M42): the matter decides whether it is a popup at all, and with a sound.
        let sound;
        {
            use glasscode_core::delivery::{Decision, Notice, Presence, Rules, Severity};
            let severity = if urgent {
                Severity::Urgent
            } else if approval.is_some() || needs_you {
                Severity::Warn
            } else {
                Severity::Notice
            };
            let watching = self.window_active && self.center == crate::center::Center::Session && self.state.active_session.as_deref() == Some(session_id);
            self.popups.retain(|(_, handle)| handle.update(cx, |_, _, _| ()).is_ok());
            let shown = self.popups.iter().any(|(id, _)| id == session_id);
            let presence = Presence { window_active: self.window_active, watching, shown };
            let rules = Rules { quiet: self.state.notify.quiet, silent: self.state.notify.sound_off };
            let now = glasscode_core::clock::now_ms();
            let minute = glasscode_core::clock::local_minutes(now).map_or(12 * 60, glasscode_core::clock::minute_of_day);
            let kind = if approval.is_some() {
                "approval".to_string()
            } else if manager.is_some() {
                // Each manager notice by its subject (its title).
                format!("manager:{title}")
            } else if needs_you {
                "needs".to_string()
            } else {
                "done".to_string()
            };
            let notice = Notice { matter: matter_of(session_id, &kind), severity, title: title.clone(), body: body.clone() };
            match delivery_mut(|d| d.offer(notice, presence, &rules, now, minute)) {
                Decision::Popup { sound: with } => sound = with,
                Decision::Update { .. } => sound = false,
                other => {
                    let outcome = match other {
                        Decision::Held => "held for the end of the quiet hours",
                        Decision::Nothing => "said before",
                        _ => "an Inbox line",
                    };
                    let session = self.state.session(session_id).map(|s| s.id.clone());
                    glasscode_core::journal::append(glasscode_core::journal::decision("notify", outcome, &format!("{title}: {body}"), serde_json::json!({ "matter": session_id, "decision": format!("{other:?}") }), None).session(session.as_deref()));
                    return;
                }
            }
        }
        let popup = Popup {
            session_id: session_id.to_string(),
            ticket,
            manager,
            agent,
            title: title.into(),
            body: body.into(),
            needs_you,
            approval: approval.map(|(id, _)| id),
            review: approval.is_some_and(|(_, review)| review),
            workspace: cx.weak_entity(),
            main: self.main_window,
            until: Instant::now() + SHOWN_FOR,
            snooze: true,
            fresh: true,
        };
        self.show_popup(popup, cx);
        if sound {
            glasscode_core::sys::play_sound(if needs_you { Sound::NeedsYou } else { Sound::Done });
        }
    }

    /// Put a notification on the screen, in the first place; the ones shown move on a place.
    fn show_popup(&mut self, popup: Popup, cx: &mut Context<Self>) {
        let session_id = popup.session_id.clone();
        let session_id = session_id.as_str();
        // Forget the ones already gone; keep at most a few on screen.
        self.popups.retain(|(_, handle)| handle.update(cx, |_, _, _| ()).is_ok());
        while self.popups.len() >= MAX_SHOWN {
            let (_, oldest) = self.popups.remove(0);
            let _ = oldest.update(cx, |_, window, _| window.remove_window());
        }
        // A newer notice for the same session replaces the older one.
        let same: Vec<AnyWindowHandle> = self.popups.iter().filter(|(id, _)| id == session_id).map(|(_, h)| *h).collect();
        for handle in same {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        self.popups.retain(|(id, _)| id != session_id);

        let Some(display) = cx.primary_display() else { return };
        // Windows leaves out the taskbar (the work area); the Mac counts from the top of the screen.
        let area = if cfg!(windows) { display.visible_bounds() } else { display.bounds() };
        let view = cx.new(|_| popup);
        let place = if cfg!(windows) {
            // The newest goes in the corner and the others move up a place, as Windows' own
            // notifications do. A window cannot move, so each one opens again a place higher,
            // with the time it has left.
            let shown = std::mem::take(&mut self.popups);
            let count = shown.len();
            for (i, (id, handle)) in shown.into_iter().enumerate() {
                let Some(older) = handle.downcast::<Popup>().and_then(|popup| popup.entity(cx).ok()) else {
                    self.popups.push((id, handle));
                    continue;
                };
                let _ = handle.update(cx, |_, window, _| window.remove_window());
                older.update(cx, |older, _| older.fresh = false);
                if let Some(handle) = open_popup(older, popup_bounds(area, count - i, theme::scale()), display.id(), cx) {
                    self.popups.push((id, handle));
                }
            }
            0
        } else {
            self.popups.len()
        };
        if let Some(handle) = open_popup(view, popup_bounds(area, place, theme::scale()), display.id(), cx) {
            self.popups.push((session_id.to_string(), handle));
        }
    }

    /// Debug builds: `GLASSCODE_DEBUG_NOTIFY` (see the top of this file), from a few seconds
    /// after launch, one at each tick as sessions tell things one at a time. Made-up words;
    /// nothing is told to the delivery rules and no sound plays.
    #[cfg(debug_assertions)]
    fn debug_sample_popups(&mut self, cx: &mut Context<Self>) {
        static WANTED: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let Some(wanted) = WANTED.get_or_init(|| std::env::var("GLASSCODE_DEBUG_NOTIFY").ok()) else { return };
        let long = "Add a local prod replica on kind, with the seed data and a script that tears it down again";
        let samples: [(&str, AgentKind, &str, &str, bool, Option<(u64, bool)>, bool, bool); 6] = [
            ("done", AgentKind::Claude, "Claude finished", long, false, None, false, false),
            ("needs", AgentKind::Codex, "Codex needs you", "Rename the export of acme/app", true, None, false, false),
            ("approval", AgentKind::Claude, "Claude asks to run a command", "C:\\Users\\sam\\code\\acme\\app\\target\\debug\\deps\\app_integration_tests-0123456789abcdef0123456789abcdef.exe --all-features", true, Some((1, false)), false, false),
            ("review", AgentKind::Claude, "Claude asks to push", "git push origin sam/replica: it leaves this computer, so look at it first. The branch has 14 commits that touch the seed data, the teardown script and the workflow that builds the replica on each pull request, and two of them rewrite history", true, Some((2, true)), false, false),
            ("manager", AgentKind::Claude, "Two sessions wait for you", "Sam Example asked about the replica; the lint fix is ready to merge", true, None, true, false),
            ("ticket", AgentKind::Claude, "#42 asks for your review", "Show each check in a tab of its own", true, None, false, true),
        ];
        let mut wanted = samples.into_iter().filter(|sample| wanted == "all" || wanted.split(',').any(|w| w == sample.0));
        let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some((name, agent, title, body, needs_you, approval, manager, ticket)) = wanted.nth(next) {
            let popup = Popup {
                session_id: format!("debug-{name}"),
                ticket: ticket.then(|| crate::tickets_ui::Key::Pr("acme/app".into(), 42)),
                manager: manager.then(|| SharedString::from("SE")),
                agent,
                title: title.into(),
                body: body.into(),
                needs_you,
                approval: approval.map(|(id, _)| id),
                review: approval.is_some_and(|(_, review)| review),
                workspace: cx.weak_entity(),
                main: self.main_window,
                until: Instant::now() + SHOWN_FOR,
                snooze: true,
                fresh: true,
            };
            self.show_popup(popup, cx);
        }
    }

    /// v2 (M42): when the quiet hours end, what they held as one notification.
    pub fn notify_digest_tick(&mut self, cx: &mut Context<Self>) {
        #[cfg(debug_assertions)]
        self.debug_sample_popups(cx);
        // Also with the quiet hours switched off: what they held still comes.
        let rules = glasscode_core::delivery::Rules { quiet: self.state.notify.quiet, silent: self.state.notify.sound_off };
        let now = glasscode_core::clock::now_ms();
        let minute = glasscode_core::clock::local_minutes(now).map_or(12 * 60, glasscode_core::clock::minute_of_day);
        keep_delivery();
        if let Some(digest) = delivery_mut(|d| d.digest(&rules, now, minute)) {
            let (initials, agent) = (self.state.manager.initials(), self.state.manager.brain);
            self.notify(&digest.matter, None, agent, digest.title, digest.body, false, None, Some(initials.into()), false, cx);
        }
    }

    /// Close a session's notifications (it was opened).
    pub fn dismiss_popups_for(&mut self, session_id: &str, cx: &mut App) {
        acknowledge(session_id);
        let mine: Vec<AnyWindowHandle> = self.popups.iter().filter(|(id, _)| id == session_id).map(|(_, h)| *h).collect();
        for handle in mine {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        self.popups.retain(|(id, _)| id != session_id);
    }
}

/// Where the notification in `place` (0 is the first) goes on a screen whose area is `area`. On
/// the Mac they hang from the top right, below the menu bar, the first place at the top. On
/// Windows they stand at the bottom right, above the taskbar, where Windows shows its own; the
/// first place is the corner.
///
/// The window is larger than the card by `INSET` on each side (the card's shadow).
///
/// The card follows the interface's `scale`, as what it holds does (at 130 % its buttons reached
/// past a card of the 100 % size); the room to the screen's edge and the menu bar belong to the
/// system and stay as they are.
/// From the top of the Mac's screen to the first window: its menu bar, and a little.
const MENU_BAR: f32 = 42. - INSET;

fn popup_bounds(area: Bounds<Pixels>, place: usize, scale: f32) -> Bounds<Pixels> {
    let (width, height) = (WIDTH * scale, HEIGHT * scale);
    let x = area.origin.x + area.size.width - px(width + MARGIN);
    let step = px(place as f32 * (height + GAP * scale));
    let y = if cfg!(windows) { area.origin.y + area.size.height - px(height + MARGIN) - step } else { area.origin.y + px(MENU_BAR) + step };
    Bounds { origin: point(x, y), size: size(px(width), px(height)) }
}

/// A notification in a window of its own, which goes by itself at the notification's time.
fn open_popup(view: Entity<Popup>, bounds: Bounds<Pixels>, display: DisplayId, cx: &mut App) -> Option<AnyWindowHandle> {
    let left = view.read(cx).until.saturating_duration_since(Instant::now());
    let opened = cx.open_window(
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            focus: false,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            display_id: Some(display),
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        |window, cx| {
            window
                .spawn(cx, async move |cx| {
                    cx.background_executor().timer(left).await;
                    let _ = cx.update(|window, _| window.remove_window());
                })
                .detach();
            view
        },
    );
    match opened {
        Ok(handle) => Some(handle.into()),
        Err(error) => {
            log::warn!("could not show a notification: {error:#}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifications_stand_one_after_the_other_from_the_corner() {
        // A 1920 × 1080 screen with a taskbar of 48 at the bottom (the work area on Windows).
        let area = Bounds { origin: point(px(0.), px(0.)), size: size(px(1920.), px(1032.)) };
        let (first, second) = (popup_bounds(area, 0, 1.), popup_bounds(area, 1, 1.));
        assert_eq!((first.size, second.size), (size(px(WIDTH), px(HEIGHT)), size(px(WIDTH), px(HEIGHT))));
        assert_eq!((first.right(), second.right()), (px(1920. - MARGIN), px(1920. - MARGIN)));
        // The card, inside the window by the inset, is 20 from the edge of the work area, and
        // two cards are 12 apart (16 on the Mac).
        assert_eq!((MARGIN + INSET, GAP + 2. * INSET), (20., if cfg!(windows) { 12. } else { 16. }));
        if cfg!(windows) {
            assert_eq!(first.bottom(), px(1032. - MARGIN), "the first is in the corner, above the taskbar");
            assert_eq!(second.bottom(), first.top() - px(GAP), "the next is above it");
        } else {
            assert_eq!(first.top(), px(MENU_BAR), "the first is under the menu bar");
            assert_eq!(second.top(), first.bottom() + px(GAP), "the next is under it");
        }
        // A second screen to the left, whose work area starts lower (a taskbar at its top).
        let left = Bounds { origin: point(px(-1280.), px(40.)), size: size(px(1280.), px(984.)) };
        let corner = popup_bounds(left, 0, 1.);
        assert_eq!(corner.right(), px(-MARGIN));
        assert!(left.contains(&corner.origin) && corner.bottom() <= left.bottom(), "{corner:?}");
    }

    #[test]
    fn a_notification_grows_with_the_scale_and_keeps_its_corner() {
        let area = Bounds { origin: point(px(0.), px(0.)), size: size(px(1920.), px(1032.)) };
        let (first, second) = (popup_bounds(area, 0, 1.3), popup_bounds(area, 1, 1.3));
        assert_eq!(first.size, size(px(WIDTH * 1.3), px(HEIGHT * 1.3)));
        assert_eq!(first.right(), px(1920. - MARGIN), "the same room to the screen's edge");
        if cfg!(windows) {
            assert_eq!(first.bottom(), px(1032. - MARGIN));
            assert!((f32::from(first.top() - second.bottom()) - GAP * 1.3).abs() < 0.01, "{first:?} {second:?}");
        } else {
            assert_eq!(first.top(), px(MENU_BAR));
            assert!((f32::from(second.top() - first.bottom()) - GAP * 1.3).abs() < 0.01, "{first:?} {second:?}");
        }
    }
}
