//! The first run (slice U18): four steps beside the backdrop that bring the person's projects and
//! logins into Glasscode and choose its look. It shows at launch when there is no project and it
//! was neither finished nor skipped (`State.first_run_done`); Settings › Advanced shows it again.
//!
//! The steps: Projects (the folders `discover::projects` found, a search over all of them, "Add a
//! folder…" and "Clone from GitHub…"), Agents (the logins `discover::logins` found, GitHub through
//! `gh`, Linear through its connect form), Appearance (the cards of Settings › Appearance), and a
//! summary. A tick takes effect at once, so Back, "Skip for now" and a second run all start from
//! what exists, and nothing that existed before is taken away.
//!
//! The system logins (`~/.claude`, `~/.codex`) are entries the person ticks: while the flow shows,
//! Glasscode takes no login by itself, starts no agent program and makes no session (rule 15 of
//! the plan). A tick on a login that is not signed in starts its sign-in at once
//! (`agents::login_launch`) in a terminal in the flow, and Continue waits until it ends: when its
//! login file appears (`discover::signed_in`, every second), or, for Claude on macOS, whose login
//! is in the Keychain, when the sign-in program exits. An exit with no login takes the tick off.
//!
//! Debug builds: `GLASSCODE_DEBUG_OPEN=firstrun[:projects|agents|appearance|summary]` opens it at
//! a step, and `GLASSCODE_DEBUG_DISCOVER_HOME=<folder>` makes it look in a made-up home (with the
//! editors' data in `<folder>/AppData/Roaming`), so that a screen check lists no real project or
//! login. With that home a system login cannot be ticked: its account would be the real one.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use glasscode_core::discover::{self, FoundLogin, FoundProject, LoginState, Source};
use glasscode_core::model::{Account, AgentKind, State, new_id};
use gpui::{
    AnyElement, App, BorderStyle, ClickEvent, Context, Corners, Div, Entity, FontWeight,
    ScrollHandle, SharedString, Stateful, Window, canvas, div, prelude::*, px, quad,
    transparent_black,
};

use crate::backdrop;
use crate::icons::{self, Brand};
use crate::platform;
use crate::settings_ui::CardId;
use crate::shell::View;
use crate::terminal::{SpawnSpec, TerminalEvent, TerminalView};
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

// ----- the steps ---------------------------------------------------------------------------------

/// The steps, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Projects,
    Agents,
    Appearance,
    Summary,
}

impl Step {
    pub const ALL: [Step; 4] = [
        Step::Projects,
        Step::Agents,
        Step::Appearance,
        Step::Summary,
    ];

    /// Its number, from 1.
    pub fn number(self) -> usize {
        Step::ALL.iter().position(|s| *s == self).unwrap_or(0) + 1
    }

    pub fn next(self) -> Option<Step> {
        Step::ALL.get(self.number()).copied()
    }

    pub fn back(self) -> Option<Step> {
        self.number().checked_sub(2).map(|i| Step::ALL[i])
    }

    /// The step a debug hook names: its word or its number.
    pub fn named(word: &str) -> Option<Step> {
        match word {
            "projects" | "1" => Some(Step::Projects),
            "agents" | "2" => Some(Step::Agents),
            "appearance" | "3" => Some(Step::Appearance),
            "summary" | "4" => Some(Step::Summary),
            _ => None,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Step::Projects => "Bring your projects",
            Step::Agents => "Connect your agents",
            Step::Appearance => "Make it yours",
            Step::Summary => "You are set",
        }
    }

    fn lead(self) -> String {
        match self {
            Step::Projects => format!(
                "These folders were found on {}. Tick the ones to add. They stay where they are.",
                platform::this_computer()
            ),
            Step::Agents => format!(
                "Each login found on {} is listed. Tick one that is not signed in and its sign-in opens at once, so all of them work when setup ends.",
                platform::this_computer()
            ),
            Step::Appearance => "Pick how Glasscode looks. It changes as you click, and all of it stays in Settings.".into(),
            Step::Summary => "Everything ticked is signed in and ready. You can change any of this later in Settings.".into(),
        }
    }
}

/// Whether the first run shows at launch: no project yet, and the flow was neither finished nor
/// skipped. A state from before the flow with projects never shows it.
pub fn wanted(state: &State) -> bool {
    !state.first_run_done && state.projects.is_empty()
}

/// The end of the flow in the state: it does not show again by itself. Says whether the system
/// logins are to be taken now (`bootstrap_accounts`), as before the first run existed: when the
/// person skipped it, except with a made-up home (a check), whose logins are not the system's.
/// Finished, the logins are what the person ticked, and no launch takes another by itself.
pub fn close_state(state: &mut State, skipped: bool, made_up: bool) -> bool {
    state.first_run_done = true;
    skipped && !made_up
}

/// The account a ticked login becomes: the CLI's own folder, as `bootstrap_accounts` makes it, or
/// a new folder of Glasscode's named from `id` (a new id).
pub fn account_for(login: &Login, id: &str) -> Account {
    match login.origin {
        Origin::System => Account {
            id: format!("{}-default", login.agent.name().to_lowercase()),
            agent: login.agent,
            label: "My login".into(),
            plan: None,
            system_default: true,
        },
        Origin::Glasscode | Origin::Swap => Account {
            id: format!(
                "{}-{}",
                login.agent.name().to_lowercase(),
                &id[..8.min(id.len())]
            ),
            agent: login.agent,
            label: login.label.clone(),
            plan: None,
            system_default: false,
        },
    }
}

/// Continue goes on, unless a sign-in runs on the Agents step.
pub fn can_continue(step: Step, signing: bool) -> bool {
    step != Step::Agents || !signing
}

// ----- projects ----------------------------------------------------------------------------------

/// A folder the first step lists.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub root: PathBuf,
    pub name: String,
    pub sources: Vec<Source>,
    pub last_used: Option<SystemTime>,
    /// A git repository (its main checkout, or a linked worktree).
    pub git: bool,
    /// The person brought it in this run ("Add a folder…", "Clone from GitHub…").
    pub mine: bool,
}

impl Found {
    /// A folder the person picked or cloned, which no program recorded.
    fn picked(root: PathBuf) -> Found {
        let name = root.file_name().map_or_else(
            || root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let git = root.join(".git").exists();
        Found {
            root,
            name,
            sources: Vec::new(),
            last_used: None,
            git,
            mine: true,
        }
    }
}

/// What a folder's `.git` is.
#[derive(Clone, Debug, PartialEq)]
pub enum Checkout {
    /// A repository of its own (a `.git` folder, or a submodule's `.git` file).
    Main,
    /// A linked worktree, with its main checkout.
    Linked(PathBuf),
    /// No git.
    Plain,
}

/// `path` with its `.` and `..` parts taken away, without a look at the disk.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The main checkout of a linked worktree, from the `gitdir:` line of its `.git` file, which names
/// `<main checkout>/.git/worktrees/<name>`, absolute or from `root`. None for another `.git` file
/// (a submodule's names `.git/modules/…`) and for the worktree of a bare repository.
pub fn linked_main(git_file: &str, root: &Path) -> Option<PathBuf> {
    let target = git_file
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:"))?
        .trim();
    let dir = lexical(&root.join(target));
    let worktrees = dir.parent()?;
    let git = worktrees.parent()?;
    if worktrees.file_name()? != "worktrees" || git.file_name()? != ".git" {
        return None;
    }
    git.parent().map(Path::to_path_buf)
}

/// Reads a folder's `.git` (on a background thread: git itself is not run for each of them).
fn checkout(root: &Path) -> Checkout {
    let git = root.join(".git");
    match std::fs::symlink_metadata(&git) {
        Ok(meta) if meta.is_dir() => Checkout::Main,
        Ok(meta) if meta.is_file() && meta.len() <= 4096 => {
            match std::fs::read_to_string(&git)
                .ok()
                .and_then(|text| linked_main(&text, root))
            {
                Some(main) => Checkout::Linked(main),
                None => Checkout::Main,
            }
        }
        _ => Checkout::Plain,
    }
}

/// What tells two folders apart: on Windows without case, as its file system does.
fn key(path: &Path) -> String {
    let clean = glasscode_core::sys::clean_path(path)
        .to_string_lossy()
        .into_owned();
    if cfg!(windows) {
        clean.to_lowercase()
    } else {
        clean
    }
}

/// The folders as the first step lists them. A linked worktree whose main checkout is in the list
/// is not a project of its own (adding it adds the main checkout): its programs and its last use
/// go to the main checkout. One whose main checkout is not in the list stays. Last used first.
pub fn fold_worktrees(found: Vec<(FoundProject, Checkout)>) -> Vec<Found> {
    let mains: HashMap<String, usize> = found
        .iter()
        .enumerate()
        .map(|(i, (p, _))| (key(&p.root), i))
        .collect();
    let mut out: Vec<Option<Found>> = found
        .iter()
        .map(|(p, c)| {
            Some(Found {
                root: p.root.clone(),
                name: p.name.clone(),
                sources: p.sources.clone(),
                last_used: p.last_used,
                git: *c != Checkout::Plain,
                mine: false,
            })
        })
        .collect();
    for (i, (_, checkout)) in found.iter().enumerate() {
        let Checkout::Linked(main) = checkout else {
            continue;
        };
        let Some(&m) = mains.get(&key(main)).filter(|&&m| m != i) else {
            continue;
        };
        let Some(linked) = out[i].take() else {
            continue;
        };
        if let Some(main) = out[m].as_mut() {
            for source in linked.sources {
                if !main.sources.contains(&source) {
                    main.sources.push(source);
                }
            }
            main.sources.sort();
            main.last_used = main.last_used.max(linked.last_used);
        }
    }
    let mut list: Vec<Found> = out.into_iter().flatten().collect();
    list.sort_by(|a, b| b.last_used.cmp(&a.last_used));
    list
}

/// How many folders the first list shows before a search.
pub const FIRST: usize = 12;
/// The most a search shows.
const SEARCH_MOST: usize = 60;

/// A path to show: under `home` it starts with `~`, which is shorter and keeps the name of the
/// account off the screen.
pub fn short_path(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => Path::new("~").join(rest).display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

/// The rows the first step shows, as places in `rows`, and how many more there are. With no
/// search: the folders the person brought in, then the `FIRST` most recent, git repositories
/// first, then each ticked one that is not among them. With a search: each folder whose name or
/// path has every word.
pub fn shown(
    rows: &[Found],
    query: &str,
    home: &Path,
    ticked: &dyn Fn(&Found) -> bool,
) -> (Vec<usize>, usize) {
    // `rows` is last used first: a stable sort puts the person's own folders first, then the
    // repositories before the other folders.
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|&i| (!rows[i].mine, !rows[i].git));
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        let mut list: Vec<usize> = order.iter().copied().take(FIRST).collect();
        list.extend(
            order
                .iter()
                .copied()
                .skip(FIRST)
                .filter(|&i| ticked(&rows[i])),
        );
        let more = rows.len() - list.len();
        return (list, more);
    }
    let matches: Vec<usize> = order
        .into_iter()
        .filter(|&i| {
            let text =
                format!("{} {}", rows[i].name, short_path(&rows[i].root, home)).to_lowercase();
            words.iter().all(|word| text.contains(word.as_str()))
        })
        .collect();
    let more = matches.len().saturating_sub(SEARCH_MOST);
    (matches.into_iter().take(SEARCH_MOST).collect(), more)
}

fn source_brand(source: Source) -> Brand {
    match source {
        Source::Cursor => Brand::Cursor,
        Source::VsCode => Brand::VsCode,
        Source::ClaudeCode => Brand::Claude,
        Source::Codex => Brand::Codex,
    }
}

// ----- logins ------------------------------------------------------------------------------------

/// Where a login entry comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The CLI's own login folder (`~/.claude`, `~/.codex`).
    System,
    /// An account of Glasscode, with its folder in `~/.glasscode/accounts`.
    Glasscode,
    /// A name that claude-swap saved: a tick signs it in again in a folder of its own.
    Swap,
}

/// A login of the Agents step.
#[derive(Clone, Debug, PartialEq)]
pub struct Login {
    pub agent: AgentKind,
    pub label: String,
    /// Its login folder, once it has one.
    pub dir: Option<PathBuf>,
    pub origin: Origin,
    pub signed_in: bool,
    /// The account it is in Glasscode: the entry is ticked.
    pub account: Option<String>,
    /// An account from before this run: it stays, so its tick cannot be taken off.
    pub kept: bool,
    /// Made in this run by "Sign in to another account": a cancel takes the row away.
    pub made: bool,
    /// Its sign-in runs now.
    pub pending: bool,
}

impl Login {
    pub fn ticked(&self) -> bool {
        self.account.is_some()
    }
}

/// The logins `discover::logins` found, with the accounts Glasscode has: those are ticked.
pub fn logins_from(found: Vec<FoundLogin>, accounts: &[Account]) -> Vec<Login> {
    found
        .into_iter()
        .map(|f| {
            let origin = match f.from.as_str() {
                discover::FROM_SYSTEM => Origin::System,
                discover::FROM_GLASSCODE => Origin::Glasscode,
                _ => Origin::Swap,
            };
            let account = match origin {
                Origin::System => accounts
                    .iter()
                    .find(|a| a.system_default && a.agent == f.agent)
                    .map(|a| a.id.clone()),
                // Its folder is named by its id.
                Origin::Glasscode => f
                    .dir
                    .as_deref()
                    .and_then(Path::file_name)
                    .map(|n| n.to_string_lossy().into_owned())
                    .filter(|id| accounts.iter().any(|a| a.id == *id)),
                Origin::Swap => None,
            };
            Login {
                agent: f.agent,
                label: f.label,
                dir: f.dir,
                origin,
                signed_in: f.state == LoginState::SignedIn,
                kept: account.is_some(),
                account,
                made: false,
                pending: false,
            }
        })
        .collect()
}

/// What a click on a login's tick asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    /// Signed in already: it becomes an account, and nothing runs.
    Adopt,
    /// Not signed in: it becomes an account, and its sign-in starts at once.
    SignIn,
    /// Ticked in this run: its account goes again.
    Untick,
    /// Its sign-in runs: stop it, and the tick goes.
    Cancel,
    /// Another sign-in runs: one at a time.
    Busy,
    /// An account from before this run: it stays.
    Nothing,
}

pub fn tick(logins: &[Login], i: usize) -> Tick {
    let login = &logins[i];
    if login.pending {
        Tick::Cancel
    } else if login.ticked() {
        if login.kept {
            Tick::Nothing
        } else {
            Tick::Untick
        }
    } else if logins.iter().any(|l| l.pending) {
        Tick::Busy
    } else if login.signed_in {
        Tick::Adopt
    } else {
        Tick::SignIn
    }
}

/// Whether a sign-in runs (Continue waits for it).
pub fn signing(logins: &[Login], github: bool) -> bool {
    github || logins.iter().any(|l| l.pending)
}

/// The end of a sign-in: whether it signed in, or None while it still runs. `file` says whether
/// `discover::signed_in` finds the login, `exited` whether the sign-in program ended. On macOS
/// (`mac`) Claude keeps its login in the Keychain and `signed_in` is true once its folder exists,
/// so there only the exit of the program ends the wait, and counts as signed in.
pub fn login_end(mac: bool, agent: AgentKind, exited: bool, file: bool) -> Option<bool> {
    if mac && agent == AgentKind::Claude {
        return exited.then_some(true);
    }
    if file {
        Some(true)
    } else if exited {
        Some(false)
    } else {
        None
    }
}

/// A sign-in ended (`signed`), was cancelled, or the tick was taken off (not `signed`): the entry
/// stops waiting. Without a login, an entry ticked in this run loses its tick, and its account
/// (returned) goes: a row made in this run goes with it, and a claude-swap name starts again from
/// its name, as its folder (which stays on disk) is no longer an account's.
pub fn finish(logins: &mut Vec<Login>, i: usize, signed: bool) -> Option<String> {
    let login = &mut logins[i];
    login.pending = false;
    if signed {
        login.signed_in = true;
        return None;
    }
    if login.kept {
        return None;
    }
    let account = login.account.take();
    if login.made {
        logins.remove(i);
    } else if login.origin == Origin::Swap {
        login.dir = None;
        login.signed_in = false;
    }
    account
}

/// The words of the button that adds an account.
pub fn another_label(agent: AgentKind, has_any: bool) -> String {
    if has_any {
        format!("Sign in to another {} account…", agent.name())
    } else {
        format!("Sign in to a {} account…", agent.name())
    }
}

// ----- the art panel ------------------------------------------------------------------------------

/// The window is wider than this (in logical pixels) for the art panel: below it the steps take
/// the whole width.
const ART_MIN: f32 = 300.;
/// The widest the art panel gets.
const ART_MAX: f32 = 560.;
/// The narrowest the steps get beside it.
pub const FORM_MIN: f32 = 480.;
/// The room around and between the two.
const PAD: f32 = 16.;
const ART_RADIUS: f32 = 16.;

/// The width of the art panel in a window `width` pixels wide at `scale`, in logical pixels; None
/// when the window is too narrow for it and the steps.
pub fn art_width(width: f32, scale: f32) -> Option<f32> {
    let logical = width / scale;
    let art = (logical * 0.4).min(ART_MAX);
    (art >= ART_MIN && logical - art - 3. * PAD >= FORM_MIN).then_some(art)
}

/// The sentence of the art panel.
const SENTENCE: &str = "Every agent in its own worktree, and all of them in one window.";

/// A window `height` pixels high at `scale` is short (900 x 560 at 130 %): the sentence under a
/// step's title goes, so that its list keeps room for a few rows.
pub fn short(height: f32, scale: f32) -> bool {
    height / scale < 480.
}

/// The height of a sign-in's terminal in a window `height` pixels high at `scale`, in logical
/// pixels: what the step leaves above and below it (about 330), between 120 and 300.
pub fn terminal_height(height: f32, scale: f32) -> f32 {
    (height / scale - 330.).clamp(120., 300.)
}

// ----- the flow on screen -------------------------------------------------------------------------

/// What runs in the flow's terminal.
#[derive(Clone, Debug, PartialEq)]
enum Signing {
    /// A login of the list, by its account.
    Login(String),
    /// `gh auth login`.
    Github,
}

struct Running {
    what: Signing,
    title: String,
    terminal: Entity<TerminalView>,
    /// Its number: a poll or an exit of an earlier sign-in does nothing.
    run: u64,
}

/// The first run while it shows (the `Workspace` field).
pub struct FirstRun {
    pub step: Step,
    /// The home it looks in, and whether it is a made-up one (debug builds).
    home: PathBuf,
    made_up: bool,
    /// The folders found, last used first; None while the search runs.
    found: Option<Vec<Found>>,
    /// Folders the person picked or cloned, at the top of the list.
    extra: Vec<Found>,
    /// The folders ticked in this run, by their key, with the project each became.
    ticked: HashMap<String, String>,
    query: String,
    search: Entity<TextInput>,
    /// The field of "Clone from GitHub…" while it is open, and whether a clone runs.
    clone: Option<Entity<TextInput>>,
    cloning: bool,
    /// The logins; None while the search runs.
    logins: Option<Vec<Login>>,
    gh_installed: bool,
    linear_open: bool,
    running: Option<Running>,
    runs: u64,
    scroll: ScrollHandle,
}

impl FirstRun {
    /// The rows of the first step: the picked folders, then the found ones.
    fn rows(&self) -> Vec<Found> {
        let picked: HashSet<String> = self.extra.iter().map(|f| key(&f.root)).collect();
        let mut rows = self.extra.clone();
        rows.extend(
            self.found
                .iter()
                .flatten()
                .filter(|f| !picked.contains(&key(&f.root)))
                .cloned(),
        );
        rows
    }

    fn signing(&self) -> bool {
        signing(
            self.logins.as_deref().unwrap_or_default(),
            self.running
                .as_ref()
                .is_some_and(|r| r.what == Signing::Github),
        )
    }
}

/// The home the first run looks in, its editors' data, and whether it is made up.
fn discovery_home() -> (PathBuf, PathBuf, bool) {
    let made_up = std::env::var("GLASSCODE_DEBUG_DISCOVER_HOME")
        .ok()
        .filter(|v| cfg!(debug_assertions) && !v.is_empty());
    match made_up {
        Some(folder) => {
            let home = PathBuf::from(folder);
            let app_data = home.join("AppData").join("Roaming");
            (home, app_data, true)
        }
        None => {
            let home = glasscode_core::paths::home();
            let app_data = discover::app_data().unwrap_or_else(|| home.join(".config"));
            (home, app_data, false)
        }
    }
}

/// The keys that would make a session or leave the flow do nothing while it shows: it ends only
/// through its own buttons. (Background events cannot leave it either: `Workspace::enter`.)
pub fn hold_keys(root: Stateful<Div>) -> Stateful<Div> {
    use crate::workspace::{
        AddProject, CloseSession, GoBack, GoForward, Handoff, NewSession, NextSession,
        OpenAccounts, OpenPalette, OpenSettings, PreviousSession,
    };
    fn hold<A: gpui::Action>(root: Stateful<Div>) -> Stateful<Div> {
        root.capture_action(|_: &A, _: &mut Window, cx: &mut App| cx.stop_propagation())
    }
    let root = hold::<NewSession>(hold::<CloseSession>(hold::<NextSession>(hold::<
        PreviousSession,
    >(root))));
    let root = hold::<OpenAccounts>(hold::<AddProject>(hold::<Handoff>(hold::<OpenPalette>(
        root,
    ))));
    hold::<GoBack>(hold::<GoForward>(hold::<OpenSettings>(root)))
}

impl Workspace {
    /// Shows the first run at `step`: at launch, from Settings › Advanced, from the debug hook.
    pub fn open_first_run(&mut self, step: Step, cx: &mut Context<Self>) {
        if let Some(flow) = self.first_run.as_mut() {
            flow.step = step;
            self.enter(View::FirstRun);
            cx.notify();
            return;
        }
        let (home, app_data, made_up) = discovery_home();
        let search = cx.new(|cx| TextInput::new("Search all the folders found", cx));
        cx.subscribe(
            &search,
            |this: &mut Workspace, input, event: &InputEvent, cx| match event {
                InputEvent::Changed => {
                    if let Some(flow) = this.first_run.as_mut() {
                        flow.query = input.read(cx).text();
                        flow.scroll.set_offset(gpui::point(px(0.), px(0.)));
                    }
                    cx.notify();
                }
                InputEvent::Cancel => input.update(cx, |input, cx| input.set_text("", cx)),
                InputEvent::Submit => {}
            },
        )
        .detach();
        self.first_run = Some(FirstRun {
            step,
            home: home.clone(),
            made_up,
            found: None,
            extra: Vec::new(),
            ticked: HashMap::new(),
            query: String::new(),
            search,
            clone: None,
            cloning: false,
            logins: None,
            gh_installed: true,
            linear_open: false,
            running: None,
            runs: 0,
            scroll: ScrollHandle::new(),
        });
        self.enter(View::FirstRun);
        // The status bar, and so its popovers, are not in the flow.
        self.popover = None;
        self.github_accounts_refresh(cx);
        // Both searches block (U09: about half a second here, longer with a slow drive).
        let env = self.env.clone();
        let task = cx.background_spawn(async move {
            let found = discover::projects(&home, &app_data);
            let found = fold_worktrees(
                found
                    .into_iter()
                    .map(|p| {
                        let checkout = checkout(&p.root);
                        (p, checkout)
                    })
                    .collect(),
            );
            let logins = discover::logins(&home);
            let gh = glasscode_core::agents::resolve_program("gh", &env).is_some();
            (found, logins, gh)
        });
        cx.spawn(async move |this, cx| {
            let (found, logins, gh) = task.await;
            let _ = this.update(cx, |this, cx| {
                let accounts = this.state.accounts.clone();
                if let Some(flow) = this.first_run.as_mut() {
                    flow.found = Some(found);
                    flow.logins = Some(logins_from(logins, &accounts));
                    flow.gh_installed = gh;
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// `GLASSCODE_DEBUG_OPEN=firstrun[:<step>]`.
    pub fn open_first_run_hook(&mut self, mode: &str, cx: &mut Context<Self>) {
        let step = mode
            .split(':')
            .nth(1)
            .and_then(Step::named)
            .unwrap_or(Step::Projects);
        self.open_first_run(step, cx);
    }

    // ----- the steps' buttons ------------------------------------------------------------------

    fn first_run_continue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(flow) = self.first_run.as_mut() else {
            return;
        };
        if !can_continue(flow.step, flow.signing()) {
            return;
        }
        match flow.step.next() {
            Some(next) => {
                flow.step = next;
                flow.scroll.set_offset(gpui::point(px(0.), px(0.)));
                cx.notify();
            }
            None => self.first_run_close(false, window, cx),
        }
    }

    fn first_run_back(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.first_run.as_mut() else {
            return;
        };
        if let Some(back) = flow.step.back() {
            flow.step = back;
            flow.scroll.set_offset(gpui::point(px(0.), px(0.)));
            cx.notify();
        }
    }

    /// The end of the flow, finished or skipped: it does not show again by itself. A sign-in that
    /// runs stops. Skipped, the system logins are taken as before the first run existed: each one
    /// whose folder exists becomes "My login" when its agent has no account yet. Today follows.
    fn first_run_close(&mut self, skipped: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.first_run.as_ref().is_some_and(|f| f.running.is_some()) {
            self.first_run_end_sign_in(false, cx);
        }
        let Some(flow) = self.first_run.take() else {
            return;
        };
        if close_state(&mut self.state, skipped, flow.made_up) {
            self.state.bootstrap_accounts(true);
        }
        self.save();
        self.go(View::Today, window, cx);
        // Back must not lead into the flow again.
        self.history.0.retain(|place| place.view != View::FirstRun);
        self.history.1.retain(|place| place.view != View::FirstRun);
        // With the new manager's memory, finished with projects: the offer to move in (M35, O10).
        // It asks before it reads anything.
        if !skipped && !self.state.projects.is_empty() {
            self.open_move_in(cx);
        }
    }

    fn first_run_tick_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let Some(flow) = self.first_run.as_ref() else {
            return;
        };
        let folder = key(&root);
        if let Some(id) = flow.ticked.get(&folder).cloned() {
            if self.state.sessions.iter().any(|s| s.project_id == id) {
                self.show_toast("That project has a session now: it stays. Settings › Git and projects removes it.", cx);
                return;
            }
            self.remove_project(&id, cx);
            if let Some(flow) = self.first_run.as_mut() {
                flow.ticked.remove(&folder);
            }
            cx.notify();
            return;
        }
        match self.add_project_quietly(root, None, cx) {
            Ok((id, true)) => {
                if let Some(flow) = self.first_run.as_mut() {
                    flow.ticked.insert(folder, id);
                }
            }
            Ok((id, false)) => {
                let name = self
                    .state
                    .project(&id)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                self.show_toast(format!("{name} is a project already."), cx);
            }
            Err(error) => self.show_toast(error, cx),
        }
        cx.notify();
    }

    /// "Add a folder…": the system's folder dialog; the folder is added and ticked. The dialog
    /// opens from a task, after the click: on Windows a system window opened inside a click
    /// handler can come back into the app while it is still borrowed.
    fn first_run_add_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let Ok(receiver) = this.update(cx, |_, cx| {
                cx.prompt_for_paths(gpui::PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: Some("Add".into()),
                })
            }) else {
                return;
            };
            if let Ok(Ok(Some(paths))) = receiver.await {
                if let Some(path) = paths.into_iter().next() {
                    // Seen on Windows: after the dialog the window drew the change only at the
                    // next mouse move. A draw is asked for.
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.first_run_pick(path, cx);
                        window.refresh();
                    });
                }
            }
        })
        .detach();
    }

    /// A folder the person picked or cloned: at the top of the list, ticked.
    fn first_run_pick(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let Some(flow) = self.first_run.as_mut() else {
            return;
        };
        let folder = key(&root);
        if !flow.extra.iter().any(|f| key(&f.root) == folder) {
            flow.extra.insert(0, Found::picked(root.clone()));
        }
        if !flow.ticked.contains_key(&folder) {
            self.first_run_tick_project(root, cx);
        }
    }

    fn first_run_open_clone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(flow) = self.first_run.as_mut() else {
            return;
        };
        if flow.clone.is_none() {
            let input = cx.new(|cx| {
                TextInput::new("owner/name, or the address of a git repository", cx).mono()
            });
            cx.subscribe_in(
                &input,
                window,
                |this: &mut Workspace, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Submit => this.first_run_clone(cx),
                    InputEvent::Cancel => {
                        if let Some(flow) = this.first_run.as_mut() {
                            flow.clone = None;
                        }
                        cx.notify();
                    }
                    InputEvent::Changed => {}
                },
            )
            .detach();
            flow.clone = Some(input);
        }
        if let Some(input) = flow.clone.clone() {
            crate::text_input::focus(&input, window, cx);
        }
        cx.notify();
    }

    /// "Clone from GitHub…" as `glasscode project import` does it: into the folder most projects
    /// are in, else `Projects` in the home folder; then the folder is added and ticked.
    fn first_run_clone(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.first_run.as_ref() else {
            return;
        };
        let (Some(input), false) = (flow.clone.as_ref(), flow.cloning) else {
            return;
        };
        let source = input.read(cx).text().trim().to_string();
        let Some((url, name)) = crate::commands2::clone_source(&source) else {
            self.show_toast(
                "Type owner/name of a GitHub repository, or a git address.",
                cx,
            );
            return;
        };
        // The home of this flow: the made-up one in a check, never the real one.
        let into = if self.state.projects.is_empty() {
            flow.home.join("Projects")
        } else {
            self.projects_folder()
        };
        let target = into.join(&name);
        if target.exists() {
            self.show_toast(
                format!(
                    "{} exists already: add it with Add a folder…",
                    short_path(&target, &flow.home)
                ),
                cx,
            );
            return;
        }
        if let Some(flow) = self.first_run.as_mut() {
            flow.cloning = true;
        }
        let env = self.env.clone();
        let dest = target.clone();
        let task =
            cx.background_spawn(async move { crate::commands2::git_clone(&url, &dest, &env) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(flow) = this.first_run.as_mut() {
                    flow.cloning = false;
                }
                match result {
                    Ok(()) => {
                        if let Some(flow) = this.first_run.as_mut() {
                            flow.clone = None;
                        }
                        this.first_run_pick(target, cx);
                    }
                    Err(error) => this.show_toast(format!("Could not clone it: {error:#}"), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    // ----- logins ------------------------------------------------------------------------------

    fn first_run_tick_login(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(flow) = self.first_run.as_ref() else {
            return;
        };
        let Some(logins) = flow.logins.as_deref() else {
            return;
        };
        if i >= logins.len() {
            return;
        }
        let login = logins[i].clone();
        let (made_up, busy) = (flow.made_up, flow.running.is_some());
        match tick(logins, i) {
            Tick::Nothing => {}
            Tick::Busy => self.show_toast("Finish or cancel the sign-in that runs first.", cx),
            Tick::Cancel => self.first_run_end_sign_in(false, cx),
            Tick::Untick => {
                if let Some(id) = login.account.as_deref() {
                    self.first_run_sign_in_ended(id, false);
                }
            }
            Tick::Adopt | Tick::SignIn if made_up && login.origin == Origin::System => {
                self.show_toast("This check looks in a made-up home: its system login would be the real one, so it cannot be ticked.", cx);
            }
            Tick::Adopt => {
                let id = self.first_run_account(&login);
                if let Some(entry) = self
                    .first_run
                    .as_mut()
                    .and_then(|f| f.logins.as_mut())
                    .and_then(|l| l.get_mut(i))
                {
                    entry.account = Some(id);
                }
            }
            Tick::SignIn if busy => {
                self.show_toast("Finish or cancel the sign-in that runs first.", cx)
            }
            Tick::SignIn => {
                let id = self.first_run_account(&login);
                let dir = self.state.account(&id).map(Account::effective_dir);
                if let Some(entry) = self
                    .first_run
                    .as_mut()
                    .and_then(|f| f.logins.as_mut())
                    .and_then(|l| l.get_mut(i))
                {
                    entry.account = Some(id.clone());
                    entry.dir = entry.dir.clone().or(dir);
                }
                self.first_run_sign_in(&id, window, cx);
            }
        }
        cx.notify();
    }

    /// "Sign in" on an account that is ticked and not signed in.
    fn first_run_sign_in_again(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self
            .first_run
            .as_ref()
            .and_then(|f| f.logins.as_ref())
            .and_then(|l| l.get(i))
            .and_then(|l| l.account.clone())
        else {
            return;
        };
        if self.first_run.as_ref().is_some_and(|f| f.running.is_some()) {
            self.show_toast("Finish or cancel the sign-in that runs first.", cx);
            return;
        }
        self.first_run_sign_in(&id, window, cx);
    }

    /// "Sign in to another account": a new account in a folder of its own, signing in at once.
    fn first_run_new_account(
        &mut self,
        agent: AgentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.first_run.as_ref().is_some_and(|f| f.running.is_some()) {
            self.show_toast("Finish or cancel the sign-in that runs first.", cx);
            return;
        }
        let label = format!(
            "{} account {}",
            agent.name(),
            self.state.accounts_for(agent).count() + 1
        );
        let login = Login {
            agent,
            label,
            dir: None,
            origin: Origin::Glasscode,
            signed_in: false,
            account: None,
            kept: false,
            made: true,
            pending: false,
        };
        let id = self.first_run_account(&login);
        let dir = self.state.account(&id).map(Account::effective_dir);
        if let Some(logins) = self.first_run.as_mut().and_then(|f| f.logins.as_mut()) {
            logins.push(Login {
                account: Some(id.clone()),
                dir,
                ..login
            });
        }
        self.first_run_sign_in(&id, window, cx);
    }

    /// The account a ticked login becomes (`account_for`), saved; the first of an agent is its
    /// default.
    fn first_run_account(&mut self, login: &Login) -> String {
        let account = account_for(login, &new_id());
        let id = account.id.clone();
        if self.state.account(&id).is_none() {
            self.state.accounts.push(account);
        }
        if self.state.defaults.for_agent(login.agent).is_none() {
            self.state.defaults.set(login.agent, Some(id.clone()));
        }
        self.save();
        id
    }

    /// An account ticked in this run goes again (the tick was taken off, or its sign-in did not
    /// end with a login); another becomes the default. Its folder stays, as in Settings.
    fn first_run_drop_account(&mut self, id: &str) {
        let Some(account) = self.state.account(id).cloned() else {
            return;
        };
        if self
            .state
            .sessions
            .iter()
            .any(|s| s.account_id.as_deref() == Some(id))
        {
            return;
        }
        self.state.accounts.retain(|a| a.id != id);
        if self.state.defaults.for_agent(account.agent) == Some(&account.id) {
            let next = self
                .state
                .accounts_for(account.agent)
                .next()
                .map(|a| a.id.clone());
            self.state.defaults.set(account.agent, next);
        }
        self.save();
    }

    /// Starts the sign-in of an account in the flow's terminal, and its poll.
    fn first_run_sign_in(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account) = self.state.account(id).cloned() else {
            return;
        };
        let Some(home) = self.first_run.as_ref().map(|f| f.home.clone()) else {
            return;
        };
        let launch = glasscode_core::agents::login_launch(&account, &self.env);
        if !Path::new(&launch.program).is_file() {
            self.show_toast(format!("{} is not installed or not on your PATH. Install it, then tick the login again.", launch.program), cx);
            self.first_run_sign_in_ended(id, false);
            return;
        }
        let spec = SpawnSpec {
            program: launch.program,
            args: launch.args,
            cwd: home,
            env: launch.env,
        };
        let terminal = match TerminalView::spawn(spec, cx) {
            Ok(terminal) => terminal,
            Err(error) => {
                self.show_toast(format!("Could not start the sign-in: {error}"), cx);
                self.first_run_sign_in_ended(id, false);
                return;
            }
        };
        let hint = match account.agent {
            // macOS keeps Claude's login in the Keychain, where Glasscode does not look: the
            // wait ends when the program does.
            AgentKind::Claude if cfg!(target_os = "macos") => {
                "type /login if it does not ask you to sign in, then /exit once you are in"
            }
            AgentKind::Claude => "type /login if it does not ask you to sign in",
            _ => "a browser window opens to sign in",
        };
        let title = format!(
            "Sign in: {} · {} ({hint})",
            account.agent.name(),
            account.label
        );
        let run = self.first_run_start(Signing::Login(id.to_string()), title, terminal, window, cx);
        if let Some(entry) = self
            .first_run
            .as_mut()
            .and_then(|f| f.logins.as_mut())
            .and_then(|l| l.iter_mut().find(|l| l.account.as_deref() == Some(id)))
        {
            entry.pending = true;
        }
        // Every second: the login file, which the CLI writes at the end of its sign-in.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let waiting = this
                    .update(cx, |this, cx| this.first_run_poll(run, cx))
                    .unwrap_or(false);
                if !waiting {
                    break;
                }
            }
        })
        .detach();
    }

    /// GitHub: `gh auth login` in the flow's terminal; its accounts are read again when it ends.
    fn first_run_github(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.first_run.as_ref().is_some_and(|f| f.running.is_some()) {
            self.show_toast("Finish or cancel the sign-in that runs first.", cx);
            return;
        }
        let Some(gh) = glasscode_core::agents::resolve_program("gh", &self.env) else {
            self.show_toast("The GitHub CLI (gh) is not installed.", cx);
            return;
        };
        let Some(home) = self.first_run.as_ref().map(|f| f.home.clone()) else {
            return;
        };
        let args = [
            "auth",
            "login",
            "--hostname",
            "github.com",
            "--git-protocol",
            "https",
            "--web",
        ]
        .map(String::from)
        .to_vec();
        let spec = SpawnSpec {
            program: gh.to_string_lossy().into_owned(),
            args,
            cwd: home,
            env: (*self.env).clone(),
        };
        match TerminalView::spawn(spec, cx) {
            Ok(terminal) => {
                self.first_run_start(
                    Signing::Github,
                    "Sign in: GitHub (gh opens a page in your browser)".into(),
                    terminal,
                    window,
                    cx,
                );
            }
            Err(error) => self.show_toast(format!("Could not start gh: {error}"), cx),
        }
        cx.notify();
    }

    /// Keeps a sign-in's terminal in the flow, with the keyboard, and follows its exit.
    fn first_run_start(
        &mut self,
        what: Signing,
        title: String,
        terminal: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> u64 {
        let handle = terminal.read(cx).focus_handle_clone();
        window.focus(&handle, cx);
        let Some(flow) = self.first_run.as_mut() else {
            return 0;
        };
        flow.runs += 1;
        let run = flow.runs;
        cx.subscribe(
            &terminal,
            move |this: &mut Workspace, _, event: &TerminalEvent, cx| {
                if matches!(event, TerminalEvent::Exited(_)) {
                    this.first_run_exited(run, cx);
                }
            },
        )
        .detach();
        // The list shows only this sign-in now: from its top.
        flow.scroll.set_offset(gpui::point(px(0.), px(0.)));
        flow.running = Some(Running {
            what,
            title,
            terminal,
            run,
        });
        cx.notify();
        run
    }

    /// The poll of a sign-in; false once it is over.
    fn first_run_poll(&mut self, run: u64, cx: &mut Context<Self>) -> bool {
        let Some(Signing::Login(id)) = self
            .first_run
            .as_ref()
            .and_then(|f| f.running.as_ref())
            .filter(|r| r.run == run)
            .map(|r| r.what.clone())
        else {
            return false;
        };
        let Some(account) = self.state.account(&id).cloned() else {
            return false;
        };
        match login_end(
            cfg!(target_os = "macos"),
            account.agent,
            false,
            discover::signed_in(&account),
        ) {
            Some(signed) => {
                self.first_run_end_sign_in(signed, cx);
                false
            }
            None => true,
        }
    }

    fn first_run_exited(&mut self, run: u64, cx: &mut Context<Self>) {
        let Some(what) = self
            .first_run
            .as_ref()
            .and_then(|f| f.running.as_ref())
            .filter(|r| r.run == run)
            .map(|r| r.what.clone())
        else {
            return;
        };
        let signed = match what {
            Signing::Github => true,
            Signing::Login(id) => self.state.account(&id).is_some_and(|a| {
                login_end(
                    cfg!(target_os = "macos"),
                    a.agent,
                    true,
                    discover::signed_in(a),
                )
                .unwrap_or(false)
            }),
        };
        self.first_run_end_sign_in(signed, cx);
    }

    /// The sign-in that runs is over (`signed`), or is cancelled: its program ends (Claude stays at
    /// its prompt after a login), its terminal goes, and without a login its tick goes.
    fn first_run_end_sign_in(&mut self, signed: bool, cx: &mut Context<Self>) {
        let Some(running) = self.first_run.as_mut().and_then(|f| f.running.take()) else {
            return;
        };
        if let Some(pid) = running.terminal.read(cx).child_pid() {
            if let Err(error) = glasscode_core::sys::end_tree(pid) {
                log::warn!("first run: could not end the sign-in program {pid}: {error}");
            }
        }
        drop(running.terminal);
        match running.what {
            Signing::Github => self.github_accounts_refresh(cx),
            Signing::Login(id) => {
                let label = self
                    .state
                    .account(&id)
                    .map(|a| format!("{} · {}", a.agent.name(), a.label))
                    .unwrap_or_default();
                self.first_run_sign_in_ended(&id, signed);
                self.show_toast(
                    if signed {
                        format!("{label} is signed in.")
                    } else {
                        format!("{label} was not signed in.")
                    },
                    cx,
                );
            }
        }
        cx.notify();
    }

    /// The entry of `id` stops waiting; without a login, its tick (and its account) goes.
    fn first_run_sign_in_ended(&mut self, id: &str, signed: bool) {
        let Some(logins) = self.first_run.as_mut().and_then(|f| f.logins.as_mut()) else {
            return;
        };
        let Some(i) = logins.iter().position(|l| l.account.as_deref() == Some(id)) else {
            return;
        };
        if let Some(account) = finish(logins, i, signed) {
            self.first_run_drop_account(&account);
        }
    }

    // ----- drawing -----------------------------------------------------------------------------

    /// The flow: the art panel with the backdrop at the left (not in a narrow window), the steps
    /// at the right on the plain ground.
    pub fn render_first_run(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(step) = self.first_run.as_ref().map(|f| f.step) else {
            return div().flex_1().into_any_element();
        };
        let art = art_width(self.window_width, theme::scale())
            .map(|width| self.first_run_art(width, window, cx));
        let window_height = f32::from(window.viewport_size().height);
        let body: Vec<AnyElement> = match step {
            Step::Projects => self.first_run_projects(cx),
            Step::Agents => {
                self.first_run_agents(terminal_height(window_height, theme::scale()), cx)
            }
            Step::Appearance => self.first_run_appearance(window, cx),
            Step::Summary => self.first_run_summary(),
        };
        let Some(flow) = self.first_run.as_ref() else {
            return div().into_any_element();
        };
        let list = ui::scroll_area(
            "first-run-list",
            &flow.scroll,
            div()
                .flex()
                .flex_col()
                .gap(theme::sz(6.))
                .pb(theme::sz(8.))
                .children(body),
        );
        let steps = div()
            .flex()
            .items_center()
            .gap(theme::sz(6.))
            .children(Step::ALL.map(|s| {
                div().w(theme::sz(28.)).h(theme::sz(3.)).rounded_full().bg(
                    if s.number() <= step.number() {
                        theme::text()
                    } else {
                        theme::border_strong()
                    },
                )
            }))
            .child(
                div()
                    .ml(theme::sz(8.))
                    .text_size(theme::sz(12.))
                    .text_color(theme::muted())
                    .child(format!("Step {} of {}", step.number(), Step::ALL.len())),
            );
        let mut column = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .max_w(theme::sz(680.))
            .mx_auto()
            .gap(theme::sz(12.))
            .child(steps)
            .child(
                div()
                    .text_size(theme::sz(22.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text_strong())
                    .child(step.title()),
            )
            .when(!short(window_height, theme::scale()), |d| {
                d.child(
                    div()
                        .text_size(theme::sz(13.5))
                        .text_color(theme::muted())
                        .child(step.lead()),
                )
            });
        if step == Step::Projects {
            column = column.child(flow.search.clone());
        }
        let column = column
            .child(div().flex().flex_col().flex_1().min_h_0().child(list))
            .when(step == Step::Projects, |d| {
                d.child(self.first_run_project_buttons(cx))
            })
            .child(self.first_run_nav(step, cx));
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .gap(theme::sz(PAD))
            .p(theme::sz(PAD))
            .bg(theme::bg())
            .text_color(theme::text())
            .children(art)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .px(theme::sz(16.))
                    .pt(theme::sz(14.))
                    .child(column),
            )
            .into_any_element()
    }

    /// The backdrop at full strength in a panel with round corners, with the mark and a sentence
    /// on strong glass.
    fn first_run_art(
        &mut self,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let layer = self.backdrop_layer(1.0, window, cx);
        let (radius, ground) = (theme::sz(ART_RADIUS), theme::bg());
        // The layer paints a rectangle and GPUI clips only to rectangles: a ring in the ground's
        // colour, as wide as half the radius, rounds its corners from outside.
        let ring = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let width = radius / 2.;
                window.paint_quad(quad(
                    bounds.dilate(width),
                    Corners::all(radius + width),
                    transparent_black(),
                    width,
                    ground,
                    BorderStyle::Solid,
                ));
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let caption = backdrop::glass_strong(theme::sz(12.))
            .flex()
            .flex_col()
            .gap(theme::sz(12.))
            .p(theme::sz(18.))
            .child(icons::icon(
                icons::LOGO,
                theme::sz(30.),
                theme::sz(28.),
                theme::text(),
            ))
            .child(
                div()
                    .text_size(theme::sz(19.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text())
                    .child(SENTENCE),
            );
        div()
            .relative()
            .flex_none()
            .w(theme::sz(width))
            .h_full()
            .child(layer)
            .child(ring)
            .child(
                div()
                    .absolute()
                    .left(theme::sz(16.))
                    .right(theme::sz(16.))
                    .bottom(theme::sz(16.))
                    .child(caption),
            )
            .into_any_element()
    }

    fn first_run_nav(&self, step: Step, cx: &mut Context<Self>) -> AnyElement {
        let signing = self.first_run.as_ref().is_some_and(FirstRun::signing);
        let next: Stateful<Div> = if !can_continue(step, signing) {
            ui::disabled_button("first-run-next", "Waiting for the sign-in…")
        } else {
            ui::primary_button(
                "first-run-next",
                if step == Step::Summary {
                    "Open Today"
                } else {
                    "Continue"
                },
            )
            .on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| this.first_run_continue(window, cx)),
            )
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(theme::sz(8.))
            .py(theme::sz(12.))
            .when(step != Step::Summary, |d| {
                d.child(
                    div()
                        .id("first-run-skip")
                        .text_size(theme::sz(13.))
                        .text_color(theme::muted())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::text()))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.first_run_close(true, window, cx)
                        }))
                        .child("Skip for now"),
                )
            })
            .child(div().flex_1())
            .when(step.back().is_some(), |d| {
                d.child(
                    ui::button("first-run-back", "Back").on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.first_run_back(cx)),
                    ),
                )
            })
            .child(next)
            .into_any_element()
    }

    /// A group's heading in the list.
    fn first_run_heading(text: impl Into<SharedString>, brand: Option<Brand>) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap(theme::sz(6.))
            .pt(theme::sz(10.))
            .pb(theme::sz(2.))
            .text_size(theme::sz(11.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::muted())
            .when_some(brand, |d, brand| {
                d.child(icons::brand(brand, 14.))
            })
            .child(text.into())
            .into_any_element()
    }

    /// A sentence in the list where it has nothing (yet).
    fn first_run_note(text: impl Into<SharedString>) -> AnyElement {
        div()
            .px(theme::sz(12.))
            .py(theme::sz(10.))
            .rounded(theme::sz(9.))
            .border_1()
            .border_color(theme::border())
            .text_size(theme::sz(13.))
            .text_color(theme::muted())
            .child(text.into())
            .into_any_element()
    }

    fn first_run_projects(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(flow) = self.first_run.as_ref() else {
            return Vec::new();
        };
        let rows = flow.rows();
        let projects: HashSet<String> = self.state.projects.iter().map(|p| key(&p.root)).collect();
        let ticked = |f: &Found| flow.ticked.contains_key(&key(&f.root));
        let (shown, more) = shown(&rows, &flow.query, &flow.home, &ticked);
        let mut out = Vec::new();
        if flow.found.is_none() {
            out.push(Self::first_run_note(
                "Looking for the folders you worked in, in Cursor, VS Code, Claude Code and Codex…",
            ));
        } else if rows.is_empty() {
            out.push(Self::first_run_note("No folders were found from Cursor, VS Code, Claude Code or Codex. Add a folder, or clone one from GitHub."));
        } else if shown.is_empty() {
            out.push(Self::first_run_note(format!(
                "No folder matches “{}”.",
                flow.query.trim()
            )));
        } else {
            let heading = if flow.query.trim().is_empty() {
                format!("Most recent · {} found", rows.len())
            } else {
                format!("{} match", shown.len() + more)
            };
            out.push(Self::first_run_heading(heading, None));
        }
        for i in shown {
            let found = &rows[i];
            let folder = key(&found.root);
            let on = flow.ticked.contains_key(&folder);
            let kept = !on && projects.contains(&folder);
            let root = found.root.clone();
            let row = option_row(SharedString::from(format!("first-run-project-{folder}")))
                .when(!kept, |d| {
                    d.cursor_pointer()
                        .hover(|s| s.border_color(theme::border_strong()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.first_run_tick_project(root.clone(), cx)
                        }))
                })
                .child(tick_box(on || kept, kept))
                .child(icons::icon(
                    icons::FOLDER,
                    theme::sz(15.),
                    theme::sz(15.),
                    theme::muted(),
                ))
                .child(two_lines(
                    found.name.clone(),
                    short_path(&found.root, &flow.home),
                    true,
                ))
                .when(kept, |d| d.child(ui::badge("Added before", theme::muted())))
                .when(!found.git, |d| d.child(ui::badge("No git", theme::muted())))
                .children(
                    found
                        .sources
                        .iter()
                        .map(|s| icons::brand(source_brand(*s), 14.)),
                );
            out.push(row.into_any_element());
        }
        if more > 0 {
            let text = if flow.query.trim().is_empty() {
                format!("{more} more. Search to find them.")
            } else {
                format!("{more} more. Type more of the name.")
            };
            out.push(
                div()
                    .px(theme::sz(12.))
                    .pt(theme::sz(4.))
                    .text_size(theme::sz(12.))
                    .text_color(theme::muted())
                    .child(text)
                    .into_any_element(),
            );
        }
        out
    }

    fn first_run_project_buttons(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(flow) = self.first_run.as_ref() else {
            return div().into_any_element();
        };
        let row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(theme::sz(8.))
            .pt(theme::sz(4.))
            .child(
                ui::button("first-run-add-folder", "Add a folder…").on_click(cx.listener(
                    |this, _: &ClickEvent, window, cx| this.first_run_add_folder(window, cx),
                )),
            );
        match (&flow.clone, flow.cloning) {
            (None, _) => row.child(
                ui::button("first-run-clone", "Clone from GitHub…").on_click(cx.listener(
                    |this, _: &ClickEvent, window, cx| this.first_run_open_clone(window, cx),
                )),
            ),
            (Some(input), cloning) => {
                row.child(div().flex_1().min_w(theme::sz(220.)).child(input.clone()))
                    .child(if cloning {
                        ui::disabled_button("first-run-clone-go", "Cloning…")
                    } else {
                        ui::primary_button("first-run-clone-go", "Clone").on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.first_run_clone(cx)),
                        )
                    })
                    .when(!cloning, |d| {
                        d.child(ui::button("first-run-clone-cancel", "Cancel").on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                if let Some(flow) = this.first_run.as_mut() {
                                    flow.clone = None;
                                }
                                cx.notify();
                            }),
                        ))
                    })
            }
        }
        .into_any_element()
    }

    fn first_run_agents(&self, terminal_height: f32, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(flow) = self.first_run.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let Some(logins) = flow.logins.as_ref() else {
            out.push(Self::first_run_note(
                "Looking for the logins of Claude Code and Codex…",
            ));
            return out;
        };
        // While a sign-in runs only its row and its terminal show: one runs at a time, and the
        // terminal stays in sight in a short window.
        if let Some(running) = flow.running.as_ref() {
            match &running.what {
                Signing::Login(id) => out.extend(
                    logins
                        .iter()
                        .position(|l| l.account.as_deref() == Some(id.as_str()))
                        .map(|i| self.first_run_login_row(i, &logins[i], flow.made_up, cx)),
                ),
                Signing::Github => out.push(self.first_run_github_row(flow, cx)),
            }
            out.push(self.first_run_terminal(running, terminal_height, cx));
            return out;
        }
        for agent in [AgentKind::Claude, AgentKind::Codex] {
            let mine: Vec<usize> = (0..logins.len())
                .filter(|&i| logins[i].agent == agent)
                .collect();
            let product = if agent == AgentKind::Claude {
                "Claude Code"
            } else {
                "Codex"
            };
            let found = match mine.len() {
                0 => format!("{product} · no login found"),
                1 => format!("{product} · 1 login found"),
                n => format!("{product} · {n} logins found"),
            };
            out.push(Self::first_run_heading(
                found,
                Some(icons::agent_brand(agent)),
            ));
            for i in mine {
                out.push(self.first_run_login_row(i, &logins[i], flow.made_up, cx));
            }
            let has_any = logins.iter().any(|l| l.agent == agent);
            out.push(
                div()
                    .flex()
                    .child(
                        ui::button(
                            SharedString::from(format!("first-run-another-{}", agent.name())),
                            another_label(agent, has_any),
                        )
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| {
                                this.first_run_new_account(agent, window, cx)
                            },
                        )),
                    )
                    .into_any_element(),
            );
        }
        out.push(Self::first_run_heading("GitHub and Linear", None));
        out.push(self.first_run_github_row(flow, cx));
        out.push(self.first_run_linear_row(flow, cx));
        // Linear's own key: a project with a GitHub repository sets up the board without one.
        if flow.linear_open && !self.tickets.linear_key {
            // The Board's form, with words in colours for its own ground.
            out.push(
                ui::card()
                    .p(theme::sz(14.))
                    .child(crate::tickets_ui::render_linear_section(self, cx))
                    .into_any_element(),
            );
        }
        out
    }

    fn first_run_login_row(
        &self,
        i: usize,
        login: &Login,
        made_up: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let detail = match login.origin {
            Origin::System => format!(
                "The CLI's own login{}",
                login
                    .dir
                    .as_deref()
                    .map(|d| format!(" · {}", self.first_run_short(d)))
                    .unwrap_or_default()
            ),
            Origin::Glasscode if login.made => "A new account in a folder of Glasscode".to_string(),
            Origin::Glasscode => "An account of Glasscode".to_string(),
            Origin::Swap => {
                "Saved by claude-swap · signs in again in a folder of Glasscode".to_string()
            }
        };
        let detail = if made_up && login.origin == Origin::System {
            format!("{detail} · made-up home: cannot be ticked")
        } else {
            detail
        };
        let status: AnyElement = if login.pending {
            div()
                .flex()
                .items_center()
                .gap(theme::sz(8.))
                .child(
                    div()
                        .text_size(theme::sz(12.5))
                        .text_color(theme::attention())
                        .child("Signing in…"),
                )
                .child(
                    ui::button(
                        SharedString::from(format!("first-run-cancel-{i}")),
                        "Cancel",
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.first_run_end_sign_in(false, cx)
                    })),
                )
                .into_any_element()
        } else if login.signed_in {
            div()
                .text_size(theme::sz(12.5))
                .text_color(theme::added())
                .child("Signed in")
                .into_any_element()
        } else if login.ticked() {
            ui::primary_button(
                SharedString::from(format!("first-run-sign-in-{i}")),
                "Sign in",
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.first_run_sign_in_again(i, window, cx)
            }))
            .into_any_element()
        } else {
            div()
                .text_size(theme::sz(12.5))
                .text_color(theme::muted())
                .child("Not signed in")
                .into_any_element()
        };
        let locked = login.kept && !login.pending;
        option_row(SharedString::from(format!("first-run-login-{i}")))
            .when(!locked, |d| {
                d.cursor_pointer()
                    .hover(|s| s.border_color(theme::border_strong()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.first_run_tick_login(i, window, cx)
                    }))
            })
            .child(tick_box(login.ticked(), locked))
            .child(two_lines(login.label.clone(), detail, false))
            .child(status)
            .into_any_element()
    }

    fn first_run_github_row(&self, flow: &FirstRun, cx: &mut Context<Self>) -> AnyElement {
        let running = flow
            .running
            .as_ref()
            .is_some_and(|r| r.what == Signing::Github);
        let accounts = self.github.list.as_ref().map_or(0, Vec::len);
        let detail = if !flow.gh_installed {
            "The GitHub CLI (gh) is not installed. Pull requests and checks need it.".to_string()
        } else if running {
            "gh signs in, in your browser…".to_string()
        } else if self.github.list.is_none() && self.github.error.is_none() {
            "Asking gh…".to_string()
        } else if accounts == 0 {
            "Not signed in through gh".to_string()
        } else {
            format!(
                "Signed in through gh · {}",
                if accounts == 1 {
                    "1 account".to_string()
                } else {
                    format!("{accounts} accounts")
                }
            )
        };
        let control: Option<AnyElement> = if running {
            Some(
                ui::button("first-run-github-cancel", "Cancel")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.first_run_end_sign_in(false, cx)
                    }))
                    .into_any_element(),
            )
        } else if flow.gh_installed && self.github.list.is_some()
            || self.github.error.is_some() && flow.gh_installed
        {
            let label = if accounts == 0 {
                "Sign in"
            } else {
                "Add an account"
            };
            Some(
                ui::button("first-run-github", label)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.first_run_github(window, cx)
                    }))
                    .into_any_element(),
            )
        } else {
            None
        };
        option_row("first-run-github-row")
            .child(icons::brand(Brand::Github, 16.))
            .child(two_lines("GitHub", detail, false))
            .children(control)
            .into_any_element()
    }

    fn first_run_linear_row(&self, flow: &FirstRun, cx: &mut Context<Self>) -> AnyElement {
        let connected = self.tickets.linear_key;
        option_row("first-run-linear-row")
            .child(icons::brand(Brand::Linear, 16.))
            .child(two_lines(
                "Linear",
                if connected {
                    "Connected: your tickets show on Today and the Board"
                } else {
                    "Not connected"
                },
                false,
            ))
            .when(!connected, |d| {
                d.child(
                    ui::button(
                        "first-run-linear",
                        if flow.linear_open {
                            "Hide"
                        } else {
                            "Connect…"
                        },
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(flow) = this.first_run.as_mut() {
                            flow.linear_open = !flow.linear_open;
                        }
                        cx.notify();
                    })),
                )
            })
            .into_any_element()
    }

    fn first_run_terminal(
        &self,
        running: &Running,
        height: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(theme::sz(8.))
            .p(theme::sz(12.))
            .rounded(theme::sz(9.))
            .border_1()
            .border_color(theme::border_strong())
            .bg(theme::panel())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(theme::sz(8.))
                    .child(two_lines(running.title.clone(), "This runs in the official CLI. Glasscode never sees your password or tokens.", false))
                    .child(ui::button("first-run-terminal-cancel", "Cancel").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.first_run_end_sign_in(false, cx)))),
            )
            .child(div().h(theme::sz(height)).rounded(theme::sz(6.)).border_1().border_color(theme::border()).overflow_hidden().child(running.terminal.clone()))
            .into_any_element()
    }

    /// Step 3: the cards of Settings › Appearance that change the look most, as Settings draws them.
    fn first_run_appearance(&self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        const CARDS: [CardId; 5] = [
            CardId::Theme,
            CardId::Backdrop,
            CardId::Finish,
            CardId::Scale,
            CardId::Accent,
        ];
        let catalog = crate::settings_ui::catalog();
        CARDS
            .iter()
            .filter_map(|card| catalog.iter().find(|info| info.card == *card))
            .flat_map(|info| self.settings_card(info, window, cx))
            .collect()
    }

    fn first_run_summary(&self) -> Vec<AnyElement> {
        let Some(flow) = self.first_run.as_ref() else {
            return Vec::new();
        };
        let count = |n: usize, one: &str, many: &str| {
            (
                n.to_string(),
                if n == 1 {
                    one.to_string()
                } else {
                    many.to_string()
                },
            )
        };
        let gh = self.github.list.as_ref().map_or(0, Vec::len);
        let tiles = [
            count(flow.ticked.len(), "project added", "projects added"),
            count(
                self.state.accounts_for(AgentKind::Claude).count(),
                "Claude account",
                "Claude accounts",
            ),
            count(
                self.state.accounts_for(AgentKind::Codex).count(),
                "Codex account",
                "Codex accounts",
            ),
            count(gh, "GitHub account in gh", "GitHub accounts in gh"),
        ];
        let tile = |(number, words): (String, String)| {
            div()
                .flex()
                .flex_col()
                .gap(theme::sz(2.))
                .flex_1()
                .min_w(theme::sz(150.))
                .px(theme::sz(14.))
                .py(theme::sz(12.))
                .rounded(theme::sz(10.))
                .border_1()
                .border_color(theme::border())
                .bg(theme::panel())
                .child(
                    div()
                        .text_size(theme::sz(24.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::text_strong())
                        .child(number),
                )
                .child(
                    div()
                        .text_size(theme::sz(12.5))
                        .text_color(theme::muted())
                        .child(words),
                )
        };
        let [a, b, c, d] = tiles;
        vec![
            div()
                .flex()
                .flex_wrap()
                .gap(theme::sz(10.))
                .child(tile(a))
                .child(tile(b))
                .into_any_element(),
            div()
                .flex()
                .flex_wrap()
                .gap(theme::sz(10.))
                .child(tile(c))
                .child(tile(d))
                .into_any_element(),
        ]
    }

    fn first_run_short(&self, path: &Path) -> String {
        self.first_run
            .as_ref()
            .map_or_else(|| path.display().to_string(), |f| short_path(path, &f.home))
    }
}

/// A row of the list: a tick, words, and what goes at its end.
fn option_row(id: impl Into<gpui::ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(theme::sz(10.))
        .min_h(theme::sz(46.))
        .px(theme::sz(12.))
        .py(theme::sz(6.))
        .rounded(theme::sz(9.))
        .border_1()
        .border_color(theme::border())
        .bg(theme::panel())
}

/// A tick box; a locked one (it stays) is faint.
fn tick_box(on: bool, locked: bool) -> AnyElement {
    let (glyph, color) = match (on, locked) {
        (true, false) => (icons::CHECKBOX_CHECKED, theme::text()),
        (true, true) => (icons::CHECKBOX_CHECKED, theme::muted()),
        (false, _) => (icons::CHECKBOX, theme::border_strong()),
    };
    icons::icon(glyph, theme::sz(17.), theme::sz(17.), color).into_any_element()
}

/// A name and a line under it (a path in the code font).
fn two_lines(
    first: impl Into<SharedString>,
    second: impl Into<SharedString>,
    path: bool,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap(theme::sz(1.))
        .child(
            div()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(theme::sz(13.5))
                .text_color(theme::text())
                .child(first.into()),
        )
        .child(
            div()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(theme::sz(12.))
                .text_color(theme::muted())
                .when(path, |d| d.font_family(theme::MONO_FONT))
                .child(second.into()),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn found(root: &str, git: bool, seconds: u64) -> Found {
        let root = PathBuf::from(root);
        Found {
            name: root.file_name().unwrap().to_string_lossy().into_owned(),
            root,
            sources: vec![Source::Cursor],
            last_used: Some(UNIX_EPOCH + Duration::from_secs(seconds)),
            git,
            mine: false,
        }
    }

    fn login(agent: AgentKind, origin: Origin, signed_in: bool) -> Login {
        Login {
            agent,
            label: "My login".into(),
            dir: None,
            origin,
            signed_in,
            account: None,
            kept: false,
            made: false,
            pending: false,
        }
    }

    #[test]
    fn the_steps_go_on_back_and_end() {
        assert_eq!(Step::Projects.next(), Some(Step::Agents));
        assert_eq!(Step::Appearance.next(), Some(Step::Summary));
        assert_eq!(Step::Summary.next(), None, "Open Today ends the flow");
        assert_eq!(Step::Projects.back(), None);
        assert_eq!(Step::Summary.back(), Some(Step::Appearance));
        assert_eq!(Step::ALL.map(Step::number), [1, 2, 3, 4]);
        assert_eq!(
            (
                Step::named("agents"),
                Step::named("3"),
                Step::named("skills")
            ),
            (Some(Step::Agents), Some(Step::Appearance), None)
        );
    }

    #[test]
    fn the_flow_shows_once_for_a_home_without_projects() {
        let mut state = State::default();
        assert!(wanted(&state), "a new home");
        state.first_run_done = true;
        assert!(!wanted(&state), "finished or skipped");
        let old: State = serde_json::from_str(
            r#"{ "projects": [{ "id": "p1", "name": "app", "root": "/tmp/app" }] }"#,
        )
        .unwrap();
        assert!(!wanted(&old), "a state from before the flow, with projects");
    }

    #[test]
    fn skip_and_finish_end_the_flow_and_only_skip_takes_the_system_logins() {
        let mut state = State::default();
        assert!(
            close_state(&mut state, true, false),
            "skipped: the logins as before the flow"
        );
        assert!(state.first_run_done && !wanted(&state));
        let mut state = State::default();
        assert!(
            !close_state(&mut state, false, false),
            "finished: what the person ticked"
        );
        assert!(state.first_run_done);
        let mut state = State::default();
        assert!(
            !close_state(&mut state, true, true),
            "a made-up home never takes the real logins"
        );
        assert!(state.first_run_done);
    }

    /// A ticked system login becomes the account `bootstrap_accounts` makes for it (which here
    /// only builds the path of the home folder, without a look at it).
    #[test]
    fn a_ticked_system_login_is_the_account_of_before() {
        let mut before = State::default();
        before.bootstrap_accounts(false);
        for agent in [AgentKind::Claude, AgentKind::Codex] {
            let adopted = account_for(&login(agent, Origin::System, true), "unused");
            let old = before
                .accounts_for(agent)
                .next()
                .expect("bootstrap makes one");
            assert_eq!(
                (
                    adopted.id.as_str(),
                    adopted.agent,
                    adopted.label.as_str(),
                    adopted.system_default,
                    adopted.config_dir()
                ),
                (
                    old.id.as_str(),
                    old.agent,
                    old.label.as_str(),
                    old.system_default,
                    None
                )
            );
        }
        let swap = Login {
            label: "Work".into(),
            ..login(AgentKind::Claude, Origin::Swap, false)
        };
        let new = account_for(&swap, "0123456789abcdef");
        assert_eq!(
            (new.id.as_str(), new.label.as_str(), new.system_default),
            ("claude-01234567", "Work", false)
        );
    }

    #[test]
    fn continue_waits_while_a_sign_in_runs() {
        let mut logins = vec![login(AgentKind::Claude, Origin::Swap, false)];
        assert!(can_continue(Step::Agents, signing(&logins, false)));
        logins[0].pending = true;
        assert!(!can_continue(Step::Agents, signing(&logins, false)));
        assert!(
            !can_continue(Step::Agents, signing(&[], true)),
            "gh auth login"
        );
        assert!(
            can_continue(Step::Appearance, true),
            "only the Agents step waits"
        );
    }

    #[test]
    fn a_tick_takes_a_login_or_starts_its_sign_in() {
        let mut logins = vec![
            login(AgentKind::Claude, Origin::System, true),
            login(AgentKind::Codex, Origin::System, false),
            Login {
                kept: true,
                account: Some("claude-work".into()),
                signed_in: true,
                ..login(AgentKind::Claude, Origin::Glasscode, true)
            },
            Login {
                account: Some("codex-1".into()),
                ..login(AgentKind::Codex, Origin::Swap, false)
            },
        ];
        assert_eq!(
            tick(&logins, 0),
            Tick::Adopt,
            "signed in already: nothing runs"
        );
        assert_eq!(tick(&logins, 1), Tick::SignIn);
        assert_eq!(
            tick(&logins, 2),
            Tick::Nothing,
            "an account from before stays"
        );
        assert_eq!(tick(&logins, 3), Tick::Untick);
        logins[3].pending = true;
        assert_eq!(tick(&logins, 3), Tick::Cancel);
        assert_eq!(tick(&logins, 1), Tick::Busy, "one sign-in at a time");
    }

    #[test]
    fn a_cancelled_sign_in_takes_the_tick_off() {
        let mut logins = vec![
            Login {
                account: Some("claude-1".into()),
                dir: Some(PathBuf::from("accounts/claude-1")),
                pending: true,
                ..login(AgentKind::Claude, Origin::Swap, false)
            },
            Login {
                account: Some("codex-2".into()),
                made: true,
                pending: true,
                ..login(AgentKind::Codex, Origin::Glasscode, false)
            },
            Login {
                account: Some("claude-work".into()),
                kept: true,
                pending: true,
                ..login(AgentKind::Claude, Origin::Glasscode, false)
            },
        ];
        assert_eq!(finish(&mut logins, 0, false), Some("claude-1".into()));
        assert!(!logins[0].ticked() && !logins[0].pending && logins[0].dir.is_none());
        assert_eq!(finish(&mut logins, 1, false), Some("codex-2".into()));
        assert_eq!(
            logins.len(),
            2,
            "a row made in this run goes with its sign-in"
        );
        assert_eq!(
            finish(&mut logins, 1, false),
            None,
            "an account from before keeps its tick"
        );
        assert!(logins[1].ticked() && !logins[1].pending);

        let mut one = vec![Login {
            account: Some("codex-3".into()),
            pending: true,
            ..login(AgentKind::Codex, Origin::System, false)
        }];
        assert_eq!(finish(&mut one, 0, true), None);
        assert!(
            one[0].signed_in && one[0].ticked() && !one[0].pending,
            "signed in: it stays ticked"
        );

        // The tick taken off a claude-swap name signed in in this run: its folder is no longer an
        // account's, so a new tick signs in again in a new one.
        let mut swap = vec![Login {
            account: Some("claude-4".into()),
            dir: Some(PathBuf::from("accounts/claude-4")),
            signed_in: true,
            ..login(AgentKind::Claude, Origin::Swap, true)
        }];
        assert_eq!(tick(&swap, 0), Tick::Untick);
        assert_eq!(finish(&mut swap, 0, false), Some("claude-4".into()));
        assert_eq!(
            (swap[0].ticked(), swap[0].signed_in, swap[0].dir.as_ref()),
            (false, false, None)
        );
        assert_eq!(tick(&swap, 0), Tick::SignIn);
    }

    #[test]
    fn a_sign_in_ends_by_its_login_file_or_on_macos_by_its_exit() {
        use AgentKind::{Claude, Codex};
        assert_eq!(
            login_end(false, Claude, false, false),
            None,
            "still signing in"
        );
        assert_eq!(login_end(false, Claude, false, true), Some(true));
        assert_eq!(
            login_end(false, Claude, true, false),
            Some(false),
            "it exited with no login"
        );
        assert_eq!(
            login_end(true, Claude, false, true),
            None,
            "macOS: the folder alone says nothing"
        );
        assert_eq!(
            login_end(true, Claude, true, false),
            Some(true),
            "macOS: the exit ends it"
        );
        assert_eq!(
            login_end(true, Codex, false, true),
            Some(true),
            "Codex has its file on macOS too"
        );
    }

    #[test]
    fn logins_found_with_an_account_are_ticked_and_stay() {
        let accounts = vec![
            Account {
                id: "claude-default".into(),
                agent: AgentKind::Claude,
                label: "My login".into(),
                plan: None,
                system_default: true,
            },
            Account {
                id: "codex-work".into(),
                agent: AgentKind::Codex,
                label: "Work".into(),
                plan: None,
                system_default: false,
            },
        ];
        let found = |agent, from: &str, dir: Option<&str>, state| FoundLogin {
            agent,
            label: "x".into(),
            dir: dir.map(PathBuf::from),
            state,
            from: from.into(),
        };
        let logins = logins_from(
            vec![
                found(
                    AgentKind::Claude,
                    discover::FROM_SYSTEM,
                    Some("home/.claude"),
                    LoginState::SignedIn,
                ),
                found(
                    AgentKind::Codex,
                    discover::FROM_SYSTEM,
                    Some("home/.codex"),
                    LoginState::NeedsSignIn,
                ),
                found(
                    AgentKind::Codex,
                    discover::FROM_GLASSCODE,
                    Some("glasscode/accounts/codex-work"),
                    LoginState::SignedIn,
                ),
                found(
                    AgentKind::Claude,
                    discover::FROM_CLAUDE_SWAP,
                    None,
                    LoginState::NeedsSignIn,
                ),
            ],
            &accounts,
        );
        let summary: Vec<(Origin, Option<&str>, bool, bool)> = logins
            .iter()
            .map(|l| (l.origin, l.account.as_deref(), l.kept, l.signed_in))
            .collect();
        assert_eq!(
            summary,
            vec![
                (Origin::System, Some("claude-default"), true, true),
                (Origin::System, None, false, false),
                (Origin::Glasscode, Some("codex-work"), true, true),
                (Origin::Swap, None, false, false),
            ]
        );
    }

    #[test]
    fn nothing_found_gives_usable_empty_states() {
        let home = PathBuf::from("home");
        assert_eq!(shown(&[], "", &home, &|_| false), (Vec::new(), 0));
        assert_eq!(shown(&[], "app", &home, &|_| false), (Vec::new(), 0));
        assert!(fold_worktrees(Vec::new()).is_empty());
        assert!(logins_from(Vec::new(), &[]).is_empty());
        assert!(
            can_continue(Step::Projects, false) && can_continue(Step::Agents, signing(&[], false)),
            "Continue works with nothing found"
        );
        assert_eq!(
            another_label(AgentKind::Claude, false),
            "Sign in to a Claude account…"
        );
        assert_eq!(
            another_label(AgentKind::Codex, true),
            "Sign in to another Codex account…"
        );
    }

    #[test]
    fn the_first_list_is_short_with_repositories_first_and_the_search_finds_all() {
        let home = PathBuf::from("home");
        // 30 folders, last used first; each third one has no git.
        let rows: Vec<Found> = (0..30)
            .map(|i| found(&format!("home/code/app{i:02}"), i % 3 != 0, 1000 - i))
            .collect();
        let (list, more) = shown(&rows, "", &home, &|_| false);
        assert_eq!((list.len(), more), (FIRST, 30 - FIRST));
        assert!(
            list.iter().all(|&i| rows[i].git),
            "20 repositories fill the first list"
        );
        assert_eq!(list[0], 1, "the most recent repository first");
        assert!(
            list.windows(2)
                .all(|w| rows[w[0]].last_used > rows[w[1]].last_used)
        );
        // A ticked folder further down stays in the list.
        let (list, more) = shown(&rows, "", &home, &|f| f.name == "app27");
        assert_eq!(
            (list.len(), more, rows[*list.last().unwrap()].name.as_str()),
            (FIRST + 1, 30 - FIRST - 1, "app27")
        );
        // A folder the person added, with no git and no use recorded, comes first.
        let mut with_mine = rows.clone();
        with_mine.push(Found {
            mine: true,
            last_used: None,
            ..found("home/picked/handbook", false, 0)
        });
        let (list, _) = shown(&with_mine, "", &home, &|_| false);
        assert_eq!(with_mine[list[0]].name, "handbook");
        // The search looks at all of them, by name and path, without case.
        let (list, more) = shown(&rows, "APP2", &home, &|_| false);
        assert_eq!((list.len(), more), (10, 0));
        let (list, _) = shown(&rows, "code app03", &home, &|_| false);
        assert_eq!(
            list.iter()
                .map(|&i| rows[i].name.as_str())
                .collect::<Vec<_>>(),
            vec!["app03"]
        );
        assert_eq!(shown(&rows, "nothing", &home, &|_| false), (Vec::new(), 0));
        let many: Vec<Found> = (0..100)
            .map(|i| found(&format!("home/w/app{i:03}"), true, i))
            .collect();
        assert_eq!(shown(&many, "app", &home, &|_| false).1, 100 - SEARCH_MOST);
    }

    #[test]
    fn a_linked_worktree_joins_its_main_checkout() {
        let work = PathBuf::from(if cfg!(windows) { r"C:\work" } else { "/work" });
        let root = work.join("app-login");
        let main = work.join("app");
        let absolute = format!(
            "gitdir: {}\n",
            main.join(".git")
                .join("worktrees")
                .join("app-login")
                .display()
        );
        assert_eq!(linked_main(&absolute, &root), Some(main.clone()));
        let relative = format!(
            "gitdir: {}",
            Path::new("..")
                .join("app")
                .join(".git")
                .join("worktrees")
                .join("login")
                .display()
        );
        assert_eq!(linked_main(&relative, &root), Some(main.clone()));
        let submodule = format!(
            "gitdir: {}",
            Path::new("..")
                .join(".git")
                .join("modules")
                .join("lib")
                .display()
        );
        assert_eq!(linked_main(&submodule, &root), None);
        assert_eq!(linked_main("not a git file", &root), None);

        let project = |root: &Path, sources: Vec<Source>, seconds: u64| FoundProject {
            root: root.to_path_buf(),
            name: root.file_name().unwrap().to_string_lossy().into_owned(),
            sources,
            last_used: Some(UNIX_EPOCH + Duration::from_secs(seconds)),
        };
        let other = work.join("tool-fix");
        let folded = fold_worktrees(vec![
            (
                project(&root, vec![Source::ClaudeCode], 30),
                Checkout::Linked(main.clone()),
            ),
            (project(&main, vec![Source::Cursor], 10), Checkout::Main),
            (
                project(&other, vec![Source::Codex], 20),
                Checkout::Linked(work.join("tool")),
            ),
            (
                project(&work.join("notes"), vec![Source::VsCode], 5),
                Checkout::Plain,
            ),
        ]);
        let names: Vec<&str> = folded.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["app", "tool-fix", "notes"],
            "the worktree joins its main checkout, which takes its last use"
        );
        assert_eq!(folded[0].sources, vec![Source::Cursor, Source::ClaudeCode]);
        assert!(folded[0].git && folded[1].git && !folded[2].git);
    }

    #[test]
    fn a_short_window_drops_the_sentence_of_a_step() {
        assert!(!short(560., 1.), "900 x 560 at 100 %");
        assert!(short(560., 1.3), "900 x 560 at 130 %");
        assert!(!short(900., 1.3));
    }

    #[test]
    fn a_sign_in_terminal_fits_the_window() {
        assert_eq!(terminal_height(900., 1.), 300., "a tall window: the most");
        assert_eq!(terminal_height(560., 1.), 230.);
        // 560 high at 130 % is 431 logical pixels: the least, and the step scrolls a little.
        assert_eq!(terminal_height(560., 1.3), 120.);
    }

    #[test]
    fn the_art_panel_hides_in_a_narrow_window() {
        // 900 x 560, the smallest window, at 100 %: the panel and the steps fit side by side.
        let art = art_width(900., 1.).expect("room for the panel");
        assert!(900. - art - 3. * PAD >= FORM_MIN, "{art}");
        // At 130 % the same window is 692 logical pixels wide: the steps take it all.
        assert_eq!(art_width(900., 1.3), None);
        assert_eq!(art_width(1100., 1.3), None);
        assert_eq!(art_width(2560., 1.), Some(ART_MAX));
        assert_eq!(art_width(1440., 1.15).map(f32::round), Some(501.));
    }

    #[test]
    fn short_paths_start_at_the_home_folder() {
        let home = Path::new("home").join("dev");
        assert_eq!(
            short_path(&home.join("code").join("app"), &home),
            Path::new("~")
                .join("code")
                .join("app")
                .display()
                .to_string()
        );
        assert_eq!(short_path(&home, &home), "~");
        let elsewhere = Path::new("srv").join("app");
        assert_eq!(
            short_path(&elsewhere, &home),
            elsewhere.display().to_string()
        );
    }

    /// Rule 12 of the plan at the strength of the first run (1.0, above the 0.85 of U04's test):
    /// the words of the art panel are `text` on strong glass over the backdrop at its brightest,
    /// in each palette.
    #[test]
    fn the_art_panel_reads_over_the_backdrop_at_full_strength() {
        use crate::theme::{CLASSIC, CONTRAST, GLASS_DARK, GLASS_LIGHT, Palette, c, contrast};
        use glasscode_core::backdrop::{self as pixels, Finish, Preset};
        let linear = |v: f32| {
            let v = v / 255.;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let palettes: [&Palette; 4] = [&GLASS_LIGHT, &GLASS_DARK, &CLASSIC, &CONTRAST];
        let mut lowest = f32::MAX;
        for preset in Preset::ALL {
            for seconds in [0., 7., 23., 61.] {
                let frames = [false, true].map(|light| {
                    pixels::render(preset, Finish::Smooth, light, 1.0, seconds, 320, 200)
                });
                for p in palettes {
                    let glass = p.glass_strong;
                    let g = [
                        (glass.rgb >> 16) as u8,
                        (glass.rgb >> 8) as u8,
                        glass.rgb as u8,
                    ]
                    .map(f32::from);
                    // Light text is weakest on the brightest surface, dark text on the darkest.
                    let mut worst = (if p.light { f32::MAX } else { f32::MIN }, [0f32; 3]);
                    for px in frames[usize::from(p.light)].bgra.chunks(4) {
                        let surface = [px[2], px[1], px[0]].map(f32::from);
                        let surface =
                            [0, 1, 2].map(|i| surface[i] * (1. - glass.alpha) + g[i] * glass.alpha);
                        let lum = 0.2126 * linear(surface[0])
                            + 0.7152 * linear(surface[1])
                            + 0.0722 * linear(surface[2]);
                        if (p.light && lum < worst.0) || (!p.light && lum > worst.0) {
                            worst = (lum, surface);
                        }
                    }
                    let [r, g, b] = worst.1.map(|v| v / 255.);
                    let text = contrast(c(p.text), gpui::Rgba { r, g, b, a: 1. }.into());
                    lowest = lowest.min(text);
                    assert!(
                        text >= 7.0,
                        "text {text:.2} on the art panel: {}, {preset:?}, {seconds} s",
                        p.name
                    );
                }
            }
        }
        eprintln!("lowest contrast of the art panel's words: {lowest:.2}");
    }
}
