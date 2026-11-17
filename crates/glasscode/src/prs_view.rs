//! The Pull requests page: the person's own open pull requests ("Mine", `github::my_open_prs`,
//! the list Today keeps in `TodayUi.prs`) and the ones that ask for their review ("Asked of
//! me", the review requests the Board keeps in `TicketsUi.reviews`). The page only reads those
//! two lists; both are asked of `gh` in the background, so the page never waits for it.
//!
//! `GLASSCODE_DEBUG_OPEN=prs[:sample][:light|dark|classic|contrast][:90|100|115|130]` opens it in
//! a debug build; `sample` puts made-up pull requests in the two lists (and links the first of
//! "Mine" to the active session), for a screen check without a GitHub login.

use std::time::{Duration, Instant};

use gpui::{AnyElement, ClickEvent, Context, FontWeight, Hsla, SharedString, Window, div, prelude::*};
use glasscode_core::github::{CheckRun, CheckState, CiStatus, MyPr, PrHit, ReviewRequest};
use glasscode_core::model::ThemeChoice;

use crate::backdrop;
use crate::icons::{self, Brand};
use crate::shell::View;
use crate::theme;
use crate::tickets_ui::Key;
use crate::ui;
use crate::workspace::Workspace;

/// The list of "Mine" is asked again when the page opens and it is older than this, and every
/// `MINE_EVERY` while the page shows (Today's own times).
const MINE_ON_OPEN: Duration = Duration::from_secs(60);
const MINE_EVERY: Duration = Duration::from_secs(5 * 60);
/// The review requests are asked again when the page opens and they are older than this (as
/// the Board does).
const ASKED_ON_OPEN: Duration = Duration::from_secs(30);

pub const NO_GH: &str = "Pull requests show here when the GitHub command (gh) is installed and signed in.";
pub const MINE_LOADING: &str = "Looking for your open pull requests…";
pub const MINE_EMPTY: &str = "You have no open pull requests.";
pub const ASKED_LOADING: &str = "Looking for pull requests that ask for your review…";
pub const ASKED_EMPTY: &str = "No pull request asks for your review.";
pub const ASKED_OFF: &str = "Review requests are off: turn on “Also show pull requests that ask for your review” in Settings › Accounts.";

/// What the checks of a pull request say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checks {
    /// This many failed; 0 when GitHub only says that some fail (a review request).
    Failing(usize),
    Running,
    Passed,
    /// They were read, and it has none.
    Nothing,
    /// Not read: past the newest `github::MY_PR_DETAILS`, GitHub did not answer, or a review
    /// request whose merge state says nothing about them.
    Unknown,
}

/// The one thing a row offers besides opening the pull request on GitHub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// One of the person's own: open the session that made it ("Fix with agent" while a check
    /// fails, else "Open session").
    Session(String),
    /// A review request with a review session: open it.
    OpenReview(String),
    /// A review request: start a review, as the Board's "Start review" does.
    Review,
    /// No session works on it: the row only opens it on GitHub.
    Nothing,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// `owner/name`.
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub draft: bool,
    pub checks: Checks,
    /// "approved", "changes requested", "review required", or empty.
    pub review: &'static str,
    /// What GitHub's merge state says of a review request ("conflicts", "behind its base", …);
    /// empty for the person's own.
    pub merge: &'static str,
    pub updated_at: String,
    pub action: Action,
}

impl Row {
    /// The second line: repository and number, then what the checks and the review say.
    pub fn detail(&self) -> String {
        let mut parts = vec![format!("{} #{}", self.repo, self.number)];
        match self.checks {
            Checks::Failing(0) => parts.push("checks failing".into()),
            Checks::Failing(1) => parts.push("1 check failing".into()),
            Checks::Failing(n) => parts.push(format!("{n} checks failing")),
            Checks::Running => parts.push("checks running".into()),
            Checks::Passed => parts.push("checks passed".into()),
            Checks::Nothing => parts.push("no checks".into()),
            Checks::Unknown => {}
        }
        for words in [self.review, self.merge] {
            if !words.is_empty() {
                parts.push(words.to_string());
            }
        }
        if self.draft {
            parts.push("draft".into());
        }
        parts.join(" · ")
    }

    /// The label of the row's action, or None when it has none.
    pub fn action_label(&self) -> Option<&'static str> {
        match &self.action {
            Action::Session(_) if matches!(self.checks, Checks::Failing(_)) => Some("Fix with agent"),
            Action::Session(_) => Some("Open session"),
            Action::OpenReview(_) => Some("Open review"),
            Action::Review => Some("Review"),
            Action::Nothing => None,
        }
    }
}

/// The checks a pull request's status gives.
pub fn checks_of(ci: Option<&CiStatus>) -> Checks {
    let Some(ci) = ci else { return Checks::Unknown };
    let failed = ci.failed().len();
    if failed > 0 {
        Checks::Failing(failed)
    } else if ci.running() {
        Checks::Running
    } else if ci.checks.iter().any(|c| c.state() == CheckState::Passed) {
        Checks::Passed
    } else {
        Checks::Nothing
    }
}

/// GitHub's review decision in words.
pub fn review_words(decision: &str) -> &'static str {
    match decision {
        "APPROVED" => "approved",
        "CHANGES_REQUESTED" => "changes requested",
        "REVIEW_REQUIRED" => "review required",
        _ => "",
    }
}

/// The person's own open pull requests, the ones that need them first: a failing check, then
/// changes requested, then the rest; newest first in each. `session_of` names the session that
/// works on a pull request (repository, number), if one does.
pub fn mine_rows(prs: &[MyPr], session_of: &dyn Fn(&str, u64) -> Option<String>) -> Vec<Row> {
    let mut rows: Vec<Row> = prs
        .iter()
        .map(|pr| Row {
            repo: pr.repo.clone(),
            number: pr.number,
            title: pr.title.clone(),
            url: pr.url.clone(),
            draft: pr.draft,
            checks: checks_of(pr.ci.as_ref()),
            review: review_words(&pr.review),
            merge: "",
            updated_at: pr.updated_at.clone(),
            action: session_of(&pr.repo, pr.number).map_or(Action::Nothing, Action::Session),
        })
        .collect();
    let need = |row: &Row| match (row.checks, row.review) {
        (Checks::Failing(_), _) => 0,
        (_, "changes requested") => 1,
        _ => 2,
    };
    rows.sort_by(|a, b| need(a).cmp(&need(b)).then_with(|| b.updated_at.cmp(&a.updated_at)));
    rows
}

/// The pull requests that ask for the person's review, newest first. `review_of` names a
/// review session of a pull request, if there is one.
pub fn asked_rows(reviews: &[ReviewRequest], review_of: &dyn Fn(&str, u64) -> Option<String>) -> Vec<Row> {
    let mut rows: Vec<Row> = reviews
        .iter()
        .map(|r| {
            // The search gives no checks, only GitHub's merge state, which says when they fail.
            let (checks, merge) = match r.merge_state.as_str() {
                "CLEAN" => (Checks::Passed, "ready to merge"),
                "UNSTABLE" => (Checks::Failing(0), ""),
                "DIRTY" => (Checks::Unknown, "conflicts"),
                "BEHIND" => (Checks::Unknown, "behind its base"),
                "BLOCKED" => (Checks::Unknown, "blocked"),
                _ => (Checks::Unknown, ""),
            };
            Row {
                repo: r.repo.clone(),
                number: r.hit.number,
                title: crate::tickets_view::tidy_pr_title(&r.hit.title),
                url: r.hit.url.clone(),
                draft: r.hit.draft,
                checks,
                review: review_words(&r.hit.review_decision),
                merge,
                updated_at: r.hit.updated_at.clone(),
                action: review_of(&r.repo, r.hit.number).map_or(Action::Review, Action::OpenReview),
            }
        })
        .collect();
    rows.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    rows
}

/// The sentence of "Mine" when it has no row: still looking, `gh` cannot be used, or none.
pub fn mine_empty(prs: Option<&Result<Vec<MyPr>, String>>) -> &'static str {
    match prs {
        None => MINE_LOADING,
        Some(Err(_)) => NO_GH,
        Some(Ok(_)) => MINE_EMPTY,
    }
}

/// The sentence of "Asked of me" when it has no row.
pub fn asked_empty(off: bool, fetched: bool, failed: bool) -> &'static str {
    match (off, fetched, failed) {
        (true, _, _) => ASKED_OFF,
        (_, false, _) => ASKED_LOADING,
        (_, _, true) => NO_GH,
        _ => ASKED_EMPTY,
    }
}

/// One sentence for the whole page when `gh` gave neither list and nothing is left to show.
pub fn page_notice(mine: Option<&Result<Vec<MyPr>, String>>, asked_failed: bool, asked_rows: usize) -> Option<&'static str> {
    (matches!(mine, Some(Err(_))) && asked_failed && asked_rows == 0).then_some(NO_GH)
}

/// The number on the sidebar's row: the pull requests the page lists.
pub fn count(mine: Option<&Result<Vec<MyPr>, String>>, asked: usize) -> usize {
    mine.and_then(|m| m.as_ref().ok()).map_or(0, Vec::len) + asked
}

/// The page's own state, kept in `TicketsUi` (the Board's data is what it shows).
#[derive(Default)]
pub struct PrsPage {
    debug_done: bool,
    /// Made-up links from a pull request (repository, number) to a session, from the `sample`
    /// hook: the scratch home's sessions have no pull request on GitHub.
    links: Vec<(String, u64, String)>,
}

fn due(at: Option<Instant>, every: Duration) -> bool {
    at.is_none_or(|at| at.elapsed() >= every)
}

/// A glass card of the page (the owner, 2026-11-17: the page stands on the backdrop as Today
/// does). In the light theme its soft shadow gives it the edge the white glass lacks.
fn glass_panel() -> gpui::Div {
    backdrop::glass(theme::sz(12.)).when(theme::is_light(), |d| d.shadow_md())
}

impl Workspace {
    /// Show the page, and ask `gh` for what is old.
    pub fn show_prs(&mut self, cx: &mut Context<Self>) {
        self.enter(View::PullRequests);
        self.menu_open = false;
        self.prs_fetch(false, cx);
        cx.notify();
    }

    /// Ask for both lists again (in the background), or only for the ones that are old.
    fn prs_fetch(&mut self, all: bool, cx: &mut Context<Self>) {
        if all || due(self.today.prs_at, MINE_ON_OPEN) {
            self.today_prs_refresh(cx);
        }
        if all || due(self.tickets.last_fetch, ASKED_ON_OPEN) {
            self.tickets.last_fetch = None;
            self.tickets_refresh(cx);
        }
    }

    /// From the Board's tick: the debug hook once, and "Mine" again while the page shows.
    pub(crate) fn prs_tick(&mut self, cx: &mut Context<Self>) {
        if cfg!(debug_assertions) && !self.tickets.page.debug_done {
            self.tickets.page.debug_done = true;
            if let Some(mode) = std::env::var("GLASSCODE_DEBUG_OPEN").ok().filter(|m| m == "prs" || m.starts_with("prs:")) {
                self.prs_debug_open(&mode, cx);
            }
        }
        if self.view == View::PullRequests && due(self.today.prs_at, MINE_EVERY) {
            self.today_prs_refresh(cx);
        }
    }

    fn prs_debug_open(&mut self, mode: &str, cx: &mut Context<Self>) {
        let (mut choice, mut scale, mut sample) = (None, None, false);
        for word in mode.split(':').skip(1) {
            match word {
                "sample" => sample = true,
                "light" => choice = Some(ThemeChoice::Light),
                "dark" => choice = Some(ThemeChoice::Dark),
                "classic" => choice = Some(ThemeChoice::Classic),
                "contrast" => choice = Some(ThemeChoice::Contrast),
                other => scale = theme::scale_named(other).or(scale),
            }
        }
        if choice.is_some() || scale.is_some() {
            self.set_appearance(
                |a| {
                    a.theme = choice.unwrap_or(a.theme);
                    a.scale = scale.unwrap_or(a.scale);
                },
                cx,
            );
        }
        if sample {
            let (mine, asked) = sample_lists();
            self.today.prs = Some(Ok(mine));
            self.today.prs_at = Some(Instant::now());
            self.tickets.reviews = asked;
            self.tickets.page.links = self.state.active_session.clone().map(|id| ("acme/app".to_string(), 128, id)).into_iter().collect();
        }
        self.show_prs(cx);
    }

    /// The session that made one of the person's pull requests: the one whose branch GitHub
    /// says it is (`pr_status`), not a review.
    fn own_pr_session(&self, repo: &str, number: u64) -> Option<String> {
        if let Some((_, _, id)) = self.tickets.page.links.iter().find(|(r, n, _)| r.eq_ignore_ascii_case(repo) && *n == number) {
            return Some(id.clone());
        }
        self.state
            .sessions
            .iter()
            .filter(|s| s.review.is_none())
            .find(|s| {
                self.prs.map.get(&s.id).is_some_and(|status| {
                    status.repo.as_deref().is_some_and(|r| r.eq_ignore_ascii_case(repo)) && status.pr.as_ref().is_some_and(|pr| pr.number == number)
                })
            })
            .map(|s| s.id.clone())
    }

    /// The number on the sidebar's Pull requests row.
    pub(crate) fn prs_count(&self) -> usize {
        count(self.today.prs.as_ref(), self.tickets.reviews.len())
    }

    pub fn render_prs(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mine = self.today.prs.as_ref();
        let mine_list: &[MyPr] = mine.and_then(|m| m.as_ref().ok()).map_or(&[], Vec::as_slice);
        let mine = mine_rows(mine_list, &|repo, number| self.own_pr_session(repo, number));
        let asked = if self.state.linear.review_prs_off { Vec::new() } else { asked_rows(&self.tickets.reviews, &|repo, number| self.work_on_pr(repo, number).first().map(|w| w.id.clone())) };
        let asked_failed = self.tickets.reviews_error.is_some();
        let notice = page_notice(self.today.prs.as_ref(), asked_failed, asked.len());
        let busy = self.today.prs_loading || self.tickets.loading;
        // One line on glass: in a narrow centre the sentence is cut short, not the button.
        let header = glass_panel()
            .flex()
            .flex_none()
            .items_center()
            .gap(theme::sz(10.))
            .px(theme::sz(14.))
            .py(theme::sz(10.))
            .child(icons::brand(Brand::Github, f32::from(theme::sz(18.))))
            .child(div().flex_none().text_size(theme::sz(17.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Pull requests"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(theme::sz(12.5))
                    .text_color(theme::card_soft_text())
                    .child("Yours that are open, and the ones that ask for your review. Click one to open it on GitHub."),
            )
            .child(if busy {
                ui::disabled_button("prs-refresh", "Refreshing…").into_any_element()
            } else {
                ui::button("prs-refresh", "Refresh")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.prs_fetch(true, cx);
                        cx.notify();
                    }))
                    .into_any_element()
            });
        let body = div()
            .flex()
            .flex_col()
            .gap(theme::sz(14.))
            .w_full()
            .max_w(theme::sz(900.))
            .px(theme::sz(14.))
            .pt(theme::sz(12.))
            .pb(theme::sz(24.))
            .child(header);
        let body = match notice {
            Some(sentence) => body.child(glass_panel().px(theme::sz(14.)).py(theme::sz(12.)).text_color(theme::text_soft()).child(sentence)),
            None => body
                .child(self.prs_section("prs-mine", "MINE", &mine, mine_empty(self.today.prs.as_ref()), cx))
                .child(self.prs_section("prs-asked", "ASKED OF ME", &asked, asked_empty(self.state.linear.review_prs_off, self.tickets.fetched_once, asked_failed), cx)),
        };
        let scroll = window.use_keyed_state("prs-scroll", cx, |_, _| gpui::ScrollHandle::new()).read(cx).clone();
        let layer = self.backdrop_layer(crate::tickets_view::BACKDROP, window, cx);
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme::bg())
            .text_color(theme::text())
            .child(layer)
            .child(ui::scroll_area("prs-scroll", &scroll, body))
            .into_any_element()
    }

    /// A section as one glass card: its heading, then its rows (or its sentence).
    fn prs_section(&self, id: &'static str, title: &'static str, rows: &[Row], empty: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let heading = div()
            .flex()
            .items_center()
            .gap(theme::sz(8.))
            .px(theme::sz(14.))
            .pt(theme::sz(11.))
            .pb(theme::sz(8.))
            .child(ui::caps_label(title).text_color(theme::card_soft_text()))
            .when(!rows.is_empty(), |d| d.child(div().text_size(theme::sz(11.5)).text_color(theme::card_soft_text()).child(rows.len().to_string())));
        let card = glass_panel().flex().flex_col().child(heading);
        if rows.is_empty() {
            card.child(div().px(theme::sz(14.)).pb(theme::sz(12.)).text_color(theme::text_soft()).child(empty)).into_any_element()
        } else {
            // A little room under the last row, so its hover stays off the round corners.
            card.pb(theme::sz(4.)).children(rows.iter().enumerate().map(|(i, row)| self.prs_row(id, i, row, cx))).into_any_element()
        }
    }

    fn prs_row(&self, list: &'static str, i: usize, row: &Row, cx: &mut Context<Self>) -> AnyElement {
        let tone: Hsla = match row.checks {
            _ if row.draft => theme::card_soft_text(),
            Checks::Failing(_) => theme::removed(),
            Checks::Running => theme::modified(),
            Checks::Passed => theme::added(),
            Checks::Nothing | Checks::Unknown => theme::text_soft(),
        };
        let url = row.url.clone();
        let action = row.action_label().map(|label| {
            let id = SharedString::from(format!("{list}-act-{i}"));
            let pill = if label == "Fix with agent" { ui::action_pill(id, label) } else { ui::button(id, label) };
            let (action, repo, number) = (row.action.clone(), row.repo.clone(), row.number);
            pill.flex_none().on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                match &action {
                    Action::Session(id) | Action::OpenReview(id) => {
                        this.center = crate::center::Center::Session;
                        this.open_session(id, window, cx);
                    }
                    Action::Review => this.open_start_ticket(Key::Pr(repo.clone(), number), None, window, cx),
                    Action::Nothing => {}
                }
            }))
        });
        div()
            .id(SharedString::from(format!("{list}-{i}")))
            .flex()
            .items_center()
            .gap(theme::sz(10.))
            .px(theme::sz(14.))
            .py(theme::sz(9.))
            .border_t_1()
            .border_color(theme::glass_border())
            .cursor_pointer()
            .hover(|s| s.bg(theme::tint()))
            .when(!url.is_empty(), |d| d.on_click(move |_: &ClickEvent, _, cx| crate::center::open_url(&url, cx)))
            .child(icons::icon(icons::PULL_REQUEST, theme::sz(16.), theme::sz(16.), tone))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(theme::sz(13.)).text_color(theme::text()).child(row.title.clone()))
                    .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(theme::sz(11.5)).text_color(theme::card_soft_text()).child(row.detail())),
            )
            .children(action)
            .into_any_element()
    }
}

/// Made-up pull requests for the `sample` hook: no real repository, person or data.
fn sample_lists() -> (Vec<MyPr>, Vec<ReviewRequest>) {
    let run = |name: &str, conclusion: &str| CheckRun { name: name.into(), status: "COMPLETED".into(), conclusion: conclusion.into(), ..Default::default() };
    let ci = |checks: Vec<CheckRun>| Some(CiStatus { state: "OPEN".into(), checks, mergeable: "MERGEABLE".into(), ..Default::default() });
    let mine = vec![
        MyPr {
            repo: "acme/app".into(),
            number: 131,
            title: "Show the build number in the title bar".into(),
            url: "https://github.com/acme/app/pull/131".into(),
            updated_at: "2026-11-17T10:00:00Z".into(),
            draft: false,
            ci: ci(vec![run("build (windows)", "SUCCESS"), run("build (macos)", "SUCCESS")]),
            review: "APPROVED".into(),
        },
        MyPr {
            repo: "acme/app".into(),
            number: 128,
            title: "Keep the panel widths after a restart".into(),
            url: "https://github.com/acme/app/pull/128".into(),
            updated_at: "2026-11-17T09:00:00Z".into(),
            draft: false,
            ci: ci(vec![run("build (windows)", "SUCCESS"), run("test (windows)", "FAILURE")]),
            review: "CHANGES_REQUESTED".into(),
        },
        MyPr {
            repo: "acme/docs".into(),
            number: 42,
            title: "Explain the scratch home in the guide".into(),
            url: "https://github.com/acme/docs/pull/42".into(),
            updated_at: "2026-11-16T15:00:00Z".into(),
            draft: true,
            ci: Some(CiStatus { state: "OPEN".into(), checks: vec![CheckRun { name: "links".into(), status: "IN_PROGRESS".into(), ..Default::default() }], ..Default::default() }),
            review: String::new(),
        },
        MyPr {
            repo: "acme/relay".into(),
            number: 7,
            title: "Retry after the network comes back".into(),
            url: "https://github.com/acme/relay/pull/7".into(),
            updated_at: "2026-11-12T08:00:00Z".into(),
            draft: false,
            ci: None,
            review: "REVIEW_REQUIRED".into(),
        },
    ];
    let hit = |number: u64, title: &str, author: &str, head: &str, updated: &str| PrHit {
        number,
        title: title.into(),
        url: format!("https://github.com/acme/app/pull/{number}"),
        author: author.into(),
        head: head.into(),
        base: "main".into(),
        same_repo: true,
        state: "OPEN".into(),
        review_decision: "REVIEW_REQUIRED".into(),
        updated_at: updated.into(),
        additions: 42,
        deletions: 7,
        changed_files: 3,
        ..Default::default()
    };
    let asked = vec![
        ReviewRequest { repo: "acme/app".into(), hit: hit(133, "feat: sort the board columns by status", "sam-example", "sort-columns", "2026-11-17T08:00:00Z"), created_at: "2026-11-16T12:00:00Z".into(), merge_state: "CLEAN".into(), reviews: Vec::new() },
        ReviewRequest { repo: "acme/app".into(), hit: hit(126, "fix: reconnect after sleep", "kim-example", "reconnect", "2026-11-15T17:00:00Z"), created_at: "2026-11-14T09:00:00Z".into(), merge_state: "DIRTY".into(), reviews: vec![("sam-example".into(), "COMMENTED".into())] },
    ];
    (mine, asked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(number: u64, updated: &str, ci: Option<CiStatus>, review: &str) -> MyPr {
        MyPr { repo: "acme/app".into(), number, title: format!("Change {number}"), url: format!("https://github.com/acme/app/pull/{number}"), updated_at: updated.into(), draft: false, ci, review: review.into() }
    }

    fn ci(conclusions: &[&str]) -> Option<CiStatus> {
        let checks = conclusions.iter().map(|c| CheckRun { name: "build".into(), status: "COMPLETED".into(), conclusion: c.to_string(), ..Default::default() }).collect();
        Some(CiStatus { checks, ..Default::default() })
    }

    fn no_session(_: &str, _: u64) -> Option<String> {
        None
    }

    /// A failing check first, then changes requested, then the rest; newest first in each.
    #[test]
    fn mine_puts_what_needs_the_person_first() {
        let prs = vec![
            pr(1, "2026-11-17T10:00:00Z", ci(&["SUCCESS"]), "APPROVED"),
            pr(2, "2026-11-10T10:00:00Z", ci(&["FAILURE"]), ""),
            pr(3, "2026-11-16T10:00:00Z", ci(&["SUCCESS"]), "CHANGES_REQUESTED"),
            pr(4, "2026-11-15T10:00:00Z", None, ""),
            pr(5, "2026-11-12T10:00:00Z", ci(&["FAILURE", "SUCCESS", "TIMED_OUT"]), ""),
        ];
        let rows = mine_rows(&prs, &no_session);
        assert_eq!(rows.iter().map(|r| r.number).collect::<Vec<_>>(), vec![5, 2, 3, 1, 4]);
        assert_eq!(rows[0].checks, Checks::Failing(2));
        assert_eq!(rows[0].detail(), "acme/app #5 · 2 checks failing");
        assert_eq!(rows[2].detail(), "acme/app #3 · checks passed · changes requested");
    }

    /// Review requests come newest first, with what GitHub's merge state says.
    #[test]
    fn asked_of_me_is_newest_first_with_the_merge_state() {
        let request = |number: u64, updated: &str, merge: &str| ReviewRequest {
            repo: "acme/app".into(),
            hit: PrHit { number, title: "feat: sort the columns".into(), updated_at: updated.into(), review_decision: "REVIEW_REQUIRED".into(), ..Default::default() },
            merge_state: merge.into(),
            ..Default::default()
        };
        let reviews = vec![request(1, "2026-11-10T10:00:00Z", "DIRTY"), request(2, "2026-11-17T10:00:00Z", "CLEAN"), request(3, "2026-11-12T10:00:00Z", "UNSTABLE")];
        let rows = asked_rows(&reviews, &|_, n| (n == 3).then(|| "s-review".to_string()));
        assert_eq!(rows.iter().map(|r| r.number).collect::<Vec<_>>(), vec![2, 3, 1]);
        assert_eq!(rows[0].detail(), "acme/app #2 · checks passed · review required · ready to merge");
        assert_eq!(rows[1].detail(), "acme/app #3 · checks failing · review required");
        assert_eq!(rows[2].detail(), "acme/app #1 · review required · conflicts");
        assert_eq!(rows[0].title, "Sort the columns", "no Conventional Commits prefix, as on the Board");
        assert_eq!((rows[0].action_label(), rows[1].action_label()), (Some("Review"), Some("Open review")));
    }

    /// An empty list has no row, and its sentence says why: still looking, no `gh`, or none.
    #[test]
    fn an_empty_list_says_why() {
        assert!(mine_rows(&[], &no_session).is_empty());
        assert!(asked_rows(&[], &no_session).is_empty());
        assert_eq!(mine_empty(None), MINE_LOADING);
        assert_eq!(mine_empty(Some(&Err("gh: not logged in".into()))), NO_GH);
        assert_eq!(mine_empty(Some(&Ok(Vec::new()))), MINE_EMPTY);
        assert_eq!(asked_empty(false, false, false), ASKED_LOADING);
        assert_eq!(asked_empty(false, true, true), NO_GH);
        assert_eq!(asked_empty(false, true, false), ASKED_EMPTY);
        assert_eq!(asked_empty(true, true, true), ASKED_OFF, "turned off wins over a failure");
    }

    /// With no `gh`, one sentence stands for the page; a list from before still shows.
    #[test]
    fn without_gh_one_sentence_says_so() {
        let failed: Result<Vec<MyPr>, String> = Err("gh: not found".into());
        assert_eq!(page_notice(Some(&failed), true, 0), Some(NO_GH));
        assert_eq!(page_notice(Some(&failed), true, 2), None, "review requests from before still show");
        assert_eq!(page_notice(None, true, 0), None, "Mine is still looking");
        assert_eq!(page_notice(Some(&Ok(vec![pr(1, "", None, "")])), true, 0), None);
    }

    /// No checks read, and none at all, are different: only the second is said.
    #[test]
    fn a_pull_request_with_no_checks() {
        let rows = mine_rows(&[pr(1, "b", None, ""), pr(2, "a", Some(CiStatus::default()), "")], &no_session);
        assert_eq!((rows[0].checks, rows[1].checks), (Checks::Unknown, Checks::Nothing));
        assert_eq!(rows[0].detail(), "acme/app #1");
        assert_eq!(rows[1].detail(), "acme/app #2 · no checks");
    }

    /// "Fix with agent" only where a session works on the pull request; without one the row has
    /// no action (it opens on GitHub).
    #[test]
    fn the_action_needs_a_session() {
        let prs = vec![pr(1, "b", ci(&["FAILURE"]), ""), pr(2, "a", ci(&["SUCCESS"]), ""), pr(3, "c", ci(&["FAILURE"]), "")];
        let rows = mine_rows(&prs, &|repo, n| (repo == "acme/app" && n != 3).then(|| format!("s{n}")));
        let by_number = |n: u64| rows.iter().find(|r| r.number == n).unwrap();
        assert_eq!((by_number(1).action.clone(), by_number(1).action_label()), (Action::Session("s1".into()), Some("Fix with agent")));
        assert_eq!(by_number(2).action_label(), Some("Open session"), "checks pass: the label says what it does");
        assert_eq!((by_number(3).action.clone(), by_number(3).action_label()), (Action::Nothing, None));
    }

    #[test]
    fn the_count_is_what_the_page_lists() {
        assert_eq!(count(None, 2), 2);
        assert_eq!(count(Some(&Err("no gh".into())), 0), 0);
        assert_eq!(count(Some(&Ok(vec![pr(1, "", None, ""), pr(2, "", None, "")])), 1), 3);
    }

    #[test]
    fn the_sample_has_no_real_data() {
        let (mine, asked) = sample_lists();
        assert!(mine.iter().all(|p| p.repo.starts_with("acme/")) && asked.iter().all(|r| r.repo.starts_with("acme/") && r.hit.author.ends_with("-example")));
    }
}
