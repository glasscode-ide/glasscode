//! GitHub accounts: the accounts `gh` is signed in to, the default every repository uses, the
//! one a project uses instead (its ⋯ menu), and each account's commit email (Accounts). A
//! session gets its project's account when it starts; Glasscode's own GitHub calls follow the
//! same choice (`glasscode_core::github_accounts`).

use std::collections::HashMap;

use gpui::{AnyElement, ClickEvent, Context, FontWeight, SharedString, Window, div, prelude::*, px};
use glasscode_core::github_accounts::{self, GhAccount};

use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

#[derive(Default)]
pub struct GithubAccounts {
    /// What `gh` is signed in to; `None` until first read.
    pub list: Option<Vec<GhAccount>>,
    pub error: Option<String>,
    pub loading: bool,
    /// The account whose commit email is being edited in Accounts.
    pub editing: Option<String>,
}

impl Workspace {
    /// The account a project's sessions use: its own, else the default.
    pub fn github_account_for(&self, project_id: &str) -> Option<String> {
        self.state.project(project_id).and_then(|p| p.github_account.clone()).or_else(|| self.state.github.default.clone())
    }

    /// The environment that makes a new session in `project_id` act as its GitHub account.
    pub fn github_env(&self, project_id: &str) -> Vec<(String, String)> {
        self.github_account_for(project_id).map(|login| github_accounts::env_for(&login)).unwrap_or_default()
    }

    /// Tell the core who works where (projects' repositories → accounts). A project's repository
    /// is git's answer (two runs of git, and `gh` is looked for on the PATH), so this runs in the
    /// background, not in the click that changed a GitHub setting; only projects with an account
    /// of their own are asked. The newest call wins: an older one that ends later is dropped.
    pub fn configure_github(&self, cx: &mut Context<Self>) {
        static NEWEST: std::sync::Mutex<u64> = std::sync::Mutex::new(0);
        let call = {
            let mut newest = NEWEST.lock().unwrap_or_else(|e| e.into_inner());
            *newest += 1;
            *newest
        };
        let own: Vec<(std::path::PathBuf, String)> = self.state.projects.iter().filter_map(|p| Some((p.root.clone(), p.github_account.clone()?))).collect();
        let (settings, env) = (self.state.github.clone(), self.env.clone());
        cx.background_spawn(async move {
            let by_repo: HashMap<String, String> = own.into_iter().filter_map(|(root, login)| Some((glasscode_core::github::repo_slug(&root)?.to_lowercase(), login))).collect();
            let newest = NEWEST.lock().unwrap_or_else(|e| e.into_inner());
            if *newest == call {
                github_accounts::configure(&settings, by_repo, &env);
            }
        })
        .detach();
    }

    /// Read `gh`'s accounts again (at start, and when Accounts opens). The first time, the
    /// default becomes `gh`'s active account, and the others get GitHub's private commit address.
    pub fn github_accounts_refresh(&mut self, cx: &mut Context<Self>) {
        if self.github.loading {
            return;
        }
        self.github.loading = true;
        let env = self.env.clone();
        let task = cx.background_spawn(async move { github_accounts::accounts(&env) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.github.loading = false;
                match result {
                    Ok(list) => {
                        this.github.error = None;
                        if this.state.github.default.is_none() {
                            if let Some(active) = list.iter().find(|a| a.active) {
                                this.state.github.default = Some(active.login.clone());
                                this.save();
                                this.configure_github(cx);
                            }
                        }
                        let missing: Vec<String> = list
                            .iter()
                            .map(|a| a.login.clone())
                            .filter(|l| Some(l) != this.state.github.default.as_ref() && !this.state.github.emails.contains_key(l))
                            .collect();
                        this.github.list = Some(list);
                        if !missing.is_empty() {
                            this.github_fill_emails(missing, cx);
                        }
                    }
                    Err(error) => this.github.error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// GitHub's private address (`<id>+<login>@users.noreply.github.com`) for accounts with no
    /// commit email yet, so their commits are credited to them.
    fn github_fill_emails(&mut self, logins: Vec<String>, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move { logins.into_iter().filter_map(|l| Some((l.clone(), github_accounts::noreply_email(&l)?))).collect::<Vec<_>>() });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |this, cx| {
                for (login, email) in found {
                    this.state.github.emails.entry(login).or_insert(email);
                }
                this.save();
                this.configure_github(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub fn set_github_default(&mut self, login: String, cx: &mut Context<Self>) {
        self.state.github.default = Some(login.clone());
        // A project that named the new default no longer needs to.
        for project in &mut self.state.projects {
            if project.github_account.as_deref() == Some(login.as_str()) {
                project.github_account = None;
            }
        }
        self.save();
        self.configure_github(cx);
        self.show_toast(format!("GitHub: {login} is the default. New sessions use it; running ones keep theirs until reopened."), cx);
    }

    pub fn set_project_github(&mut self, project_id: &str, login: Option<String>, cx: &mut Context<Self>) {
        let default = self.state.github.default.clone();
        let Some(project) = self.state.projects.iter_mut().find(|p| p.id == project_id) else { return };
        project.github_account = login.filter(|l| Some(l) != default.as_ref());
        let name = project.name.clone();
        let running = self.state.sessions.iter().filter(|s| s.project_id == project_id && self.terminals.contains_key(&s.id)).count();
        self.save();
        self.configure_github(cx);
        let now = self.github_account_for(project_id).unwrap_or_else(|| "gh's active account".into());
        self.show_toast(
            if running > 0 {
                format!("{name} uses GitHub account {now} from now on. Its {running} running session(s) keep the old one until reopened.")
            } else {
                format!("{name} uses GitHub account {now}.")
            },
            cx,
        );
    }

    pub fn save_github_email(&mut self, login: &str, email: String, cx: &mut Context<Self>) {
        let email = email.trim().to_string();
        if email.is_empty() {
            self.state.github.emails.remove(login);
        } else {
            self.state.github.emails.insert(login.to_string(), email);
        }
        self.github.editing = None;
        self.save();
        self.configure_github(cx);
        cx.notify();
    }

    /// The project ⋯ menu's GitHub account choices.
    pub fn github_menu_items(&self, project_id: &str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(list) = self.github.list.as_ref().filter(|l| l.len() > 1) else { return Vec::new() };
        let chosen = self.github_account_for(project_id);
        let default = self.state.github.default.clone();
        let mut items = vec![div().h(px(1.)).my_1().bg(theme::border()).into_any_element(), div().px_2p5().pt_1().pb_0p5().child(ui::caps_label("GitHub account")).into_any_element()];
        for (i, account) in list.iter().enumerate() {
            let login = account.login.clone();
            let selected = chosen.as_deref() == Some(login.as_str());
            let is_default = default.as_deref() == Some(login.as_str());
            let project_id = project_id.to_string();
            items.push(
                div()
                    .id(("menu-project-github", i))
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(theme::sz(28.))
                    .px_2p5()
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(theme::sz(12.5))
                    .hover(|s| s.bg(theme::hover()))
                    .child(div().w(theme::sz(12.)).text_color(theme::accent()).child(if selected { "✓" } else { "" }))
                    .child(div().text_color(theme::text()).child(login.clone()))
                    .when(is_default, |d| d.child(div().text_size(theme::sz(11.)).text_color(theme::muted()).child("default")))
                    .when(!account.ok, |d| d.child(div().text_size(theme::sz(11.)).text_color(theme::removed()).child("signed out")))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.row_menu = None;
                        this.set_project_github(&project_id, Some(login.clone()), cx);
                    }))
                    .into_any_element(),
            );
        }
        items
    }
}

/// The GitHub accounts card of Settings › Agents and accounts.
pub fn render_github_section(ws: &Workspace, email: &gpui::Entity<crate::text_input::TextInput>, _window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    use crate::settings_ui::{card, row, row_with};
    let mut rows: Vec<gpui::Div> = Vec::new();
    let default = ws.state.github.default.clone();
    for (i, account) in ws.github.list.clone().unwrap_or_default().into_iter().enumerate() {
        let login = account.login.clone();
        let is_default = default.as_deref() == Some(login.as_str());
        let projects: Vec<String> = ws.state.projects.iter().filter(|p| p.github_account.as_deref() == Some(login.as_str())).map(|p| p.name.clone()).collect();
        let uses = if is_default {
            let others = ws.state.projects.iter().filter(|p| p.github_account.is_some()).count();
            if others == 0 { "every project".to_string() } else { format!("every project but {others}") }
        } else if projects.is_empty() {
            "no project yet (choose it in Git and projects)".to_string()
        } else {
            projects.join(", ")
        };
        let commit_email = ws.state.github.emails.get(&login).cloned();
        let editing = ws.github.editing.as_deref() == Some(login.as_str());
        let (l1, l2, l3, l4) = (login.clone(), login.clone(), login.clone(), login.clone());
        let title = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(div().font_family(theme::MONO_FONT).font_weight(FontWeight::MEDIUM).text_color(theme::text()).child(login.clone()))
            // On glass a badge's words are `text`, not the accent (Amber on light glass is faint).
            .when(is_default, |d| d.child(ui::badge("Default for every repo", theme::text())))
            // Red words are too faint on glass: a red dot, and the words in a quiet badge.
            .when(!account.ok, |d| d.child(ui::dot(theme::removed())).child(ui::badge("Signed out in gh", theme::text_soft())));
        let buttons = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .when(!is_default, |d| {
                d.child(ui::button(("github-default", i), "Make default").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_github_default(l1.clone(), cx))))
            })
            .when(!editing, |d| {
                d.child(ui::button(("github-email", i), "Commit email…").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.github.editing = Some(l2.clone());
                    if let Some(form) = this.settings.form.as_ref() {
                        let current = this.state.github.emails.get(&l2).cloned().unwrap_or_default();
                        form.github_email.update(cx, |input, cx| input.set_text(current, cx));
                        crate::text_input::focus(&form.github_email, window, cx);
                    }
                    cx.notify();
                })))
            })
            .into_any_element();
        let detail = format!("Commits as {} · {uses}", commit_email.clone().unwrap_or_else(|| "your git email (git config user.email)".into()));
        rows.push(row_with(title, detail, Some(buttons)).when(editing, |d| {
            d.child(
                div()
                    .w_full()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(theme::sz(8.))
                    .child(div().flex_1().min_w(theme::sz(220.)).child(email.clone()))
                    .child(ui::primary_button(("github-email-save", i), "Save").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let Some(text) = this.settings.form.as_ref().map(|form| form.github_email.read(cx).text()) else { return };
                        this.save_github_email(&l3, text, cx);
                    })))
                    .child(ui::button(("github-email-cancel", i), "Cancel").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let _ = &l4;
                        this.github.editing = None;
                        cx.notify();
                    }))),
            )
        }));
    }
    let status = match (&ws.github.list, &ws.github.error) {
        (_, Some(error)) => Some(error.clone()),
        (None, None) => Some("Reading gh's accounts…".into()),
        (Some(list), None) if list.is_empty() => Some("gh is not signed in to GitHub.".into()),
        _ => None,
    };
    if let Some(status) = status {
        rows.push(row(status, "", None));
    }
    rows.push(row(
        "Add a GitHub account",
        "Runs gh auth login in a terminal here",
        Some(
            ui::button("github-add", "Add…")
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.run_in_accounts_terminal("gh auth login --hostname github.com --git-protocol https --web", window, cx);
                }))
                .into_any_element(),
        ),
    ));
    let signed_in = match ws.github.list.as_ref().map_or(0, Vec::len) {
        1 => "1 account in gh".to_string(),
        n => format!("{n} accounts in gh"),
    };
    card("GitHub accounts", signed_in)
        .rows(rows)
        .footer("Sessions push, open pull requests and run gh as their project's account: the default, unless Git and projects picks another. Glasscode takes the account's token from gh when a session starts and never stores it. Leave the commit email empty to use your git email.")
        .render()
}

/// What the status bar says about the active session's GitHub account.
pub fn status_label(ws: &Workspace, project_id: &str) -> Option<SharedString> {
    ws.github_account_for(project_id).map(|login| format!("GitHub: {login}").into())
}
