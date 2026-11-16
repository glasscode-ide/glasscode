//! Drawing the Tickets view, the Board (state and actions are in `tickets_ui.rs`): the toolbar
//! and quick filters, the board or the list, and a ticket's page in the right panel.

use std::ops::Range;

use gpui::{
    AnyElement, ClickEvent, Context, FontStyle, FontWeight, HighlightStyle, Hsla, InteractiveText, SharedString, StyledText, UnderlineStyle, Window, div, prelude::*, px,
};
use glasscode_core::linear::{self, Status, Ticket};
use glasscode_core::markdown::{self, Block, Span};
use glasscode_core::model::{Provider, now};

use crate::center::open_url;
use crate::sidebar::{self, RowState};
use crate::theme;
use crate::tickets_ui::{Key, Layout, Loaded, Quick, Scope, Work, bold, label_color};
use crate::ui;
use crate::workspace::Workspace;

pub(crate) const READER_WIDTH: f32 = 540.;
const COLUMN_WIDTH: f32 = 290.;

/// A card on the board (and a row in the list).
pub struct Card {
    pub key: Key,
    pub label: String,
    pub title: String,
    pub status: Option<Status>,
    /// The column: its order, position and name.
    pub column: (u8, i64, String),
    pub priority: Option<u8>,
    pub chips: Vec<(String, Option<Hsla>)>,
    pub pr: Option<(String, Hsla)>,
    pub due: Option<(String, bool)>,
    pub due_soon: bool,
    pub review_tag: bool,
    pub avatar: Option<(String, Hsla, String)>,
    pub foot: String,
    pub work: Vec<Work>,
    pub worktree: Option<String>,
    pub search: String,
    pub done: bool,
}

impl Card {
    /// The most urgent state of its sessions.
    pub fn worst(&self) -> Option<RowState> {
        self.work.iter().map(|w| w.state).min()
    }
}

/// A ticket card being dragged to another column (status), and what follows the pointer.
#[derive(Clone)]
pub struct DraggedCard {
    identifier: String,
    title: String,
    from: String,
}

impl Render for DraggedCard {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .w(px(COLUMN_WIDTH - 28.))
            .p_2p5()
            .rounded_lg()
            .border_1()
            .border_color(theme::accent())
            .bg(theme::raised())
            .shadow_lg()
            .opacity(0.95)
            .child(div().text_size(px(11.5)).text_color(theme::muted()).child(self.identifier.clone()))
            .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(theme::text_strong()).overflow_hidden().whitespace_nowrap().text_ellipsis().child(self.title.clone()))
    }
}

/// A pull request's title for reading: no Conventional Commits prefix.
pub fn tidy_pr_title(title: &str) -> String {
    glasscode_core::naming::tidy_title(title, 200)
}

pub fn avatar_color(name: &str) -> Hsla {
    const COLORS: [u32; 8] = [0xc2185b, 0x2b8a3e, 0x1864ab, 0x6741d9, 0x0b7285, 0xc2410c, 0xa61e4d, 0x4263eb];
    let hash = name.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    theme::c(COLORS[(hash % COLORS.len() as u32) as usize])
}

fn initials(name: &str) -> String {
    linear::Person { id: String::new(), name: name.to_string(), email: String::new() }.initials()
}

/// Linear's status colours, in this theme.
pub fn status_color(status: &Status) -> Hsla {
    match status.kind.as_str() {
        "started" if status.name.to_lowercase().contains("review") => theme::added(),
        "started" => theme::modified(),
        "completed" => theme::codex(),
        "triage" => sidebar::attention(),
        "canceled" | "backlog" => theme::faint(),
        _ => theme::text_soft(),
    }
}

/// A status as a small circle: dashed for backlog, empty for to do, a dot while it is being
/// worked on (a bigger one in review), filled with a mark once done, cancelled or in triage.
pub fn status_icon(status: &Status, size: f32) -> AnyElement {
    let color = status_color(status);
    let ring = div().flex_none().size(px(size)).rounded_full().border_2().border_color(color).flex().items_center().justify_center();
    match status.kind.as_str() {
        "backlog" => ring.border_dashed().into_any_element(),
        "started" => {
            let dot = if status.name.to_lowercase().contains("review") { size * 0.5 } else { size * 0.3 };
            ring.child(div().size(px(dot)).rounded_full().bg(color)).into_any_element()
        }
        "completed" | "canceled" | "triage" => div()
            .flex_none()
            .size(px(size))
            .rounded_full()
            .bg(color)
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme::bg())
            .text_size(px(size * 0.7))
            .font_weight(FontWeight::BOLD)
            .child(match status.kind.as_str() {
                "completed" => "✓",
                "canceled" => "×",
                _ => "›",
            })
            .into_any_element(),
        _ => ring.into_any_element(),
    }
}

/// Linear's priority: a filled square for urgent, three bars otherwise, dots for none.
pub fn priority_icon(priority: u8) -> AnyElement {
    match priority {
        1 => div()
            .flex_none()
            .size(px(13.))
            .rounded(px(3.))
            .bg(sidebar::attention())
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme::on_attention())
            .text_size(px(10.))
            .font_weight(FontWeight::BOLD)
            .child("!")
            .into_any_element(),
        2..=4 => div()
            .flex_none()
            .flex()
            .items_end()
            .gap(px(1.5))
            .h(px(12.))
            .children([4., 7.5, 11.].into_iter().enumerate().map(|(i, h)| {
                let lit = i < (5 - priority as usize).min(3);
                div().w(px(3.)).h(px(h)).rounded(px(1.)).bg(if lit { theme::text_soft() } else { theme::border_strong() })
            }))
            .into_any_element(),
        _ => div().flex_none().text_color(theme::faint()).text_size(px(11.)).child("···").into_any_element(),
    }
}

fn avatar(initials: &str, color: Hsla, size: f32) -> gpui::Div {
    div()
        .flex_none()
        .size(px(size))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(color)
        .text_color(theme::c(0xffffff))
        .text_size(px(size * 0.42))
        .font_weight(FontWeight::BOLD)
        .child(initials.to_string())
}

fn chip(text: impl Into<SharedString>, dot: Option<Hsla>) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1p5()
        .h(px(22.))
        .px_2()
        .rounded_md()
        .border_1()
        .border_color(theme::border_strong())
        .text_size(px(11.5))
        .text_color(theme::text_soft())
        .whitespace_nowrap()
        .when_some(dot, |d, c| d.child(div().size(px(7.)).rounded_full().bg(c)))
        .child(text.into())
}

fn review_tag() -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .h(px(17.))
        .px_1p5()
        .rounded_full()
        .border_1()
        .border_color(theme::border_strong())
        .text_color(theme::text_soft())
        .text_size(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .child("Review")
}

impl Workspace {
    // ----- cards ---------------------------------------------------------------------------

    fn ticket_card(&self, t: &Ticket, reviewing: bool) -> Card {
        let work = self.work_on_ticket(t);
        let worktree = if work.is_empty() { self.idle_worktrees_for(t).first().map(|(_, _, b)| b.clone()) } else { None };
        let prs = t.pull_requests();
        let open: Vec<&(String, u64)> = prs.iter().collect();
        let pr = match open.len() {
            0 => None,
            1 => Some((format!("#{}", open[0].1), theme::added())),
            n => Some((format!("{n} PRs"), theme::added())),
        };
        let days = t.due.as_deref().and_then(|d| linear::days_until(d, now()));
        let label = self.tickets.label().unwrap_or_default().to_string();
        let done = t.status.kind == "completed";
        Card {
            key: Key::Ticket(t.identifier.clone()),
            label: t.identifier.clone(),
            title: t.title.clone(),
            status: Some(t.status.clone()),
            column: if done { (4, 0, "Done".into()) } else { (t.status.order(), (t.status.position * 100.) as i64, t.status.name.clone()) },
            priority: Some(t.priority),
            chips: t.labels.iter().filter(|l| **l != label).map(|l| (l.clone(), Some(label_color(l)))).collect(),
            pr,
            due: t.due.as_ref().map(|d| (linear::short_date(d), days.is_some_and(|n| n <= 3))),
            due_soon: days.is_some_and(|n| (0..=7).contains(&n)),
            review_tag: reviewing,
            avatar: t.assignee.as_ref().map(|a| (initials(&a.name), avatar_color(&a.name), a.name.clone())),
            foot: match &t.completed_at {
                Some(at) => format!("Done {}", linear::short_date(at)),
                None => format!("Created {}", linear::short_date(&t.created_at)),
            },
            work,
            worktree,
            search: format!("{} {} {} {}", t.identifier, t.title, t.labels.join(" "), t.status.name).to_lowercase(),
            done,
        }
    }

    fn pr_card(&self, r: &glasscode_core::github::ReviewRequest) -> Card {
        let mut chips: Vec<(String, Option<Hsla>)> = Vec::new();
        let tickets: Vec<String> = linear::identifiers_in(&format!("{} {}", r.hit.title, r.hit.head)).into_iter().filter(|id| self.tickets.ticket(id).is_some()).collect();
        for id in &tickets {
            chips.push((id.clone(), None));
        }
        match r.bot_review() {
            Some("CHANGES_REQUESTED") => chips.push(("Review ✕".into(), Some(theme::removed()))),
            Some("APPROVED") => chips.push(("Review ✓".into(), Some(theme::added()))),
            Some(_) => chips.push(("Review commented".into(), Some(theme::muted()))),
            None => {}
        }
        if r.hit.draft {
            chips.push(("Draft".into(), Some(theme::muted())));
        } else {
            match r.merge_state.as_str() {
                "DIRTY" => chips.push(("Conflicts".into(), Some(theme::removed()))),
                "BEHIND" => chips.push(("Behind main".into(), Some(theme::modified()))),
                "CLEAN" => chips.push(("Ready to merge".into(), Some(theme::added()))),
                _ => {}
            }
        }
        // "app#784", unless another owner has a repository by that name.
        let name = r.repo.rsplit('/').next().unwrap_or(&r.repo);
        let clash = self.tickets.reviews.iter().any(|o| o.repo != r.repo && o.repo.rsplit('/').next() == Some(name));
        let short_repo = if clash { r.repo.clone() } else { name.to_string() };
        Card {
            key: Key::Pr(r.repo.clone(), r.hit.number),
            label: format!("{short_repo}#{}", r.hit.number),
            title: tidy_pr_title(&r.hit.title),
            status: None,
            column: (0, -1, "Asking for your review".into()),
            priority: None,
            chips,
            pr: Some((format!("{} files", r.hit.changed_files), theme::muted())),
            due: None,
            due_soon: false,
            review_tag: false,
            avatar: Some((initials(&r.hit.author), avatar_color(&r.hit.author), format!("By {}", r.hit.author))).filter(|_| !r.hit.author.is_empty()),
            foot: format!("Opened {} · updated {}", linear::short_date(&r.created_at), linear::short_date(&r.hit.updated_at)),
            work: self.work_on_pr(&r.repo, r.hit.number),
            worktree: None,
            search: format!("{} {} {} {} {}", r.repo, r.hit.number, r.hit.title, r.hit.head, r.hit.author).to_lowercase(),
            done: false,
        }
    }

    /// The cards the scope, the search and the quick filter leave.
    fn cards(&self, cx: &Context<Self>) -> (Vec<Card>, Vec<Card>) {
        let mut all: Vec<Card> = Vec::new();
        if self.tickets.scope != Scope::Review {
            all.extend(self.tickets.mine().into_iter().map(|t| self.ticket_card(t, false)));
        }
        if self.tickets.scope != Scope::Mine {
            all.extend(self.tickets.reviewing().into_iter().map(|t| self.ticket_card(t, true)));
            all.extend(self.tickets.reviews.iter().map(|r| self.pr_card(r)));
        }
        let query = self.tickets.query.read(cx).text().trim().to_lowercase();
        let searched: Vec<Card> = all.into_iter().filter(|c| query.is_empty() || query.split_whitespace().all(|w| c.search.contains(w))).collect();
        let quick = self.tickets.quick;
        let (shown, rest): (Vec<Card>, Vec<Card>) = searched.into_iter().partition(|c| quick_test(quick, c));
        let mut shown = shown;
        shown.sort_by(|a, b| {
            let rank = |c: &Card| c.worst().map_or(3, |s| s as u8);
            rank(a).cmp(&rank(b)).then(a.priority.map_or(9, |p| if p == 0 { 9 } else { p }).cmp(&b.priority.map_or(9, |p| if p == 0 { 9 } else { p }))).then(a.label.cmp(&b.label))
        });
        (shown, rest)
    }

    // ----- the view --------------------------------------------------------------------------

    /// Nothing to show yet: Linear is not connected and no pull request asks for a review.
    fn tickets_unconnected(&self) -> bool {
        matches!(self.tickets_provider(), Provider::Linear) && !self.tickets.connected && self.tickets.reviews.is_empty()
    }

    /// The board or the list; a ticket being read sits in the right panel (`render_reader_panel`),
    /// or here in its place when it is expanded.
    pub fn render_tickets(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.tickets_unconnected() {
            return div()
                .flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .items_center()
                .justify_center()
                .bg(theme::bg())
                .child(
                    div()
                        .w(px(560.))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .p_6()
                        .rounded_xl()
                        .border_1()
                        .border_color(theme::border_strong())
                        .bg(theme::dialog())
                        .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child("Your Linear tickets, here"))
                        .child(div().text_color(theme::muted()).child("The tickets assigned to you and the ones you review, on a board, each with the sessions working on it. Start a session from any ticket."))
                        .child(crate::tickets_ui::render_connect(self, cx)),
                )
                .into_any_element();
        }
        let (cards, _) = self.cards(cx);
        let body = match self.tickets.layout {
            Layout::Board => self.render_board(&cards, cx),
            Layout::List => self.render_list(&cards, cx),
        };
        let reader = if self.tickets.expanded { self.render_reader(window, cx) } else { None };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme::bg())
            .child(self.render_tickets_toolbar(cx))
            .child(self.render_quick_filters(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when(reader.is_none(), |d| d.child(div().id("tickets-body").flex_1().min_w_0().h_full().overflow_scroll().p_3().child(body)))
                    .when_some(reader, |d, reader| {
                        d.child(div().flex().flex_col().flex_1().h_full().min_w_0().bg(theme::panel()).border_l_1().border_color(theme::border()).child(reader))
                    }),
            )
            .into_any_element()
    }

    /// A ticket or pull request is being read in the right panel (not expanded, and still there).
    pub(crate) fn reader_open(&self) -> bool {
        let found = match &self.tickets.selected {
            Some(Key::Ticket(id)) => self.tickets.ticket(id).is_some(),
            Some(Key::Pr(repo, number)) => self.tickets.review(repo, *number).is_some(),
            None => false,
        };
        found && !self.tickets.expanded && !self.tickets_unconnected()
    }

    /// The ticket being read, as the Board's right panel (when it is not expanded).
    pub(crate) fn render_reader_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.reader_open() {
            return None;
        }
        let reader = self.render_reader(window, cx)?;
        Some(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .w(px(self.tickets_width()))
                .h_full()
                .min_w_0()
                .bg(theme::panel())
                .border_l_1()
                .border_color(theme::border())
                .child(reader)
                .into_any_element(),
        )
    }

    /// The width the reader shows at (narrower in a narrow window, `shell::fit_panels`).
    pub fn tickets_width(&self) -> f32 {
        self.shown_width(crate::workspace::Side::Tickets)
    }

    fn render_tickets_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let mine = self.tickets.mine().into_iter().filter(|t| !t.is_done()).count();
        let reviewing = self.tickets.reviewing().into_iter().filter(|t| !t.is_done()).count() + self.tickets.reviews.len();
        // The source row: provider pill + provider picker + project picker. Built first because
        // the pickers take `cx` mutably, before the scope/layout closures borrow it.
        let snapshot = self.tickets.snapshot.as_ref();
        let provider = self.tickets_provider();
        let who = match provider {
            Provider::Linear => snapshot
                .map(|s| format!("Linear · {} · {}", s.org, self.state.linear.me_label.clone().unwrap_or_else(|| if s.viewer.email.is_empty() { s.viewer.name.clone() } else { s.viewer.email.clone() })))
                .unwrap_or_else(|| "Linear · not connected".into()),
            Provider::GitHub => self.tickets_github_repo().map(|r| format!("GitHub · {r}")).unwrap_or_else(|| "GitHub · no repository".into()),
        };
        let provider_picker = {
            let options: Vec<(String, String, bool, Box<dyn Fn(&mut Workspace, &mut Context<Workspace>)>)> = vec![
                ("Linear".into(), "Your Linear tickets and reviews".into(), provider == Provider::Linear, Box::new(|this: &mut Workspace, cx: &mut Context<Workspace>| this.tickets_pick_provider(Provider::Linear, cx))),
                ("GitHub Issues".into(), "Issues in a GitHub repository".into(), provider == Provider::GitHub, Box::new(|this: &mut Workspace, cx: &mut Context<Workspace>| this.tickets_pick_provider(Provider::GitHub, cx))),
            ];
            crate::tickets_ui::picker("tickets-provider", provider.label().to_string(), self.tickets.picker == Some("tickets-provider"), options, cx)
        };
        let project_picker = {
            let mut options: Vec<(String, String, bool, Box<dyn Fn(&mut Workspace, &mut Context<Workspace>)>)> = Vec::new();
            let value = match provider {
                Provider::Linear => {
                    options.push(("All teams".into(), "The whole workspace".into(), self.state.tickets.linear_team.is_none(), Box::new(|this: &mut Workspace, cx: &mut Context<Workspace>| this.tickets_pick_project(None, cx))));
                    for opt in self.tickets.project_options.iter().flatten() {
                        let on = self.state.tickets.linear_team.as_deref() == Some(opt.id.as_str());
                        let id = opt.id.clone();
                        options.push((opt.label.clone(), opt.detail.clone(), on, Box::new(move |this: &mut Workspace, cx: &mut Context<Workspace>| this.tickets_pick_project(Some(id.clone()), cx))));
                    }
                    self.state.tickets.linear_team.clone().unwrap_or_else(|| "All teams".into())
                }
                Provider::GitHub => {
                    let current = self.tickets_github_repo();
                    for opt in self.tickets.project_options.iter().flatten() {
                        let on = current.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(&opt.id));
                        let id = opt.id.clone();
                        options.push((opt.label.clone(), opt.detail.clone(), on, Box::new(move |this: &mut Workspace, cx: &mut Context<Workspace>| this.tickets_pick_project(Some(id.clone()), cx))));
                    }
                    current.unwrap_or_else(|| "Pick a repository".into())
                }
            };
            if options.is_empty() {
                options.push(("Loading…".into(), String::new(), false, Box::new(|_: &mut Workspace, _: &mut Context<Workspace>| {})));
            }
            crate::tickets_ui::picker("tickets-project", value, self.tickets.picker == Some("tickets-project"), options, cx)
        };
        let scope_tab = |id: &'static str, scope: Scope, label: &'static str, count: usize| {
            let on = self.tickets.scope == scope;
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_1p5()
                .h(px(28.))
                .px_2p5()
                .rounded_md()
                .cursor_pointer()
                .when(on, |d| d.bg(theme::selected()).text_color(theme::text_strong()).font_weight(FontWeight::SEMIBOLD))
                .when(!on, |d| d.text_color(theme::muted()))
                .hover(|s| s.text_color(theme::text_strong()))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.tickets.scope = scope;
                    cx.notify();
                }))
                .child(label)
                .child(div().text_size(px(11.5)).text_color(theme::faint()).child(count.to_string()))
        };
        let layout_tab = |id: &'static str, layout: Layout, label: &'static str| {
            let on = self.tickets.layout == layout;
            div()
                .id(id)
                .flex()
                .items_center()
                .h(px(28.))
                .px_2p5()
                .rounded_md()
                .cursor_pointer()
                .when(on, |d| d.bg(theme::selected()).text_color(theme::text_strong()).font_weight(FontWeight::SEMIBOLD))
                .when(!on, |d| d.text_color(theme::muted()))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.tickets.layout = layout;
                    cx.notify();
                }))
                .child(label)
        };
        let segment = || div().flex().items_center().gap_0p5().p_0p5().rounded_lg().border_1().border_color(theme::border_strong()).bg(theme::raised());
        let synced = if self.tickets.loading {
            "Syncing…".to_string()
        } else {
            match snapshot.map(|s| now().saturating_sub(s.fetched_at)) {
                Some(secs) if secs < 60 => "Synced just now".into(),
                Some(secs) => format!("Synced {}", crate::session_start::ago_secs(secs as i64)),
                None => "Sync".into(),
            }
        };
        div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap_3()
            .px_4()
            .py_2p5()
            .border_b_1()
            .border_color(theme::border())
            .child(div().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child("Tickets"))
            .child(
                div()
                    .id("tickets-source")
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .h(px(24.))
                    .px_2p5()
                    .rounded_full()
                    .border_1()
                    .border_color(theme::border_strong())
                    .text_size(px(11.5))
                    .text_color(theme::text_soft())
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_linear_settings(window, cx)))
                    .child(ui::dot(if self.tickets.error.is_some() { theme::removed() } else if self.tickets.connected { theme::added() } else { theme::faint() }))
                    .child(who),
            )
            .child(provider_picker)
            .child(project_picker)
            .child(
                segment()
                    .child(scope_tab("scope-mine", Scope::Mine, "Assigned to me", mine))
                    .child(scope_tab("scope-review", Scope::Review, "To review", reviewing))
                    .child(scope_tab("scope-both", Scope::Both, "Both", mine + reviewing)),
            )
            .child(div().flex_1())
            .child(div().w(px(280.)).child(self.tickets.query.clone()))
            .child(segment().child(layout_tab("layout-board", Layout::Board, "Board")).child(layout_tab("layout-list", Layout::List, "List")))
            .child(ui::button("tickets-sync", synced).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.tickets.last_fetch = None;
                this.tickets_refresh(cx);
                cx.notify();
            })))
            .into_any_element()
    }

    fn render_quick_filters(&self, cx: &mut Context<Self>) -> AnyElement {
        let (shown, rest) = self.cards(cx);
        let all: Vec<&Card> = shown.iter().chain(rest.iter()).collect();
        let filters = [
            (Quick::All, "Everything", None),
            (Quick::NeedsYou, "A session needs you", Some(sidebar::attention())),
            (Quick::Working, "An agent is on it", Some(theme::added())),
            (Quick::NoSession, "No session yet", None),
            (Quick::DueSoon, "Due in 7 days", None),
        ];
        let error = self.tickets.error.clone().or_else(|| self.tickets.reviews_error.clone().map(|e| format!("GitHub: {e}")));
        div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap_1p5()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme::border())
            .children(filters.into_iter().map(|(quick, label, dot)| {
                let on = self.tickets.quick == quick;
                let count = all.iter().filter(|c| quick_test(quick, c)).count();
                div()
                    .id(SharedString::from(format!("quick-{label}")))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .h(px(26.))
                    .px_2p5()
                    .rounded_full()
                    .border_1()
                    .border_color(if on { theme::faint() } else { theme::border_strong() })
                    .when(on, |d| d.bg(theme::selected()))
                    .text_size(px(12.))
                    .text_color(if on { theme::text_strong() } else { theme::text_soft() })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.tickets.quick = if this.tickets.quick == quick { Quick::All } else { quick };
                        cx.notify();
                    }))
                    .when_some(dot, |d, c| d.child(div().size(px(7.)).rounded_full().bg(c)))
                    .child(label)
                    .child(div().text_color(theme::faint()).child(count.to_string()))
            }))
            .child(div().flex_1())
            .child(match error {
                Some(e) => div().text_size(px(12.)).text_color(theme::removed()).max_w(px(520.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(e),
                None => div().text_size(px(12.)).text_color(theme::faint()).child("Each card shows the sessions working on it"),
            })
            .into_any_element()
    }

    // ----- the board and the list ----------------------------------------------------------------

    fn columns(&self, cards: &[Card]) -> Vec<((u8, i64, String), Vec<usize>)> {
        let mut columns: Vec<((u8, i64, String), Vec<usize>)> = Vec::new();
        for (i, card) in cards.iter().enumerate() {
            match columns.iter_mut().find(|(c, _)| c.2 == card.column.2) {
                Some((_, list)) => list.push(i),
                None => columns.push((card.column.clone(), vec![i])),
            }
        }
        // Linear's own statuses with nothing in them still show (but not in To review).
        if self.tickets.scope != Scope::Review {
            for status in self.tickets.mine().iter().map(|t| &t.status).filter(|s| s.kind != "completed" && s.kind != "canceled") {
                if !columns.iter().any(|(c, _)| c.2 == status.name) {
                    columns.push(((status.order(), (status.position * 100.) as i64, status.name.clone()), Vec::new()));
                }
            }
        }
        columns.sort_by(|a, b| (a.0.0, a.0.1).cmp(&(b.0.0, b.0.1)));
        columns
    }

    fn column_icon(&self, column: &(u8, i64, String), cards: &[Card], list: &[usize]) -> AnyElement {
        if column.1 == -1 {
            return div().text_color(theme::text_soft()).child("⇄").into_any_element();
        }
        let status = list.first().and_then(|&i| cards[i].status.clone()).or_else(|| self.tickets.mine().iter().map(|t| &t.status).find(|s| s.name == column.2).cloned());
        match status {
            Some(s) => status_icon(&s, 13.),
            None => status_icon(&Status { name: column.2.clone(), kind: if column.0 == 4 { "completed".into() } else { String::new() }, position: 0. }, 13.),
        }
    }

    fn render_board(&self, cards: &[Card], cx: &mut Context<Self>) -> AnyElement {
        let columns = self.columns(cards);
        div()
            .flex()
            .items_start()
            .gap_2p5()
            .children(columns.into_iter().map(|(column, list)| {
                let done = column.0 == 4;
                let folded = done && !self.tickets.done_open;
                let icon = self.column_icon(&column, cards, &list);
                let count = list.len();
                if folded {
                    return div()
                        .id(SharedString::from(format!("col-{}", column.2)))
                        .flex()
                        .flex_none()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .w(px(44.))
                        .py_3()
                        .rounded_xl()
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::panel())
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.tickets.done_open = true;
                            cx.notify();
                        }))
                        .drag_over::<DraggedCard>(|style, _, _, _| style.border_color(theme::accent()).bg(theme::selected()))
                        .on_drop(cx.listener({
                            let to = column.2.clone();
                            move |this, drag: &DraggedCard, _, cx| this.ticket_move(&drag.identifier, &to, cx)
                        }))
                        .child(icon)
                        .children(column.2.chars().map(|c| div().text_size(px(12.)).line_height(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(c.to_string())))
                        .child(div().text_size(px(11.5)).text_color(theme::muted()).child(count.to_string()))
                        .into_any_element();
                }
                // A status column takes ticket cards dropped on it (the review column does not).
                let to = column.2.clone();
                let from = column.2.clone();
                let takes_cards = column.1 != -1;
                div()
                    .id(SharedString::from(format!("col-{}", column.2)))
                    .flex()
                    .flex_none()
                    .flex_col()
                    .gap_2()
                    .w(px(COLUMN_WIDTH))
                    .p_2()
                    .rounded_xl()
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::panel())
                    .when(takes_cards, |d| {
                        d.drag_over::<DraggedCard>(move |style, drag, _, _| if drag.from == from { style } else { style.border_color(theme::accent()).bg(theme::selected()) })
                            .on_drop(cx.listener(move |this, drag: &DraggedCard, _, cx| this.ticket_move(&drag.identifier, &to, cx)))
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .h(px(26.))
                            .px_1()
                            .child(icon)
                            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(column.2.clone()))
                            .child(div().text_color(theme::muted()).child(count.to_string()))
                            .when(done, |d| {
                                d.child(div().flex_1()).child(ui::button("done-fold", "Fold").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.tickets.done_open = false;
                                    cx.notify();
                                })))
                            }),
                    )
                    .when(column.1 == -1, |d| d.child(div().px_1().text_size(px(11.5)).text_color(theme::muted()).child("Pull requests on GitHub that wait on your review")))
                    .children(list.iter().map(|&i| self.render_card(&cards[i], cx)))
                    .when(list.is_empty(), |d| d.child(div().py_3().text_center().text_size(px(12.)).text_color(theme::faint()).child("Nothing here")))
                    .into_any_element()
            }))
            .into_any_element()
    }

    fn render_card(&self, card: &Card, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.tickets.selected.as_ref() == Some(&card.key);
        let worst = card.worst();
        let key = card.key.clone();
        let key_act = card.key.clone();
        let first_work = card.work.first().cloned();
        let worktree = card.worktree.clone();
        let reviewing = card.review_tag || matches!(card.key, Key::Pr(..));
        let action = if first_work.is_some() {
            "Open session"
        } else if reviewing {
            "Start review"
        } else if worktree.is_some() {
            "Start in worktree"
        } else {
            "Start session"
        };
        let group = SharedString::from(format!("card-{}", card.label));
        let work_line = (!card.work.is_empty() || card.worktree.is_some()).then(|| {
            let label = match (card.work.first(), &card.worktree) {
                (Some(w), _) => format!("{}{}", w.title, if card.work.len() > 1 { format!(" +{}", card.work.len() - 1) } else { String::new() }),
                (None, Some(b)) => format!("⎇ {b} · no session"),
                _ => String::new(),
            };
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .px_2()
                .py_1p5()
                .rounded_md()
                .bg(match worst {
                    Some(RowState::NeedsYou) => Hsla { a: 0.14, ..sidebar::attention() },
                    Some(RowState::Working) => Hsla { a: 0.12, ..theme::added() },
                    _ => theme::hover(),
                })
                .children(card.work.iter().take(3).map(|w| sidebar::agent_icon(w.agent, 17., w.running)))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(12.)).text_color(theme::text_soft()).child(label))
                .children(worst.and_then(|s| sidebar::status_icon(s, &format!("card-{}", card.label))))
        });
        div()
            .id(SharedString::from(format!("card-{}", card.label)))
            .group(group.clone())
            .relative()
            .flex()
            .flex_col()
            .gap_1p5()
            .p_2p5()
            .rounded_lg()
            .border_1()
            .border_color(if selected {
                theme::accent()
            } else if worst == Some(RowState::NeedsYou) {
                Hsla { a: 0.7, ..sidebar::attention() }
            } else {
                theme::border_strong()
            })
            .bg(theme::raised())
            .cursor_pointer()
            .hover(|s| s.bg(theme::hover()))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_ticket(key.clone(), cx)))
            // Ticket cards drag to another column to change their status in Linear.
            .when_some(match &card.key {
                Key::Ticket(id) => Some(DraggedCard { identifier: id.clone(), title: card.title.clone(), from: card.column.2.clone() }),
                Key::Pr(..) => None,
            }, |d, drag| d.on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone())))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(px(11.5))
                    .text_color(theme::muted())
                    .child(card.label.clone())
                    .when(card.review_tag, |d| d.child(review_tag()))
                    .child(div().flex_1())
                    .when_some(card.avatar.clone(), |d, (i, c, _)| d.child(avatar(&i, c, 19.))),
            )
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(div().mt(px(2.)).child(match &card.status {
                        Some(s) => status_icon(s, 13.),
                        None => div().text_size(px(12.)).text_color(card.pr.as_ref().map_or(theme::muted(), |p| p.1)).child("⇄").into_any_element(),
                    }))
                    .child(div().flex_1().min_w_0().text_size(px(13.)).line_height(px(18.)).font_weight(FontWeight::MEDIUM).text_color(theme::text_strong()).line_clamp(2).text_ellipsis().child(card.title.clone())),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .when_some(card.priority, |d, p| d.child(div().flex().items_center().justify_center().w(px(24.)).h(px(22.)).rounded_md().border_1().border_color(theme::border_strong()).child(priority_icon(p))))
                    .children(card.chips.iter().map(|(text, dot)| chip(text.clone(), *dot)))
                    .when_some(card.pr.clone(), |d, (text, color)| d.child(chip(text, Some(color))))
                    .when_some(card.due.clone(), |d, (text, soon)| d.child(chip(format!("Due {text}"), None).when(soon, |c| c.text_color(sidebar::attention())))),
            )
            .when_some(work_line, |d, line| d.child(line))
            .child(div().text_size(px(11.)).text_color(theme::faint()).child(card.foot.clone()))
            .child(
                div()
                    .absolute()
                    .right(px(8.))
                    .bottom(px(6.))
                    .invisible()
                    .group_hover(group, |s| s.visible())
                    .child(ui::button(SharedString::from(format!("card-act-{}", card.label)), action).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        match &first_work {
                            Some(work) => this.open_work(work, window, cx),
                            None => {
                                let existing = match &key_act {
                                    Key::Ticket(id) => this.tickets.ticket(id).cloned().and_then(|t| this.idle_worktrees_for(&t).into_iter().next()),
                                    _ => None,
                                };
                                this.open_start_ticket(key_act.clone(), existing, window, cx);
                            }
                        }
                    }))),
            )
            .into_any_element()
    }

    fn render_list(&self, cards: &[Card], cx: &mut Context<Self>) -> AnyElement {
        let columns = self.columns(cards);
        div()
            .flex()
            .flex_col()
            .gap_4()
            .max_w(px(1180.))
            .children(columns.into_iter().map(|(column, list)| {
                let done = column.0 == 4;
                let open = !done || self.tickets.done_open;
                let icon = self.column_icon(&column, cards, &list);
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .id(SharedString::from(format!("list-head-{}", column.2)))
                            .flex()
                            .items_center()
                            .gap_2()
                            .h(px(28.))
                            .px_1()
                            .when(done, |d| {
                                d.cursor_pointer().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.tickets.done_open = !this.tickets.done_open;
                                    cx.notify();
                                }))
                            })
                            .child(icon)
                            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(column.2.clone()))
                            .child(div().text_color(theme::muted()).child(list.len().to_string()))
                            .when(done, |d| d.child(div().text_color(theme::faint()).child(if open { "▾" } else { "▸" }))),
                    )
                    .when(open, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .rounded_lg()
                                .border_1()
                                .border_color(theme::border())
                                .bg(theme::raised())
                                .overflow_hidden()
                                .children(list.iter().enumerate().map(|(n, &i)| self.render_row(&cards[i], n == 0, cx)))
                                .when(list.is_empty(), |d| d.child(div().px_3().py_2().text_size(px(12.)).text_color(theme::faint()).child("Nothing here"))),
                        )
                    })
            }))
            .into_any_element()
    }

    fn render_row(&self, card: &Card, first: bool, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.tickets.selected.as_ref() == Some(&card.key);
        let key = card.key.clone();
        let worst = card.worst();
        div()
            .id(SharedString::from(format!("row-{}", card.label)))
            .flex()
            .items_center()
            .gap_2p5()
            .h(px(40.))
            .px_3()
            .when(!first, |d| d.border_t_1().border_color(theme::border()))
            .when(selected, |d| d.bg(theme::selected()))
            .cursor_pointer()
            .hover(|s| s.bg(theme::hover()))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_ticket(key.clone(), cx)))
            .child(div().w(px(16.)).flex().justify_center().children(card.priority.map(priority_icon)))
            .child(div().w(px(if matches!(card.key, Key::Pr(..)) { 132. } else { 72. })).flex_none().text_size(px(12.)).text_color(theme::muted()).whitespace_nowrap().overflow_hidden().text_ellipsis().child(card.label.clone()))
            .child(match &card.status {
                Some(s) => status_icon(s, 13.),
                None => div().text_size(px(12.)).text_color(theme::muted()).child("⇄").into_any_element(),
            })
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme::text_strong()).child(card.title.clone()))
            .when(card.review_tag, |d| d.child(review_tag()))
            .children(card.chips.iter().take(3).map(|(text, dot)| chip(text.clone(), *dot)))
            .when_some(card.due.clone(), |d, (text, soon)| d.child(chip(format!("Due {text}"), None).when(soon, |c| c.text_color(sidebar::attention()))))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .w(px(190.))
                    .flex_none()
                    .children(card.work.iter().take(3).map(|w| sidebar::agent_icon(w.agent, 17., w.running)))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(12.)).text_color(theme::text_soft()).child(card.work.first().map(|w| w.title.clone()).or_else(|| card.worktree.as_ref().map(|b| format!("⎇ {b}"))).unwrap_or_default()))
                    .children(worst.and_then(|s| sidebar::status_icon(s, &format!("row-{}", card.label)))),
            )
            .child(div().w(px(20.)).flex_none().children(card.avatar.clone().map(|(i, c, _)| avatar(&i, c, 19.))))
            .into_any_element()
    }

    // ----- a ticket's page -----------------------------------------------------------------

    fn render_reader(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let key = self.tickets.selected.clone()?;
        let header = |this: &Self, id_text: String, branch: String, url: String, open_label: &'static str, cx: &mut Context<Self>| {
            let branch_copy = branch.clone();
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .h(px(44.))
                .px_3()
                .border_b_1()
                .border_color(theme::border())
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_soft()).whitespace_nowrap().child(id_text))
                .when(!branch.is_empty(), |d| {
                    d.child(
                        div()
                            .id("reader-branch")
                            .flex()
                            .items_center()
                            .gap_1()
                            .min_w_0()
                            .h(px(24.))
                            .px_2()
                            .rounded_md()
                            .border_1()
                            .border_color(theme::border_strong())
                            .font_family(theme::MONO_FONT)
                            .text_size(px(11.))
                            .text_color(theme::muted())
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme::text_strong()))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(branch_copy.clone()));
                                this.show_toast(format!("Copied {branch_copy}."), cx);
                            }))
                            .child("⎇")
                            .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(branch)),
                    )
                })
                .child(div().flex_1())
                .child(ui::button("reader-open", format!("{open_label} ↗")).on_click(move |_: &ClickEvent, _, _| open_url(&url)))
                .child(ui::icon_button("reader-expand", if this.tickets.expanded { "⇥" } else { "⤢" }).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.tickets.expanded = !this.tickets.expanded;
                    cx.notify();
                })))
                .child(ui::icon_button("reader-close", "×").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.tickets.selected = None;
                    this.tickets.expanded = false;
                    cx.notify();
                })))
        };
        let (head, content) = match &key {
            Key::Ticket(id) => {
                let ticket = self.tickets.ticket(id)?.clone();
                (header(self, ticket.identifier.clone(), ticket.branch.clone(), ticket.url.clone(), "Open in Linear", cx), self.render_ticket_page(&ticket, cx))
            }
            Key::Pr(repo, number) => {
                let r = self.tickets.review(repo, *number)?.clone();
                (header(self, format!("{}#{}", r.repo.rsplit('/').next().unwrap_or(&r.repo), number), r.hit.head.clone(), r.hit.url.clone(), "Open on GitHub", cx), self.render_pr_page(&r, cx))
            }
        };
        Some(
            div()
                .flex()
                .flex_col()
                .size_full()
                .child(head)
                .child(div().id("reader-scroll").flex_1().min_h_0().overflow_y_scroll().child(div().max_w(px(780.)).mx_auto().px_5().py_4().flex().flex_col().gap_4().child(content)))
                .into_any_element(),
        )
    }

    fn sessions_box(&self, work: &[Work], idle: Vec<(String, std::path::PathBuf, String)>, key: Key, reviewing: bool, start_note: String, cx: &mut Context<Self>) -> AnyElement {
        let needs = work.iter().any(|w| w.state == RowState::NeedsYou);
        let start_label = match (work.is_empty() && idle.is_empty(), reviewing) {
            (true, true) => "Start review".to_string(),
            (true, false) => "Start session".to_string(),
            (false, true) => "New review session".to_string(),
            (false, false) => format!("New session for {}", match &key {
                Key::Ticket(id) => id.clone(),
                Key::Pr(_, n) => format!("#{n}"),
            }),
        };
        let primary = work.is_empty() && idle.is_empty();
        let key_start = key.clone();
        div()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(if needs { Hsla { a: 0.7, ..sidebar::attention() } } else { theme::border_strong() })
            .bg(theme::raised())
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .pt_2p5()
                    .pb_2()
                    .child(ui::caps_label("SESSIONS"))
                    .when(!work.is_empty(), |d| d.child(div().text_size(px(11.5)).text_color(theme::faint()).child("Found by the ticket’s ID and pull requests"))),
            )
            .children(work.iter().enumerate().map(|(i, w)| {
                let w2 = w.clone();
                let (color, state_label) = match w.state {
                    RowState::NeedsYou => (sidebar::attention(), "needs you"),
                    RowState::Working => (theme::added(), "working"),
                    RowState::Idle => (theme::muted(), "idle"),
                };
                div()
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(theme::border())
                    .child(sidebar::agent_icon(w.agent, 22., w.running))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme::text_strong()).child(w.title.clone()))
                            .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(11.5)).text_color(theme::muted()).child(format!("{} · ⎇ {} · {}", w.agent.name(), w.branch, w.why))),
                    )
                    .child(div().flex().items_center().gap_1().text_size(px(11.5)).text_color(color).children(sidebar::status_icon(w.state, &format!("box-{i}"))).child(state_label))
                    .child(if i == 0 {
                        ui::primary_button(SharedString::from(format!("work-open-{i}")), "Open").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_work(&w2, window, cx))).into_any_element()
                    } else {
                        ui::button(SharedString::from(format!("work-open-{i}")), "Open").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_work(&w2, window, cx))).into_any_element()
                    })
                    .into_any_element()
            }))
            .children(idle.into_iter().enumerate().map(|(i, (project, path, branch))| {
                let key = key.clone();
                let pick = (project, path, branch.clone());
                div()
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(theme::border())
                    .child(div().flex_none().size(px(22.)).rounded_md().border_1().border_dashed().border_color(theme::border_strong()).flex().items_center().justify_center().text_color(theme::muted()).child("⎇"))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_family(theme::MONO_FONT).text_size(px(12.)).text_color(theme::text_strong()).overflow_hidden().whitespace_nowrap().text_ellipsis().child(branch))
                            .child(div().text_size(px(11.5)).text_color(theme::muted()).child("A worktree with no session")),
                    )
                    .child(ui::button(SharedString::from(format!("idle-start-{i}")), "Start here").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_start_ticket(key.clone(), Some(pick.clone()), window, cx))))
                    .into_any_element()
            }))
            .when(primary, |d| d.child(div().px_3().pb_2().text_color(theme::text_soft()).child(if reviewing { "No session is looking at it yet." } else { "No session is working on this yet." })))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2p5()
                    .border_t_1()
                    .border_color(theme::border())
                    .bg(theme::panel())
                    .child(if primary {
                        ui::primary_button("reader-start", start_label).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_start_ticket(key_start.clone(), None, window, cx))).into_any_element()
                    } else {
                        ui::button("reader-start", start_label).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_start_ticket(key_start.clone(), None, window, cx))).into_any_element()
                    })
                    .child(div().flex_1().min_w_0().text_size(px(11.5)).text_color(theme::muted()).child(start_note)),
            )
            .into_any_element()
    }

    fn render_ticket_page(&self, t: &Ticket, cx: &mut Context<Self>) -> AnyElement {
        let reviewing = self.tickets.reviewing().iter().any(|r| r.identifier == t.identifier);
        let work = self.work_on_ticket(t);
        let idle = self.idle_worktrees_for(t);
        let days = t.due.as_deref().and_then(|d| linear::days_until(d, now()));
        let prop = |content: AnyElement| div().flex().items_center().gap_1p5().h(px(26.)).px_2().rounded_md().border_1().border_color(theme::border_strong()).bg(theme::raised()).text_size(px(12.)).text_color(theme::text_soft()).child(content);
        let assignee = t.assignee.clone();
        let start_note = if reviewing {
            match t.pull_requests().first() {
                Some((repo, n)) => format!("Opens {}#{n} in a review worktree, with this ticket as context.", repo.rsplit('/').next().unwrap_or(repo)),
                None => "There is no pull request to review yet.".into(),
            }
        } else {
            format!("A new worktree on {} from the latest main. The ticket goes in as the first message.", linear::branch_for(&t.identifier, &t.title))
        };
        let links: Vec<(String, String, String, Option<Hsla>, String)> = {
            let mut out: Vec<(String, String, String, Option<Hsla>, String)> = Vec::new();
            for a in &t.attachments {
                if let Some((repo, n)) = a.pull_request() {
                    let state = self.tickets.review(&repo, n).map(|_| "Asks for your review".to_string()).unwrap_or_default();
                    out.push(("⇄".into(), if a.title.is_empty() { format!("Pull request #{n}") } else { tidy_pr_title(&a.title) }, format!("{repo}#{n}"), Some(theme::added()).filter(|_| !state.is_empty()), a.url.clone()));
                } else if a.team_task().is_some() {
                    out.push(("☑".into(), a.title.clone(), "Task".into(), None, a.url.clone()));
                } else {
                    out.push(("↗".into(), a.title.clone(), a.subtitle.clone(), None, a.url.clone()));
                }
            }
            out
        };
        let detail = self.tickets.details.get(&t.identifier);
        let description = match detail {
            Some(Loaded::Ready(d)) if !d.description.is_empty() => d.description.clone(),
            _ => t.description.clone(),
        };
        let comments: AnyElement = match detail {
            Some(Loaded::Ready(d)) if d.comments.is_empty() => div().text_color(theme::muted()).child("No comments yet.").into_any_element(),
            Some(Loaded::Ready(d)) => div()
                .flex()
                .flex_col()
                .gap_3()
                .children(d.comments.iter().enumerate().map(|(i, c)| {
                    div()
                        .flex()
                        .gap_2p5()
                        .when(c.reply, |d| d.pl_8())
                        .child(avatar(&initials(&c.author), avatar_color(&c.author), 24.))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .p_3()
                                .rounded_lg()
                                .border_1()
                                .border_color(theme::border())
                                .bg(theme::raised())
                                .child(div().flex().items_baseline().gap_2().child(bold(c.author.clone())).child(div().text_size(px(11.5)).text_color(theme::muted()).child(linear::short_date(&c.created_at))))
                                .child(self.render_markdown(&c.body, &format!("comment-{i}"), 13., cx)),
                        )
                }))
                .into_any_element(),
            Some(Loaded::Failed(e)) => div().text_color(theme::removed()).child(format!("Could not load the comments: {e}")).into_any_element(),
            _ => div().text_color(theme::muted()).child("Loading comments…").into_any_element(),
        };
        let comment_count = match detail {
            Some(Loaded::Ready(d)) => d.comments.len().to_string(),
            _ => String::new(),
        };
        let creator = t.creator.clone().unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().text_size(px(19.)).line_height(px(26.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(t.title.clone()))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1p5()
                    .child(prop(div().flex().items_center().gap_1p5().child(status_icon(&t.status, 12.)).child(t.status.name.clone()).into_any_element()))
                    .child(prop(div().flex().items_center().gap_1p5().child(priority_icon(t.priority)).child(t.priority_name()).into_any_element()))
                    .child(prop(match &assignee {
                        Some(a) => div().flex().items_center().gap_1p5().child(avatar(&initials(&a.name), avatar_color(&a.name), 16.)).child(a.name.clone()).into_any_element(),
                        None => div().child("No one").into_any_element(),
                    }))
                    .children(t.labels.iter().map(|l| prop(div().flex().items_center().gap_1p5().child(div().size(px(7.)).rounded_full().bg(label_color(l))).child(l.clone()).into_any_element())))
                    .when_some(t.due.clone(), |d, due| d.child(prop(div().when(days.is_some_and(|n| n <= 3), |d| d.text_color(sidebar::attention())).child(format!("Due {}", linear::short_date(&due))).into_any_element()))),
            )
            .child(div().text_size(px(12.)).text_color(theme::muted()).child(format!(
                "Created {}{}{}{}",
                linear::short_date(&t.created_at),
                if creator.is_empty() { String::new() } else { format!(" by {creator}") },
                t.completed_at.as_ref().map(|c| format!(" · done {}", linear::short_date(c))).unwrap_or_default(),
                t.parent.as_ref().map(|(id, _)| format!(" · part of {id}")).unwrap_or_default()
            )))
            .child(self.sessions_box(&work, idle, Key::Ticket(t.identifier.clone()), reviewing, start_note, cx))
            .when(!links.is_empty(), |d| {
                d.child(
                    div().flex().flex_col().gap_1p5().child(ui::caps_label("LINKED")).children(links.into_iter().enumerate().map(|(i, (glyph, title, sub, state_color, url))| {
                        div()
                            .id(SharedString::from(format!("link-{i}")))
                            .flex()
                            .items_center()
                            .gap_2p5()
                            .px_2p5()
                            .py_2()
                            .rounded_lg()
                            .border_1()
                            .border_color(theme::border())
                            .bg(theme::raised())
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .on_click(move |_: &ClickEvent, _, _| open_url(&url))
                            .child(div().w(px(16.)).text_color(theme::text_soft()).child(glyph))
                            .child(div().flex().flex_col().flex_1().min_w_0().child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme::text_strong()).child(title)).child(div().text_size(px(11.5)).text_color(theme::muted()).child(sub)))
                            .when_some(state_color, |d, c| d.child(div().text_size(px(11.5)).text_color(c).child("Asks for your review")))
                    })),
                )
            })
            .child(div().h(px(1.)).bg(theme::border()))
            .child(if description.trim().is_empty() {
                div().text_color(theme::muted()).child("No description.").into_any_element()
            } else {
                self.render_markdown(&description, "desc", 13.5, cx)
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .pt_2()
                    .child(div().flex().items_baseline().gap_2().child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child("Comments")).child(div().text_color(theme::muted()).child(comment_count)))
                    .child(comments)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(self.tickets.comment.clone())
                            .child(div().flex().justify_end().child(if self.tickets.posting {
                                ui::disabled_button("comment-post", "Posting…").into_any_element()
                            } else {
                                ui::primary_button("comment-post", "Comment").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.ticket_comment(cx))).into_any_element()
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_pr_page(&self, r: &glasscode_core::github::ReviewRequest, cx: &mut Context<Self>) -> AnyElement {
        let work = self.work_on_pr(&r.repo, r.hit.number);
        let tickets: Vec<Ticket> = linear::identifiers_in(&format!("{} {}", r.hit.title, r.hit.head)).into_iter().filter_map(|id| self.tickets.ticket(&id).cloned()).collect();
        let (merge, merge_color) = match (r.hit.draft, r.merge_state.as_str()) {
            (true, _) => ("Draft", theme::muted()),
            (_, "DIRTY") => ("Conflicts with its base", theme::removed()),
            (_, "BEHIND") => ("Behind its base", theme::modified()),
            (_, "BLOCKED") => ("Blocked", sidebar::attention()),
            (_, "CLEAN") => ("Ready to merge", theme::added()),
            _ => ("Open", theme::added()),
        };
        let prop = |content: AnyElement| div().flex().items_center().gap_1p5().h(px(26.)).px_2().rounded_md().border_1().border_color(theme::border_strong()).bg(theme::raised()).text_size(px(12.)).text_color(theme::text_soft()).child(content);
        let me = self.tickets.snapshot.as_ref().map(|s| s.viewer.name.clone()).unwrap_or_else(|| "You".into());
        let mut reviewers: Vec<(String, String, Hsla, String)> = vec![(format!("{me} (you)"), "Waiting on you".into(), sidebar::attention(), initials(&me))];
        for (who, state) in &r.reviews {
            let (label, color) = match state.as_str() {
                "APPROVED" => ("Approved", theme::added()),
                "CHANGES_REQUESTED" => ("Changes requested", theme::removed()),
                "COMMENTED" => ("Commented", theme::muted()),
                _ => ("Reviewed", theme::muted()),
            };
            reviewers.push((who.clone(), label.into(), color, initials(who)));
        }
        let note = format!("A review worktree on review/{}{}.", r.hit.number, tickets.first().map(|t| format!(", with {} as context", t.identifier)).unwrap_or_default());
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().text_size(px(19.)).line_height(px(26.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(tidy_pr_title(&r.hit.title)))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1p5()
                    .child(prop(div().flex().items_center().gap_1p5().child(div().size(px(7.)).rounded_full().bg(merge_color)).child(merge).into_any_element()))
                    .child(prop(div().child(format!("{} files", r.hit.changed_files)).into_any_element()))
                    .child(prop(div().flex().gap_1().child(div().text_color(theme::added()).child(format!("+{}", r.hit.additions))).child(div().text_color(theme::removed()).child(format!("−{}", r.hit.deletions))).into_any_element()))
                    .when(!r.hit.author.is_empty(), |d| d.child(prop(div().flex().items_center().gap_1p5().child(avatar(&initials(&r.hit.author), avatar_color(&r.hit.author), 16.)).child(r.hit.author.clone()).into_any_element()))),
            )
            .child(div().text_size(px(12.)).text_color(theme::muted()).child(format!("{} → {} · {} · opened {} · updated {}", r.hit.head, r.hit.base, r.repo, linear::short_date(&r.created_at), linear::short_date(&r.hit.updated_at))))
            .child(self.sessions_box(&work, Vec::new(), Key::Pr(r.repo.clone(), r.hit.number), true, note, cx))
            .child(
                div().flex().flex_col().child(ui::caps_label("REVIEWERS")).children(reviewers.into_iter().enumerate().map(|(i, (who, label, color, letters))| {
                    div()
                        .flex()
                        .items_center()
                        .gap_2p5()
                        .py_2()
                        .when(i > 0, |d| d.border_t_1().border_color(theme::border()))
                        .child(avatar(&letters, avatar_color(&who), 22.))
                        .child(div().flex_1().text_color(theme::text_strong()).child(who))
                        .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(color).child(label))
                })),
            )
            .children(tickets.into_iter().map(|t| {
                let key = Key::Ticket(t.identifier.clone());
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(ui::caps_label("ITS TICKET"))
                    .child(
                        div()
                            .id(SharedString::from(format!("pr-ticket-{}", t.identifier)))
                            .flex()
                            .items_center()
                            .gap_2p5()
                            .px_2p5()
                            .py_2()
                            .rounded_lg()
                            .border_1()
                            .border_color(theme::border())
                            .bg(theme::raised())
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_ticket(key.clone(), cx)))
                            .child(status_icon(&t.status, 13.))
                            .child(div().text_size(px(12.)).text_color(theme::muted()).child(t.identifier.clone()))
                            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme::text_strong()).child(t.title.clone()))
                            .child(div().text_size(px(12.)).text_color(theme::muted()).child("Read →")),
                    )
            }))
            .child(div().text_size(px(12.)).text_color(theme::muted()).child("The diff, threads and comments stay on GitHub. Start review opens it in a worktree here, with Glasscode's review steps."))
            .into_any_element()
    }

    // ----- Markdown, with links --------------------------------------------------------------

    pub fn render_markdown(&self, text: &str, id: &str, size: f32, cx: &mut Context<Self>) -> AnyElement {
        let blocks = markdown::parse(&linear::markdown(text));
        let ws = cx.entity().downgrade();
        let line = |spans: &[Span], key: String, size: f32, color: Hsla| -> AnyElement { rich(spans, key, size, color, ws.clone()) };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(blocks.into_iter().enumerate().map(|(i, block)| {
                let key = format!("{id}-{i}");
                match block {
                    Block::Heading { level, text } => div().pt_1().font_weight(FontWeight::SEMIBOLD).child(line(&text, key, if level <= 2 { size + 2.5 } else { size + 1. }, theme::text_strong())).into_any_element(),
                    Block::Paragraph(text) => line(&text, key, size, theme::text()),
                    Block::Item { number, depth, text } => div()
                        .flex()
                        .gap_2()
                        .pl(px(depth as f32 * 16.))
                        .child(div().flex_none().w(px(16.)).text_size(px(size)).text_color(theme::muted()).child(number.map(|n| format!("{n}.")).unwrap_or_else(|| "•".into())))
                        .child(div().flex_1().min_w_0().child(line(&text, key, size, theme::text())))
                        .into_any_element(),
                    Block::Check { done, depth, text } => div()
                        .flex()
                        .gap_2()
                        .pl(px(depth as f32 * 16.))
                        .child(div().mt(px(3.)).child(ui::checkbox(SharedString::from(format!("{key}-box")), done)))
                        .child(div().flex_1().min_w_0().child(line(&text, key, size, if done { theme::text_soft() } else { theme::text() })))
                        .into_any_element(),
                    Block::Code(code) => div()
                        .p_2p5()
                        .rounded_md()
                        .bg(theme::bg())
                        .border_1()
                        .border_color(theme::border())
                        .font_family(theme::MONO_FONT)
                        .text_size(px(11.5))
                        .text_color(theme::text_soft())
                        .children(code.lines().map(|l| div().child(l.to_string())).collect::<Vec<_>>())
                        .into_any_element(),
                    Block::Quote(text) => div().flex().gap_2().py_1().px_2().rounded_md().bg(theme::hover()).child(div().w(px(3.)).flex_none().rounded_full().bg(theme::border_strong())).child(div().flex_1().min_w_0().child(line(&text, key, size, theme::text_soft()))).into_any_element(),
                    Block::Table(rows) => {
                        let columns = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
                        div()
                            .flex()
                            .flex_col()
                            .rounded_md()
                            .border_1()
                            .border_color(theme::border())
                            .overflow_hidden()
                            .children(rows.into_iter().enumerate().map(|(r, row)| {
                                let key = format!("{key}-{r}");
                                div()
                                    .flex()
                                    .when(r == 0, |d| d.bg(theme::hover()).font_weight(FontWeight::SEMIBOLD))
                                    .when(r > 0, |d| d.border_t_1().border_color(theme::border()))
                                    .children((0..columns).map(|c| {
                                        let cell = row.get(c).cloned().unwrap_or_default();
                                        div().flex_1().min_w(px(90.)).px_2p5().py_1p5().child(rich(&cell, format!("{key}-{c}"), size - 0.5, theme::text(), ws.clone()))
                                    }))
                            }))
                            .into_any_element()
                    }
                    Block::Rule => div().h(px(1.)).bg(theme::border()).into_any_element(),
                }
            }))
            .into_any_element()
    }

    pub fn open_linear_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_accounts(window, cx);
        if let Some(form) = self.settings.form.as_mut() {
            form.linear_first = true;
        }
    }
}

fn quick_test(quick: Quick, card: &Card) -> bool {
    match quick {
        Quick::All => true,
        Quick::NeedsYou => card.worst() == Some(RowState::NeedsYou),
        Quick::Working => card.worst() == Some(RowState::Working),
        Quick::NoSession => card.work.is_empty() && !card.done,
        Quick::DueSoon => card.due_soon,
    }
}

/// One line of rich text: bold, italic, code, and links you can click (a ticket here opens
/// in Tickets; anything else in the browser).
fn rich(spans: &[Span], key: String, size: f32, color: Hsla, ws: gpui::WeakEntity<Workspace>) -> AnyElement {
    let mut text = String::new();
    let mut highlights: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    let mut mono: Vec<(Range<usize>, SharedString)> = Vec::new();
    let mut links: Vec<(Range<usize>, String, String)> = Vec::new();
    for span in spans {
        let start = text.len();
        text.push_str(&span.text);
        let range = start..text.len();
        if range.is_empty() {
            continue;
        }
        let mut style = HighlightStyle::default();
        if span.bold {
            style.font_weight = Some(FontWeight::SEMIBOLD);
            style.color = Some(theme::text_strong());
        }
        if span.italic {
            style.font_style = Some(FontStyle::Italic);
        }
        if span.code {
            style.color = Some(theme::code());
            mono.push((range.clone(), SharedString::from(theme::MONO_FONT)));
        }
        if let Some(link) = &span.link {
            style.color = Some(theme::link());
            style.underline = Some(UnderlineStyle { thickness: px(1.), color: Some(Hsla { a: 0.5, ..theme::link() }), wavy: false });
            links.push((range.clone(), link.clone(), span.text.clone()));
        }
        if style != HighlightStyle::default() {
            highlights.push((range, style));
        }
    }
    let styled = StyledText::new(text).with_highlights(highlights).with_font_family_overrides(mono);
    let body: AnyElement = if links.is_empty() {
        styled.into_any_element()
    } else {
        let ranges: Vec<Range<usize>> = links.iter().map(|(r, _, _)| r.clone()).collect();
        let targets: Vec<(String, String)> = links.into_iter().map(|(_, url, label)| (url, label)).collect();
        InteractiveText::new(SharedString::from(key), styled)
            .on_click(ranges, move |index, _window, cx| {
                let Some((url, label)) = targets.get(index).cloned() else { return };
                let ticket = linear::identifiers_in(&label).into_iter().next().filter(|_| url.contains("linear.app"));
                let opened = ticket.is_some_and(|id| {
                    ws.update(cx, |this, cx| {
                        if this.tickets.ticket(&id).is_some() {
                            this.open_ticket(Key::Ticket(id.clone()), cx);
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false)
                });
                if !opened {
                    open_url(&url);
                }
            })
            .into_any_element()
    };
    div().text_size(px(size)).line_height(px(size * 1.55)).text_color(color).child(body).into_any_element()
}
