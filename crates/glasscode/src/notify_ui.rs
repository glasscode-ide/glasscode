//! Telling the person a session is done or needs them, the way Zed does: a small window at
//! the top right of the screen (the bottom right on Windows; it takes no focus and stays above
//! other apps), with Open and Dismiss, and a short sound. Only when they are not looking at
//! that session already. It goes by itself after a while, or when the session is opened.

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

const WIDTH: f32 = 380.;
const HEIGHT: f32 = 88.;
const GAP: f32 = 10.;
const SHOWN_FOR: Duration = Duration::from_secs(45);
const MAX_SHOWN: usize = 4;

pub struct Popup {
    session_id: String,
    /// A ticket or a review request (Open goes to Tickets); else a session.
    ticket: Option<crate::tickets_ui::Key>,
    agent: AgentKind,
    title: SharedString,
    body: SharedString,
    needs_you: bool,
    /// An agent's request waiting for the person: Allow and Deny instead of Open.
    approval: Option<u64>,
    workspace: WeakEntity<Workspace>,
    main: Option<AnyWindowHandle>,
    /// When it goes by itself.
    until: Instant,
}

impl Popup {
    fn open_session(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let (workspace, id, ticket) = (self.workspace.clone(), self.session_id.clone(), self.ticket.clone());
        cx.activate(true);
        if let Some(main) = self.main {
            let _ = main.update(cx, |_, main_window, cx| {
                main_window.activate_window();
                let _ = workspace.update(cx, |ws, cx| match ticket {
                    Some(key) => ws.open_ticket(key, cx),
                    None => {
                        ws.view = crate::shell::View::Sessions;
                        ws.select_session(&id, main_window, cx)
                    }
                });
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

impl Render for Popup {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let accent = if self.needs_you { crate::sidebar::attention() } else { theme::added() };
        div().size_full().p(px(4.)).font_family(theme::UI_FONT).child(
            div()
                .size_full()
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .rounded_xl()
                .border_1()
                .border_color(theme::border_strong())
                .bg(theme::dialog())
                .shadow_lg()
                .child(div().relative().child(if self.ticket.is_some() {
                    div().size(px(30.)).rounded(px(8.)).flex().items_center().justify_center().bg(theme::selected()).text_color(theme::text_strong()).text_size(px(15.)).child("◫")
                } else {
                    crate::sidebar::agent_icon(self.agent, 30., true)
                }).child(
                    div().absolute().right(px(-3.)).bottom(px(-3.)).size(px(11.)).rounded_full().border_2().border_color(theme::dialog()).bg(accent),
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(self.title.clone()))
                        .child(div().text_size(px(12.)).text_color(theme::muted()).overflow_hidden().whitespace_nowrap().text_ellipsis().child(self.body.clone())),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .when(self.approval.is_none(), |d| {
                            d.child(ui::primary_button("popup-open", "Open").on_click(cx.listener(Self::open_session)))
                                .child(ui::button("popup-dismiss", "Dismiss").on_click(cx.listener(|_, _: &ClickEvent, window, _| window.remove_window())))
                        })
                        .when(self.approval.is_some(), |d| {
                            d.child(ui::primary_button("popup-allow", "Allow").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.decide(true, window, cx))))
                                .child(ui::button("popup-deny", "Deny").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.decide(false, window, cx))))
                        }),
                ),
        )
    }
}

impl Workspace {
    /// Show a notification for a session (and play the sound, unless switched off).
    pub fn notify_session(&mut self, session_id: &str, agent: AgentKind, title: String, body: String, needs_you: bool, cx: &mut Context<Self>) {
        self.notify(session_id, None, agent, title, body, needs_you, None, cx);
    }

    /// An agent asks to do something: Allow and Deny right in the notification.
    pub fn notify_approval(&mut self, id: u64, agent: AgentKind, title: String, body: String, cx: &mut Context<Self>) {
        self.notify(&format!("approval:{id}"), None, agent, title, body, true, Some(id), cx);
    }

    /// A ticket assigned to the person, or a pull request that asks for their review.
    pub fn notify_ticket(&mut self, key: crate::tickets_ui::Key, title: String, body: String, cx: &mut Context<Self>) {
        let id = match &key {
            crate::tickets_ui::Key::Ticket(id) => format!("ticket:{id}"),
            crate::tickets_ui::Key::Pr(repo, n) => format!("pr:{repo}#{n}"),
        };
        self.notify(&id, Some(key), AgentKind::Claude, title, body, true, None, cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(&mut self, session_id: &str, ticket: Option<crate::tickets_ui::Key>, agent: AgentKind, title: String, body: String, needs_you: bool, approval: Option<u64>, cx: &mut Context<Self>) {
        // A test copy (debug, in the background) never puts windows on the person's screen.
        if cfg!(debug_assertions) && std::env::var_os("GLASSCODE_DEBUG_BACKGROUND").is_some() {
            log::warn!("notification (not shown in a test copy): {title} · {body}");
            return;
        }
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
        let popup = Popup {
            session_id: session_id.to_string(),
            ticket,
            agent,
            title: title.into(),
            body: body.into(),
            needs_you,
            approval,
            workspace: cx.weak_entity(),
            main: self.main_window,
            until: Instant::now() + SHOWN_FOR,
        };
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
                if let Some(handle) = open_popup(older, popup_bounds(area, count - i), display.id(), cx) {
                    self.popups.push((id, handle));
                }
            }
            0
        } else {
            self.popups.len()
        };
        if let Some(handle) = open_popup(view, popup_bounds(area, place), display.id(), cx) {
            self.popups.push((session_id.to_string(), handle));
        }
        if !self.state.notify.sound_off {
            glasscode_core::sys::play_sound(if needs_you { Sound::NeedsYou } else { Sound::Done });
        }
    }

    /// Close a session's notifications (it was opened).
    pub fn dismiss_popups_for(&mut self, session_id: &str, cx: &mut App) {
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
fn popup_bounds(area: Bounds<Pixels>, place: usize) -> Bounds<Pixels> {
    let x = area.origin.x + area.size.width - px(WIDTH + 16.);
    let step = px(place as f32 * (HEIGHT + GAP));
    let y = if cfg!(windows) { area.origin.y + area.size.height - px(HEIGHT + 16.) - step } else { area.origin.y + px(38.) + step };
    Bounds { origin: point(x, y), size: size(px(WIDTH), px(HEIGHT)) }
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
        let (first, second) = (popup_bounds(area, 0), popup_bounds(area, 1));
        assert_eq!((first.size, second.size), (size(px(WIDTH), px(HEIGHT)), size(px(WIDTH), px(HEIGHT))));
        assert_eq!((first.right(), second.right()), (px(1920. - 16.), px(1920. - 16.)));
        if cfg!(windows) {
            assert_eq!(first.bottom(), px(1032. - 16.), "the first is in the corner, above the taskbar");
            assert_eq!(second.bottom(), first.top() - px(GAP), "the next is above it");
        } else {
            assert_eq!(first.top(), px(38.), "the first is under the menu bar");
            assert_eq!(second.top(), first.bottom() + px(GAP), "the next is under it");
        }
        // A second screen to the left, whose work area starts lower (a taskbar at its top).
        let left = Bounds { origin: point(px(-1280.), px(40.)), size: size(px(1280.), px(984.)) };
        let corner = popup_bounds(left, 0);
        assert_eq!(corner.right(), px(-16.));
        assert!(left.contains(&corner.origin) && corner.bottom() <= left.bottom(), "{corner:?}");
    }
}
