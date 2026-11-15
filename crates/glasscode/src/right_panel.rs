//! The session's side panel (right of the terminal): Pull request, Changes, Files and
//! Glasscode.


use gpui::{AnyElement, ClickEvent, Context, FontWeight, SharedString, div, prelude::*, px};

use crate::theme;
use crate::ui;
use crate::workspace::{RightTab, Workspace};

impl Workspace {
    pub(crate) fn render_right_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        if let crate::center::Center::Server(id) = &self.center {
            // The files are on the server; this Mac's git view would show the wrong worktree.
            let folder = self.server.session(id).map(|s| s.session.worktree.display().to_string()).unwrap_or_default();
            return div()
                .flex()
                .flex_col()
                .flex_none()
                .w(px(self.right_width()))
                .h_full()
                .bg(theme::panel())
                .border_l_1()
                .border_color(theme::border())
                .child(div().flex().items_center().h(px(38.)).px_3().border_b_1().border_color(theme::border()).child(ui::caps_label(format!("ON {}", self.server.name().to_uppercase()))))
                .child(
                    div()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .text_size(px(12.5))
                        .text_color(theme::muted())
                        .child(div().text_color(theme::text()).child("This session's files are on the server."))
                        .child(div().font_family(theme::MONO_FONT).text_size(px(11.5)).child(folder))
                        .child("Ask the agent for a diff or to commit and push; the Git panel for server worktrees comes with laptop ⇄ server sync."),
                )
                .into_any_element();
        }
        let tab_shown = self.right_tab;
        let plain_folder = self.active_session().is_some_and(|s| !self.is_git_project(&s.project_id));
        let content: AnyElement = match tab_shown {
            RightTab::PullRequest | RightTab::Changes | RightTab::Files if plain_folder => self.render_no_git(cx),
            RightTab::PullRequest => div()
                .id("pr-scroll")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .pb_3()
                .child(self.render_pr_section(cx).unwrap_or_else(|| {
                    div().p_4().text_size(px(13.)).text_color(theme::muted()).child("This repository is not on GitHub, so its work has no pull request here.").into_any_element()
                }))
                .into_any_element(),
            RightTab::Changes => self.render_changes_tab(cx),
            RightTab::Files => self.render_files_tab(cx),
            RightTab::Glasscode => self.glasscode_panel(cx),
        };
        let tab = |id: &'static str, label: String, shown: RightTab| {
            let selected = tab_shown == shown;
            div()
                .id(id)
                .flex()
                .flex_none()
                .items_center()
                .gap_1p5()
                .h(px(32.))
                .px_3()
                .rounded_lg()
                .cursor_pointer()
                .text_size(px(13.5))
                .font_weight(FontWeight::SEMIBOLD)
                .when(selected, |d| d.bg(theme::selected()).text_color(theme::text_strong()))
                .when(!selected, |d| d.text_color(theme::muted()).hover(|s| s.text_color(theme::text_strong())))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.right_tab = shown;
                    if shown == RightTab::Files {
                        this.files_tick(cx);
                    }
                    cx.notify();
                }))
                .child(label)
        };
        let changes = if self.changes.is_empty() { "Changes".to_string() } else { format!("Changes {}", self.changes.len()) };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.right_width()))
            .h_full()
            .bg(theme::panel())
            .border_l_1()
            .border_color(theme::border())
            .child(
                div()
                    .id("right-tabs")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .px_2p5()
                    .py_2()
                    .overflow_x_scroll()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(tab("tab-pr", "Pull request".into(), RightTab::PullRequest))
                    .child(tab("tab-changes", changes, RightTab::Changes))
                    .child(tab("tab-files", "Files".into(), RightTab::Files))
                    .when(crate::glasscode_ui::VOICE, |d| d.child(tab("tab-glasscode", "Glasscode".into(), RightTab::Glasscode))),
            )
            .child(content)
            .into_any_element()
    }

    /// A folder without git: the tabs that need it say so, with Set up git.
    fn render_no_git(&self, cx: &mut Context<Self>) -> AnyElement {
        let project = self.active_session().map(|s| s.project_id.clone()).unwrap_or_default();
        div()
            .p_4()
            .flex()
            .flex_col()
            .gap_2()
            .text_size(px(13.))
            .text_color(theme::muted())
            .child(div().text_color(theme::text_strong()).font_weight(FontWeight::SEMIBOLD).child("This folder has no git"))
            .child("Its sessions work in the folder itself. Set up git to track changes, push to GitHub and give each new session its own worktree. It commits what is here as a first commit; files that look like secrets (.env, keys) are kept out.")
            .child(div().flex().child(ui::primary_button("git-set-up", "Set up git").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_up_git(&project, cx)))))
            .into_any_element()
    }

    /// The worktree's changed files, what is staged, and the commit box.
    fn render_changes_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let branch = self.active_session().map(|s| s.branch.clone()).unwrap_or_default();
        let staged = self.changes.iter().filter(|c| c.staged).count();
        let author = self.active_session().and_then(|s| self.github_account_for(&s.project_id));
        let loaded = self.changes_for.is_some() && self.changes_for.as_ref() == self.active_session().map(|s| &s.worktree);
        let rows: Vec<AnyElement> = self
            .changes
            .iter()
            .map(|change| {
                let (dir, name) = match change.path.rsplit_once('/') {
                    Some((dir, name)) => (dir.to_string(), name.to_string()),
                    None => (String::new(), change.path.clone()),
                };
                let stage_change = change.clone();
                let open_path = change.path.clone();
                let color = match change.status {
                    'A' | '?' => theme::added(),
                    'D' | 'U' => theme::removed(),
                    _ => theme::modified(),
                };
                div()
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .h(px(36.))
                    .px_1()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(ui::checkbox(SharedString::from(format!("stage-{}", change.path)), change.staged).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_stage(stage_change.clone(), cx))))
                    .child(
                        div()
                            .id(SharedString::from(format!("change-{}", change.path)))
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .h_full()
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme::text_strong()))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_diff(open_path.clone(), cx)))
                            .child(div().flex_none().w(px(12.)).font_family(theme::MONO_FONT).text_size(px(11.5)).font_weight(FontWeight::BOLD).text_color(color).child(match change.status {
                                '?' => "U".to_string(),
                                'U' => "!".to_string(),
                                s => s.to_string(),
                            }))
                            .child(
                                div()
                                    .flex()
                                    .flex_1()
                                    .min_w_0()
                                    .items_baseline()
                                    .gap_1p5()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(div().font_family(theme::MONO_FONT).text_size(px(12.5)).text_color(theme::text()).child(name))
                                    .child(div().min_w_0().overflow_hidden().text_ellipsis().font_family(theme::MONO_FONT).text_size(px(11.)).text_color(theme::muted()).child(dir)),
                            )
                            .child(div().flex_none().font_family(theme::MONO_FONT).text_size(px(12.)).text_color(theme::added()).child(format!("+{}", change.added)))
                            .child(div().flex_none().font_family(theme::MONO_FONT).text_size(px(12.)).text_color(theme::removed()).child(format!("−{}", change.removed))),
                    )
                    .into_any_element()
            })
            .collect();
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .px_3()
            .pt_3()
            .pb_1p5()
            .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(theme::muted()).child(if self.changes.is_empty() {
                String::new()
            } else {
                format!("{} file{} on {branch}", self.changes.len(), if self.changes.len() == 1 { "" } else { "s" })
            }))
            .child(div().flex_1())
            .when(!self.changes.is_empty(), |d| {
                d.child(
                    div()
                        .id("stage-all")
                        .text_size(px(12.))
                        .text_color(theme::muted())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::text_strong()))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.stage_all(cx)))
                        .child(format!("{staged} staged · stage all")),
                )
            });
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .when(!self.changes.is_empty(), |d| d.child(header))
            .child(
                div()
                    .id("changes-scroll")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_2p5()
                    .children(rows)
                    .when(self.changes.is_empty(), |d| {
                        d.child(div().pt_3().px_1().text_size(px(13.5)).text_color(theme::muted()).child(if loaded { format!("Clean on {branch}: nothing to commit.") } else { "Checking…".to_string() }))
                    }),
            )
            .when(!self.changes.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_none()
                        .flex_col()
                        .gap_2()
                        .p_3()
                        .border_t_1()
                        .border_color(theme::border())
                        .child(div().text_size(px(12.)).text_color(theme::muted()).child("Commit message"))
                        .child(self.commit_input.clone())
                        .child(if staged > 0 {
                            // Who the commit is made as; how many files go in is the hint below.
                            let label = match &author {
                                Some(login) => format!("Commit as {login}"),
                                None => "Commit".to_string(),
                            };
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(ui::primary_button("commit", label).w_full().on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.commit(cx))))
                                .child(div().text_size(px(11.5)).text_color(theme::muted()).child(format!("{staged} staged file{}", if staged == 1 { " goes in" } else { "s go in" })))
                        } else {
                            div().child(ui::disabled_button("commit", "Stage files to commit").w_full())
                        }),
                )
            })
            .into_any_element()
    }
}
