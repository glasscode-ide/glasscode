//! The data Glasscode persists: projects, accounts and sessions.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
    Shell,
}

impl AgentKind {
    pub fn name(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude",
            AgentKind::Codex => "Codex",
            AgentKind::Shell => "Shell",
        }
    }

    pub fn uses_accounts(self) -> bool {
        !matches!(self, AgentKind::Shell)
    }

    pub fn other(self) -> AgentKind {
        match self {
            AgentKind::Claude => AgentKind::Codex,
            _ => AgentKind::Claude,
        }
    }
}

/// One signed-in subscription. Each account owns a config directory, which is how the
/// official CLIs keep several logins apart (`CLAUDE_CONFIG_DIR` / `CODEX_HOME`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub agent: AgentKind,
    pub label: String,
    #[serde(default)]
    pub plan: Option<String>,
    /// `true` for the login the CLI already had before Glasscode (`~/.claude` / `~/.codex`).
    #[serde(default)]
    pub system_default: bool,
}

impl Account {
    /// The config directory to point the CLI at, or `None` to let it use its own default.
    pub fn config_dir(&self) -> Option<PathBuf> {
        if self.system_default {
            None
        } else {
            Some(crate::paths::accounts_dir().join(&self.id))
        }
    }

    /// Where the CLI keeps this account's data, whether or not we override it.
    pub fn effective_dir(&self) -> PathBuf {
        self.config_dir().unwrap_or_else(|| {
            let home = crate::paths::home();
            match self.agent {
                AgentKind::Codex => home.join(".codex"),
                _ => home.join(".claude"),
            }
        })
    }

    pub fn display(&self) -> String {
        match &self.plan {
            Some(plan) if !plan.is_empty() => format!("{} ({plan})", self.label),
            _ => self.label.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    /// The main checkout of the repository.
    pub root: PathBuf,
    #[serde(default = "yes")]
    pub expanded: bool,
    /// The GitHub account its sessions push and use `gh` as; none: Glasscode's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_account: Option<String>,
}

fn yes() -> bool {
    true
}

/// A terminal tab running an agent (or a shell) in one worktree with one account.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub project_id: String,
    pub worktree: PathBuf,
    pub branch: String,
    pub title: String,
    pub agent: AgentKind,
    #[serde(default)]
    pub account_id: Option<String>,
    /// The CLI's own conversation id, used for `--resume`.
    #[serde(default)]
    pub agent_session_id: Option<String>,
    /// The CLI's transcript file, when known.
    #[serde(default)]
    pub transcript_path: Option<PathBuf>,
    /// "Claude · Personal → Codex · Work" history of who drove this session.
    #[serde(default)]
    pub lineage: Vec<String>,
    /// First prompt to send when the session starts (used by handoff).
    #[serde(default)]
    pub initial_prompt: Option<String>,
    /// Remote task this session was handed to, if any.
    #[serde(default)]
    pub team_task: Option<String>,
    /// A review session: the pull request it reviews (Explain → Plan → private review).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewRef>,
    /// The model the agent starts with (`--model`), when one was chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// How Claude asks for permission (`default`, `acceptEdits`, `plan`, `auto`); for Codex
    /// `full-auto` or `read-only`. None: the CLI's own setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub created_at: u64,
}

/// The pull request a review session is about, and its private review once started.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReviewRef {
    /// `owner/name`.
    pub repo: String,
    pub number: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub head: String,
    #[serde(default)]
    pub base: String,
    /// The private review being run or already run for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_review: Option<String>,
}

impl Session {
    pub fn in_worktree(&self, path: &Path) -> bool {
        self.worktree == path
    }

    /// This session continues with another agent (a handoff in the same tab): a new
    /// conversation that starts with `prompt`. The model goes back to the CLI's own, because
    /// the model name of one agent means nothing to the other.
    pub fn hand_to(&mut self, agent: AgentKind, account_id: String, agent_session_id: Option<String>, prompt: String, label: String) {
        // Auto-generated titles name the agent ("Claude · main"); keep them accurate.
        if let Some(rest) = self.title.strip_prefix(&format!("{} · ", self.agent.name())) {
            self.title = format!("{} · {rest}", agent.name());
        }
        self.agent = agent;
        self.account_id = Some(account_id);
        self.agent_session_id = agent_session_id;
        self.transcript_path = None;
        self.initial_prompt = Some(prompt);
        self.model = None;
        self.lineage.push(label);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupMode {
    /// Bring every session back as it was.
    #[default]
    Restore,
    /// Restore, and also open a fresh Claude session.
    RestoreAndNewClaude,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Defaults {
    pub claude: Option<String>,
    pub codex: Option<String>,
}

impl Defaults {
    pub fn for_agent(&self, agent: AgentKind) -> Option<&String> {
        match agent {
            AgentKind::Claude => self.claude.as_ref(),
            AgentKind::Codex => self.codex.as_ref(),
            AgentKind::Shell => None,
        }
    }

    pub fn set(&mut self, agent: AgentKind, id: Option<String>) {
        match agent {
            AgentKind::Claude => self.claude = id,
            AgentKind::Codex => self.codex = id,
            AgentKind::Shell => {}
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub accounts: Vec<Account>,
    #[serde(default)]
    pub sessions: Vec<Session>,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub active_session: Option<String>,
    #[serde(default)]
    pub startup: StartupMode,
    #[serde(default)]
    pub github: crate::github_accounts::GithubSettings,
    /// Off: Glasscode does not ask a session's agent to fix its PR's failing CI by itself.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ci_autofix_off: bool,
    /// Off: Glasscode does not ask a session's agent to resolve its PR's merge conflicts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub conflict_autofix_off: bool,
    /// Repositories (`owner/name`) where Glasscode may review a pull request that asks for
    /// your review by itself, and post the overview as a comment. Opt-in, empty by default.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub auto_review_repos: std::collections::BTreeSet<String>,
    /// Sessions whose title the person chose (Glasscode does not rename those).
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub custom_titles: std::collections::BTreeSet<String>,
    /// Off: sessions keep the titles they were created with, and branches are not renamed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_names_off: bool,
    #[serde(default)]
    pub layout: Layout,
    #[serde(default)]
    pub notify: NotifySettings,
    /// Linear, for the Tickets view (the API key is in the Keychain, not here).
    #[serde(default)]
    pub linear: crate::linear::LinearSettings,
    /// Sessions started from a ticket: session id → the ticket's identifier ("ENG-826").
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub ticket_of: std::collections::BTreeMap<String, String>,
    /// Which ticket provider the board shows, and the chosen project per provider.
    #[serde(default)]
    pub tickets: TicketSettings,
    /// Smaller choices from Settings (editor, asking before closing).
    #[serde(default)]
    pub prefs: crate::prefs::Prefs,
    /// What agents may do with the `glasscode` command (Settings › Automation).
    #[serde(default, deserialize_with = "lenient")]
    pub agents: crate::control::AgentSettings,
    /// The theme, accent, fonts and sizes (Themes). Never stops the state from loading: a
    /// value this build does not know (from a newer build, or written by hand) is the default.
    #[serde(default, deserialize_with = "lenient")]
    pub appearance: Appearance,
    /// Worktrees pinned to the top of the sidebar, in pin order (first pinned sits highest).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned: Vec<PathBuf>,
    /// Projects hidden from the sidebar, the CLI and Glasscode (Settings › Projects). A hidden
    /// project's worktrees, sessions and usage are not listed or reachable anywhere until it
    /// is switched back on; the project, its sessions and its on-disk worktrees are untouched.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub hidden_projects: std::collections::BTreeSet<String>,
    /// What the person answered about each project's setup script (its
    /// `.glasscode/settings.json`), by the project's folder. Only a hash of an approved command
    /// is kept; a different command asks again.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub setup_approvals: std::collections::BTreeMap<String, SetupApproval>,
    /// The first run was finished or skipped. A state from before it has none, so a home with
    /// projects never shows it, and one without projects shows it once.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub first_run_done: bool,
}

/// The person's answer about one project's setup script.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupApproval {
    /// SHA-256 (hex) of the command that was approved with Run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved: Option<String>,
    /// "Never for this project": no question and no run until "Ask again" in Settings.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub never: bool,
}

/// Where the Tickets board gets its tickets from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Linear,
    #[serde(alias = "github")]
    GitHub,
}

impl Provider {
    pub fn label(&self) -> &'static str {
        match self {
            Provider::Linear => "Linear",
            Provider::GitHub => "GitHub",
        }
    }
}

/// What the person chose for the Tickets board: which provider, and the project picked for each
/// (a Linear team key, a GitHub `owner/name`). Remembered across launches, and auto-detected
/// per repository when nothing is set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketSettings {
    /// Manual provider override per GitHub repo slug ("owner/name"); auto-detect fills the rest.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub provider_by_repo: std::collections::BTreeMap<String, Provider>,
    /// The Linear team key the board is filtered to (none: the whole workspace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linear_team: Option<String>,
    /// The GitHub repository the board shows issues for ("owner/name"); none: the first project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_repo: Option<String>,
    /// The provider the board last showed, so it opens the same way next time.
    #[serde(default)]
    pub last_provider: Provider,
}

/// How Glasscode looks (Themes). Every field has a default, and an unknown value is the
/// default too, so older and newer state files both load.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Appearance {
    #[serde(default, deserialize_with = "lenient")]
    pub theme: ThemeChoice,
    #[serde(default, deserialize_with = "lenient")]
    pub accent: AccentChoice,
    #[serde(default, deserialize_with = "lenient")]
    pub text_size: TextSize,
    #[serde(default, deserialize_with = "lenient")]
    pub density: Density,
    #[serde(default, deserialize_with = "lenient")]
    pub ui_font: UiFont,
    #[serde(default, deserialize_with = "lenient")]
    pub code_font: CodeFont,
    #[serde(default, deserialize_with = "lenient")]
    pub cursor: CursorStyle,
    /// What agents write in the Chat view.
    #[serde(default, deserialize_with = "lenient")]
    pub chat_font: ChatFont,
    /// How a Claude or Codex session opens: as a chat, or its terminal.
    #[serde(default, deserialize_with = "lenient")]
    pub session_view: SessionView,
    /// The moving backdrop behind the screens, or none.
    #[serde(default, deserialize_with = "lenient")]
    pub backdrop: BackdropChoice,
    /// How the backdrop is drawn; the voice orb follows it.
    #[serde(default, deserialize_with = "lenient")]
    pub finish: crate::backdrop::Finish,
    /// The backdrop shows behind sessions too, not only behind Today, Settings and the first run.
    #[serde(default = "yes", deserialize_with = "lenient_yes")]
    pub backdrop_in_sessions: bool,
    /// The size of the whole interface.
    #[serde(default, deserialize_with = "lenient")]
    pub scale: UiScale,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance {
            theme: ThemeChoice::default(),
            accent: AccentChoice::default(),
            text_size: TextSize::default(),
            density: Density::default(),
            ui_font: UiFont::default(),
            code_font: CodeFont::default(),
            cursor: CursorStyle::default(),
            chat_font: ChatFont::default(),
            session_view: SessionView::default(),
            backdrop: BackdropChoice::default(),
            finish: crate::backdrop::Finish::default(),
            backdrop_in_sessions: true,
            scale: UiScale::default(),
        }
    }
}

/// A value that, when it does not read (unknown or malformed), is its default instead of an
/// error for the whole file.
fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(T::deserialize(value).unwrap_or_default())
}

/// `lenient` for a switch that is on unless the file says otherwise.
fn lenient_yes<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(bool::deserialize(value).unwrap_or(true))
}

/// What is behind the screens: one of the backdrop's looks, or none (a plain background).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackdropChoice {
    #[default]
    Prism,
    Aurora,
    Dusk,
    None,
}

impl BackdropChoice {
    /// The look it draws; none for `None`.
    pub fn preset(self) -> Option<crate::backdrop::Preset> {
        use crate::backdrop::Preset;
        match self {
            BackdropChoice::Prism => Some(Preset::Prism),
            BackdropChoice::Aurora => Some(Preset::Aurora),
            BackdropChoice::Dusk => Some(Preset::Dusk),
            BackdropChoice::None => None,
        }
    }
}

/// The size of the whole interface, in percent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UiScale {
    P90,
    #[default]
    P100,
    P115,
    P130,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeChoice {
    /// Glasscode Light or Dark, following macOS.
    #[default]
    Auto,
    Light,
    Dark,
    /// Deep navy and amber, the look before the redesign.
    Classic,
    Contrast,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccentChoice {
    /// The theme's own (ink in Glasscode).
    #[default]
    Theme,
    Indigo,
    Amber,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TextSize {
    Small,
    #[default]
    Medium,
    Large,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Density {
    #[default]
    Comfortable,
    Compact,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UiFont {
    #[default]
    Geist,
    Inter,
    Plex,
    System,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodeFont {
    #[default]
    Geist,
    JetBrains,
    Plex,
    System,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CursorStyle {
    #[default]
    Block,
    Bar,
    Underline,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChatFont {
    /// The interface font.
    #[default]
    Ui,
    /// A serif, like the Claude app's replies.
    Serif,
    System,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionView {
    #[default]
    Chat,
    Terminal,
}

/// The sidebars' widths, and which ones show, as the person left them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<f32>,
    /// The Tickets view's reading panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tickets: Option<f32>,
    /// The left sidebar is hidden (its button or key, or its edge dragged in).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub left_hidden: bool,
    /// The right panel is hidden.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub right_hidden: bool,
    /// The right panel's tab: "pull-request", "changes", "files" or "glasscode".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right_tab: Option<String>,
    /// The projects and worktrees folded in the sidebar, by key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folded: Vec<String>,
}

/// When Glasscode tells the person about a session (all on unless switched off).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NotifySettings {
    /// A session finished its turn.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub finished_off: bool,
    /// A session asks something or waits for a permission.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_input_off: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sound_off: bool,
}

impl State {
    pub fn account(&self, id: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    pub fn project(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    /// Whether this project is switched off in Settings › Projects. Hidden projects, their
    /// worktrees, sessions and usage are invisible to the sidebar, the CLI and Glasscode; only
    /// the Settings projects list (which switches them back on) sees past this.
    pub fn is_hidden(&self, project_id: &str) -> bool {
        self.hidden_projects.contains(project_id)
    }

    /// A session belongs to a hidden project (so it is itself hidden everywhere).
    pub fn session_hidden(&self, s: &Session) -> bool {
        self.hidden_projects.contains(&s.project_id)
    }

    /// Whether this worktree path lives under a hidden project's root.
    pub fn worktree_hidden(&self, worktree: &Path) -> bool {
        self.projects
            .iter()
            .any(|p| self.hidden_projects.contains(&p.id) && worktree.starts_with(&p.root))
    }

    /// The projects the sidebar, CLI and Glasscode may see (every project but the hidden ones).
    pub fn visible_projects(&self) -> impl Iterator<Item = &Project> {
        self.projects.iter().filter(|p| !self.hidden_projects.contains(&p.id))
    }

    /// The sessions the sidebar, CLI and Glasscode may see (none from a hidden project).
    pub fn visible_sessions(&self) -> impl Iterator<Item = &Session> {
        self.sessions.iter().filter(|s| !self.hidden_projects.contains(&s.project_id))
    }

    pub fn session(&self, id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn session_mut(&mut self, id: &str) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    /// Whether this worktree is pinned to the top of the sidebar.
    pub fn is_pinned(&self, worktree: &Path) -> bool {
        self.pinned.iter().any(|p| p == worktree)
    }

    /// Its place among the pinned worktrees (top is 0), or None when it isn't pinned.
    pub fn pin_index(&self, worktree: &Path) -> Option<usize> {
        self.pinned.iter().position(|p| p == worktree)
    }

    pub fn accounts_for(&self, agent: AgentKind) -> impl Iterator<Item = &Account> {
        self.accounts.iter().filter(move |a| a.agent == agent)
    }

    /// The default account for an agent, falling back to the first one available.
    pub fn default_account(&self, agent: AgentKind) -> Option<&Account> {
        self.defaults
            .for_agent(agent)
            .and_then(|id| self.account(id))
            .or_else(|| self.accounts_for(agent).next())
    }

    /// Make the existing CLI logins (`~/.claude`, `~/.codex`) available as "My login"
    /// accounts. On a server they are added even before the first sign-in.
    pub fn bootstrap_accounts(&mut self, require_existing_login: bool) {
        let home = crate::paths::home();
        let candidates = [
            (AgentKind::Claude, "claude-default", home.join(".claude")),
            (AgentKind::Codex, "codex-default", home.join(".codex")),
        ];
        for (agent, id, dir) in candidates {
            let known = self.accounts.iter().any(|a| a.id == id);
            let present = !require_existing_login || dir.is_dir();
            if !known && present && self.accounts_for(agent).next().is_none() {
                self.accounts.push(Account {
                    id: id.into(),
                    agent,
                    label: "My login".into(),
                    plan: None,
                    system_default: true,
                });
            }
            if self.defaults.for_agent(agent).is_none() {
                let first = self.accounts_for(agent).next().map(|a| a.id.clone());
                self.defaults.set(agent, first);
            }
        }
    }

    pub fn lineage_tag(&self, agent: AgentKind, account_id: Option<&str>) -> String {
        match account_id.and_then(|id| self.account(id)) {
            Some(account) => format!("{} · {}", agent.name(), account.label),
            None => agent.name().to_string(),
        }
    }
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod appearance_tests {
    use super::*;

    #[test]
    fn unknown_appearance_values_keep_the_state() {
        let json = r#"{
            "projects": [{ "id": "p1", "name": "myapp", "root": "/tmp/app" }],
            "sessions": [{ "id": "s1", "project_id": "p1", "worktree": "/tmp/app", "branch": "main", "title": "Shell", "agent": "shell", "created_at": 1 }],
            "appearance": { "theme": "moonlight", "accent": "teal", "ui_font": "comic", "code_font": 42, "text_size": "huge", "density": null, "cursor": "bar" }
        }"#;
        let state: State = serde_json::from_str(json).expect("state loads despite the unknown values");
        assert_eq!((state.projects.len(), state.sessions.len()), (1, 1));
        let a = state.appearance;
        assert_eq!((a.theme, a.accent, a.ui_font, a.code_font, a.text_size, a.density), (ThemeChoice::Auto, AccentChoice::Theme, UiFont::Geist, CodeFont::Geist, TextSize::Medium, Density::Comfortable));
        assert_eq!(a.cursor, CursorStyle::Bar, "a known value next to unknown ones is kept");

        let broken = r#"{ "projects": [{ "id": "p1", "name": "myapp", "root": "/tmp/app" }], "appearance": "dark" }"#;
        let state: State = serde_json::from_str(broken).expect("state loads with a malformed appearance");
        assert_eq!((state.projects.len(), state.appearance), (1, Appearance::default()));

        let round = serde_json::to_string(&Appearance { theme: ThemeChoice::Classic, code_font: CodeFont::JetBrains, ..Appearance::default() }).unwrap();
        let back: Appearance = serde_json::from_str(&round).unwrap();
        assert_eq!((back.theme, back.code_font), (ThemeChoice::Classic, CodeFont::JetBrains));
    }

    /// A state.json from before the backdrop: Prism, smooth, also behind sessions, at 100 %.
    #[test]
    fn an_old_appearance_gets_the_backdrop_defaults() {
        use crate::backdrop::{Finish, Preset};
        let state: State = serde_json::from_str(r#"{ "projects": [], "appearance": { "theme": "dark" } }"#).expect("an old state loads");
        let a = state.appearance;
        assert_eq!((a.theme, a.backdrop, a.finish, a.backdrop_in_sessions, a.scale), (ThemeChoice::Dark, BackdropChoice::Prism, Finish::Smooth, true, UiScale::P100));
        let d = Appearance::default();
        assert_eq!((d.backdrop, d.finish, d.backdrop_in_sessions, d.scale), (BackdropChoice::Prism, Finish::Smooth, true, UiScale::P100));
        assert_eq!(BackdropChoice::Prism.preset(), Some(Preset::Prism));
        assert_eq!(BackdropChoice::None.preset(), None);

        // Unknown values are the defaults; the switch stays on.
        let odd: Appearance = serde_json::from_str(r#"{ "backdrop": "lava", "finish": 3, "backdrop_in_sessions": "maybe", "scale": "p200" }"#).unwrap();
        assert_eq!((odd.backdrop, odd.finish, odd.backdrop_in_sessions, odd.scale), (BackdropChoice::Prism, Finish::Smooth, true, UiScale::P100));

        let chosen = Appearance { backdrop: BackdropChoice::None, finish: Finish::Dither, backdrop_in_sessions: false, scale: UiScale::P130, ..Appearance::default() };
        let json = serde_json::to_string(&chosen).unwrap();
        assert!(json.contains(r#""backdrop":"none""#) && json.contains(r#""finish":"dither""#) && json.contains(r#""scale":"p130""#), "{json}");
        assert_eq!(serde_json::from_str::<Appearance>(&json).unwrap(), chosen);
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    /// A state.json from before the panels could hide: the widths stay, and both panels show.
    #[test]
    fn an_old_layout_loads_with_both_panels_shown() {
        let json = r#"{
            "projects": [{ "id": "p1", "name": "myapp", "root": "/tmp/app" }],
            "layout": { "sidebar": 320.0, "right": 480.0, "tickets": 600.0 }
        }"#;
        let state: State = serde_json::from_str(json).expect("an old state loads");
        let layout = &state.layout;
        assert_eq!((layout.sidebar, layout.right, layout.tickets), (Some(320.), Some(480.), Some(600.)));
        assert_eq!((layout.left_hidden, layout.right_hidden, layout.right_tab.as_deref(), layout.folded.len()), (false, false, None, 0));

        let state: State = serde_json::from_str(r#"{ "projects": [] }"#).expect("a state without a layout loads");
        assert_eq!(state.layout, Layout::default());
    }

    #[test]
    fn the_new_layout_fields_go_round() {
        let layout = Layout { left_hidden: true, right_tab: Some("files".into()), folded: vec!["p1".into()], ..Layout::default() };
        let json = serde_json::to_string(&layout).unwrap();
        assert!(!json.contains("right_hidden"), "a field at its default is not written: {json}");
        assert_eq!(serde_json::from_str::<Layout>(&json).unwrap(), layout);
        assert_eq!(serde_json::to_string(&Layout::default()).unwrap(), "{}", "an old build reads what this one writes");
    }
}

#[cfg(test)]
mod setup_approval_tests {
    use super::*;

    /// An old state.json has no approvals; one with approvals goes round, and an empty list is
    /// not written, so an old build reads the file as before.
    #[test]
    fn approvals_load_from_old_and_new_state() {
        let old: State = serde_json::from_str(r#"{ "projects": [] }"#).expect("an old state loads");
        assert!(old.setup_approvals.is_empty());
        assert!(!serde_json::to_string(&old).unwrap().contains("setup_approvals"));
        let mut state = State::default();
        state.setup_approvals.insert("/w/app".into(), SetupApproval { approved: Some("ab".repeat(32)), never: false });
        state.setup_approvals.insert("/w/other".into(), SetupApproval { approved: None, never: true });
        let json = serde_json::to_string(&state).unwrap();
        let back: State = serde_json::from_str(&json).unwrap();
        assert_eq!(back.setup_approvals, state.setup_approvals);
    }
}

#[cfg(test)]
mod first_run_tests {
    use super::*;

    /// An old state.json has no `first_run_done`: it reads as not done, and a state that never
    /// had the first run writes nothing new, so an old build reads what this one writes.
    #[test]
    fn an_old_state_reads_as_first_run_not_done() {
        let old: State = serde_json::from_str(r#"{ "projects": [{ "id": "p1", "name": "myapp", "root": "/tmp/app" }] }"#).expect("an old state loads");
        assert!(!old.first_run_done);
        assert!(!serde_json::to_string(&old).unwrap().contains("first_run_done"));
        let done = State { first_run_done: true, ..State::default() };
        let json = serde_json::to_string(&done).unwrap();
        assert!(json.contains(r#""first_run_done":true"#), "{json}");
        assert!(serde_json::from_str::<State>(&json).unwrap().first_run_done);
    }
}

#[cfg(test)]
mod handoff_tests {
    use super::*;

    #[test]
    fn a_handoff_in_the_same_tab_does_not_keep_the_model_of_the_old_agent() {
        let json = r#"{ "id": "s1", "project_id": "p1", "worktree": "/tmp/app", "branch": "main", "title": "Codex · main", "agent": "codex",
            "account_id": "a", "agent_session_id": "old", "transcript_path": "/tmp/old.jsonl", "model": "gpt-5.6-luna", "mode": "auto", "created_at": 1 }"#;
        let mut session: Session = serde_json::from_str(json).unwrap();
        session.hand_to(AgentKind::Claude, "b".into(), Some("new".into()), "Read the notes.".into(), "Claude · Work".into());
        assert_eq!(session.model, None, "Claude must not start with a Codex model");
        assert_eq!((session.agent, session.title.as_str()), (AgentKind::Claude, "Claude · main"));
        assert_eq!((session.account_id.as_deref(), session.agent_session_id.as_deref(), session.transcript_path.as_ref()), (Some("b"), Some("new"), None));
        assert_eq!((session.initial_prompt.as_deref(), session.lineage.as_slice()), (Some("Read the notes."), &["Claude · Work".to_string()][..]));
        assert_eq!(session.mode.as_deref(), Some("auto"), "the mode stays: the two agents read it");
    }
}
