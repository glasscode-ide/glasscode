//! Today: the person's day in one list (the rail's Today). Their own tasks and the ones agents
//! add through `glasscode board`, recurring ones, Linear tickets and review requests, with what
//! needs them now, what agents ask to do, and what agents did. The board itself is
//! `glasscode_core::board`; this file has the state, what the buttons do, and the page.

use std::collections::HashSet;

use gpui::{AnyElement, AppContext as _, ClickEvent, Context, Entity, FontWeight, Hsla, SharedString, Window, div, prelude::*, px};
use glasscode_core::board::{self, Board, Item, Outside, Section, TodayView};
use glasscode_core::day;
use glasscode_core::model::AgentKind;

use crate::control_ui::{Approval, Responder};
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::workspace::Workspace;

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
}

impl TodayUi {
    pub fn new(cx: &mut Context<Workspace>) -> Self {
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
        }
    }
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
        self.tickets.view = crate::tickets_ui::View::Today;
        self.menu_open = false;
        let today = day::iso(day::today());
        if self.today.board.seen.as_deref() != Some(today.as_str()) {
            self.today.board.seen = Some(today);
            let _ = self.today.board.save();
        }
        if self.tickets.last_fetch.is_none() {
            self.tickets_refresh(cx);
        }
        cx.notify();
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
        if seen != Some(today) && morning && self.window_active && just_in_front && !navigating && free && !self.state.agents.morning_off && self.tickets.view == crate::tickets_ui::View::Sessions {
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

    fn today_needs(&self) -> Vec<(String, String, Hsla, NeedOpen)> {
        let mut out = Vec::new();
        for s in &self.state.sessions {
            if self.state.session_hidden(s) {
                continue;
            }
            if self.row_state(s) == crate::sidebar::RowState::NeedsYou && self.control.pending_for(&s.id).is_empty() {
                out.push((self.session_title(s), format!("{} is waiting for your answer", s.agent.name()), crate::sidebar::attention(), NeedOpen::Session(s.id.clone())));
            }
        }
        for r in self.server.state.as_ref().map(|s| s.sessions.as_slice()).unwrap_or_default() {
            if r.activity == Some(glasscode_core::agents::AgentActivity::NeedsInput) {
                out.push((r.session.title.clone(), format!("On {} · waiting for your answer", self.server.name()), crate::sidebar::attention(), NeedOpen::Server(r.session.id.clone())));
            }
        }
        for s in self.state.sessions.iter().filter(|s| s.review.is_none() && s.team_task.is_none() && !self.state.session_hidden(s)) {
            let Some(status) = self.prs.map.get(&s.id) else { continue };
            let (Some(pr), Some(ci)) = (&status.pr, &status.ci) else { continue };
            let failing = ci.failed().len();
            if failing > 0 && ci.state == "OPEN" {
                let fixing = if self.state.ci_autofix_off { "" } else { " · the agent is asked to fix it" };
                out.push((format!("{} · #{}", self.session_title(s), pr.number), format!("CI failing ({}){fixing}", if failing == 1 { "1 check".to_string() } else { format!("{failing} checks") }), theme::removed(), NeedOpen::Session(s.id.clone())));
            }
        }
        out
    }

    fn today_open_need(&mut self, open: &NeedOpen, window: &mut Window, cx: &mut Context<Self>) {
        match open {
            NeedOpen::Session(id) => {
                self.tickets.view = crate::tickets_ui::View::Sessions;
                self.select_session(id, window, cx);
            }
            NeedOpen::Server(id) => {
                self.tickets.view = crate::tickets_ui::View::Sessions;
                self.open_server_session(id, window, cx);
            }
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
                .map(|(title, what, _, open)| glasscode_core::remote::TodayNeed {
                    title,
                    what,
                    session: match open { NeedOpen::Session(id) => Some(id), NeedOpen::Server(id) => Some(format!("server:{id}")) },
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
        // The key must be one Glasscode added to the server for a phone (authorized_keys).
        let known = self.phones.as_ref().is_some_and(|phones| phones.iter().any(|p| p.key.split_whitespace().take(2).eq(key.split_whitespace().take(2))));
        known && glasscode_core::remote::verify_phone_signature(key, message.as_bytes(), sig)
    }

    // ----- the page --------------------------------------------------------------------------------

    pub fn render_today(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let today = day::today();
        let view = self.today_view();
        let needs = self.today_needs();
        let waiting_sessions = self.today_waiting_sessions();
        let name = self.tickets.snapshot.as_ref().and_then(|s| s.viewer.name.split_whitespace().next().map(crate::control_ui::capitalize));
        let greeting = match day::hour_now() {
            4..=11 => "Good morning",
            12..=17 => "Good afternoon",
            _ => "Good evening",
        };
        let header = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(theme::muted()).child(day::long(today)))
            .child(div().text_size(px(28.)).line_height(px(34.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(match name {
                Some(n) => format!("{greeting}, {n}"),
                None => greeting.to_string(),
            }))
            .child(div().text_size(px(15.)).text_color(theme::text_soft()).child(view.summary_with(waiting_sessions, self.control.approvals.len())));

        let filters = div().flex().flex_wrap().gap(px(6.)).children(Filter::ALL.iter().map(|(filter, label)| {
            let filter = *filter;
            let on = self.today.filter == filter;
            let count: usize = view.sections.iter().filter(|s| s.section.is_some_and(|sec| filter.shows(sec) && !matches!(sec, Section::Later | Section::Recurring | Section::Done))).map(|s| s.items.iter().filter(|i| !i.done).count()).sum();
            div()
                .id(SharedString::from(format!("today-filter-{label}")))
                .flex()
                .items_center()
                .gap_1p5()
                .h(px(30.))
                .px_3()
                .rounded_full()
                .border_1()
                .border_color(if on { theme::text_strong() } else { theme::border_strong() })
                .when(on, |d| d.bg(theme::text_strong()))
                .text_size(px(13.))
                .text_color(if on { theme::bg() } else { theme::text_soft() })
                .font_weight(if on { FontWeight::MEDIUM } else { FontWeight::NORMAL })
                .cursor_pointer()
                .when(!on, |d| d.hover(|s| s.bg(theme::hover())))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.today.filter = filter;
                    cx.notify();
                }))
                .child(label.to_string())
                .child(div().text_color(if on { theme::faint() } else { theme::faint() }).child(count.to_string()))
        }));

        let has_text = !self.today.add.read(cx).text().trim().is_empty();
        let add = div()
            .flex()
            .items_center()
            .gap_3()
            .h(px(50.))
            .pl(px(14.))
            .pr(px(8.))
            .rounded(px(12.))
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .shadow_sm()
            .child(crate::icons::icon(crate::icons::ADD, px(18.), px(18.), theme::faint()))
            .child(div().flex_1().min_w_0().child(self.today.add.clone()))
            .when_some(self.today.hint.clone(), |d, hint| d.child(div().flex_none().text_size(px(12.5)).text_color(theme::muted()).child(hint)))
            .child(
                div()
                    .id("today-add")
                    .flex_none()
                    .h(px(32.))
                    .px_3p5()
                    .flex()
                    .items_center()
                    .rounded(px(8.))
                    .text_size(px(13.))
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
                            .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(14.)).text_color(theme::text_strong()).child(s.title.clone()))
                            .child(div().text_size(px(13.)).text_color(theme::faint()).child(s.items.len().to_string()))
                            .child(div().ml_auto().text_size(px(12.5)).text_color(theme::faint()).hover(|st| st.text_color(theme::text())).child(if open { "Hide" } else { "Show" })),
                    )
                    .when(open && s.items.is_empty(), |d| d.child(div().py_3().px_1().text_size(px(13.)).text_color(theme::faint()).child(if section == Section::Focus { "Star up to three tasks to keep them here." } else { "Nothing here." })))
                    .when(open && !rows.is_empty(), |d| d.child(div().flex().flex_col().pt_1().children(rows)))
                    .into_any_element(),
            );
        }

        let left = div().flex().flex_col().flex_1().min_w(px(420.)).max_w(px(760.)).gap(px(26.)).child(header).child(filters).child(add).children(sections);
        let right = self.today_side(needs, cx);

        div()
            .id("today-scroll")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .bg(theme::bg())
            .child(div().flex().flex_wrap().items_start().gap(px(40.)).px(px(40.)).pt(px(36.)).pb(px(56.)).child(left).child(right))
            .into_any_element()
    }

    fn today_row(&self, item: &Item, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let group = SharedString::from(format!("today-row-{index}"));
        let done = item.done;
        let chip = self.today_chip(item);
        let mut meta: Vec<AnyElement> = Vec::new();
        let sep = || div().mx(px(7.)).text_color(theme::border_strong()).child("·").into_any_element();
        if let (Some(label), false) = (&item.due_label, done) {
            let color = if item.late { theme::removed() } else if item.due_now { crate::sidebar::attention() } else { theme::muted() };
            meta.push(div().text_color(color).font_weight(FontWeight::MEDIUM).child(label.clone()).into_any_element());
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
            meta.push(div().flex().items_center().gap(px(5.)).child(avatar(person)).child(text).into_any_element());
        }
        if let Some(source) = &item.source {
            meta.push(div().child(source.clone()).into_any_element());
        }
        if let Some(chip) = &chip {
            let color = if chip.needs { crate::sidebar::attention() } else if chip.working { theme::added() } else { theme::border_strong() };
            meta.push(div().flex().items_center().gap(px(5.)).child(div().size(px(6.)).rounded_full().bg(color)).child(chip.title.clone()).into_any_element());
        }
        if item.urgent && !done {
            meta.push(div().text_color(theme::removed()).child("Urgent").into_any_element());
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
        let checkbox = div()
            .id(SharedString::from(format!("today-check-{index}")))
            .flex_none()
            .mt(px(1.))
            .size(px(20.))
            .rounded_full()
            .border(px(1.5))
            .border_color(if done { theme::added() } else { theme::border_strong() })
            .when(done, |d| d.bg(theme::added()))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| s.border_color(theme::added()))
            .when(!is_recurring, |d| d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.today_toggle_done(&a, cx))))
            .when(is_recurring, |d| d.border_dashed())
            .when(done, |d| d.child(crate::icons::icon(crate::icons::CHECK, px(11.), px(11.), theme::bg())));
        let icon_button = |id: String, glyph: &'static [u8], color: Hsla| {
            div()
                .id(SharedString::from(id))
                .size(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .cursor_pointer()
                .hover(|s| s.bg(theme::selected()))
                .child(crate::icons::icon(glyph, px(15.), px(15.), color))
        };
        let actions = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(2.))
            .invisible()
            .group_hover(group.clone(), |s| s.visible())
            .when(!done && !is_recurring, |row| {
                row.child(
                    icon_button(format!("today-star-{index}"), if b.focus { crate::icons::STAR_FILLED } else { crate::icons::STAR }, if b.focus { crate::sidebar::attention() } else { theme::muted() })
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.today_toggle_focus(&b, cx))),
                )
                .child(
                    icon_button(format!("today-snooze-{index}"), crate::icons::MOON, if c.snoozed.is_some() { crate::sidebar::attention() } else { theme::muted() })
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.today_toggle_snooze(&c, cx))),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("today-start-{index}")))
                        .h(px(30.))
                        .px(px(11.))
                        .flex()
                        .items_center()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(theme::border_strong())
                        .bg(theme::raised())
                        .text_size(px(12.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::text())
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.today_start(&d, window, cx)))
                        .child(start_label),
                )
            })
            .when(item.key.starts_with("task:") && done, |row| {
                row.child(div().id(SharedString::from(format!("today-undo-{index}"))).px_2().text_size(px(12.5)).text_color(theme::muted()).cursor_pointer().hover(|s| s.text_color(theme::text())).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.today_toggle_done(&e, cx))).child("Undo"))
            });
        let title_color = if done { theme::faint() } else { theme::text_strong() };
        div()
            .id(SharedString::from(format!("today-item-{index}")))
            .group(group)
            .flex()
            .items_start()
            .gap_3()
            .pl_1()
            .pr_2()
            .py(px(9.))
            .rounded(px(10.))
            .hover(|s| s.bg(theme::hover()))
            .child(checkbox)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap_2()
                            .text_size(px(14.5))
                            .line_height(px(21.))
                            .text_color(title_color)
                            .when(done, |d| d.line_through())
                            .when_some(item.reference.clone().filter(|_| item.outside), |d, r| d.child(div().flex_none().pt(px(1.5)).font_family(theme::MONO_FONT).text_size(px(12.5)).text_color(theme::faint()).child(r)))
                            .child(div().flex_1().min_w_0().child(item.title.clone())),
                    )
                    .when(!meta_row.is_empty(), |d| d.child(div().flex().flex_wrap().items_center().text_size(px(12.5)).text_color(theme::muted()).children(meta_row))),
            )
            .child(actions)
            .into_any_element()
    }

    fn today_side(&self, needs: Vec<(String, String, Hsla, NeedOpen)>, cx: &mut Context<Self>) -> AnyElement {
        let heading = |title: &str, count: Option<usize>| {
            div()
                .flex()
                .items_baseline()
                .gap_2()
                .pb_2()
                .border_b_1()
                .border_color(theme::border())
                .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(14.)).text_color(theme::text_strong()).child(title.to_string()))
                .when_some(count, |d, n| d.child(div().text_size(px(13.)).text_color(theme::faint()).child(n.to_string())))
        };
        let mut needs_rows: Vec<AnyElement> = Vec::new();
        for (i, (title, what, color, open)) in needs.into_iter().enumerate() {
            needs_rows.push(
                div()
                    .id(SharedString::from(format!("today-need-{i}")))
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .px_1p5()
                    .py(px(9.))
                    .rounded(px(10.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.today_open_need(&open, window, cx)))
                    .child(div().flex_none().mt(px(6.)).size(px(7.)).rounded_full().bg(color))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            .child(div().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).text_color(theme::text_strong()).child(title))
                            .child(div().text_size(px(12.5)).text_color(theme::muted()).child(what)),
                    )
                    .into_any_element(),
            );
        }
        let count = needs_rows.len();
        let approvals: Vec<AnyElement> = self.control.approvals.iter().map(|a| render_approval(a, false, cx)).collect();
        let today = day::today();
        let log: Vec<AnyElement> = if self.state.agents.log_off {
            Vec::new()
        } else {
            self.control
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
                        .py(px(8.))
                        .text_size(px(12.5))
                        .child(div().flex_none().w(px(38.)).text_color(theme::faint()).child(crate::commands::clock_of(e.at)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(1.))
                                .child(div().text_color(theme::text_soft()).child(format!("{} {what}", e.who)))
                                .when_some(e.session_title.clone(), |d, t| d.child(div().text_color(theme::faint()).overflow_hidden().whitespace_nowrap().text_ellipsis().child(t))),
                        )
                        .into_any_element()
                })
                .collect()
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(340.))
            .gap(px(28.))
            .pt_1()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(heading("Needs you", Some(count)))
                    .when(needs_rows.is_empty(), |d| d.child(div().py_3().px_1().text_size(px(13.)).text_color(theme::faint()).child("Nothing waits for you right now.")))
                    .children(needs_rows),
            )
            .children(approvals)
            .when(!self.state.agents.log_off, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .child(heading("What agents did", None))
                        .when(log.is_empty(), |d| d.child(div().py_3().px_1().text_size(px(13.)).text_color(theme::faint()).child("Nothing yet today. Agents use the glasscode command; what they do shows here.")))
                        .children(log),
                )
            })
            .into_any_element()
    }
}

#[derive(Clone)]
enum NeedOpen {
    Session(String),
    Server(String),
}

/// A command for an approval card: at most a few lines, the rest trimmed.
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

/// A small round avatar with the person's initial.
fn avatar(name: &str) -> AnyElement {
    let initial: String = name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "?".into());
    let hue = name.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b))) % 360;
    div()
        .flex_none()
        .size(px(16.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::hsla(hue as f32 / 360., 0.42, if theme::is_light() { 0.46 } else { 0.52 }, 1.))
        .text_color(gpui::white())
        .text_size(px(8.5))
        .font_weight(FontWeight::BOLD)
        .child(initial)
        .into_any_element()
}

/// An agent's request waiting for the person: what it wants to do, the command, Allow / Deny.
/// `compact` for the Chat view.
pub fn render_approval(a: &Approval, compact: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let id = a.id;
    let from = match (&a.caller.session_title, compact) {
        (Some(t), false) => format!("From “{t}”."),
        _ => String::new(),
    };
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .p(px(16.))
        .rounded(px(14.))
        .border_1()
        .border_color(theme::border())
        .bg(theme::raised())
        .shadow_md()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_none().size(px(22.)).rounded_full().bg(theme::warn_bg()).flex().items_center().justify_center().text_color(crate::sidebar::attention()).text_size(px(12.)).font_weight(FontWeight::BOLD).child("?"))
                .child(div().flex_1().min_w_0().font_weight(FontWeight::SEMIBOLD).text_size(px(13.5)).text_color(theme::text_strong()).child(format!("{} asks to {}", a.caller.who, a.what))),
        )
        .when(!from.is_empty(), |d| d.child(div().text_size(px(13.)).text_color(theme::text_soft()).child(from)))
        .child(div().px(px(10.)).py(px(8.)).rounded(px(8.)).bg(theme::hover()).font_family(theme::MONO_FONT).text_size(px(12.)).text_color(theme::text_soft()).max_h(px(72.)).overflow_hidden().child(clip_command(&a.command)))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .id(SharedString::from(format!("approve-{id}-{compact}")))
                        .flex_1()
                        .h(px(32.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(8.))
                        .bg(theme::text_strong())
                        .text_color(theme::bg())
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .hover(|s| s.opacity(0.88))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.control_decide(id, true, false, window, cx)))
                        .child("Allow"),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("deny-{id}-{compact}")))
                        .flex_1()
                        .h(px(32.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(theme::border_strong())
                        .bg(theme::raised())
                        .text_size(px(13.))
                        .text_color(theme::text())
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.control_decide(id, false, false, window, cx)))
                        .child("Deny"),
                ),
        )
        .when(a.caller.session.is_some(), |d| {
            d.child(
                div()
                    .id(SharedString::from(format!("approve-session-{id}-{compact}")))
                    .self_center()
                    .text_size(px(12.5))
                    .text_color(theme::muted())
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme::text()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.control_decide(id, true, true, window, cx)))
                    .child("Allow this for the rest of the session"),
            )
        })
        .into_any_element()
}
