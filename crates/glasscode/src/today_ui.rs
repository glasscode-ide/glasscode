//! Today: the person's day on one screen (slice U11), and the one home of the app: the manager
//! has no page of its own, it is part of this one. On the moving backdrop: the start box (with
//! the manager on it is the manager's box: a question to it, or a new session from a few words),
//! the cards (`today_cards.rs`), and "Your list", the board of their own tasks and the ones
//! agents add through `glasscode board`, recurring ones, Linear tickets and review requests. The
//! right panel has the manager (its conversation, and what it holds: `manager_ui.rs`), Inbox
//! (what agents ask to do, and what needs the person now) and Activity (what agents did). The
//! board itself is `glasscode_core::board`; this file has the state, what the buttons do, and
//! the page.
//!
//! Today draws ten times a second while the backdrop moves, so it reads no file and runs no
//! program while it draws: the pull requests (`gh`) and the activity file are read in the
//! background, when Today opens and every few minutes while it shows, and kept here.
//!
//! Debug builds: `GLASSCODE_DEBUG_TODAY` takes words with commas between them. `still`: less
//! motion, as the system's setting. `manager`, `inbox`, `activity`: that tab of the right panel
//! (`followups`, `messages`, `memory`: that part of the manager's tab). `sample`: made-up rows in
//! each card (see `sample` at the end of this file). `chat`: a made-up conversation with the
//! manager, kept in the scratch home's own store, and made-up answers to what is sent
//! (`thinking`: an answer that is being written; `long`: a long conversation). `scene`: a
//! made-up picture the manager explains; `tour`: a made-up walk. `typed`: words in the start box, and the cursor
//! in it. `panel`: the manager's panel over the window, on another page. `scrolled`: each card's
//! list at its end. `end`: the page at its end. `loading`: the
//! cards that wait for something as they are while they wait.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{AnyElement, AppContext as _, ClickEvent, Context, Entity, FontWeight, Hsla, Pixels, Point, ScrollHandle, SharedString, Window, anchored, deferred, div, prelude::*, px};
use glasscode_core::activity::Activity;

/// The key of a Coming up line made from the work ledger: `work:<session>|<work id>`.
const WORK_KEY: &str = "work:";
/// The key of a Coming up line of what the manager does next: `manager:<n>`.
const MANAGER_KEY: &str = "manager:";
/// How many of the journal's lines Activity reads, and shows.
const JOURNAL_LINES: usize = 200;
const JOURNAL_SHOWN: usize = 20;
use glasscode_core::board::{self, Board, Item, Outside, Section, TodayView};
use glasscode_core::control::Reply;
use glasscode_core::day;
use glasscode_core::github::MyPr;
use glasscode_core::model::AgentKind;

use crate::backdrop;
use crate::composer::{Composer, ComposerEvent};
use crate::control_ui::{Approval, Responder};
use crate::icons;
use crate::motion;
use crate::shell::View;
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::today_cards::{self as cards, Card, Open, Tone};
use crate::ui;
use crate::workspace::{Side, Workspace};

/// The person's pull requests are asked for again after this while Today shows, and when Today
/// opens after this much.
const PRS_EVERY: Duration = Duration::from_secs(5 * 60);
const PRS_ON_OPEN: Duration = Duration::from_secs(60);
/// The activity file is read again after this while Today shows.
const ACTIVITY_EVERY: Duration = Duration::from_secs(60);

/// The heat map shows this many days of the agents' own history at most (its 26 weeks, and the
/// month before for "This month").
const HISTORY_DAYS: i64 = 7 * cards::WEEKS as i64 + 62;

/// The right panel's tabs on Today.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TodayTab {
    /// The manager: its conversation, and what it holds (`Workspace::render_assistant`). With
    /// the manager off the tab is Memory alone, which is not the manager's.
    Manager,
    /// What agents ask to do, and what needs the person now.
    #[default]
    Inbox,
    /// What agents did today.
    Activity,
}

/// A pill of the start box whose list is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartMenu {
    Agent,
    Project,
    From,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Due,
    Asked,
    Reviews,
    Waiting,
}

impl Filter {
    const ALL: [(Filter, &'static str); 5] = [(Filter::All, "Everything"), (Filter::Due, "Due"), (Filter::Asked, "Asked of me"), (Filter::Reviews, "Reviews"), (Filter::Waiting, "Waiting")];

    fn shows(self, section: Section) -> bool {
        match self {
            Filter::All => true,
            Filter::Due => matches!(section, Section::Focus | Section::Due),
            Filter::Asked => section == Section::Asked,
            Filter::Reviews => section == Section::Reviews,
            Filter::Waiting => section == Section::Waiting,
        }
    }
}

pub struct TodayUi {
    pub board: Board,
    pub add: Entity<TextInput>,
    pub filter: Filter,
    /// Sections folded to their title.
    pub closed: HashSet<Section>,
    /// What the add box will do with what is typed ("→ Asked of me · Sam · Fri").
    pub hint: Option<String>,
    /// When the window last came to the front (the morning open happens only then, never
    /// while the person is already working).
    pub activated_at: Option<std::time::Instant>,
    /// When the person last opened a session or need (so the morning open does not take over
    /// right after they clicked Open on a notification).
    pub navigated_at: Option<std::time::Instant>,
    /// Tasks whose Start is under way, so a second click does not make a second session.
    pub starting: HashSet<String>,
    /// The start box: what to ask, and what its pills are set to (none: the default).
    pub start: Entity<Composer>,
    pub start_agent: Option<(AgentKind, Option<String>)>,
    pub start_project: Option<String>,
    /// The worktree to start in; none: a new worktree.
    pub start_from: Option<PathBuf>,
    /// The pill whose list is open, and where it was clicked.
    pub menu: Option<(StartMenu, Point<Pixels>)>,
    /// The press outside the list that closed it, and the pill of that list (`pill_click`).
    menu_closed_by: Option<(StartMenu, Point<Pixels>)>,
    /// A Send is under way (a second one does nothing); `sent`: it worked, and the box empties.
    pub sending: bool,
    pub sent: bool,
    pub tab: TodayTab,
    /// The person's open pull requests: none until the first search comes back; an error when
    /// `gh` is missing, not signed in or did not answer (a later error keeps the last list).
    pub prs: Option<Result<Vec<MyPr>, String>>,
    pub prs_at: Option<Instant>,
    pub prs_loading: bool,
    /// What the heat map and "This month" show: `~/.glasscode/activity.json` as last read
    /// (`activity_own`), with the person's own agent history for the days before it starts
    /// (`history`, read once in the background: `glasscode_core::agent_history`).
    pub activity: Option<Activity>,
    pub activity_at: Option<Instant>,
    activity_own: Option<Activity>,
    history: Option<BTreeMap<String, u32>>,
    history_asked: bool,
    /// v2 (M31): the autonomous actions of the last 30 days, from the effect ledger, newest first.
    pub effects: Vec<glasscode_core::effects::EffectRecord>,
    /// v2 (M30): the journal's lines of the last day for Activity: a line per turn, and the work's events.
    pub journal_lines: Vec<glasscode_core::journal::ActivityLine>,
    /// Every event of the day, as `glasscode events` lists them (M14), and whether Activity shows
    /// those instead of the turns and the work's events.
    pub journal_every: Vec<glasscode_core::journal::ActivityLine>,
    pub journal_all: bool,
    pub scroll: ScrollHandle,
    pub side_scroll: ScrollHandle,
    /// Where each card that scrolls is: kept here, so that a card stays where it was scrolled
    /// to when Today draws again.
    pub card_scroll: CardScroll,
    /// Debug builds: `GLASSCODE_DEBUG_TODAY` was read, and its words `sample`, `scrolled`, `end`
    /// and `loading` (the top of this file).
    debug_done: bool,
    debug_sample: bool,
    debug_scrolled: bool,
    debug_end: bool,
    debug_loading: bool,
}

/// A scroll handle for each card of Today whose body scrolls.
#[derive(Default)]
pub struct CardScroll {
    pub needs: ScrollHandle,
    pub coming: ScrollHandle,
    pub usage: ScrollHandle,
    pub recent: ScrollHandle,
    pub prs: ScrollHandle,
}

impl TodayUi {
    pub fn new(cx: &mut Context<Workspace>) -> Self {
        let start = cx.new(|cx| Composer::new("Ask an agent to build, fix or review something", cx));
        cx.subscribe(&start, |this: &mut Workspace, _, event: &ComposerEvent, cx| {
            let now = matches!(event, ComposerEvent::SubmitNow);
            // Esc stops an answer that is being written (ux-9).
            if matches!(event, ComposerEvent::Cancel) && this.mchat.turn.is_some() {
                this.chat_stop(cx);
            }
            if matches!(event, ComposerEvent::Submit | ComposerEvent::SubmitNow) {
                // Starting a session needs the window. With the manager on and Ask chosen, Enter
                // asks it (M52.1, ux-4), and ⌘Enter keeps its meaning: send now, stopping an
                // answer that is being written.
                this.with_main_window(cx, move |ws, window, cx| ws.today_enter(now, window, cx));
            }
        })
        .detach();
        let add = cx.new(|cx| TextInput::new("Add a task — “Ask Sam about the API token by Friday”", cx).bare());
        cx.subscribe(&add, |this: &mut Workspace, _, event: &InputEvent, cx| match event {
            InputEvent::Submit => this.today_add(cx),
            InputEvent::Changed => this.today_hint(cx),
            InputEvent::Cancel => {
                this.today.add.update(cx, |i, cx| i.set_text("", cx));
                this.today.hint = None;
                cx.notify();
            }
        })
        .detach();
        let mut board = board::load();
        if board.prepare(day::today()) {
            let _ = board.save();
        }
        TodayUi {
            board,
            add,
            filter: Filter::All,
            closed: [Section::Waiting, Section::Later, Section::Recurring, Section::Done].into_iter().collect(),
            hint: None,
            activated_at: Some(std::time::Instant::now()),
            navigated_at: None,
            starting: HashSet::new(),
            start,
            start_agent: None,
            start_project: None,
            start_from: None,
            menu: None,
            menu_closed_by: None,
            sending: false,
            sent: false,
            // The conversation with the manager is the first thing of the right panel (with the
            // manager off the panel shows Inbox: `Workspace::today_tab`).
            tab: TodayTab::Manager,
            prs: None,
            prs_at: None,
            prs_loading: false,
            activity: None,
            activity_at: None,
            activity_own: None,
            history: None,
            history_asked: false,
            effects: Vec::new(),
            journal_lines: Vec::new(),
            journal_every: Vec::new(),
            journal_all: false,
            scroll: ScrollHandle::new(),
            side_scroll: ScrollHandle::new(),
            card_scroll: CardScroll::default(),
            debug_done: false,
            debug_sample: false,
            debug_scrolled: false,
            debug_end: false,
            debug_loading: false,
        }
    }
}

/// Something that needs the person now: a session that waits for an answer, or a pull request
/// whose checks fail.
pub(crate) struct NeedRow {
    title: String,
    what: String,
    color: Hsla,
    agent: Option<AgentKind>,
    failing: bool,
    open: Open,
}

/// The time since `at` is past `every` (or there was no `at`).
fn due(at: Option<Instant>, every: Duration) -> bool {
    at.is_none_or(|at| at.elapsed() >= every)
}

/// Whether the list of the pill `which` is open after a click on that pill that began with a
/// press at `press`: it opens, or closes when it is open. An open list closes itself at a press
/// outside it (`closed_by`), which comes before the click on the pill: a click on its own pill
/// that began with that same press leaves it closed, instead of opening it again.
fn pill_click(open: Option<StartMenu>, closed_by: Option<(StartMenu, Point<Pixels>)>, which: StartMenu, press: Option<Point<Pixels>>) -> bool {
    if closed_by.is_some_and(|(closed, at)| closed == which && Some(at) == press) {
        return false;
    }
    open != Some(which)
}

/// Which of the start box's `choices` (agent, account, label) the agent pill shows: the one the
/// person `chosen`, else `wanted` (the project's default agent) on its default account, else on
/// its first account, else Claude's default account, else the first choice. The box starts no
/// shell (it takes no first message), so a project whose default is the shell gets Claude.
fn pick_start_agent(choices: &[(AgentKind, Option<String>, String)], chosen: Option<&(AgentKind, Option<String>)>, wanted: AgentKind, default_of: impl Fn(AgentKind) -> Option<String>) -> usize {
    let find = |agent: AgentKind, account: Option<String>| choices.iter().position(|c| c.0 == agent && c.1 == account);
    let wanted = if wanted == AgentKind::Shell { AgentKind::Claude } else { wanted };
    chosen
        .and_then(|(agent, account)| find(*agent, account.clone()))
        .or_else(|| find(wanted, default_of(wanted)))
        .or_else(|| choices.iter().position(|c| c.0 == wanted))
        .or_else(|| find(AgentKind::Claude, default_of(AgentKind::Claude)))
        .unwrap_or(0)
}

/// A row's link to a session: (title, needs you, working, its id; on the server).
struct Chip {
    title: String,
    needs: bool,
    working: bool,
    work: Option<crate::tickets_ui::Work>,
}

impl Workspace {
    // ----- data --------------------------------------------------------------------------------

    /// Linear tickets and review requests, as board items.
    pub fn today_outside(&self) -> Vec<Outside> {
        board::outside_items(self.tickets.snapshot.as_ref(), &self.tickets.reviews, day::today())
    }

    pub fn today_view(&self) -> TodayView {
        self.today.board.view(&self.today_outside(), day::today())
    }

    /// Names Glasscode knows, for reading who is in a new task.
    pub fn today_people(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let mut push = |n: &str| {
            let n = n.trim();
            if !n.is_empty() && !names.iter().any(|x| x.eq_ignore_ascii_case(n)) {
                names.push(n.to_string());
            }
        };
        if let Some(people) = &self.tickets.people {
            for p in people {
                push(&p.name);
                if let Some(first) = p.name.split_whitespace().next() {
                    push(first);
                }
            }
        }
        if let Some(s) = &self.tickets.snapshot {
            for t in s.mine.iter().chain(&s.reviewing) {
                if let Some(a) = &t.assignee {
                    push(&a.name);
                }
            }
        }
        for t in &self.today.board.tasks {
            if let Some(p) = &t.person {
                push(p);
            }
        }
        names
    }

    pub fn today_save(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = self.today.board.save() {
            self.show_toast(format!("Could not save Today: {e:#}"), cx);
        }
        cx.notify();
    }

    pub fn show_today(&mut self, cx: &mut Context<Self>) {
        self.enter(crate::shell::View::Today);
        self.menu_open = false;
        let today = day::iso(day::today());
        if self.today.board.seen.as_deref() != Some(today.as_str()) {
            self.today.board.seen = Some(today);
            let _ = self.today.board.save();
        }
        if self.tickets.last_fetch.is_none() {
            self.tickets_refresh(cx);
        }
        if due(self.today.prs_at, PRS_ON_OPEN) {
            self.today_prs_refresh(cx);
        }
        self.today_activity_reload(cx);
        cx.notify();
    }

    /// Ask `gh` for the person's open pull requests, in the background (it takes seconds).
    pub(crate) fn today_prs_refresh(&mut self, cx: &mut Context<Self>) {
        if self.today.prs_loading {
            return;
        }
        self.today.prs_loading = true;
        self.today.prs_at = Some(Instant::now());
        let env = self.env.clone();
        let task = cx.background_spawn(async move { glasscode_core::github::my_open_prs(&env).map_err(|e| format!("{e:#}")) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.today.prs_loading = false;
                // Debug builds: the made-up list stays.
                if this.today.debug_sample {
                    return;
                }
                match result {
                    Ok(list) => this.today.prs = Some(Ok(list)),
                    Err(error) => {
                        log::warn!("Today: could not list your open pull requests: {error}");
                        // A list from before stays: one failed search does not empty the card.
                        if !matches!(this.today.prs, Some(Ok(_))) {
                            this.today.prs = Some(Err(error));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Read the activity file again, in the background.
    fn today_activity_reload(&mut self, cx: &mut Context<Self>) {
        self.today.activity_at = Some(Instant::now());
        let task = cx.background_spawn(async move {
            let effects = {
                glasscode_core::effects::recent(glasscode_core::effects::ACTIVITY_DAYS).unwrap_or_else(|err| {
                    log::warn!("effects: the ledger could not be read ({err})");
                    Vec::new()
                })
            };
            // The journal's part of the day (M30).
            let since = glasscode_core::clock::now_ms().saturating_sub(24 * 3_600_000);
            let (lines, every) = (glasscode_core::journal::activity_lines(since, JOURNAL_LINES), glasscode_core::journal::event_lines(since, JOURNAL_LINES));
            // The last scan of the agents' own history, so the map is whole at once.
            (Activity::load(), glasscode_core::agent_history::History::load(), effects, lines, every)
        });
        cx.spawn(async move |this, cx| {
            let (activity, history, effects, lines, every) = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.today.history.is_none() {
                    this.today.history = history.map(|history| history.days);
                }
                this.today.activity_own = Some(activity);
                this.today_activity_show();
                this.today.effects = effects;
                this.today.journal_lines = lines;
                this.today.journal_every = every;
                this.today_history_scan(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// What the heat map shows: Glasscode's own count, and before its first day the
    /// conversations the person had in their agents.
    fn today_activity_show(&mut self) {
        // Debug builds: the made-up history stays.
        if self.today.debug_sample {
            return;
        }
        let Some(own) = &self.today.activity_own else { return };
        self.today.activity = Some(match &self.today.history {
            Some(days) => own.with_history(days),
            None => own.clone(),
        });
    }

    /// Count the person's own Claude and Codex conversations of the days the map shows, once
    /// for each run of the app, in the background: only names, times and the head of each file
    /// are read, and only the count of each day is kept (`agent-history.json`).
    fn today_history_scan(&mut self, cx: &mut Context<Self>) {
        if self.today.history_asked {
            return;
        }
        self.today.history_asked = true;
        let first = day::today() - HISTORY_DAYS;
        let task = cx.background_spawn(async move {
            let roots = glasscode_core::agent_history::Roots::of(history_home().as_deref(), &glasscode_core::paths::accounts_dir());
            let history = glasscode_core::agent_history::scan(&roots, first);
            if let Err(err) = history.save() {
                log::warn!("Today: the agents' history could not be kept ({err})");
            }
            log::info!("Today: {} earlier conversations on {} days in the agents' own history", history.sessions(), history.days.len());
            history
        });
        cx.spawn(async move |this, cx| {
            let history = task.await;
            let _ = this.update(cx, |this, cx| {
                this.today.history = Some(history.days);
                this.today_activity_show();
                cx.notify();
            });
        })
        .detach();
    }

    /// Switch to Today and put the cursor in the add box.
    pub fn open_today(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_today(cx);
        let handle = self.today.add.read(cx).focus_handle_clone();
        window.focus(&handle, cx);
    }

    /// A new day: recurring tasks come back; the first time the window is in front in the
    /// morning, Today opens by itself (unless switched off).
    pub fn today_tick(&mut self, cx: &mut Context<Self>) {
        // Debug builds: GLASSCODE_DEBUG_TODAY (the top of this file). `still` asks for less
        // motion, as the system's setting does: the backdrop stands still on its frame, the
        // baseline of a measurement of its cost.
        if cfg!(debug_assertions) && !self.today.debug_done {
            self.today.debug_done = true;
            if let Ok(how) = std::env::var("GLASSCODE_DEBUG_TODAY") {
                for part in how.split(',') {
                    match part {
                        "still" => cx.set_reduce_motion(true),
                        "manager" => self.today.tab = TodayTab::Manager,
                        "inbox" => self.today.tab = TodayTab::Inbox,
                        "activity" => self.today.tab = TodayTab::Activity,
                        "followups" => self.manager.stage = crate::manager_view::Stage::FollowUps,
                        "messages" => self.manager.stage = crate::manager_view::Stage::Messages,
                        "memory" => self.manager_set_stage(crate::manager_view::Stage::Memory, cx),
                        // The assistant's panel over the window (on a page that is not Today).
                        "panel" if self.view != View::Today && self.state.manager.enabled => {
                            self.chat_overlay_input(cx);
                            self.mchat.overlay = true;
                        }
                        "chat" => self.chat_debug_sample(false, false, cx),
                        "thinking" => self.chat_debug_sample(true, false, cx),
                        "long" => self.chat_debug_sample(false, true, cx),
                        "scene" => self.manager_debug_scene(cx),
                        "tour" => self.manager_debug_tour(cx),
                        "sample" => {
                            self.today.debug_sample = true;
                            self.today.prs = Some(Ok(sample::pull_requests()));
                            self.today.activity = Some(sample::activity(day::today()));
                        }
                        "scrolled" => self.today.debug_scrolled = true,
                        "end" => self.today.debug_end = true,
                        "loading" => self.today.debug_loading = true,
                        "typed" => self.with_main_window(cx, |ws, window, cx| {
                            ws.today.start.update(cx, |c, cx| c.set_text("Find why the import test fails on Windows, and fix it", cx));
                            let handle = ws.today.start.read(cx).focus_handle_clone();
                            window.focus(&handle, cx);
                        }),
                        _ => {}
                    }
                }
            }
        }
        // A session started from the box: the box empties (the reply had no `cx`).
        if self.today.sent {
            self.today.sent = false;
            self.today.start.update(cx, |c, cx| c.set_text("", cx));
        }
        if self.view == View::Today {
            if due(self.today.prs_at, PRS_EVERY) {
                self.today_prs_refresh(cx);
            }
            if due(self.today.activity_at, ACTIVITY_EVERY) {
                self.today_activity_reload(cx);
            }
        }
        let today = day::today();
        let mut changed = self.today.board.prepare(today);
        // Only once a fetch has actually come back (never at startup, when the lists are still the
        // disk cache or empty) do we drop Focus keys whose item is gone: otherwise a pinned
        // pull request disappears from Focus on every launch.
        if self.tickets.fetched_once && self.tickets.reviews_error.is_none() {
            changed |= self.today.board.prune_focus(&self.today_outside());
        }
        if changed {
            let _ = self.today.board.save();
            cx.notify();
        }
        let seen = self.today.board.seen.as_deref().and_then(day::parse_iso);
        let morning = (4..12).contains(&day::hour_now());
        let free = self.modal.is_none() && self.palette.is_none();
        let just_in_front = self.today.activated_at.is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(3));
        let navigating = self.today.navigated_at.is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(3));
        if seen != Some(today) && morning && self.window_active && just_in_front && !navigating && free && !self.state.agents.morning_off && self.view == crate::shell::View::Sessions {
            self.show_today(cx);
        }
    }

    fn today_hint(&mut self, cx: &mut Context<Self>) {
        let text = self.today.add.read(cx).text();
        self.today.hint = (!text.trim().is_empty()).then(|| {
            let today = day::today();
            let d = board::parse_quick(&text, today, &self.today_people());
            let mut parts: Vec<String> = Vec::new();
            match d.every {
                Some(every) => parts.push(every.label()),
                None => parts.push(
                    match (d.due.is_some_and(|due| due <= today), d.list) {
                        (true, _) => "Due today",
                        (_, board::List::Asked) => "Asked of me",
                        (_, board::List::Waiting) => "Waiting on others",
                        (_, board::List::Later) => "Later",
                        _ => "To do",
                    }
                    .to_string(),
                ),
            }
            if let Some(p) = &d.person {
                parts.push(p.clone());
            }
            if let Some(due) = d.due.filter(|due| *due > today) {
                parts.push(day::relative(due, today));
            }
            if d.urgent {
                parts.push("urgent".into());
            }
            format!("→ {}", parts.join(" · "))
        });
        cx.notify();
    }

    fn today_add(&mut self, cx: &mut Context<Self>) {
        let text = self.today.add.read(cx).text();
        if text.trim().is_empty() {
            return;
        }
        let today = day::today();
        let draft = board::parse_quick(&text, today, &self.today_people());
        let section = draft.list.section();
        let id = self.today.board.add(draft, board::Origin::you(), today);
        self.today.add.update(cx, |i, cx| i.set_text("", cx));
        self.today.hint = None;
        self.today.filter = Filter::All;
        // Show where it went.
        let placed = self.today_view().items().find(|i| i.id == id).and_then(|i| i.section).unwrap_or(section);
        self.today.closed.remove(&placed);
        self.today_save(cx);
    }

    fn today_toggle_done(&mut self, item: &Item, cx: &mut Context<Self>) {
        if let Err(e) = self.today.board.set_done(&item.key, !item.done, day::today()) {
            self.show_toast(e, cx);
        }
        self.today_save(cx);
    }

    fn today_toggle_focus(&mut self, item: &Item, cx: &mut Context<Self>) {
        if let Err(e) = self.today.board.set_focus(&item.key, !item.focus) {
            self.show_toast(e, cx);
        }
        self.today_save(cx);
    }

    fn today_toggle_snooze(&mut self, item: &Item, cx: &mut Context<Self>) {
        let until = if item.snoozed.is_some() { None } else { Some(day::today() + 1) };
        if let Err(e) = self.today.board.snooze(&item.key, until) {
            self.show_toast(e, cx);
        }
        self.today_save(cx);
    }

    /// The session working on an item (a ticket's, a review's, the one started from a task).
    fn today_chip(&self, item: &Item) -> Option<Chip> {
        let from_work = |work: Vec<crate::tickets_ui::Work>| {
            let first = work.into_iter().next()?;
            Some(Chip { title: first.title.clone(), needs: first.state == crate::sidebar::RowState::NeedsYou, working: first.state == crate::sidebar::RowState::Working, work: Some(first) })
        };
        if let Some(id) = item.key.strip_prefix("linear:").or_else(|| item.key.strip_prefix("linear-review:")) {
            let ticket = self.tickets.ticket(id)?.clone();
            return from_work(self.work_on_ticket(&ticket));
        }
        if let Some(rest) = item.key.strip_prefix("review:") {
            let (repo, n) = rest.rsplit_once('#')?;
            return from_work(self.work_on_pr(repo, n.parse().ok()?));
        }
        let task = self.today.board.task(item.key.strip_prefix("task:")?)?;
        if let Some(s) = task.session.as_deref().and_then(|id| self.state.session(id)) {
            let state = self.row_state(s);
            return Some(Chip {
                title: self.session_title(s),
                needs: state == crate::sidebar::RowState::NeedsYou,
                working: state == crate::sidebar::RowState::Working,
                work: Some(crate::tickets_ui::Work { id: s.id.clone(), on_server: false, agent: s.agent, title: self.session_title(s), branch: s.branch.clone(), state, running: self.terminals.contains_key(&s.id), why: String::new() }),
            });
        }
        // A task about a ticket: that ticket's session.
        let ticket = task.links.iter().find_map(|l| self.tickets.ticket(l).cloned())?;
        from_work(self.work_on_ticket(&ticket))
    }

    /// Start (or open) the work for an item: a ticket's Start session, a review, or a new
    /// session with a task as its first message.
    fn today_start(&mut self, item: &Item, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(chip) = self.today_chip(item) {
            if let Some(work) = chip.work {
                self.open_work(&work, window, cx);
                return;
            }
        }
        if let Some(id) = item.key.strip_prefix("linear:").or_else(|| item.key.strip_prefix("linear-review:")) {
            self.open_start_ticket(crate::tickets_ui::Key::Ticket(id.to_string()), None, window, cx);
            return;
        }
        if let Some(rest) = item.key.strip_prefix("review:") {
            if let Some((repo, n)) = rest.rsplit_once('#').and_then(|(r, n)| Some((r.to_string(), n.parse::<u64>().ok()?))) {
                self.open_start_ticket(crate::tickets_ui::Key::Pr(repo, n), None, window, cx);
            }
            return;
        }
        let Some(task) = item.key.strip_prefix("task:").and_then(|id| self.today.board.task(id)).cloned() else { return };
        if !self.today.starting.insert(task.id.clone()) {
            return;
        }
        let Some(project_id) = self.active_session().map(|s| s.project_id.clone()).or_else(|| self.state.projects.first().map(|p| p.id.clone())) else {
            self.today.starting.remove(&task.id);
            self.show_toast("Add a project first (Sessions → Add project).", cx);
            return;
        };
        let ticket = task.links.iter().find_map(|l| self.tickets.ticket(l).cloned());
        let mut message = task.title.clone();
        if let Some(person) = &task.person {
            message.push_str(&format!("\n\n({} is involved{})", person, task.via.as_ref().map(|v| format!("; they asked in {v}")).unwrap_or_default()));
        }
        for note in &task.notes {
            message.push_str(&format!("\n\nNote: {}", note.text));
        }
        let spec = crate::commands::NewSpec {
            project_id,
            agent: AgentKind::Claude,
            account_id: self.state.default_account(AgentKind::Claude).map(|a| a.id.clone()),
            title: Some(task.title.clone()),
            branch: None,
            worktree: None,
            from: None,
            ticket,
            pr: None,
            review: None,
            model: None,
            mode: None,
            message: Some(message),
            // A task's words (anyone may have added it), with Glasscode's around them.
            message_origin: None,
            open: true,
            on_server: false,
            started_by: "You".into(),
        };
        let task_id = task.id.clone();
        let responder = Responder::local(move |ws: &mut Workspace, reply| {
            ws.today.starting.remove(&task_id);
            if let glasscode_core::control::Reply::Done { code: 0, data, .. } = reply {
                if let Some(session) = data["id"].as_str() {
                    if let Some(t) = ws.today.board.task_mut(&task_id) {
                        t.session = Some(session.to_string());
                    }
                    let _ = ws.today.board.save();
                }
            } else if let glasscode_core::control::Reply::Done { text, .. } = reply {
                let text = text.clone();
                ws.toast = Some((text, std::time::Instant::now()));
            }
        });
        self.start_session(spec, responder, window, cx);
    }

    /// What needs the person now (besides approvals): sessions waiting for an answer, remote
    /// tasks that need them, their pull requests with failing CI.
    /// Sessions (and server sessions) that are waiting for the person's answer — for the count
    /// in the summary, without double-counting a session that also has failing CI.
    fn today_waiting_sessions(&self) -> usize {
        let mut ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for s in &self.state.sessions {
            // Hidden projects' sessions never raise their hand on the Today board.
            if self.state.session_hidden(s) {
                continue;
            }
            if self.row_state(s) == crate::sidebar::RowState::NeedsYou && self.control.pending_for(&s.id).is_empty() {
                ids.insert(&s.id);
            }
        }
        let server = self.server.state.as_ref().map(|s| s.sessions.iter().filter(|r| r.activity == Some(glasscode_core::agents::AgentActivity::NeedsInput)).count()).unwrap_or(0);
        ids.len() + server
    }

    pub(crate) fn today_needs(&self) -> Vec<NeedRow> {
        let mut out = Vec::new();
        for s in &self.state.sessions {
            if self.state.session_hidden(s) {
                continue;
            }
            if self.row_state(s) == crate::sidebar::RowState::NeedsYou && self.control.pending_for(&s.id).is_empty() {
                out.push(NeedRow {
                    title: self.session_title(s),
                    what: format!("{} is waiting for your answer", s.agent.name()),
                    color: crate::sidebar::attention(),
                    agent: Some(s.agent),
                    failing: false,
                    open: Open::Session(s.id.clone()),
                });
            }
        }
        // v2: work the ledger finds stuck (M32), with why.
        for (id, why) in self.journal_stuck() {
            // A server session's work is kept here too (M32's Server line).
            if let Some(r) = self.server.state.as_ref().and_then(|state| state.sessions.iter().find(|r| r.session.id == id)) {
                if !out.iter().any(|row| row.open == Open::Server(id.clone())) {
                    out.push(NeedRow {
                        title: r.session.title.clone(),
                        what: format!("On {} · stuck: {why}", self.server.name()),
                        color: crate::sidebar::attention(),
                        agent: Some(r.session.agent),
                        failing: false,
                        open: Open::Server(id.clone()),
                    });
                }
                continue;
            }
            let Some(s) = self.state.session(&id).filter(|s| !self.state.session_hidden(s)) else { continue };
            if out.iter().any(|row| row.open == Open::Session(id.clone())) {
                continue;
            }
            out.push(NeedRow {
                title: self.session_title(s),
                what: format!("Stuck: {why}"),
                color: crate::sidebar::attention(),
                agent: Some(s.agent),
                failing: false,
                open: Open::Session(id.clone()),
            });
        }
        for r in self.server.state.as_ref().map(|s| s.sessions.as_slice()).unwrap_or_default() {
            if r.activity == Some(glasscode_core::agents::AgentActivity::NeedsInput) {
                out.push(NeedRow {
                    title: r.session.title.clone(),
                    what: format!("On {} · waiting for your answer", self.server.name()),
                    color: crate::sidebar::attention(),
                    agent: Some(r.session.agent),
                    failing: false,
                    open: Open::Server(r.session.id.clone()),
                });
            }
        }
        for s in self.state.sessions.iter().filter(|s| s.review.is_none() && s.team_task.is_none() && !self.state.session_hidden(s)) {
            let Some(status) = self.prs.map.get(&s.id) else { continue };
            let (Some(pr), Some(ci)) = (&status.pr, &status.ci) else { continue };
            let failing = ci.failed().len();
            if failing > 0 && ci.state == "OPEN" {
                let fixing = if self.state.ci_autofix_off { "" } else { " · the agent is asked to fix it" };
                out.push(NeedRow {
                    title: format!("{} · #{}", self.session_title(s), pr.number),
                    what: format!("CI failing ({}){fixing}", if failing == 1 { "1 check".to_string() } else { format!("{failing} checks") }),
                    color: theme::removed(),
                    agent: Some(s.agent),
                    failing: true,
                    open: Open::Session(s.id.clone()),
                });
            }
        }
        // What the manager holds for the person: a message to allow, an urgent one that came in,
        // a follow-up the other side says is fixed. Each opens its part of the manager's panel.
        for need in self.manager_waits() {
            let crate::manager_ui::Go::Stage(stage) = need.go else { continue };
            let row = NeedRow { title: need.title, what: need.detail, color: if need.urgent { theme::removed() } else { crate::sidebar::attention() }, agent: None, failing: false, open: Open::Manager(stage) };
            // An urgent one comes first.
            if need.urgent { out.insert(0, row) } else { out.push(row) }
        }
        out
    }

    /// The tab the right panel shows: the one chosen, or Inbox while the manager's tab has
    /// nothing to show (the manager is off, and it is not Memory that was opened).
    pub(crate) fn today_tab(&self) -> TodayTab {
        match self.today.tab {
            TodayTab::Manager if !self.state.manager.enabled && self.manager.stage != crate::manager_view::Stage::Memory => TodayTab::Inbox,
            tab => tab,
        }
    }

    /// What a click on a row of Today does.
    pub(crate) fn today_open(&mut self, open: &Open, window: &mut Window, cx: &mut Context<Self>) {
        match open {
            Open::Nothing => return,
            // Both show their session in the Sessions view.
            Open::Session(id) => self.open_session(id, window, cx),
            Open::Server(id) => self.open_server_session(id, window, cx),
            Open::Url(url) => crate::center::open_url(url, cx),
            Open::Inbox => {
                self.today.tab = TodayTab::Inbox;
                if !self.show_right {
                    self.set_hidden(Side::Right, false);
                    self.save();
                }
            }
            Open::Usage => self.go(View::Usage, window, cx),
            Open::Manager(stage) => self.manager_open_stage(*stage, cx),
        }
        cx.notify();
    }

    /// Today as phones see it (sent to the server with each sync).
    pub fn today_snapshot(&self, mac_id: &str, mac: &str) -> glasscode_core::remote::TodaySnapshot {
        let needs = self.today_needs();
        let waiting = self.today_waiting_sessions();
        let view = self.today_view();
        let today = day::today();
        glasscode_core::remote::TodaySnapshot {
            mac: mac.to_string(),
            mac_id: mac_id.to_string(),
            launch: self.control.launch.clone(),
            at: glasscode_core::model::now(),
            summary: view.summary_with(waiting, self.control.approvals.len()),
            view,
            needs: needs
                .into_iter()
                .map(|need| glasscode_core::remote::TodayNeed {
                    title: need.title,
                    what: need.what,
                    session: match need.open {
                        Open::Session(id) => Some(id),
                        Open::Server(id) => Some(format!("server:{id}")),
                        _ => None,
                    },
                })
                .collect(),
            approvals: self
                .control
                .approvals
                .iter()
                .map(|a| glasscode_core::remote::TodayApproval { id: a.id, who: a.caller.who.clone(), what: a.what.clone(), command: a.command.clone(), session_title: a.caller.session_title.clone() })
                .collect(),
            activity: self
                .control
                .log
                .iter()
                .filter(|e| e.who != "You" && day::local(e.at as i64) == today)
                .take(12)
                .map(|e| glasscode_core::remote::TodayActivity { at: e.at, who: e.who.clone(), what: lower_first(&e.what), session_title: e.session_title.clone() })
                .collect(),
        }
    }

    /// What the person did on Today on their iPhone.
    pub fn today_apply(&mut self, op: glasscode_core::remote::TodayOp, window: &mut Window, cx: &mut Context<Self>) {
        use glasscode_core::remote::TodayOp;
        let today = day::today();
        let result = match op {
            TodayOp::Done { key, done } => self.today.board.set_done(&key, done, today),
            TodayOp::Snooze { key, until } => self.today.board.snooze(&key, until.as_deref().map(|u| day::parse_iso(u).or_else(|| day::parse_when(u, today)).unwrap_or(today + 1))),
            TodayOp::Focus { key, on } => self.today.board.set_focus(&key, on),
            TodayOp::Add { text } => {
                let draft = board::parse_quick(&text, today, &self.today_people());
                self.today.board.add(draft, board::Origin::you(), today);
                Ok(())
            }
            // Only the person may allow an agent's request. The phone signs the answer with its
            // device key (which this Mac knows from pairing), and names this run of the app, so a
            // number from an earlier run, or from anyone but the phone, does nothing.
            TodayOp::Decide { approval, allow, launch, key, sig } => {
                if launch != self.control.launch {
                    Ok(())
                } else if !self.phone_signed(&key, &sig, &glasscode_core::remote::decide_message(&launch, approval, allow)) {
                    Err("That answer did not come from your paired iPhone.".to_string())
                } else {
                    self.control_decide(approval, allow, false, window, cx);
                    Ok(())
                }
            }
        };
        if let Err(e) = result {
            self.show_toast(format!("From your iPhone: {e}"), cx);
        }
        self.today_save(cx);
    }

    /// Whether `sig` is one of this account's paired iPhones' signatures of `message`.
    fn phone_signed(&self, key: &str, sig: &str, message: &str) -> bool {
        if key.is_empty() || sig.is_empty() {
            return false;
        }
        // The key must be one this Mac paired itself: with the v2 engine, one pinned in the
        // authority store at pairing (trust-m5), never one the server merely lists; with v1, one
        // Glasscode added to the server for a phone (authorized_keys), as before.
        let known = self.control.authority.devices.contains(&glasscode_core::gate::device_id(key));
        known && glasscode_core::remote::verify_phone_signature(key, message.as_bytes(), sig)
    }

    // ----- the start box ---------------------------------------------------------------------------

    /// The agents and accounts a session from the box can start with: each Claude and Codex
    /// account; with none, Claude on its own login (as a task's Start does). A shell takes no
    /// first message, so it is not one: New session starts shells.
    fn start_agents(&self) -> Vec<(AgentKind, Option<String>, String)> {
        let mut out: Vec<(AgentKind, Option<String>, String)> =
            self.state.accounts.iter().filter(|a| a.agent != AgentKind::Shell).map(|a| (a.agent, Some(a.id.clone()), format!("{} · {}", a.agent.name(), a.label))).collect();
        if out.is_empty() {
            out.push((AgentKind::Claude, None, AgentKind::Claude.name().to_string()));
        }
        out
    }

    /// The pill's choice, else the chosen project's `default_agent` (U22, as its file was last
    /// read) with its default account, else the default Claude account, else the first choice.
    fn start_agent(&self) -> (AgentKind, Option<String>, String) {
        let choices = self.start_agents();
        let wanted = self.start_project().map_or(AgentKind::Claude, |(id, _)| self.default_agent(&id, false).0);
        let at = pick_start_agent(&choices, self.today.start_agent.as_ref(), wanted, |agent| self.state.default_account(agent).map(|a| a.id.clone()));
        choices[at].clone()
    }

    /// The pill's project, else the active session's, else the first one; none without a project.
    fn start_project(&self) -> Option<(String, String)> {
        let projects: Vec<(String, String)> = self.state.visible_projects().map(|p| (p.id.clone(), p.name.clone())).collect();
        let wanted = self.today.start_project.clone().or_else(|| self.active_session().map(|s| s.project_id.clone()));
        wanted.and_then(|id| projects.iter().find(|p| p.0 == id).cloned()).or_else(|| projects.into_iter().next())
    }

    /// Where a session of `project_id` can start: a new worktree, or one of its worktrees as
    /// git listed them at the last refresh (a folder without git: the folder). Drawn ten times
    /// a second, so it asks git nothing.
    fn start_froms(&self, project_id: &str) -> Vec<(Option<PathBuf>, String)> {
        let listed = self.worktrees.get(project_id);
        // Not listed yet: git is the likely case; `start_session` looks for itself anyway.
        if listed.is_some_and(|l| l.is_empty()) {
            return vec![(None, "In the folder".into())];
        }
        let mut out: Vec<(Option<PathBuf>, String)> = vec![(None, "New worktree".into())];
        let named = |path: &PathBuf, branch: &str| if branch.is_empty() { path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() } else { branch.to_string() };
        for w in listed.into_iter().flatten() {
            out.push((Some(w.path.clone()), named(&w.path, &w.branch)));
        }
        for s in self.state.sessions.iter().filter(|s| s.project_id == project_id) {
            if !out.iter().any(|(p, _)| p.as_ref() == Some(&s.worktree)) {
                out.push((Some(s.worktree.clone()), named(&s.worktree, &s.branch)));
            }
        }
        out
    }

    fn start_from(&self, project_id: &str) -> (Option<PathBuf>, String) {
        let choices = self.start_froms(project_id);
        self.today.start_from.as_ref().and_then(|wanted| choices.iter().find(|c| c.0.as_ref() == Some(wanted))).unwrap_or(&choices[0]).clone()
    }

    /// Enter in the start box. With the manager on it asks the manager (unless the person chose
    /// that Enter starts a session: `chat_today_mode`); with it off, or while it cannot answer,
    /// it starts a session as it always did (M52.1).
    pub fn today_enter(&mut self, now: bool, window: &mut Window, cx: &mut Context<Self>) {
        match self.chat_enter().mode {
            glasscode_core::runtime::chat::StartMode::Ask => self.today_ask(now, window, cx),
            glasscode_core::runtime::chat::StartMode::Session => self.today_send(window, cx),
        }
    }

    /// The box's quiet second action: what Enter does not do. Start a session directly while
    /// Enter asks the manager; ask the manager while Enter starts a session.
    fn today_other(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let enter = self.chat_enter();
        match enter.mode {
            glasscode_core::runtime::chat::StartMode::Ask => self.today_send(window, cx),
            // While the manager cannot answer there is no second action.
            glasscode_core::runtime::chat::StartMode::Session if enter.choice && enter.why_not.is_none() => self.today_ask(false, window, cx),
            glasscode_core::runtime::chat::StartMode::Session => {}
        }
    }

    /// Ask the manager what is typed in the start box. The answer shows in its panel when the
    /// right panel is there, else under the box.
    fn today_ask(&mut self, now: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (message, untyped) = {
            let start = self.today.start.read(cx);
            let text = start.text();
            let (message, untyped) = glasscode_core::words::trimmed(&text, start.typing());
            (message.to_string(), untyped)
        };
        if message.is_empty() {
            return;
        }
        // "Start a session on X" needs no model turn: the box starts it, as Session would (ux-4).
        if let Some(task) = glasscode_core::runtime::chat::start_request(&message) {
            self.today.start.update(cx, |c, cx| c.set_text(task, cx));
            return self.today_send(window, cx);
        }
        // Typed in the Today box: the person's words, where they typed them (D1).
        let words = glasscode_core::words::record_with(glasscode_core::words::Origin::Composer(glasscode_core::words::ComposerKind::TodayBox), &message, &untyped, None, None);
        self.today.start.update(cx, |c, cx| c.set_text("", cx));
        self.mchat.today_asked = true;
        // The conversation is beside the box when the right panel shows: the answer comes there.
        if self.show_right {
            self.today.tab = TodayTab::Manager;
            self.manager.stage = crate::manager_view::Stage::Chat;
            self.manager.page.chat_scroll.scroll_to_bottom();
        }
        if now && self.mchat.turn.is_some() {
            self.chat_stop(cx);
        }
        self.manager_ask_as(message, false, Some(words), None, None, cx);
        cx.notify();
    }

    /// Send: a session with what is typed as its first message, in the project and from the
    /// start point of the pills, as a task's Start does (`start_session`); it opens when it is
    /// made. The text stays until it worked.
    pub fn today_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (message, untyped) = {
            let start = self.today.start.read(cx);
            let text = start.text();
            let (message, untyped) = glasscode_core::words::trimmed(&text, start.typing());
            (message.to_string(), untyped)
        };
        if message.is_empty() || self.today.sending {
            return;
        }
        let Some((project_id, _)) = self.start_project() else {
            self.show_toast("Add a project first: each session starts in one of your projects.", cx);
            return;
        };
        let (agent, account_id, _) = self.start_agent();
        let (worktree, _) = self.start_from(&project_id);
        self.today.sending = true;
        self.today.menu = None;
        let spec = crate::commands::NewSpec {
            project_id,
            agent,
            account_id,
            title: None,
            branch: None,
            worktree,
            from: None,
            ticket: None,
            pr: None,
            review: None,
            model: None,
            mode: None,
            message: Some(message),
            // Typed in the Today box: the person's words, where they typed them (D1).
            message_origin: Some((glasscode_core::words::Origin::Composer(glasscode_core::words::ComposerKind::TodayBox), untyped)),
            open: true,
            on_server: false,
            started_by: "You".into(),
        };
        let responder = Responder::local(|ws: &mut Workspace, reply| {
            ws.today.sending = false;
            match reply {
                Reply::Done { code: 0, .. } => ws.today.sent = true,
                Reply::Done { text, .. } => ws.toast = Some((text.clone(), Instant::now())),
                _ => {}
            }
        });
        self.start_session(spec, responder, window, cx);
        cx.notify();
    }

    /// The start box: the greeting and the day's sentence, then the field, which is the main
    /// thing of the page. With the manager on it is the manager's box: Enter asks it, and
    /// starting a session directly is the quiet second action beside the pills that say where
    /// that session would start. The field is strong glass with an edge that takes the accent
    /// while the cursor is in it (`focused`), the words at a large size, and under them the
    /// pills, the microphone and Send.
    fn today_start_box(&mut self, view: &TodayView, waiting: usize, focused: bool, managed: &[glasscode_core::manager::SessionInfo], cx: &mut Context<Self>) -> AnyElement {
        use glasscode_core::runtime::chat::StartMode;
        let today = day::today();
        let name = self.tickets.snapshot.as_ref().and_then(|s| s.viewer.name.split_whitespace().next().map(crate::control_ui::capitalize));
        let greeting = match day::hour_now() {
            4..=11 => "Good morning",
            12..=17 => "Good afternoon",
            _ => "Good evening",
        };
        // The day in one sentence: the board's, then what the manager adds to it.
        let mut summary = view.summary_with(waiting, self.control.approvals.len());
        let managers = self.manager_sentence(managed);
        if !managers.is_empty() {
            summary = format!("{summary} {managers}");
        }
        let header = div()
            .flex()
            .flex_col()
            .gap(theme::sz(4.))
            .px(theme::sz(4.))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_baseline()
                    .gap_x(theme::sz(12.))
                    .child(div().flex_1().min_w_0().text_size(theme::sz(22.)).line_height(theme::sz(28.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(match name {
                        Some(n) => format!("{greeting}, {n}"),
                        None => greeting.to_string(),
                    }))
                    // In a narrow centre the date is cut with an ellipsis, not by the card's edge.
                    .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(theme::sz(12.5)).text_color(theme::card_soft_text()).child(day::long(today))),
            )
            .child(div().text_size(theme::sz(13.5)).line_height(theme::sz(19.)).text_color(theme::text_soft()).child(summary));
        let frame = backdrop::glass(theme::sz(18.)).flex().flex_col().gap(theme::sz(cards::GAP)).p(theme::sz(cards::GAP)).pt(theme::sz(16.)).when(theme::is_light(), |d| d.shadow_md()).child(header);
        // The field's edge at rest: the text colour, thin, which shows on glass in each theme
        // (the border colours are made for opaque ground).
        let mut edge = theme::text();
        edge.a = 0.24;
        let field = backdrop::glass_strong(theme::sz(14.)).flex().flex_col().border(theme::sz(1.5)).border_color(if focused { theme::accent() } else { edge }).when(theme::is_light() || focused, |d| d.shadow_md());

        // No project: what to do first.
        let Some((project_id, project_name)) = self.start_project() else {
            let field = field.p(theme::sz(18.)).child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(theme::sz(12.))
                    .child(div().flex_1().min_w(theme::sz(200.)).text_size(theme::sz(14.)).text_color(theme::text()).child("Add a project to start a session here: each session works in one of your folders."))
                    .child(ui::primary_button("today-add-project", "Add project…").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.prompt_add_project(window, cx)))),
            );
            return motion::fade_in("today-start", frame.child(field)).into_any_element();
        };
        let agents = self.start_agents();
        let (agent, _, agent_label) = self.start_agent();
        let projects = self.state.visible_projects().count();
        let froms = self.start_froms(&project_id);
        let (_, from_label) = self.start_from(&project_id);
        let typed = !self.today.start.read(cx).text().trim().is_empty();
        let can_send = typed && !self.today.sending;
        // With the manager on the box is its box: the placeholder says what Enter does (ux-4).
        let enter = self.chat_enter();
        let asking = enter.mode == StartMode::Ask;
        let manager_name = self.state.manager.name.clone();
        let placeholder = glasscode_core::runtime::chat::placeholder(enter.mode, &manager_name);
        if self.mchat.placeholder.as_deref() != Some(placeholder.as_str()) {
            self.mchat.placeholder = Some(placeholder.clone());
            self.today.start.update(cx, |c, _| c.set_placeholder(placeholder));
        }
        // What Enter does not do, as a quiet button beside the pills (none with the manager off,
        // or while it cannot answer).
        let other: Option<(&'static [u8], String, String)> = match (enter.choice, asking, enter.why_not.is_some()) {
            (true, true, _) => Some((icons::TERMINAL, "Start session".to_string(), "Start a session directly with these words, with the agent, in the project and from the start point beside this button".to_string())),
            (true, false, false) => Some((icons::ASSISTANT, format!("Ask {manager_name}"), format!("Ask {manager_name} these words instead of starting a session"))),
            _ => None,
        };

        let pill = |id: &'static str, icon: AnyElement, label: String, which: Option<StartMenu>, cx: &mut Context<Self>| {
            // A pill is as wide as its words, and no wider than its row: in a centre narrower
            // than the pill, the words are cut with an ellipsis.
            div()
                .id(id)
                .flex()
                .min_w_0()
                .max_w(theme::sz(240.))
                .items_center()
                .gap(theme::sz(6.))
                .h(theme::sz(30.))
                .px(theme::sz(10.))
                .rounded(theme::sz(8.))
                .border_1()
                .border_color(theme::border())
                .text_size(theme::sz(12.5))
                .text_color(theme::text_soft())
                .child(icon)
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))
                // One choice: the pill only says what it is.
                .when_some(which, |d, which| {
                    d.cursor_pointer()
                        .hover(|s| s.bg(theme::tint()).text_color(theme::text()))
                        .child(icons::icon(icons::CHEVRON_DOWN, theme::sz(12.), theme::sz(12.), theme::card_soft_text()))
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                            let press = match event {
                                ClickEvent::Mouse(click) => Some(click.down.position),
                                _ => None,
                            };
                            let open = pill_click(this.today.menu.map(|(open, _)| open), this.today.menu_closed_by.take(), which, press);
                            this.today.menu = open.then(|| (which, event.position()));
                            cx.notify();
                        }))
                })
        };
        let other_button = other.map(|(glyph, label, words)| {
            div()
                .id("today-start-other")
                .role(gpui::Role::Button)
                .aria_label(words.clone())
                .flex()
                .flex_none()
                .items_center()
                .gap(theme::sz(6.))
                .h(theme::sz(30.))
                .px(theme::sz(10.))
                .rounded(theme::sz(8.))
                .text_size(theme::sz(12.5))
                .font_weight(FontWeight::MEDIUM)
                .tooltip(ui::tip(words))
                .map(|d| {
                    if can_send {
                        d.bg(theme::tint()).text_color(theme::text()).cursor_pointer().hover(|s| s.bg(theme::hover())).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.today_other(window, cx)))
                    } else {
                        d.text_color(theme::card_soft_text())
                    }
                })
                .child(icons::icon(glyph, theme::sz(14.), theme::sz(14.), if can_send { theme::text_soft() } else { theme::card_soft_text() }))
                .child(label)
        });
        // Send is the one filled thing of the page: the accent, paler until there are words.
        let mut send_fill = theme::accent();
        if !can_send {
            send_fill.a *= 0.38;
        }
        let send_words = if asking { format!("Ask {manager_name} (Enter)") } else { "Start a session (Enter)".to_string() };
        // The pills wrap in a narrow centre; Send stays at the right end.
        let pills = div()
            .flex()
            .items_end()
            .gap(theme::sz(8.))
            .px(theme::sz(12.))
            .pb(theme::sz(12.))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .flex_wrap()
                    .items_center()
                    .gap(theme::sz(6.))
                    .child(pill("today-start-agent", icons::brand(icons::agent_brand(agent), 14.), agent_label, (agents.len() > 1).then_some(StartMenu::Agent), cx))
                    .child(pill("today-start-project", icons::icon(icons::FOLDER, theme::sz(14.), theme::sz(14.), theme::card_soft_text()).into_any_element(), project_name, (projects > 1).then_some(StartMenu::Project), cx))
                    .child(pill("today-start-from", icons::icon(icons::BRANCH, theme::sz(14.), theme::sz(14.), theme::card_soft_text()).into_any_element(), from_label, (froms.len() > 1).then_some(StartMenu::From), cx))
                    .children(other_button),
            )
            .when(crate::glasscode_ui::VOICE, |d| d.child(cards::mic_button().on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.voice_open(window, cx)))))
            .child(
                div()
                    .id("today-send")
                    .role(gpui::Role::Button)
                    .aria_label(send_words.clone())
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(theme::sz(38.))
                    .rounded_full()
                    .bg(send_fill)
                    .tooltip(ui::tip(send_words))
                    .when(can_send, |d| d.cursor_pointer().hover(|s| s.opacity(0.88)).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.today_enter(false, window, cx))))
                    .child(icons::icon(icons::SEND, theme::sz(17.), theme::sz(17.), theme::on_accent())),
            );
        let why_not = enter.why_not.clone();
        // Things to ask the manager, one click each: one row of them. It stays while the person
        // types, so the cards under the box do not move.
        let answer_here = self.mchat.today_asked && enter.choice && !self.assistant_docked();
        let tries = (asking && !answer_here).then(|| {
            div().flex().flex_wrap().gap(theme::sz(6.)).h(theme::sz(26.)).overflow_hidden().px(theme::sz(4.)).children(crate::manager_ui::TRY.iter().enumerate().map(|(i, phrase)| {
                crate::manager_ui::try_chip(SharedString::from(format!("today-try-{i}")), phrase).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.mchat.today_asked = true;
                    if this.show_right {
                        this.today.tab = TodayTab::Manager;
                        this.manager.stage = crate::manager_view::Stage::Chat;
                    }
                    this.manager_ask(phrase.to_string(), false, cx);
                    cx.notify();
                }))
            }))
        });
        // The answer to what was asked here, in the conversation's own component (ux-3), while
        // the conversation itself is not on screen beside the box.
        let thread = answer_here.then(|| {
            div()
                .flex()
                .flex_col()
                .gap(theme::sz(10.))
                .px(theme::sz(4.))
                .pt(theme::sz(2.))
                .child(self.render_chat_thread(crate::manager_chat::ThreadView { id: "today-thread", limit: 2, compact: true, fold: true, topics: false }, cx))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(theme::sz(4.))
                        .ml(theme::sz(-6.))
                        .child(
                            crate::manager_chat::quiet_action("today-thread-open".into(), icons::PANEL_RIGHT, "Open the conversation")
                                .role(gpui::Role::Link)
                                .aria_label("Open the full conversation")
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.show_manager(window, cx))),
                        )
                        .child(crate::manager_chat::quiet_action("today-thread-hide".into(), icons::CLOSE, "Hide").role(gpui::Role::Button).aria_label("Hide the answer").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.mchat.today_asked = false;
                            cx.notify();
                        }))),
                )
        });
        // The whole of the field above the pills takes the cursor, not only the line of words.
        let words = div()
            .id("today-start-input")
            .role(gpui::Role::TextInput)
            .aria_label(placeholder_label(asking, &manager_name))
            .px(theme::sz(18.))
            .pt(theme::sz(16.))
            .pb(theme::sz(10.))
            .min_h(theme::sz(84.))
            .text_size(theme::sz(16.))
            .cursor(gpui::CursorStyle::IBeam)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                let handle = this.today.start.read(cx).focus_handle_clone();
                window.focus(&handle, cx);
            }))
            .child(self.today.start.clone());
        let field = field
            .child(words)
            .child(pills)
            .when_some(why_not, |d, why| d.child(div().px(theme::sz(18.)).pb(theme::sz(12.)).text_size(theme::sz(12.)).text_color(theme::muted()).child(format!("Enter starts a session: {why}."))));
        motion::fade_in("today-start", frame.child(field).children(tries).children(thread)).into_any_element()
    }

    /// The open list of a pill of the start box.
    fn today_start_menu(&self, which: StartMenu, at: Point<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let choice = |id: String, label: String, on: bool| {
            div()
                .id(SharedString::from(id))
                .flex()
                .items_center()
                .gap(theme::sz(8.))
                .px(theme::sz(10.))
                .py(theme::sz(6.))
                .rounded(theme::sz(6.))
                .text_size(theme::sz(13.))
                .text_color(theme::text())
                .when(on, |d| d.bg(theme::selected()).font_weight(FontWeight::MEDIUM))
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))
        };
        let rows: Vec<AnyElement> = match which {
            StartMenu::Agent => {
                let (agent_now, account_now, _) = self.start_agent();
                self.start_agents()
                    .into_iter()
                    .enumerate()
                    .map(|(i, (agent, account, label))| {
                        let on = agent == agent_now && account == account_now;
                        choice(format!("today-agent-{i}"), label, on)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.today.start_agent = Some((agent, account.clone()));
                                this.today.menu = None;
                                cx.notify();
                            }))
                            .into_any_element()
                    })
                    .collect()
            }
            StartMenu::Project => {
                let now = self.start_project().map(|p| p.0);
                self.state
                    .visible_projects()
                    .enumerate()
                    .map(|(i, p)| {
                        let id = p.id.clone();
                        choice(format!("today-project-{i}"), p.name.clone(), now.as_deref() == Some(p.id.as_str()))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.today.start_project = Some(id.clone());
                                // A worktree of the project before is no start point here, and
                                // the agent is the new project's default until it is changed.
                                this.today.start_from = None;
                                this.today.start_agent = None;
                                this.today.menu = None;
                                cx.notify();
                            }))
                            .into_any_element()
                    })
                    .collect()
            }
            StartMenu::From => {
                let Some((project_id, _)) = self.start_project() else { return div().into_any_element() };
                let now = self.start_from(&project_id).0;
                self.start_froms(&project_id)
                    .into_iter()
                    .enumerate()
                    .map(|(i, (path, label))| {
                        let on = path == now;
                        choice(format!("today-from-{i}"), label, on)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.today.start_from = path.clone();
                                this.today.menu = None;
                                cx.notify();
                            }))
                            .into_any_element()
                    })
                    .collect()
            }
        };
        deferred(
            anchored().position(at).snap_to_window_with_margin(theme::sz(8.)).child(
                div()
                    .id("today-start-menu")
                    .occlude()
                    .min_w(theme::sz(200.))
                    .max_w(theme::sz(320.))
                    .max_h(theme::sz(320.))
                    .overflow_y_scroll()
                    .p_1()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme::border_strong())
                    .bg(theme::dialog())
                    .shadow_lg()
                    .on_mouse_down_out(cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                        this.today.menu_closed_by = this.today.menu.take().map(|(which, _)| (which, event.position));
                        cx.notify();
                    }))
                    .children(rows),
            ),
        )
        .with_priority(4)
        .into_any_element()
    }

    // ----- the page --------------------------------------------------------------------------------

    pub fn render_today(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // The voice screen covers Today (U19); the dashboard is not laid out under it.
        if self.voice_shows() {
            return self.render_voice(window, cx);
        }
        let view = self.today_view();
        let waiting_sessions = self.today_waiting_sessions();
        // The width of the centre, as at 100 %: the window less the sidebars that show.
        let mut centre = self.window_width;
        if self.view_has_left() && !self.state.layout.left_hidden {
            centre -= self.shown_width(Side::Left);
        }
        if self.view_has_right() && self.show_right {
            centre -= self.shown_width(Side::Right);
        }
        let columns = cards::columns(centre / theme::scale());
        use crate::shell::frames::{Part, part};
        // The start box takes the accent on its edge while the cursor is in it.
        let focused = self.today.start.read(cx).focus_handle_clone().is_focused(window);
        // What the manager knows of the sessions it manages, read once for this frame.
        let managed = self.manager_session_lines();
        let start = part(Part::Start, || self.today_start_box(&view, waiting_sessions, focused, &managed, cx));
        // The picture the manager explains, while it explains one: between its box and the cards.
        let picture = self.render_scene_card(cx);
        let grid = part(Part::Cards, || self.today_grid(&view, columns, &managed, cx));
        let list = part(Part::List, || self.today_list(&view, cx));
        let menu = self.today.menu.map(|(which, at)| self.today_start_menu(which, at, cx));
        let content = div().w_full().flex().justify_center().child(
            div().w_full().max_w(theme::sz(1000.)).flex().flex_col().gap(theme::sz(cards::GAP)).px(theme::sz(22.)).pt(theme::sz(28.)).pb(theme::sz(40.)).child(start).children(picture).child(grid).child(list),
        );
        let layer = self.backdrop_layer(0.85, window, cx);
        if cfg!(debug_assertions) && self.today.debug_end {
            self.today.scroll.set_offset(gpui::point(px(0.), -self.today.scroll.max_offset().y));
        }
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
            .child(ui::scroll_area("today-scroll", &self.today.scroll, content))
            .children(menu)
            .into_any_element()
    }

    /// v2 (M32): what the work ledger says comes next for each piece of open work whose session
    /// shows, after the board's own items (`WorkLedger::coming_up`).
    fn today_coming_work(&self) -> Vec<cards::Upcoming> {
        let Some(ledger) = self.work_ledger() else { return Vec::new() };
        ledger
            .coming_up()
            .into_iter()
            .filter_map(|next| {
                let id = next.session.as_deref()?;
                let what = match &next.pr {
                    Some((repo, number)) => format!("{} · {repo}#{number}", next.what),
                    None => next.what.clone(),
                };
                // A server session's work (M32's Server line): its title, on the server.
                let title = match self.state.session(id) {
                    Some(session) if self.state.session_hidden(session) => return None,
                    Some(session) => self.session_title(session),
                    None => {
                        let remote = self.server.state.as_ref()?.sessions.iter().find(|r| r.session.id == id)?;
                        format!("{} (on {})", remote.session.title, self.server.name())
                    }
                };
                Some(cards::Upcoming { key: format!("{WORK_KEY}{id}|{}", next.key.id), when: next.when.to_string(), title, detail: upper_first(&what), url: None })
            })
            .collect()
    }

    /// The cards, in a grid of three, two or one columns. The cards of a row are as high as each
    /// other, and a list scrolls in its card. Each card fades in when Today first shows, one
    /// after the other.
    fn today_grid(&self, view: &TodayView, columns: usize, managed: &[glasscode_core::manager::SessionInfo], cx: &mut Context<Self>) -> AnyElement {
        let today = day::today();
        let today_iso = day::iso(today);
        let (sample, waits) = (self.today.debug_sample, self.today.debug_loading);
        let needs: Vec<cards::Need> = if sample { sample::needs() } else { self.today_needs().into_iter().map(|n| cards::Need { title: n.title, what: n.what, agent: n.agent, failing: n.failing, open: n.open }).collect() };
        let needs = cards::needs(&needs, self.control.approvals.len());
        let items: Vec<Item> = view.items().cloned().collect();
        let mut upcoming = if sample { sample::coming_up() } else { cards::coming_up(&items, today) };
        if !sample {
            upcoming.extend(self.today_coming_work());
            // What the manager does next, by itself, and when its daily brief comes.
            upcoming.extend(self.manager_next().into_iter().enumerate().map(|(n, (what, when))| cards::Upcoming { key: format!("{MANAGER_KEY}{n}"), when: if when.is_empty() { "Next".to_string() } else { when }, title: what, detail: format!("{} does this by itself", self.state.manager.name), url: None }));
        }
        let coming = cards::coming_up_list(&upcoming, |u| match &u.url {
            Some(url) => Open::Url(url.clone()),
            None if u.key.starts_with(MANAGER_KEY) => Open::Manager(crate::manager_view::Stage::Chat),
            // Work of the ledger: its session.
            None if u.key.starts_with(WORK_KEY) => match u.key.strip_prefix(WORK_KEY).and_then(|rest| rest.split_once('|')).map(|(id, _)| id.to_string()) {
                Some(id) if self.state.session(&id).is_some() => Open::Session(id),
                Some(id) if self.server.state.as_ref().is_some_and(|state| state.sessions.iter().any(|r| r.session.id == id)) => Open::Server(id),
                _ => Open::Nothing,
            },
            // A task with a session: that session.
            None => u
                .key
                .strip_prefix("task:")
                .and_then(|id| self.today.board.task(id))
                .and_then(|t| t.session.clone())
                .filter(|id| self.state.session(id).is_some())
                .map_or(Open::Nothing, Open::Session),
        });
        let now = glasscode_core::model::now();
        let mut accounts: Option<Vec<cards::UsageAccount>> = self.usage.report.as_ref().map(|report| {
            report
                .accounts
                .iter()
                .filter_map(|a| Some(cards::UsageAccount { agent: a.agent?, label: self.state.account(&a.id).map_or_else(|| a.id.clone(), |x| x.label.clone()), limits: a.limits.clone() }))
                .collect()
        });
        if sample {
            accounts = Some(sample::usage(now as i64));
        }
        if waits {
            accounts = None;
        }
        let usage = cards::usage(accounts.as_deref(), now as i64);
        let activity = self.today.activity.as_ref().filter(|_| !waits);
        let heat = cards::heat(activity, &today_iso);
        // What the manager knows of each session it manages: where its last reply ends, and the
        // sessions it sees on the server.
        let mut recent: Vec<cards::Recent> = self
            .state
            .visible_sessions()
            .map(|s| cards::Recent {
                id: s.id.clone(),
                title: self.session_title(s),
                project: self.state.project(&s.project_id).map(|p| p.name.clone()).unwrap_or_default(),
                agent: s.agent,
                state: self.row_state(s),
                created_at: s.created_at,
                line: managed.iter().find(|info| info.id == s.id && info.last_reply.is_some()).map(Self::manager_line).unwrap_or_default(),
                on_server: false,
            })
            .collect();
        for info in managed.iter().filter(|info| info.on_server && self.state.session(&info.id).is_none()) {
            let Some(remote) = self.server.state.as_ref().and_then(|state| state.sessions.iter().find(|r| r.session.id == info.id)) else { continue };
            recent.push(cards::Recent {
                id: info.id.clone(),
                title: if info.title.trim().is_empty() { info.branch.clone() } else { info.title.clone() },
                project: self.server.name(),
                agent: remote.session.agent,
                state: match info.state {
                    glasscode_core::manager::SessionState::NeedsYou => crate::sidebar::RowState::NeedsYou,
                    glasscode_core::manager::SessionState::Working => crate::sidebar::RowState::Working,
                    _ => crate::sidebar::RowState::Idle,
                },
                created_at: remote.session.created_at,
                line: Self::manager_line(info),
                on_server: true,
            });
        }
        let recent = cards::recent(recent, now);
        let prs = cards::pull_requests(self.today.prs.as_ref().filter(|_| !waits));
        let month = cards::month(activity, &today_iso);

        // What is on its way: a skeleton stands for it, and only while it is (the usage scan
        // starts a while after Glasscode does; until then the card says so in words).
        let usage_wait = waits || (self.usage.report.is_none() && self.usage.loading);
        let prs_wait = waits || (self.today.prs.is_none() && self.today.prs_loading);
        let heat_wait = activity.is_none();
        // A heat map with nothing to show is the narrow card of its row (not while it loads:
        // the row would change under the eye a moment later).
        let quiet = !heat_wait && heat.empty.is_some();
        let scroll = &self.today.card_scroll;
        if cfg!(debug_assertions) && self.today.debug_scrolled {
            for handle in [&scroll.needs, &scroll.coming, &scroll.usage, &scroll.recent, &scroll.prs] {
                handle.set_offset(gpui::point(px(0.), -handle.max_offset().y));
            }
        }
        let laid = [
            (Card::Needs, needs.empty.is_some()),
            (Card::ComingUp, coming.empty.is_some()),
            (Card::Usage, usage.empty.is_some()),
            // While the history is read, the card is as it will be with a map.
            (Card::Activity, heat.empty.is_some() && !heat_wait),
            (Card::Recent, recent.empty.is_some()),
            (Card::PullRequests, prs.empty.is_some()),
            (Card::Month, month.empty.is_some()),
        ];
        let heights = cards::heights(&laid, columns, quiet);
        let bodies: [(&'static [u8], Tone, &'static str, Option<String>, AnyElement); 7] = [
            (icons::BELL, Tone::Attention, "Needs you", needs.count.clone(), cards::list("today-needs", icons::BELL, &needs, false, &scroll.needs, cx)),
            (icons::CALENDAR, Tone::Soft, "Coming up", coming.count.clone(), cards::list("today-coming", icons::CALENDAR, &coming, false, &scroll.coming, cx)),
            (icons::GAUGE, Tone::Soft, "Usage", None, cards::rings(&usage, usage_wait, &scroll.usage, cx)),
            (icons::GRID, Tone::Soft, "Activity", None, cards::heat_map(&heat, heat_wait)),
            (icons::SESSIONS, Tone::Soft, "Recent sessions", recent.count.clone(), cards::list("today-recent", icons::SESSIONS, &recent, false, &scroll.recent, cx)),
            (icons::PULL_REQUEST, Tone::Soft, "Pull requests", prs.count.clone(), cards::list("today-prs", icons::PULL_REQUEST, &prs, prs_wait, &scroll.prs, cx)),
            (icons::CHART, Tone::Soft, "This month", None, cards::month_stats(&month)),
        ];
        // The id is the card's, and the wrapper is there whatever the card shows: the fade plays
        // once when Today shows, not when a list arrives.
        let cells = bodies.into_iter().enumerate().map(|(i, (glyph, tone, title, count, body))| {
            let card = cards::card(glyph, tone, title, count, heights[i], body).col_span(cards::span(laid[i].0, columns, quiet));
            motion::fade_in_after(("today-card", i), i + 1, card).into_any_element()
        });
        div().grid().grid_cols(cards::TRACKS).gap(theme::sz(cards::GAP)).children(cells).into_any_element()
    }

    /// "Your list": the board, with its filters, its add box and its sections, on glass.
    fn today_list(&mut self, view: &TodayView, cx: &mut Context<Self>) -> AnyElement {
        let filters = div().flex().flex_wrap().gap(theme::sz(6.)).children(Filter::ALL.iter().map(|(filter, label)| {
            let filter = *filter;
            let on = self.today.filter == filter;
            let count: usize = view.sections.iter().filter(|s| s.section.is_some_and(|sec| filter.shows(sec) && !matches!(sec, Section::Later | Section::Recurring | Section::Done))).map(|s| s.items.iter().filter(|i| !i.done).count()).sum();
            // On the ink of the chosen filter, its number in the ground's colour, softer.
            let mut on_count = theme::bg();
            on_count.a = 0.7;
            div()
                .id(SharedString::from(format!("today-filter-{label}")))
                .flex()
                .items_center()
                .gap_1p5()
                .h(theme::sz(28.))
                .px_3()
                .rounded_full()
                .border_1()
                .border_color(if on { theme::text_strong() } else { theme::border_strong() })
                .when(on, |d| d.bg(theme::text_strong()))
                .text_size(theme::sz(12.5))
                .text_color(if on { theme::bg() } else { theme::text_soft() })
                .font_weight(if on { FontWeight::MEDIUM } else { FontWeight::NORMAL })
                .cursor_pointer()
                .when(!on, |d| d.hover(|s| s.bg(theme::tint())))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.today.filter = filter;
                    cx.notify();
                }))
                .child(label.to_string())
                .child(div().text_color(if on { on_count } else { theme::card_soft_text() }).child(count.to_string()))
        }));

        // The add box is a field: opaque, as text boxes are.
        let has_text = !self.today.add.read(cx).text().trim().is_empty();
        let add = div()
            .flex()
            .items_center()
            .gap_3()
            .h(theme::sz(46.))
            .pl(theme::sz(14.))
            .pr(theme::sz(8.))
            .rounded(theme::sz(12.))
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .child(crate::icons::icon(crate::icons::ADD, theme::sz(18.), theme::sz(18.), theme::faint()))
            .child(div().flex_1().min_w_0().child(self.today.add.clone()))
            .when_some(self.today.hint.clone(), |d, hint| d.child(div().flex_none().text_size(theme::sz(12.5)).text_color(theme::muted()).child(hint)))
            .child(
                div()
                    .id("today-add")
                    .flex_none()
                    .h(theme::sz(32.))
                    .px_3p5()
                    .flex()
                    .items_center()
                    .rounded(theme::sz(8.))
                    .text_size(theme::sz(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(has_text, |d| d.bg(theme::text_strong()).text_color(theme::bg()).cursor_pointer().hover(|s| s.opacity(0.88)))
                    .when(!has_text, |d| d.bg(theme::hover()).text_color(theme::faint()))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.today_add(cx)))
                    .child("Add"),
            );

        let mut sections: Vec<AnyElement> = Vec::new();
        for (index, s) in view.sections.iter().enumerate() {
            let Some(section) = s.section else { continue };
            if !self.today.filter.shows(section) {
                continue;
            }
            // Empty sections stay out of the way — unless this is the only section the filter
            // shows, where a blank page would look broken.
            let filtered = self.today.filter != Filter::All;
            if s.items.is_empty() && section != Section::Focus && !filtered {
                continue;
            }
            let open = !self.today.closed.contains(&section) || (self.today.filter != Filter::All && self.today.filter.shows(section));
            let rows: Vec<AnyElement> = if open { s.items.iter().enumerate().map(|(i, item)| self.today_row(item, index * 1000 + i, cx)).collect() } else { Vec::new() };
            sections.push(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id(SharedString::from(format!("today-section-{}", section.key())))
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .pb_2()
                            .border_b_1()
                            .border_color(theme::border())
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if !this.today.closed.remove(&section) {
                                    this.today.closed.insert(section);
                                }
                                cx.notify();
                            }))
                            .child(div().font_weight(FontWeight::SEMIBOLD).text_size(theme::sz(14.)).text_color(theme::text()).child(s.title.clone()))
                            .child(div().text_size(theme::sz(13.)).text_color(theme::card_soft_text()).child(s.items.len().to_string()))
                            .child(div().ml_auto().text_size(theme::sz(12.5)).text_color(theme::card_soft_text()).hover(|st| st.text_color(theme::text())).child(if open { "Hide" } else { "Show" })),
                    )
                    .when(open && s.items.is_empty(), |d| {
                        d.child(div().py_3().px_1().text_size(theme::sz(13.)).text_color(theme::card_soft_text()).child(if section == Section::Focus { "Star up to three tasks to keep them here." } else { "Nothing here." }))
                    })
                    .when(open && !rows.is_empty(), |d| d.child(div().flex().flex_col().pt_1().children(rows)))
                    .into_any_element(),
            );
        }

        backdrop::glass_strong(theme::sz(16.))
            .flex()
            .flex_col()
            .gap(theme::sz(18.))
            .px(theme::sz(18.))
            .pt(theme::sz(16.))
            .pb(theme::sz(20.))
            .when(theme::is_light(), |d| d.shadow_md())
            .child(div().text_size(theme::sz(15.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Your list"))
            .child(filters)
            .child(add)
            .children(sections)
            .into_any_element()
    }

    /// A row of "Your list". It lies on glass: its words are `text` or `card_soft_text`, and a
    /// colour is a dot beside them (late, due now, urgent).
    fn today_row(&self, item: &Item, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let group = SharedString::from(format!("today-row-{index}"));
        let done = item.done;
        let chip = self.today_chip(item);
        let mut meta: Vec<AnyElement> = Vec::new();
        let sep = || div().mx(theme::sz(7.)).child("·").into_any_element();
        let marked = |color: Hsla, text: String| div().flex().items_center().gap(theme::sz(5.)).text_color(theme::text()).font_weight(FontWeight::MEDIUM).child(ui::dot(color)).child(text).into_any_element();
        if let (Some(label), false) = (&item.due_label, done) {
            meta.push(if item.late {
                marked(theme::removed(), label.clone())
            } else if item.due_now {
                marked(theme::attention(), label.clone())
            } else {
                div().font_weight(FontWeight::MEDIUM).child(label.clone()).into_any_element()
            });
        }
        if let Some(back) = &item.snoozed {
            meta.push(div().child(format!("Back {}", back.to_lowercase())).into_any_element());
        }
        if let Some(person) = &item.person {
            let text = match item.person_role.as_deref() {
                Some("asked") => format!("{person} asked"),
                Some("waiting") => format!("Waiting on {person}"),
                Some("author") => format!("by {person}"),
                _ => person.clone(),
            };
            meta.push(div().flex().items_center().gap(theme::sz(5.)).child(avatar(person)).child(text).into_any_element());
        }
        if let Some(source) = &item.source {
            meta.push(div().child(source.clone()).into_any_element());
        }
        if let Some(chip) = &chip {
            let color = if chip.needs { crate::sidebar::attention() } else if chip.working { theme::added() } else { theme::border_strong() };
            meta.push(div().flex().items_center().gap(theme::sz(5.)).child(div().size(theme::sz(6.)).rounded_full().bg(color)).child(chip.title.clone()).into_any_element());
        }
        if item.urgent && !done {
            meta.push(marked(theme::removed(), "Urgent".into()));
        }
        if let Some(by) = &item.added_by {
            meta.push(div().child(by.clone()).into_any_element());
        }
        let mut meta_row: Vec<AnyElement> = Vec::new();
        for (i, m) in meta.into_iter().enumerate() {
            if i > 0 {
                meta_row.push(sep());
            }
            meta_row.push(m);
        }
        let is_recurring = item.key.starts_with("recurring:");
        let start_label = if chip.as_ref().and_then(|c| c.work.as_ref()).is_some() {
            "Open"
        } else if item.key.starts_with("review:") || item.key.starts_with("linear-review:") {
            "Review"
        } else {
            "Start"
        };
        let (a, b, c, d, e) = (item.clone(), item.clone(), item.clone(), item.clone(), item.clone());
        // A recurring task is ticked on the day it comes back, not here.
        let (glyph, color) = match (done, is_recurring) {
            (true, _) => (icons::CHECKBOX_CHECKED, theme::added()),
            (false, true) => (icons::CHECKBOX, theme::border_strong()),
            (false, false) => (icons::CHECKBOX, theme::card_soft_text()),
        };
        let checkbox = div()
            .id(SharedString::from(format!("today-check-{index}")))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .size(theme::sz(22.))
            .rounded(theme::sz(6.))
            .child(icons::icon(glyph, theme::sz(20.), theme::sz(20.), color))
            .when(!is_recurring, |d| d.cursor_pointer().hover(|s| s.bg(theme::tint())).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.today_toggle_done(&a, cx))));
        let icon_button = |id: String, glyph: &'static [u8], color: Hsla| {
            div()
                .id(SharedString::from(id))
                .size(theme::sz(30.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme::sz(8.))
                .cursor_pointer()
                .hover(|s| s.bg(theme::tint()))
                .child(icons::icon(glyph, theme::sz(15.), theme::sz(15.), color))
        };
        let actions = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(theme::sz(2.))
            .invisible()
            .group_hover(group.clone(), |s| s.visible())
            .when(!done && !is_recurring, |row| {
                row.child(
                    icon_button(format!("today-star-{index}"), if b.focus { icons::STAR_FILLED } else { icons::STAR }, if b.focus { crate::sidebar::attention() } else { theme::card_soft_text() })
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.today_toggle_focus(&b, cx))),
                )
                .child(
                    icon_button(format!("today-snooze-{index}"), icons::MOON, if c.snoozed.is_some() { crate::sidebar::attention() } else { theme::card_soft_text() })
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.today_toggle_snooze(&c, cx))),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("today-start-{index}")))
                        .h(theme::sz(30.))
                        .px(theme::sz(11.))
                        .flex()
                        .items_center()
                        .rounded(theme::sz(8.))
                        .border_1()
                        .border_color(theme::border_strong())
                        .bg(theme::raised())
                        .text_size(theme::sz(12.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::text())
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.today_start(&d, window, cx)))
                        .child(start_label),
                )
            })
            .when(item.key.starts_with("task:") && done, |row| {
                row.child(
                    div()
                        .id(SharedString::from(format!("today-undo-{index}")))
                        .px_2()
                        .text_size(theme::sz(12.5))
                        .text_color(theme::card_soft_text())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::text()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.today_toggle_done(&e, cx)))
                        .child("Undo"),
                )
            });
        let title_color = if done { theme::card_soft_text() } else { theme::text() };
        div()
            .id(SharedString::from(format!("today-item-{index}")))
            .group(group)
            .flex()
            .items_start()
            .gap_3()
            .pl_1()
            .pr_2()
            .py(theme::sz(8.))
            .rounded(theme::sz(10.))
            .hover(|s| s.bg(theme::tint()))
            .child(checkbox)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(theme::sz(3.))
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap_2()
                            .text_size(theme::sz(14.5))
                            .line_height(theme::sz(21.))
                            .text_color(title_color)
                            .when(done, |d| d.line_through())
                            .when_some(item.reference.clone().filter(|_| item.outside), |d, r| {
                                d.child(div().flex_none().pt(theme::sz(1.5)).font_family(theme::MONO_FONT).text_size(theme::sz(12.5)).text_color(theme::card_soft_text()).child(r))
                            })
                            .child(div().flex_1().min_w_0().child(item.title.clone())),
                    )
                    .when(!meta_row.is_empty(), |d| d.child(div().flex().flex_wrap().items_center().text_size(theme::sz(12.5)).text_color(theme::card_soft_text()).children(meta_row))),
            )
            .child(actions)
            .into_any_element()
    }

    // ----- the right panel -------------------------------------------------------------------------

    /// Today's right panel: the manager (its conversation and what it holds), Inbox (what agents
    /// ask to do, and what needs the person now) and Activity (what agents did today). It is
    /// opaque, as the session's is.
    pub fn render_today_right(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.today_tab();
        let needs = self.today_needs();
        let inbox_count = needs.len() + self.control.approvals.len();
        let manager_on = self.state.manager.enabled;
        // With the manager off its tab is Memory alone, and only once Memory was opened.
        let manager_tab = manager_on || self.manager.stage == crate::manager_view::Stage::Memory;
        let (unread, _) = if tab == TodayTab::Manager { (0, 0) } else { self.chat_unread() };
        let tab_button = |id: &'static str, label: String, count: usize, hot: bool, shown: TodayTab, cx: &mut Context<Self>| {
            let selected = tab == shown;
            div()
                .id(id)
                .role(gpui::Role::Tab)
                .aria_label(if count > 0 { format!("{label}: {count}") } else { label.clone() })
                .aria_selected(selected)
                .flex()
                .flex_none()
                .items_center()
                .gap(theme::sz(6.))
                .h(theme::sz(32.))
                .px(theme::sz(10.))
                .rounded_lg()
                .cursor_pointer()
                .text_size(theme::sz(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .when(selected, |d| d.bg(theme::selected()).text_color(theme::text_strong()))
                .when(!selected, |d| d.text_color(theme::muted()).hover(|s| s.text_color(theme::text_strong())))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.today.tab = shown;
                    if shown == TodayTab::Manager {
                        this.manager.page.chat_scroll.scroll_to_bottom();
                    }
                    cx.notify();
                }))
                .child(label)
                .when(count > 0, |d| {
                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .min_w(theme::sz(17.))
                            .h(theme::sz(17.))
                            .px(theme::sz(4.))
                            .rounded_full()
                            .text_size(theme::sz(10.5))
                            .font_weight(FontWeight::BOLD)
                            .map(|d| if hot { d.bg(theme::attention()).text_color(theme::on_attention()) } else { d.bg(theme::hover()).text_color(theme::text_soft()) })
                            .child(count.to_string()),
                    )
                })
        };
        let tabs = div()
            .id("today-right-tabs")
            .role(gpui::Role::TabList)
            .aria_label("Today's panel")
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap(theme::sz(2.))
            .px(theme::sz(8.))
            .py_2()
            .border_b_1()
            .border_color(theme::border())
            .when(manager_tab, |d| d.child(tab_button("today-tab-manager", if manager_on { "Manager".to_string() } else { "Memory".to_string() }, unread, false, TodayTab::Manager, cx)))
            .child(tab_button("today-tab-inbox", "Inbox".into(), inbox_count, true, TodayTab::Inbox, cx))
            .child(tab_button("today-tab-activity", "Activity".into(), 0, false, TodayTab::Activity, cx));
        let panel = div().flex().flex_col().flex_none().w(px(self.right_width())).h_full().bg(theme::panel()).border_l_1().border_color(theme::border()).child(tabs);
        // The manager's tab fills the panel: its conversation scrolls, its box stays at the foot.
        if tab == TodayTab::Manager {
            return panel.child(self.render_assistant(true, window, cx)).into_any_element();
        }
        let content = match tab {
            TodayTab::Activity => self.today_agent_log(cx),
            _ => self.today_inbox(needs, cx),
        };
        panel.child(ui::scroll_area("today-right", &self.today.side_scroll, div().flex().flex_col().gap(theme::sz(22.)).p(theme::sz(14.)).child(content))).into_any_element()
    }

    fn panel_heading(title: &str, count: Option<usize>) -> gpui::Div {
        div()
            .flex()
            .items_baseline()
            .gap_2()
            .pb_2()
            .border_b_1()
            .border_color(theme::border())
            .child(div().font_weight(FontWeight::SEMIBOLD).text_size(theme::sz(14.)).text_color(theme::text_strong()).child(title.to_string()))
            .when_some(count, |d, n| d.child(div().text_size(theme::sz(13.)).text_color(theme::faint()).child(n.to_string())))
    }

    /// Inbox: the requests that wait for an OK, then the sessions that wait for an answer and
    /// the pull requests whose checks fail.
    fn today_inbox(&self, needs: Vec<NeedRow>, cx: &mut Context<Self>) -> AnyElement {
        let mut needs_rows: Vec<AnyElement> = Vec::new();
        for (i, need) in needs.into_iter().enumerate() {
            let open = need.open;
            needs_rows.push(
                div()
                    .id(SharedString::from(format!("today-need-{i}")))
                    .flex()
                    .items_start()
                    .gap(theme::sz(10.))
                    .px_1p5()
                    .py(theme::sz(9.))
                    .rounded(theme::sz(10.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.today_open(&open, window, cx)))
                    .child(div().flex_none().mt(theme::sz(6.)).size(theme::sz(7.)).rounded_full().bg(need.color))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(theme::sz(1.))
                            .child(div().text_size(theme::sz(13.5)).font_weight(FontWeight::MEDIUM).text_color(theme::text_strong()).child(need.title))
                            .child(div().text_size(theme::sz(12.5)).text_color(theme::muted()).child(need.what)),
                    )
                    .into_any_element(),
            );
        }
        let count = needs_rows.len();
        let approvals: Vec<AnyElement> = self.control.approvals.iter().map(|a| render_approval(a, false, self.control.expanded.contains(&a.id), cx)).collect();
        div()
            .flex()
            .flex_col()
            .gap(theme::sz(22.))
            .when(!approvals.is_empty(), |d| d.child(div().flex().flex_col().gap(theme::sz(12.)).child(Self::panel_heading("Asks for your OK", Some(approvals.len()))).children(approvals)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme::sz(2.))
                    .child(Self::panel_heading("Needs you", Some(count)))
                    .when(needs_rows.is_empty(), |d| d.child(div().py_3().px_1().text_size(theme::sz(13.)).text_color(theme::faint()).child(cards::NEEDS_EMPTY)))
                    .children(needs_rows),
            )
            .into_any_element()
    }

    /// Activity: what agents did today, one line each, with the session that did it.
    fn today_agent_log(&self, cx: &mut Context<Self>) -> AnyElement {
        let heading = Self::panel_heading("What agents did", None);
        if self.state.agents.log_off {
            return div()
                .flex()
                .flex_col()
                .gap(theme::sz(10.))
                .child(heading)
                .child(div().px_1().text_size(theme::sz(13.)).text_color(theme::muted()).child("This list is switched off in Settings › Automation (“Show what agents did on Today”)."))
                .child(
                    div().child(ui::button("today-log-settings", "Open Automation settings").on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.open_settings(crate::settings_ui::Section::Automation, window, cx);
                    }))),
                )
                .into_any_element();
        }
        let today = day::today();
        let log: Vec<AnyElement> = self
            .control
            .log
            .iter()
            .filter(|e| e.who != "You" && day::local(e.at as i64) == today)
            .take(8)
            .map(|e| {
                // "Added “…” to To do (id x8yk)." reads as "Claude added “…” to To do".
                let what = lower_first(&e.what);
                let what = match what.rfind(" (id ") {
                    Some(i) if what.ends_with(").") || what.ends_with(')') => what[..i].to_string(),
                    _ => what.trim_end_matches('.').to_string(),
                };
                div()
                    .flex()
                    .gap_3()
                    .py(theme::sz(8.))
                    .text_size(theme::sz(12.5))
                    .child(div().flex_none().w(theme::sz(38.)).text_color(theme::faint()).child(crate::commands::clock_of(e.at)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(theme::sz(1.))
                            .child(div().text_color(theme::text_soft()).child(format!("{} {what}", e.who)))
                            .when_some(e.session_title.clone(), |d, t| d.child(div().text_color(theme::faint()).overflow_hidden().whitespace_nowrap().text_ellipsis().child(t))),
                    )
                    .into_any_element()
            })
            .collect();
        let effects = self.today_effects();
        let intake = self.today_intake(cx);
        let journal = self.today_journal(cx);
        div()
            .flex()
            .flex_col()
            .child(heading)
            .when(log.is_empty(), |d| d.child(div().py_3().px_1().text_size(theme::sz(13.)).text_color(theme::faint()).child("Nothing yet today. Agents use the glasscode command; what they do shows here.")))
            .children(log)
            .when_some(intake, |d, intake| d.child(intake))
            .when_some(journal, |d, journal| d.child(journal))
            .when_some(effects, |d, effects| d.child(effects))
            .into_any_element()
    }

    /// v2 (M30): today's lines of the journal: a line per turn of each session (its report's), and
    /// what happened to the work (pull requests, CI, reviews, its state).
    fn today_journal(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let today = day::today();
        let all = self.today.journal_all;
        let source = if all { &self.today.journal_every } else { &self.today.journal_lines };
        let lines: Vec<&glasscode_core::journal::ActivityLine> = source.iter().filter(|line| day::local((line.at_ms / 1000) as i64) == today).collect();
        let today_in = |list: &[glasscode_core::journal::ActivityLine]| list.iter().any(|line| day::local((line.at_ms / 1000) as i64) == today);
        if !(today_in(&self.today.journal_lines) || today_in(&self.today.journal_every)) {
            return None;
        }
        let turns = lines.iter().filter(|line| line.turn).count();
        let rows: Vec<AnyElement> = lines
            .iter()
            .take(JOURNAL_SHOWN)
            .map(|line| {
                let title = line.session.as_deref().and_then(|id| self.state.session(id)).map(|s| self.session_title(s));
                div()
                    .flex()
                    .gap_3()
                    .py(theme::sz(6.))
                    .text_size(theme::sz(12.5))
                    .child(div().flex_none().w(theme::sz(38.)).text_color(theme::faint()).child(crate::commands::clock_of(line.at_ms / 1000)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(theme::sz(1.))
                            .child(div().text_color(if line.turn { theme::text_soft() } else { theme::muted() }).child(line.text.clone()))
                            .when_some(title, |d, t| d.child(div().text_color(theme::faint()).overflow_hidden().whitespace_nowrap().text_ellipsis().child(t))),
                    )
                    .into_any_element()
            })
            .collect();
        let heading = match (all, turns) {
            (true, _) => "Every event today".to_string(),
            (false, 1) => "Turns and work today: 1 turn".to_string(),
            (false, turns) => format!("Turns and work today: {turns} turns"),
        };
        // M14: the same events as `glasscode events`, one click away.
        let toggle = div()
            .id("today-journal-all")
            .cursor_pointer()
            .px_1()
            .text_size(theme::sz(12.))
            .text_color(theme::accent())
            .child(if all { "Turns and work only" } else { "Every event" })
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.today.journal_all = !this.today.journal_all;
                cx.notify();
            }));
        Some(div().flex().flex_col().pt(theme::sz(12.)).child(Self::panel_heading(&heading, Some(lines.len()))).child(toggle).children(rows).into_any_element())
    }

    /// v2 (M60): work intake found and left for the person: each item, why intake did not take
    /// it, and Start.
    fn today_intake(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.intake.suggestions.values().all(Vec::is_empty) {
            return None;
        }
        let items: Vec<(String, crate::intake_ui::Suggestion)> = self.intake.suggestions.iter().flat_map(|(project, list)| list.iter().map(move |s| (project.clone(), s.clone()))).collect();
        let rows: Vec<AnyElement> = items
            .iter()
            .take(8)
            .enumerate()
            .map(|(n, (project, s))| {
                let name = self.state.project(project).map(|p| p.name.clone()).unwrap_or_default();
                let (project, id) = (project.clone(), s.found.id());
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .py(theme::sz(6.))
                    .text_size(theme::sz(12.5))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(theme::sz(1.))
                            .child(div().text_color(theme::text_soft()).overflow_hidden().whitespace_nowrap().text_ellipsis().child(format!("{} {} · {name}", s.found.id(), s.found.title.trim())))
                            .child(div().text_color(theme::faint()).overflow_hidden().whitespace_nowrap().text_ellipsis().child(format!("Not taken: {}", s.why))),
                    )
                    .child(ui::button(SharedString::from(format!("today-intake-{n}")), "Start").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.intake_start_suggestion(&project, &id, window, cx))))
                    .into_any_element()
            })
            .collect();
        Some(div().flex().flex_col().pt(theme::sz(12.)).child(Self::panel_heading("Work intake found", Some(items.len()))).children(rows).into_any_element())
    }

    /// v2 (M31): the autonomous actions of the last 30 days, from the effect ledger: when, who set
    /// it off and why, what it was, and what came of it (land-21).
    fn today_effects(&self) -> Option<AnyElement> {
        if self.today.effects.is_empty() {
            return None;
        }
        use glasscode_core::effects::EffectState;
        let rows: Vec<AnyElement> = self
            .today
            .effects
            .iter()
            .take(12)
            .map(|record| {
                let (result, tone) = match &record.state {
                    EffectState::Claimed => ("running".to_string(), theme::muted()),
                    EffectState::Done(_) => ("done".to_string(), theme::faint()),
                    EffectState::Failed(why) => (format!("failed: {}", why.chars().take(60).collect::<String>()), theme::muted()),
                    EffectState::Uncertain => ("uncertain: look whether it happened".to_string(), theme::text_soft()),
                };
                let at = record.claimed_ms / 1000;
                let when = if day::local(at as i64) == day::today() { crate::commands::clock_of(at) } else { day::relative(day::local(at as i64), day::today()) };
                let who = if record.caller.is_empty() { record.origin.clone() } else { record.caller.clone() };
                div()
                    .flex()
                    .gap_3()
                    .py(theme::sz(6.))
                    .text_size(theme::sz(12.5))
                    .child(div().flex_none().w(theme::sz(64.)).text_color(theme::faint()).overflow_hidden().whitespace_nowrap().text_ellipsis().child(when))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(theme::sz(1.))
                            .child(div().text_color(theme::text_soft()).child(format!("{who}: {}", lower_first(&record.reason))))
                            .child(div().text_color(tone).overflow_hidden().whitespace_nowrap().text_ellipsis().child(format!("{} · {result}", record.op))),
                    )
                    .into_any_element()
            })
            .collect();
        Some(div().flex().flex_col().pt(theme::sz(12.)).child(Self::panel_heading("Autonomous actions, 30 days", Some(self.today.effects.len()))).children(rows).into_any_element())
    }
}

/// A command for an approval card: at most a few lines, the rest trimmed.
/// Whether card `id` is a pending action (v2), answered through the `approvals.decide` op.
fn pending_card(ws: &Workspace, id: u64) -> bool {
    ws.control.approvals.iter().any(|a| a.id == id && a.pending.is_some())
}

/// "Waits 23 h more", "Waits 12 min more": how long a pending action still waits.
fn waits_for(expires_ms: u64) -> String {
    let left = expires_ms.saturating_sub(glasscode_core::clock::now_ms()) / 60_000;
    match left {
        0 => "It is about to expire.".to_string(),
        m if m < 120 => format!("Waits {m} min more."),
        m => format!("Waits {} h more.", m / 60),
    }
}

fn clip_command(command: &str) -> String {
    let short: String = command.chars().take(220).collect();
    let short = short.lines().take(4).collect::<Vec<_>>().join("\n");
    if short.len() < command.len() { format!("{short} …") } else { short }
}

fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn upper_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// A small round avatar with the person's initial.
fn avatar(name: &str) -> AnyElement {
    let initial: String = name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "?".into());
    let hue = name.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b))) % 360;
    div()
        .flex_none()
        .size(theme::sz(16.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::hsla(hue as f32 / 360., 0.42, if theme::is_light() { 0.46 } else { 0.52 }, 1.))
        .text_color(gpui::white())
        .text_size(theme::sz(8.5))
        .font_weight(FontWeight::BOLD)
        .child(initial)
        .into_any_element()
}

/// An agent's request waiting for the person: what it wants to do, the command, Allow / Deny.
/// `compact` for the Chat view.
pub fn render_approval(a: &Approval, compact: bool, open: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let id = a.id;
    // With the v2 engine (M21.2): who asks and the rule that asks, the expiry, and the full
    // payload, which opens and is never clipped; the answer is the `approvals.decide` op.
    let pending = a.pending.as_ref();
    let decide = move |allow: bool| {
        move |this: &mut Workspace, _: &ClickEvent, window: &mut Window, cx: &mut Context<Workspace>| {
            if pending_card(this, id) {
                this.run_op_named("approvals.decide", serde_json::json!({ "id": id.to_string(), "answer": if allow { "allow" } else { "deny" } }), window, cx);
            } else {
                this.control_decide(id, allow, false, window, cx);
            }
        }
    };
    let from = match (&a.caller.session_title, compact) {
        (Some(t), false) => format!("From “{t}”."),
        _ => String::new(),
    };
    div()
        .flex()
        .flex_col()
        .gap(theme::sz(12.))
        .p(theme::sz(16.))
        .rounded(theme::sz(14.))
        .border_1()
        .border_color(theme::border())
        .bg(theme::raised())
        .shadow_md()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_none().size(theme::sz(22.)).rounded_full().bg(theme::warn_bg()).flex().items_center().justify_center().text_color(crate::sidebar::attention()).text_size(theme::sz(12.)).font_weight(FontWeight::BOLD).child("?"))
                .child(div().flex_1().min_w_0().font_weight(FontWeight::SEMIBOLD).text_size(theme::sz(13.5)).text_color(theme::text_strong()).child(format!("{} to {}", glasscode_core::caller::who_does(&a.caller.who, "ask"), a.what))),
        )
        .when(!from.is_empty(), |d| d.child(div().text_size(theme::sz(13.)).text_color(theme::text_soft()).child(from)))
        .when_some(a.why.clone().filter(|_| pending.is_some()), |d, why| d.child(div().text_size(theme::sz(12.5)).text_color(theme::muted()).child(format!("Why it asks: {why}."))))
        .when_some(pending.map(|p| waits_for(p.expires_ms)), |d, expiry| d.child(div().text_size(theme::sz(12.5)).text_color(theme::muted()).child(expiry)))
        .when(pending.is_none(), |d| d.child(div().px(theme::sz(10.)).py(theme::sz(8.)).rounded(theme::sz(8.)).bg(theme::hover()).font_family(theme::MONO_FONT).text_size(theme::sz(12.)).text_color(theme::text_soft()).max_h(theme::sz(72.)).overflow_hidden().child(clip_command(&a.command))))
        .when_some(pending, |d, p| {
            let payload = p.payload();
            let lines = payload.lines().count();
            let shown = if open { payload.clone() } else { payload.lines().take(4).collect::<Vec<_>>().join("\n") };
            d.child(
                div()
                    .px(theme::sz(10.))
                    .py(theme::sz(8.))
                    .rounded(theme::sz(8.))
                    .bg(theme::hover())
                    .font_family(theme::MONO_FONT)
                    .text_size(theme::sz(12.))
                    .text_color(theme::text_soft())
                    .child(format!("{}\n{shown}", p.op_name())),
            )
            .when(lines > 4, |d| {
                d.child(
                    div()
                        .id(SharedString::from(format!("approval-payload-{id}-{compact}")))
                        .text_size(theme::sz(12.5))
                        .text_color(theme::muted())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::text()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if !this.control.expanded.remove(&id) {
                                this.control.expanded.insert(id);
                            }
                            cx.notify();
                        }))
                        .child(if open { "Show less".to_string() } else { format!("Show all {lines} lines") }),
                )
            })
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap(theme::sz(6.))
                .child(
                    div()
                        .id(SharedString::from(format!("approve-{id}-{compact}")))
                        .flex_1()
                        .h(theme::sz(32.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(theme::sz(8.))
                        .bg(theme::text_strong())
                        .text_color(theme::bg())
                        .text_size(theme::sz(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .hover(|s| s.opacity(0.88))
                        .on_click(cx.listener(decide(true)))
                        .child(if pending.is_some() { "Approve" } else { "Allow" }),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("deny-{id}-{compact}")))
                        .flex_1()
                        .h(theme::sz(32.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(theme::sz(8.))
                        .border_1()
                        .border_color(theme::border_strong())
                        .bg(theme::raised())
                        .text_size(theme::sz(13.))
                        .text_color(theme::text())
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(decide(false)))
                        .child("Deny"),
                ),
        )
        .when_some(a.always_in.clone(), |d, project| {
            d.child(
                div()
                    .id(SharedString::from(format!("approve-always-{id}-{compact}")))
                    .self_center()
                    .text_size(theme::sz(12.5))
                    .text_color(theme::muted())
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme::text()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.control_offer_always(id, cx)))
                    .child(format!("Always allow this in {project}")),
            )
        })
        .when(a.caller.session.is_some() && a.allow_here, |d| {
            d.child(
                div()
                    .id(SharedString::from(format!("approve-session-{id}-{compact}")))
                    .self_center()
                    .text_size(theme::sz(12.5))
                    .text_color(theme::muted())
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme::text()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.control_decide(id, true, true, window, cx)))
                    .child("Allow this for the rest of the session"),
            )
        })
        .into_any_element()
}

/// Made-up rows for the cards, for screen checks of a full Today in a debug build
/// (`GLASSCODE_DEBUG_TODAY=sample`): no name, repository or number here is a real one.
mod sample {
    use glasscode_core::activity::Activity;
    use glasscode_core::day::{self, Day};
    use glasscode_core::github::{CheckRun, CiStatus, MyPr};
    use glasscode_core::model::AgentKind;
    use glasscode_core::usage::Limit;

    use crate::today_cards::{Need, Open, Upcoming, UsageAccount};

    pub fn needs() -> Vec<Need> {
        let need = |title: &str, what: &str, agent: AgentKind, failing: bool| Need { title: title.into(), what: what.into(), agent: Some(agent), failing, open: Open::Nothing };
        vec![
            need("Speed up the build cache", "Claude is waiting for your answer", AgentKind::Claude, false),
            need("Port the settings page · #48", "CI failing (2 checks) · the agent is asked to fix it", AgentKind::Codex, true),
            need("Rename the config keys", "Codex is waiting for your answer", AgentKind::Codex, false),
            need("Write the release notes", "Claude is waiting for your answer", AgentKind::Claude, false),
            need("Trim the log output · #51", "CI failing (1 check)", AgentKind::Claude, true),
            need("Check the migration script", "Claude is waiting for your answer", AgentKind::Claude, false),
            need("Profile the start time", "Codex is waiting for your answer", AgentKind::Codex, false),
        ]
    }

    pub fn coming_up() -> Vec<Upcoming> {
        let item = |n: usize, when: &str, title: &str, detail: &str| Upcoming { key: format!("sample:{n}"), when: when.into(), title: title.into(), detail: detail.into(), url: None };
        vec![
            item(0, "Tomorrow", "ENG-7 The import of large files", "Linear · In Progress"),
            item(1, "Tomorrow", "Answer the question about the API token", "Your list · Sam asked"),
            item(2, "Mon", "Write the notes for the release", "Your list"),
            item(3, "Tue", "ENG-12 A page for the settings of a project", "Linear · Todo"),
            item(4, "Wed", "Look at the numbers of the new cache", "Your list · waiting on Kim"),
            item(5, "Thu", "ENG-15 The search loses its place", "Linear · Todo"),
            item(6, "Review", "#12 Speed up the build", "GitHub · acme/app · by octocat"),
            item(7, "Review", "#31 A shorter log", "GitHub · acme/tools · by hubot"),
        ]
    }

    /// Three accounts: one near its limit, one in the middle, one hardly used.
    pub fn usage(now: i64) -> Vec<UsageAccount> {
        let account = |agent: AgentKind, label: &str, limits: Vec<Limit>| UsageAccount { agent, label: label.into(), limits };
        let limit = |window_minutes: u64, used_percent: f64, hours: i64| Limit { window_minutes, used_percent, resets_at: Some(now + hours * 3_600) };
        vec![
            account(AgentKind::Claude, "My login", vec![limit(10080, 86., 75), limit(300, 31., 2)]),
            account(AgentKind::Codex, "Work", vec![limit(300, 47., 3), limit(10080, 22., 100)]),
            account(AgentKind::Claude, "Spare", vec![limit(10080, 9., 150)]),
            account(AgentKind::Codex, "Team", vec![limit(10080, 64., 30)]),
        ]
    }

    pub fn pull_requests() -> Vec<MyPr> {
        let check = |conclusion: &str| CheckRun { name: "test".into(), status: if conclusion.is_empty() { "IN_PROGRESS".into() } else { "COMPLETED".into() }, conclusion: conclusion.into(), ..Default::default() };
        let ci = |checks: Vec<CheckRun>| Some(CiStatus { state: "OPEN".into(), checks, ..Default::default() });
        let titles = ["Speed up the build cache", "A page for the settings of a project", "The import of large files", "A shorter log", "The search keeps its place", "Rename the config keys", "Trim the examples", "A test for the migration", "The formatter of the new release", "Notes for the release"];
        titles
            .iter()
            .enumerate()
            .map(|(n, title)| MyPr {
                repo: if n % 3 == 2 { "acme/tools".into() } else { "acme/app".into() },
                number: 40 + n as u64,
                title: (*title).into(),
                // No address: a click in a check opens nothing.
                url: String::new(),
                draft: n == 3,
                ci: match n % 4 {
                    0 => ci(vec![check("SUCCESS"), check("FAILURE")]),
                    1 => ci(vec![check("SUCCESS")]),
                    2 => ci(vec![check("")]),
                    _ => None,
                },
                review: ["REVIEW_REQUIRED", "APPROVED", "", "CHANGES_REQUESTED"][n % 4].into(),
                ..Default::default()
            })
            .collect()
    }

    /// Some sessions on most working days of the last half year, and a merge now and then.
    pub fn activity(today: Day) -> Activity {
        let mut activity = Activity::default();
        for back in 0..150i64 {
            let date = day::iso(today - back);
            let weekend = day::weekday(today - back) >= 5;
            let sessions = (back * 7 + back / 3) % 6;
            let sessions = if weekend { sessions / 4 } else { sessions };
            for _ in 0..sessions {
                activity.add_session(&date);
            }
            if back % 4 == 1 && !weekend {
                activity.add_merge(&date, 120 + (back as u32 * 37) % 400, &format!("acme/app#{back}"));
            }
        }
        activity
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::point;

    /// A press on an open pill closes its list (the press is outside the list), and the click
    /// that the press began leaves it closed.
    #[test]
    fn a_click_on_the_open_pill_closes_its_list() {
        let (on_pill, elsewhere) = (point(px(40.), px(300.)), point(px(600.), px(80.)));
        // Closed: a click opens it.
        assert!(pill_click(None, None, StartMenu::Project, Some(on_pill)));
        // Open: the press closed it, and the click of that press keeps it closed.
        assert!(!pill_click(None, Some((StartMenu::Project, on_pill)), StartMenu::Project, Some(on_pill)));
        // A click with no press outside the list before it (a keyboard click) closes an open list.
        assert!(!pill_click(Some(StartMenu::Project), None, StartMenu::Project, None));
        // The press on another pill closed this list: that pill opens its own.
        assert!(pill_click(None, Some((StartMenu::Agent, on_pill)), StartMenu::Project, Some(on_pill)));
        // A list closed by a press elsewhere: a later click on its pill opens it again.
        assert!(pill_click(None, Some((StartMenu::Project, elsewhere)), StartMenu::Project, Some(on_pill)));
    }

    /// A scratch Glasscode folder and a test never read the real home's agent history; a made-up
    /// home is read in its place when one is named.
    #[test]
    fn the_agents_history_is_not_read_from_the_real_home_in_a_check() {
        let home = PathBuf::from("/home/sam");
        assert_eq!(history_home_of(None, false, home.clone()), Some(home.clone()));
        assert_eq!(history_home_of(None, true, home.clone()), None);
        assert_eq!(history_home_of(Some("/tmp/made-up".into()), true, home.clone()), Some(PathBuf::from("/tmp/made-up")));
        // The tests of this crate run with the tests' own Glasscode folder.
        glasscode_core::paths::use_test_root();
        assert_eq!(history_home(), std::env::var("GLASSCODE_DEBUG_DISCOVER_HOME").ok().filter(|v| !v.is_empty()).map(PathBuf::from));
    }

    /// The agent pill starts at the project's `default_agent`, and the person's own choice wins.
    #[test]
    fn the_start_box_takes_the_projects_default_agent() {
        let choice = |agent: AgentKind, account: &str| (agent, Some(account.to_string()), format!("{} · {account}", agent.name()));
        let choices = [choice(AgentKind::Claude, "c1"), choice(AgentKind::Claude, "c2"), choice(AgentKind::Codex, "x1"), choice(AgentKind::Codex, "x2")];
        let defaults = |agent: AgentKind| Some(if agent == AgentKind::Claude { "c2" } else { "x2" }.to_string());
        // No project default: Claude on its default account.
        assert_eq!(pick_start_agent(&choices, None, AgentKind::Claude, defaults), 1);
        // A Codex project: Codex on its default account, or its first one with no default.
        assert_eq!(pick_start_agent(&choices, None, AgentKind::Codex, defaults), 3);
        assert_eq!(pick_start_agent(&choices, None, AgentKind::Codex, |_| None), 2);
        // The person's choice wins over the project's.
        let mine = (AgentKind::Claude, Some("c1".to_string()));
        assert_eq!(pick_start_agent(&choices, Some(&mine), AgentKind::Codex, defaults), 0);
        // A shell project: the box starts no shell, so Claude.
        assert_eq!(pick_start_agent(&choices, None, AgentKind::Shell, defaults), 1);
        // No Codex account at all: Claude's default.
        assert_eq!(pick_start_agent(&choices[..2], None, AgentKind::Codex, defaults), 1);
        // Only Claude on its own login (no account).
        let login = [(AgentKind::Claude, None, "Claude".to_string())];
        assert_eq!(pick_start_agent(&login, None, AgentKind::Codex, |_| None), 0);
    }
}

/// The home whose `.claude` and `.codex` folders hold the person's own agent history, for the
/// heat map. None in a test and under a scratch `GLASSCODE_HOME`: a check of the app, or a test,
/// never reads the real home. Debug builds: `GLASSCODE_DEBUG_DISCOVER_HOME` names a made-up
/// home, as for the first run.
fn history_home() -> Option<PathBuf> {
    let made_up = std::env::var("GLASSCODE_DEBUG_DISCOVER_HOME").ok().filter(|v| cfg!(debug_assertions) && !v.is_empty());
    let scratch = std::env::var("GLASSCODE_HOME").is_ok_and(|v| !v.is_empty());
    history_home_of(made_up, scratch || glasscode_core::paths::testing(), glasscode_core::paths::home())
}

fn history_home_of(made_up: Option<String>, scratch: bool, home: PathBuf) -> Option<PathBuf> {
    match made_up {
        Some(folder) => Some(PathBuf::from(folder)),
        None if scratch => None,
        None => Some(home),
    }
}

/// The start box's label for screen readers: what typing there does.
fn placeholder_label(asking: bool, name: &str) -> String {
    if asking { format!("Ask {name}") } else { "Describe a task to start a session on it".into() }
}
