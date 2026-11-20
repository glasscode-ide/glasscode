//! Settings › Manager's trust part (M53, ux-13): each project's level with what it allows, its
//! conditions, the playbooks and the schedules as sentences. Every change is previewed (what
//! `gate::decide` says before and after, which playbooks start acting, what GitHub does not guard
//! yet) and written only when the person confirms the preview (`glasscode_core::trust`). With the
//! engine at v1 none of it shows: v1's Permissions grid is the person's.

use std::cell::OnceCell;
use std::time::{Duration, Instant};

use gpui::{AnyElement, AppContext as _, ClickEvent, Context, Div, Entity, FontWeight, Window, div, prelude::*, px};
use glasscode_core::gate::{self, Level};
use glasscode_core::playbooks::{self, Book};
use glasscode_core::schedule::ScheduleFile;
use glasscode_core::store::Store as _;
use glasscode_core::trust::{self, Change, Pending};

use crate::settings_ui::{group, row};
use crate::text_input::TextInput;
use crate::theme;
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
}

impl Workspace {
    /// The approved policy the gate reads: what a change starts from.
    fn trust_policy(&self) -> Option<gate::Policy> {
        let authority = &self.control.authority;
        authority.approved.clone().or_else(|| (!authority.live_differs).then(|| authority.policy.clone()))
    }

    /// Read the playbooks and schedules again, off the main thread, now and then.
    pub fn trust_tick(&mut self, cx: &mut Context<Self>) {
        if !self.control.engine_v2 || self.trust.loading || self.trust.loaded_at.is_some_and(|at| at.elapsed() < REFRESH) {
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

    /// The groups of the trust part, for Settings › Manager.
    pub fn trust_settings(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if !self.control.engine_v2 {
            return Vec::new();
        }
        let mut out = Vec::new();
        let condition = self.trust.condition.get_or_init(|| cx.new(|cx| TextInput::new("not on main · never secrets/** · hours 09:00-18:00 weekdays · only Sam", cx))).clone();
        let policy = self.trust_policy();
        let mut rows: Vec<Div> = Vec::new();
        if policy.is_none() || self.control.authority.live_differs {
            rows.push(row("The policy file changed outside Glasscode", "Every project counts as Careful until you look at the change and approve it.", None));
        }
        for project in self.state.visible_projects() {
            let Some(key) = self.control.project_keys.get(&project.id).cloned() else { continue };
            let current = policy.as_ref().map(|p| p.of(&key).clone()).unwrap_or_default();
            let picker = segments().children(Level::ALL.into_iter().map(|level| {
                let key = key.clone();
                segment(format!("trust-{}-{}", project.id, level.key()), current.level == level, level.label()).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.trust_change(Change::Level { project: key.clone(), level }, cx)))
            }));
            rows.push(row(project.name.clone(), trust::consequences(current.level).join(" "), Some(picker.into_any_element())));
            for (index, condition) in current.conditions.iter().enumerate() {
                let key = key.clone();
                let remove = crate::ui::button(format!("trust-remove-{}-{index}", project.id), "Remove").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.trust_change(Change::RemoveCondition { project: key.clone(), index }, cx)));
                rows.push(row(format!("    {}", condition.describe()), "", Some(remove.into_any_element())));
            }
            let (key_add, input) = (key.clone(), condition.clone());
            let add = crate::ui::button(format!("trust-add-{}", project.id), "Add the condition").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                let text = input.read(cx).text();
                match trust::parse_condition(&text) {
                    Ok(condition) => this.trust_change(Change::AddCondition { project: key_add.clone(), condition }, cx),
                    Err(why) => this.show_toast(why, cx),
                }
            }));
            rows.push(row("    Add a condition", "Typed in the field below; it is previewed before it counts.", Some(add.into_any_element())));
        }
        rows.push(row("A new condition", "not on <branches> · never <paths> · hours <from>-<to> [weekdays] · only <people>", Some(div().w(px(360.)).child(condition).into_any_element())));
        out.push(group("Trust level", "What the manager may do in each project. A change shows what it does first, and counts only once you confirm it.", rows));

        // What each level allows: the gate's own table.
        let mut table: Vec<Div> = vec![row_cells("", ["Careful", "Careful (its sessions)", "Standard", "Autopilot"], true)];
        for line in trust::level_table() {
            table.push(row_cells(line.what, line.cells, false));
        }
        out.push(group("What each level allows", "Generated from the gate's default table; a project's conditions and your own rules come on top.", table));

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
        out.push(group("Playbooks", "What the manager does by itself when something happens. Each is a sentence; on, off and approve show a preview first.", books));

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
        out.push(group("Schedules", "What runs at a time, as sentences.", schedules));
        out
    }
}

fn segments() -> Div {
    div().flex().flex_wrap().gap_0p5().p_0p5().rounded(px(8.)).border_1().border_color(theme::border()).bg(theme::bg())
}

fn segment(id: impl Into<gpui::SharedString>, on: bool, label: impl Into<gpui::SharedString>) -> gpui::Stateful<Div> {
    div()
        .id(gpui::ElementId::Name(id.into()))
        .flex()
        .items_center()
        .h(px(30.))
        .px_3()
        .rounded(px(6.))
        .cursor_pointer()
        .when(on, |d| d.bg(theme::selected()).text_color(theme::text_strong()).font_weight(FontWeight::SEMIBOLD))
        .when(!on, |d| d.text_color(theme::muted()).hover(|s| s.text_color(theme::text())))
        .child(label.into())
}

/// One line of the level table: what, then a cell for each column.
fn row_cells(what: &str, cells: [&str; 4], head: bool) -> Div {
    let cell = |text: &str| div().w(px(150.)).flex_none().text_size(theme::sz(12.)).text_color(if head { theme::muted() } else { theme::text() }).child(text.to_string());
    div()
        .flex()
        .items_center()
        .gap(theme::sz(8.))
        .px(theme::sz(16.))
        .py(theme::sz(6.))
        .child(div().flex_1().text_size(theme::sz(12.)).text_color(theme::text()).when(head, |d| d.font_weight(FontWeight::SEMIBOLD)).child(what.to_string()))
        .children(cells.into_iter().map(cell))
}
