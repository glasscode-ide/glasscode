//! The Pull request tab: the session's pull request, its checks, preview, reviews and comments.
//! Sections that are fine fold to one line; the ones that need you open themselves.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{AnyElement, ClickEvent, Context, Entity, FontWeight, Hsla, SharedString, Window, div, prelude::*};
use glasscode_core::github::{self, CheckState, PrDetails, PreviewConfig, ReviewThread};
use glasscode_core::model::AgentKind;

use crate::center::{Center, open_url};
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::ui;
use crate::workspace::{RightTab, Workspace};

type PrKey = (String, u64);

#[derive(Default)]
pub struct GithubUi {
    pub details: HashMap<PrKey, PrDetails>,
    pub loading: HashSet<PrKey>,
    pub errors: HashMap<PrKey, String>,
    fetched: HashMap<PrKey, Instant>,
    /// Sections the developer opened or closed, by "<repo>#<n>:<section>".
    open: HashMap<String, bool>,
    threads_open: HashSet<String>,
    pub show_all_comments: bool,
    /// An action in progress ("Re-running failed checks…").
    pub busy: Option<String>,
    /// The thread (or "pr" for the conversation) being replied to, and its input.
    pub reply: Option<(String, Entity<TextInput>)>,
    /// Debug screenshots: open every section ("all") or one, with its comments.
    pub expand: Option<String>,
    /// The Files tab.
    pub files: crate::files_ui::FilesUi,
}

/// The active local session's pull request, as Glasscode found it.
struct Target {
    repo: String,
    number: u64,
    worktree: PathBuf,
}

fn ago(stamp: &str) -> String {
    let Some(then) = github::parse_time(stamp) else { return String::new() };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(then);
    let s = (now - then).max(0);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

fn ago_unix(at: u64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(at);
    match now.saturating_sub(at) {
        0..=59 => "just now".into(),
        s @ 60..=3599 => format!("{} min ago", s / 60),
        s @ 3600..=86399 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86400),
    }
}

fn duration(start: &str, end: &str) -> String {
    match (github::parse_time(start), github::parse_time(end)) {
        (Some(a), Some(b)) if b >= a => {
            let s = b - a;
            if s >= 60 { format!("{}m {:02}s", s / 60, s % 60) } else { format!("{s}s") }
        }
        _ => String::new(),
    }
}

/// Comment text for the panel: no HTML comments, no runs of blank lines, a sane length.
fn clean_body(body: &str, max: usize) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        rest = rest[start..].find("-->").map(|end| &rest[start + end + 3..]).unwrap_or("");
    }
    out.push_str(rest);
    // Light markdown: headings, emphasis and quote markers read as plain text here.
    let out: String = out
        .lines()
        .map(|line| {
            let t = line.trim_start();
            let t = t.trim_start_matches('#').trim_start_matches('>');
            let t = if line.trim_start() != t { t.trim_start() } else { line };
            t.replace("**", "").replace("__", "")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut lines: Vec<&str> = Vec::new();
    for line in out.lines().map(str::trim_end) {
        if line.trim().is_empty() && lines.last().is_none_or(|l| l.trim().is_empty()) {
            continue;
        }
        lines.push(line);
    }
    let text = lines.join("\n").trim().to_string();
    if text.chars().count() > max { format!("{}…", text.chars().take(max).collect::<String>().trim_end()) } else { text }
}

fn first_line(body: &str) -> String {
    clean_body(body, 400).lines().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(140).collect()
}

fn initials(login: &str) -> String {
    let parts: Vec<&str> = login.split(['-', '_', '.']).filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [a, b, ..] => format!("{}{}", a.chars().next().unwrap_or(' '), b.chars().next().unwrap_or(' ')).to_uppercase(),
        [a] => a.chars().take(2).collect::<String>().to_uppercase(),
        [] => "?".into(),
    }
}

fn pill(text: impl Into<SharedString>, fg: Hsla, bg: Hsla) -> gpui::Div {
    div().flex_none().px_2().py_0p5().rounded_full().bg(bg).text_color(fg).text_size(theme::sz(12.)).font_weight(FontWeight::SEMIBOLD).child(text.into())
}

fn red_bg() -> Hsla {
    theme::error_bg()
}
fn green_bg() -> Hsla {
    theme::success_bg()
}
fn amber_bg() -> Hsla {
    theme::warn_bg()
}
fn blue() -> Hsla {
    theme::codex()
}
fn blue_bg() -> Hsla {
    theme::info_bg()
}

/// The dashed box a switch and what Glasscode does by itself sit in.
fn auto_box(border: Hsla) -> gpui::Div {
    div().flex().items_start().gap_2p5().mt_2p5().px_3().py_2p5().rounded_lg().border_1().border_dashed().border_color(border)
}

impl Workspace {
    fn gh_target(&self) -> Option<Target> {
        if self.center != Center::Session {
            return None;
        }
        let session = self.active_session()?;
        let status = self.prs.map.get(&session.id)?;
        Some(Target { repo: status.repo.clone()?, number: status.pr.as_ref()?.number, worktree: session.worktree.clone() })
    }

    /// Every tick: keep the shown PR fresh while the Git tab is open (every 30 s).
    pub fn gh_tick(&mut self, cx: &mut Context<Self>) {
        // Debug builds: GLASSCODE_DEBUG_RIGHT_TAB=pr|changes|files opens that tab once;
        // files:changed shows only changed files, files:<path> reveals that file.
        if cfg!(debug_assertions) {
            static DONE: std::sync::Once = std::sync::Once::new();
            let mut wanted = None;
            DONE.call_once(|| wanted = std::env::var("GLASSCODE_DEBUG_RIGHT_TAB").ok());
            if let Some(wanted) = wanted {
                match wanted.split_once(':') {
                    Some(("files", "changed")) => {
                        self.right_tab = RightTab::Files;
                        self.gh.files.debug_changed_only();
                    }
                    Some(("files", path)) => {
                        self.right_tab = RightTab::Files;
                        self.files_select(path.to_string(), cx);
                    }
                    _ => {
                        self.right_tab = match wanted.as_str() {
                            "changes" => RightTab::Changes,
                            "files" => RightTab::Files,
                            _ => self.right_tab,
                        }
                    }
                }
            }
        }
        self.files_tick(cx);
        if !self.show_right || !matches!(self.right_tab, RightTab::PullRequest) {
            return;
        }
        let Some(target) = self.gh_target() else { return };
        let key = (target.repo.clone(), target.number);
        let stale = self.gh.fetched.get(&key).is_none_or(|t| t.elapsed() > Duration::from_secs(30));
        if stale && !self.gh.loading.contains(&key) {
            self.gh_fetch(cx);
        }
    }

    pub fn gh_fetch(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.gh_target() else { return };
        let key = (target.repo.clone(), target.number);
        if !self.gh.loading.insert(key.clone()) {
            return;
        }
        self.gh.fetched.insert(key.clone(), Instant::now());
        let env = (*self.env).clone();
        let task = cx.background_spawn(async move {
            let preview = PreviewConfig::load(&target.worktree);
            github::pr_details(&target.repo, target.number, &preview, &env)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.gh.loading.remove(&key);
                match result {
                    Ok(details) => {
                        this.gh.errors.remove(&key);
                        this.gh.details.insert(key, details);
                    }
                    Err(err) => {
                        this.gh.errors.insert(key, format!("{err:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Run a GitHub action in the background, then refresh the PR.
    fn gh_action(&mut self, busy: &str, done: &'static str, action: impl FnOnce(&HashMap<String, String>) -> anyhow::Result<()> + Send + 'static, cx: &mut Context<Self>) {
        if self.gh.busy.is_some() {
            return;
        }
        self.gh.busy = Some(busy.to_string());
        let env = (*self.env).clone();
        let task = cx.background_spawn(async move { action(&env) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.gh.busy = None;
                match result {
                    Ok(()) => this.show_toast(done, cx),
                    Err(err) => this.show_toast(format!("GitHub: {err:#}"), cx),
                }
                if this.gh_target().is_some() {
                    this.gh_fetch(cx);
                } else {
                    // A new PR: find it for the session.
                    this.pr_refresh(cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn gh_is_open(&self, key: &PrKey, section: &str, default: bool) -> bool {
        self.gh.expand.as_deref().is_some_and(|e| e == section || (e == "all" && section != "passed")) || self.gh.open.get(&format!("{}#{}:{section}", key.0, key.1)).copied().unwrap_or(default)
    }

    fn gh_toggle(&mut self, key: &PrKey, section: &str, current: bool, cx: &mut Context<Self>) {
        self.gh.open.insert(format!("{}#{}:{section}", key.0, key.1), !current);
        cx.notify();
    }

    /// Type a message into the session's agent and submit it.
    fn gh_send_to_agent(&mut self, session_id: &str, message: String, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminals.get(session_id).cloned() else {
            self.show_toast("Open the session first; its agent is not running.", cx);
            return;
        };
        // Typing into an open question would answer it (its options take number keys).
        if glasscode_core::prompt::parse(&terminal.read(cx).text_tail(60)).is_some() {
            self.show_toast("The agent is asking something in the terminal. Answer it first, then try again.", cx);
            return;
        }
        let line = message.split_whitespace().collect::<Vec<_>>().join(" ");
        terminal.update(cx, |t, cx| t.write(line.into_bytes(), cx));
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(Duration::from_millis(200)).await;
            let _ = terminal.update(cx, |t, cx| t.write(b"\r".to_vec(), cx));
        })
        .detach();
        self.show_toast("Sent to the agent.", cx);
    }

    fn gh_start_reply(&mut self, target: String, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| TextInput::new("Write a reply…", cx));
        cx.subscribe_in(&input, window, |this: &mut Workspace, _, event, _window, cx| match event {
            InputEvent::Submit => this.gh_send_reply(cx),
            InputEvent::Cancel => {
                this.gh.reply = None;
                cx.notify();
            }
            InputEvent::Changed => {}
        })
        .detach();
        crate::text_input::focus(&input, window, cx);
        self.gh.reply = Some((target, input));
        cx.notify();
    }

    fn gh_send_reply(&mut self, cx: &mut Context<Self>) {
        let Some((target, input)) = self.gh.reply.clone() else { return };
        let body = input.read(cx).text().trim().to_string();
        if body.is_empty() {
            return;
        }
        let Some(pr) = self.gh_target() else { return };
        self.gh.reply = None;
        if target == "pr" {
            self.gh_action("Commenting…", "Comment posted.", move |env| github::comment(&pr.repo, pr.number, &body, env), cx);
        } else {
            self.gh_action("Replying…", "Reply posted.", move |env| github::reply_to_thread(&pr.repo, &target, &body, env), cx);
        }
    }

    fn gh_open_draft(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.active_session().cloned() else { return };
        let branch = glasscode_core::git::current_branch(&session.worktree);
        let base = glasscode_core::git::default_branch(&session.worktree);
        let dir = session.worktree.clone();
        self.gh_action("Pushing and opening a draft PR…", "Draft pull request opened.", move |env| github::push_and_open_draft(&dir, &branch, &base, env), cx);
    }
}

// ----- rendering -------------------------------------------------------------------------------

impl Workspace {
    /// The pull request block at the top of the Git tab (nothing when the repo is not on GitHub).
    pub fn render_pr_section(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.active_session()?;
        let status = self.prs.map.get(&session.id);
        let Some(status) = status else {
            return Some(div().p_4().text_size(theme::sz(13.)).text_color(theme::muted()).child("Looking for this branch's pull request…").into_any_element());
        };
        status.repo.as_ref()?;
        let Some(pr) = status.pr.as_ref() else {
            // No PR yet for this branch.
            let busy = self.gh.busy.clone();
            let account = self.github_account_for(&session.project_id).map(|a| format!(", as {a}")).unwrap_or_default();
            return Some(
                div()
                    .p_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .p_4()
                            .rounded(theme::sz(14.))
                            .border_1()
                            .border_color(theme::border())
                            .bg(theme::raised())
                            .shadow_sm()
                            .child(div().text_color(theme::text_strong()).font_weight(FontWeight::SEMIBOLD).child(format!("No pull request for {}", status.branch)))
                            .child(div().text_size(theme::sz(13.)).text_color(theme::muted()).child(format!("Pushes the branch and opens a draft pull request titled from its commits{account}.")))
                            .child(div().flex().mt_1().child(match busy {
                                Some(text) => ui::disabled_button("gh-open-draft", text).into_any_element(),
                                None => ui::primary_button("gh-open-draft", "Push and open draft PR")
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.gh_open_draft(cx)))
                                    .into_any_element(),
                            })),
                    )
                    .into_any_element(),
            );
        };
        let repo = status.repo.clone().unwrap_or_default();
        let key: PrKey = (repo.clone(), pr.number);
        let details = self.gh.details.get(&key);
        let loading = self.gh.loading.contains(&key);
        let updated = if loading { "updating…".to_string() } else { self.gh.fetched.get(&key).map(|t| format!("updated {}", if t.elapsed().as_secs() < 60 { "just now".into() } else { format!("{} min ago", t.elapsed().as_secs() / 60) })).unwrap_or_default() };
        let refresh = div()
            .id("gh-refresh")
            .text_size(theme::sz(11.5))
            .text_color(theme::faint())
            .cursor_pointer()
            .hover(|s| s.text_color(theme::text_strong()))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.gh_fetch(cx)))
            .child(if updated.is_empty() { "refresh".to_string() } else { format!("{updated} · refresh") });
        let mut body = div().flex().flex_col().gap_2().px_3().pt_3();
        let Some(d) = details else {
            let text = self.gh.errors.get(&key).cloned().unwrap_or_else(|| format!("Loading #{}…", pr.number));
            return Some(body.child(div().text_size(theme::sz(13.)).text_color(theme::muted()).child(text)).into_any_element());
        };

        // --- the card: which PR, its state, and four answers at a glance ---
        let (state_label, state_fg, state_bg) = match (d.state.as_str(), d.is_draft) {
            ("MERGED", _) => ("Merged", theme::violet(), theme::violet_bg()),
            ("CLOSED", _) => ("Closed", theme::removed(), red_bg()),
            (_, true) => ("Draft", theme::muted(), theme::selected()),
            _ => ("Open", theme::added(), green_bg()),
        };
        let failed: Vec<&github::CheckRun> = d.checks.iter().filter(|c| c.state() == CheckState::Failed).collect();
        let running = d.checks.iter().filter(|c| c.state() == CheckState::Running).count();
        let passed = d.checks.iter().filter(|c| c.state() == CheckState::Passed).count();
        let unresolved: Vec<&ReviewThread> = d.threads.iter().filter(|t| !t.resolved).collect();
        let approvals = d.reviewers.iter().filter(|r| r.state == "APPROVED").count();
        let skipped = d.checks.iter().filter(|c| c.state() == CheckState::Skipped).count();
        let ci = if !failed.is_empty() {
            (format!("CI: {} failing", failed.len()), theme::removed(), red_bg())
        } else if running > 0 {
            (format!("CI: {running} running"), blue(), blue_bg())
        } else if passed > 0 {
            (format!("CI: {passed} passed"), theme::added(), green_bg())
        } else if d.merge_state == "DIRTY" {
            // GitHub runs no pull_request workflows while the branch conflicts with its base.
            ("CI: blocked by conflicts".to_string(), theme::modified(), amber_bg())
        } else if skipped > 0 {
            (format!("CI: {skipped} skipped"), theme::muted(), theme::selected())
        } else {
            ("CI: no checks".to_string(), theme::muted(), theme::selected())
        };
        let review = match d.review_decision.as_deref() {
            Some("APPROVED") => (format!("Approved ({approvals})"), theme::added(), green_bg()),
            Some("CHANGES_REQUESTED") => ("Changes requested".to_string(), theme::removed(), red_bg()),
            _ => ("Review needed".to_string(), theme::modified(), amber_bg()),
        };
        let preview = match (d.preview.labeled, d.preview.state.as_deref()) {
            _ if !d.preview.available => None,
            (false, _) => Some(("No preview".to_string(), theme::muted(), theme::selected())),
            (true, Some("success")) => Some(("Preview ready".to_string(), theme::added(), green_bg())),
            (true, Some("failure" | "error")) => Some(("Preview failed".to_string(), theme::removed(), red_bg())),
            (true, _) => Some(("Preview deploying".to_string(), blue(), blue_bg())),
        };
        // A review bot's verdict, if one keeps a managed review comment on the PR.
        let verdict = d.verdict.as_ref().map(|v| {
            let (fg, bg) = if v.approved { (theme::added(), green_bg()) } else { (theme::removed(), red_bg()) };
            let label = if v.version > 0 { format!("Review: {} v{}", v.verdict, v.version) } else { format!("Review: {}", v.verdict) };
            (label, fg, bg)
        });
        let chip = |(text, fg, bg): (String, Hsla, Hsla)| div().flex().items_center().px_2p5().py_2().rounded_lg().bg(bg).text_color(fg).text_size(theme::sz(13.)).font_weight(FontWeight::SEMIBOLD).overflow_hidden().whitespace_nowrap().text_ellipsis().child(text);
        let url = d.url.clone();
        let mut meta: Vec<AnyElement> = vec![div().child(format!("{} → {}", d.head, d.base)).into_any_element()];
        match d.merge_state.as_str() {
            "DIRTY" => meta.push(div().text_color(theme::removed()).child("conflicts with base").into_any_element()),
            "BEHIND" => meta.push(div().text_color(theme::modified()).child(format!("behind {}", d.base)).into_any_element()),
            _ => {}
        }
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_3p5()
                .rounded(theme::sz(14.))
                .border_1()
                .border_color(theme::border())
                .bg(theme::raised())
                .shadow_sm()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(format!("#{}", d.number)))
                        .child(pill(state_label, state_fg, state_bg))
                        .child(div().flex_1())
                        .child(refresh)
                        .child(div().id("gh-open").text_size(theme::sz(13.)).text_color(theme::text_strong()).underline().cursor_pointer().on_click(cx.listener(move |_, _: &ClickEvent, _, _| open_url(&url))).child("GitHub ↗")),
                )
                .child(div().text_size(theme::sz(15.)).line_height(theme::sz(20.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(d.title.clone()))
                .child(div().flex().flex_wrap().gap_x_2().text_size(theme::sz(12.5)).text_color(theme::muted()).children(meta))
                .child({
                    // Two answers per row: CI and reviews, then the review verdict and the preview.
                    let chips: Vec<(String, Hsla, Hsla)> = [Some(ci), Some(review), verdict, preview].into_iter().flatten().collect();
                    div().flex().flex_col().gap_1p5().children(chips.chunks(2).map(|pair| {
                        div()
                            .flex()
                            .gap_1p5()
                            .children(pair.iter().cloned().map(|c| chip(c).flex_1().min_w_0().into_any_element()))
                            .when(pair.len() == 1, |d| d.child(div().flex_1()))
                    }))
                }),
        );

        // --- sections: open by themselves only when they need you ---
        let section = |id: &'static str, open: bool, title: &'static str, summary: String, color: Hsla| {
            div()
                .id(SharedString::from(format!("gh-section-{id}")))
                .flex()
                .items_baseline()
                .gap_2()
                .pt_3()
                .pb_1()
                .cursor_pointer()
                .child(div().text_size(theme::sz(12.)).font_weight(FontWeight::MEDIUM).text_color(theme::muted()).child(title))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(theme::sz(13.)).text_color(color).child(summary))
                .child(div().text_size(theme::sz(10.)).text_color(theme::faint()).child(if open { "▾" } else { "▸" }))
        };
        let row = || div().flex().items_center().gap_2p5().px_3().py_2().rounded_full().border_1().border_color(theme::border()).bg(theme::raised());

        // Checks.
        let conflicting = d.mergeable == "CONFLICTING";
        let checks_open = self.gh_is_open(&key, "checks", !failed.is_empty() || running > 0 || conflicting);
        let checks_summary = {
            let mut parts = Vec::new();
            if !failed.is_empty() {
                parts.push(format!("{} failing", failed.len()));
            }
            if running > 0 {
                parts.push(format!("{running} running"));
            }
            if passed > 0 {
                parts.push(format!("{passed} passed"));
            }
            if skipped > 0 {
                parts.push(format!("{skipped} skipped"));
            }
            if passed + running + failed.len() == 0 && d.merge_state == "DIRTY" {
                parts.push(format!("not run until the conflicts with {} are resolved", d.base));
            }
            if parts.is_empty() {
                parts.push("none".into());
            }
            parts.join(" · ")
        };
        let k = key.clone();
        body = body.child(section("checks", checks_open, "Checks", checks_summary, if failed.is_empty() { theme::muted() } else { theme::removed() }).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.gh_toggle(&k, "checks", checks_open, cx))));
        if checks_open {
            let mut rows: Vec<AnyElement> = Vec::new();
            let shown: Vec<&github::CheckRun> = d.checks.iter().filter(|c| matches!(c.state(), CheckState::Failed | CheckState::Running)).collect();
            for (i, check) in shown.iter().enumerate() {
                let failing = check.state() == CheckState::Failed;
                let link = check.url.clone();
                let detail = if failing {
                    format!("{} {}", check.conclusion.to_lowercase().replace('_', " "), duration(&check.started_at, &check.completed_at)).trim().to_string()
                } else {
                    format!("running {}", ago(&check.started_at).trim_end_matches(" ago"))
                };
                rows.push(
                    row()
                        .child(div().w(theme::sz(14.)).text_color(if failing { theme::removed() } else { blue() }).child(if failing { "✕" } else { "◌" }))
                        .child(div().flex().flex_col().flex_1().min_w_0().child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme::text_strong()).child(check.label())).child(div().text_size(theme::sz(11.5)).text_color(theme::muted()).child(detail)))
                        .when(!link.is_empty(), |r| r.child(div().id(SharedString::from(format!("gh-log-{i}"))).text_size(theme::sz(13.)).text_color(theme::text_strong()).underline().cursor_pointer().on_click(cx.listener(move |_, _: &ClickEvent, _, _| open_url(&link))).child("Logs")))
                        .into_any_element(),
                );
            }
            // All green: the list itself. Otherwise the passed ones fold under the problems.
            // The passed ones fold behind one row (all green or not), so what needs you stays near.
            let passed_open = self.gh_is_open(&key, "passed", false);
            if passed > 0 {
                let k = key.clone();
                rows.push(
                    div()
                        .id("gh-passed")
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2p5()
                        .py_1p5()
                        .rounded_md()
                        .border_1()
                        .border_color(theme::border())
                        .cursor_pointer()
                        .text_size(theme::sz(12.))
                        .text_color(theme::muted())
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.gh_toggle(&k, "passed", passed_open, cx)))
                        .child(div().w(theme::sz(14.)).text_color(theme::added()).child("✓"))
                        .child(div().flex_1().child(format!("{passed} passed")))
                        .child(if passed_open { "Hide" } else { "Show" })
                        .into_any_element(),
                );
            }
            if passed > 0 && passed_open {
                rows.push(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .pl(theme::sz(34.))
                        .text_size(theme::sz(12.))
                        .text_color(theme::muted())
                        .children(d.checks.iter().filter(|c| c.state() == CheckState::Passed).take(60).map(|c| {
                            div()
                                .flex()
                                .gap_2()
                                .child(div().flex_none().text_color(theme::added()).child("✓"))
                                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(c.label()))
                                .child(div().flex_none().text_color(theme::faint()).child(duration(&c.started_at, &c.completed_at)))
                                .into_any_element()
                        }))
                        .into_any_element(),
                );
            }
            if !failed.is_empty() {
                let runs: Vec<u64> = {
                    let mut ids: Vec<u64> = failed.iter().filter_map(|c| c.run_id()).collect();
                    ids.sort();
                    ids.dedup();
                    ids
                };
                let message = format!(
                    "CI is failing on PR #{} ({}): {}. Read the failing logs with `gh run view <run-id> --log-failed` and fix it.",
                    d.number,
                    d.url,
                    failed.iter().map(|c| format!("{} ({})", c.label(), c.url)).collect::<Vec<_>>().join("; ")
                );
                let repo_rerun = repo.clone();
                let session_id = session.id.clone();
                let busy = self.gh.busy.is_some();
                let has_agent = session.agent != AgentKind::Shell;
                rows.push(
                    div()
                        .flex()
                        .gap_2()
                        .mt_1()
                        .when(!runs.is_empty(), |d| {
                            d.child(if busy {
                                ui::disabled_button("gh-rerun", "Re-run failed").into_any_element()
                            } else {
                                ui::button("gh-rerun", "Re-run failed")
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        let (repo, runs) = (repo_rerun.clone(), runs.clone());
                                        this.gh_action("Re-running failed checks…", "Failed checks are running again.", move |env| github::rerun_failed(&repo, &runs, env), cx)
                                    }))
                                    .into_any_element()
                            })
                        })
                        .when(has_agent, |d| d.child(ui::button("gh-fix", "Ask agent to fix").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.gh_send_to_agent(&session_id, message.clone(), cx)))))
                        .into_any_element(),
                );
            }
            body = body.child(div().flex().flex_col().gap_1().children(rows));
        }
        // What Glasscode does by itself about CI (crate::ci_fix), with its switch.
        {
            use crate::ci_fix::Note;
            let on = !self.state.ci_autofix_off;
            let (text, color, can_switch) = if session.review.is_some() {
                ("Review session: this pull request is someone else's, so Glasscode leaves its CI to them.".to_string(), theme::muted(), false)
            } else if session.agent == AgentKind::Shell {
                ("A shell has no agent, so Glasscode does not ask about CI here.".to_string(), theme::muted(), false)
            } else {
                let (text, color) = match self.ci_fix_note(&session.id) {
                    _ if !on => ("Off: Glasscode does not ask the agent to fix failing CI by itself.".to_string(), theme::muted()),
                    Some(Note::Asked { at, tries }) => (format!("Glasscode asked the agent to fix the failing checks {} (ask {tries} of 3).", ago_unix(at)), theme::accent()),
                    Some(Note::GaveUp) => ("The agent tried 3 times and CI still fails: over to you. Glasscode asks again once CI has passed.".to_string(), theme::removed()),
                    Some(Note::WaitingForCi) => ("Glasscode asks the agent to fix it when the other checks finish.".to_string(), theme::muted()),
                    Some(Note::WaitingForConflicts) => ("Glasscode asks about CI once the conflicts are resolved (that makes a new commit).".to_string(), theme::muted()),
                    Some(Note::WaitingForAgent) => ("Glasscode asks the agent to fix it as soon as its turn is over.".to_string(), theme::muted()),
                    Some(Note::WaitingForSession) => ("Glasscode asks the agent to fix it when this session is open.".to_string(), theme::muted()),
                    _ => ("If CI fails on a new commit, Glasscode asks this agent to fix it.".to_string(), theme::muted()),
                };
                (text, color, true)
            };
            body = body.child(
                auto_box(theme::border_strong())
                    .when(can_switch, |d| {
                        d.child(ui::switch("gh-autofix", on).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.state.ci_autofix_off = on;
                            this.save();
                            cx.notify();
                        })))
                    })
                    .child(div().flex_1().min_w_0().text_size(theme::sz(13.)).text_color(color).child(text)),
            );
        }
        // Conflicts: Glasscode asks the agent to resolve them by itself too (crate::ci_fix).
        if let Some(note) = self.conflict_fix_note(&session.id).filter(|_| conflicting) {
            use crate::ci_fix::Note;
            let base = d.base.clone();
            let (text, color) = match note {
                Note::Off => ("Glasscode does not ask the agent to resolve them by itself (off).".to_string(), theme::muted()),
                Note::Asked { at, tries } => (format!("Glasscode asked the agent to rebase and resolve them {} (ask {tries} of 3). If {base} moves on and they come back, it asks again.", ago_unix(at)), theme::text()),
                Note::GaveUp => (format!("The agent was asked 3 times and it still conflicts: over to you. Glasscode asks again once it merges cleanly with {base}."), theme::removed()),
                Note::WaitingForSession => ("Glasscode asks the agent to resolve them when this session is open.".to_string(), theme::muted()),
                _ => ("Glasscode asks the agent to resolve them as soon as its turn is over.".to_string(), theme::muted()),
            };
            let on = !self.state.conflict_autofix_off;
            body = body.child(
                auto_box(theme::removed())
                    .child(ui::switch("gh-conflictfix", on).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.state.conflict_autofix_off = on;
                        this.save();
                        cx.notify();
                    })))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(theme::sz(13.))
                            .text_color(color)
                            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::removed()).child(format!("Conflicts with {base}.")))
                            .child(text),
                    ),
            );
        }

        // Preview.
        let preview_state = d.preview.state.clone().unwrap_or_default();
        let preview_needs = d.preview.labeled && preview_state != "success";
        let preview_open = self.gh_is_open(&key, "preview", preview_needs);
        let preview_summary = match (d.preview.labeled, preview_state.as_str()) {
            (true, "success") => format!("ready{}", d.preview.sha.as_ref().map(|s| format!(" · {s}")).unwrap_or_default()),
            (true, "failure" | "error") => "deploy failed".into(),
            (true, _) => "deploying".into(),
            (false, _) => "none".into(),
        };
        let preview_color = match (d.preview.labeled, preview_state.as_str()) {
            (true, "success") => theme::added(),
            (true, "failure" | "error") => theme::removed(),
            (true, _) => blue(),
            _ => theme::muted(),
        };
        let k = key.clone();
        if d.preview.available {
            body = body.child(section("preview", preview_open, "Preview", preview_summary, preview_color).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.gh_toggle(&k, "preview", preview_open, cx))));
        }
        if d.preview.available && preview_open {
            let cfg = PreviewConfig::load(&session.worktree);
            let mut items: Vec<AnyElement> = Vec::new();
            if d.preview.labeled {
                let links: Vec<(String, String)> = if d.preview.links.is_empty() {
                    d.preview.url.clone().map(|u| vec![("Web".to_string(), u)]).unwrap_or_default()
                } else {
                    d.preview.links.clone()
                };
                for (i, (label, url)) in links.into_iter().enumerate() {
                    let open = url.clone();
                    items.push(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().w(theme::sz(70.)).text_size(theme::sz(12.)).text_color(theme::muted()).child(label))
                            .child(
                                div()
                                    .id(SharedString::from(format!("gh-preview-{i}")))
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .font_family(theme::MONO_FONT)
                                    .text_size(theme::sz(12.))
                                    .text_color(theme::link())
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |_, _: &ClickEvent, _, _| open_url(&open)))
                                    .child(url.trim_start_matches("https://").to_string()),
                            )
                            .into_any_element(),
                    );
                }
                if items.is_empty() {
                    items.push(div().text_size(theme::sz(12.)).text_color(theme::muted()).child("The next CI run builds and deploys it; the links appear here when it is ready.").into_any_element());
                }
                let busy = self.gh.busy.is_some();
                let (repo_a, repo_b, label, redeploy) = (repo.clone(), repo.clone(), cfg.label.clone(), cfg.redeploy_comment.clone());
                let number = d.number;
                items.push(
                    div()
                        .flex()
                        .gap_2()
                        .mt_1()
                        .child(if busy {
                            ui::disabled_button("gh-redeploy", "Redeploy fresh").into_any_element()
                        } else {
                            ui::button("gh-redeploy", "Redeploy fresh")
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    let (repo, body) = (repo_a.clone(), redeploy.clone());
                                    this.gh_action("Asking for a fresh preview…", "Redeploy requested.", move |env| github::comment(&repo, number, &body, env), cx)
                                }))
                                .into_any_element()
                        })
                        .child(
                            ui::button("gh-teardown", "Tear down")
                                .text_color(theme::removed())
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    let (repo, label) = (repo_b.clone(), label.clone());
                                    this.gh_action("Tearing the preview down…", "Preview label removed.", move |env| github::set_label(&repo, number, &label, false, env), cx)
                                })),
                        )
                        .into_any_element(),
                );
                items.push(div().text_size(theme::sz(11.5)).text_color(theme::faint()).child(format!("Redeploy fresh comments \"{}\" (wipes the preview's data and rebuilds). Tear down removes the {} label.", cfg.redeploy_comment, cfg.label)).into_any_element());
            } else {
                let repo_add = repo.clone();
                let label = cfg.label.clone();
                let number = d.number;
                items.push(div().text_size(theme::sz(12.)).text_color(theme::muted()).child(format!("Adding one puts the {} label on the PR; CI builds the changed services and deploys them.", cfg.label)).into_any_element());
                items.push(
                    div()
                        .flex()
                        .child(if self.gh.busy.is_some() {
                            ui::disabled_button("gh-add-preview", "Add preview").into_any_element()
                        } else {
                            ui::primary_button("gh-add-preview", "Add preview")
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    let (repo, label) = (repo_add.clone(), label.clone());
                                    this.gh_action("Adding the preview label…", "Preview requested; CI deploys it.", move |env| github::set_label(&repo, number, &label, true, env), cx)
                                }))
                                .into_any_element()
                        })
                        .into_any_element(),
                );
            }
            body = body.child(div().flex().flex_col().gap_2().p_3().rounded_lg().border_1().border_color(theme::border()).bg(theme::raised()).children(items));
        }

        // Reviews.
        let changes_requested = d.review_decision.as_deref() == Some("CHANGES_REQUESTED");
        let reviews_open = self.gh_is_open(&key, "reviews", changes_requested);
        let reviews_summary = match d.review_decision.as_deref() {
            Some("APPROVED") => format!("approved · {approvals} approval{}", if approvals == 1 { "" } else { "s" }),
            Some("CHANGES_REQUESTED") => "changes requested".into(),
            _ => format!("review needed · {approvals} approval{}", if approvals == 1 { "" } else { "s" }),
        };
        let k = key.clone();
        body = body.child(section("reviews", reviews_open, "Reviews", reviews_summary, if changes_requested { theme::removed() } else { theme::muted() }).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.gh_toggle(&k, "reviews", reviews_open, cx))));
        if reviews_open {
            let rows: Vec<AnyElement> = if d.reviewers.is_empty() {
                vec![div().text_size(theme::sz(12.)).text_color(theme::muted()).child("No reviews or review requests yet.").into_any_element()]
            } else {
                d.reviewers
                    .iter()
                    .map(|r| {
                        let (text, color) = match r.state.as_str() {
                            "APPROVED" => (format!("approved {}", ago(&r.submitted_at)), theme::added()),
                            "CHANGES_REQUESTED" => (format!("requested changes {}", ago(&r.submitted_at)), theme::removed()),
                            "COMMENTED" => (format!("commented {}", ago(&r.submitted_at)), theme::muted()),
                            "DISMISSED" => ("review dismissed".to_string(), theme::faint()),
                            _ => ("review requested".to_string(), theme::modified()),
                        };
                        row()
                            .child(div().size(theme::sz(24.)).flex_none().flex().items_center().justify_center().rounded_full().bg(theme::selected()).text_size(theme::sz(10.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_soft()).child(initials(&r.login)))
                            .child(div().flex().flex_col().flex_1().min_w_0().child(div().text_color(theme::text_strong()).child(r.login.clone())).child(div().text_size(theme::sz(11.5)).text_color(color).child(text)))
                            .into_any_element()
                    })
                    .collect()
            };
            body = body.child(div().flex().flex_col().gap_1().children(rows));
        }

        // Comments: unresolved file threads and people's comments first; bots and resolved behind "all".
        let people: Vec<&github::IssueComment> = d.comments.iter().filter(|c| !c.is_bot).collect();
        let total = d.threads.len() + d.comments.len();
        let comments_open = self.gh_is_open(&key, "comments", !unresolved.is_empty());
        let comments_summary = if total == 0 {
            "none".to_string()
        } else if unresolved.is_empty() {
            format!("{total} · nothing unresolved")
        } else {
            format!("{} unresolved · {total} in all", unresolved.len())
        };
        let k = key.clone();
        body = body.child(section("comments", comments_open, "Comments", comments_summary, if unresolved.is_empty() { theme::muted() } else { theme::modified() }).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.gh_toggle(&k, "comments", comments_open, cx))));
        if comments_open {
            let mut items: Vec<AnyElement> = Vec::new();
            let all = self.gh.show_all_comments;
            for thread in d.threads.iter().filter(|t| all || !t.resolved) {
                items.push(self.render_thread(thread, (session.agent != AgentKind::Shell).then_some(session.id.as_str()), cx));
            }
            let conversation: Vec<&github::IssueComment> = if all { d.comments.iter().collect() } else { people.clone() };
            for comment in conversation {
                items.push(self.render_issue_comment(comment, cx));
            }
            let hidden = d.threads.iter().filter(|t| t.resolved).count() + d.comments.iter().filter(|c| c.is_bot).count();
            if hidden > 0 || all {
                items.push(
                    div()
                        .id("gh-all-comments")
                        .text_size(theme::sz(12.))
                        .text_color(theme::link())
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.gh.show_all_comments = !this.gh.show_all_comments;
                            cx.notify();
                        }))
                        .child(if all { "Hide resolved and bot comments".to_string() } else { format!("Show {hidden} resolved and bot comments") })
                        .into_any_element(),
                );
            }
            let replying_pr = self.gh.reply.as_ref().filter(|(t, _)| t == "pr").map(|(_, i)| i.clone());
            items.push(match replying_pr {
                Some(input) => div()
                    .flex()
                    .gap_2()
                    .child(div().flex_1().child(input))
                    .child(ui::primary_button("gh-comment-send", "Comment").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.gh_send_reply(cx))))
                    .into_any_element(),
                None => div()
                    .flex()
                    .child(ui::button("gh-comment", "Comment on the PR").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.gh_start_reply("pr".into(), window, cx))))
                    .into_any_element(),
            });
            body = body.child(div().flex().flex_col().gap_1p5().children(items));
        }
        if let Some(busy) = &self.gh.busy {
            body = body.child(div().text_size(theme::sz(12.)).text_color(theme::text_soft()).child(busy.clone()));
        }
        Some(body.into_any_element())
    }

    /// One file comment thread; `agent` is the session to send it to (none for a shell).
    fn render_thread(&self, thread: &ReviewThread, agent: Option<&str>, cx: &mut Context<Self>) -> AnyElement {
        let id = thread.id.clone();
        let open = self.gh.expand.is_some() || self.gh.threads_open.contains(&id);
        let first = thread.comments.first();
        let location = format!("{}{}", thread.path, thread.line.map(|l| format!(":{l}")).unwrap_or_default());
        let toggle_id = id.clone();
        let header = div()
            .id(SharedString::from(format!("gh-thread-{id}")))
            .flex()
            .items_start()
            .gap_2()
            .px_2p5()
            .py_2()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if !this.gh.threads_open.remove(&toggle_id) {
                    this.gh.threads_open.insert(toggle_id.clone());
                }
                cx.notify();
            }))
            .child(div().w(theme::sz(10.)).text_size(theme::sz(10.)).text_color(theme::faint()).mt(theme::sz(2.)).child(if open { "▾" } else { "▸" }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .gap_1p5()
                            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().font_family(theme::MONO_FONT).text_size(theme::sz(11.)).text_color(theme::muted()).child(location.clone()))
                            .when(thread.resolved, |d| d.child(div().flex_none().text_size(theme::sz(11.)).text_color(theme::added()).child("resolved")))
                            .when(thread.outdated && !thread.resolved, |d| d.child(div().flex_none().text_size(theme::sz(11.)).text_color(theme::faint()).child("outdated"))),
                    )
                    .child(div().text_color(theme::text_strong()).child(first.map(|c| first_line(&c.body)).unwrap_or_default())),
            )
            .child(div().flex_none().text_size(theme::sz(11.)).text_color(theme::faint()).child(thread.comments.len().to_string()));
        let mut card = div().flex().flex_col().rounded_lg().border_1().border_color(theme::border_strong()).bg(theme::raised()).child(header);
        if open {
            let mut inner: Vec<AnyElement> = Vec::new();
            if let Some(hunk) = first.map(|c| c.diff_hunk.clone()).filter(|h| !h.is_empty()) {
                let tail: Vec<&str> = hunk.lines().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
                inner.push(div().p_2().rounded_md().bg(theme::bg()).border_1().border_color(theme::border()).font_family(theme::MONO_FONT).text_size(theme::sz(11.)).text_color(theme::text_soft()).child(tail.join("\n")).into_any_element());
            }
            for c in &thread.comments {
                inner.push(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(div().text_size(theme::sz(12.)).child(div().flex().gap_1().child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_soft()).child(c.author.clone())).child(div().text_color(theme::faint()).child(format!("· {}", ago(&c.created_at))))))
                        .child(div().text_size(theme::sz(12.5)).text_color(theme::text()).child(clean_body(&c.body, 1500)))
                        .into_any_element(),
                );
            }
            let replying = self.gh.reply.as_ref().filter(|(t, _)| t == &id).map(|(_, i)| i.clone());
            let message = format!(
                "Review comment on {location} by {}: {} Please address it.",
                first.map(|c| c.author.clone()).unwrap_or_default(),
                first.map(|c| clean_body(&c.body, 800)).unwrap_or_default()
            );
            let (reply_id, resolve_id) = (id.clone(), id.clone());
            inner.push(match replying {
                Some(input) => div()
                    .flex()
                    .gap_2()
                    .child(div().flex_1().child(input))
                    .child(ui::primary_button("gh-reply-send", "Reply").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.gh_send_reply(cx))))
                    .into_any_element(),
                None => div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .when_some(agent, |d, session_id| {
                        let session_id = session_id.to_string();
                        d.child(ui::button(SharedString::from(format!("gh-agent-{id}")), "Send to agent").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.gh_send_to_agent(&session_id, message.clone(), cx))))
                    })
                    .child(ui::button(SharedString::from(format!("gh-reply-{id}")), "Reply").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.gh_start_reply(reply_id.clone(), window, cx))))
                    .when(!thread.resolved, |d| {
                        d.child(ui::button(SharedString::from(format!("gh-resolve-{id}")), "Resolve").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let thread = resolve_id.clone();
                            let Some(pr) = this.gh_target() else { return };
                            this.gh_action("Resolving…", "Thread resolved.", move |env| github::resolve_thread(&pr.repo, &thread, env), cx)
                        })))
                    })
                    .into_any_element(),
            });
            card = card.child(div().flex().flex_col().gap_2p5().px_3().pb_3().pl(theme::sz(30.)).children(inner));
        }
        card.into_any_element()
    }

    fn render_issue_comment(&self, comment: &github::IssueComment, cx: &mut Context<Self>) -> AnyElement {
        let id = comment.url.clone();
        let open = self.gh.threads_open.contains(&id);
        let toggle_id = id.clone();
        div()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(theme::border())
            .child(
                div()
                    .id(SharedString::from(format!("gh-comment-{id}")))
                    .flex()
                    .items_start()
                    .gap_2()
                    .px_2p5()
                    .py_2()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if !this.gh.threads_open.remove(&toggle_id) {
                            this.gh.threads_open.insert(toggle_id.clone());
                        }
                        cx.notify();
                    }))
                    .child(div().w(theme::sz(10.)).text_size(theme::sz(10.)).text_color(theme::faint()).mt(theme::sz(2.)).child(if open { "▾" } else { "▸" }))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_size(theme::sz(11.5)).text_color(theme::muted()).child(format!("{}{} · {}", comment.author, if comment.is_bot { " (bot)" } else { "" }, ago(&comment.created_at))))
                            .when(!open, |d| d.child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme::text()).child(first_line(&comment.body)))),
                    ),
            )
            .when(open, |d| d.child(div().px_3().pb_3().pl(theme::sz(30.)).text_size(theme::sz(12.5)).text_color(theme::text()).child(clean_body(&comment.body, 3000))))
            .into_any_element()
    }
}
