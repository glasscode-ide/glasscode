//! Today: the person's day on one screen (slice U11). On the moving backdrop: the start box (a
//! new session from a few words), the cards (`today_cards.rs`), and "Your list", the board of
//! their own tasks and the ones agents add through `glasscode board`, recurring ones, Linear
//! tickets and review requests. The right panel has Inbox (what agents ask to do, and what needs
//! the person now) and Activity (what agents did). The board itself is `glasscode_core::board`;
//! this file has the state, what the buttons do, and the page.
//!
//! Today draws ten times a second while the backdrop moves, so it reads no file and runs no
//! program while it draws: the pull requests (`gh`) and the activity file are read in the
//! background, when Today opens and every few minutes while it shows, and kept here.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{AnyElement, AppContext as _, ClickEvent, Context, Entity, FontWeight, Hsla, Pixels, Point, ScrollHandle, SharedString, Window, anchored, deferred, div, prelude::*, px};
use glasscode_core::activity::Activity;

/// The key of a Coming up line made from the work ledger: `work:<session>|<work id>`.
const WORK_KEY: &str = "work:";
use glasscode_core::board::{self, Board, Item, Outside, Section, TodayView};
use glasscode_core::control::Reply;
use glasscode_core::day;
use glasscode_core::github::MyPr;
use glasscode_core::model::AgentKind;

use crate::backdrop;
use crate::composer::{Composer, ComposerEvent};
use crate::control_ui::{Approval, Responder};
use crate::icons;
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

/// The right panel's tabs on Today.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TodayTab {
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
    /// `~/.glasscode/activity.json` as last read.
    pub activity: Option<Activity>,
    pub activity_at: Option<Instant>,
    /// v2 (M31): the autonomous actions of the last 30 days, from the effect ledger, newest first.
    pub effects: Vec<glasscode_core::effects::EffectRecord>,
    pub scroll: ScrollHandle,
    pub side_scroll: ScrollHandle,
    /// Debug builds: `GLASSCODE_DEBUG_TODAY` was read.
    debug_done: bool,
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
            tab: TodayTab::Inbox,
            prs: None,
            prs_at: None,
            prs_loading: false,
            activity: None,
            activity_at: None,
            effects: Vec::new(),
            scroll: ScrollHandle::new(),
            side_scroll: ScrollHandle::new(),
            debug_done: false,
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
        let v2 = self.control.engine_v2;
        let task = cx.background_spawn(async move {
            let effects = if v2 {
                glasscode_core::effects::recent(glasscode_core::effects::ACTIVITY_DAYS).unwrap_or_else(|err| {
                    log::warn!("effects: the ledger could not be read ({err})");
                    Vec::new()
                })
            } else {
                Vec::new()
            };
            (Activity::load(), effects)
        });
        cx.spawn(async move |this, cx| {
            let (activity, effects) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.today.activity = Some(activity);
                this.today.effects = effects;
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
        // Debug builds: GLASSCODE_DEBUG_TODAY=still|inbox|activity (comma-separated). `still`
        // asks for less motion, as the system's setting does: the backdrop stands still on its
        // frame, the baseline of a measurement of its cost.
        if cfg!(debug_assertions) && !self.today.debug_done {
            self.today.debug_done = true;
            if let Ok(how) = std::env::var("GLASSCODE_DEBUG_TODAY") {
                for part in how.split(',') {
                    match part {
                        "still" => cx.set_reduce_motion(true),
                        "inbox" => self.today.tab = TodayTab::Inbox,
                        "activity" => self.today.tab = TodayTab::Activity,
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
        out
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
        let known = if self.control.engine_v2 {
            self.control.authority.devices.contains(&glasscode_core::gate::device_id(key))
        } else {
            self.phones.as_ref().is_some_and(|phones| phones.iter().any(|p| p.key.split_whitespace().take(2).eq(key.split_whitespace().take(2))))
        };
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

    /// Send: a session with what is typed as its first message, in the project and from the
    /// start point of the pills, as a task's Start does (`start_session`); it opens when it is
    /// made. The text stays until it worked.
    /// Enter in the start box: what the Session | Ask control says (M52.1).
    pub fn today_enter(&mut self, now: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat_enter().mode != glasscode_core::runtime::chat::StartMode::Ask {
            return self.today_send(window, cx);
        }
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
        if now && self.mchat.turn.is_some() {
            self.chat_stop(cx);
        }
        self.manager_ask_as(message, false, Some(words), None, None, cx);
        cx.notify();
    }

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

    fn today_start_box(&mut self, view: &TodayView, waiting: usize, cx: &mut Context<Self>) -> AnyElement {
        let today = day::today();
        let name = self.tickets.snapshot.as_ref().and_then(|s| s.viewer.name.split_whitespace().next().map(crate::control_ui::capitalize));
        let greeting = match day::hour_now() {
            4..=11 => "Good morning",
            12..=17 => "Good afternoon",
            _ => "Good evening",
        };
        let header = div()
            .flex()
            .flex_col()
            .gap(theme::sz(3.))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_baseline()
                    .gap_x(theme::sz(12.))
                    .child(div().flex_1().text_size(theme::sz(19.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(match name {
                        Some(n) => format!("{greeting}, {n}"),
                        None => greeting.to_string(),
                    }))
                    .child(div().flex_none().text_size(theme::sz(12.5)).text_color(theme::card_soft_text()).child(day::long(today))),
            )
            .child(div().text_size(theme::sz(13.5)).text_color(theme::text_soft()).child(view.summary_with(waiting, self.control.approvals.len())));
        let frame = backdrop::glass_strong(theme::sz(16.)).flex().flex_col().gap(theme::sz(14.)).px(theme::sz(18.)).pt(theme::sz(16.)).pb(theme::sz(14.)).when(theme::is_light(), |d| d.shadow_md()).child(header);

        // No project: what to do first.
        let Some((project_id, project_name)) = self.start_project() else {
            return frame
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(theme::sz(12.))
                        .child(div().flex_1().min_w(theme::sz(200.)).text_size(theme::sz(13.5)).text_color(theme::text()).child("Add a project to start a session here: each session works in one of your folders."))
                        .child(ui::primary_button("today-add-project", "Add project…").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.prompt_add_project(window, cx)))),
                )
                .into_any_element();
        };
        let agents = self.start_agents();
        let (agent, _, agent_label) = self.start_agent();
        let projects = self.state.visible_projects().count();
        let froms = self.start_froms(&project_id);
        let (_, from_label) = self.start_from(&project_id);
        let typed = !self.today.start.read(cx).text().trim().is_empty();
        let can_send = typed && !self.today.sending;
        // With the manager on: Session | Ask, and the placeholder for what Enter does (ux-4).
        let enter = self.chat_enter();
        let asking = enter.mode == glasscode_core::runtime::chat::StartMode::Ask;
        let manager_name = self.state.manager.name.clone();
        let placeholder = glasscode_core::runtime::chat::placeholder(enter.mode, &manager_name);
        if self.mchat.placeholder.as_deref() != Some(placeholder.as_str()) {
            self.mchat.placeholder = Some(placeholder.clone());
            self.today.start.update(cx, |c, _| c.set_placeholder(placeholder));
        }
        let mode_switch = enter.choice.then(|| self.today_mode_switch(&manager_name, cx));

        let pill = |id: &'static str, icon: AnyElement, label: String, which: Option<StartMenu>, cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .flex_none()
                .max_w(theme::sz(240.))
                .items_center()
                .gap(theme::sz(6.))
                .px(theme::sz(9.))
                .py(theme::sz(4.))
                .rounded(theme::sz(7.))
                .border_1()
                .border_color(theme::border())
                .text_size(theme::sz(12.))
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
        // The pills wrap in a narrow centre; Send stays at the right end.
        let pills = div()
            .flex()
            .items_end()
            .gap(theme::sz(8.))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .flex_wrap()
                    .items_center()
                    .gap(theme::sz(6.))
                    .children(mode_switch)
                    .child(pill("today-start-agent", icons::brand(icons::agent_brand(agent), 14.), agent_label, (agents.len() > 1).then_some(StartMenu::Agent), cx))
                    .child(pill("today-start-project", icons::icon(icons::FOLDER, theme::sz(14.), theme::sz(14.), theme::card_soft_text()).into_any_element(), project_name, (projects > 1).then_some(StartMenu::Project), cx))
                    .child(pill("today-start-from", icons::icon(icons::BRANCH, theme::sz(14.), theme::sz(14.), theme::card_soft_text()).into_any_element(), from_label, (froms.len() > 1).then_some(StartMenu::From), cx)),
            )
            .when(crate::glasscode_ui::VOICE, |d| d.child(cards::mic_button().on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.voice_open(window, cx)))))
            .child(
                div()
                    .id("today-send")
                    .role(gpui::Role::Button)
                    .aria_label(if asking { format!("Ask {manager_name}") } else { "Start a session".to_string() })
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(theme::sz(30.))
                    .rounded_full()
                    .when(can_send, |d| {
                        d.bg(theme::text()).cursor_pointer().hover(|s| s.opacity(0.88)).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.today_enter(false, window, cx)))
                    })
                    .when(!can_send, |d| d.bg(theme::tint()))
                    .child(icons::icon(icons::SEND, theme::sz(15.), theme::sz(15.), if can_send { theme::bg() } else { theme::card_soft_text() })),
            );
        let why_not = enter.why_not.clone();
        // The answer to what was asked here, in the conversation's own component (ux-3).
        let thread = (self.mchat.today_asked && enter.choice).then(|| {
            div()
                .flex()
                .flex_col()
                .gap(theme::sz(8.))
                .pt(theme::sz(10.))
                .border_t_1()
                .border_color(theme::border())
                .child(self.render_chat_thread(crate::manager_chat::ThreadView { id: "today-thread", limit: 2, compact: true }, cx))
                .child(
                    div()
                        .flex()
                        .gap(theme::sz(12.))
                        .text_size(theme::sz(12.))
                        .child(
                            div()
                                .id("today-thread-open")
                                .role(gpui::Role::Link)
                                .aria_label("Open the full conversation")
                                .text_color(theme::text_soft())
                                .underline()
                                .cursor_pointer()
                                .hover(|s| s.text_color(theme::text()))
                                .child("Open the full conversation")
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.show_manager(window, cx))),
                        )
                        .child(
                            div()
                                .id("today-thread-hide")
                                .role(gpui::Role::Button)
                                .aria_label("Hide the answer")
                                .text_color(theme::muted())
                                .cursor_pointer()
                                .hover(|s| s.text_color(theme::text()))
                                .child("Hide")
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.mchat.today_asked = false;
                                    cx.notify();
                                })),
                        ),
                )
        });
        frame
            .child(div().id("today-start-input").role(gpui::Role::TextInput).aria_label(placeholder_label(asking, &manager_name)).text_size(theme::sz(15.)).min_h(theme::sz(40.)).child(self.today.start.clone()))
            .child(pills)
            .when_some(why_not, |d, why| d.child(div().text_size(theme::sz(12.)).text_color(theme::muted()).child(format!("Enter starts a session: {why}."))))
            .children(thread)
            .into_any_element()
    }

    /// The start box's Session | Ask control: what Enter does, remembered (ux-4).
    fn today_mode_switch(&self, name: &str, cx: &mut Context<Self>) -> AnyElement {
        use glasscode_core::runtime::chat::StartMode;
        let chosen = self.mchat.today_mode;
        let option = |mode: StartMode, label: String, cx: &mut Context<Self>| {
            let on = chosen == mode;
            div()
                .id(if mode == StartMode::Session { "today-mode-session" } else { "today-mode-ask" })
                .role(gpui::Role::RadioButton)
                .aria_label(label.clone())
                .aria_toggled(if on { gpui::Toggled::True } else { gpui::Toggled::False })
                .px(theme::sz(8.))
                .py(theme::sz(3.))
                .rounded(theme::sz(6.))
                .text_size(theme::sz(12.))
                .when(on, |d| d.bg(theme::tint()).text_color(theme::text()).font_weight(FontWeight::MEDIUM))
                .when(!on, |d| d.text_color(theme::text_soft()).cursor_pointer().hover(|s| s.text_color(theme::text())))
                .child(label)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.chat_set_today_mode(mode, cx)))
        };
        div()
            .id("today-mode")
            .role(gpui::Role::RadioGroup)
            .aria_label("What Enter does")
            .flex()
            .flex_none()
            .items_center()
            .p(theme::sz(2.))
            .rounded(theme::sz(8.))
            .border_1()
            .border_color(theme::border())
            .child(option(StartMode::Session, "Session".into(), cx))
            .child(option(StartMode::Ask, format!("Ask {name}"), cx))
            .into_any_element()
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
        let start = part(Part::Start, || self.today_start_box(&view, waiting_sessions, cx));
        let grid = part(Part::Cards, || self.today_grid(&view, columns, cx));
        let list = part(Part::List, || self.today_list(&view, cx));
        let menu = self.today.menu.map(|(which, at)| self.today_start_menu(which, at, cx));
        let content = div().w_full().flex().justify_center().child(
            div().w_full().max_w(theme::sz(1000.)).flex().flex_col().gap(theme::sz(14.)).px(theme::sz(22.)).pt(theme::sz(28.)).pb(theme::sz(40.)).child(start).child(grid).child(list),
        );
        let layer = self.backdrop_layer(0.85, window, cx);
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
                let session = self.state.session(next.session.as_deref()?).filter(|s| !self.state.session_hidden(s))?;
                let what = match &next.pr {
                    Some((repo, number)) => format!("{} · {repo}#{number}", next.what),
                    None => next.what.clone(),
                };
                Some(cards::Upcoming { key: format!("{WORK_KEY}{}|{}", session.id, next.key.id), when: next.when.to_string(), title: self.session_title(session), detail: upper_first(&what), url: None })
            })
            .collect()
    }

    /// The cards, in a grid of three, two or one columns.
    fn today_grid(&self, view: &TodayView, columns: usize, cx: &mut Context<Self>) -> AnyElement {
        let today = day::today();
        let today_iso = day::iso(today);
        let needs: Vec<cards::Need> = self.today_needs().into_iter().map(|n| cards::Need { title: n.title, what: n.what, agent: n.agent, failing: n.failing, open: n.open }).collect();
        let needs = cards::needs(&needs, self.control.approvals.len());
        let items: Vec<Item> = view.items().cloned().collect();
        let mut upcoming = cards::coming_up(&items, today);
        upcoming.extend(self.today_coming_work());
        let coming = cards::coming_up_list(&upcoming, |u| match &u.url {
            Some(url) => Open::Url(url.clone()),
            // Work of the ledger: its session.
            None if u.key.starts_with(WORK_KEY) => u.key.strip_prefix(WORK_KEY).and_then(|rest| rest.split_once('|')).map(|(id, _)| id.to_string()).filter(|id| self.state.session(id).is_some()).map_or(Open::Nothing, Open::Session),
            // A task with a session: that session.
            None => u
                .key
                .strip_prefix("task:")
                .and_then(|id| self.today.board.task(id))
                .and_then(|t| t.session.clone())
                .filter(|id| self.state.session(id).is_some())
                .map_or(Open::Nothing, Open::Session),
        });
        let accounts: Option<Vec<cards::UsageAccount>> = self.usage.report.as_ref().map(|report| {
            report
                .accounts
                .iter()
                .filter_map(|a| Some(cards::UsageAccount { agent: a.agent?, label: self.state.account(&a.id).map_or_else(|| a.id.clone(), |x| x.label.clone()), limits: a.limits.clone() }))
                .collect()
        });
        let usage = cards::usage(accounts.as_deref(), glasscode_core::model::now() as i64);
        let heat = cards::heat(self.today.activity.as_ref(), &today_iso);
        let recent: Vec<cards::Recent> = self
            .state
            .visible_sessions()
            .map(|s| cards::Recent {
                id: s.id.clone(),
                title: self.session_title(s),
                project: self.state.project(&s.project_id).map(|p| p.name.clone()).unwrap_or_default(),
                agent: s.agent,
                state: self.row_state(s),
                created_at: s.created_at,
            })
            .collect();
        let recent = cards::recent(recent, glasscode_core::model::now());
        let prs = cards::pull_requests(self.today.prs.as_ref());
        let month = cards::month(self.today.activity.as_ref(), &today_iso);

        let span = |card: Card| cards::span(card, columns);
        let cells: Vec<AnyElement> = vec![
            cards::card(icons::BELL, Tone::Attention, "Needs you", needs.count.clone()).children(cards::list("today-needs", &needs, cx)).col_span(span(Card::Needs)).into_any_element(),
            cards::card(icons::CALENDAR, Tone::Soft, "Coming up", None).children(cards::list("today-coming", &coming, cx)).col_span(span(Card::ComingUp)).into_any_element(),
            cards::card(icons::GAUGE, Tone::Soft, "Usage", None).child(cards::meters(&usage, cx)).col_span(span(Card::Usage)).into_any_element(),
            cards::card(icons::GRID, Tone::Soft, "Activity", None).child(cards::heat_map(&heat)).col_span(span(Card::Activity)).into_any_element(),
            cards::card(icons::SESSIONS, Tone::Soft, "Recent sessions", None).children(cards::list("today-recent", &recent, cx)).col_span(span(Card::Recent)).into_any_element(),
            cards::card(icons::PULL_REQUEST, Tone::Soft, "Pull requests", prs.count.clone()).children(cards::list("today-prs", &prs, cx)).col_span(span(Card::PullRequests)).into_any_element(),
            cards::card(icons::CHART, Tone::Soft, "This month", None).child(cards::month_stats(&month)).col_span(span(Card::Month)).into_any_element(),
        ];
        div().grid().grid_cols(cards::TRACKS).gap(theme::sz(14.)).children(cells).into_any_element()
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

    /// Today's right panel: Inbox (what agents ask to do, and what needs the person now) and
    /// Activity (what agents did today). It is opaque, as the session's is.
    pub fn render_today_right(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.today.tab;
        let needs = self.today_needs();
        let inbox_count = needs.len() + self.control.approvals.len();
        let tab_button = |id: &'static str, label: String, shown: TodayTab, cx: &mut Context<Self>| {
            let selected = tab == shown;
            div()
                .id(id)
                .flex()
                .flex_none()
                .items_center()
                .gap_1p5()
                .h(theme::sz(32.))
                .px_3()
                .rounded_lg()
                .cursor_pointer()
                .text_size(theme::sz(13.5))
                .font_weight(FontWeight::SEMIBOLD)
                .when(selected, |d| d.bg(theme::selected()).text_color(theme::text_strong()))
                .when(!selected, |d| d.text_color(theme::muted()).hover(|s| s.text_color(theme::text_strong())))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.today.tab = shown;
                    cx.notify();
                }))
                .child(label)
        };
        let inbox_label = if inbox_count == 0 { "Inbox".to_string() } else { format!("Inbox {inbox_count}") };
        let tabs = div()
            .id("today-right-tabs")
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .px_2p5()
            .py_2()
            .border_b_1()
            .border_color(theme::border())
            .child(tab_button("today-tab-inbox", inbox_label, TodayTab::Inbox, cx))
            .child(tab_button("today-tab-activity", "Activity".into(), TodayTab::Activity, cx));
        let content = match tab {
            TodayTab::Inbox => self.today_inbox(needs, cx),
            TodayTab::Activity => self.today_agent_log(cx),
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.right_width()))
            .h_full()
            .bg(theme::panel())
            .border_l_1()
            .border_color(theme::border())
            .child(tabs)
            .child(ui::scroll_area("today-right", &self.today.side_scroll, div().flex().flex_col().gap(theme::sz(22.)).p(theme::sz(14.)).child(content)))
            .into_any_element()
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
        div()
            .flex()
            .flex_col()
            .child(heading)
            .when(log.is_empty(), |d| d.child(div().py_3().px_1().text_size(theme::sz(13.)).text_color(theme::faint()).child("Nothing yet today. Agents use the glasscode command; what they do shows here.")))
            .children(log)
            .when_some(effects, |d, effects| d.child(effects))
            .into_any_element()
    }

    /// v2 (M31): the autonomous actions of the last 30 days, from the effect ledger: when, who set
    /// it off and why, what it was, and what came of it (land-21).
    fn today_effects(&self) -> Option<AnyElement> {
        if !self.control.engine_v2 || self.today.effects.is_empty() {
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
                .child(div().flex_1().min_w_0().font_weight(FontWeight::SEMIBOLD).text_size(theme::sz(13.5)).text_color(theme::text_strong()).child(format!("{} asks to {}", a.caller.who, a.what))),
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

/// The start box's label for screen readers: what typing there does.
fn placeholder_label(asking: bool, name: &str) -> String {
    if asking { format!("Ask {name}") } else { "Describe a task to start a session on it".into() }
}
