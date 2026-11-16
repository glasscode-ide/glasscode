//! The frame of the window: the title bar, the left sidebar, the centre, the right panel, the
//! status bar and the popovers over them.

use gpui::{ClickEvent, Context, FontWeight, div, prelude::*, px};

use crate::theme;
use crate::ui;
use crate::workspace::{OpenPalette, Workspace};

impl Workspace {
    pub(crate) fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (project, worktree) = match (&self.center, self.active_session()) {
            _ if matches!(self.tickets.view, crate::tickets_ui::View::Usage | crate::tickets_ui::View::Settings | crate::tickets_ui::View::Themes) => (
                match self.tickets.view {
                    crate::tickets_ui::View::Usage => "Usage",
                    crate::tickets_ui::View::Themes => "Themes",
                    _ => "Settings",
                }
                .to_string(),
                String::new(),
            ),
            _ if self.tickets.view == crate::tickets_ui::View::Today => ("Today".to_string(), String::new()),
            _ if self.tickets.view == crate::tickets_ui::View::Tickets => (
                "Tickets".to_string(),
                self.tickets.snapshot.as_ref().map(|s| format!("Linear · {}", s.org)).unwrap_or_else(|| "Linear".into()),
            ),
            (crate::center::Center::Server(id), _) => (
                self.server.name(),
                self.server.session(id).map(|s| s.session.branch.clone()).unwrap_or_default(),
            ),
            (_, session) => match session {
            Some(session) => (
                self.state
                    .project(&session.project_id)
                    .map(|p| p.name.clone())
                    .unwrap_or_default(),
                session.branch.clone(),
            ),
            None => ("No project".into(), String::new()),
            },
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_4()
            .h(px(44.))
            .pl(px(84.))
            .pr_3()
            // Windows: no traffic lights at the left, and the window buttons end the bar at the right.
            .when(cfg!(windows), |d| {
                d.pl(px(12.))
                    .pr_0()
                    .window_control_area(gpui::WindowControlArea::Drag)
            })
            .bg(theme::chrome())
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(crate::icons::icon(crate::icons::LOGO, px(19.), px(18.), theme::text_strong()))
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(14.)).child("Glasscode")),
            )
            .child(
                div()
                    .flex()
                    .flex_shrink(1.)
                    .min_w_0()
                    .max_w(px(380.))
                    .items_center()
                    .gap_1p5()
                    .text_color(theme::muted())
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(project)
                    .when(!worktree.is_empty(), |d| {
                        d.child(div().text_color(theme::faint()).child("/"))
                            .child(div().min_w_0().overflow_hidden().text_ellipsis().font_family(theme::MONO_FONT).text_size(px(12.)).text_color(theme::text_soft()).child(worktree))
                    }),
            )
            // Search: the command palette.
            .child(
                div()
                    .id("title-search")
                    // Windows: a press opens the palette, it does not move the window.
                    .when(cfg!(windows), |d| d.occlude())
                    .flex()
                    .flex_1()
                    .min_w(px(180.))
                    .max_w(px(460.))
                    .mx_auto()
                    .items_center()
                    .gap_2()
                    .h(px(30.))
                    .px_2p5()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(theme::border_strong())
                    .bg(theme::raised())
                    .text_color(theme::muted())
                    .cursor_pointer()
                    .hover(|s| s.border_color(theme::faint()))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_palette(window, cx)))
                    .child(div().text_color(theme::faint()).child("⌕"))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child("Search sessions, tickets and commands"))
                    .child(div().px_1p5().rounded(px(4.)).border_1().border_color(theme::border()).text_size(px(11.)).text_color(theme::faint()).child(crate::keymap::label(&OpenPalette))),
            )
            .when_some(self.render_switch_buttons(cx), |d, buttons| {
                d.child(crate::window_controls::clickable(buttons))
            })
            .when_some(self.update_pill(cx), |d, pill| {
                d.child(crate::window_controls::clickable(pill))
            })
            .child(crate::window_controls::clickable(self.server_pill(cx)))
            .child(crate::window_controls::clickable(
                ui::primary_button("new-session", "+  New session").on_click(cx.listener(
                    |this, _: &ClickEvent, window, cx| this.open_new_session(None, None, window, cx),
                )),
            ))
            .when(cfg!(windows), |d| {
                d.child(crate::window_controls::WindowControls)
            })
    }

    pub(crate) fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.active_session();
        let account = session.and_then(|s| {
            s.account_id
                .as_deref()
                .and_then(|id| self.state.account(id))
                .map(|a| format!("{} · {}", s.agent.name(), a.display()))
        });
        let cli_warning = self
            .cli_status
            .iter()
            .filter(|s| !s.is_ok())
            .map(|s| s.summary())
            .collect::<Vec<_>>()
            .join(" · ");
        let cli_ok = self
            .cli_status
            .iter()
            .filter(|s| s.is_ok())
            .map(|s| s.summary())
            .collect::<Vec<_>>()
            .join(" · ");
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_4()
            .h(px(26.))
            .px_3()
            .bg(theme::chrome())
            .border_t_1()
            .border_color(theme::border())
            .text_size(px(11.5))
            .text_color(theme::muted())
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .child(format!("⎇ {}", session.map(|s| s.branch.clone()).unwrap_or_default())),
            )
            .child(if self.changes.is_empty() {
                "Clean".to_string()
            } else {
                format!("{} changed", self.changes.len())
            })
            .when_some(session.and_then(|s| crate::github_accounts_ui::status_label(self, &s.project_id)), |d, label| d.child(label))
            .child(div().flex_1())
            .when(!cli_warning.is_empty(), |d| {
                d.child(div().text_color(theme::attention()).child(format!("⚠ {cli_warning}")))
            })
            .when(cli_warning.is_empty() && !cli_ok.is_empty(), |d| d.child(cli_ok))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(div().size(px(6.)).rounded_full().bg(theme::added()))
                    .child({
                        let n = self.state.sessions.len();
                        let synced = self.mirror.synced.is_some() && self.mirror.error.is_none();
                        format!("{n} {} saved · {}", if n == 1 { "session" } else { "sessions" }, if synced { "in sync on the server" } else { "restored on relaunch" })
                    }),
            )
            .when_some(account, |d, account| d.child(account))
    }
}
