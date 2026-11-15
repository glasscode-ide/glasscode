//! The developer's server: glasscoded over SSH (usually Tailscale). Connecting and
//! installing, the Server dialog (checks, server accounts, projects), server sessions in the
//! sidebar, and their terminals in the center.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AnyElement, ClickEvent, Context, Entity, FontWeight, Hsla, SharedString, Window, div, prelude::*, px};
use glasscode_core::agents::AgentActivity;
use glasscode_core::model::AgentKind;
use glasscode_core::remote::{self, Attach, Check, CheckLevel, CreateSession, RemoteClient, RemoteSession, ServerConfig, ServerState};
use serde_json::{Value, json};

use crate::dialogs::Modal;
use crate::center::Center;
use crate::sidebar::{Row, RowState, session_row, without_number};
use crate::terminal::{SpawnSpec, TerminalEvent, TerminalView};
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

#[derive(Clone, Debug, PartialEq)]
pub enum ServerStatus {
    Off,
    Connecting,
    Connected,
    Failed(String),
}

pub struct ServerUi {
    pub config: Option<ServerConfig>,
    pub client: Option<Arc<RemoteClient>>,
    pub status: ServerStatus,
    pub state: Option<ServerState>,
    pub checks: Option<Vec<Check>>,
    /// A long action in progress ("Installing glasscoded…").
    pub busy: Option<String>,
    /// Output of the last install, or its error.
    pub output: Option<String>,
    /// Terminals attached to server sessions, by session id.
    pub terminals: HashMap<String, Entity<TerminalView>>,
    last_poll: Instant,
    last_attempt: Instant,
    refreshing: bool,
    /// glasscoded was brought up to this app's build once this run (never in a loop).
    auto_updated: bool,
}

impl ServerUi {
    pub fn new() -> Self {
        let long_ago = Instant::now() - Duration::from_secs(3600);
        ServerUi {
            config: ServerConfig::load(),
            client: None,
            status: ServerStatus::Off,
            state: None,
            checks: None,
            busy: None,
            output: None,
            terminals: HashMap::new(),
            last_poll: long_ago,
            last_attempt: long_ago,
            refreshing: false,
            auto_updated: false,
        }
    }

    pub fn name(&self) -> String {
        self.config.as_ref().map(ServerConfig::name).unwrap_or_else(|| "server".into())
    }

    pub fn connected(&self) -> bool {
        self.status == ServerStatus::Connected && self.client.is_some()
    }

    pub fn session(&self, id: &str) -> Option<&RemoteSession> {
        self.state.as_ref()?.sessions.iter().find(|s| s.session.id == id)
    }
}

/// The Server dialog's own inputs and its embedded sign-in terminal.
pub struct ServerForm {
    pub target: Entity<TextInput>,
    /// The code `glasscoded login` shows, to add a server through a relay.
    pub relay_code: Entity<TextInput>,
    pub login: Option<(String, Entity<TerminalView>)>,
    pub new_account: Option<AgentKind>,
    pub account_label: Entity<TextInput>,
}

/// Where the Linux builds of glasscoded are: inside the app bundle, or `target/glasscoded`
/// next to a development build.
pub fn binaries_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let bundled = exe.parent().and_then(|p| p.parent()).map(|c| c.join("Resources/glasscoded"));
    if let Some(dir) = bundled.filter(|d| d.is_dir()) {
        return dir;
    }
    exe.parent().and_then(|p| p.parent()).map(|t| t.join("glasscoded")).unwrap_or_default()
}

pub fn activity_label(session: &RemoteSession) -> (&'static str, Hsla) {
    match (session.running, session.activity) {
        (false, _) => ("stopped", theme::faint()),
        (true, Some(AgentActivity::Working)) => ("working", theme::added()),
        (true, Some(AgentActivity::NeedsInput)) => ("needs you", theme::attention()),
        (true, _) if session.session.agent == AgentKind::Shell => ("shell", theme::muted()),
        (true, _) => ("idle", theme::muted()),
    }
}

impl Workspace {
    // ----- connection ----------------------------------------------------------------------

    /// At startup: connect to the configured server, if there is one.
    pub fn server_start(&mut self, cx: &mut Context<Self>) {
        if self.server.config.is_some() {
            self.server_connect(cx);
        }
    }

    pub fn server_connect(&mut self, cx: &mut Context<Self>) {
        let Some(config) = self.server.config.clone() else { return };
        if self.server.status == ServerStatus::Connecting {
            return;
        }
        self.server.status = ServerStatus::Connecting;
        self.server.last_attempt = Instant::now();
        let task = cx.background_spawn(async move { RemoteClient::connect(&config) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(client) => {
                        // A server behind this app gets the glasscoded it brings (a server that
                        // updated itself past it is left alone).
                        let behind = glasscode_core::update::BUILD != 0
                            && client.hello.build < glasscode_core::update::BUILD
                            && binaries_dir().join(format!("glasscoded-linux-{}", if client.hello.arch == "aarch64" { "aarch64" } else { "x86_64" })).is_file();
                        this.server.client = Some(Arc::new(client));
                        this.server.status = ServerStatus::Connected;
                        if let Some(config) = &this.server.config {
                            let _ = config.save();
                        }
                        this.server_refresh(cx);
                        this.server_doctor(cx);
                        if behind && !this.server.auto_updated {
                            this.server.auto_updated = true;
                            let name = this.server.name();
                            this.show_toast(format!("Updating glasscoded on {name} to match {}; sessions keep running", crate::platform::this_computer()), cx);
                            this.server_install(cx);
                        }
                    }
                    Err(err) => {
                        this.server.client = None;
                        this.server.status = ServerStatus::Failed(format!("{err:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub fn server_disconnect(&mut self, forget: bool, cx: &mut Context<Self>) {
        self.server.client = None;
        self.server.state = None;
        self.server.checks = None;
        self.server.terminals.clear();
        self.server.status = ServerStatus::Off;
        if forget {
            ServerConfig::remove();
            self.server.config = None;
        }
        if matches!(self.center, Center::Server(_)) {
            self.center = Center::Session;
        }
        cx.notify();
    }

    /// Every few seconds while connected; reconnect quietly after a drop (sleep, Wi-Fi).
    pub fn server_tick(&mut self, cx: &mut Context<Self>) {
        match &self.server.status {
            ServerStatus::Connected if self.server.last_poll.elapsed() > Duration::from_secs(3) => self.server_refresh(cx),
            ServerStatus::Failed(_) if self.server.last_attempt.elapsed() > Duration::from_secs(20) => self.server_connect(cx),
            _ => {}
        }
    }

    pub fn server_refresh(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.server.client.clone() else { return };
        if self.server.refreshing {
            return;
        }
        self.server.refreshing = true;
        self.server.last_poll = Instant::now();
        let task = cx.background_spawn(async move { (client.call::<ServerState>("state", json!({})), client.is_alive()) });
        cx.spawn(async move |this, cx| {
            let (result, alive) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.server.refreshing = false;
                match result {
                    Ok(state) => this.server.state = Some(state),
                    Err(err) if !alive => {
                        this.server.client = None;
                        this.server.status = ServerStatus::Failed(format!("connection lost: {err}"));
                        this.server.last_attempt = Instant::now();
                    }
                    Err(_) => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn server_doctor(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.server.client.clone() else { return };
        let task = cx.background_spawn(async move { client.call::<Vec<Check>>("doctor", json!({})) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(checks) = result {
                    this.server.checks = Some(checks);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Call the daemon in the background; `then` runs on the UI thread with the answer.
    pub fn server_call(
        &mut self,
        method: &'static str,
        params: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Workspace, anyhow::Result<Value>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some(client) = self.server.client.clone() else {
            self.show_toast(format!("Not connected to {}.", self.server.name()), cx);
            return;
        };
        let task = cx.background_spawn(async move { client.call_value(method, params) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                then(this, result, window, cx);
                this.server_refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub fn server_install(&mut self, cx: &mut Context<Self>) {
        let Some(config) = self.server.config.clone() else { return };
        self.server.busy = Some("Installing glasscoded on the server…".into());
        self.server.output = None;
        let dir = binaries_dir();
        let task = cx.background_spawn(async move { remote::install_on_server(&config, &dir) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.server.busy = None;
                match result {
                    Ok(output) => {
                        this.server.output = Some(output.trim().to_string());
                        // Reconnect to the new version once it has taken over (a few seconds).
                        this.server.client = None;
                        this.server.status = ServerStatus::Connecting;
                        cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(Duration::from_secs(4)).await;
                            let _ = this.update(cx, |this, cx| {
                                this.server.status = ServerStatus::Off;
                                this.server_connect(cx);
                            });
                        })
                        .detach();
                    }
                    Err(err) => this.server.output = Some(format!("Install failed: {err:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    // ----- sessions ------------------------------------------------------------------------

    /// A local terminal that shows a server session (`ssh -tt server tmux attach …`).
    fn server_terminal(&mut self, key: String, attach: &Attach, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<TerminalView>> {
        let config = self.server.config.clone()?;
        let (program, args) = config.attach_command(attach);
        let spec = SpawnSpec { program, args, cwd: glasscode_core::paths::home(), env: (*self.env).clone() };
        match TerminalView::spawn(spec, cx) {
            Ok(terminal) => {
                // Pasted and dropped files are copied to the server for its agent.
                terminal.update(cx, |t, _| t.files = crate::terminal::files::FileTarget::Server(config.clone()));
                let id = key.clone();
                let sub = cx.subscribe_in(&terminal, window, move |this, _, event, _window, cx| {
                    if let TerminalEvent::Notice(message) = event {
                        this.show_toast(message.clone(), cx);
                    }
                    if let TerminalEvent::Exited(_) = event {
                        // SSH ended (sleep, network, or the agent quit): reattach next time.
                        this.server.terminals.remove(&id);
                        this.server_refresh(cx);
                        cx.notify();
                    }
                });
                self.keep_subscription(sub);
                Some(terminal)
            }
            Err(err) => {
                self.show_toast(format!("Could not open the terminal: {err}"), cx);
                None
            }
        }
    }

    pub fn open_server_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.center = Center::Server(id.to_string());
        if let Some(terminal) = self.server.terminals.get(id).cloned() {
            let handle = terminal.read(cx).focus_handle_clone();
            window.focus(&handle, cx);
            cx.notify();
            return;
        }
        let session_id = id.to_string();
        self.server_call("sessions.attach", json!({ "session_id": id }), window, cx, move |this, result, window, cx| match result
            .and_then(|v| serde_json::from_value::<Attach>(v).map_err(Into::into))
        {
            Ok(attach) => {
                if let Some(terminal) = this.server_terminal(session_id.clone(), &attach, window, cx) {
                    let handle = terminal.read(cx).focus_handle_clone();
                    this.server.terminals.insert(session_id, terminal);
                    window.focus(&handle, cx);
                }
            }
            Err(err) => this.show_toast(format!("Could not open the session: {err:#}"), cx),
        });
        cx.notify();
    }

    pub fn close_server_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.server.terminals.remove(id);
        if self.center == Center::Server(id.to_string()) {
            self.center = Center::Session;
        }
        let title = self.server.session(id).map(|s| s.session.title.clone()).unwrap_or_default();
        let name = self.server.name();
        self.server_call("sessions.close", json!({ "session_id": id }), window, cx, move |this, result, _, cx| match result {
            Ok(_) => this.show_toast(format!("Closed “{title}” on {name}. New session there can resume the conversation."), cx),
            Err(err) => this.show_toast(format!("Could not close it: {err:#}"), cx),
        });
    }

    pub fn stop_server_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.server.terminals.remove(id);
        self.server_call("sessions.stop", json!({ "session_id": id }), window, cx, |_, _, _, _| {});
    }

    pub fn create_server_session(&mut self, create: CreateSession, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.server.name();
        self.server_call("sessions.create", serde_json::to_value(&create).unwrap_or_default(), window, cx, move |this, result, window, cx| {
            match result.and_then(|v| serde_json::from_value::<RemoteSession>(v).map_err(Into::into)) {
                Ok(session) => {
                    this.modal = None;
                    if let Some(state) = this.server.state.as_mut() {
                        state.sessions.push(session.clone());
                    }
                    this.open_server_session(&session.session.id, window, cx);
                }
                Err(err) => {
                    if let Some(Modal::NewSession(form)) = this.modal.as_mut() {
                        form.busy = false;
                        form.error = Some(format!("{name}: {err:#}"));
                    }
                }
            }
        });
    }

    /// Copy a local project to the server: clone its `origin` at the same place relative to
    /// the home folder (`~/Projects/myapp` here → `~/Projects/myapp` there).
    pub fn server_copy_project(&mut self, project_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.state.project(project_id).cloned() else { return };
        let Some(server_home) = self.server.client.as_ref().map(|c| c.hello.home.clone()) else { return };
        let url = glasscode_core::sys::command("git")
            .arg("-C")
            .arg(&project.root)
            .args(["remote", "get-url", "origin"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let Some(url) = url else {
            self.show_toast(format!("{} has no `origin` remote to clone from.", project.name), cx);
            return;
        };
        let home = glasscode_core::paths::home();
        let path = project.root.strip_prefix(&home).map(|rel| server_home.join(rel)).unwrap_or_else(|_| server_home.join("Projects").join(&project.name));
        self.server.busy = Some(format!("Cloning {} on {}…", project.name, self.server.name()));
        let name = project.name.clone();
        self.server_call("projects.add", json!({ "url": url, "path": path, "name": project.name }), window, cx, move |this, result, _, cx| {
            this.server.busy = None;
            match result {
                Ok(_) => this.show_toast(format!("{name} is on the server now."), cx),
                Err(err) => this.server.output = Some(format!("Could not clone {name}: {err:#}")),
            }
        });
    }

    pub fn server_login(&mut self, account_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let label = self
            .server
            .state
            .as_ref()
            .and_then(|s| s.accounts.iter().find(|a| a.id == account_id))
            .map(|a| format!("{} · {}", a.agent.name(), a.label))
            .unwrap_or_default();
        let key = format!("login-{account_id}");
        self.server_call("accounts.login", json!({ "account_id": account_id }), window, cx, move |this, result, window, cx| {
            match result.and_then(|v| serde_json::from_value::<Attach>(v).map_err(Into::into)) {
                Ok(attach) => {
                    if let Some(terminal) = this.server_terminal(key, &attach, window, cx) {
                        let handle = terminal.read(cx).focus_handle_clone();
                        if let Some(Modal::Server(form)) = this.modal.as_mut() {
                            form.login = Some((format!("Signing in {label} on the server"), terminal));
                        }
                        window.focus(&handle, cx);
                    }
                }
                Err(err) => this.show_toast(format!("Could not start the sign-in: {err:#}"), cx),
            }
        });
    }

    pub fn open_server_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = cx.new(|cx| {
            TextInput::new("SSH host, e.g. devbox or you@devbox.your-tailnet.ts.net", cx)
                .mono()
                .with_text(self.server.config.as_ref().filter(|c| c.relay.is_none()).map(|c| c.ssh_target.clone()).unwrap_or_else(|| "devbox".into()))
        });
        let relay_code = cx.new(|cx| TextInput::new("ABCD-EFGH", cx).mono());
        let account_label = cx.new(|cx| TextInput::new("Name, e.g. Work", cx));
        cx.subscribe_in(&target, window, |this: &mut Workspace, _, event, window, cx| match event {
            InputEvent::Submit => this.server_connect_from_form(cx),
            InputEvent::Cancel => this.close_modal(window, cx),
            InputEvent::Changed => {}
        })
        .detach();
        cx.subscribe_in(&account_label, window, |this: &mut Workspace, _, event, window, cx| match event {
            InputEvent::Submit => this.server_add_account_from_form(window, cx),
            InputEvent::Cancel => this.close_modal(window, cx),
            InputEvent::Changed => {}
        })
        .detach();
        crate::text_input::focus(&target, window, cx);
        self.modal = Some(Modal::Server(ServerForm { target, relay_code, login: None, new_account: None, account_label }));
        if self.server.connected() {
            self.server_doctor(cx);
        }
        cx.notify();
    }

    fn server_connect_from_form(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::Server(form)) = self.modal.as_ref() else { return };
        let target = form.target.read(cx).text().trim().to_string();
        if target.is_empty() {
            return;
        }
        let mut config = self.server.config.clone().filter(|c| c.relay.is_none()).unwrap_or_else(|| ServerConfig::new(target.clone()));
        config.ssh_target = target;
        self.server.config = Some(config);
        self.server.client = None;
        self.server.status = ServerStatus::Off;
        self.server_connect(cx);
    }

    fn server_add_account_from_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::Server(form)) = self.modal.as_mut() else { return };
        let Some(agent) = form.new_account else { return };
        let label = form.account_label.read(cx).text().trim().to_string();
        if label.is_empty() {
            return;
        }
        form.new_account = None;
        form.account_label.update(cx, |i, cx| i.set_text("", cx));
        self.server_call("accounts.add", json!({ "agent": agent, "label": label }), window, cx, |this, result, window, cx| match result {
            Ok(value) => {
                if let Some(id) = value.get("id").and_then(Value::as_str) {
                    let id = id.to_string();
                    this.server_login(&id, window, cx);
                }
            }
            Err(err) => this.show_toast(format!("Could not add the account: {err:#}"), cx),
        });
    }
}

// ----- rendering -------------------------------------------------------------------------------

impl Workspace {
    /// Title bar pill: the server's name and whether it is reachable.
    pub fn server_pill(&self, cx: &mut Context<Self>) -> AnyElement {
        let (color, label) = match (&self.server.config, &self.server.status) {
            (None, _) => (theme::faint(), "Server…".to_string()),
            (Some(_), ServerStatus::Connected) => (theme::added(), self.server.name()),
            (Some(_), ServerStatus::Connecting) => (theme::accent(), format!("{} · connecting", self.server.name())),
            (Some(_), ServerStatus::Failed(_)) => (theme::removed(), format!("{} · offline", self.server.name())),
            (Some(_), ServerStatus::Off) => (theme::faint(), self.server.name()),
        };
        div()
            .id("server-pill")
            .flex()
            .items_center()
            .gap_1p5()
            .h(px(30.))
            .px_2p5()
            .rounded_md()
            .border_1()
            .border_color(theme::border_strong())
            .text_size(px(12.))
            .text_color(theme::text_soft())
            .cursor_pointer()
            .hover(|s| s.bg(theme::hover()))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_server_dialog(window, cx)))
            .child(div().size(px(7.)).rounded_full().bg(color))
            .child(label)
            .into_any_element()
    }

    /// The server's projects and sessions, below the local projects.
    pub fn render_server_sidebar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.server.config.as_ref()?;
        let name = self.server.name();
        let (dot, note) = match &self.server.status {
            ServerStatus::Connected => (theme::added(), None),
            ServerStatus::Connecting => (theme::accent(), Some("Connecting…".to_string())),
            ServerStatus::Failed(err) => (theme::removed(), Some(format!("Offline: {}", err.lines().next().unwrap_or_default()))),
            ServerStatus::Off => (theme::faint(), Some("Not connected".to_string())),
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(note) = note {
            rows.push(
                div()
                    .id("server-note")
                    .px_2()
                    .py_1()
                    .text_size(px(11.5))
                    .text_color(theme::muted())
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_server_dialog(window, cx)))
                    .child(note)
                    .into_any_element(),
            );
        }
        if let Some(state) = self.server.state.as_ref().filter(|_| self.server.connected()) {
            if state.projects.is_empty() {
                rows.push(
                    div()
                        .id("server-empty")
                        .px_2()
                        .py_1()
                        .text_size(px(11.5))
                        .text_color(theme::muted())
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_server_dialog(window, cx)))
                        .child("No projects on the server yet. Copy one in the Server settings.")
                        .into_any_element(),
                );
            }
            for project in &state.projects {
                let project_id = project.id.clone();
                rows.push(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .h(px(26.))
                        .px_1p5()
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_soft()).child(project.name.clone()))
                        .child(ui::icon_button(SharedString::from(format!("server-add-{}", project.id)), "+").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.open_new_server_session(Some(project_id.clone()), None, window, cx)
                        })))
                        .into_any_element(),
                );
                for remote in state.sessions.iter().filter(|s| s.session.project_id == project.id) {
                    let id = remote.session.id.clone();
                    let close_id = id.clone();
                    let state = match (remote.running, remote.activity) {
                        (true, Some(AgentActivity::NeedsInput)) => RowState::NeedsYou,
                        (true, Some(AgentActivity::Working)) => RowState::Working,
                        _ => RowState::Idle,
                    };
                    let number = remote.session.review.as_ref().map(|r| r.number);
                    let title = match &remote.session.review {
                        Some(review) if !review.title.is_empty() => glasscode_core::naming::tidy_title(&review.title, 70),
                        _ => without_number(&remote.session.title, number),
                    };
                    let mut sub = if remote.session.review.is_some() { "Review".to_string() } else { format!("⎇ {}", remote.session.branch) };
                    if !remote.running {
                        sub.push_str(" · stopped");
                    }
                    let row = Row {
                        key: format!("server-{id}"),
                        title,
                        agent: remote.session.agent,
                        running: remote.running,
                        selected: self.center == Center::Server(id.clone()),
                        state,
                        sub,
                        tag: remote.session.review.is_some().then_some("Review"),
                        pr: number.map(|n| (format!("#{n}"), theme::muted())),
                        count: 1,
                        pinned: false,
                        kids: Vec::new(),
                    };
                    rows.push(session_row(
                        row,
                        Box::new(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_server_session(&id, window, cx))),
                        Some(Box::new(cx.listener(move |this, _: &ClickEvent, window, cx| this.close_server_session(&close_id, window, cx)))),
                        None,
                    ));
                }
            }
        }
        Some(
            div()
                .mt_2()
                .pt_2()
                .border_t_1()
                .border_color(theme::border())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .px_1p5()
                        .pb_1()
                        .child(div().size(px(7.)).rounded_full().bg(dot))
                        .child(ui::caps_label(format!("ON {} · KEEPS RUNNING", name.to_uppercase())))
                        .child(div().flex_1())
                        .child(ui::icon_button("server-new", "+").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_new_server_session(None, None, window, cx)))),
                )
                .children(rows)
                .into_any_element(),
        )
    }

    /// New session dialog with "Run on: server" already chosen.
    pub fn open_new_server_session(&mut self, project_id: Option<String>, worktree: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.server.connected() {
            self.open_server_dialog(window, cx);
            return;
        }
        let project = project_id.or_else(|| self.server.state.as_ref().and_then(|s| s.projects.first().map(|p| p.id.clone())));
        let Some(project) = project else {
            self.open_server_dialog(window, cx);
            return;
        };
        self.modal = Some(Modal::new_session(self, project, worktree, window, cx));
        if let Some(Modal::NewSession(form)) = self.modal.as_mut() {
            form.on_server = true;
            form.account_id = self.server.state.as_ref().and_then(|s| s.defaults.claude.clone());
        }
        self.load_resume_list(cx);
        cx.notify();
    }

    pub fn render_server_center(&mut self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let name = self.server.name();
        let Some(remote) = self.server.session(id).cloned() else {
            return div().flex().flex_1().items_center().justify_center().text_color(theme::muted()).child(format!("This session is no longer on {name}.")).into_any_element();
        };
        let account = self
            .server
            .state
            .as_ref()
            .and_then(|s| remote.session.account_id.as_ref().and_then(|a| s.accounts.iter().find(|x| &x.id == a)))
            .map(|a| a.display());
        let (status, color) = activity_label(&remote);
        let terminal = self.server.terminals.get(id).cloned();
        let (stop_id, close_id, open_id) = (id.to_string(), id.to_string(), id.to_string());
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .px_3p5()
            .py_2()
            .bg(theme::header())
            .border_b_1()
            .border_color(theme::border())
            .child(div().flex().flex_none().items_center().gap_2().font_weight(FontWeight::SEMIBOLD).child(ui::dot(theme::agent_color(remote.session.agent))).child(remote.session.agent.name()))
            .when_some(account, |d, a| d.child(div().flex_none().text_color(theme::text_soft()).child(a)))
            .child(div().min_w(px(60.)).overflow_hidden().whitespace_nowrap().text_ellipsis().font_family(theme::MONO_FONT).text_size(px(12.)).text_color(theme::text_soft()).child(format!("⎇ {}", remote.session.branch)))
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .bg(theme::success_bg())
                    .text_size(px(11.5))
                    .text_color(theme::added())
                    .child(format!("on {name} · keeps running when {} sleeps", crate::platform::this_computer())),
            )
            .child(div().flex_none().text_size(px(11.5)).text_color(color).child(format!("● {status}")))
            .when(remote.signed_out, |d| {
                let account = remote.session.account_id.clone();
                d.child(div().flex_none().text_size(px(11.5)).text_color(theme::removed()).child(format!("{} is signed out on {name}", remote.session.agent.name())))
                    .when_some(account, |d, account| {
                        d.child(ui::primary_button("server-relogin", "Sign in again").flex_none().on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            // The sign-in terminal shows in the Server dialog.
                            this.open_server_dialog(window, cx);
                            this.server_login(&account, window, cx);
                        })))
                    })
            })
            .child(div().flex_1())
            .child({
                let id = id.to_string();
                ui::button("server-to-mac", format!("⇣ Bring to {}", crate::platform::this_computer())).flex_none().on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.move_one(crate::switch_ui::Direction::ToLaptop, id.clone(), cx)))
            })
            .when(remote.running, |d| {
                d.child(ui::button("server-stop", "Stop").flex_none().on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.stop_server_session(&stop_id, window, cx))))
            })
            .child(ui::button("server-close", "Close").flex_none().on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.close_server_session(&close_id, window, cx))));
        let body = match terminal {
            Some(terminal) => div().flex_1().min_h_0().child(terminal).into_any_element(),
            None => div()
                .flex()
                .flex_col()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(remote.session.title.clone()))
                .child(div().text_color(theme::muted()).child(if remote.running {
                    format!("Running on {name}. Open its terminal here; closing it leaves the session running.")
                } else {
                    format!("Stopped on {name}. Opening it resumes the conversation.")
                }))
                .child(ui::primary_button("server-open", if remote.running { "Open terminal" } else { "Resume" }).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_server_session(&open_id, window, cx))))
                .into_any_element(),
        };
        div().flex().flex_col().flex_1().min_w_0().h_full().bg(theme::bg()).child(header).child(body).into_any_element()
    }
}

pub fn render_server_dialog(ws: &Workspace, form: &ServerForm, cx: &mut Context<Workspace>) -> AnyElement {
    let server = &ws.server;
    let section = |title: &str| div().flex().flex_col().gap_2().child(ui::caps_label(title.to_uppercase()));
    let mut body: Vec<AnyElement> = Vec::new();

    // Where the server is.
    let status_line = match &server.status {
        ServerStatus::Connected => server.client.as_ref().map(|c| {
            let h = &c.hello;
            format!("Connected to {} ({} {}, user {}) · glasscoded {}", h.host, h.os, h.arch, h.user, glasscode_core::update::describe(&h.version, h.build))
        }),
        ServerStatus::Connecting => Some("Connecting…".into()),
        ServerStatus::Failed(err) => Some(format!("Could not connect: {err}")),
        ServerStatus::Off => None,
    };
    let failed = matches!(server.status, ServerStatus::Failed(_));
    let through_relay = server.config.as_ref().and_then(|c| c.relay.clone());
    body.push(
        section("Server")
            .child(match &through_relay {
                Some(route) => div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(format!("{} through the relay", route.name)))
                    .child(ui::primary_button("server-connect", if server.connected() { "Reconnect" } else { "Connect" }).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.server.client = None;
                        this.server.status = ServerStatus::Off;
                        this.server_connect(cx);
                    })))
                    .child(ui::button("server-use-ssh", "Use SSH instead").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.server_disconnect(true, cx))))
                    .into_any_element(),
                None => div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(form.target.clone()))
                    .child(ui::primary_button("server-connect", if server.connected() { "Reconnect" } else { "Connect" }).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.server_connect_from_form(cx))))
                    .into_any_element(),
            })
            .child(div().text_size(px(12.)).text_color(theme::muted()).child(if through_relay.is_some() {
                format!("Your own always-on machine (Linux), reached through a self-hosted relay from anywhere. Sessions there keep running when {} is closed.", crate::platform::this_computer())
            } else {
                format!("Your own always-on machine (Linux), reached with your SSH setup — a host from ~/.ssh/config or a Tailscale name. Sessions there keep running when {} is closed.", crate::platform::this_computer())
            }))
            .when_some(status_line, |d, line| d.child(div().text_size(px(12.5)).text_color(if failed { theme::removed() } else { theme::text() }).child(line)))
            .when_some(ws.mirror_status().filter(|_| server.connected()), |d, line| d.child(div().text_size(px(12.)).text_color(theme::muted()).child(line)))
            .when(failed || server.connected(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(
                            ui::button("server-install", if server.connected() { "Update glasscoded" } else { "Install glasscoded on the server" })
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.server_install(cx))),
                        )
                        .when(server.config.is_some(), |d| {
                            d.child(ui::button("server-forget", "Remove server").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.server_disconnect(true, cx))))
                        })
                        .when_some(server.busy.clone(), |d, busy| d.child(div().text_size(px(12.)).text_color(theme::text_soft()).child(busy))),
                )
            })
            .when_some(server.output.clone(), |d, out| {
                d.child(div().p_2p5().rounded_md().bg(theme::bg()).font_family(theme::MONO_FONT).text_size(px(11.5)).text_color(theme::text_soft()).child(out))
            })
            .into_any_element(),
    );

    // Is it ready to keep sessions running?
    if let Some(checks) = &server.checks {
        let rows: Vec<AnyElement> = checks
            .iter()
            .map(|c| {
                let (mark, color) = match c.level {
                    CheckLevel::Ok => ("✓", theme::added()),
                    CheckLevel::Warn => ("!", theme::modified()),
                    CheckLevel::Fail => ("✕", theme::removed()),
                };
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(div().w(px(14.)).text_color(color).child(mark))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_size(px(12.5)).child(format!("{}: {}", c.name, c.detail)))
                            .when_some(c.fix.clone(), |d, fix| d.child(div().font_family(theme::MONO_FONT).text_size(px(11.)).text_color(theme::muted()).child(fix))),
                    )
                    .into_any_element()
            })
            .collect();
        body.push(section("Ready to keep sessions running?").child(div().flex().flex_col().gap_1p5().children(rows)).into_any_element());
    }

    if server.connected() && through_relay.is_none() {
        body.push(ws.render_pairing(cx));
    }

    if let Some(state) = server.state.as_ref().filter(|_| server.connected()) {
        // Accounts: each signs in on the server once; the login stays there.
        let mut rows: Vec<AnyElement> = Vec::new();
        for account in &state.accounts {
            let signed = state.signed_in.get(&account.id).copied().unwrap_or(false);
            let id = account.id.clone();
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(ui::dot(theme::agent_color(account.agent)))
                    .child(div().flex_1().child(format!("{} · {}", account.agent.name(), account.display())))
                    .child(div().text_size(px(12.)).text_color(if signed { theme::added() } else { theme::muted() }).child(if signed { "signed in" } else { "not signed in" }))
                    .child(ui::button(SharedString::from(format!("server-login-{}", account.id)), if signed { "Sign in again" } else { "Sign in" }).on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| this.server_login(&id, window, cx),
                    )))
                    .into_any_element(),
            );
        }
        let adding = form.new_account;
        body.push(
            section("Accounts on the server")
                .child(div().text_size(px(12.)).text_color(theme::muted()).child(format!(
                    "Each account signs in on the server once, with the CLI's own login. Logins are never copied from {} (they would sign each other out).",
                    crate::platform::this_computer()
                )))
                .child(div().flex().flex_col().gap_2().children(rows))
                .child(match adding {
                    Some(agent) => div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().text_size(px(12.5)).child(format!("New {} account:", agent.name())))
                        .child(div().flex_1().child(form.account_label.clone()))
                        .child(ui::primary_button("server-account-add", "Add and sign in").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.server_add_account_from_form(window, cx))))
                        .into_any_element(),
                    None => div()
                        .flex()
                        .gap_2()
                        .child(ui::button("server-add-claude", "+ Claude account").on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            if let Some(Modal::Server(form)) = this.modal.as_mut() {
                                form.new_account = Some(AgentKind::Claude);
                                crate::text_input::focus(&form.account_label.clone(), window, cx);
                            }
                            cx.notify();
                        })))
                        .child(ui::button("server-add-codex", "+ Codex account").on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            if let Some(Modal::Server(form)) = this.modal.as_mut() {
                                form.new_account = Some(AgentKind::Codex);
                                crate::text_input::focus(&form.account_label.clone(), window, cx);
                            }
                            cx.notify();
                        })))
                        .into_any_element(),
                })
                .into_any_element(),
        );

        // Projects: clone the laptop's projects on the server.
        let server_names: Vec<&str> = state.projects.iter().map(|p| p.name.as_str()).collect();
        let rows: Vec<AnyElement> = ws
            .state
            .projects
            .iter()
            .map(|p| {
                let on_server = server_names.contains(&p.name.as_str());
                let id = p.id.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(p.name.clone()))
                    .child(if on_server {
                        div().text_size(px(12.)).text_color(theme::added()).child("on the server ✓").into_any_element()
                    } else {
                        ui::button(SharedString::from(format!("server-copy-{}", p.id)), "Copy to server")
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.server_copy_project(&id, window, cx)))
                            .into_any_element()
                    })
                    .into_any_element()
            })
            .collect();
        body.push(
            section("Projects")
                .child(div().text_size(px(12.)).text_color(theme::muted()).child("Copying clones the project's origin on the server, at the same place under the home folder."))
                .child(div().flex().flex_col().gap_2().children(rows))
                .into_any_element(),
        );
    }

    if let Some((title, terminal)) = &form.login {
        body.push(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(title.clone()))
                        .child(ui::button("server-login-close", "Close terminal").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            if let Some(Modal::Server(form)) = this.modal.as_mut() {
                                form.login = None;
                            }
                            this.server_refresh(cx);
                            cx.notify();
                        }))),
                )
                .child(div().text_size(px(12.)).text_color(theme::muted()).child(format!(
                    "This terminal runs on the server. Open the link it shows in {}'s browser and paste the code back here.",
                    crate::platform::this_computer()
                )))
                .child(div().h(px(300.)).rounded_md().border_1().border_color(theme::border_strong()).overflow_hidden().child(terminal.clone()))
                .into_any_element(),
        );
    }

    div().flex().flex_col().gap_5().children(body).into_any_element()
}
