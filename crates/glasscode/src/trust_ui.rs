//! Settings › Manager's trust part (M53, ux-13): each project's level with what it allows, its
//! conditions, the playbooks and the schedules as sentences. Every change is previewed (what
//! `gate::decide` says before and after, which playbooks start acting, what GitHub does not guard
//! yet) and written only when the person confirms the preview (`glasscode_core::trust`). With the
//! engine at v1 none of it shows: v1's Permissions grid is the person's.

use std::cell::OnceCell;
use std::time::{Duration, Instant};

use gpui::{AnyElement, AppContext as _, ClickEvent, Context, Div, Entity, FontWeight, SharedString, Window, div, prelude::*, px, relative};
use glasscode_core::gate::{self, Level};
use glasscode_core::playbooks::{self, Book};
use glasscode_core::schedule::ScheduleFile;
use glasscode_core::store::Store as _;
use glasscode_core::trust::{self, Change, Pending};

use crate::icons;
use crate::settings_ui::{card, disclosure, inset, row, titled_group, wide_row};
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

/// How often the playbooks and schedules are read again while the engine is v2.
const REFRESH: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct TrustUi {
    /// The playbooks and schedules as this machine reads them (None: not read yet, or damaged).
    pub book: Option<Book>,
    pub schedules: Option<ScheduleFile>,
    loading: bool,
    loaded_at: Option<Instant>,
    /// What a new condition says, typed once for any project.
    condition: OnceCell<Entity<TextInput>>,
    /// A new delegated decision (M61) and a new site (M23), typed the same way.
    delegation: OnceCell<Entity<TextInput>>,
    site: OnceCell<Entity<TextInput>>,
    /// The part of one project's row that is open: what its level allows, or the field of a new
    /// condition, delegated decision or site. One at a time, so the three fields serve every
    /// project.
    open: Option<(String, Panel)>,
}

/// What a project's row can show under its level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Panel {
    Allows,
    Condition,
    Delegation,
    Site,
}

impl Panel {
    /// The words of its button in the row, what its field takes, and the words of its "add".
    fn words(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Panel::Allows => ("", "", ""),
            Panel::Condition => ("Add a condition", "not on <branches> · never <paths> · hours <from>-<to> [weekdays] · only <people>", "Add the condition"),
            Panel::Delegation => ("Delegate a decision", "<name>: <words of the question> [for <command> | <command>] [=> <answer> | <answer>]", "Delegate the decision"),
            Panel::Site => ("Allow a site", "Its host, as docs.rs (its subdomains too)", "Add the site"),
        }
    }

    fn sentence(self) -> &'static str {
        match self {
            Panel::Allows => "",
            Panel::Condition => "A condition narrows the level in this project. It is previewed before it counts.",
            Panel::Delegation => "On Autopilot the manager answers this question by itself; any other answer is a card.",
            Panel::Site => "A site its sessions that you do not drive may open, in a browser with no sign-ins.",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Panel::Allows => "allows",
            Panel::Condition => "add",
            Panel::Delegation => "delegate",
            Panel::Site => "site",
        }
    }
}

impl Workspace {
    /// The approved policy the gate reads: what a change starts from.
    fn trust_policy(&self) -> Option<gate::Policy> {
        let authority = &self.control.authority;
        authority.approved.clone().or_else(|| (!authority.live_differs).then(|| authority.policy.clone()))
    }

    /// Read the playbooks and schedules again, off the main thread, now and then.
    pub fn trust_tick(&mut self, cx: &mut Context<Self>) {
        if self.trust.loading || self.trust.loaded_at.is_some_and(|at| at.elapsed() < REFRESH) {
            return;
        }
        self.trust_reload(cx);
    }

    fn trust_reload(&mut self, cx: &mut Context<Self>) {
        self.trust.loading = true;
        let task = cx.background_spawn(async move { (playbooks::load(&playbooks::model::journal_schema()).ok(), ScheduleFile::load().ok()) });
        cx.spawn(async move |this, cx| {
            let (book, schedules) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.trust.loading = false;
                this.trust.loaded_at = Some(Instant::now());
                this.trust.book = book;
                this.trust.schedules = schedules;
                cx.notify();
            });
        })
        .detach();
    }

    /// Preview a change: its sentences, in a sheet the person confirms (`trust_confirm`).
    pub fn trust_change(&mut self, change: Change, cx: &mut Context<Self>) {
        let Some(policy) = self.trust_policy() else {
            self.show_toast("The approved policy could not be read, so nothing can change until it is repaired.", cx);
            return;
        };
        let forge = self.control_policy_set().forge;
        let (book, schedules) = (self.trust.book.clone(), self.trust.schedules.clone());
        let world = trust::World { policy: &policy, book: book.as_ref(), schedules: schedules.as_ref(), forge: &forge };
        match trust::prepare(change, &world) {
            Ok(pending) => {
                self.modal = Some(crate::dialogs::Modal::TrustChange(Box::new(pending)));
                cx.notify();
            }
            Err(why) => self.show_toast(why, cx),
        }
    }

    /// The person confirmed the preview: written in the background, then the gate and the
    /// playbooks read again.
    pub fn trust_confirm(&mut self, pending: Pending, cx: &mut Context<Self>) {
        // A level moving in asked about (M54): its question is answered once this is confirmed.
        let level = match &pending.change {
            Change::Level { project, level } => Some((project.clone(), *level)),
            _ => None,
        };
        let task = cx.background_spawn(async move { (trust::apply(&pending, true), gate::load()) });
        cx.spawn(async move |this, cx| {
            let (done, authority) = task.await;
            let _ = this.update(cx, |this, cx| {
                match done {
                    Ok(text) => {
                        this.show_toast(text, cx);
                        if let Some((project, level)) = level {
                            this.move_in_level_confirmed(project, level, cx);
                        }
                    }
                    Err(why) => this.show_toast(format!("Nothing changed: {why}"), cx),
                }
                this.control_set_authority(authority, cx);
                this.trust_reload(cx);
            });
        })
        .detach();
    }

    /// "Try it on the last 7 days": the playbook against the journal's past, without acting.
    fn trust_try(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let op = match glasscode_core::ops::typed("playbook.test", serde_json::json!({ "playbook": id })) {
            Ok(op) => op,
            Err(error) => return self.show_toast(error.text, cx),
        };
        self.run_op(
            &op,
            crate::control_ui::Caller::you(),
            |ws, reply| {
                if let glasscode_core::control::Reply::Done { text, .. } = reply {
                    ws.toast = Some((text.lines().take(6).collect::<Vec<_>>().join("\n"), Instant::now()));
                }
            },
            window,
            cx,
        );
    }

    /// Open a part of a project's row (or close it when it is the open one).
    fn trust_panel(&mut self, project: String, panel: Panel, window: &mut Window, cx: &mut Context<Self>) {
        let same = self.trust.open.as_ref().is_some_and(|(id, open)| *id == project && *open == panel);
        self.trust.open = (!same).then_some((project, panel));
        if !same {
            let input = match panel {
                Panel::Allows => None,
                Panel::Condition => self.trust.condition.get(),
                Panel::Delegation => self.trust.delegation.get(),
                Panel::Site => self.trust.site.get(),
            };
            if let Some(input) = input {
                crate::text_input::focus(input, window, cx);
            }
        }
        cx.notify();
    }

    /// Debug builds: open a part of the first project's row, for a screen check without input
    /// (`GLASSCODE_DEBUG_SETTINGS_OPEN=trust:<part>`, see `settings_debug_scroll`).
    #[cfg(debug_assertions)]
    pub(crate) fn trust_debug_open(&mut self, part: &str) {
        let panel = [Panel::Allows, Panel::Condition, Panel::Delegation, Panel::Site].into_iter().find(|panel| panel.key() == part);
        let project = self.state.visible_projects().next().map(|project| project.id.clone());
        if let (Some(panel), Some(project)) = (panel, project) {
            self.trust.open = Some((project, panel));
        }
    }

    /// The field of the open part was confirmed (its button, or Enter in it): the change it
    /// names is previewed.
    fn trust_submit(&mut self, cx: &mut Context<Self>) {
        let Some((project, panel)) = self.trust.open.clone() else { return };
        let Some(key) = self.control.project_keys.get(&project).cloned() else { return };
        let text = |input: &OnceCell<Entity<TextInput>>, cx: &mut Context<Self>| input.get().map(|input| input.read(cx).text()).unwrap_or_default();
        match panel {
            Panel::Allows => {}
            Panel::Condition => match trust::parse_condition(&text(&self.trust.condition, cx)) {
                Ok(condition) => self.trust_change(Change::AddCondition { project: key, condition }, cx),
                Err(why) => self.show_toast(why, cx),
            },
            Panel::Delegation => match glasscode_core::delegation::parse(&text(&self.trust.delegation, cx)) {
                Ok(delegation) => self.trust_change(Change::AddDelegation { project: key, delegation }, cx),
                Err(why) => self.show_toast(why, cx),
            },
            Panel::Site => {
                let host = text(&self.trust.site, cx);
                self.trust_change(Change::AddBrowserSite { project: key, host }, cx);
            }
        }
    }

    /// A field of the trust part: Enter in it does what its button does, Escape closes it.
    fn trust_field(&self, cell: &OnceCell<Entity<TextInput>>, placeholder: &'static str, cx: &mut Context<Self>) -> Entity<TextInput> {
        cell.get_or_init(|| {
            let input = cx.new(|cx| TextInput::new(placeholder, cx));
            cx.subscribe(&input, |this: &mut Workspace, _, event: &InputEvent, cx| match event {
                InputEvent::Submit => this.trust_submit(cx),
                InputEvent::Cancel => {
                    this.trust.open = None;
                    cx.notify();
                }
                InputEvent::Changed => {}
            })
            .detach();
            input
        })
        .clone()
    }

    /// The groups of the trust part, for Settings › Manager.
    pub fn trust_settings(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        let condition = self.trust_field(&self.trust.condition, "not on main · never secrets/** · hours 09:00-18:00 weekdays · only Sam", cx);
        let delegation = self.trust_field(&self.trust.delegation, "run the tests: proceed for cargo test => Yes", cx);
        let site = self.trust_field(&self.trust.site, "docs.rs", cx);
        let policy = self.trust_policy();
        let mut rows: Vec<Div> = Vec::new();
        if policy.is_none() || self.control.authority.live_differs {
            rows.push(row("The policy file changed outside Glasscode", "Every project counts as Careful until you look at the change and approve it.", None));
        }
        for project in self.state.visible_projects() {
            let Some(key) = self.control.project_keys.get(&project.id).cloned() else { continue };
            #[allow(unused_mut)]
            let mut current = policy.as_ref().map(|p| p.of(&key).clone()).unwrap_or_default();
            // Debug builds: `GLASSCODE_DEBUG_SETTINGS_OPEN=trust:sample` shows the first project
            // with a made-up condition, delegated decision and site (nothing is written).
            #[cfg(debug_assertions)]
            if rows.is_empty() && std::env::var("GLASSCODE_DEBUG_SETTINGS_OPEN").is_ok_and(|v| v.split(',').any(|part| part.trim() == "trust:sample")) {
                current.conditions.extend(["not on main", "hours 09:00-18:00 weekdays"].into_iter().filter_map(|text| trust::parse_condition(text).ok()));
                current.delegate.extend(glasscode_core::delegation::parse("run the tests: proceed for cargo test => Yes").ok());
                current.browser_hosts.push("docs.rs".into());
            }
            let open = self.trust.open.as_ref().filter(|(id, _)| *id == project.id).map(|(_, panel)| *panel);
            let picker = ui::segmented().children(Level::ALL.into_iter().map(|level| {
                let key = key.clone();
                ui::segment(SharedString::from(format!("trust-{}-{}", project.id, level.key())), current.level == level, level.label()).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.trust_change(Change::Level { project: key.clone(), level }, cx)))
            }));

            // What the project has on top of its level, each with the way to take it away.
            let mut extras: Vec<Div> = Vec::new();
            for (index, condition) in current.conditions.iter().enumerate() {
                let key = key.clone();
                let remove = ui::pill(SharedString::from(format!("trust-remove-{}-{index}", project.id)), "Remove").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.trust_change(Change::RemoveCondition { project: key.clone(), index }, cx)));
                extras.push(extra("Condition", condition.describe(), "", Some(remove.into_any_element())));
            }
            // M60: what it picks up by itself on Autopilot, when the person set it.
            if let Some(intake) = &current.intake {
                extras.push(extra("Intake", intake.describe(), format!("`glasscode policy set {} --intake-off` stops it; --intake-linear and --intake-github change the labels.", project.name), None));
            }
            // M61: the questions the manager may answer by itself on Autopilot.
            for (index, d) in current.delegate.iter().enumerate() {
                let key = key.clone();
                let remove = ui::pill(SharedString::from(format!("trust-undelegate-{}-{index}", project.id)), "Take back").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.trust_change(Change::RemoveDelegation { project: key.clone(), index }, cx)));
                extras.push(extra("Delegated", d.describe(), "", Some(remove.into_any_element())));
            }
            // M23: the sites its sessions the person does not drive may open.
            for host in &current.browser_hosts {
                let (key, gone) = (key.clone(), host.clone());
                let remove = ui::pill(SharedString::from(format!("trust-unsite-{}-{host}", project.id)), "Remove").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.trust_change(Change::RemoveBrowserSite { project: key.clone(), host: gone.clone() }, cx)));
                extras.push(extra("Site", format!("Its sessions may open {host}"), "", Some(remove.into_any_element())));
            }

            // The long explanation, and the three things to add: each opens under the row.
            let toggle = |panel: Panel| {
                let id = project.id.clone();
                move |this: &mut Self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>| this.trust_panel(id.clone(), panel, window, cx)
            };
            let actions = div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(theme::sz(6.))
                .child(disclosure(SharedString::from(format!("trust-allows-{}", project.id)), format!("What {} allows", current.level.label()), open == Some(Panel::Allows)).on_click(cx.listener(toggle(Panel::Allows))))
                .children([Panel::Condition, Panel::Delegation, Panel::Site].map(|panel| {
                    add_pill(SharedString::from(format!("trust-open-{}-{}", panel.key(), project.id)), panel.words().0, open == Some(panel)).on_click(cx.listener(toggle(panel)))
                }));
            let panel = open.map(|panel| match panel {
                Panel::Allows => inset().children(trust::consequences(current.level).into_iter().map(|sentence| {
                    div().flex().items_start().gap(theme::sz(8.)).child(div().flex_none().mt(theme::sz(6.)).child(ui::dot(theme::faint()))).child(small(sentence).flex_1().min_w_0())
                })),
                _ => {
                    let (_, takes, add) = panel.words();
                    let input = match panel {
                        Panel::Condition => condition.clone(),
                        Panel::Delegation => delegation.clone(),
                        _ => site.clone(),
                    };
                    inset()
                        .child(small(panel.sentence()))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(theme::sz(8.))
                                .child(div().flex_1().min_w(theme::sz(220.)).child(input))
                                .child(ui::button(SharedString::from(format!("trust-{}-{}", panel.key(), project.id)), add).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.trust_submit(cx)))),
                        )
                        .child(div().font_family(theme::MONO_FONT).text_size(theme::sz(11.5)).text_color(theme::card_soft_text()).child(takes))
                }
            });
            let body = div().flex().flex_col().gap(theme::sz(8.)).when(!extras.is_empty(), |d| d.child(inset().children(extras))).child(actions).children(panel);
            rows.push(card(project.name.clone(), level_words(current.level)).control(picker).body(body).into_row());
        }
        if rows.is_empty() {
            rows.push(row("No projects yet", "A project shows here with its level once it is added.", None));
        }
        out.push(titled_group("Trust level", "What the manager may do in each project. A change shows what it does first, and counts only once you confirm it.", rows));

        // What each level allows: the gate's own table.
        out.push(titled_group("What each level allows", "From the gate's default table; a project's conditions and your own rules come on top.", vec![wide_row(level_table(&table_lines()))]));

        // Playbooks.
        let mut books: Vec<Div> = Vec::new();
        match &self.trust.book {
            None => books.push(row("The playbooks could not be read yet", "They are read again in a minute.", None)),
            Some(book) => {
                for alert in &book.alerts {
                    books.push(row(alert.clone(), "", None));
                }
                for playbook in &book.playbooks {
                    let state = match (playbook.enabled, playbook.approved(), &playbook.paused, &playbook.proposed_by) {
                        (_, _, Some(paused), _) => format!("Paused by {}: {}", paused.by, paused.why),
                        (_, false, _, Some(by)) => format!("Proposed by {by}; not approved"),
                        (true, true, None, _) => "On, approved".into(),
                        (true, false, None, _) => "On, not approved: it does not run".into(),
                        (false, _, None, _) => "Off".into(),
                    };
                    let (id_on, id_approve, id_try) = (playbook.id.clone(), playbook.id.clone(), playbook.id.clone());
                    let on = playbook.enabled;
                    let controls = div()
                        .flex()
                        .gap_2()
                        .child(crate::ui::switch(gpui::SharedString::from(format!("trust-pb-{}", playbook.id)), on).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.trust_change(Change::Playbook { id: id_on.clone(), on: !on }, cx))))
                        .when(!playbook.approved(), |d| d.child(crate::ui::button(format!("trust-pb-approve-{}", playbook.id), "Approve").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.trust_change(Change::ApprovePlaybook { id: id_approve.clone() }, cx)))))
                        .child(crate::ui::button(format!("trust-pb-try-{}", playbook.id), "Try on the last 7 days").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.trust_try(id_try.clone(), window, cx))));
                    books.push(row(trust::playbook_sentence(playbook), state, Some(controls.into_any_element())));
                }
            }
        }
        out.push(titled_group("Playbooks", "What the manager does by itself when something happens. Each is a sentence; on, off and approve show a preview first.", books));

        // Schedules.
        let mut schedules: Vec<Div> = Vec::new();
        match &self.trust.schedules {
            None => schedules.push(row("The schedules could not be read yet", "", None)),
            Some(file) if file.schedules.is_empty() => schedules.push(row("No schedules", "The manager's checks and the daily brief show here once there are any.", None)),
            Some(file) => {
                for schedule in &file.schedules {
                    let (on, id_on, id_approve) = (schedule.enabled, schedule.id.clone(), schedule.id.clone());
                    let approved = self.trust.book.is_some() && schedule.proposed_by.is_none();
                    let controls = div()
                        .flex()
                        .gap_2()
                        .child(crate::ui::switch(gpui::SharedString::from(format!("trust-sc-{}", schedule.id)), on).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.trust_change(Change::Schedule { id: id_on.clone(), on: !on }, cx))))
                        .when(!approved, |d| d.child(crate::ui::button(format!("trust-sc-approve-{}", schedule.id), "Approve").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.trust_change(Change::ApproveSchedule { id: id_approve.clone() }, cx)))));
                    let state = match &schedule.proposed_by {
                        Some(by) => format!("Proposed by {by}"),
                        None => if on { "On".into() } else { "Off".into() },
                    };
                    schedules.push(row(trust::schedule_sentence(schedule), state, Some(controls.into_any_element())));
                }
            }
        }
        // Times are the machine's local ones (M41 step 1).
        let zone = glasscode_core::clock::offset_minutes(glasscode_core::clock::now_ms()).map(glasscode_core::clock::offset_words).unwrap_or_else(|| "local time".into());
        out.push(titled_group("Schedules", &format!("What runs at a time, as sentences. Times are {}'s local time ({zone}); a change of time zone is said.", crate::platform::this_computer()), schedules));
        out
    }
}

/// A level in one line, for a project's row (`Level::describe`, without "the manager").
fn level_words(level: Level) -> String {
    let words = level.describe();
    format!("{}.", sentence_case(words.strip_prefix("the manager ").unwrap_or(words)))
}

fn sentence_case(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

/// Small words on a card: the lines of a row's open part.
fn small(text: impl Into<SharedString>) -> Div {
    div().text_size(theme::sz(12.)).text_color(theme::card_soft_text()).child(text.into())
}

/// One thing a project has on top of its level: what kind, what it says, and the way to take
/// it away.
fn extra(kind: &'static str, says: impl Into<SharedString>, note: impl Into<SharedString>, control: Option<AnyElement>) -> Div {
    let note: SharedString = note.into();
    div()
        .flex()
        .items_center()
        .gap(theme::sz(10.))
        .child(div().flex_none().w(theme::sz(74.)).text_size(theme::sz(11.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::card_soft_text()).child(kind.to_uppercase()))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(theme::sz(2.))
                .child(div().text_size(theme::sz(12.5)).text_color(theme::text()).child(says.into()))
                .when(!note.is_empty(), |d| d.child(small(note))),
        )
        .children(control)
}

/// The button of a thing to add to a project: quiet, with a plus; marked while its field shows.
fn add_pill(id: impl Into<gpui::ElementId>, label: &'static str, open: bool) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(theme::sz(5.))
        .h(theme::sz(26.))
        .px(theme::sz(9.))
        .rounded(theme::sz(7.))
        .border_1()
        .border_color(theme::border())
        .text_size(theme::sz(12.))
        .text_color(if open { theme::text() } else { theme::text_soft() })
        .when(open, |d| d.bg(theme::selected()))
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(|s| s.bg(theme::tint()).text_color(theme::text()))
        .child(icons::icon(icons::PLUS, theme::sz(11.), theme::sz(11.), theme::text_soft()))
        .child(label)
}

/// The share of the table's width that says what the manager does; the four answers share the
/// rest in equal parts.
const WHAT_SHARE: f32 = 0.34;

/// The headings of the four answers: the level, and whose answer it is where a level has two.
const LEVEL_HEADS: [(&str, &str); 4] = [("Careful", "the manager"), ("Careful", "its sessions"), ("Standard", ""), ("Autopilot", "")];

/// What an answer of the table shows: its mark and its words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mark {
    Yes,
    Asks,
    Card,
    Never,
    Partly,
}

/// The mark and the words of one of the gate's answers (`trust::cell_words`). "a card each
/// time" is "card" in its cell, with the key under the table; the two long answers keep their
/// words and wrap.
fn mark_of(words: &str) -> (Mark, &str) {
    match words {
        "yes" => (Mark::Yes, "yes"),
        "asks" => (Mark::Asks, "asks"),
        "a card each time" => (Mark::Card, "card"),
        "never" => (Mark::Never, "never"),
        other => (Mark::Partly, other),
    }
}

/// The mark of an answer, 14 px wide, drawn so that the kinds read apart without their words.
fn mark(mark: Mark) -> Div {
    let frame = div().flex().flex_none().items_center().justify_center().size(theme::sz(14.));
    match mark {
        Mark::Yes => frame.child(icons::icon(icons::CHECK, theme::sz(13.), theme::sz(13.), theme::success_text())),
        Mark::Asks => frame.child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(theme::sz(13.))
                .rounded_full()
                .border_1()
                .border_color(theme::attention())
                .text_size(theme::sz(9.))
                .line_height(theme::sz(11.))
                .font_weight(FontWeight::BOLD)
                .text_color(theme::attention())
                .child("?"),
        ),
        Mark::Card => frame.child(
            div()
                .flex()
                .flex_col()
                .justify_center()
                .gap(theme::sz(1.5))
                .w(theme::sz(13.))
                .h(theme::sz(10.))
                .px(theme::sz(2.))
                .rounded(theme::sz(2.5))
                .border_1()
                .border_color(theme::text_soft())
                .child(div().h(px(1.)).bg(theme::text_soft()))
                .child(div().h(px(1.)).w(relative(0.6)).bg(theme::text_soft())),
        ),
        Mark::Never => frame.child(div().w(theme::sz(9.)).h(theme::sz(1.5)).rounded_full().bg(theme::faint())),
        Mark::Partly => frame.child(div().size(theme::sz(8.)).rounded_full().border_1().border_color(theme::text_soft())),
    }
}

fn answer(words: &str) -> Div {
    let (kind, words) = mark_of(words);
    div()
        .flex()
        .flex_1()
        .min_w_0()
        .items_start()
        .gap(theme::sz(5.))
        .child(div().flex_none().pt(theme::sz(1.5)).child(mark(kind)))
        .child(div().flex_1().min_w_0().text_size(theme::sz(12.)).text_color(if kind == Mark::Never { theme::card_soft_text() } else { theme::text() }).child(words.to_string()))
}

/// The lines of the table, each once. The gate's table has two rows of moves (one for the
/// sessions, one for the worktrees) with the same words and the same answers, and
/// `trust::level_table` keeps both because only one of them has a note; here a line that says
/// and answers the same as one before it is left out, and the first keeps its note.
fn table_lines() -> Vec<trust::TableLine> {
    let mut out: Vec<trust::TableLine> = Vec::new();
    for line in trust::level_table() {
        if !out.iter().any(|have| have.what == line.what && have.cells == line.cells) {
            out.push(line);
        }
    }
    out
}

/// A note of the table without the plan's own mark at its end ("… the send policy (M42)").
fn plain_note(note: &str) -> &str {
    match note.rsplit_once(" (") {
        Some((words, mark)) if mark.ends_with(')') && mark.len() <= 5 && mark.starts_with(|c: char| c.is_ascii_uppercase()) && mark[1..mark.len() - 1].chars().all(|c| c.is_ascii_digit()) => words,
        _ => note,
    }
}

/// The table of what each level allows: what the manager does at the left, then one column for
/// each level, all of one width, under a heading that stands apart from the rows. Every cell
/// wraps inside its share of the width, so the table fits the narrowest card.
fn level_table(lines: &[trust::TableLine]) -> Div {
    let what = |d: Div| d.flex_none().w(relative(WHAT_SHARE)).min_w_0().pr(theme::sz(10.));
    let head = div()
        .flex()
        .items_start()
        .px(theme::sz(10.))
        .py(theme::sz(8.))
        .rounded(theme::sz(8.))
        .bg(theme::tint())
        .child(what(div()).child(div().text_size(theme::sz(11.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::card_soft_text()).child("WHAT IT DOES")))
        .children(LEVEL_HEADS.map(|(level, whose)| {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(div().text_size(theme::sz(12.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(level))
                .when(!whose.is_empty(), |d| d.child(div().text_size(theme::sz(11.)).text_color(theme::card_soft_text()).child(whose)))
        }));
    let rows = lines.iter().enumerate().map(|(i, line)| {
        div()
            .flex()
            .items_start()
            .px(theme::sz(10.))
            .py(theme::sz(8.))
            .when(i > 0, |d| d.border_t_1().border_color(theme::border()))
            .child(
                what(div())
                    .flex()
                    .flex_col()
                    .gap(theme::sz(2.))
                    .child(div().text_size(theme::sz(12.5)).text_color(theme::text()).child(sentence_case(line.what)))
                    .when(!line.note.is_empty(), |d| d.child(div().text_size(theme::sz(11.)).text_color(theme::card_soft_text()).child(sentence_case(plain_note(line.note))))),
            )
            .children(line.cells.map(answer))
    });
    let key = div()
        .flex()
        .flex_wrap()
        .gap_x(theme::sz(16.))
        .gap_y(theme::sz(6.))
        .px(theme::sz(10.))
        .pt(theme::sz(10.))
        .border_t_1()
        .border_color(theme::border())
        .children(
            [(Mark::Yes, "without asking"), (Mark::Asks, "asks; your words in chat may allow it"), (Mark::Card, "a card with everything, each time"), (Mark::Never, "never"), (Mark::Partly, "as its cell says")]
                .map(|(kind, words)| div().flex().items_center().gap(theme::sz(5.)).child(mark(kind)).child(small(words))),
        );
    div().flex().flex_col().min_w_0().child(head).children(rows).child(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each answer the gate can give has its mark, and only the two long ones are "partly".
    #[test]
    fn each_answer_of_the_table_has_its_mark() {
        let mut partly = std::collections::BTreeSet::new();
        for line in trust::level_table() {
            for cell in line.cells {
                if let (Mark::Partly, words) = mark_of(cell) {
                    partly.insert(words);
                }
            }
        }
        assert!(partly.len() <= 2, "{partly:?}");
        assert_eq!(mark_of("a card each time"), (Mark::Card, "card"));
    }

    /// A row of the table shows once, whatever its note (the two rows of moves).
    #[test]
    fn a_row_of_the_table_shows_once() {
        let lines = table_lines();
        assert!(trust::level_table().len() > lines.len(), "the gate's table has no row twice any more: `table_lines` can go");
        assert_eq!(plain_note("routine pings follow the send policy (M42)"), "routine pings follow the send policy");
        assert_eq!(plain_note("a session's mode is never raised"), "a session's mode is never raised");
        for (i, line) in lines.iter().enumerate() {
            assert!(!lines[..i].iter().any(|have| have.what == line.what && have.cells == line.cells), "{} twice", line.what);
        }
    }

    #[test]
    fn a_level_reads_as_one_sentence() {
        assert_eq!(level_words(Level::Careful), "Asks before it does anything but read, keep the board and take notes.");
    }
}
