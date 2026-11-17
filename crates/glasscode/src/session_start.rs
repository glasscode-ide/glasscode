//! New session: where the work starts. Build from the latest main (fetched first) or from a
//! pull request; or Review a pull request, found by its number or words from it.

use std::time::Duration;

use gpui::{AnyElement, ClickEvent, Context, FontWeight, SharedString, div, prelude::*};
use glasscode_core::git::{self, FreshBase};
use glasscode_core::github::{self, PrHit};

use crate::dialogs::Modal;
use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

/// "2 hours ago" for a GitHub time stamp.
pub fn ago_stamp(stamp: &str) -> String {
    let Some(at) = github::parse_time(stamp) else { return String::new() };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(at);
    ago_secs(now - at)
}

pub fn ago_secs(secs: i64) -> String {
    let minutes = secs.max(0) / 60;
    match minutes {
        0..=1 => "just now".into(),
        2..=59 => format!("{minutes} min ago"),
        60..=119 => "1 hour ago".into(),
        120..=1439 => format!("{} hours ago", minutes / 60),
        1440..=2879 => "yesterday".into(),
        _ => format!("{} days ago", minutes / 1440),
    }
}

/// Where a PR stands, in a word or two, and its color.
pub fn pr_state(hit: &PrHit) -> (&'static str, gpui::Hsla) {
    match (hit.state.as_str(), hit.draft, hit.review_decision.as_str()) {
        ("MERGED", _, _) => ("Merged", theme::codex()),
        ("CLOSED", _, _) => ("Closed", theme::muted()),
        (_, true, _) => ("Draft", theme::muted()),
        (_, _, "APPROVED") => ("Approved", theme::added()),
        (_, _, "CHANGES_REQUESTED") => ("Changes requested", theme::removed()),
        _ => ("Waiting for review", theme::modified()),
    }
}

fn with_form(ws: &mut Workspace, f: impl FnOnce(&mut crate::dialogs::NewSessionForm)) {
    if let Some(Modal::NewSession(form)) = ws.modal.as_mut() {
        f(form);
    }
}

impl Workspace {
    /// The dialog's project folder on this Mac, or its repository on the server.
    fn new_session_target(&self) -> Option<(Option<std::path::PathBuf>, Option<String>)> {
        let Some(Modal::NewSession(form)) = self.modal.as_ref() else { return None };
        if form.on_server {
            let repo = self.server.state.as_ref().and_then(|s| s.repos.get(&form.project_id).cloned());
            Some((None, repo))
        } else {
            Some((self.state.project(&form.project_id).map(|p| p.root.clone()), None))
        }
    }

    /// Fetch the default branch as the dialog opens (this Mac), so it can say where a new
    /// branch starts. The server fetches when it creates the worktree.
    pub fn new_session_fetch_base(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::NewSession(form)) = self.modal.as_ref() else { return };
        if form.on_server {
            return;
        }
        let Some(root) = self.state.project(&form.project_id).map(|p| p.root.clone()) else { return };
        if form.fresh_for.as_ref() == Some(&root) {
            return;
        }
        let wanted = root.clone();
        with_form(self, |f| {
            f.fresh = None;
            f.fresh_for = Some(root.clone());
        });
        let fetch = !self.state.prefs.fresh_main_off;
        let task = cx.background_spawn(async move { git::start_base(&root, fetch) });
        cx.spawn(async move |this, cx| {
            let fresh = task.await;
            let _ = this.update(cx, |this, cx| {
                with_form(this, |f| {
                    if f.fresh_for.as_ref() == Some(&wanted) {
                        f.fresh = Some(fresh);
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// Search once typing settles.
    pub fn pr_search_soon(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::NewSession(form)) = self.modal.as_ref() else { return };
        let typed = form.pr_query.read(cx).text();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(350)).await;
            let _ = this.update(cx, |this, cx| {
                let Some(Modal::NewSession(form)) = this.modal.as_ref() else { return };
                if form.pr_query.read(cx).text() == typed {
                    this.pr_search_now(cx);
                }
            });
        })
        .detach();
    }

    /// Search for what is typed (an empty query lists the PRs asking for your review, else
    /// the newest open ones).
    pub fn pr_search_now(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::NewSession(form)) = self.modal.as_ref() else { return };
        let query = form.pr_query.read(cx).text().trim().to_string();
        let review_mode = form.review_mode;
        let key = format!("{}|{}|{query}", form.on_server, form.project_id);
        if form.pr_searched.as_deref() == Some(key.as_str()) {
            return;
        }
        let Some((root, repo)) = self.new_session_target() else { return };
        let env = self.env.clone();
        with_form(self, |f| {
            f.pr_searched = Some(key.clone());
            f.pr_hits = None;
            f.pr_error = None;
        });
        cx.notify();
        let task = cx.background_spawn(async move {
            let repo = repo
                .or_else(|| root.as_deref().and_then(github::repo_slug))
                .ok_or_else(|| anyhow::anyhow!("This project has no GitHub repository (origin is not on GitHub)."))?;
            if query.is_empty() && review_mode {
                let asked = github::review_requests(&repo, &env)?;
                if !asked.is_empty() {
                    return Ok::<_, anyhow::Error>((asked, true));
                }
            }
            Ok((github::search_prs(&repo, &query, &env)?, false))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                with_form(this, |f| {
                    if f.pr_searched.as_deref() != Some(key.as_str()) {
                        return;
                    }
                    match result {
                        Ok((hits, asked)) => {
                            f.pr_hits = Some(hits);
                            f.pr_asked = asked;
                        }
                        Err(err) => f.pr_error = Some(err.to_string()),
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    pub fn pick_pr(&mut self, hit: Option<PrHit>, cx: &mut Context<Self>) {
        let searching = hit.is_none();
        with_form(self, |f| f.pr_picked = hit);
        if searching {
            self.pr_search_now(cx);
        }
        cx.notify();
    }
}

/// Where a branch from "Latest main" starts, as fetched when the dialog opened.
pub fn base_note(form: &crate::dialogs::NewSessionForm, server: Option<String>, fetch: bool) -> AnyElement {
    let (mark, color, text) = match (server, &form.fresh) {
        (Some(server), _) => ("↻", theme::muted(), format!("{server} fetches origin first, so the branch starts from the latest commit.")),
        (None, Some(FreshBase { base, .. })) if !fetch => ("·", theme::muted(), format!("Starts from {base} as it is on {} (Settings › Git and projects).", crate::platform::this_computer())),
        (None, None) => ("↻", theme::muted(), "Fetching the latest main…".to_string()),
        (None, Some(FreshBase { fetched: true, base, newest_commit, .. })) => (
            "✓",
            theme::added(),
            match newest_commit {
                Some(at) => {
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(*at);
                    format!("Fetched {base} just now · newest commit {}", ago_secs(now - at))
                }
                None => format!("Fetched {base} just now"),
            },
        ),
        (None, Some(FreshBase { base, .. })) => ("!", theme::modified(), format!("Could not fetch (offline?); starts from {base} as it is here.")),
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_2p5()
        .py_2()
        .rounded_md()
        .bg(theme::input())
        .border_1()
        .border_color(theme::border())
        .child(div().flex_none().w(theme::sz(14.)).text_color(color).font_weight(FontWeight::BOLD).child(mark))
        .child(div().text_size(theme::sz(12.5)).text_color(theme::text_soft()).child(text))
        .into_any_element()
}

/// The PR search box, its results, and the picked PR.
pub fn render_pr_picker(form: &crate::dialogs::NewSessionForm, cx: &mut Context<Workspace>) -> AnyElement {
    let meta = |hit: &PrHit| {
        let (state, color) = pr_state(hit);
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x_2()
            .text_size(theme::sz(11.5))
            .text_color(theme::muted())
            .child(hit.author.clone())
            .child(div().font_family(theme::MONO_FONT).text_color(theme::text_soft()).child(format!("{} → {}", hit.head, hit.base)))
            .child(ago_stamp(&hit.updated_at))
            .child(div().text_color(color).font_weight(FontWeight::SEMIBOLD).child(state))
    };
    if let Some(hit) = &form.pr_picked {
        return div()
            .flex()
            .flex_col()
            .gap_1p5()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(theme::accent())
            .bg(theme::hover())
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .child(div().flex_none().font_family(theme::MONO_FONT).text_size(theme::sz(12.)).text_color(theme::muted()).child(format!("#{}", hit.number)))
                    .child(div().flex_1().min_w_0().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(hit.title.clone()))
                    .child(
                        div()
                            .id("pr-change")
                            .flex_none()
                            .text_size(theme::sz(12.5))
                            .text_color(theme::link())
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_pr(None, cx)))
                            .child("Change…"),
                    ),
            )
            .child(meta(hit))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .text_size(theme::sz(11.5))
                    .font_family(theme::MONO_FONT)
                    .child(div().text_color(theme::muted()).child(format!("{} files", hit.changed_files)))
                    .child(div().text_color(theme::added()).child(format!("+{}", hit.additions)))
                    .child(div().text_color(theme::removed()).child(format!("−{}", hit.deletions))),
            )
            .into_any_element();
    }
    let caption = if form.pr_asked {
        "ASKING FOR YOUR REVIEW"
    } else if form.pr_query.read(cx).text().trim().is_empty() {
        "OPEN PULL REQUESTS"
    } else {
        "MATCHES"
    };
    let mut rows: Vec<AnyElement> = Vec::new();
    match (&form.pr_error, &form.pr_hits) {
        (Some(err), _) => rows.push(div().px_1().text_size(theme::sz(12.)).text_color(theme::removed()).child(err.clone()).into_any_element()),
        (None, None) => rows.push(div().px_1().text_size(theme::sz(12.)).text_color(theme::faint()).child("Looking on GitHub…").into_any_element()),
        (None, Some(hits)) if hits.is_empty() => rows.push(
            div().px_1().text_size(theme::sz(12.)).text_color(theme::faint()).child("No open pull requests match. Type a number for a closed one.").into_any_element(),
        ),
        (None, Some(hits)) => {
            for hit in hits.iter().take(8) {
                let picked = hit.clone();
                rows.push(
                    div()
                        .id(SharedString::from(format!("pr-hit-{}", hit.number)))
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .px_2p5()
                        .py_2()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.pick_pr(Some(picked.clone()), cx)))
                        .child(
                            div()
                                .flex()
                                .items_baseline()
                                .gap_2()
                                .child(div().flex_none().font_family(theme::MONO_FONT).text_size(theme::sz(12.)).text_color(theme::muted()).child(format!("#{}", hit.number)))
                                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme::text_strong()).child(hit.title.clone())),
                        )
                        .child(meta(hit))
                        .into_any_element(),
                );
            }
        }
    }
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(form.pr_query.clone())
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .p_1()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::input())
                .child(div().px_1p5().pt_1().child(ui::caps_label(caption)))
                .children(rows),
        )
        .into_any_element()
}

/// The three steps a review goes through, shown in the dialog.
pub fn review_steps() -> AnyElement {
    let step = |n: &str, title: &str, detail: &str, first: bool| {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap_1()
            .p_2p5()
            .rounded_lg()
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .size(theme::sz(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(if first { theme::accent() } else { theme::border_strong() })
                            .text_size(theme::sz(11.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(if first { theme::on_accent() } else { theme::text() })
                            .child(n.to_string()),
                    )
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(title.to_string())),
            )
            .child(div().text_size(theme::sz(11.5)).text_color(theme::muted()).child(detail.to_string()))
    };
    div()
        .flex()
        .gap_2()
        .child(step("1", "Explain", "The agent reads the PR and the code around it and tells you what it does", true))
        .child(step("2", "Plan", "Add your view if you like; it writes a review plan you can edit", false))
        .child(step("3", "Private review", "A reviewer checks it with your plan. Nothing is posted", false))
        .into_any_element()
}
