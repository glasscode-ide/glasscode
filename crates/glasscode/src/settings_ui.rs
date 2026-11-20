//! Settings: one page on the backdrop (the Settings row of the left sidebar, and its key), in
//! twelve sections. Each setting is a card on glass: its title, one sentence, its control, and a
//! footer line where it helps; a list (accounts, projects, shortcuts) is rows in its card. A
//! change saves at once. The search finds the cards of every section by title and sentence.
//!
//! Every card is in one table (`cards!` below): a section draws its cards from it, the search
//! reads it, and the tests check that each saved setting has a card.

use std::path::{Path, PathBuf};

use glasscode_core::control::{Kind, Rule};
use glasscode_core::model::{Project, StartupMode, State};
use gpui::{
    AnyElement, ClickEvent, Context, Div, Entity, FontWeight, ScrollHandle, SharedString, Window,
    div, point, prelude::*, px,
};

use crate::backdrop;
use crate::dialogs::AccountsForm;
use crate::icons;
use crate::platform;
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

/// The sections of the page. The old names stay, because other files and the debug hooks open
/// them by name; their labels are in `SECTIONS`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    General,
    /// The Themes page that was beside the sections.
    Appearance,
    /// The manager: the agent above every session.
    Manager,
    /// Agents and accounts.
    Accounts,
    Permissions,
    /// Git and projects.
    Projects,
    Integrations,
    Automation,
    Browser,
    /// Server and devices.
    Server,
    Updates,
    /// Keyboard.
    Shortcuts,
    Advanced,
}

/// The sections in the order of the menu: the label, the sentence under the heading, the icon.
const SECTIONS: [(Section, &str, &str, &[u8]); 13] = [
    (
        Section::General,
        "General",
        "How Glasscode starts, what it tells you about, and how sessions behave.",
        icons::GEAR,
    ),
    // Where the Manager had it before the redesign: right after General.
    (
        Section::Manager,
        "Manager",
        "One agent above all your sessions: its name and brain, what it manages and what it may send.",
        icons::STAR,
    ),
    (
        Section::Appearance,
        "Appearance",
        "Theme, backdrop, type and density. A change shows at once, in every window.",
        icons::THEMES,
    ),
    (
        Section::Accounts,
        "Agents and accounts",
        "The Claude, Codex and GitHub accounts that sessions use.",
        icons::SESSIONS,
    ),
    (
        Section::Permissions,
        "Permissions",
        "What agents may do with the glasscode command: Allow does it, Ask waits for you, Never refuses.",
        icons::SHIELD,
    ),
    (
        Section::Projects,
        "Git and projects",
        "Each project's repository and GitHub account, and how worktrees start and go.",
        icons::BRANCH,
    ),
    (
        Section::Integrations,
        "Integrations",
        "Linear, for your tickets on Today and the Board.",
        icons::PLUG,
    ),
    (
        Section::Automation,
        "Automation",
        "What Glasscode does by itself, and when it stops.",
        icons::PLAY,
    ),
    (
        Section::Browser,
        "Browser",
        "The browser agents use for previews, signed in as you.",
        icons::GRID,
    ),
    (
        Section::Server,
        "Server and devices",
        "Your always-on server and the iPhone.",
        icons::CPU,
    ),
    (
        Section::Updates,
        "Updates",
        "Signed releases of Glasscode, and the CLI versions sessions run.",
        icons::REFRESH,
    ),
    (
        Section::Shortcuts,
        "Keyboard",
        "Keys that work everywhere in Glasscode.",
        icons::KEYBOARD,
    ),
    (
        Section::Advanced,
        "Advanced",
        "Where Glasscode keeps its settings and its logs.",
        icons::FILE,
    ),
];

impl Section {
    fn entry(self) -> (Section, &'static str, &'static str, &'static [u8]) {
        SECTIONS
            .into_iter()
            .find(|(s, ..)| *s == self)
            .unwrap_or(SECTIONS[0])
    }

    fn label(self) -> &'static str {
        self.entry().1
    }

    /// Every section, in the order of the menu.
    pub(crate) fn all() -> [Section; 13] {
        SECTIONS.map(|(section, ..)| section)
    }

    /// Its label in lower case with dashes for the spaces ("git-and-projects"): the name a place
    /// gives it (`capabilities::Place`), which [`Section::named`] reads too.
    pub(crate) fn slug(self) -> String {
        self.label().to_lowercase().replace(' ', "-")
    }

    fn sentence(self) -> &'static str {
        self.entry().2
    }

    fn icon(self) -> &'static [u8] {
        self.entry().3
    }

    /// The section a debug hook names (`GLASSCODE_DEBUG_OPEN=settings:<name>`): its label in the
    /// menu in lower case, with dashes for the spaces ("git-and-projects"), the label's first word
    /// ("git", "agents"), or a name from before the page had 12 sections ("projects",
    /// "shortcuts", "accounts", "themes").
    pub fn named(name: &str) -> Option<Section> {
        let name = name.trim().to_lowercase().replace([' ', '_'], "-");
        let old = match name.as_str() {
            "accounts" => Some(Section::Accounts),
            "projects" => Some(Section::Projects),
            "shortcuts" => Some(Section::Shortcuts),
            "themes" => Some(Section::Appearance),
            _ => None,
        };
        old.or_else(|| {
            SECTIONS.iter().map(|(section, label, ..)| (*section, label.to_lowercase().replace(' ', "-"))).find_map(|(section, label)| {
                (label == name || label.split('-').next() == Some(name.as_str())).then_some(section)
            })
        })
    }
}

/// A card as the search and the sections see it.
pub(crate) struct CardInfo {
    pub card: CardId,
    pub section: Section,
    pub title: &'static str,
    pub sentence: &'static str,
}

/// Makes the card ids and the table of cards from one list, so that no card can be drawn and
/// not found, or found and not drawn (`settings_card` matches each id).
macro_rules! cards {
    ($($card:ident in $section:ident: $title:expr, $sentence:expr;)*) => {
        /// A card of the page.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub(crate) enum CardId {
            $($card),*
        }

        /// Every card, in the order of the page.
        pub(crate) fn catalog() -> Vec<CardInfo> {
            vec![$(CardInfo { card: CardId::$card, section: Section::$section, title: $title, sentence: $sentence }),*]
        }
    };
}

cards! {
    Startup in General: "When Glasscode opens", "Sessions come back as you left them: the same projects, worktrees, tabs, accounts and conversations.";
    NotifyNeeds in General: "A session needs you", "Notify me when a session asks a question, wants a permission or has a plan to approve.";
    NotifyFinished in General: "A session finishes its turn", "Notify me when an agent is done and waits for you.";
    NotifyCi in General: "CI fails on one of my pull requests", "Notify me, also when Glasscode gives up asking the agent to fix it, and for merge conflicts.";
    NotifyTickets in General: "A ticket or a review asks for me", "Notify me when a ticket is assigned to me or a review asks for me, from Linear and GitHub.";
    NotifySound in General: "Play a sound", "With each notice that Glasscode shows.";
    AutoNames in General: "Name sessions and new worktrees by themselves", "From the pull request's title, else Claude's own title for the conversation, else your first message.";
    ConfirmClose in General: "Ask before closing a session that is working", "So that a busy session does not close by mistake.";
    Editor in General: "Open files in", "The editor that the editor button and file links use.";

    // The manager's own section draws its groups itself (`manager_settings.rs`).
    Manager in Manager: "The manager", "Switch it on, its name and brain, the projects it manages, where it runs, messages, teammates, voice and the daily brief.";

    Theme in Appearance: "Theme", "Glasscode Light and Dark, Glasscode classic and High contrast. Auto follows the system.";
    Accent in Appearance: "Accent", "Primary buttons, switches and the cursor. Glasscode uses ink; colour is kept for status.";
    Backdrop in Appearance: "Backdrop", "The moving light behind Today, Settings and sessions. None is a plain background.";
    Finish in Appearance: "Backdrop finish", "Smooth, or Dither: fine dots, as in print. The voice orb follows it.";
    BackdropInSessions in Appearance: "Backdrop behind sessions", "Off: sessions keep a plain background, and the backdrop moves only behind Today, Settings and the first run.";
    Scale in Appearance: "Scale", "The size of the whole interface: text, icons, gaps and panels.";
    TextSize in Appearance: "Text size", "The terminals, and the base size of the interface, on top of the scale.";
    Density in Appearance: "Density", "Rows in the sidebar.";
    UiFont in Appearance: "Interface font", "Menus, the sidebar, settings.";
    CodeFont in Appearance: "Code font", "Terminals, branches and paths.";
    ChatFont in Appearance: "Conversation font", "What Claude and Codex write in the Chat view.";
    SessionView in Appearance: "Sessions open as", "Chat reads like the Claude app; Terminal is the CLI itself. Each session has the switch too.";
    Cursor in Appearance: "Cursor", "In every terminal, where the program does not choose its own.";

    AgentAccounts in Accounts: "Claude and Codex accounts", "Each session runs on one account; new sessions use the default. Sign in, add an account, make one the default or remove one.";
    GithubAccounts in Accounts: "GitHub accounts", "The accounts gh is signed in to, the default for every repository, and the email each one commits with.";

    RuleRead in Permissions: Kind::Read.label(), suggested(Kind::Read);
    RuleBoard in Permissions: Kind::Board.label(), suggested(Kind::Board);
    RuleStart in Permissions: Kind::Start.label(), suggested(Kind::Start);
    RuleRename in Permissions: Kind::Rename.label(), suggested(Kind::Rename);
    RuleSend in Permissions: Kind::Send.label(), suggested(Kind::Send);
    RuleImport in Permissions: Kind::Import.label(), suggested(Kind::Import);
    RuleClose in Permissions: Kind::Close.label(), suggested(Kind::Close);
    RuleRemove in Permissions: Kind::Remove.label(), suggested(Kind::Remove);
    RuleMove in Permissions: Kind::Move.label(), suggested(Kind::Move);
    RuleTickets in Permissions: Kind::Tickets.label(), suggested(Kind::Tickets);
    RuleGithub in Permissions: Kind::Github.label(), suggested(Kind::Github);
    RuleShow in Permissions: Kind::Show.label(), suggested(Kind::Show);
    RuleSettings in Permissions: Kind::Settings.label(), suggested(Kind::Settings);
    AgentsSkill in Permissions: "Tell new sessions about the glasscode command", "Claude gets a skill, Codex a few lines of instructions. Off: agents only find it if someone tells them.";
    AgentsOutside in Permissions: "Programs Glasscode did not start follow these rules too", "Off: they can only read; anything else waits for your Allow. You at a terminal can always do everything.";
    ControlStatus in Permissions: "The glasscode command", "Claude and Codex control Glasscode with it; glasscode help lists the commands.";

    Projects in Projects: "Projects", "Show a project in Glasscode or hide it, its repository and GitHub account, where its worktrees go, and cleaning them up.";
    FreshMain in Projects: "Start new work from the latest main", "Fetched first, never from a stale folder.";
    DeleteBranch in Projects: "Also delete the local branch when removing a worktree", "Only when every commit of it is on the remote or another branch; you can change it each time.";
    // A project's own file (`.glasscode/settings.json`): the scope "Project" of this section.
    ProjectSetup in Projects: "Setup script", "Runs in each new worktree before the agent starts, once you allow it on this computer. Set in the project's file.";
    ProjectBranch in Projects: "Branch names", "How a new branch is named, from {type}, {issue}, {slug}, {repo} and {branch}. Set in the project's file.";
    ProjectFolder in Projects: "Worktree folder", "Where a new worktree goes, always below Glasscode's worktree folder. Set in the project's file.";
    ProjectAgent in Projects: "Default agent", "The agent of a new session when none is chosen: Claude, Codex or Shell. Set in the project's file.";

    Linear in Integrations: "Linear", "Your tickets on Today and the Board: the API key, who you are, your reviewer label, and who drives sessions started from a ticket.";

    FixCi in Automation: "Ask the session's agent to fix failing CI", "Once per commit, when its turn is over and nobody is typing. Never for review sessions.";
    FixConflicts in Automation: "Ask the session's agent to resolve merge conflicts", "When GitHub says the pull request conflicts with its base: once per new commit on either side. Conflicts come before CI.";
    FixTries in Automation: "Asks before it is over to you", "How many times in a row Glasscode asks the agent to fix CI or conflicts, until it passes or merges; then it tells you.";
    CiWait in Automation: "Wait for the other checks", "Once one check fails, so that the agent hears of every failure at once.";
    AutoReview in Automation: "Auto-review pull requests", "Runs your agent on the diff of a pull request that asks for your review and posts an overview comment, never an approval. Each repository opts in.";
    MoveTicket in Automation: "Move a ticket to In Progress when its session starts", "Linear: only from Triage, Backlog or Todo; you can turn it off each time.";
    AgentsLog in Automation: "Show what agents did on Today", "One line for each action, with the session that did it. glasscode log lists them all.";
    Morning in Automation: "Open Today the first time each morning", "When Glasscode is in front before noon and you have not looked at Today yet.";

    BrowserState in Browser: "Glasscode Browser", "A browser window of its own that the agents use. Your own browser is not touched.";
    BrowserAgents in Browser: "Agents may use the browser", "New Claude and Codex sessions can use it, each in its own background tab, without asking.";
    BrowserReset in Browser: "Start over", "Quits the Glasscode Browser and forgets everything it knows: sign-ins, history, extensions.";

    Server in Server: "Server", "Your always-on machine: sessions there keep running, and the iPhone reaches them.";
    Phones in Server: "iPhone", "Phones that reach your server. Pair one with a code to scan in Glasscode for iPhone.";
    Voice in Server: "Reach Glasscode on your network", "The glasses and the phone reach this computer over the same Wi-Fi or Tailscale, through the voice server on port 4517.";

    UpdateStatus in Updates: "Glasscode updates", "Glasscode looks for a new release by itself every few hours; releases are signed.";
    Channel in Updates: "Channel", "Beta gets new builds first.";
    InstallOnQuit in Updates: "Install updates when Glasscode quits", "Or at once with Restart to update.";
    CliVersions in Updates: "CLI versions", "Claude Code and Codex are built in; keep them current here, and add any other CLI you use. New and reopened sessions pick up a change; running ones keep theirs.";

    Shortcuts in Shortcuts: "Shortcuts", "Search, sessions, panels and the terminal.";

    GlasscodeFolder in Advanced: "The Glasscode folder", "state.json keeps every setting on this page, with your projects, accounts and sessions; activity.json what Today counts; agent-log.jsonl what agents did with the glasscode command. Glasscode writes its other messages to the console that started it.";
    UnsavedCopies in Advanced: "Unsaved copies", "When Glasscode quit and could not ask, the editor kept a copy of each file with unsaved changes here. Glasscode never removes them.";
    FirstRun in Advanced: "Run first run again", "Its steps once more: projects, agents and the look. What you have stays, and each step starts from it.";
}

impl CardId {
    /// A card of a project's own file (the scope "Project" of Git and projects).
    fn of_project_file(self) -> bool {
        matches!(
            self,
            CardId::ProjectSetup
                | CardId::ProjectBranch
                | CardId::ProjectFolder
                | CardId::ProjectAgent
        )
    }

    /// The kind of action of a Permissions rule card.
    fn rule(self) -> Option<Kind> {
        Some(match self {
            CardId::RuleRead => Kind::Read,
            CardId::RuleBoard => Kind::Board,
            CardId::RuleStart => Kind::Start,
            CardId::RuleRename => Kind::Rename,
            CardId::RuleSend => Kind::Send,
            CardId::RuleImport => Kind::Import,
            CardId::RuleClose => Kind::Close,
            CardId::RuleRemove => Kind::Remove,
            CardId::RuleMove => Kind::Move,
            CardId::RuleTickets => Kind::Tickets,
            CardId::RuleGithub => Kind::Github,
            CardId::RuleShow => Kind::Show,
            CardId::RuleSettings => Kind::Settings,
            _ => return None,
        })
    }
}

/// The sentence of a rule card: the rule Glasscode suggests for it.
fn suggested(kind: Kind) -> &'static str {
    match kind.default_rule() {
        Rule::Allow => "Suggested: Allow.",
        Rule::Ask => "Suggested: Ask.",
        Rule::Never => "Suggested: Never.",
    }
}

/// Whether a card is found by the words typed: each word, in any case, is in its title or its
/// sentence. No words find nothing (the page then shows its section).
pub(crate) fn finds(query: &str, title: &str, sentence: &str) -> bool {
    let text = format!("{title} {sentence}").to_lowercase();
    let mut words = query.split_whitespace().peekable();
    words.peek().is_some() && words.all(|word| text.contains(&word.to_lowercase()))
}

/// The cards the search shows, in the order of the page.
pub(crate) fn search<'a>(cards: &'a [CardInfo], query: &str) -> Vec<&'a CardInfo> {
    cards
        .iter()
        .filter(|c| finds(query, c.title, c.sentence))
        .collect()
}

/// The section that shows. The Board's "Connect Linear" opens Accounts and asks for Linear first
/// (`linear_first`, set in `tickets_view.rs`); Linear is in Integrations now.
fn shown_section(section: Section, linear_first: bool) -> Section {
    if section == Section::Accounts && linear_first {
        Section::Integrations
    } else {
        section
    }
}

/// Below this width (the window's, divided by the scale, as `theme::sz` counts) the section menu
/// is rows of chips above the cards instead of a column beside them.
const NARROW_BELOW: f32 = 800.;

fn narrow(window_width: f32, scale: f32) -> bool {
    window_width / scale < NARROW_BELOW
}

/// A path to show, with the home folder as `~`: shorter, and the name of the account is not on
/// the screen.
pub(crate) fn tilde(path: &Path) -> String {
    match path.strip_prefix(glasscode_core::paths::home()) {
        Ok(rest) => Path::new("~").join(rest).display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

/// Git and projects shows your own settings, or the values of one project's own file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Scope {
    #[default]
    Me,
    /// A project, by id.
    Project(String),
}

/// Whether a card of Git and projects shows: your own cards for "For me", the file's for a
/// project. (The search has no scope: it shows both.)
fn in_scope(card: CardId, scope: &Scope) -> bool {
    (*scope == Scope::Me) != card.of_project_file()
}

/// A project's folder, as the background read found it.
#[derive(Clone, Debug, PartialEq)]
struct Repo {
    root: PathBuf,
    git: bool,
    /// Its GitHub repository (`owner/name`).
    slug: Option<String>,
}

/// Whether the projects' folders must be read (again): never read, or the projects changed.
fn repos_stale(roots: &[PathBuf], read: Option<&[Repo]>) -> bool {
    read.is_none_or(|read| {
        read.len() != roots.len()
            || read
                .iter()
                .zip(roots)
                .any(|(repo, root)| repo.root != *root)
    })
}

#[derive(Default)]
pub struct SettingsUi {
    pub section: Section,
    /// The inputs Accounts uses (adding an account, a commit email) and its sign-in terminal.
    pub form: Option<AccountsForm>,
    /// The search field of the menu, made at the first visit.
    search: Option<Entity<TextInput>>,
    /// What the search field holds. Empty: the page shows its section.
    query: String,
    scroll: ScrollHandle,
    menu_scroll: ScrollHandle,
    /// Each project's folder: in git or not, and its GitHub repository. `repo_slug` runs git, so
    /// it never runs in the render (the backdrop draws the page ten times a second); Settings
    /// reads the folders in the background when it opens and when the projects change.
    repos: Option<Vec<Repo>>,
    repos_loading: bool,
    /// Git and projects: your settings, or one project's file.
    scope: Scope,
    /// How many copies of unsaved text `~/.glasscode/unsaved` holds, counted in the background
    /// each time Settings opens (none: not counted yet).
    unsaved_copies: Option<usize>,
}

/// A card of the page: what it is (title, sentence) and its control in the head, then a body
/// (content that draws itself), its rows, and a footer line. It lies on strong glass, so its
/// words are `text` and `card_soft_text`; controls keep their own opaque ground.
pub(crate) struct Frame {
    title: AnyElement,
    sentence: SharedString,
    control: Option<AnyElement>,
    body: Option<AnyElement>,
    rows: Vec<Div>,
    footer: Option<SharedString>,
}

pub(crate) fn card(title: impl Into<SharedString>, sentence: impl Into<SharedString>) -> Frame {
    card_with(
        div()
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::text())
            .child(title.into()),
        sentence,
    )
}

/// A card whose title is more than words.
pub(crate) fn card_with(title: impl IntoElement, sentence: impl Into<SharedString>) -> Frame {
    Frame {
        title: title.into_any_element(),
        sentence: sentence.into(),
        control: None,
        body: None,
        rows: Vec::new(),
        footer: None,
    }
}

impl Frame {
    pub(crate) fn control(mut self, control: impl IntoElement) -> Self {
        self.control = Some(control.into_any_element());
        self
    }

    pub(crate) fn body(mut self, body: impl IntoElement) -> Self {
        self.body = Some(body.into_any_element());
        self
    }

    pub(crate) fn rows(mut self, rows: impl IntoIterator<Item = Div>) -> Self {
        self.rows.extend(rows);
        self
    }

    pub(crate) fn footer(mut self, text: impl Into<SharedString>) -> Self {
        self.footer = Some(text.into());
        self
    }

    pub(crate) fn render(self) -> AnyElement {
        let radius = theme::sz(ui::CARD_RADIUS);
        let head = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(theme::sz(12.))
            .px(theme::sz(16.))
            .py(theme::sz(14.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(theme::sz(200.))
                    .gap(theme::sz(3.))
                    .child(self.title)
                    .when(!self.sentence.is_empty(), |d| {
                        d.child(
                            div()
                                .text_size(theme::sz(12.5))
                                .text_color(theme::card_soft_text())
                                .child(self.sentence),
                        )
                    }),
            )
            // A wide control (the four fonts) wraps its segments in a narrow card.
            .when_some(self.control, |d, control| {
                d.child(div().flex().max_w_full().child(control))
            });
        backdrop::glass_strong(radius)
            .flex()
            .flex_col()
            .min_w_0()
            .when(theme::is_light(), |d| d.shadow_md())
            .child(head)
            .when_some(self.body, |d, body| {
                d.child(div().px(theme::sz(16.)).pb(theme::sz(16.)).child(body))
            })
            .children(
                self.rows
                    .into_iter()
                    .map(|row| row.border_t_1().border_color(theme::glass_border())),
            )
            .when_some(self.footer, |d, footer| {
                d.child(
                    div()
                        .px(theme::sz(16.))
                        .py(theme::sz(9.))
                        .border_t_1()
                        .border_color(theme::glass_border())
                        .rounded_b(radius)
                        .bg(theme::tint())
                        .text_size(theme::sz(12.5))
                        .text_color(theme::card_soft_text())
                        .child(footer),
                )
            })
            .into_any_element()
    }
}

/// A card of rows (the account groups and the GitHub accounts draw themselves with it).
pub(crate) fn group(title: &str, note: &str, rows: Vec<Div>) -> AnyElement {
    card(title.to_string(), note.to_string())
        .rows(rows)
        .render()
}

pub(crate) fn row(
    label: impl Into<SharedString>,
    detail: impl Into<SharedString>,
    control: Option<AnyElement>,
) -> Div {
    row_with(
        div()
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::text())
            .child(label.into()),
        detail,
        control,
    )
}

/// A row of a card whose title is more than words (a name with its badges).
pub(crate) fn row_with(
    title: impl IntoElement,
    detail: impl Into<SharedString>,
    control: Option<AnyElement>,
) -> Div {
    let detail: SharedString = detail.into();
    div()
        .flex()
        .items_center()
        .flex_wrap()
        .gap(theme::sz(12.))
        .min_h(theme::sz(52.))
        .px(theme::sz(16.))
        .py(theme::sz(10.))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(theme::sz(200.))
                .gap(theme::sz(2.))
                .child(title)
                .when(!detail.is_empty(), |d| {
                    d.child(
                        div()
                            .text_size(theme::sz(12.5))
                            .text_color(theme::card_soft_text())
                            .child(detail),
                    )
                }),
        )
        .when_some(control, |d, c| d.child(c))
}

fn mono(text: impl Into<SharedString>) -> AnyElement {
    div()
        .font_family(theme::MONO_FONT)
        .text_size(theme::sz(12.))
        .text_color(theme::text_soft())
        .child(text.into())
        .into_any_element()
}

/// The heading of the page: the section (or the search) on strong glass, as all words are here.
fn heading(
    icon: &'static [u8],
    title: impl Into<SharedString>,
    sentence: impl Into<SharedString>,
) -> AnyElement {
    backdrop::glass_strong(theme::sz(ui::CARD_RADIUS))
        .flex()
        .items_center()
        .gap(theme::sz(14.))
        .px(theme::sz(18.))
        .py(theme::sz(14.))
        .when(theme::is_light(), |d| d.shadow_md())
        .child(icons::icon(
            icon,
            theme::sz(20.),
            theme::sz(20.),
            theme::text_soft(),
        ))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(theme::sz(2.))
                .child(
                    div()
                        .text_size(theme::sz(20.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(title.into()),
                )
                .child(
                    div()
                        .text_size(theme::sz(13.))
                        .text_color(theme::card_soft_text())
                        .child(sentence.into()),
                ),
        )
        .into_any_element()
}

/// A row of the section menu (and, in a narrow window, a chip of it).
fn menu_item(
    id: impl Into<SharedString>,
    icon: &'static [u8],
    label: &'static str,
    on: bool,
    count: Option<usize>,
) -> gpui::Stateful<Div> {
    div()
        .id(gpui::ElementId::Name(id.into()))
        .flex()
        .flex_none()
        .items_center()
        .gap(theme::sz(9.))
        .h(theme::sz(32.))
        .px(theme::sz(10.))
        .rounded(theme::sz(8.))
        .cursor_pointer()
        .text_size(theme::sz(13.5))
        .text_color(theme::text())
        .when(on, |d| {
            d.bg(theme::selected()).font_weight(FontWeight::SEMIBOLD)
        })
        .when(!on, |d| d.hover(|s| s.bg(theme::tint())))
        .child(icons::icon(
            icon,
            theme::sz(15.),
            theme::sz(15.),
            if on {
                theme::text()
            } else {
                theme::text_soft()
            },
        ))
        .child(div().whitespace_nowrap().child(label))
        .when_some(count, |d, n| {
            d.child(div().flex_1()).child(
                div()
                    .text_size(theme::sz(12.))
                    .text_color(theme::card_soft_text())
                    .child(n.to_string()),
            )
        })
}

/// What the moving backdrop costs, under its choice (measured in U24 and U25; the owner accepted
/// it).
const BACKDROP_COST: &str = "The moving backdrop uses processor time while the window is active: about a fifth of one core on Today, more in a long chat. It stands still when the window is in the back, and “Backdrop behind sessions” off keeps it out of sessions.";

impl Workspace {
    pub fn open_settings(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.form.is_none() {
            self.settings.form = Some(AccountsForm::new(window, cx));
        }
        if let Some(form) = self.settings.form.as_mut() {
            form.linear_first = false;
        }
        if self.settings.section != section {
            self.settings.scroll.set_offset(point(px(0.), px(0.)));
        }
        self.settings.section = section;
        // A section chosen: the search is over.
        self.settings_clear_search(cx);
        if section == Section::Server && self.server.connected() {
            self.phones_refresh(cx);
        }
        if section == Section::Browser {
            self.browser_refresh(cx);
        }
        // Whether the manager's voice key is kept: asked once here, never while drawing.
        if section == Section::Manager {
            self.manager_voice_keys_check(cx);
        }
        self.github.editing = None;
        self.github_accounts_refresh(cx);
        self.tickets.picker = None;
        self.linear_load_choices(cx);
        // A remote may have changed since the last visit to the projects, and a project's own
        // settings file with it.
        if section == Section::Projects {
            self.settings.repos = None;
            self.project_files_refresh(cx);
        }
        self.settings_repos_refresh(cx);
        self.settings_count_unsaved(cx);
        self.show_view(crate::shell::View::Settings, cx);
    }

    /// Count the copies of unsaved text for Advanced, in the background (a folder read).
    fn settings_count_unsaved(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move { glasscode_core::save::kept_copies(&glasscode_core::paths::unsaved_dir()) });
        cx.spawn(async move |this, cx| {
            let count = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.settings.unsaved_copies != Some(count) {
                    this.settings.unsaved_copies = Some(count);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Settings at Appearance (the theme command of Jump, and `GLASSCODE_DEBUG_THEMES`).
    pub fn open_appearance(&mut self, cx: &mut Context<Self>) {
        self.settings.section = Section::Appearance;
        self.settings.scroll.set_offset(point(px(0.), px(0.)));
        self.settings_clear_search(cx);
        self.show_view(crate::shell::View::Settings, cx);
    }

    /// Out of Settings: back where the person came from, else to the sessions.
    fn leave_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history.0.is_empty() {
            self.go(crate::shell::View::Sessions, window, cx);
        } else {
            self.go_back(window, cx);
        }
    }

    /// The search field, made at the first use. Esc in it clears it.
    fn settings_search_input(&mut self, cx: &mut Context<Self>) -> Entity<TextInput> {
        if let Some(input) = self.settings.search.as_ref() {
            return input.clone();
        }
        let input = cx.new(|cx| TextInput::new("Search settings", cx));
        cx.subscribe(
            &input,
            |this: &mut Workspace, input, event: &InputEvent, cx| match event {
                InputEvent::Changed => {
                    this.settings.query = input.read(cx).text();
                    this.settings.scroll.set_offset(point(px(0.), px(0.)));
                    cx.notify();
                }
                InputEvent::Cancel => input.update(cx, |input, cx| input.set_text("", cx)),
                InputEvent::Submit => {}
            },
        )
        .detach();
        self.settings.search = Some(input.clone());
        input
    }

    fn settings_clear_search(&mut self, cx: &mut Context<Self>) {
        self.settings.query.clear();
        if let Some(input) = self.settings.search.clone() {
            input.update(cx, |input, cx| input.set_text("", cx));
        }
    }

    /// Read the projects' folders in the background (git: is it a repository, and which one on
    /// GitHub), when they were never read or the projects changed.
    fn settings_repos_refresh(&mut self, cx: &mut Context<Self>) {
        let roots: Vec<PathBuf> = self.state.projects.iter().map(|p| p.root.clone()).collect();
        if self.settings.repos_loading || !repos_stale(&roots, self.settings.repos.as_deref()) {
            return;
        }
        self.settings.repos_loading = true;
        let task = cx.background_spawn(async move {
            roots
                .into_iter()
                .map(|root| {
                    let git = root.join(".git").exists();
                    let slug = git
                        .then(|| glasscode_core::github::repo_slug(&root))
                        .flatten();
                    Repo { root, git, slug }
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let repos = task.await;
            let _ = this.update(cx, |this, cx| {
                this.settings.repos_loading = false;
                this.settings.repos = Some(repos);
                cx.notify();
            });
        })
        .detach();
    }

    pub fn render_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // Appearance opens without a window at hand (Jump, the theme hook): the form comes here.
        if self.settings.form.is_none() {
            self.settings.form = Some(AccountsForm::new(window, cx));
        }
        let search = self.settings_search_input(cx);
        self.settings_repos_refresh(cx);
        let linear_first = self.settings.form.as_ref().is_some_and(|f| f.linear_first);
        let current = shown_section(self.settings.section, linear_first);
        let query = self.settings.query.trim().to_string();
        let narrow = narrow(self.window_width, theme::scale());

        let mut column: Vec<AnyElement> = Vec::new();
        if narrow {
            column.push(self.settings_menu_chips(current, query.is_empty(), search.clone(), cx));
        }
        if query.is_empty() {
            column.push(heading(current.icon(), current.label(), current.sentence()));
            if current == Section::Projects {
                column.extend(self.settings_scope(cx));
            }
            column.extend(self.settings_section(current, window, cx));
        } else {
            column.extend(self.settings_found(&query, window, cx));
        }
        let content = div().w_full().flex().justify_center().child(
            div()
                .w_full()
                .max_w(theme::sz(800.))
                .flex()
                .flex_col()
                .gap(theme::sz(12.))
                .px(theme::sz(if narrow { 12. } else { 20. }))
                .pt(theme::sz(12.))
                .pb(theme::sz(40.))
                .children(column),
        );
        let scroll = ui::scroll_area("settings-scroll", &self.settings.scroll, content);
        let page = if narrow {
            div().flex().flex_col().flex_1().min_h_0().child(scroll)
        } else {
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(self.settings_menu(current, query.is_empty(), search, cx))
                .child(scroll)
        };
        let layer = self.backdrop_layer(0.8, window, cx);
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
            .child(page)
            .into_any_element()
    }

    /// The menu beside the cards: the way back to work (the left sidebar does not show in
    /// Settings), the search, and the sections.
    fn settings_menu(
        &self,
        current: Section,
        browsing: bool,
        search: Entity<TextInput>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let items: Vec<AnyElement> = SECTIONS
            .into_iter()
            .map(|(section, label, _, icon)| {
                menu_item(
                    format!("settings-nav-{label}"),
                    icon,
                    label,
                    browsing && section == current,
                    self.settings_count(section),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_settings(section, window, cx)
                }))
                .into_any_element()
            })
            .collect();
        div()
            .flex_none()
            .h_full()
            .pl(theme::sz(12.))
            .py(theme::sz(12.))
            // In a short window (900 x 560 at 100 %) the menu is taller than its column: it
            // scrolls, with the thumb of `scroll_area` to show that it does.
            .child(
                backdrop::glass_strong(theme::sz(ui::CARD_RADIUS))
                    .flex()
                    .flex_col()
                    .w(theme::sz(236.))
                    .h_full()
                    .when(theme::is_light(), |d| d.shadow_md())
                    .child(ui::scroll_area(
                        "settings-menu",
                        &self.settings.menu_scroll,
                        div()
                            .flex()
                            .flex_col()
                            .gap(theme::sz(1.))
                            .p(theme::sz(8.))
                            .child(self.settings_back(cx))
                            .child(div().py(theme::sz(6.)).child(search))
                            .children(items),
                    )),
            )
            .into_any_element()
    }

    /// The menu in a narrow window: the way back, the search, and the sections as chips that
    /// wrap, above the cards.
    fn settings_menu_chips(
        &self,
        current: Section,
        browsing: bool,
        search: Entity<TextInput>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chips: Vec<AnyElement> = SECTIONS
            .into_iter()
            .map(|(section, label, _, icon)| {
                menu_item(
                    format!("settings-chip-{label}"),
                    icon,
                    label,
                    browsing && section == current,
                    None,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_settings(section, window, cx)
                }))
                .into_any_element()
            })
            .collect();
        backdrop::glass_strong(theme::sz(ui::CARD_RADIUS))
            .flex()
            .flex_col()
            .gap(theme::sz(8.))
            .p(theme::sz(8.))
            .when(theme::is_light(), |d| d.shadow_md())
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(theme::sz(8.))
                    .child(self.settings_back(cx))
                    .child(div().flex_1().min_w(theme::sz(160.)).child(search)),
            )
            .child(div().flex().flex_wrap().gap(theme::sz(4.)).children(chips))
            .into_any_element()
    }

    fn settings_back(&self, cx: &mut Context<Self>) -> AnyElement {
        menu_item("settings-back", icons::BACK, "Back to work", false, None)
            .on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| this.leave_settings(window, cx)),
            )
            .into_any_element()
    }

    fn settings_count(&self, section: Section) -> Option<usize> {
        match section {
            Section::Accounts => Some(self.state.accounts.len()),
            Section::Projects => Some(self.state.projects.len()),
            _ => None,
        }
    }

    /// The cards of a section, in the order of the table.
    fn settings_section(
        &self,
        section: Section,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut out = Vec::new();
        let scope = self.settings.scope.clone();
        for info in catalog().iter().filter(|c| c.section == section) {
            if section == Section::Projects && !in_scope(info.card, &scope) {
                continue;
            }
            out.extend(self.settings_card(info, window, cx));
            // The terminal that signs an account in, or installs a CLI, under the card that
            // started it.
            if matches!(info.card, CardId::AgentAccounts | CardId::CliVersions) {
                out.extend(self.settings_terminal(cx));
            }
        }
        out
    }

    fn settings_terminal(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.settings
            .form
            .as_ref()
            .and_then(|form| crate::dialogs::accounts_terminal(form, cx))
    }

    /// The search: the cards found in each section, under the section's name (a click opens it).
    fn settings_found(
        &self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let cards = catalog();
        let found = search(&cards, query);
        let mut out = vec![heading(
            icons::SEARCH,
            "Search",
            format!("Settings matching “{query}”"),
        )];
        for (section, label, _, icon) in SECTIONS {
            let mut shown: Vec<AnyElement> = Vec::new();
            for info in found.iter().filter(|c| c.section == section) {
                shown.extend(self.settings_card(info, window, cx));
            }
            if shown.is_empty() {
                continue;
            }
            out.push(
                div()
                    .flex()
                    .pt(theme::sz(6.))
                    .child(
                        backdrop::glass_strong(theme::sz(8.)).child(
                            menu_item(format!("settings-found-{label}"), icon, label, false, None)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.open_settings(section, window, cx)
                                })),
                        ),
                    )
                    .into_any_element(),
            );
            out.extend(shown);
        }
        if out.len() == 1 {
            out.push(
                card(
                    "Nothing matches",
                    "Try a shorter word, or one of the sections.",
                )
                .render(),
            );
        }
        out.extend(self.settings_terminal(cx));
        out
    }

    /// The switch of a setting kept in the state (most say `…_off`, so on is `!off`).
    fn setting_switch(
        &self,
        card: CardId,
        on: bool,
        flip: fn(&mut State),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        ui::switch(SharedString::from(format!("settings-{card:?}")), on)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                flip(&mut this.state);
                this.save();
                cx.notify();
            }))
            .into_any_element()
    }

    /// Segments for a setting with a few values.
    fn choice<T: PartialEq + Copy + 'static>(
        &self,
        card: CardId,
        options: Vec<(T, &'static str)>,
        current: T,
        set: fn(&mut State, T),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        ui::segmented()
            .children(options.into_iter().enumerate().map(|(i, (value, label))| {
                ui::segment(
                    SharedString::from(format!("settings-{card:?}-{i}")),
                    value == current,
                    label,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    set(&mut this.state, value);
                    this.save();
                    cx.notify();
                }))
            }))
            .into_any_element()
    }

    /// The cards of one entry of the table: one card, several (an account group for each agent,
    /// one card for each project), or none (the iPhone without a server, voice on Windows). The
    /// first run draws the Appearance cards with it too.
    pub(crate) fn settings_card(
        &self,
        info: &CardInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let id = info.card;
        let frame = || card(info.title, info.sentence);
        let s = &self.state;
        let framed = match id {
            CardId::Startup => frame().control(self.choice(id, vec![(StartupMode::Restore, "Restore everything"), (StartupMode::RestoreAndNewClaude, "Restore + a new Claude")], s.startup, |s, m| s.startup = m, cx)),
            CardId::NotifyNeeds => frame()
                .control(self.setting_switch(id, !s.notify.needs_input_off, |s| s.notify.needs_input_off = !s.notify.needs_input_off, cx))
                .footer(format!("Notices show at {}, unless you are looking at that session.", platform::notice_corner())),
            CardId::NotifyFinished => frame().control(self.setting_switch(id, !s.notify.finished_off, |s| s.notify.finished_off = !s.notify.finished_off, cx)),
            CardId::NotifyCi => frame().control(self.setting_switch(id, !s.prefs.notify_ci_off, |s| s.prefs.notify_ci_off = !s.prefs.notify_ci_off, cx)),
            CardId::NotifyTickets => frame().control(self.setting_switch(id, !s.linear.notify_off, |s| s.linear.notify_off = !s.linear.notify_off, cx)),
            CardId::NotifySound => frame().control(self.setting_switch(id, !s.notify.sound_off, |s| s.notify.sound_off = !s.notify.sound_off, cx)),
            CardId::AutoNames => frame()
                .control(self.setting_switch(id, !s.auto_names_off, |s| s.auto_names_off = !s.auto_names_off, cx))
                .footer("A new worktree's branch follows, renamed only before its first push. Names you give stay."),
            CardId::ConfirmClose => frame()
                .control(self.setting_switch(id, !s.prefs.confirm_close_off, |s| s.prefs.confirm_close_off = !s.prefs.confirm_close_off, cx))
                .footer(format!("{close} asks first; a second {close} closes it.", close = crate::keymap::label(&crate::workspace::CloseSession))),
            CardId::Editor => frame().control(self.editor_choice(cx)),

            CardId::Theme => frame().body(self.theme_choices(cx)),
            CardId::Accent => frame().control(self.accent_choices(cx)),
            CardId::Backdrop => frame().control(self.appearance_control(id, cx)).footer(BACKDROP_COST),
            CardId::Finish
            | CardId::BackdropInSessions
            | CardId::Scale
            | CardId::TextSize
            | CardId::Density
            | CardId::UiFont
            | CardId::CodeFont
            | CardId::ChatFont
            | CardId::SessionView
            | CardId::Cursor => frame().control(self.appearance_control(id, cx)),

            CardId::Manager => return self.settings_manager(window, cx),
            CardId::AgentAccounts => {
                let Some(form) = self.settings.form.as_ref() else { return Vec::new() };
                return crate::dialogs::account_groups(self, form, cx);
            }
            CardId::GithubAccounts => {
                let Some(form) = self.settings.form.as_ref() else { return Vec::new() };
                return vec![crate::github_accounts_ui::render_github_section(self, &form.github_email, window, cx)];
            }

            CardId::RuleRead
            | CardId::RuleBoard
            | CardId::RuleStart
            | CardId::RuleRename
            | CardId::RuleSend
            | CardId::RuleImport
            | CardId::RuleClose
            | CardId::RuleRemove
            | CardId::RuleMove
            | CardId::RuleTickets
            | CardId::RuleGithub
            | CardId::RuleShow
            | CardId::RuleSettings => {
                let Some(kind) = id.rule() else { return Vec::new() };
                let framed = frame().control(self.rule_control(kind, cx));
                if s.agents.rule(kind) != kind.default_rule() { framed.footer("Changed from the suggestion.") } else { framed }
            }
            CardId::AgentsSkill => frame().control(ui::switch("settings-agents-skill", !s.agents.skill_off).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.state.agents.skill_off = !this.state.agents.skill_off;
                this.save();
                this.control_write_files();
                cx.notify();
            }))),
            CardId::AgentsOutside => frame().control(self.setting_switch(id, s.agents.outside_allowed, |s| s.agents.outside_allowed = !s.agents.outside_allowed, cx)),
            CardId::ControlStatus => {
                let status = match (&self.control.error, self.control.listening) {
                    (Some(e), _) => e.clone(),
                    (None, true) => format!("Listening on {}.", glasscode_core::control::socket_path().display()),
                    (None, false) => "Not started.".into(),
                };
                frame().footer(status)
            }

            CardId::Projects => return self.settings_projects(cx),
            CardId::FreshMain => frame()
                .control(self.setting_switch(id, !s.prefs.fresh_main_off, |s| s.prefs.fresh_main_off = !s.prefs.fresh_main_off, cx))
                .footer(format!("Off: from main as it is on {}.", platform::this_computer())),
            CardId::DeleteBranch => frame()
                .control(self.setting_switch(id, !s.prefs.keep_branch_on_remove, |s| s.prefs.keep_branch_on_remove = !s.prefs.keep_branch_on_remove, cx))
                .footer("Removing runs in the background: the dialog closes, the row says Removing… and you carry on."),
            CardId::ProjectSetup | CardId::ProjectBranch | CardId::ProjectFolder | CardId::ProjectAgent => {
                let Some(project) = self.scope_project() else { return Vec::new() };
                return vec![self.project_file_card(info, &project, cx)];
            }

            // The form is the Board's (`tickets_ui.rs`), with words in colours that are not for
            // glass: it gets an opaque ground of its own.
            CardId::Linear => frame().body(
                ui::card().p(theme::sz(14.)).child(crate::tickets_ui::render_linear_section(self, cx)),
            ),

            CardId::FixCi => frame().control(self.setting_switch(id, !s.ci_autofix_off, |s| s.ci_autofix_off = !s.ci_autofix_off, cx)),
            CardId::FixConflicts => frame().control(self.setting_switch(id, !s.conflict_autofix_off, |s| s.conflict_autofix_off = !s.conflict_autofix_off, cx)),
            CardId::FixTries => frame().control(self.choice(id, vec![(1, "1"), (2, "2"), (3, "3"), (5, "5")], s.prefs.fix_tries(), |s, n| s.prefs.fix_tries = Some(n), cx)),
            CardId::CiWait => frame().control(self.choice(id, vec![(5, "5 min"), (10, "10 min"), (20, "20 min")], (s.prefs.ci_wait().as_secs() / 60) as u32, |s, m| s.prefs.ci_wait_minutes = Some(m), cx)),
            CardId::AutoReview => frame().rows(self.auto_review_rows(cx)),
            CardId::MoveTicket => frame().control(self.setting_switch(id, !s.linear.move_on_start_off, |s| s.linear.move_on_start_off = !s.linear.move_on_start_off, cx)),
            CardId::AgentsLog => frame().control(self.setting_switch(id, !s.agents.log_off, |s| s.agents.log_off = !s.agents.log_off, cx)),
            CardId::Morning => frame().control(self.setting_switch(id, !s.agents.morning_off, |s| s.agents.morning_off = !s.agents.morning_off, cx)),

            CardId::BrowserState => frame().body(crate::browser_ui::browser_state(self, cx)),
            CardId::BrowserAgents => {
                let framed = frame()
                    .control(crate::browser_ui::agents_switch(cx))
                    .footer("Agents work in background tabs, many at once, and never bring the browser forward. Sessions started before a change keep what they had.");
                match crate::browser_ui::browser_tabs(self, cx) {
                    Some(tabs) => framed.body(tabs),
                    None => framed,
                }
            }
            CardId::BrowserReset => frame().control(crate::browser_ui::reset_control(self, cx)),

            CardId::Server => frame().rows(self.server_rows(cx)),
            CardId::Phones => {
                if !self.server.connected() {
                    return Vec::new();
                }
                frame().rows(self.phone_rows(cx))
            }
            CardId::Voice => {
                if !crate::glasscode_ui::VOICE {
                    return Vec::new();
                }
                frame()
            }

            CardId::UpdateStatus => frame().rows(self.update_rows(cx)),
            CardId::Channel => frame().control(self.choice(
                id,
                vec![("stable", "Stable"), ("beta", "Beta")],
                if s.prefs.update_channel() == "beta" { "beta" } else { "stable" },
                |s, c| s.prefs.update_channel = (c == "beta").then(|| c.to_string()),
                cx,
            )),
            CardId::InstallOnQuit => frame().control(self.setting_switch(id, !s.prefs.install_on_quit_off, |s| s.prefs.install_on_quit_off = !s.prefs.install_on_quit_off, cx)),
            CardId::CliVersions => frame().rows(crate::dialogs::cli_rows(self, cx).into_iter().map(|r| div().px(theme::sz(16.)).py(theme::sz(10.)).child(r))),

            CardId::Shortcuts => frame().rows(shortcuts().into_iter().map(|(label, key)| row(label, "", Some(ui::kbd(key).into_any_element())))),

            // One folder holds state.json, activity.json and agent-log.jsonl: the app has no log
            // folder of its own (its other messages go to the console).
            CardId::GlasscodeFolder => frame()
                .control(ui::button("settings-open-folder", "Open the Glasscode folder").on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                    let folder = glasscode_core::paths::root();
                    crate::center::start_or_toast(format!("Could not open {}", tilde(&folder)), move || glasscode_core::sys::open_path(&folder), cx);
                })))
                .footer(tilde(&glasscode_core::paths::root())),
            // Only while there are copies: the editor writes one for each unsaved file when a quit
            // cannot ask, and nothing removes them (the person decides).
            CardId::UnsavedCopies => {
                let count = self.settings.unsaved_copies.unwrap_or(0);
                if count == 0 {
                    return Vec::new();
                }
                frame()
                    .control(ui::button("settings-open-unsaved", "Open folder").on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                        let folder = glasscode_core::paths::unsaved_dir();
                        crate::center::start_or_toast(format!("Could not open {}", tilde(&folder)), move || glasscode_core::sys::open_path(&folder), cx);
                    })))
                    .footer(format!(
                        "{} in {}",
                        if count == 1 { "1 copy".to_string() } else { format!("{count} copies") },
                        tilde(&glasscode_core::paths::unsaved_dir())
                    ))
            }
            CardId::FirstRun => frame().control(ui::button("settings-first-run", "Run first run again").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.open_first_run(crate::first_run::Step::Projects, cx);
            }))),
        };
        vec![framed.render()]
    }

    fn rule_control(&self, kind: Kind, cx: &mut Context<Self>) -> AnyElement {
        let current = self.state.agents.rule(kind);
        ui::segmented()
            .children(
                [
                    (Rule::Allow, "Allow"),
                    (Rule::Ask, "Ask"),
                    (Rule::Never, "Never"),
                ]
                .map(|(rule, label)| {
                    let on = rule == current;
                    ui::segment(
                        SharedString::from(format!("agent-rule-{}-{label}", kind.key())),
                        on,
                        label,
                    )
                    .when(on && rule == Rule::Never, |d| {
                        d.text_color(theme::removed())
                    })
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.state.agents.set(kind, rule);
                            this.save();
                            cx.notify();
                        },
                    ))
                }),
            )
            .into_any_element()
    }

    fn editor_choice(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.state.prefs.editor.clone();
        let mut options: Vec<(Option<String>, &'static str)> =
            vec![(None, "Whichever is installed")];
        options.extend(
            glasscode_core::prefs::EDITORS
                .iter()
                .map(|(id, name, _)| (Some(id.to_string()), *name)),
        );
        ui::segmented()
            .children(options.into_iter().enumerate().map(|(i, (id, name))| {
                ui::segment(
                    SharedString::from(format!("editor-{i}")),
                    id == current,
                    name,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.state.prefs.editor = id.clone();
                    this.save();
                    cx.notify();
                }))
            }))
            .into_any_element()
    }

    /// Opt-in per repository: each repository with a current review request, or already opted in.
    fn auto_review_rows(&self, cx: &mut Context<Self>) -> Vec<Div> {
        let repos = self.auto_review_repos();
        if repos.is_empty() {
            return vec![row(
                "No repositories yet",
                "A repository appears here once a pull request asks for your review.",
                None,
            )];
        }
        repos
            .into_iter()
            .map(|repo| {
                let r = repo.clone();
                let control = ui::switch(
                    SharedString::from(format!("auto-review-{repo}")),
                    self.auto_review_on(&repo),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if !this.state.auto_review_repos.remove(&r) {
                        this.state.auto_review_repos.insert(r.clone());
                    }
                    this.save();
                    cx.notify();
                }))
                .into_any_element();
                row(
                    repo,
                    "Review a pull request that asks for you",
                    Some(control),
                )
            })
            .collect()
    }

    /// Switch a project off (hidden from the sidebar, the CLI and Glasscode) or back on. Nothing on
    /// disk changes; sessions keep running. When the active session belonged to a project just
    /// hidden, move to a still-visible session so the center pane never shows a hidden one.
    pub fn set_project_hidden(&mut self, project_id: &str, hidden: bool, cx: &mut Context<Self>) {
        let Some(name) = self.state.project(project_id).map(|p| p.name.clone()) else {
            return;
        };
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
        let sessions = self
            .state
            .sessions
            .iter()
            .filter(|s| s.project_id == project_id)
            .count();
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

    /// A card for each project: shown or hidden, its repository, its GitHub account, where its
    /// worktrees go, and cleaning them up. The repository comes from the background read.
    fn settings_projects(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if self.state.projects.is_empty() {
            return vec![
                card(
                    "No projects yet",
                    "Add one with “Add project…” at the end of the left sidebar.",
                )
                .render(),
            ];
        }
        let logins: Vec<String> = self
            .github
            .list
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|a| a.login)
            .collect();
        let default = self.state.github.default.clone();
        let mut out: Vec<AnyElement> = Vec::new();
        for project in &self.state.projects {
            // Off hides this project (its worktrees, sessions and usage) from the sidebar, the
            // CLI and Glasscode; it stays listed here so it can be switched back on.
            let hidden = self.state.is_hidden(&project.id);
            let toggle_id = project.id.clone();
            let visibility = row(
                "Show in Glasscode",
                "Off: hidden from the sidebar, the glasscode CLI and Glasscode. The folder, branches and sessions stay.",
                Some(
                    ui::switch(
                        SharedString::from(format!("project-visible-{}", project.id)),
                        !hidden,
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_project_hidden(&toggle_id, !this.state.is_hidden(&toggle_id), cx)
                    }))
                    .into_any_element(),
                ),
            );
            let mut rows = vec![visibility];
            match self
                .settings
                .repos
                .as_ref()
                .and_then(|repos| repos.iter().find(|r| r.root == project.root))
            {
                None => rows.push(row(
                    "Repository",
                    "Reading the folder…",
                    Some(mono(tilde(&project.root))),
                )),
                Some(repo) if !repo.git => {
                    let id = project.id.clone();
                    rows.push(row(
                        "Not in git yet",
                        "Sessions work in the folder itself. Set up git to get worktrees, branches and pull requests.",
                        Some(ui::button(SharedString::from(format!("setup-git-{}", project.id)), "Set up git…").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_up_git(&id, cx))).into_any_element()),
                    ));
                }
                Some(repo) => {
                    let on_github = repo
                        .slug
                        .as_ref()
                        .map(|r| format!("{r} on GitHub"))
                        .unwrap_or_else(|| "Not on GitHub".into());
                    rows.push(row(
                        "Repository",
                        on_github,
                        Some(mono(tilde(&project.root))),
                    ));
                    let mut choices: Vec<(Option<String>, String)> = vec![(
                        None,
                        format!(
                            "Default · {}",
                            default.clone().unwrap_or_else(|| "gh's own".into())
                        ),
                    )];
                    choices.extend(
                        logins
                            .iter()
                            .filter(|l| Some(*l) != default.as_ref())
                            .map(|l| (Some(l.clone()), l.clone())),
                    );
                    let picked = project.github_account.clone();
                    let accounts = ui::segmented().children(choices.into_iter().enumerate().map(
                        |(i, (login, label))| {
                            let id = project.id.clone();
                            ui::segment(
                                SharedString::from(format!("project-gh-{}-{i}", project.id)),
                                login == picked,
                                label,
                            )
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, _, cx| {
                                    this.set_project_github(&id, login.clone(), cx)
                                },
                            ))
                        },
                    ));
                    rows.push(row(
                        "GitHub account",
                        "Pushes, pull requests, gh and the commit email for every session here",
                        Some(accounts.into_any_element()),
                    ));
                    // The project's own file may name the folder (Project · its name, above).
                    let folder = self
                        .project_file(&project.id)
                        .and_then(|l| l.settings.worktree_folder.clone());
                    rows.push(row(
                        "New worktrees go in",
                        if folder.is_some() {
                            "As the project's settings file says"
                        } else {
                            ""
                        },
                        Some(mono(match folder {
                            Some(template) => format!("~/.glasscode/worktrees/{template}"),
                            None => format!("~/.glasscode/worktrees/{}", project.name),
                        })),
                    ));
                    let id = project.id.clone();
                    rows.push(row(
                        "Clean up worktrees",
                        "Pick the ones you no longer need; each is checked for unsaved work first",
                        Some(
                            ui::button(
                                SharedString::from(format!("cleanup-{}", project.id)),
                                "Clean up…",
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.open_remove_worktrees(id.clone(), None, window, cx)
                            }))
                            .into_any_element(),
                        ),
                    ));
                }
            }
            let title = div()
                .flex()
                .items_center()
                .gap(theme::sz(8.))
                .child(icons::icon(
                    icons::FOLDER,
                    theme::sz(15.),
                    theme::sz(15.),
                    theme::text_soft(),
                ))
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::text())
                        .child(project.name.clone()),
                );
            out.push(
                card_with(
                    title,
                    if hidden {
                        "Hidden: switched off below."
                    } else {
                        ""
                    },
                )
                .rows(rows)
                .render(),
            );
        }
        out
    }

    /// The project whose file the scope "Project" shows; in the search (no scope), the active
    /// session's project, else the first one.
    fn scope_project(&self) -> Option<Project> {
        let id = match &self.settings.scope {
            Scope::Project(id) => id.clone(),
            Scope::Me => self
                .active_session()
                .map(|s| s.project_id.clone())
                .or_else(|| self.state.projects.first().map(|p| p.id.clone()))?,
        };
        self.state.project(&id).cloned()
    }

    fn set_settings_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        self.settings.scope = scope;
        self.settings.scroll.set_offset(point(px(0.), px(0.)));
        // The file is read again in the background; the page shows the last read until then.
        self.project_files_refresh(cx);
        cx.notify();
    }

    /// The switch "For me / Project · <name>" of Git and projects; for a project, where its
    /// values come from, the file's state and what is wrong in it.
    fn settings_scope(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if let Scope::Project(id) = &self.settings.scope {
            if self.state.project(id).is_none() {
                self.settings.scope = Scope::Me;
            }
        }
        let scope = self.settings.scope.clone();
        let mut choices: Vec<(Scope, String)> = vec![(Scope::Me, "For me".into())];
        choices.extend(self.state.visible_projects().map(|p| {
            (
                Scope::Project(p.id.clone()),
                format!("Project · {}", p.name),
            )
        }));
        let switch = ui::segmented().children(choices.into_iter().enumerate().map(
            |(i, (choice, label))| {
                ui::segment(
                    SharedString::from(format!("settings-scope-{i}")),
                    choice == scope,
                    label,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_settings_scope(choice.clone(), cx)
                }))
            },
        ));
        let Some(project) = self
            .state
            .project(match &scope {
                Scope::Project(id) => id,
                Scope::Me => "",
            })
            .cloned()
        else {
            return vec![
                card(
                    "For me",
                    "Your own settings, for every project. A project can bring its own setup script, branch and folder names and default agent in its .glasscode/settings.json: pick the project to see them.",
                )
                .control(switch)
                .render(),
            ];
        };
        let file = glasscode_core::project_settings::file(&project.root);
        let loaded = self.project_files.files.get(&project.root);
        let state = loaded.map_or(glasscode_core::project_settings::FileState::Missing, |l| {
            l.state
        });
        let root = project.root.clone();
        let mut rows = vec![match state {
            glasscode_core::project_settings::FileState::Missing => row(
                "No file yet",
                "Glasscode uses its own defaults. Make the file, fill in the values you want, and commit it.",
                Some(
                    ui::button("project-file-make", "Make the file")
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.project_file_make(root.clone(), cx)
                        }))
                        .into_any_element(),
                ),
            ),
            _ => row(
                "Edit it in your editor",
                "Glasscode reads it again when Settings opens these values and when a session starts.",
                Some(
                    ui::button("project-file-open", "Open the file")
                        .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                            // Not in the click handler: on Windows, opening a file there can let
                            // the system call back into the window while it is borrowed (U17).
                            let root = root.clone();
                            cx.spawn(async move |this, cx| {
                                let _ =
                                    this.update(cx, |this, cx| this.project_file_open(&root, cx));
                            })
                            .detach();
                        }))
                        .into_any_element(),
                ),
            ),
        }];
        // What is wrong in it: the values that are not used, or the whole file.
        for warning in loaded.map(|l| l.warnings.clone()).unwrap_or_default() {
            rows.push(row_with(
                div()
                    .flex()
                    .items_center()
                    .gap(theme::sz(8.))
                    .child(ui::dot(theme::removed()))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text())
                            .child(
                                if state == glasscode_core::project_settings::FileState::Broken {
                                    "The file is not used"
                                } else {
                                    "A value is not used"
                                },
                            ),
                    ),
                format!("settings.json {warning}."),
                None,
            ));
        }
        vec![
            card(
                format!("Project · {}", project.name),
                format!(
                    "These values come from {} in the repository, so everyone on the project gets them, and they win over Glasscode's own. The file is meant to be committed (git add -f .glasscode/settings.json, where git ignores .glasscode/).",
                    tilde(&file)
                ),
            )
            .control(switch)
            .rows(rows)
            .render(),
        ]
    }

    /// Open a project's settings file in the editor of Settings › General, else in the program
    /// the system has for it.
    fn project_file_open(&mut self, root: &Path, cx: &mut Context<Self>) {
        let file = glasscode_core::project_settings::file(root);
        let (program, env, root) = (self.preferred_editor().map(|(_, program)| program), self.env.clone(), root.to_path_buf());
        // Out of the click handler that asked (`center::start_outside`).
        crate::center::start_or_toast(
            format!("Could not open {}", tilde(&file)),
            move || match program {
                Some(program) => glasscode_core::sys::command(program).arg(&root).arg(&file).envs(env.iter()).spawn().map(|_| ()),
                None => glasscode_core::sys::open_path(&file),
            },
            cx,
        );
    }

    /// Make a project's settings file with each key empty (never over one that exists), then
    /// open it.
    fn project_file_make(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let made_in = root.clone();
        let task =
            cx.background_spawn(async move { glasscode_core::project_settings::create(&made_in) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(_) => this.project_file_open(&root, cx),
                    Err(error) => this.show_toast(format!("Could not make the file: {error}"), cx),
                }
                this.project_files_refresh(cx);
            });
        })
        .detach();
    }

    /// A card of a project's own file: its value, the value in effect and where that comes from,
    /// and for the setup, what you answered on this computer.
    fn project_file_card(
        &self,
        info: &CardInfo,
        project: &Project,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use glasscode_core::project_settings::{self as ps, Source};
        let values = self
            .project_files
            .files
            .get(&project.root)
            .map(|l| l.settings.clone())
            .unwrap_or_default();
        let not_set = || {
            div()
                .text_size(theme::sz(12.5))
                .text_color(theme::card_soft_text())
                .child("Not set")
                .into_any_element()
        };
        let in_file = |value: Option<String>| {
            row(
                "In the file",
                "",
                Some(value.map(mono).unwrap_or_else(not_set)),
            )
        };
        let effect = |source: Source, value: String| {
            row(
                "In effect",
                crate::project_setup::source_words(source),
                Some(mono(value)),
            )
        };
        let framed = match info.card {
            CardId::ProjectSetup => {
                let key = ps::approval_key(&project.root);
                let answer = match (&values.setup, self.state.setup_approvals.get(&key)) {
                    (_, Some(a)) if a.never => Some((
                        "Never for this project",
                        "Nothing runs and nothing asks on this computer.",
                        true,
                    )),
                    // The approval covers the command and the files of the worktree it runs, as
                    // they were; whether they still are is seen when a new worktree reads them.
                    (Some(_), Some(a)) if a.approved.is_some() => Some((
                        "Allowed on this computer",
                        "It runs in a new worktree without asking while the command and the files it runs stay as you allowed them; a change asks again.",
                        true,
                    )),
                    (Some(_), _) => Some((
                        "Not allowed yet",
                        "The next new worktree shows the command and asks first.",
                        false,
                    )),
                    (None, _) => None,
                };
                let mut rows = vec![in_file(values.setup.clone())];
                if let Some((said, detail, forget)) = answer {
                    let control = forget.then(|| {
                        ui::button(
                            SharedString::from(format!("setup-ask-again-{}", project.id)),
                            "Ask again",
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.state.setup_approvals.remove(&key);
                            this.save();
                            cx.notify();
                        }))
                        .into_any_element()
                    });
                    rows.push(row(said, detail, control));
                }
                card(info.title, info.sentence).rows(rows).footer(format!(
                    "It runs with {} in the new worktree before the agent, and its output shows in the session. Only a hash of a command you allow is kept, in state.json.",
                    crate::project_setup::shell_name()
                ))
            }
            CardId::ProjectBranch => {
                let template = values.branch_template.clone();
                let (identifier, title) = ("ENG-212", "First run import");
                let default = glasscode_core::linear::branch_for(identifier, title);
                let example = template
                    .as_deref()
                    .and_then(|t| {
                        ps::branch_name(
                            t,
                            &ps::BranchFor {
                                project_name: &project.name,
                                default: &default,
                                issue: Some(identifier),
                                title: Some(title),
                                labels: &[],
                            },
                        )
                    })
                    .unwrap_or_else(|| default.clone());
                let (shown, source) = ps::in_effect(
                    template.clone(),
                    None,
                    format!("{default}, or glasscode/<words of the title>"),
                );
                card(info.title, info.sentence)
                    .rows([
                        in_file(template),
                        effect(source, shown),
                        row("For example", format!("A session for {identifier} “{title}”"), Some(mono(example))),
                    ])
                    .footer("A branch you name yourself stays as you wrote it. Characters git or Windows refuse become -.")
            }
            CardId::ProjectFolder => {
                let template = values.worktree_folder.clone();
                let worktrees = glasscode_core::paths::worktrees_dir();
                let branch = "feat/eng-212-first-run-import";
                let example =
                    ps::worktree_dir(&worktrees, &project.name, branch, template.as_deref());
                let (shown, source) =
                    ps::in_effect(template.clone(), None, ps::DEFAULT_FOLDER.to_string());
                card(info.title, info.sentence)
                    .rows([
                        in_file(template),
                        effect(source, format!("{}/{shown}", tilde(&worktrees))),
                        row("For example", format!("The branch {branch}"), Some(mono(tilde(&example)))),
                    ])
                    .footer("Always below Glasscode's worktree folder: .. , a drive, and characters a path cannot have are taken out.")
            }
            CardId::ProjectAgent => {
                let (agent, source) = ps::in_effect(
                    values.default_agent,
                    None,
                    glasscode_core::model::AgentKind::Claude,
                );
                card(info.title, info.sentence)
                    .rows([
                        in_file(values.default_agent.map(|a| ps::agent_word(a).to_string())),
                        effect(source, agent.name().to_string()),
                    ])
                    .footer("New session and glasscode session new start with it when no agent is chosen.")
            }
            _ => card(info.title, info.sentence),
        };
        framed.render()
    }

    fn server_rows(&self, cx: &mut Context<Self>) -> Vec<Div> {
        let connected = self.server.connected();
        let has_server = self.server.config.is_some();
        let (status, detail) = if connected {
            (
                format!("{} · online", self.server.name()),
                format!(
                    "Sessions there keep running when {} is closed or asleep",
                    platform::this_computer()
                ),
            )
        } else if has_server {
            (
                format!("{} · not connected", self.server.name()),
                "Glasscode keeps trying by itself".to_string(),
            )
        } else {
            (
                "No server yet".to_string(),
                format!(
                    "An always-on machine where sessions keep running while {} sleeps",
                    platform::this_computer()
                ),
            )
        };
        let controls =
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .when(has_server && !connected, |d| {
                    d.child(ui::button("settings-reconnect", "Reconnect").on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.server_connect(cx)),
                    ))
                })
                .child(
                    ui::button(
                        "settings-server",
                        if has_server {
                            "Server settings…"
                        } else {
                            "Set up a server…"
                        },
                    )
                    .on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| this.open_server_dialog(window, cx),
                    )),
                )
                .into_any_element();
        let mut rows = vec![row(status, detail, Some(controls))];
        if has_server {
            rows.push(row(
                format!(
                    "Sessions on {} in sync on the server",
                    platform::this_computer()
                ),
                self.mirror_status()
                    .unwrap_or_else(|| "Synced as they change, so the iPhone sees them".into()),
                None,
            ));
        }
        rows
    }

    fn phone_rows(&self, cx: &mut Context<Self>) -> Vec<Div> {
        let mut rows: Vec<Div> = self
            .phones
            .clone()
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(i, phone)| {
                let key = phone.key.clone();
                let short = phone
                    .key
                    .split_whitespace()
                    .nth(1)
                    .map(|k| format!("…{}", &k[k.len().saturating_sub(10)..]))
                    .unwrap_or_default();
                row(
                    phone.name.clone(),
                    format!("Paired · key {short}"),
                    Some(
                        ui::button(SharedString::from(format!("phone-remove-{i}")), "Remove")
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.phone_remove(key.clone(), cx)
                            }))
                            .into_any_element(),
                    ),
                )
            })
            .collect();
        rows.push(row(
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
        rows
    }

    fn update_rows(&self, cx: &mut Context<Self>) -> Vec<Div> {
        let build = glasscode_core::update::describe(
            glasscode_core::update::VERSION,
            glasscode_core::update::BUILD,
        );
        let state = match (&self.update.ready, &self.update.error) {
            (Some((build, version, _)), _) => row(
                format!(
                    "Ready: {}",
                    glasscode_core::update::describe(version, *build)
                ),
                "Downloaded and checked. It goes in when Glasscode quits, or now:",
                Some(
                    ui::primary_button("settings-restart", "Restart to update")
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.update_restart(cx)),
                        )
                        .into_any_element(),
                ),
            ),
            (None, Some(err)) => row(
                "Could not check for updates",
                err.clone(),
                Some(
                    ui::button("settings-retry", "Retry")
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.update_check_now(cx)),
                        )
                        .into_any_element(),
                ),
            ),
            (None, None) if self.update.checking => row("Checking for updates…", "", None),
            (None, None) => row(
                "Up to date",
                "",
                Some(
                    ui::button("settings-check", "Check now")
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.update_check_now(cx)),
                        )
                        .into_any_element(),
                ),
            ),
        };
        vec![row("This build", "", Some(mono(build))), state]
    }
}

/// The keys of the Keyboard section, with their labels for this computer. Read only: they
/// cannot be changed yet.
fn shortcuts() -> Vec<(&'static str, String)> {
    use crate::keymap::label;
    use crate::workspace::{
        AddProject, CloseSession, GoBack, GoForward, Handoff, NewSession, NextSession,
        OpenAccounts, OpenPalette, OpenSettings, PreviousSession, Quit, ToggleLeftPanel,
        ToggleRightPanel,
    };
    let paste = label(&crate::composer::Paste);
    let mut keys = vec![
        ("Search and commands", label(&OpenPalette)),
        ("New session", label(&NewSession)),
        ("Close session", label(&CloseSession)),
        ("Next session", label(&NextSession)),
        ("Previous session", label(&PreviousSession)),
        ("Back", label(&GoBack)),
        ("Forward", label(&GoForward)),
        ("Settings", label(&OpenSettings)),
        ("Settings at Accounts", label(&OpenAccounts)),
        ("Add a project", label(&AddProject)),
        ("Hand off", label(&Handoff)),
        ("Show or hide the left sidebar", label(&ToggleLeftPanel)),
        ("Show or hide the side panel", label(&ToggleRightPanel)),
        ("Paste text, an image or files", paste.clone()),
    ];
    // Windows leaves Ctrl+C and Ctrl+V to the program in the terminal; it copies and pastes there with other keys.
    let terminal_paste = label(&crate::terminal::Paste);
    if terminal_paste != paste {
        keys.push(("Copy in the terminal", label(&crate::terminal::Copy)));
        keys.push(("Paste in the terminal", terminal_paste));
    }
    keys.push((
        "Clear the terminal",
        label(&crate::terminal::ClearScrollback),
    ));
    keys.push(("Quit", label(&Quit)));
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use glasscode_core::control::AgentSettings;
    use glasscode_core::github_accounts::GithubSettings;
    use glasscode_core::linear::LinearSettings;
    use glasscode_core::model::{Appearance, Defaults, NotifySettings, Project, TicketSettings};
    use glasscode_core::prefs::Prefs;

    /// Where a saved setting shows in Settings: its cards, or why it has none.
    enum Shown {
        Cards(Vec<CardId>),
        Not(&'static str),
    }

    fn on<T>(_field: T, cards: &[CardId]) -> Shown {
        Shown::Cards(cards.to_vec())
    }

    fn not<T>(_field: T, why: &'static str) -> Shown {
        Shown::Not(why)
    }

    fn rule_cards() -> Vec<CardId> {
        catalog()
            .iter()
            .map(|c| c.card)
            .filter(|c| c.rule().is_some())
            .collect()
    }

    /// Each saved setting and where Settings shows it. The patterns name every field (no `..`)
    /// and an unused one is an error here, so a new field does not compile until it gets a
    /// card or a reason for having none.
    #[deny(unused_variables)]
    fn saved_settings() -> Vec<(&'static str, Shown)> {
        let State {
            projects,
            accounts,
            sessions,
            defaults,
            active_session,
            startup,
            github,
            ci_autofix_off,
            conflict_autofix_off,
            auto_review_repos,
            custom_titles,
            auto_names_off,
            layout,
            notify,
            linear,
            ticket_of,
            tickets,
            prefs,
            agents,
            appearance,
            pinned,
            hidden_projects,
            setup_approvals,
            first_run_done,
            manager,
            worktrees_moving,
            worktrees_elsewhere,
        } = State::default();
        let Prefs {
            editor,
            confirm_close_off,
            fix_tries,
            ci_wait_minutes,
            update_channel,
            install_on_quit_off,
            notify_ci_off,
            keep_branch_on_remove,
            fresh_main_off,
            new_on_server,
            glasscode_relay_off,
        } = prefs;
        let Appearance {
            theme,
            accent,
            text_size,
            density,
            ui_font,
            code_font,
            cursor,
            chat_font,
            session_view,
            backdrop,
            finish,
            backdrop_in_sessions,
            scale,
        } = appearance;
        let NotifySettings {
            finished_off,
            needs_input_off,
            sound_off,
        } = notify;
        let AgentSettings {
            rules,
            outside_allowed,
            log_off,
            skill_off,
            morning_off,
        } = agents;
        let LinearSettings {
            me,
            me_label,
            reviewer_label,
            review_prs_off,
            move_on_start_off,
            notify_off,
            agent,
        } = linear;
        let GithubSettings { default, emails } = github;
        let Defaults { claude, codex } = defaults;
        let TicketSettings {
            provider_by_repo,
            linear_team,
            github_repo,
            last_provider,
        } = tickets;
        let Project {
            id,
            name,
            root,
            expanded,
            github_account,
        } = Project {
            id: String::new(),
            name: String::new(),
            root: PathBuf::new(),
            expanded: true,
            github_account: None,
        };
        vec![
            ("State.projects", on(projects, &[CardId::Projects])),
            ("State.accounts", on(accounts, &[CardId::AgentAccounts])),
            ("State.sessions", not(sessions, "the sessions themselves")),
            (
                "State.active_session",
                not(active_session, "the session on the screen"),
            ),
            ("State.startup", on(startup, &[CardId::Startup])),
            ("State.ci_autofix_off", on(ci_autofix_off, &[CardId::FixCi])),
            (
                "State.conflict_autofix_off",
                on(conflict_autofix_off, &[CardId::FixConflicts]),
            ),
            (
                "State.auto_review_repos",
                on(auto_review_repos, &[CardId::AutoReview]),
            ),
            (
                "State.custom_titles",
                not(
                    custom_titles,
                    "the sessions the person renamed; renaming a session sets it",
                ),
            ),
            (
                "State.auto_names_off",
                on(auto_names_off, &[CardId::AutoNames]),
            ),
            (
                "State.layout",
                not(
                    layout,
                    "the panels of the window: their handles and buttons set it",
                ),
            ),
            (
                "State.ticket_of",
                not(ticket_of, "the ticket each session started from"),
            ),
            (
                "State.pinned",
                not(pinned, "the pins of the sidebar, set there"),
            ),
            (
                "State.hidden_projects",
                on(hidden_projects, &[CardId::Projects]),
            ),
            (
                "State.setup_approvals",
                on(setup_approvals, &[CardId::ProjectSetup]),
            ),
            (
                "State.first_run_done",
                on(first_run_done, &[CardId::FirstRun]),
            ),
            ("State.manager", on(manager, &[CardId::Manager])),
            ("State.worktrees_moving", not(worktrees_moving, "moves of the Server mode switch under way, finished after a restart")),
            ("State.worktrees_elsewhere", not(worktrees_elsewhere, "the worktrees the server owns, as this Mac last heard")),
            ("Defaults.claude", on(claude, &[CardId::AgentAccounts])),
            ("Defaults.codex", on(codex, &[CardId::AgentAccounts])),
            ("Project.id", not(id, "the project's own key")),
            ("Project.name", on(name, &[CardId::Projects])),
            ("Project.root", on(root, &[CardId::Projects])),
            (
                "Project.expanded",
                not(expanded, "the fold of the sidebar, set there"),
            ),
            (
                "Project.github_account",
                on(github_account, &[CardId::Projects]),
            ),
            ("Prefs.editor", on(editor, &[CardId::Editor])),
            (
                "Prefs.confirm_close_off",
                on(confirm_close_off, &[CardId::ConfirmClose]),
            ),
            ("Prefs.fix_tries", on(fix_tries, &[CardId::FixTries])),
            (
                "Prefs.ci_wait_minutes",
                on(ci_wait_minutes, &[CardId::CiWait]),
            ),
            (
                "Prefs.update_channel",
                on(update_channel, &[CardId::Channel]),
            ),
            (
                "Prefs.install_on_quit_off",
                on(install_on_quit_off, &[CardId::InstallOnQuit]),
            ),
            (
                "Prefs.notify_ci_off",
                on(notify_ci_off, &[CardId::NotifyCi]),
            ),
            (
                "Prefs.keep_branch_on_remove",
                on(keep_branch_on_remove, &[CardId::DeleteBranch]),
            ),
            (
                "Prefs.fresh_main_off",
                on(fresh_main_off, &[CardId::FreshMain]),
            ),
            (
                "Prefs.new_on_server",
                not(
                    new_on_server,
                    "the new-session dialog remembers where the last session ran; choosing there changes it",
                ),
            ),
            (
                "Prefs.glasscode_relay_off",
                not(
                    glasscode_relay_off,
                    "nothing reads it since the hosted relay was removed, so a switch would do nothing",
                ),
            ),
            ("Appearance.theme", on(theme, &[CardId::Theme])),
            ("Appearance.accent", on(accent, &[CardId::Accent])),
            ("Appearance.text_size", on(text_size, &[CardId::TextSize])),
            ("Appearance.density", on(density, &[CardId::Density])),
            ("Appearance.ui_font", on(ui_font, &[CardId::UiFont])),
            ("Appearance.code_font", on(code_font, &[CardId::CodeFont])),
            ("Appearance.cursor", on(cursor, &[CardId::Cursor])),
            ("Appearance.chat_font", on(chat_font, &[CardId::ChatFont])),
            (
                "Appearance.session_view",
                on(session_view, &[CardId::SessionView]),
            ),
            ("Appearance.backdrop", on(backdrop, &[CardId::Backdrop])),
            ("Appearance.finish", on(finish, &[CardId::Finish])),
            (
                "Appearance.backdrop_in_sessions",
                on(backdrop_in_sessions, &[CardId::BackdropInSessions]),
            ),
            ("Appearance.scale", on(scale, &[CardId::Scale])),
            (
                "NotifySettings.finished_off",
                on(finished_off, &[CardId::NotifyFinished]),
            ),
            (
                "NotifySettings.needs_input_off",
                on(needs_input_off, &[CardId::NotifyNeeds]),
            ),
            (
                "NotifySettings.sound_off",
                on(sound_off, &[CardId::NotifySound]),
            ),
            ("AgentSettings.rules", on(rules, &rule_cards())),
            (
                "AgentSettings.outside_allowed",
                on(outside_allowed, &[CardId::AgentsOutside]),
            ),
            ("AgentSettings.log_off", on(log_off, &[CardId::AgentsLog])),
            (
                "AgentSettings.skill_off",
                on(skill_off, &[CardId::AgentsSkill]),
            ),
            (
                "AgentSettings.morning_off",
                on(morning_off, &[CardId::Morning]),
            ),
            ("LinearSettings.me", on(me, &[CardId::Linear])),
            ("LinearSettings.me_label", on(me_label, &[CardId::Linear])),
            (
                "LinearSettings.reviewer_label",
                on(reviewer_label, &[CardId::Linear]),
            ),
            (
                "LinearSettings.review_prs_off",
                on(review_prs_off, &[CardId::Linear]),
            ),
            (
                "LinearSettings.move_on_start_off",
                on(move_on_start_off, &[CardId::MoveTicket, CardId::Linear]),
            ),
            (
                "LinearSettings.notify_off",
                on(notify_off, &[CardId::NotifyTickets, CardId::Linear]),
            ),
            ("LinearSettings.agent", on(agent, &[CardId::Linear])),
            (
                "GithubSettings.default",
                on(default, &[CardId::GithubAccounts, CardId::Projects]),
            ),
            (
                "GithubSettings.emails",
                on(emails, &[CardId::GithubAccounts]),
            ),
            (
                "TicketSettings.provider_by_repo",
                not(provider_by_repo, "chosen on the Board"),
            ),
            (
                "TicketSettings.linear_team",
                not(linear_team, "chosen on the Board"),
            ),
            (
                "TicketSettings.github_repo",
                not(github_repo, "chosen on the Board"),
            ),
            (
                "TicketSettings.last_provider",
                not(last_provider, "what the Board showed last"),
            ),
        ]
    }

    #[test]
    fn each_saved_setting_has_a_card_or_a_reason() {
        let cards: Vec<CardId> = catalog().iter().map(|c| c.card).collect();
        let settings = saved_settings();
        assert!(settings.len() > 60);
        for (field, shown) in settings {
            match shown {
                Shown::Cards(on) => {
                    assert!(!on.is_empty(), "{field} has no card");
                    for card in on {
                        assert!(
                            cards.contains(&card),
                            "{field}: {card:?} is not on the page"
                        );
                    }
                }
                Shown::Not(why) => assert!(!why.is_empty(), "{field} has no card and no reason"),
            }
        }
    }

    /// `GLASSCODE_DEBUG_OPEN=settings:<name>` reaches each of the 13 sections, and the names from
    /// before still work.
    #[test]
    fn a_hook_names_each_section() {
        for (section, label, ..) in SECTIONS {
            assert_eq!(Section::named(&label.to_lowercase().replace(' ', "-")), Some(section), "{label}");
            assert_eq!(Section::named(label), Some(section), "{label}");
        }
        for (name, section) in [
            ("permissions", Section::Permissions),
            ("integrations", Section::Integrations),
            ("advanced", Section::Advanced),
            ("appearance", Section::Appearance),
            ("git", Section::Projects),
            ("agents", Section::Accounts),
            ("keyboard", Section::Shortcuts),
            ("server", Section::Server),
            ("manager", Section::Manager),
            // The names before U16.
            ("accounts", Section::Accounts),
            ("projects", Section::Projects),
            ("shortcuts", Section::Shortcuts),
            ("automation", Section::Automation),
            ("browser", Section::Browser),
            ("updates", Section::Updates),
            ("themes", Section::Appearance),
        ] {
            assert_eq!(Section::named(name), Some(section), "{name}");
        }
        assert_eq!(Section::named("nothing"), None);
        assert_eq!(Section::named(""), None);
    }

    /// The 13 rules of what agents may do, each with its own card in Permissions.
    #[test]
    fn each_kind_of_action_has_its_rule_card() {
        let cards = catalog();
        let rules: Vec<&CardInfo> = cards.iter().filter(|c| c.card.rule().is_some()).collect();
        assert_eq!(
            rules
                .iter()
                .filter_map(|c| c.card.rule())
                .collect::<Vec<_>>(),
            Kind::ALL.to_vec()
        );
        for info in rules {
            let kind = info.card.rule().unwrap();
            assert_eq!(
                (info.section, info.title, info.sentence),
                (Section::Permissions, kind.label(), suggested(kind))
            );
        }
    }

    #[test]
    fn the_search_finds_cards_by_title_and_sentence() {
        let cards = catalog();
        let ids = |query: &str| {
            search(&cards, query)
                .iter()
                .map(|c| c.card)
                .collect::<Vec<_>>()
        };
        // No words: nothing, and the page shows its section.
        assert!(ids("").is_empty());
        assert!(ids("   ").is_empty());
        // A word that no card has.
        assert!(ids("xylophone").is_empty());
        // A word only in a sentence.
        assert_eq!(ids("stale"), vec![CardId::FreshMain]);
        assert!(
            !cards
                .iter()
                .any(|c| c.title.to_lowercase().contains("stale"))
        );
        // Case does not count.
        assert_eq!(ids("NOTIFY"), ids("notify"));
        assert!(finds("Sound", "Play a sound", ""));
        // The word of the demonstration finds the notifications of General.
        let notify = ids("notify");
        for card in [
            CardId::NotifyNeeds,
            CardId::NotifyFinished,
            CardId::NotifyCi,
            CardId::NotifyTickets,
        ] {
            assert!(notify.contains(&card), "{card:?}");
        }
        // Each word must be there, in any order, in the order of the page.
        assert_eq!(
            ids("sessions backdrop"),
            vec![CardId::Backdrop, CardId::BackdropInSessions]
        );
    }

    #[test]
    fn the_sections_list_their_cards_in_the_order_of_the_menu() {
        let order = |section: Section| SECTIONS.iter().position(|(s, ..)| *s == section).unwrap();
        let cards = catalog();
        assert!(
            cards
                .windows(2)
                .all(|pair| order(pair[0].section) <= order(pair[1].section))
        );
        for (section, label, ..) in SECTIONS {
            assert!(
                cards.iter().any(|c| c.section == section),
                "{label} has no card"
            );
            assert_eq!(
                SECTIONS.iter().filter(|(s, ..)| *s == section).count(),
                1,
                "{label} twice"
            );
        }
        assert!(
            cards
                .iter()
                .all(|c| !c.title.is_empty() && !c.sentence.is_empty())
        );
        // The cost line names the switch by its title.
        let in_sessions = cards
            .iter()
            .find(|c| c.card == CardId::BackdropInSessions)
            .unwrap();
        assert!(BACKDROP_COST.contains(in_sessions.title));
    }

    /// 900 px at 130 % is about 692 wide: the menu goes above the cards.
    #[test]
    fn the_menu_goes_above_the_cards_in_a_narrow_window() {
        assert!(!narrow(1440., 1.0));
        assert!(!narrow(900., 1.0), "the smallest window at 100 %");
        assert!(narrow(900., 1.15));
        assert!(narrow(900., 1.3));
        assert!(!narrow(1440., 1.3));
    }

    /// The Board's "Connect Linear" opens Accounts with Linear first: Linear is in Integrations.
    #[test]
    fn linear_opens_in_integrations() {
        assert_eq!(
            shown_section(Section::Accounts, true),
            Section::Integrations
        );
        assert_eq!(shown_section(Section::Accounts, false), Section::Accounts);
        assert_eq!(shown_section(Section::General, true), Section::General);
    }

    /// Git and projects: "For me" shows your own cards, a project its file's; the search finds
    /// both, and no other section has a scope.
    #[test]
    fn the_scope_switch_divides_git_and_projects() {
        let cards = catalog();
        let projects: Vec<CardId> = cards
            .iter()
            .filter(|c| c.section == Section::Projects)
            .map(|c| c.card)
            .collect();
        let shown = |scope: &Scope| {
            projects
                .iter()
                .copied()
                .filter(|c| in_scope(*c, scope))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            shown(&Scope::Me),
            vec![CardId::Projects, CardId::FreshMain, CardId::DeleteBranch]
        );
        assert_eq!(
            shown(&Scope::Project("p1".into())),
            vec![
                CardId::ProjectSetup,
                CardId::ProjectBranch,
                CardId::ProjectFolder,
                CardId::ProjectAgent
            ]
        );
        assert!(
            cards
                .iter()
                .filter(|c| c.card.of_project_file())
                .all(|c| c.section == Section::Projects)
        );
        let found: Vec<CardId> = search(&cards, "setup").iter().map(|c| c.card).collect();
        assert_eq!(found, vec![CardId::ProjectSetup]);
        let found: Vec<CardId> = search(&cards, "worktree").iter().map(|c| c.card).collect();
        assert!(
            found.contains(&CardId::ProjectFolder) && found.contains(&CardId::Projects),
            "{found:?}"
        );
    }

    /// The folders are read once, and again only when the projects change.
    #[test]
    fn the_folders_are_read_again_when_the_projects_change() {
        let roots = vec![PathBuf::from("a"), PathBuf::from("b")];
        let read = vec![
            Repo {
                root: "a".into(),
                git: true,
                slug: Some("acme/app".into()),
            },
            Repo {
                root: "b".into(),
                git: false,
                slug: None,
            },
        ];
        assert!(repos_stale(&roots, None), "never read");
        assert!(!repos_stale(&roots, Some(&read)));
        assert!(repos_stale(&roots[..1], Some(&read)), "a project went");
        assert!(
            repos_stale(&[PathBuf::from("b"), PathBuf::from("a")], Some(&read)),
            "the order changed"
        );
    }

    /// A footer (the cost line under the backdrop) is `card_soft_text` on `tint` over strong
    /// glass. U04's test covers the glass alone; this one adds the tint, with U04's method: the
    /// pixel of the backdrop at Settings' strength that gives the least contrast, in each palette.
    #[test]
    fn a_footer_reads_over_each_backdrop() {
        use crate::theme::{
            CLASSIC, CONTRAST, GLASS_DARK, GLASS_LIGHT, Palette, Translucent, c, contrast,
        };
        use glasscode_core::backdrop::{self as pixels, Finish, Preset};
        let linear = |v: f32| {
            let v = v / 255.;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let lay = |under: [f32; 3], over: Translucent| {
            let o = [
                (over.rgb >> 16) as u8,
                (over.rgb >> 8) as u8,
                over.rgb as u8,
            ];
            [0, 1, 2].map(|i| under[i] * (1. - over.alpha) + o[i] as f32 * over.alpha)
        };
        let palettes: [&Palette; 4] = [&GLASS_LIGHT, &GLASS_DARK, &CLASSIC, &CONTRAST];
        let mut lowest = f32::MAX;
        for preset in Preset::ALL {
            for seconds in [0., 7., 23., 61.] {
                let frames = [false, true].map(|light| {
                    pixels::render(preset, Finish::Smooth, light, 0.8, seconds, 320, 200)
                });
                for p in palettes {
                    // Light text is weakest on the brightest surface, dark text on the darkest.
                    let mut worst = (if p.light { f32::MAX } else { f32::MIN }, [0f32; 3]);
                    for px in frames[usize::from(p.light)].bgra.chunks(4) {
                        let surface = lay(
                            lay([px[2], px[1], px[0]].map(f32::from), p.glass_strong),
                            p.tint,
                        );
                        let lum = 0.2126 * linear(surface[0])
                            + 0.7152 * linear(surface[1])
                            + 0.0722 * linear(surface[2]);
                        if (p.light && lum < worst.0) || (!p.light && lum > worst.0) {
                            worst = (lum, surface);
                        }
                    }
                    let [r, g, b] = worst.1.map(|v| v / 255.);
                    let soft = contrast(c(p.card_soft_text), gpui::Rgba { r, g, b, a: 1. }.into());
                    lowest = lowest.min(soft);
                    assert!(
                        soft >= 4.5,
                        "card_soft_text {soft:.2} on a footer: {}, {preset:?}, {seconds} s",
                        p.name
                    );
                }
            }
        }
        eprintln!("lowest contrast of a footer: {lowest:.2}");
    }

    #[test]
    fn paths_show_the_home_folder_as_a_tilde() {
        let home = glasscode_core::paths::home();
        assert_eq!(
            tilde(&home.join(".glasscode").join("state.json")),
            Path::new("~")
                .join(".glasscode")
                .join("state.json")
                .display()
                .to_string()
        );
        let elsewhere = if cfg!(windows) {
            PathBuf::from(r"Z:\work\app")
        } else {
            PathBuf::from("/srv/work/app")
        };
        assert_eq!(tilde(&elsewhere), elsewhere.display().to_string());
    }
}
