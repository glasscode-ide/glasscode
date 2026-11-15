//! Settings: a page of its own (the rail's Settings), in sections: General, Accounts, Projects
//! & Git, Automation, Browser, Server & devices, Updates & CLIs and Shortcuts. Each section is
//! cards of rows, a label and what it does on the left, its control on the right; changes save
//! at once.

use gpui::{AnyElement, ClickEvent, Context, Div, FontWeight, SharedString, Stateful, Window, div, prelude::*, px};
use glasscode_core::model::StartupMode;

use crate::dialogs::AccountsForm;
use crate::platform;
use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    General,
    Accounts,
    Projects,
    Automation,
    Browser,
    Server,
    Updates,
    Shortcuts,
}

const SECTIONS: [(Section, &str, &str); 8] = [
    (Section::General, "General", "How Glasscode starts, and what it tells you about."),
    (Section::Accounts, "Accounts", "Claude, Codex, GitHub and Linear accounts."),
    (Section::Projects, "Projects & Git", "Per repository: which GitHub account works there and where worktrees go."),
    (Section::Automation, "Automation", "What Glasscode does by itself, and when it stops."),
    (Section::Browser, "Browser", "The browser agents use for previews, signed in as you."),
    (Section::Server, "Server & devices", "Your always-on server and the iPhone."),
    (Section::Updates, "Updates & CLIs", "Signed releases for every part of Glasscode, and the CLI versions sessions run."),
    (Section::Shortcuts, "Shortcuts", "Keys that work everywhere in Glasscode."),
];

#[derive(Default)]
pub struct SettingsUi {
    pub section: Section,
    /// The inputs Accounts uses (adding an account, a commit email) and its sign-in terminal.
    pub form: Option<AccountsForm>,
}

/// A settings card: its rows, with a line between them.
pub(crate) fn group(title: &str, note: &str, rows: Vec<Div>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_baseline()
                .gap_2p5()
                .px_1()
                .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(theme::muted()).child(title.to_string()))
                .when(!note.is_empty(), |d| d.child(div().text_size(px(12.5)).text_color(theme::faint()).child(note.to_string()))),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .rounded(px(14.))
                .border_1()
                .border_color(theme::border())
                .bg(theme::raised())
                .children(rows.into_iter().enumerate().map(|(i, row)| row.when(i > 0, |d| d.border_t_1().border_color(theme::border())))),
        )
        .into_any_element()
}

/// A card around something that draws itself (the account lists, the browser).
fn block(content: AnyElement) -> AnyElement {
    div().p_4().rounded(px(14.)).border_1().border_color(theme::border()).bg(theme::raised()).child(content).into_any_element()
}

pub(crate) fn row(label: impl Into<SharedString>, detail: impl Into<SharedString>, control: Option<AnyElement>) -> Div {
    row_with(div().font_weight(FontWeight::MEDIUM).text_color(theme::text_strong()).child(label.into()), detail, control)
}

/// A settings row whose title is more than words (a name with its badges).
pub(crate) fn row_with(title: impl IntoElement, detail: impl Into<SharedString>, control: Option<AnyElement>) -> Div {
    let detail: SharedString = detail.into();
    div()
        .flex()
        .items_center()
        .flex_wrap()
        .gap_4()
        .min_h(px(56.))
        .px_4()
        .py_3()
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(240.))
                .gap_0p5()
                .child(title)
                .when(!detail.is_empty(), |d| d.child(div().text_size(px(12.5)).text_color(theme::muted()).child(detail))),
        )
        .when_some(control, |d, c| d.child(c))
}

fn value(text: impl Into<SharedString>) -> AnyElement {
    div().text_color(theme::text_soft()).child(text.into()).into_any_element()
}

fn mono(text: impl Into<SharedString>) -> AnyElement {
    div().font_family(theme::MONO_FONT).text_size(px(12.)).text_color(theme::text_soft()).child(text.into()).into_any_element()
}

/// An on/off switch.
fn switch(id: impl Into<SharedString>, on: bool) -> Stateful<Div> {
    div()
        .id(gpui::ElementId::Name(id.into()))
        .flex()
        .flex_none()
        .items_center()
        .w(px(40.))
        .h(px(24.))
        .p(px(3.))
        .rounded_full()
        .cursor_pointer()
        .bg(if on { theme::accent() } else { theme::selected() })
        .when(on, |d| d.justify_end())
        .child(div().size(px(18.)).rounded_full().bg(if on { theme::on_accent() } else { theme::muted() }))
}

/// A row for something that is not built yet: a quiet "Coming" tag, and its switch (when it
/// has one) drawn but doing nothing.
fn coming(label: &str, detail: &str, on: Option<bool>) -> Div {
    let tag = div()
        .flex_none()
        .h(px(20.))
        .px_2()
        .flex()
        .items_center()
        .rounded_full()
        .border_1()
        .border_color(theme::border())
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::muted())
        .child("Coming");
    let control = div()
        .flex()
        .items_center()
        .gap_2p5()
        .child(tag)
        .when_some(on, |d, on| d.child(switch(SharedString::from(format!("coming-{label}")), on).opacity(0.45).cursor_default()))
        .into_any_element();
    row(label.to_string(), detail.to_string(), Some(control))
}

impl Workspace {
    pub fn open_settings(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.form.is_none() {
            self.settings.form = Some(AccountsForm::new(window, cx));
        }
        if let Some(form) = self.settings.form.as_mut() {
            form.linear_first = false;
        }
        self.settings.section = section;
        if section == Section::Server && self.server.connected() {
            self.phones_refresh(cx);
        }
        self.github.editing = None;
        self.github_accounts_refresh(cx);
        self.tickets.picker = None;
        self.linear_load_choices(cx);
        self.show_view(crate::tickets_ui::View::Settings, cx);
    }

    /// A switch row for one of the state's "…_off" settings (on means the feature works).
    /// Opt-in per repository: review a pull request that asks for you and post an overview
    /// comment. Each repository with a current review request (or already opted in) gets a row.
    fn settings_auto_review(&self, cx: &mut Context<Self>) -> AnyElement {
        let repos = self.auto_review_repos();
        let rows: Vec<Div> = if repos.is_empty() {
            vec![row(
                "No repositories yet",
                "A repository appears here once a pull request asks for your review.",
                None,
            )]
        } else {
            repos
                .into_iter()
                .map(|repo| {
                    let on = self.auto_review_on(&repo);
                    let r = repo.clone();
                    let control = switch(SharedString::from(format!("auto-review-{repo}")), on)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if !this.state.auto_review_repos.remove(&r) {
                                this.state.auto_review_repos.insert(r.clone());
                            }
                            this.save();
                            cx.notify();
                        }))
                        .into_any_element();
                    row(repo, "Review a PR that asks for you and post an overview comment", Some(control))
                })
                .collect()
        };
        group(
            "Auto-review pull requests",
            "Local — runs your agent on the diff and posts a comment, never an approval",
            rows,
        )
    }

    fn switch_row(&self, id: &'static str, label: &str, detail: &str, on: bool, flip: fn(&mut glasscode_core::model::State), cx: &mut Context<Self>) -> Div {
        row(
            label.to_string(),
            detail.to_string(),
            Some(
                switch(id, on)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        flip(&mut this.state);
                        this.save();
                        this.configure_github(cx);
                        cx.notify();
                    }))
                    .into_any_element(),
            ),
        )
    }

    pub fn render_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let current = self.settings.section;
        let nav = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(232.))
            .h_full()
            .px_3()
            .py_5()
            .gap_0p5()
            .bg(theme::panel())
            .border_r_1()
            .border_color(theme::border())
            .child(div().px_2p5().pb_3().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(theme::muted()).child("Settings"))
            .children(SECTIONS.iter().map(|(section, label, _)| {
                let section = *section;
                let on = section == current;
                let count = match section {
                    Section::Accounts => Some(self.state.accounts.len()),
                    Section::Projects => Some(self.state.projects.len()),
                    _ => None,
                };
                div()
                    .id(SharedString::from(format!("settings-nav-{label}")))
                    .flex()
                    .items_center()
                    .h(px(34.))
                    .px_2p5()
                    .rounded(px(8.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(theme::selected()).text_color(theme::text_strong()).font_weight(FontWeight::SEMIBOLD))
                    .when(!on, |d| d.text_color(theme::text_soft()))
                    .hover(|s| s.bg(theme::hover()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_settings(section, window, cx)))
                    .child(div().flex_1().child(label.to_string()))
                    .when_some(count, |d, n| d.child(div().text_size(px(12.)).text_color(theme::faint()).child(n.to_string())))
            }))
            .child(div().h(px(1.)).my_3().mx_2p5().bg(theme::border()))
            .child(
                div()
                    .id("settings-themes")
                    .flex()
                    .items_center()
                    .h(px(34.))
                    .px_2p5()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(theme::border())
                    .text_color(theme::text_soft())
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.show_view(crate::tickets_ui::View::Themes, cx)))
                    .child("Appearance → Themes"),
            );
        let (_, title, note) = SECTIONS.iter().find(|(s, _, _)| *s == current).copied().unwrap_or(SECTIONS[0]);
        let body: Vec<AnyElement> = match current {
            Section::General => self.settings_general(cx),
            Section::Accounts => self.settings_accounts(window, cx),
            Section::Projects => self.settings_projects(cx),
            Section::Automation => self.settings_automation(cx),
            Section::Browser => vec![
                block(crate::browser_ui::render_browser_dialog(self, window, cx)),
                group("Limits", "", vec![coming("Sites agents may open", "Any site for now", None)]),
            ],
            Section::Server => self.settings_server(cx),
            Section::Updates => self.settings_updates(cx),
            Section::Shortcuts => self.settings_shortcuts(),
        };
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme::bg())
            .child(nav)
            .child(
                div().id("settings-scroll").flex_1().min_w_0().h_full().overflow_y_scroll().child(
                    div()
                        .max_w(px(840.))
                        .px_8()
                        .py_7()
                        .flex()
                        .flex_col()
                        .gap_6()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1p5()
                                .child(div().text_size(px(23.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(title))
                                .child(div().text_color(theme::muted()).child(note)),
                        )
                        .children(body),
                ),
            )
            .into_any_element()
    }

    fn settings_general(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let startup = self.state.startup;
        let seg = |id: &'static str, mode: StartupMode, label: &'static str| {
            let on = startup == mode;
            div()
                .id(id)
                .flex()
                .items_center()
                .h(px(30.))
                .px_3()
                .rounded(px(6.))
                .cursor_pointer()
                .when(on, |d| d.bg(theme::selected()).text_color(theme::text_strong()).font_weight(FontWeight::SEMIBOLD))
                .when(!on, |d| d.text_color(theme::muted()))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.state.startup = mode;
                    this.save();
                    cx.notify();
                }))
                .child(label)
        };
        let startup_control = div()
            .flex()
            .gap_0p5()
            .p_0p5()
            .rounded(px(8.))
            .border_1()
            .border_color(theme::border())
            .bg(theme::bg())
            .child(seg("startup-restore", StartupMode::Restore, "Restore everything"))
            .child(seg("startup-claude", StartupMode::RestoreAndNewClaude, "Restore + a new Claude"))
            .into_any_element();
        let n = &self.state.notify;
        vec![
            group("When Glasscode opens", "", vec![row("Sessions", "Same projects, worktrees, tabs, accounts and conversations", Some(startup_control))]),
            group(
                "Tell me when",
                "A small notice at the top right, unless you are looking at that session",
                vec![
                    self.switch_row("notify-needs", "A session needs you", "A question, a permission or a plan to approve", !n.needs_input_off, |s| s.notify.needs_input_off = !s.notify.needs_input_off, cx),
                    self.switch_row("notify-finished", "A session finishes its turn", "", !n.finished_off, |s| s.notify.finished_off = !s.notify.finished_off, cx),
                    self.switch_row("notify-ci", "CI fails on one of my pull requests", "Also when Glasscode gives up asking the agent to fix it, and for merge conflicts", !self.state.prefs.notify_ci_off, |s| s.prefs.notify_ci_off = !s.prefs.notify_ci_off, cx),
                    self.switch_row("notify-tickets", "A ticket is assigned to me or a review asks for me", "From Linear and GitHub (Tickets)", !self.state.linear.notify_off, |s| s.linear.notify_off = !s.linear.notify_off, cx),
                    self.switch_row("notify-sound", "Play a sound", "", !n.sound_off, |s| s.notify.sound_off = !s.notify.sound_off, cx),
                ],
            ),
            group(
                "Sessions",
                "",
                vec![
                    self.switch_row(
                        "auto-names",
                        "Name sessions and new worktrees by themselves",
                        "From the pull request's title, else Claude's own title for the conversation, else your first message. A new worktree's branch follows, renamed only before its first push. Names you give stay.",
                        !self.state.auto_names_off,
                        |s| s.auto_names_off = !s.auto_names_off,
                        cx,
                    ),
                    self.switch_row("confirm-close", "Ask before closing a session that is working", &format!("{close} asks first; a second {close} closes it", close = crate::keymap::label(&crate::workspace::CloseSession)), !self.state.prefs.confirm_close_off, |s| s.prefs.confirm_close_off = !s.prefs.confirm_close_off, cx),
                    row("Open files in", "The editor button and file links", Some(self.editor_choice(cx))),
                ],
            ),
        ]
    }

    /// Segmented buttons for a setting with a few values.
    fn choice<T: PartialEq + Copy + 'static>(&self, id: &'static str, options: Vec<(T, &'static str)>, current: T, set: fn(&mut glasscode_core::model::State, T), cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_wrap()
            .gap_0p5()
            .p_0p5()
            .rounded(px(8.))
            .border_1()
            .border_color(theme::border())
            .bg(theme::bg())
            .children(options.into_iter().enumerate().map(|(i, (v, label))| {
                let on = v == current;
                div()
                    .id(SharedString::from(format!("{id}-{i}")))
                    .flex()
                    .items_center()
                    .h(px(30.))
                    .px_3()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(theme::selected()).text_color(theme::text_strong()).font_weight(FontWeight::SEMIBOLD))
                    .when(!on, |d| d.text_color(theme::muted()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        set(&mut this.state, v);
                        this.save();
                        cx.notify();
                    }))
                    .child(label)
            }))
            .into_any_element()
    }

    fn editor_choice(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.state.prefs.editor.clone();
        let mut options: Vec<(Option<String>, &'static str)> = vec![(None, "Whichever is installed")];
        options.extend(glasscode_core::prefs::EDITORS.iter().map(|(id, name, _)| (Some(id.to_string()), *name)));
        div()
            .flex()
            .flex_wrap()
            .gap_0p5()
            .p_0p5()
            .rounded(px(8.))
            .border_1()
            .border_color(theme::border())
            .bg(theme::bg())
            .children(options.into_iter().enumerate().map(|(i, (id, name))| {
                let on = id == current;
                div()
                    .id(SharedString::from(format!("editor-{i}")))
                    .flex()
                    .items_center()
                    .h(px(30.))
                    .px_3()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(theme::selected()).text_color(theme::text_strong()).font_weight(FontWeight::SEMIBOLD))
                    .when(!on, |d| d.text_color(theme::muted()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.state.prefs.editor = id.clone();
                        this.save();
                        cx.notify();
                    }))
                    .child(name)
            }))
            .into_any_element()
    }

    fn settings_accounts(&self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(form) = self.settings.form.as_ref() else { return Vec::new() };
        let mut out: Vec<AnyElement> = Vec::new();
        let linear = block(crate::tickets_ui::render_linear_section(self, cx));
        let linear_first = form.linear_first;
        if linear_first {
            out.push(linear);
        }
        out.extend(crate::dialogs::account_groups(self, form, cx));
        out.extend(crate::dialogs::accounts_terminal(form, cx));
        out.push(crate::github_accounts_ui::render_github_section(self, &form.github_email, window, cx));
        if !linear_first {
            out.push(block(crate::tickets_ui::render_linear_section(self, cx)));
        }
        // TODO(glasscode): the hosted sign-in section was removed from the standalone build.
        out
    }

    /// Switch a project off (hidden from the sidebar, the CLI and Glasscode) or back on. Nothing on
    /// disk changes; sessions keep running. When the active session belonged to a project just
    /// hidden, move to a still-visible session so the center pane never shows a hidden one.
    pub fn set_project_hidden(&mut self, project_id: &str, hidden: bool, cx: &mut Context<Self>) {
        let Some(name) = self.state.project(project_id).map(|p| p.name.clone()) else { return };
        if hidden {
            self.state.hidden_projects.insert(project_id.to_string());
            let active_hidden = self
                .state
                .active_session
                .as_deref()
                .and_then(|id| self.state.session(id))
                .is_some_and(|s| s.project_id == project_id);
            if active_hidden {
                let next = self.state.visible_sessions().next().map(|s| s.id.clone());
                self.state.active_session = next;
            }
        } else {
            self.state.hidden_projects.remove(project_id);
        }
        let sessions = self.state.sessions.iter().filter(|s| s.project_id == project_id).count();
        self.save();
        self.show_toast(
            if hidden {
                let kept = match sessions {
                    0 => "no sessions".to_string(),
                    1 => "1 session kept".to_string(),
                    n => format!("{n} sessions kept"),
                };
                format!("{name} is hidden — off the sidebar, the CLI and Glasscode ({kept}). Switch it back on here anytime.")
            } else {
                format!("{name} is back in Glasscode.")
            },
            cx,
        );
        cx.notify();
    }

    fn settings_projects(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let logins: Vec<String> = self.github.list.clone().unwrap_or_default().into_iter().map(|a| a.login).collect();
        let default = self.state.github.default.clone();
        let mut out: Vec<AnyElement> = Vec::new();
        for project in &self.state.projects {
            let id = project.id.clone();
            let git = project.root.join(".git").exists();
            let mut choices: Vec<(Option<String>, String)> = vec![(None, format!("Default · {}", default.clone().unwrap_or_else(|| "gh's own".into())))];
            for login in logins.iter().filter(|l| Some(*l) != default.as_ref()) {
                choices.push((Some(login.clone()), login.clone()));
            }
            let picked = project.github_account.clone();
            let account_control = div()
                .flex()
                .flex_wrap()
                .gap_0p5()
                .p_0p5()
                .rounded(px(8.))
                .border_1()
                .border_color(theme::border())
                .bg(theme::bg())
                .children(choices.into_iter().enumerate().map(|(i, (login, label))| {
                    let on = login == picked;
                    let id = id.clone();
                    div()
                        .id(SharedString::from(format!("project-gh-{}-{i}", project.id)))
                        .flex()
                        .items_center()
                        .h(px(30.))
                        .px_3()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .when(on, |d| d.bg(theme::selected()).text_color(theme::text_strong()).font_weight(FontWeight::SEMIBOLD))
                        .when(!on, |d| d.text_color(theme::muted()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_project_github(&id, login.clone(), cx)))
                        .child(label)
                }))
                .into_any_element();
            let repo = git.then(|| glasscode_core::github::repo_slug(&project.root)).flatten();
            let git_row = if git {
                row("Repository", repo.map(|r| format!("{r} on GitHub")).unwrap_or_else(|| "Not on GitHub".into()), Some(mono(project.root.display().to_string())))
            } else {
                let id = project.id.clone();
                row(
                    "Not in git yet",
                    "Sessions work in the folder itself. Set up git to get worktrees, branches and pull requests.",
                    Some(ui::button(SharedString::from(format!("setup-git-{}", project.id)), "Set up git…").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_up_git(&id, cx))).into_any_element()),
                )
            };
            // The one switch that lives here and nowhere else: off hides this project — its
            // worktrees, sessions and usage — from the sidebar, the CLI and Glasscode, but it stays
            // listed here so it can be switched back on. Nothing on disk is touched.
            let hidden = self.state.is_hidden(&project.id);
            let toggle_id = project.id.clone();
            let visibility_row = row(
                "Show in Glasscode",
                "Off: hidden from the sidebar, the glasscode CLI and Glasscode. The folder, branches and sessions stay; switch back on to restore it.",
                Some(
                    switch(SharedString::from(format!("project-visible-{}", project.id)), !hidden)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.set_project_hidden(&toggle_id, !this.state.is_hidden(&toggle_id), cx)
                        }))
                        .into_any_element(),
                ),
            );
            let mut rows = vec![visibility_row, git_row];
            if git {
                rows.push(row("GitHub account", "Pushes, PRs, gh and the commit email for every session here", Some(account_control)));
                rows.push(row("New worktrees go in", "", Some(mono(format!("~/.glasscode/worktrees/{}", project.name)))));
                let id = project.id.clone();
                rows.push(row(
                    "Clean up worktrees",
                    "Pick the ones you no longer need; each is checked for unsaved work first",
                    Some(ui::button(SharedString::from(format!("cleanup-{}", project.id)), "Clean up…").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_remove_worktrees(id.clone(), None, window, cx))).into_any_element()),
                ));
            }
            out.push(group(&project.name, if hidden { "Hidden — switched off below" } else { "" }, rows));
        }
        out.push(group(
            "Worktrees",
            "For every project",
            vec![
                self.switch_row(
                    "fresh-main",
                    "Start new work from the latest main",
                    &format!("Fetched first, never from a stale folder. Off: from main as it is on {}.", platform::this_computer()),
                    !self.state.prefs.fresh_main_off,
                    |s| s.prefs.fresh_main_off = !s.prefs.fresh_main_off,
                    cx,
                ),
                row("Removing a worktree", "The dialog closes, the row says Removing… and you carry on", Some(value("In the background"))),
                self.switch_row(
                    "delete-branch",
                    "Also delete the local branch when removing a worktree",
                    "Only when every commit of it is on the remote or another branch; you can change it each time",
                    !self.state.prefs.keep_branch_on_remove,
                    |s| s.prefs.keep_branch_on_remove = !s.prefs.keep_branch_on_remove,
                    cx,
                ),
            ],
        ));
        out
    }

    fn settings_automation(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            group(
                "When a pull request needs fixing",
                "",
                vec![
                    self.switch_row(
                        "auto-ci",
                        "Ask the session's agent to fix failing CI",
                        "Once per commit, when its turn is over and nobody is typing. Never for review sessions.",
                        !self.state.ci_autofix_off,
                        |s| s.ci_autofix_off = !s.ci_autofix_off,
                        cx,
                    ),
                    self.switch_row(
                        "auto-conflicts",
                        "Ask the session's agent to resolve merge conflicts",
                        "When GitHub says the pull request conflicts with its base: once per new commit on either side. Conflicts come before CI.",
                        !self.state.conflict_autofix_off,
                        |s| s.conflict_autofix_off = !s.conflict_autofix_off,
                        cx,
                    ),
                    row(
                        "Asks before it is over to you",
                        "In a row, until it passes or merges again; then a notification",
                        Some(self.choice("fix-tries", vec![(1, "1"), (2, "2"), (3, "3"), (5, "5")], self.state.prefs.fix_tries(), |s, n| s.prefs.fix_tries = Some(n), cx)),
                    ),
                    row(
                        "Wait for the other checks",
                        "Once one check fails, so the agent hears of every failure at once",
                        Some(self.choice("ci-wait", vec![(5, "5 min"), (10, "10 min"), (20, "20 min")], (self.state.prefs.ci_wait().as_secs() / 60) as u32, |s, m| s.prefs.ci_wait_minutes = Some(m), cx)),
                    ),
                ],
            ),
            self.settings_auto_review(cx),
            group(
                "Tickets",
                "Linear",
                vec![self.switch_row(
                    "auto-move",
                    "Move a ticket to In Progress when its session starts",
                    "Only from Triage, Backlog or Todo; you can turn it off each time",
                    !self.state.linear.move_on_start_off,
                    |s| s.linear.move_on_start_off = !s.linear.move_on_start_off,
                    cx,
                )],
            ),
            self.settings_agents(cx),
            group(
                "Today",
                "",
                vec![self.switch_row(
                    "today-morning",
                    "Open Today the first time each morning",
                    "When Glasscode is in front before noon and you have not looked at Today yet",
                    !self.state.agents.morning_off,
                    |s| s.agents.morning_off = !s.agents.morning_off,
                    cx,
                )],
            ),
            group("iPhone", "", vec![coming("Answer “Claude wants to push” from the Lock Screen", "And from the Dynamic Island", Some(false))]),
        ]
    }

    /// What agents may do with the `glasscode` command: a rule per kind of action, and who
    /// else may use it.
    fn settings_agents(&self, cx: &mut Context<Self>) -> AnyElement {
        use glasscode_core::control::{Kind, Rule};
        let mut rows: Vec<Div> = Vec::new();
        for kind in Kind::ALL {
            let current = self.state.agents.rule(kind);
            let segments = div()
                .flex()
                .flex_none()
                .gap_0p5()
                .p_0p5()
                .rounded(px(8.))
                .border_1()
                .border_color(theme::border())
                .bg(theme::bg())
                .children([(Rule::Allow, "Allow"), (Rule::Ask, "Ask"), (Rule::Never, "Never")].into_iter().map(|(rule, label)| {
                    let on = rule == current;
                    div()
                        .id(SharedString::from(format!("agent-rule-{}-{label}", kind.key())))
                        .flex()
                        .items_center()
                        .h(px(26.))
                        .px_2p5()
                        .rounded(px(6.))
                        .text_size(px(12.5))
                        .cursor_pointer()
                        .when(on, |d| d.bg(theme::selected()).text_color(if rule == Rule::Never { theme::removed() } else { theme::text_strong() }).font_weight(FontWeight::SEMIBOLD))
                        .when(!on, |d| d.text_color(theme::muted()).hover(|s| s.text_color(theme::text())))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.state.agents.set(kind, rule);
                            this.save();
                            cx.notify();
                        }))
                        .child(label)
                }))
                .into_any_element();
            let note = if kind.default_rule() == current { "" } else { "Changed from the suggestion" };
            rows.push(row(kind.label(), note, Some(segments)));
        }
        let skill_on = !self.state.agents.skill_off;
        rows.push(row(
            "Tell new sessions about the glasscode command",
            "Claude gets a skill, Codex a few lines of instructions. Off: agents only find it if someone tells them.",
            Some(
                switch("agents-skill", skill_on)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.state.agents.skill_off = !this.state.agents.skill_off;
                        this.save();
                        this.control_write_files();
                        cx.notify();
                    }))
                    .into_any_element(),
            ),
        ));
        rows.push(self.switch_row(
            "agents-outside",
            "Programs Glasscode did not start follow these rules too",
            "Off: they can only read; anything else waits for your Allow. You at a terminal can always do everything.",
            self.state.agents.outside_allowed,
            |s| s.agents.outside_allowed = !s.agents.outside_allowed,
            cx,
        ));
        rows.push(self.switch_row(
            "agents-log",
            "Show what agents did on Today",
            "One line per action, with the session that did it (`glasscode log` lists them all)",
            !self.state.agents.log_off,
            |s| s.agents.log_off = !s.agents.log_off,
            cx,
        ));
        let status = match (&self.control.error, self.control.listening) {
            (Some(e), _) => e.clone(),
            (None, true) => format!("Listening. `glasscode help` lists the commands ({}).", glasscode_core::control::socket_path().display()),
            (None, false) => "Not started.".into(),
        };
        rows.push(row("The glasscode command", status, None));
        group("What agents may do", "Claude and Codex control Glasscode with the glasscode command", rows)
    }

    fn settings_server(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let connected = self.server.connected();
        let has_server = self.server.config.is_some();
        let (status, detail) = if connected {
            (format!("{} · online", self.server.name()), format!("Sessions there keep running when {} is closed or asleep", platform::this_computer()))
        } else if has_server {
            (format!("{} · not connected", self.server.name()), "Glasscode keeps trying by itself".to_string())
        } else {
            ("No server yet".to_string(), format!("An always-on machine where sessions keep running while {} sleeps", platform::this_computer()))
        };
        let controls = div()
            .flex()
            .gap_2()
            .when(has_server && !connected, |d| d.child(ui::button("settings-reconnect", "Reconnect").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.server_connect(cx)))))
            .child(ui::button("settings-server", if has_server { "Server settings…" } else { "Set up a server…" }).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_server_dialog(window, cx))))
            .into_any_element();
        let mut server_rows = vec![row(status, detail, Some(controls))];
        if has_server {
            server_rows.push(row(format!("Sessions on {} in sync on the server", platform::this_computer()), self.mirror_status().unwrap_or_else(|| "Synced as they change, so the iPhone sees them".into()), None));
            server_rows.push(coming(&format!("Keep {}'s sessions in sync on the server", platform::this_computer()), "Always on for now", Some(true)));
        }
        let mut out = vec![group("Server", "", server_rows)];
        if connected {
            let mut phones: Vec<Div> = self
                .phones
                .clone()
                .unwrap_or_default()
                .into_iter()
                .enumerate()
                .map(|(i, phone)| {
                    let key = phone.key.clone();
                    let short = phone.key.split_whitespace().nth(1).map(|k| format!("…{}", &k[k.len().saturating_sub(10)..])).unwrap_or_default();
                    row(
                        phone.name.clone(),
                        format!("Paired · key {short}"),
                        Some(ui::button(SharedString::from(format!("phone-remove-{i}")), "Remove").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.phone_remove(key.clone(), cx))).into_any_element()),
                    )
                })
                .collect();
            phones.push(row(
                "Pair an iPhone",
                "Shows a code to scan with Glasscode for iPhone",
                Some(
                    ui::primary_button("settings-pair", "Pair an iPhone…")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.open_server_dialog(window, cx);
                            this.start_pairing(cx);
                        }))
                        .into_any_element(),
                ),
            ));
            out.push(group("iPhone", "Phones that reach your server", phones));
        }
        // TODO(glasscode): removed the hosted "Devices" account section and the off-LAN relay
        // host. The glasses and phone reach this Mac over LAN / Tailscale only.
        out.push(group(
            "Glasscode",
            "The glasses and phone voice copilot",
            vec![row(
                "Reach Glasscode on your network",
                "Glasscode reaches this Mac over the same Wi-Fi or Tailscale, through the voice server on port 4517.".to_string(),
                None,
            )],
        ));
        out
    }

    fn settings_updates(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let build = glasscode_core::update::describe(glasscode_core::update::VERSION, glasscode_core::update::BUILD);
        let update = match (&self.update.ready, &self.update.error) {
            (Some((build, version, _)), _) => row(
                format!("Ready: {}", glasscode_core::update::describe(version, *build)),
                "Downloaded and checked. It goes in when Glasscode quits, or now:",
                Some(ui::primary_button("settings-restart", "Restart to update").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.update_restart(cx))).into_any_element()),
            ),
            (None, Some(err)) => row(
                "Could not check for updates",
                err.clone(),
                Some(ui::button("settings-retry", "Retry").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.update_check_now(cx))).into_any_element()),
            ),
            (None, None) if self.update.checking => row("Checking for updates…", "", None),
            (None, None) => row(
                "Up to date",
                "Glasscode checks by itself every few hours; releases are signed",
                Some(ui::button("settings-check", "Check now").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.update_check_now(cx))).into_any_element()),
            ),
        };
        let channel = row(
            "Channel",
            "Beta gets new builds first",
            Some(self.choice("channel", vec![("stable", "Stable"), ("beta", "Beta")], if self.state.prefs.update_channel() == "beta" { "beta" } else { "stable" }, |s, c| s.prefs.update_channel = (c == "beta").then(|| c.to_string()), cx)),
        );
        let on_quit = self.switch_row("install-on-quit", "Install updates when Glasscode quits", "Or right away with Restart to update", !self.state.prefs.install_on_quit_off, |s| s.prefs.install_on_quit_off = !s.prefs.install_on_quit_off, cx);
        let cli: Vec<Div> = crate::dialogs::cli_rows(self, cx).into_iter().map(|r| div().px_4().py_3().child(r)).collect();
        let mut out = vec![
            group("Glasscode", "", vec![row("This build", "", Some(mono(build))), update, channel, on_quit]),
            group("CLI versions", "Claude Code and Codex are built in; keep them current here, and add any other CLI you use (from npm, or your own install command). New and reopened sessions pick up a change; running ones keep theirs.", cli),
        ];
        // The install runs in a terminal; show it here (it lives on the accounts form)
        // so clicking "Update to…" gives visible progress and can answer any prompt.
        if let Some(form) = self.settings.form.as_ref() {
            out.extend(crate::dialogs::accounts_terminal(form, cx));
        }
        out
    }

    fn settings_shortcuts(&self) -> Vec<AnyElement> {
        use crate::keymap::label;
        use crate::workspace::{AddProject, CloseSession, Handoff, NewSession, NextSession, OpenAccounts, OpenPalette, PreviousSession, Quit, ToggleRightPanel};
        let paste = label(&crate::composer::Paste);
        let mut keys = vec![
            ("Search and commands", label(&OpenPalette)),
            ("New session", label(&NewSession)),
            ("Close session", label(&CloseSession)),
            ("Next session", label(&NextSession)),
            ("Previous session", label(&PreviousSession)),
            ("Settings", label(&OpenAccounts)),
            ("Add a project", label(&AddProject)),
            ("Hand off", label(&Handoff)),
            ("Show or hide the side panel", label(&ToggleRightPanel)),
            ("Paste text, an image or files", paste.clone()),
        ];
        // Windows leaves Ctrl+C and Ctrl+V to the program in the terminal; it copies and pastes there with other keys.
        let terminal_paste = label(&crate::terminal::Paste);
        if terminal_paste != paste {
            keys.push(("Copy in the terminal", label(&crate::terminal::Copy)));
            keys.push(("Paste in the terminal", terminal_paste));
        }
        keys.push(("Clear the terminal", label(&crate::terminal::ClearScrollback)));
        keys.push(("Quit", label(&Quit)));
        vec![group("Keyboard", "", keys.into_iter().map(|(label, key)| row(label, "", Some(mono(key)))).collect())]
    }
}
