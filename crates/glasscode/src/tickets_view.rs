//! Drawing the Board (state and actions are in `tickets_ui.rs`): the toolbar and quick filters,
//! the columns of cards or the list, and a ticket's page in a panel over the page. The Board stands
//! on the backdrop with glass, as Today, the sessions and Settings do (the owner, 2026-11-17;
//! the mock draws it opaque): the toolbar and each column on `glass`, each card on
//! `glass_strong`. Text on glass is `text`, `text_soft` or `card_soft_text`; a colour that means
//! something goes on a dot or an icon beside it. The reader's panel stays opaque.
//!
//! Motion (`motion.rs`): the cards fade in once when they first show, the reader's panel slides in
//! over a scrim, and only a fetch that runs moves anything for longer (the skeleton columns of
//! the first one, the arc in the Sync button).

use std::ops::Range;

use gpui::{
    AnyElement, ClickEvent, Context, FontStyle, FontWeight, HighlightStyle, Hsla, InteractiveText, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, SharedString, StyledText,
    UnderlineStyle, Window, deferred, div, prelude::*, px,
};
use glasscode_core::linear::{self, Status, Ticket};
use glasscode_core::markdown::{self, Block, Span};
use glasscode_core::model::{AgentKind, Provider, now};

use crate::backdrop;
use crate::center::open_url;
use crate::icons::{self, Brand};
use crate::motion;
use crate::sidebar::{self, RowState};
use crate::theme;
use crate::tickets_ui::{Key, Layout, Loaded, Quick, Scope, Work, bold, label_color};
use crate::ui;
use crate::workspace::{Side, Workspace};

pub(crate) const READER_WIDTH: f32 = 540.;
/// The strength of the backdrop behind the Board and the Pull requests page: Today's, as they
/// are pages of the same kind (cards on glass).
pub(crate) const BACKDROP: f32 = 0.85;
/// A column starts this wide and grows with the room, up to `COLUMN_MAX`; past the room the
/// board scrolls sideways.
const COLUMN_WIDTH: f32 = 260.;
const COLUMN_MAX: f32 = 380.;

/// The mark of where a ticket comes from.
pub fn source_brand(provider: Provider) -> Brand {
    match provider {
        Provider::Linear => Brand::Linear,
        Provider::GitHub => Brand::Github,
    }
}

/// The agents of the sessions on a card, each once, in the order of the sessions, at most three.
fn card_agents(work: &[Work]) -> Vec<(AgentKind, bool)> {
    let mut out: Vec<(AgentKind, bool)> = Vec::new();
    for w in work {
        match out.iter().position(|(agent, _)| *agent == w.agent) {
            Some(i) => out[i].1 |= w.running,
            None if out.len() < 3 => out.push((w.agent, w.running)),
            None => {}
        }
    }
    out
}

/// The button of a card: open the session that works on it, or start a workspace (a worktree and
/// a session) for it. A done card with no session has none; its page in the reader still starts
/// one.
fn card_action(has_work: bool, done: bool, reviewing: bool, idle_worktree: bool) -> Option<&'static str> {
    if has_work {
        Some("Open session")
    } else if done {
        None
    } else if reviewing {
        Some("Start review")
    } else if idle_worktree {
        Some("Start in worktree")
    } else {
        Some("Start workspace")
    }
}

/// A card on the board (and a row in the list).
pub struct Card {
    pub key: Key,
    /// Where it comes from: Linear, or GitHub (an issue or a pull request).
    pub source: Brand,
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
            .w(theme::sz(COLUMN_WIDTH - 28.))
            .p_2p5()
            .rounded_lg()
            .border_1()
            .border_color(theme::accent())
            .bg(theme::raised())
            .shadow_lg()
            .opacity(0.95)
            .child(div().text_size(theme::sz(11.5)).text_color(theme::muted()).child(self.identifier.clone()))
            .child(div().text_size(theme::sz(13.)).font_weight(FontWeight::MEDIUM).text_color(theme::text_strong()).overflow_hidden().whitespace_nowrap().text_ellipsis().child(self.title.clone()))
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
    let ring = div().flex_none().size(theme::sz(size)).rounded_full().border_2().border_color(color).flex().items_center().justify_center();
    match status.kind.as_str() {
        "backlog" => ring.border_dashed().into_any_element(),
        "started" => {
            let dot = if status.name.to_lowercase().contains("review") { size * 0.5 } else { size * 0.3 };
            ring.child(div().size(theme::sz(dot)).rounded_full().bg(color)).into_any_element()
        }
        "completed" | "canceled" | "triage" => div()
            .flex_none()
            .size(theme::sz(size))
            .rounded_full()
            .bg(color)
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme::bg())
            .text_size(theme::sz(size * 0.7))
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
            .size(theme::sz(13.))
            .rounded(theme::sz(3.))
            .bg(sidebar::attention())
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme::on_attention())
            .text_size(theme::sz(10.))
            .font_weight(FontWeight::BOLD)
            .child("!")
            .into_any_element(),
        2..=4 => div()
            .flex_none()
            .flex()
            .items_end()
            .gap(theme::sz(1.5))
            .h(theme::sz(12.))
            .children([4., 7.5, 11.].into_iter().enumerate().map(|(i, h)| {
                let lit = i < (5 - priority as usize).min(3);
                div().w(theme::sz(3.)).h(theme::sz(h)).rounded(px(1.)).bg(if lit { theme::text_soft() } else { theme::border_strong() })
            }))
            .into_any_element(),
        // In a colour that reads on glass too (a card on the Board).
        _ => div().flex_none().text_color(theme::card_soft_text()).text_size(theme::sz(11.)).child("···").into_any_element(),
    }
}

fn avatar(initials: &str, color: Hsla, size: f32) -> gpui::Div {
    div()
        .flex_none()
        .size(theme::sz(size))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(color)
        .text_color(theme::c(0xffffff))
        .text_size(theme::sz(size * 0.42))
        .font_weight(FontWeight::BOLD)
        .child(initials.to_string())
}

/// A tag on a card: a label, a pull request, a due date.
fn chip(text: impl Into<SharedString>, dot: Option<Hsla>) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(theme::sz(5.))
        .h(theme::sz(20.))
        .px(theme::sz(7.))
        .rounded(theme::sz(5.))
        .bg(theme::hover())
        .text_size(theme::sz(11.))
        .text_color(theme::text_soft())
        .whitespace_nowrap()
        .when_some(dot, |d, c| d.child(div().size(theme::sz(7.)).rounded_full().bg(c)))
        .child(text.into())
}

fn review_tag() -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .h(theme::sz(17.))
        .px_1p5()
        .rounded_full()
        .border_1()
        .border_color(theme::border_strong())
        .text_color(theme::text_soft())
        .text_size(theme::sz(10.))
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
            source: source_brand(t.provider),
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
            source: Brand::Github,
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

    /// The board or the list, under the toolbar. A ticket being read is a panel over the page
    /// (`render_reader_overlay`), or stands in the place of the board when it is expanded.
    pub fn render_tickets(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.tickets_unconnected() {
            // The connect card is opaque (a dialog's ground) over the backdrop.
            let layer = self.backdrop_layer(BACKDROP, window, cx);
            return div()
                .relative()
                .flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .items_center()
                .justify_center()
                .p(theme::sz(16.))
                .bg(theme::bg())
                .child(layer)
                .child(
                    div()
                        // Narrower in a narrow centre (a small window, or at 130 %), not cut.
                        .w_full()
                        .max_w(theme::sz(560.))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .p_6()
                        .rounded_xl()
                        .border_1()
                        .border_color(theme::border_strong())
                        .bg(theme::dialog())
                        .child(div().text_size(theme::sz(18.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child("Your Linear tickets, here"))
                        .child(div().text_color(theme::muted()).child("The tickets assigned to you and the ones you review, on a board, each with the sessions working on it. Start a session from any ticket."))
                        .child(crate::tickets_ui::render_connect(self, cx)),
                )
                .into_any_element();
        }
        let (cards, _) = self.cards(cx);
        // The board scrolls sideways, and each column down on its own (as in the mock); the list
        // scrolls down.
        let body = match self.tickets.layout {
            // The first fetch is not back and nothing is kept from before: columns that wait.
            _ if self.board_first_load() => board_skeleton(),
            Layout::Board => div()
                .id("tickets-body")
                // A row, so the board in it is as wide as its columns: what scrolls is the width
                // of the child itself, and a child of a block is never wider than the block.
                .flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_x_scroll()
                .restrict_scroll_to_axis()
                .track_scroll(&self.tickets.board_scroll)
                // A mouse wheel has no sideways gesture: over a column whose cards all show (or
                // between columns) it moves the board sideways; over a column with more cards it
                // scrolls that column. Shift with the wheel moves the board anywhere.
                .on_scroll_wheel(cx.listener(|this, event: &gpui::ScrollWheelEvent, window, cx| {
                    let delta = event.delta.pixel_delta(window.line_height());
                    if delta.x != px(0.) || delta.y == px(0.) {
                        return;
                    }
                    let column_scrolls = this.tickets.column_scrolls.borrow().values().any(|h| h.bounds().contains(&event.position) && h.max_offset().y > px(0.));
                    if column_scrolls {
                        return;
                    }
                    let board = &this.tickets.board_scroll;
                    let mut offset = board.offset();
                    let before = offset.x;
                    offset.x = (offset.x + delta.y).clamp(-board.max_offset().x, px(0.));
                    if offset.x != before {
                        board.set_offset(offset);
                        cx.notify();
                    }
                }))
                .px(theme::sz(12.))
                .pt(theme::sz(10.))
                .pb(theme::sz(12.))
                .child(self.render_board(&cards, cx))
                .into_any_element(),
            Layout::List => div().id("tickets-body").flex_1().min_w_0().h_full().overflow_scroll().px(theme::sz(14.)).py(theme::sz(12.)).child(self.render_list(&cards, cx)).into_any_element(),
        };
        // The keyboard goes into the reader when it opens, and back to the window when it closes.
        // `open_ticket` and `close_reader` have no window: they leave it to this draw. A ticket
        // that is not on the board yet (it opens before the first fetch is back) waits.
        // (The board is connected here, so the reader shows when its ticket is found.)
        let reading = self.reader_found();
        match self.tickets.reader_focus_due {
            Some(true) if reading => {
                self.tickets.reader_focus_due = None;
                window.focus(&self.tickets.reader_focus, cx);
            }
            Some(false) => {
                self.tickets.reader_focus_due = None;
                if self.modal.is_none() && self.palette.is_none() {
                    window.focus(self.focus_handle_ref(), cx);
                }
            }
            // The reader shows and the keyboard is on the window itself (the person came back to
            // the Board, or a dialog over the reader closed): the reader has it again.
            None if reading && self.modal.is_none() && self.palette.is_none() && self.focus_handle_ref().is_focused(window) => window.focus(&self.tickets.reader_focus, cx),
            _ => {}
        }
        // Debug builds (`board:…:esc`): Esc as from the keyboard, to check that the reader has it.
        if cfg!(debug_assertions) && std::mem::take(&mut self.tickets.debug_escape) {
            window.defer(cx, |window, cx| {
                if let Ok(escape) = gpui::Keystroke::parse("escape") {
                    window.dispatch_keystroke(escape, cx);
                }
            });
        }
        let expanded = if self.tickets.expanded { self.render_reader(window, cx) } else { None };
        let overlay = if reading && !self.tickets.expanded { self.render_reader_overlay(window, cx) } else { None };
        // The toolbar and the quick filters on one glass panel over the backdrop. In the narrowest
        // centre (900 x 560 at 130 %) it is four short rows, a little under half the height;
        // should it ever need more than half, it scrolls, so the columns stay in view.
        let toolbar = backdrop::glass(theme::sz(12.))
            .flex()
            .flex_none()
            .flex_col()
            .max_h(gpui::relative(0.5))
            .mx(theme::sz(PAGE_MARGIN))
            .mt(theme::sz(12.))
            .when(theme::is_light(), |d| d.shadow_md())
            .child(div().id("tickets-toolbar").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().child(self.render_tickets_toolbar(window, cx)));
        let layer = self.backdrop_layer(BACKDROP, window, cx);
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
            .child(toolbar)
            .child(div().flex().flex_1().min_h_0().when(expanded.is_none(), |d| d.child(body)).when_some(expanded, |d, reader| {
                d.child(self.reader_keys(div().flex().flex_col().flex_1().h_full().min_w_0().bg(theme::panel()).border_l_1().border_color(theme::border()), cx).child(reader))
            }))
            .children(overlay)
            .into_any_element()
    }

    /// The first fetch of the board is on its way and nothing is kept from an earlier one: the
    /// only time the columns show as skeletons. A later fetch keeps the cards, and the Sync
    /// button turns.
    fn board_first_load(&self) -> bool {
        self.tickets.loading && self.tickets.snapshot.is_none() && self.tickets.reviews.is_empty()
    }

    /// The selected ticket or pull request is on the board (so its page can be drawn).
    fn reader_found(&self) -> bool {
        match &self.tickets.selected {
            Some(Key::Ticket(id)) => self.tickets.ticket(id).is_some(),
            Some(Key::Pr(repo, number)) => self.tickets.review(repo, *number).is_some(),
            None => false,
        }
    }

    /// The reader's edge follows the mouse while it is dragged, within the limits of its width.
    fn reader_drag_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some((start, width)) = self.tickets.reader_drag else { return };
        if event.pressed_button != Some(MouseButton::Left) {
            self.reader_drag_end(cx);
            return;
        }
        // The drag is in pixels on the screen; the layout keeps the width at 100 %.
        let (min, max) = crate::sessions_ui::edge_limits(Side::Tickets);
        let moved = (f32::from(event.position.x) - start) / theme::scale();
        self.state.layout.tickets = Some((width - moved).clamp(min, max).round());
        cx.notify();
    }

    /// The edge is let go: the width is kept.
    fn reader_drag_end(&mut self, cx: &mut Context<Self>) {
        if self.tickets.reader_drag.take().is_some() {
            self.save();
            cx.notify();
        }
    }

    /// The width of the Board page on the screen: the window less the left sidebar.
    fn board_width(&self) -> f32 {
        let left = if self.view_has_left() && !self.state.layout.left_hidden { self.sidebar_width() } else { 0. };
        (self.window_width - left).max(0.)
    }

    /// The width the reader shows at, on the screen: as the person left it, at the scale, and
    /// never so wide that nothing of the page is left beside it to click.
    pub(crate) fn reader_width(&self) -> f32 {
        let (page, scale) = (self.board_width(), theme::scale());
        (self.saved_width(Side::Tickets) * scale).min(page - 56. * scale).max((320. * scale).min(page))
    }

    /// The width `sessions_ui` asks for the reader's edge (the reader is not a sidebar any more:
    /// its own edge is in `render_reader_overlay`).
    pub fn tickets_width(&self) -> f32 {
        self.shown_width(Side::Tickets)
    }

    /// The reader holds the keyboard while it shows, and Esc closes it.
    fn reader_keys(&self, panel: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        panel.track_focus(&self.tickets.reader_focus).on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" {
                this.close_reader(cx);
                cx.stop_propagation();
            }
        }))
    }

    /// The ticket being read: a panel that slides in at the right edge, over the whole Board page
    /// (the toolbar too), with a scrim over the rest. The page under it keeps its width, takes no
    /// click and does not scroll; a click on the scrim, Esc or × closes the panel. Its left edge
    /// drags to another width, and a double click on it brings back the first one.
    fn render_reader_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let reader = self.render_reader(window, cx)?;
        let width = px(self.reader_width());
        let dragging = self.tickets.reader_drag.is_some();
        let scrim = div().absolute().top_0().left_0().size_full().bg(theme::overlay()).on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                this.close_reader(cx);
                cx.stop_propagation();
            }),
        );
        let edge = div()
            .id("reader-edge")
            .absolute()
            .top_0()
            .left(theme::sz(-3.))
            .w(theme::sz(6.))
            .h_full()
            .cursor_col_resize()
            .when(dragging, |d| d.bg(theme::border_strong()))
            .hover(|s| s.bg(theme::border_strong()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.tickets.reader_drag = Some((f32::from(event.position.x), this.reader_width() / theme::scale()));
                    // Not a click on the scrim under the edge.
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                if event.click_count() >= 2 {
                    this.state.layout.tickets = None;
                    this.save();
                    cx.notify();
                }
            }));
        let panel = self
            .reader_keys(div().absolute().top_0().bottom_0().right_0().w(width).flex().flex_col(), cx)
            // A click in the panel is not a click on the scrim. So the scrim's layer does not hear
            // the mouse over the panel: the drag of the edge is followed here too.
            .occlude()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| this.reader_drag_move(event, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, cx| this.reader_drag_end(cx)))
            .bg(theme::panel())
            .border_l_1()
            .border_color(theme::border_strong())
            .shadow_lg()
            .child(reader)
            .child(edge);
        Some(
            // Over the page; under a dialog (5), which the panel can open, and the status bar's lists (2).
            deferred(
                div()
                    .id("reader-overlay")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .overflow_hidden()
                    // The page under it takes no click and no wheel.
                    .occlude()
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| this.reader_drag_move(event, cx)))
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, cx| this.reader_drag_end(cx)))
                    // One id for each: the panel comes in when it opens, and not again when another
                    // ticket is opened from it (the page in it fades in, `render_reader`).
                    .child(motion::scrim_in("reader-scrim", scrim))
                    .child(motion::slide_in_right("reader-panel", width, panel)),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// The toolbar: where the tickets come from (the provider, and its repository or team with
    /// the state of the connection), whose they are, then the filter, Board or List, and Sync;
    /// under it the quick filters. Each control is `CONTROL` high. The three groups wrap as wholes
    /// when the page is narrower, and the filter is what gives way first. In a page narrower than
    /// `COMPACT_BELOW` (the smallest window with the sidebar, or 130 %) each group is a row of its
    /// own with shorter words, and the quick filters are one row that scrolls sideways.
    fn render_tickets_toolbar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // What is left in the glass panel for the controls, as at 100 %.
        let room = self.board_width() / theme::scale() - 2. * PAGE_MARGIN - 2. * TOOLBAR_PAD;
        let compact = room < COMPACT_BELOW;
        // Words beside the icons while there is room for them; then the icons stand for
        // themselves (each says its word when the pointer rests on it), and the toolbar stays one
        // row.
        let words = room >= WORDS_FROM;
        // No counts before the first fetch is back: a 0 would say that there is nothing.
        let counted = !self.board_first_load();
        let mine = self.tickets.mine().into_iter().filter(|t| !t.is_done()).count();
        let reviewing = self.tickets.reviewing().into_iter().filter(|t| !t.is_done()).count() + self.tickets.reviews.len();
        let snapshot = self.tickets.snapshot.as_ref();
        let provider = self.tickets_provider();
        let error = self.tickets.error.clone().or_else(|| self.tickets.reviews_error.clone().map(|e| format!("GitHub: {e}")));

        // ----- the source: provider, then repository or team with the state of the connection
        let (state_color, state_words) = if self.tickets.error.is_some() {
            (theme::removed(), "The last sync failed")
        } else if self.tickets.connected {
            (theme::added(), "Connected")
        } else {
            (theme::faint(), "Not connected")
        };
        let account = match provider {
            Provider::Linear => snapshot.map(|s| format!("{} · {}", s.org, self.state.linear.me_label.clone().unwrap_or_else(|| if s.viewer.email.is_empty() { s.viewer.name.clone() } else { s.viewer.email.clone() }))),
            Provider::GitHub => self.tickets_github_repo(),
        };
        let provider_select = {
            let detail = |of: Provider, words: &str| if of == provider { account.clone().unwrap_or_else(|| words.to_string()) } else { words.to_string() };
            let choices = vec![
                Choice::new("Linear", detail(Provider::Linear, "Your Linear tickets and reviews"), provider == Provider::Linear, |this, _, cx| this.tickets_pick_provider(Provider::Linear, cx)),
                Choice::new("GitHub Issues", detail(Provider::GitHub, "Issues in a GitHub repository"), provider == Provider::GitHub, |this, _, cx| this.tickets_pick_provider(Provider::GitHub, cx)),
                Choice::new("Connection settings…", state_words, false, |this, window, cx| this.open_linear_settings(window, cx)).after_rule(),
            ];
            select("tickets-provider", Some(icons::brand(source_brand(provider), 14.)), provider.label().to_string(), self.tickets.picker == Some("tickets-provider"), false, choices, cx)
        };
        let project_select = {
            let mut choices: Vec<Choice> = Vec::new();
            let value = match provider {
                Provider::Linear => {
                    choices.push(Choice::new("All teams", "The whole workspace", self.state.tickets.linear_team.is_none(), |this, _, cx| this.tickets_pick_project(None, cx)));
                    for opt in self.tickets.project_options.iter().flatten() {
                        let on = self.state.tickets.linear_team.as_deref() == Some(opt.id.as_str());
                        let id = opt.id.clone();
                        choices.push(Choice::new(opt.label.clone(), opt.detail.clone(), on, move |this, _, cx| this.tickets_pick_project(Some(id.clone()), cx)));
                    }
                    self.state.tickets.linear_team.clone().unwrap_or_else(|| "All teams".into())
                }
                Provider::GitHub => {
                    let current = self.tickets_github_repo();
                    for opt in self.tickets.project_options.iter().flatten() {
                        let on = current.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(&opt.id));
                        let id = opt.id.clone();
                        choices.push(Choice::new(opt.label.clone(), opt.detail.clone(), on, move |this, _, cx| this.tickets_pick_project(Some(id.clone()), cx)));
                    }
                    current.unwrap_or_else(|| "Pick a repository".into())
                }
            };
            if choices.is_empty() {
                choices.push(Choice::new("Loading…", "", false, |_, _, _| {}));
            }
            // The dot is the connection: green when it works, red when the last sync failed.
            select("tickets-project", Some(ui::dot(state_color).into_any_element()), value, self.tickets.picker == Some("tickets-project"), compact, choices, cx)
        };
        let source = div()
            .flex()
            .items_center()
            .gap(theme::sz(6.))
            .min_w_0()
            .child(div().flex_none().child(provider_select))
            .child(div().min_w_0().when(compact, |d| d.flex_1()).when(!compact, |d| d.flex_none().w(theme::sz(190.))).child(project_select));

        // ----- whose
        let scope_tab = |id: &'static str, scope: Scope, glyph: &'static [u8], label: &'static str, short: &'static str, count: usize| {
            let on = self.tickets.scope == scope;
            segment(id, on, compact)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.tickets.scope = scope;
                    cx.notify();
                }))
                .tooltip(ui::tip(label))
                .child(icons::icon(glyph, theme::sz(14.), theme::sz(14.), if on { theme::text_strong() } else { theme::text_soft() }))
                // A compact page gives the control a row of its own, with room for short words.
                .when(words || compact, |d| d.child(if words { label } else { short }))
                .when(counted, |d| d.child(div().text_size(theme::sz(11.5)).text_color(theme::card_soft_text()).child(count.to_string())))
        };
        let scope = segmented()
            .when(compact, |d| d.w_full())
            .child(scope_tab("scope-mine", Scope::Mine, icons::USER, "Assigned to me", "Mine", mine))
            .child(scope_tab("scope-review", Scope::Review, icons::EYE, "To review", "Review", reviewing))
            .child(scope_tab("scope-both", Scope::Both, icons::LAYERS, "Both", "Both", mine + reviewing));

        // ----- the filter, Board or List, Sync
        let layout_tab = |id: &'static str, layout: Layout, glyph: &'static [u8], label: &'static str| {
            let on = self.tickets.layout == layout;
            segment(id, on, false)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.tickets.layout = layout;
                    cx.notify();
                }))
                .tooltip(ui::tip(if layout == Layout::Board { "Columns by status" } else { "One list, grouped by status" }))
                .aria_label(label)
                .child(icons::icon(glyph, theme::sz(14.), theme::sz(14.), if on { theme::text_strong() } else { theme::text_soft() }))
        };
        let searching = self.tickets.query.read(cx).focus_handle_clone().is_focused(window);
        // The text field draws its own frame, 36 high (`text_input.rs`). Here it shows through a
        // window in a frame of the toolbar's height, which cuts its frame off on each side: its
        // ground is the same colour, so only its text and caret are seen.
        let search = control()
            .flex_1()
            .min_w(theme::sz(if compact { 96. } else { 140. }))
            .when(!compact, |d| d.max_w(theme::sz(260.)))
            .gap(theme::sz(2.))
            .pl(theme::sz(10.))
            .pr(theme::sz(4.))
            .bg(theme::input())
            .when(searching, |d| d.border_color(theme::accent()))
            .child(icons::icon(icons::SEARCH, theme::sz(14.), theme::sz(14.), theme::faint()))
            .child(div().relative().flex_1().min_w_0().h_full().overflow_hidden().child(div().absolute().top(px(-(2. * theme::scale() + 1.))).left(theme::sz(-8.)).right(theme::sz(-8.)).child(self.tickets.query.clone())));
        let synced = if self.tickets.loading { "Syncing…".to_string() } else { synced_label(snapshot.map(|s| now().saturating_sub(s.fetched_at))) };
        // As wide while it syncs as after: the controls beside it do not move.
        let sync = control()
            .id("tickets-sync")
            .gap(theme::sz(7.))
            .flex_none()
            .w(theme::sz(CONTROL))
            .justify_center()
            .tooltip(ui::tip(format!("{synced}. Sync now")))
            .cursor_pointer()
            .hover(|s| s.bg(theme::hover()))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.tickets.last_fetch = None;
                this.tickets_refresh(cx);
                cx.notify();
            }))
            // The arc turns only while the fetch runs.
            .child(if self.tickets.loading {
                motion::spinner("tickets-sync-spin", 14., theme::text_soft()).into_any_element()
            } else {
                icons::icon(icons::REFRESH, theme::sz(14.), theme::sz(14.), theme::text_soft()).into_any_element()
            });
        let view = div()
            .flex()
            .items_center()
            .justify_end()
            .gap(theme::sz(8.))
            .min_w_0()
            // On the line of the other groups while it has this much room; else on the next one,
            // which it fills.
            .when(!compact, |d| d.flex_grow(1.).flex_shrink(1.).flex_basis(theme::sz(VIEW_GROUP_ICONS)))
            .child(search)
            .child(segmented().child(layout_tab("layout-board", Layout::Board, icons::COLUMNS, "Board")).child(layout_tab("layout-list", Layout::List, icons::LIST, "List")))
            .child(sync);

        // ----- the quick filters
        let (shown, rest) = self.cards(cx);
        let all: Vec<&Card> = shown.iter().chain(rest.iter()).collect();
        // Each quick filter has an icon; the two that are a state have the state's colour.
        let filters = [
            (Quick::All, "Everything", icons::GRID, None),
            (Quick::NeedsYou, "Needs you", icons::BELL, Some(sidebar::attention())),
            (Quick::Working, "Agent working", icons::ACTIVITY, Some(theme::added())),
            (Quick::NoSession, "No session yet", icons::ADD, None),
            (Quick::DueSoon, "Due in 7 days", icons::CALENDAR, None),
        ];
        let quick = div()
            .id("tickets-quick")
            .flex()
            .items_center()
            .gap(theme::sz(6.))
            // One row that scrolls sideways in a narrow page; else the chips wrap.
            .when(compact, |d| d.overflow_x_scroll())
            .when(!compact, |d| d.flex_wrap())
            .children(filters.into_iter().map(|(quick, label, glyph, tone)| {
                let on = self.tickets.quick == quick;
                let count = all.iter().filter(|c| quick_test(quick, c)).count();
                div()
                    .id(SharedString::from(format!("quick-{label}")))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(theme::sz(6.))
                    .h(theme::sz(26.))
                    .pl(theme::sz(8.))
                    .pr(theme::sz(10.))
                    .rounded_full()
                    .tooltip(ui::tip(label))
                    .border_1()
                    .border_color(if on { theme::border_strong() } else { gpui::transparent_black() })
                    .when(on, |d| d.bg(theme::selected()))
                    .when(!on, |d| d.hover(|s| s.bg(theme::tint())))
                    .text_size(theme::sz(12.))
                    .text_color(if on { theme::text() } else { theme::text_soft() })
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.tickets.quick = if this.tickets.quick == quick { Quick::All } else { quick };
                        cx.notify();
                    }))
                    .child(icons::icon(glyph, theme::sz(13.), theme::sz(13.), tone.unwrap_or(if on { theme::text() } else { theme::text_soft() })))
                    // A compact page has the icons and the counts: five chips fit its one row.
                    .when(!compact, |d| d.child(label))
                    .when(counted, |d| d.child(div().text_color(theme::card_soft_text()).child(count.to_string())))
            }));
        // Two calm rows. The first is the page: its name, where the tickets come from, and at the
        // right what changes the view (the filter, Board or List, Sync). The second is what to
        // show: whose tickets, then the quick filters. Only the fields have an edge; the rest
        // are words and icons on the glass. A compact page gives each group a row of its own.
        let title = div().flex_none().text_size(theme::sz(16.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Board");
        let rule = div().flex_none().w(px(1.)).h(theme::sz(18.)).bg(theme::border());
        let rows = if compact {
            div().flex().flex_col().gap(theme::sz(8.)).child(source).child(scope).child(view).child(quick)
        } else {
            div()
                .flex()
                .flex_col()
                .gap(theme::sz(12.))
                .child(div().flex().flex_wrap().items_center().gap_x(theme::sz(14.)).gap_y(theme::sz(8.)).child(title).child(source.flex_none()).child(view))
                .child(div().flex().flex_wrap().items_center().gap_x(theme::sz(12.)).gap_y(theme::sz(8.)).child(scope.flex_none()).child(rule).child(div().flex_1().min_w_0().child(quick)))
        };
        div()
            .flex()
            .flex_none()
            .flex_col()
            .gap(theme::sz(8.))
            .px(theme::sz(TOOLBAR_PAD + 2.))
            .py(theme::sz(if compact { 10. } else { 14. }))
            .child(rows)
            // On glass the error reads in a text colour; the red dot says what it is.
            .when_some(error, |d, e| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(theme::sz(6.))
                        .min_w_0()
                        .text_size(theme::sz(12.))
                        .text_color(theme::text_soft())
                        .child(ui::dot(theme::removed()))
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(e)),
                )
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
            // GitHub's columns are fixed, so all of them show: an empty one is where a card drops.
            if self.tickets.fetched_from == Provider::GitHub {
                add_empty_columns(&mut columns, &glasscode_core::github_tickets::columns());
            }
        }
        columns.sort_by(|a, b| (a.0.0, a.0.1).cmp(&(b.0.0, b.0.1)));
        columns
    }

    fn column_icon(&self, column: &(u8, i64, String), cards: &[Card], list: &[usize]) -> AnyElement {
        if column.1 == -1 {
            return icons::icon(icons::PULL_REQUEST, theme::sz(14.), theme::sz(14.), theme::text_soft()).into_any_element();
        }
        let status = list.first().and_then(|&i| cards[i].status.clone()).or_else(|| self.tickets.mine().iter().map(|t| &t.status).find(|s| s.name == column.2).cloned());
        match status {
            Some(s) => status_icon(&s, 13.),
            None => status_icon(&Status { name: column.2.clone(), kind: if column.0 == 4 { "completed".into() } else { String::new() }, position: 0. }, 13.),
        }
    }

    fn render_board(&self, cards: &[Card], cx: &mut Context<Self>) -> AnyElement {
        let columns = self.columns(cards);
        // One scroll handle for each column that shows; a column that is gone loses its own.
        let handles: Vec<gpui::ScrollHandle> = {
            let mut known = self.tickets.column_scrolls.borrow_mut();
            known.retain(|name, _| columns.iter().any(|(column, _)| &column.2 == name));
            columns.iter().map(|(column, _)| known.entry(column.2.clone()).or_default().clone()).collect()
        };
        // A column is glass inside a ring that shows the accent while a card is dragged over it:
        // the glass paints over its own element's ground and edge, so the ring is the part of the
        // ground it leaves.
        let ring = |id: String| div().id(SharedString::from(id)).flex().flex_col().p(theme::sz(2.)).rounded(theme::sz(13.));
        let glass = || backdrop::glass(theme::sz(11.)).flex().flex_col().flex_1().min_h_0().when(theme::is_light(), |d| d.shadow_md());
        // The columns fill the height and share the width; the board scrolls sideways only for a
        // sideways gesture, so a wheel over a column scrolls that column alone.
        // The row is as wide as the page, and never narrower than its columns at their first
        // width: with more columns than fit, the row itself is wider than the page. What scrolls
        // is measured from this row's own box (GPUI takes the bounds of the scrolling element's
        // children, not of what overflows them): a row as wide as the page with columns running
        // out of it had nothing to scroll, and the board did not move.
        let folded_done = !self.tickets.done_open;
        let least: f32 = columns.iter().map(|(column, _)| if column.0 == 4 && folded_done { 48. } else { COLUMN_WIDTH }).sum::<f32>() + 8. * columns.len().saturating_sub(1) as f32;
        div()
            .flex()
            .flex_none()
            .h_full()
            .w_full()
            .min_w(theme::sz(least))
            .gap(theme::sz(8.))
            .children(columns.into_iter().zip(handles).enumerate().map(|(place, ((column, list), handle))| {
                let done = column.0 == 4;
                let folded = done && !self.tickets.done_open;
                let icon = self.column_icon(&column, cards, &list);
                let count = list.len();
                if folded {
                    return ring(format!("col-{}", column.2))
                        .flex_none()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.tickets.done_open = true;
                            cx.notify();
                        }))
                        .drag_over::<DraggedCard>(|style, _, _, _| style.bg(theme::accent()))
                        .on_drop(cx.listener({
                            let to = column.2.clone();
                            move |this, drag: &DraggedCard, _, cx| this.ticket_move(&drag.identifier, &to, cx)
                        }))
                        .child(
                            glass()
                                .items_center()
                                .gap_2()
                                .w(theme::sz(44.))
                                .py_3()
                                .child(icon)
                                .children(column.2.chars().map(|c| div().text_size(theme::sz(12.)).line_height(theme::sz(13.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(c.to_string())))
                                .child(div().text_size(theme::sz(11.5)).text_color(theme::card_soft_text()).child(count.to_string())),
                        )
                        .into_any_element();
                }
                // A status column takes ticket cards dropped on it (the review column does not).
                let to = column.2.clone();
                let from = column.2.clone();
                let takes_cards = column.1 != -1;
                let header = div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(theme::sz(7.))
                    .h(theme::sz(34.))
                    .px(theme::sz(12.))
                    .child(icon)
                    .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(theme::sz(12.5)).font_weight(FontWeight::MEDIUM).text_color(theme::text_soft()).child(column.2.clone()))
                    .child(div().text_size(theme::sz(12.5)).text_color(theme::card_soft_text()).child(count.to_string()))
                    .when(done, |d| {
                        d.child(div().flex_1()).child(ui::button("done-fold", "Fold").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.tickets.done_open = false;
                            cx.notify();
                        })))
                    });
                ring(format!("col-{}", column.2))
                    .flex_grow(1.)
                    .flex_shrink_0()
                    .flex_basis(theme::sz(COLUMN_WIDTH))
                    .max_w(theme::sz(COLUMN_MAX))
                    .min_h_0()
                    .when(takes_cards, |d| {
                        d.drag_over::<DraggedCard>(move |style, drag, _, _| if drag.from == from { style } else { style.bg(theme::accent()) })
                            .on_drop(cx.listener(move |this, drag: &DraggedCard, _, cx| this.ticket_move(&drag.identifier, &to, cx)))
                    })
                    .child(
                        glass()
                            .child(header)
                            .when(column.1 == -1, |d| d.child(div().flex_none().px(theme::sz(12.)).pb(theme::sz(6.)).text_size(theme::sz(11.5)).text_color(theme::card_soft_text()).child("Pull requests on GitHub that wait on your review")))
                            .child(
                                div()
                                    .id(SharedString::from(format!("col-cards-{}", column.2)))
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_h_0()
                                    .gap(theme::sz(7.))
                                    .px(theme::sz(7.))
                                    .pb(theme::sz(7.))
                                    .overflow_y_scroll()
                                    .restrict_scroll_to_axis()
                                    .track_scroll(&handle)
                                    // Each card fades in once, when it first shows: one after the
                                    // other, from the top left. Its id is the card's, so a draw of
                                    // the board does not play it again.
                                    .children(list.iter().enumerate().map(|(row, &i)| motion::fade_in_after(SharedString::from(format!("card-in-{}", cards[i].label)), place + row, self.render_card(&cards[i], cx))))
                                    .when(list.is_empty(), |d| d.child(div().py_3().text_center().text_size(theme::sz(12.)).text_color(theme::card_soft_text()).child("Nothing here"))),
                            ),
                    )
                    .into_any_element()
            }))
            .into_any_element()
    }

    fn render_card(&self, card: &Card, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let selected = self.tickets.selected.as_ref() == Some(&card.key);
        let worst = card.worst();
        let key = card.key.clone();
        let key_act = card.key.clone();
        let first_work = card.work.first().cloned();
        let reviewing = card.review_tag || matches!(card.key, Key::Pr(..));
        let action = card_action(first_work.is_some(), card.done, reviewing, card.worktree.is_some());
        // What works on it: the first session's title (and how many more) with its state, or a
        // worktree of its branch with no session.
        let work_line = (!card.work.is_empty() || card.worktree.is_some()).then(|| {
            let (glyph, label) = match (card.work.first(), &card.worktree) {
                (Some(w), _) => (None, format!("{}{}", w.title, if card.work.len() > 1 { format!(" +{}", card.work.len() - 1) } else { String::new() })),
                (None, Some(b)) => (Some(icons::BRANCH), format!("{b} · no session")),
                _ => (None, String::new()),
            };
            div()
                .flex()
                .items_center()
                .gap(theme::sz(6.))
                .px(theme::sz(8.))
                .py(theme::sz(5.))
                .rounded(theme::sz(6.))
                .bg(match worst {
                    Some(RowState::NeedsYou) => Hsla { a: 0.14, ..sidebar::attention() },
                    Some(RowState::Working) => Hsla { a: 0.12, ..theme::added() },
                    _ => theme::hover(),
                })
                .when_some(glyph, |d, glyph| d.child(icons::icon(glyph, theme::sz(12.), theme::sz(12.), theme::card_soft_text())))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(theme::sz(12.)).text_color(theme::text_soft()).child(label))
                .children(worst.filter(|s| *s != RowState::Idle).map(|s| ui::status_dot(s, false)))
        });
        let has_meta = card.priority.is_some() || !card.chips.is_empty() || card.pr.is_some() || card.due.is_some();
        // The card being read, or one whose session needs the person: an edge drawn over the
        // glass (the glass paints over its own element's edge).
        let edge = if selected {
            Some(theme::accent())
        } else if worst == Some(RowState::NeedsYou) {
            Some(Hsla { a: 0.7, ..sidebar::attention() })
        } else {
            None
        };
        backdrop::glass_strong(theme::sz(8.))
            .id(SharedString::from(format!("card-{}", card.label)))
            .flex()
            .flex_none()
            .flex_col()
            .gap(theme::sz(7.))
            .px(theme::sz(10.))
            .py(theme::sz(9.))
            .when(theme::is_light(), |d| d.shadow_md())
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_ticket(key.clone(), cx)))
            .when_some(edge, |d, color| d.child(div().absolute().top_0().left_0().size_full().rounded(theme::sz(8.)).border_1().border_color(color)))
            // Ticket cards drag to another column to change their status in Linear.
            .when_some(match &card.key {
                Key::Ticket(id) => Some(DraggedCard { identifier: id.clone(), title: card.title.clone(), from: card.column.2.clone() }),
                Key::Pr(..) => None,
            }, |d, drag| d.on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone())))
            // Where it comes from and its ID; at the right the marks of the sessions on it and
            // who it is assigned to (or who wrote the pull request).
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(theme::sz(6.))
                    .child(icons::brand(card.source, 13.))
                    .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().font_family(theme::MONO_FONT).text_size(theme::sz(11.)).text_color(theme::card_soft_text()).child(card.label.clone()))
                    .when(card.review_tag, |d| d.child(review_tag()))
                    .child(div().flex_1())
                    .children(card_agents(&card.work).into_iter().map(|(agent, running)| sidebar::agent_icon(agent, 16., running)))
                    .when_some(card.avatar.clone(), |d, (i, c, _)| d.child(avatar(&i, c, 18.))),
            )
            .child(div().text_size(theme::sz(13.)).line_height(theme::sz(18.)).font_weight(FontWeight::MEDIUM).text_color(theme::text()).line_clamp(2).text_ellipsis().child(card.title.clone()))
            .when(has_meta, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(theme::sz(5.))
                        .when_some(card.priority, |d, p| d.child(div().flex().items_center().justify_center().w(theme::sz(22.)).h(theme::sz(20.)).rounded(theme::sz(5.)).bg(theme::hover()).child(priority_icon(p))))
                        .children(card.chips.iter().map(|(text, dot)| chip(text.clone(), *dot)))
                        .when_some(card.pr.clone(), |d, (text, color)| d.child(chip(text, Some(color))))
                        // Due soon: an orange dot; the words stay in a colour that reads on glass.
                        .when_some(card.due.clone(), |d, (text, soon)| d.child(chip(format!("Due {text}"), soon.then(sidebar::attention)))),
                )
            })
            .when_some(work_line, |d, line| d.child(line))
            .child(div().text_size(theme::sz(11.)).text_color(theme::card_soft_text()).child(card.foot.clone()))
            .when_some(action, |d, action| {
                d.child(
                    div().flex().child(
                        div()
                            .id(SharedString::from(format!("card-act-{}", card.label)))
                            .flex()
                            .items_center()
                            .gap(theme::sz(6.))
                            .px(theme::sz(8.))
                            .py(theme::sz(4.))
                            .rounded(theme::sz(6.))
                            .border_1()
                            .border_color(theme::border())
                            .text_size(theme::sz(11.5))
                            .text_color(theme::text_soft())
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::tint()).text_color(theme::text()))
                            .child(icons::icon(if first_work.is_some() { icons::FORWARD } else { icons::PLAY }, theme::sz(12.), theme::sz(12.), theme::text_soft()))
                            .child(action)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
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
                            })),
                    ),
                )
            })
    }

    fn render_list(&self, cards: &[Card], cx: &mut Context<Self>) -> AnyElement {
        let columns = self.columns(cards);
        div()
            .flex()
            .flex_col()
            .gap_4()

            .children(columns.into_iter().enumerate().map(|(place, (column, list))| {
                let done = column.0 == 4;
                let open = !done || self.tickets.done_open;
                let icon = self.column_icon(&column, cards, &list);
                let group = SharedString::from(format!("list-in-{}", column.2));
                // Each status as one glass card: its heading, then its rows.
                let card = backdrop::glass(theme::sz(12.))
                    .flex()
                    .flex_col()
                    .when(theme::is_light(), |d| d.shadow_md())
                    .child(
                        div()
                            .id(SharedString::from(format!("list-head-{}", column.2)))
                            .flex()
                            .items_center()
                            .gap_2()
                            .h(theme::sz(36.))
                            .px_3()
                            .when(done, |d| {
                                d.cursor_pointer().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.tickets.done_open = !this.tickets.done_open;
                                    cx.notify();
                                }))
                            })
                            .child(icon)
                            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(column.2.clone()))
                            .child(div().text_color(theme::card_soft_text()).child(list.len().to_string()))
                            .when(done, |d| d.child(div().text_color(theme::card_soft_text()).child(if open { "▾" } else { "▸" }))),
                    )
                    .when(open, |d| {
                        d.children(list.iter().map(|&i| self.render_row(&cards[i], cx)))
                            .when(list.is_empty(), |d| d.child(div().px_3().py_2().border_t_1().border_color(theme::glass_border()).text_size(theme::sz(12.)).text_color(theme::card_soft_text()).child("Nothing here")))
                    });
                motion::fade_in_after(group, place, card)
            }))
            .into_any_element()
    }

    fn render_row(&self, card: &Card, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.tickets.selected.as_ref() == Some(&card.key);
        let key = card.key.clone();
        let worst = card.worst();
        div()
            .id(SharedString::from(format!("row-{}", card.label)))
            .flex()
            .items_center()
            .gap_2p5()
            .h(theme::sz(40.))
            .px_3()
            .border_t_1()
            .border_color(theme::glass_border())
            .when(selected, |d| d.bg(theme::tint()))
            .cursor_pointer()
            .hover(|s| s.bg(theme::tint()))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_ticket(key.clone(), cx)))
            .child(icons::brand(card.source, 13.))
            .child(div().w(theme::sz(16.)).flex().justify_center().children(card.priority.map(priority_icon)))
            .child(div().w(theme::sz(if matches!(card.key, Key::Pr(..)) { 132. } else { 72. })).flex_none().text_size(theme::sz(12.)).text_color(theme::card_soft_text()).whitespace_nowrap().overflow_hidden().text_ellipsis().child(card.label.clone()))
            .child(match &card.status {
                Some(s) => status_icon(s, 13.),
                None => icons::icon(icons::PULL_REQUEST, theme::sz(13.), theme::sz(13.), theme::card_soft_text()).into_any_element(),
            })
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme::text()).child(card.title.clone()))
            .when(card.review_tag, |d| d.child(review_tag()))
            .children(card.chips.iter().take(3).map(|(text, dot)| chip(text.clone(), *dot)))
            .when_some(card.due.clone(), |d, (text, soon)| d.child(chip(format!("Due {text}"), soon.then(sidebar::attention))))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .w(theme::sz(190.))
                    .flex_none()
                    .children(card_agents(&card.work).into_iter().map(|(agent, running)| sidebar::agent_icon(agent, 17., running)))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(theme::sz(12.)).text_color(theme::text_soft()).child(card.work.first().map(|w| w.title.clone()).or_else(|| card.worktree.as_ref().map(|b| format!("⎇ {b}"))).unwrap_or_default()))
                    .children(worst.filter(|s| *s != RowState::Idle).map(|s| ui::status_dot(s, false))),
            )
            .child(div().w(theme::sz(20.)).flex_none().children(card.avatar.clone().map(|(i, c, _)| avatar(&i, c, 19.))))
            .into_any_element()
    }

    // ----- a ticket's page -----------------------------------------------------------------

    fn render_reader(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let key = self.tickets.selected.clone()?;
        let header = |this: &Self, source: Brand, id_text: String, branch: String, url: String, open_label: &'static str, cx: &mut Context<Self>| {
            let branch_copy = branch.clone();
            div()
                .flex()
                .flex_none()
                // In a narrow panel the buttons go to a second line, and are not cut.
                .flex_wrap()
                .items_center()
                .gap_2()
                .min_h(theme::sz(44.))
                .px_3()
                .py(theme::sz(6.))
                .border_b_1()
                .border_color(theme::border())
                .child(icons::brand(source, 14.))
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_soft()).whitespace_nowrap().child(id_text))
                .when(!branch.is_empty(), |d| {
                    d.child(
                        div()
                            .id("reader-branch")
                            .flex()
                            .items_center()
                            .gap_1()
                            .min_w_0()
                            .h(theme::sz(24.))
                            .px_2()
                            .rounded_md()
                            .border_1()
                            .border_color(theme::border_strong())
                            .font_family(theme::MONO_FONT)
                            .text_size(theme::sz(11.))
                            .text_color(theme::muted())
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme::text_strong()))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(branch_copy.clone()));
                                this.show_toast(format!("Copied {branch_copy}."), cx);
                            }))
                            .child(icons::icon(icons::BRANCH, theme::sz(12.), theme::sz(12.), theme::muted()))
                            .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(branch)),
                    )
                })
                .child(div().flex_1())
                .child(ui::button("reader-open", format!("{open_label} ↗")).on_click(move |_: &ClickEvent, _, cx| open_url(&url, cx)))
                .child(ui::icon_button("reader-expand", if this.tickets.expanded { "⇥" } else { "⤢" }).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.tickets.expanded = !this.tickets.expanded;
                    cx.notify();
                })))
                .child(ui::icon_button("reader-close", "×").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_reader(cx))))
        };
        let (head, content) = match &key {
            Key::Ticket(id) => {
                let ticket = self.tickets.ticket(id)?.clone();
                let open = match ticket.provider {
                    Provider::Linear => "Open in Linear",
                    Provider::GitHub => "Open on GitHub",
                };
                (header(self, source_brand(ticket.provider), ticket.identifier.clone(), ticket.branch.clone(), ticket.url.clone(), open, cx), self.render_ticket_page(&ticket, cx))
            }
            Key::Pr(repo, number) => {
                let r = self.tickets.review(repo, *number)?.clone();
                (header(self, Brand::Github, format!("{}#{}", r.repo.rsplit('/').next().unwrap_or(&r.repo), number), r.hit.head.clone(), r.hit.url.clone(), "Open on GitHub", cx), self.render_pr_page(&r, cx))
            }
        };
        Some(
            div()
                .flex()
                .flex_col()
                .size_full()
                .child(head)
                // The page fades in for each ticket: also for one opened from another one's page.
                .child(div().id("reader-scroll").flex_1().min_h_0().overflow_y_scroll().child(motion::fade_in(SharedString::from(format!("reader-in-{key:?}")), div().max_w(theme::sz(780.)).mx_auto().px_5().py_4().flex().flex_col().gap_4().child(content))))
                .into_any_element(),
        )
    }

    fn sessions_box(&self, work: &[Work], idle: Vec<(String, std::path::PathBuf, String)>, key: Key, reviewing: bool, start_note: String, cx: &mut Context<Self>) -> AnyElement {
        let needs = work.iter().any(|w| w.state == RowState::NeedsYou);
        let start_label = match (work.is_empty() && idle.is_empty(), reviewing) {
            (true, true) => "Start review".to_string(),
            // A workspace: a worktree from the latest main and a session in it, as on the card.
            (true, false) => "Start workspace".to_string(),
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
                    .when(!work.is_empty(), |d| d.child(div().text_size(theme::sz(11.5)).text_color(theme::faint()).child("Found by the ticket’s ID and pull requests"))),
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
                            .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(theme::sz(11.5)).text_color(theme::muted()).child(format!("{} · ⎇ {} · {}", w.agent.name(), w.branch, w.why))),
                    )
                    .child(div().flex().flex_none().items_center().gap_1().text_size(theme::sz(11.5)).text_color(color).child(ui::status_dot(w.state, false)).child(state_label))
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
                    .child(div().flex_none().size(theme::sz(22.)).rounded_md().border_1().border_dashed().border_color(theme::border_strong()).flex().items_center().justify_center().child(icons::icon(icons::BRANCH, theme::sz(12.), theme::sz(12.), theme::muted())))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_family(theme::MONO_FONT).text_size(theme::sz(12.)).text_color(theme::text_strong()).overflow_hidden().whitespace_nowrap().text_ellipsis().child(branch))
                            .child(div().text_size(theme::sz(11.5)).text_color(theme::muted()).child("A worktree with no session")),
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
                    .child(div().flex_1().min_w_0().text_size(theme::sz(11.5)).text_color(theme::muted()).child(start_note)),
            )
            .into_any_element()
    }

    fn render_ticket_page(&self, t: &Ticket, cx: &mut Context<Self>) -> AnyElement {
        let reviewing = self.tickets.reviewing().iter().any(|r| r.identifier == t.identifier);
        let work = self.work_on_ticket(t);
        let idle = self.idle_worktrees_for(t);
        let days = t.due.as_deref().and_then(|d| linear::days_until(d, now()));
        let prop = |content: AnyElement| div().flex().items_center().gap_1p5().h(theme::sz(26.)).px_2().rounded_md().border_1().border_color(theme::border_strong()).bg(theme::raised()).text_size(theme::sz(12.)).text_color(theme::text_soft()).child(content);
        let assignee = t.assignee.clone();
        let start_note = if reviewing {
            match t.pull_requests().first() {
                Some((repo, n)) => format!("Opens {}#{n} in a review worktree, with this ticket as context.", repo.rsplit('/').next().unwrap_or(repo)),
                None => "There is no pull request to review yet.".into(),
            }
        } else {
            // The branch the Start dialog fills in: the project's template, else the ticket's name.
            let project = self.ticket_project(t.pull_requests().first().map(|(repo, _)| repo.as_str()));
            format!("A new worktree on {} from the latest main. The ticket goes in as the first message.", self.ticket_branch(&project, t))
        };
        // Each link with its icon: a pull request, a task (a check box), or another page (no
        // icon yet: the arrow stays a character).
        let links: Vec<(Option<&'static [u8]>, String, String, Option<Hsla>, String)> = {
            let mut out: Vec<(Option<&'static [u8]>, String, String, Option<Hsla>, String)> = Vec::new();
            for a in &t.attachments {
                if let Some((repo, n)) = a.pull_request() {
                    let state = self.tickets.review(&repo, n).map(|_| "Asks for your review".to_string()).unwrap_or_default();
                    out.push((Some(icons::PULL_REQUEST), if a.title.is_empty() { format!("Pull request #{n}") } else { tidy_pr_title(&a.title) }, format!("{repo}#{n}"), Some(theme::added()).filter(|_| !state.is_empty()), a.url.clone()));
                } else if a.team_task().is_some() {
                    out.push((Some(icons::CHECKBOX_CHECKED), a.title.clone(), "Task".into(), None, a.url.clone()));
                } else {
                    out.push((None, a.title.clone(), a.subtitle.clone(), None, a.url.clone()));
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
                                .child(div().flex().items_baseline().gap_2().child(bold(c.author.clone())).child(div().text_size(theme::sz(11.5)).text_color(theme::muted()).child(linear::short_date(&c.created_at))))
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
            .child(div().text_size(theme::sz(19.)).line_height(theme::sz(26.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(t.title.clone()))
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
                    .children(t.labels.iter().map(|l| prop(div().flex().items_center().gap_1p5().child(div().size(theme::sz(7.)).rounded_full().bg(label_color(l))).child(l.clone()).into_any_element())))
                    .when_some(t.due.clone(), |d, due| d.child(prop(div().when(days.is_some_and(|n| n <= 3), |d| d.text_color(sidebar::attention())).child(format!("Due {}", linear::short_date(&due))).into_any_element()))),
            )
            .child(div().text_size(theme::sz(12.)).text_color(theme::muted()).child(format!(
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
                            .on_click(move |_: &ClickEvent, _, cx| open_url(&url, cx))
                            .child(div().flex().flex_none().justify_center().w(theme::sz(16.)).text_color(theme::text_soft()).child(match glyph {
                                Some(icon) => icons::icon(icon, theme::sz(14.), theme::sz(14.), theme::text_soft()).into_any_element(),
                                None => "↗".into_any_element(),
                            }))
                            .child(div().flex().flex_col().flex_1().min_w_0().child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme::text_strong()).child(title)).child(div().text_size(theme::sz(11.5)).text_color(theme::muted()).child(sub)))
                            .when_some(state_color, |d, c| d.child(div().text_size(theme::sz(11.5)).text_color(c).child("Asks for your review")))
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
                    .child(div().flex().items_baseline().gap_2().child(div().text_size(theme::sz(15.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child("Comments")).child(div().text_color(theme::muted()).child(comment_count)))
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
        let prop = |content: AnyElement| div().flex().items_center().gap_1p5().h(theme::sz(26.)).px_2().rounded_md().border_1().border_color(theme::border_strong()).bg(theme::raised()).text_size(theme::sz(12.)).text_color(theme::text_soft()).child(content);
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
            .child(div().text_size(theme::sz(19.)).line_height(theme::sz(26.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(tidy_pr_title(&r.hit.title)))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1p5()
                    .child(prop(div().flex().items_center().gap_1p5().child(div().size(theme::sz(7.)).rounded_full().bg(merge_color)).child(merge).into_any_element()))
                    .child(prop(div().child(format!("{} files", r.hit.changed_files)).into_any_element()))
                    .child(prop(div().flex().gap_1().child(div().text_color(theme::added()).child(format!("+{}", r.hit.additions))).child(div().text_color(theme::removed()).child(format!("−{}", r.hit.deletions))).into_any_element()))
                    .when(!r.hit.author.is_empty(), |d| d.child(prop(div().flex().items_center().gap_1p5().child(avatar(&initials(&r.hit.author), avatar_color(&r.hit.author), 16.)).child(r.hit.author.clone()).into_any_element()))),
            )
            .child(div().text_size(theme::sz(12.)).text_color(theme::muted()).child(format!("{} → {} · {} · opened {} · updated {}", r.hit.head, r.hit.base, r.repo, linear::short_date(&r.created_at), linear::short_date(&r.hit.updated_at))))
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
                        .child(div().text_size(theme::sz(12.)).font_weight(FontWeight::SEMIBOLD).text_color(color).child(label))
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
                            .child(div().text_size(theme::sz(12.)).text_color(theme::muted()).child(t.identifier.clone()))
                            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme::text_strong()).child(t.title.clone()))
                            .child(div().text_size(theme::sz(12.)).text_color(theme::muted()).child("Read →")),
                    )
            }))
            .child(div().text_size(theme::sz(12.)).text_color(theme::muted()).child("The diff, threads and comments stay on GitHub. Start review opens it in a worktree here, with Glasscode's review steps."))
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
                        .pl(theme::sz(depth as f32 * 16.))
                        .child(div().flex_none().w(theme::sz(16.)).text_size(theme::sz(size)).text_color(theme::muted()).child(number.map(|n| format!("{n}.")).unwrap_or_else(|| "•".into())))
                        .child(div().flex_1().min_w_0().child(line(&text, key, size, theme::text())))
                        .into_any_element(),
                    Block::Check { done, depth, text } => div()
                        .flex()
                        .gap_2()
                        .pl(theme::sz(depth as f32 * 16.))
                        // A mark to read, not a control: a click on it changes nothing in Linear.
                        .child(div().flex_none().mt(theme::sz(2.)).child(if done {
                            icons::icon(icons::CHECKBOX_CHECKED, theme::sz(16.), theme::sz(16.), theme::added())
                        } else {
                            icons::icon(icons::CHECKBOX, theme::sz(16.), theme::sz(16.), theme::muted())
                        }))
                        .child(div().flex_1().min_w_0().child(line(&text, key, size, if done { theme::text_soft() } else { theme::text() })))
                        .into_any_element(),
                    Block::Code(code) => div()
                        .p_2p5()
                        .rounded_md()
                        .bg(theme::bg())
                        .border_1()
                        .border_color(theme::border())
                        .font_family(theme::MONO_FONT)
                        .text_size(theme::sz(11.5))
                        .text_color(theme::text_soft())
                        .children(code.lines().map(|l| div().child(l.to_string())).collect::<Vec<_>>())
                        .into_any_element(),
                    Block::Quote(text) => div().flex().gap_2().py_1().px_2().rounded_md().bg(theme::hover()).child(div().w(theme::sz(3.)).flex_none().rounded_full().bg(theme::border_strong())).child(div().flex_1().min_w_0().child(line(&text, key, size, theme::text_soft()))).into_any_element(),
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
                                        div().flex_1().min_w(theme::sz(90.)).px_2p5().py_1p5().child(rich(&cell, format!("{key}-{c}"), size - 0.5, theme::text(), ws.clone()))
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

// ----- the toolbar's controls --------------------------------------------------------------------

/// The height of each control of the toolbar.
const CONTROL: f32 = 32.;
/// The Board's margin beside the toolbar, and the toolbar's own padding.
const PAGE_MARGIN: f32 = 14.;
const TOOLBAR_PAD: f32 = 14.;
/// The room (at 100 %) the filter, Board or List and Sync need to share a line with the source
/// and the scope: under it the group goes to the next line as a whole.
/// With this much room in the toolbar its three groups are on one line (so the filter keeps a
/// width, and does not take all that is left).
/// From this much room the controls have their words beside their icons; under it the icons
/// stand alone.
const WORDS_FROM: f32 = 960.;
/// What the view group needs when its controls are icons.
const VIEW_GROUP_ICONS: f32 = 280.;
/// Under this much room the toolbar is compact: a row for each group, shorter words.
const COMPACT_BELOW: f32 = 600.;

/// The frame every control of the toolbar has: one height, one radius, one edge.
fn control() -> gpui::Div {
    div()
        .flex()
        .items_center()
        .h(theme::sz(CONTROL))
        .rounded(theme::sz(8.))
        .border_1()
        .border_color(theme::border_strong())
        .bg(theme::raised())
        .text_size(theme::sz(13.))
        .text_color(theme::text())
}

/// The frame around [`segment`]s.
fn segmented() -> gpui::Div {
    control().flex_none().gap(theme::sz(2.)).p(theme::sz(2.))
}

/// One choice of a segmented control. The selected one differs by its ground and colour, not by
/// its weight, so the control is as wide whichever is selected. `grow`: the segments share the
/// width of the control.
fn segment(id: &'static str, selected: bool, grow: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .gap(theme::sz(6.))
        .h_full()
        .px(theme::sz(10.))
        .rounded(theme::sz(6.))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .cursor_pointer()
        .when(grow, |d| d.flex_1())
        .when(!grow, |d| d.flex_none())
        .when(selected, |d| d.bg(theme::selected()).text_color(theme::text_strong()))
        .when(!selected, |d| d.text_color(theme::text_soft()).hover(|s| s.bg(theme::hover()).text_color(theme::text_strong())))
}

/// A row of a [`select`]'s list.
struct Choice {
    label: String,
    detail: String,
    on: bool,
    /// A line over the row: what follows is not one of the choices above it.
    rule: bool,
    pick: Box<dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)>,
}

impl Choice {
    fn new(label: impl Into<String>, detail: impl Into<String>, on: bool, pick: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static) -> Self {
        Self { label: label.into(), detail: detail.into(), on, rule: false, pick: Box::new(pick) }
    }

    fn after_rule(mut self) -> Self {
        self.rule = true;
        self
    }
}

/// A control that shows a value and opens a list to choose another (`TicketsUi::picker` holds
/// the id of the open one). It is as wide as its parent lets it be, and cuts a long value short.
/// The list hangs from its left edge, or with `list_right` from its right one.
fn select(id: &'static str, lead: Option<AnyElement>, value: String, open: bool, list_right: bool, choices: Vec<Choice>, cx: &mut Context<Workspace>) -> AnyElement {
    let rows: Vec<AnyElement> = choices
        .into_iter()
        .enumerate()
        .map(|(i, choice)| {
            let Choice { label, detail, on, rule, pick } = choice;
            let row = div()
                .id(SharedString::from(format!("{id}-opt-{i}")))
                .flex()
                .items_center()
                .gap(theme::sz(10.))
                .min_h(theme::sz(34.))
                .px(theme::sz(10.))
                .py(theme::sz(5.))
                .rounded(theme::sz(6.))
                .cursor_pointer()
                .when(on, |d| d.bg(theme::selected()))
                .hover(|s| s.bg(theme::hover()))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.tickets.picker = None;
                    pick(this, window, cx);
                    cx.notify();
                }))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))
                        .when(!detail.is_empty(), |d| d.child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(theme::sz(11.5)).text_color(theme::muted()).child(detail))),
                )
                .when(on, |d| d.child(icons::icon(icons::CHECK, theme::sz(14.), theme::sz(14.), theme::accent())));
            if rule {
                div().flex().flex_col().gap(theme::sz(4.)).pt(theme::sz(4.)).child(div().h(px(1.)).bg(theme::border())).child(row).into_any_element()
            } else {
                row.into_any_element()
            }
        })
        .collect();
    div()
        .relative()
        .w_full()
        .child(
            control()
                .id(id)
                .w_full()
                .gap(theme::sz(7.))
                .px(theme::sz(10.))
                .cursor_pointer()
                .when(open, |d| d.border_color(theme::faint()))
                .hover(|s| s.border_color(theme::faint()))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.tickets.picker = if this.tickets.picker == Some(id) { None } else { Some(id) };
                    cx.notify();
                }))
                .children(lead)
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(value))
                .child(icons::icon(icons::CHEVRON_DOWN, theme::sz(12.), theme::sz(12.), theme::faint())),
        )
        .when(open, |d| {
            d.child(
                deferred(
                    div()
                        .id(SharedString::from(format!("{id}-list")))
                        .occlude()
                        .absolute()
                        .top(theme::sz(CONTROL + 4.))
                        .when(list_right, |d| d.right_0())
                        .when(!list_right, |d| d.left_0())
                        .w(theme::sz(300.))
                        .max_h(theme::sz(320.))
                        .overflow_y_scroll()
                        .p(theme::sz(4.))
                        .rounded(theme::sz(10.))
                        .border_1()
                        .border_color(theme::border_strong())
                        .bg(theme::dialog())
                        .shadow_lg()
                        .text_size(theme::sz(13.))
                        .children(rows),
                )
                .with_priority(2),
            )
        })
        .into_any_element()
}

/// The board while its first fetch is on its way: four columns of the shape the cards will have.
/// The blocks breathe (`motion::skeleton`) for as long as that fetch runs, and no longer.
fn board_skeleton() -> AnyElement {
    let card = |column: usize, row: usize| {
        let id = column * 40 + row * 4;
        backdrop::glass_strong(theme::sz(8.))
            .flex()
            .flex_none()
            .flex_col()
            .gap(theme::sz(9.))
            .px(theme::sz(10.))
            .py(theme::sz(10.))
            .child(motion::skeleton(("board-wait", id), 64., 9.))
            .child(motion::skeleton_block(("board-wait", id + 1), div().w_full().h(theme::sz(12.))))
            .child(motion::skeleton(("board-wait", id + 2), 120. + 24. * ((column + row) % 3) as f32, 12.))
            .child(motion::skeleton(("board-wait", id + 3), 86., 16.))
    };
    div()
        .flex()
        .flex_1()
        .min_w_0()
        .h_full()
        .overflow_hidden()
        .gap(theme::sz(8.))
        .px(theme::sz(12.))
        .pt(theme::sz(10.))
        .pb(theme::sz(12.))
        .children([3usize, 2, 3, 1].into_iter().enumerate().map(|(column, cards)| {
            div().flex().flex_col().flex_grow(1.).flex_shrink_0().flex_basis(theme::sz(COLUMN_WIDTH)).max_w(theme::sz(COLUMN_MAX)).min_h_0().p(theme::sz(2.)).child(
                backdrop::glass(theme::sz(11.))
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .when(theme::is_light(), |d| d.shadow_md())
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(theme::sz(7.))
                            .h(theme::sz(34.))
                            .px(theme::sz(12.))
                            .child(motion::skeleton_block(("board-wait-head", column * 2), div().size(theme::sz(13.)).rounded_full()))
                            .child(motion::skeleton(("board-wait-head", column * 2 + 1), 84., 11.)),
                    )
                    .child(div().flex().flex_col().gap(theme::sz(7.)).px(theme::sz(7.)).pb(theme::sz(7.)).children((0..cards).map(|row| card(column, row)))),
            )
        }))
        .into_any_element()
}

/// The words of the Sync button, short so that the button keeps one width: when the board was
/// last fetched (`secs` ago), or "Sync" when it never was.
fn synced_label(secs: Option<u64>) -> String {
    match secs {
        None => "Sync".into(),
        Some(secs) if secs < 60 => "Synced just now".into(),
        Some(secs) if secs < 3600 => format!("Synced {}m ago", secs / 60),
        Some(secs) if secs < 86_400 => format!("Synced {}h ago", secs / 3600),
        Some(secs) => format!("Synced {}d ago", secs / 86_400),
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
                    open_url(&url, cx);
                }
            })
            .into_any_element()
    };
    div().text_size(theme::sz(size)).line_height(theme::sz(size * 1.55)).text_color(color).child(body).into_any_element()
}

/// Add a column with no cards for each of `statuses` that `columns` does not have. The key of a
/// done column is the one a done card has, so the two are one column.
fn add_empty_columns(columns: &mut Vec<((u8, i64, String), Vec<usize>)>, statuses: &[Status]) {
    for status in statuses {
        if !columns.iter().any(|(c, _)| c.2 == status.name) {
            let key = if status.kind == "completed" { (4, 0, status.name.clone()) } else { (status.order(), (status.position * 100.) as i64, status.name.clone()) };
            columns.push((key, Vec::new()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A GitHub board with all its issues in Todo still has the other five columns, in order.
    #[test]
    fn a_github_board_has_every_column() {
        let mut columns = vec![((1, 0, "Todo".to_string()), vec![0, 1])];
        add_empty_columns(&mut columns, &glasscode_core::github_tickets::columns());
        columns.sort_by(|a, b| (a.0.0, a.0.1).cmp(&(b.0.0, b.0.1)));
        let names: Vec<&str> = columns.iter().map(|(c, _)| c.2.as_str()).collect();
        assert_eq!(names, ["In Progress", "In Review", "Todo", "Triage", "Backlog", "Done"]);
        assert_eq!(columns[2].1, vec![0, 1], "the cards stay in their column");
    }

    fn work(agent: AgentKind, running: bool) -> Work {
        Work { id: String::new(), on_server: false, agent, title: String::new(), branch: String::new(), state: RowState::Idle, running, why: String::new() }
    }

    /// A card shows each agent of its sessions once, lit when one of them runs, at most three.
    #[test]
    fn a_card_shows_each_agent_once() {
        let marks = card_agents(&[work(AgentKind::Claude, false), work(AgentKind::Codex, false), work(AgentKind::Claude, true), work(AgentKind::Shell, false)]);
        assert_eq!(marks, vec![(AgentKind::Claude, true), (AgentKind::Codex, false), (AgentKind::Shell, false)]);
        assert!(card_agents(&[]).is_empty());
    }

    /// "Start workspace" on a card with no session; a session opens instead; a done card has no
    /// button.
    #[test]
    fn the_card_button_follows_its_sessions() {
        assert_eq!(card_action(false, false, false, false), Some("Start workspace"));
        assert_eq!(card_action(false, false, false, true), Some("Start in worktree"));
        assert_eq!(card_action(false, false, true, false), Some("Start review"));
        assert_eq!(card_action(true, false, false, false), Some("Open session"));
        assert_eq!(card_action(true, true, false, false), Some("Open session"), "a done card still opens its session");
        assert_eq!(card_action(false, true, false, false), None);
    }

    #[test]
    fn the_sync_button_says_when_in_few_letters() {
        assert_eq!(synced_label(None), "Sync");
        assert_eq!(synced_label(Some(59)), "Synced just now");
        assert_eq!(synced_label(Some(60)), "Synced 1m ago");
        assert_eq!(synced_label(Some(3599)), "Synced 59m ago");
        assert_eq!(synced_label(Some(7200)), "Synced 2h ago");
        assert_eq!(synced_label(Some(3 * 86_400 + 5)), "Synced 3d ago");
    }

    #[test]
    fn each_source_has_its_mark() {
        assert_eq!(source_brand(Provider::Linear), Brand::Linear);
        assert_eq!(source_brand(Provider::GitHub), Brand::Github);
    }
}
