//! The frame of the window: the title bar, the left sidebar, the centre, the right panel, the
//! status bar and the popovers over them.

use gpui::{Context, div, prelude::*, px};

use crate::theme;
use crate::workspace::Workspace;

impl Workspace {
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
