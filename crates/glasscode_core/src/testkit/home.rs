//! A scratch Glasscode home (M02 step 1): projects with git repositories, scratch accounts, the
//! agents' own files and a `state.json`, in a folder of the test's own.
//!
//! The folder holds:
//! - `glasscode/`: the Glasscode folder (`GLASSCODE_HOME`), with `state.json`, the two scratch
//!   accounts under `accounts/` (`claude-test`, `codex-test`: never the person's own folders), and
//!   the worktrees under `worktrees/`;
//! - `projects/`: the projects' main checkouts, outside the Glasscode folder, as a person's are;
//! - `remotes/`: bare repositories that stand for GitHub. Remotes are named
//!   `https://github.com/<owner>/<name>.git` and each checkout rewrites that to its bare repository
//!   (`url.<…>.insteadOf`), so nothing reaches the network;
//! - `user/`: a home folder for the programs a test starts (`HOME`), with planted credentials that
//!   are not credentials;
//! - `scenarios/`: the stand-ins' default scenarios; `gitconfig`: the only git configuration the
//!   kit's git calls read; `gh-calls.jsonl`: what the `gh` stand-in was asked.
//!
//! [`Home::new`] makes the folder this process's Glasscode folder (`paths::root`) until it is
//! dropped. A test of the core gets it through `GLASSCODE_HOME`; a test of the `testkit` feature
//! (the eval suites) through [`paths::use_test_root`], which no variable can redirect afterwards.
//! [`build_at`] fills a folder without either, for the binary `make-test-home`.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::agents::{self, Ending, FileSink, Mode, Out};
use crate::model::{Account, AgentKind, Defaults, Project, Session, State};
use crate::paths;

/// One repository checkout of a scratch home.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectSpec {
    /// The project's name and the folder of its main checkout (`projects/<name>`).
    pub name: String,
    /// `owner/name` on the stand-in GitHub; projects with the same `repo` are clones of one
    /// repository.
    pub repo: String,
    pub kind: RepoKind,
    /// Branches with a Glasscode worktree (`worktrees/<name>/<branch>`).
    pub worktrees: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepoKind {
    /// A clone of `repo`.
    Clone,
    /// A clone of the fork `repo`, whose `upstream` remote is the repository it was forked from.
    Fork { upstream: String },
    /// A clone with the last commit only (`git clone --depth 1`).
    Shallow,
}

/// What a scratch home holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HomeSpec {
    pub projects: Vec<ProjectSpec>,
    /// A Claude and a Codex conversation in the first project, with their sessions in `state.json`.
    pub conversations: bool,
    /// Claude's auto-memory folder of each project, and a Codex memories folder.
    pub agent_memories: bool,
    /// When the home was made, in milliseconds: commits, transcripts and rollouts are dated from it.
    pub start_ms: u64,
}

impl Default for HomeSpec {
    /// Four projects: `app` (with a worktree), `app-copy` (a second clone of the same repository),
    /// `billing` (a clone of a fork) and `docs` (a shallow clone); conversations and memories.
    fn default() -> HomeSpec {
        let project = |name: &str, repo: &str, kind: RepoKind, worktrees: &[&str]| ProjectSpec { name: name.into(), repo: repo.into(), kind, worktrees: worktrees.iter().map(|w| w.to_string()).collect() };
        HomeSpec {
            projects: vec![
                project("app", "acme/app", RepoKind::Clone, &["feat/login"]),
                project("app-copy", "acme/app", RepoKind::Clone, &[]),
                project("billing", "alex/billing", RepoKind::Fork { upstream: "acme/billing".into() }, &[]),
                project("docs", "acme/docs", RepoKind::Shallow, &[]),
            ],
            conversations: true,
            agent_memories: true,
            start_ms: super::clock::utc_ms(2026, 11, 19, 9, 0),
        }
    }
}

impl HomeSpec {
    /// One project and nothing else: the quickest home.
    pub fn bare() -> HomeSpec {
        HomeSpec { projects: vec![HomeSpec::default().projects.remove(0)], conversations: false, agent_memories: false, ..HomeSpec::default() }
    }
}

/// A project as it was made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltProject {
    pub id: String,
    pub name: String,
    pub repo: String,
    /// The main checkout.
    pub root: PathBuf,
    /// The bare repository `origin` stands for.
    pub origin: PathBuf,
    /// The bare repository `upstream` stands for, for a fork.
    pub upstream: Option<PathBuf>,
    pub worktrees: Vec<PathBuf>,
}

/// A conversation the home was made with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    pub agent: AgentKind,
    pub id: String,
    /// Its transcript or rollout.
    pub path: PathBuf,
    pub cwd: PathBuf,
}

/// What [`build_at`] made, and where.
#[derive(Clone, Debug)]
pub struct Built {
    pub top: PathBuf,
    pub glasscode: PathBuf,
    pub user: PathBuf,
    pub projects: Vec<BuiltProject>,
    pub accounts: Vec<Account>,
    pub conversations: Vec<Conversation>,
    pub scenarios: PathBuf,
    pub gitconfig: PathBuf,
    pub gh_log: PathBuf,
}

/// The file that marks the top folder of a scratch home: the stand-ins run a command (a scenario's
/// `run` step) only in a folder below it.
pub const MARKER: &str = ".glasscode-test-home";

/// The scratch accounts: never `system_default`, so their folders are under `accounts/`.
pub const CLAUDE_ACCOUNT: &str = "claude-test";
pub const CODEX_ACCOUNT: &str = "codex-test";

/// The person's own folders, which no test may name (rule 11, exec-m4).
pub fn real_folders() -> Vec<PathBuf> {
    let home = paths::home();
    [".glasscode", ".claude", ".claude.json", ".codex"].iter().map(|name| home.join(name)).collect()
}

/// `path` with its links resolved: those of its nearest folder that exists, and the rest of it as
/// written, so that a folder not made yet behind a link is where the link leads.
fn resolved(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        if let Ok(real) = current.canonicalize() {
            return rest.into_iter().rev().fold(real, |path, part| path.join(part));
        }
        match (current.file_name().map(|name| name.to_os_string()), current.parent()) {
            (Some(name), Some(parent)) if !parent.as_os_str().is_empty() => {
                rest.push(name);
                current = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

fn folded(path: &Path) -> String {
    let text = resolved(path).to_string_lossy().replace('\\', "/");
    let text = text.strip_prefix("//?/").map(str::to_string).unwrap_or(text);
    if cfg!(any(target_os = "macos", windows)) { text.to_lowercase() } else { text }
}

/// Whether `path` is one of the person's own folders, or inside one.
pub fn is_real(path: &Path) -> bool {
    let path = folded(path);
    real_folders().iter().map(|folder| folded(folder)).any(|folder| path == folder || path.starts_with(&format!("{folder}/")))
}

/// Panics when `path` is one of the person's own folders, or inside one.
pub fn assert_scratch(path: &Path) {
    assert!(!is_real(path), "{} is a folder of the person's own: the test kit works in scratch folders only (rule 11)", path.display());
}

/// The git of the kit: its own configuration only, a fixed author, and dates from the spec, so a
/// home made twice has the same commits.
struct Git {
    config: PathBuf,
    next: std::cell::Cell<u64>,
}

impl Git {
    fn run(&self, dir: &Path, args: &[&str]) -> String {
        let seconds = self.next.get();
        self.next.set(seconds + 60);
        let date = format!("@{seconds} +0000");
        let mut command = crate::sys::command("git");
        command.arg("-C").arg(dir).args(args);
        command.env("GIT_CONFIG_GLOBAL", &self.config).env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_AUTHOR_DATE", &date).env("GIT_COMMITTER_DATE", &date).env("GIT_TERMINAL_PROMPT", "0");
        // A test run from a git hook has these set for the repository of the hook.
        for name in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY", "GIT_CEILING_DIRECTORIES"] {
            command.env_remove(name);
        }
        let output = command.output().unwrap_or_else(|err| panic!("git {args:?}: {err}"));
        assert!(output.status.success(), "git {args:?} in {}: {}", dir.display(), String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}

fn write(path: &Path, text: &str) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap_or_else(|err| panic!("{}: {err}", dir.display()));
    }
    std::fs::write(path, text).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
}

/// A folder by its real path on Unix. Windows keeps the path as it is: its real path has a `\\?\`
/// prefix that git and the CLIs do not take.
fn real_path(path: &Path) -> PathBuf {
    if cfg!(windows) { path.to_path_buf() } else { path.canonicalize().unwrap_or_else(|_| path.to_path_buf()) }
}

/// The stand-in GitHub's address of a repository.
pub fn github_url(repo: &str) -> String {
    format!("https://github.com/{repo}.git")
}

/// The files of each repository's first commit.
fn first_files(repo: &str) -> Vec<(&'static str, String)> {
    let name = repo.rsplit('/').next().unwrap_or(repo);
    vec![
        ("README.md", format!("# {name}\n\nRun ./setup.sh to install.\n")),
        ("CLAUDE.md", "# Notes for Claude\n\nRun `bun test` before you push.\n".into()),
        ("AGENTS.md", "# Notes for agents\n\nRun `bun test` before you push.\n".into()),
        (".gitignore", ".env\nnode_modules/\n".into()),
        ("src/main.ts", format!("export const name = \"{name}\";\n")),
    ]
}

/// Makes the bare repository of `repo` under `remotes/`, with three commits, once.
fn make_remote(git: &Git, top: &Path, repo: &str) -> PathBuf {
    let bare = top.join("remotes").join(format!("{repo}.git"));
    if bare.exists() {
        return bare;
    }
    let seed = top.join("seed").join(repo);
    std::fs::create_dir_all(&seed).unwrap();
    git.run(&seed, &["init", "-q", "-b", "main"]);
    for (file, text) in first_files(repo) {
        write(&seed.join(file), &text);
    }
    git.run(&seed, &["add", "-A"]);
    git.run(&seed, &["commit", "-q", "-m", "Start the project"]);
    write(&seed.join("docs/setup.md"), "# Setup\n\n1. Install bun.\n2. Run make setup.\n");
    git.run(&seed, &["add", "-A"]);
    git.run(&seed, &["commit", "-q", "-m", "docs: setup steps"]);
    write(&seed.join("src/main.ts"), "export const name = \"app\";\nexport const retryBackoffMs = 250;\n");
    git.run(&seed, &["commit", "-q", "-am", "Retry with a backoff"]);
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    git.run(top, &["clone", "-q", "--bare", &seed.to_string_lossy(), &bare.to_string_lossy()]);
    bare
}

/// Points the stand-in GitHub at the bare repositories, in a checkout's own configuration.
fn rewrite_github(git: &Git, checkout: &Path, top: &Path) {
    let base = url::Url::from_directory_path(top.join("remotes")).expect("an absolute folder").to_string();
    git.run(checkout, &["config", &format!("url.{base}.insteadOf"), "https://github.com/"]);
}

fn make_project(git: &Git, top: &Path, glasscode: &Path, spec: &ProjectSpec) -> BuiltProject {
    let root = top.join("projects").join(&spec.name);
    std::fs::create_dir_all(top.join("projects")).unwrap();
    let mut upstream = None;
    let origin = match &spec.kind {
        RepoKind::Fork { upstream: from } => {
            let original = make_remote(git, top, from);
            let fork = top.join("remotes").join(format!("{}.git", spec.repo));
            if !fork.exists() {
                std::fs::create_dir_all(fork.parent().unwrap()).unwrap();
                git.run(top, &["clone", "-q", "--bare", &original.to_string_lossy(), &fork.to_string_lossy()]);
            }
            upstream = Some(original);
            fork
        }
        _ => make_remote(git, top, &spec.repo),
    };
    // The clone reads the scratch gitconfig, whose insteadOf points GitHub at `remotes/`; a
    // shallow clone needs the file:// form, which it gets the same way.
    let mut clone = vec!["clone", "-q"];
    if spec.kind == RepoKind::Shallow {
        clone.extend(["--depth", "1"]);
    }
    let url = github_url(&spec.repo);
    let target = root.to_string_lossy().into_owned();
    clone.extend([url.as_str(), target.as_str()]);
    git.run(top, &clone);
    rewrite_github(git, &root, top);
    if let RepoKind::Fork { upstream: from } = &spec.kind {
        git.run(&root, &["remote", "add", "upstream", &github_url(from)]);
        git.run(&root, &["fetch", "-q", "upstream"]);
    }
    // An untracked .env with a value that is no secret (the memory scan refuses values of it).
    write(&root.join(".env"), &format!("API_TOKEN=test-{}-not-a-secret\n", spec.name));
    let mut worktrees = Vec::new();
    for branch in &spec.worktrees {
        let dir = glasscode.join("worktrees").join(crate::git::slug(&spec.name)).join(crate::git::slug(branch));
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        git.run(&root, &["worktree", "add", "-q", "-b", branch, &dir.to_string_lossy()]);
        worktrees.push(dir);
    }
    BuiltProject { id: format!("p-{}", spec.name), name: spec.name.clone(), repo: spec.repo.clone(), root, origin, upstream, worktrees }
}

/// Claude's auto-memory of a project (`<config>/projects/<folder>/memory/`), as Claude Code 2.1
/// writes it: an index and one file per memory.
fn write_claude_memory(config: &Path, project: &BuiltProject) {
    let dir = config.join("projects").join(crate::history::claude_folder_name(&project.root)).join("memory");
    write(&dir.join("MEMORY.md"), "- [Test command](test-command.md) — run bun test before pushing\n- [Release owner](release-owner.md) — sam signs off releases\n");
    write(
        &dir.join("test-command.md"),
        "---\nname: test-command\ndescription: run bun test before pushing\nmetadata:\n  type: feedback\n---\n\nRun `bun test` before every push.\n**Why:** CI takes twenty minutes.\n",
    );
    write(&dir.join("release-owner.md"), "---\nname: release-owner\ndescription: sam signs off releases\nmetadata:\n  type: project\n---\n\nSam signs off every release of this project.\n");
}

/// A Codex memories folder. Its name and format differ between Codex versions (mem-m6); M01
/// (P04) records the real one.
fn write_codex_memories(codex_home: &Path) {
    write(&codex_home.join("memories").join("notes.md"), "# Memories\n\n- The person prefers small pull requests.\n- `make setup` replaced ./setup.sh.\n");
}

/// Writes a scratch home into `top`, which must be empty or not exist, and must not be the
/// person's own. Its Glasscode folder is `glasscode` (inside `top` or not).
pub fn build_into(top: &Path, glasscode: &Path, spec: &HomeSpec) -> Built {
    for path in [top, glasscode] {
        assert_scratch(path);
        if path.exists() {
            assert!(std::fs::read_dir(path).map(|mut entries| entries.next().is_none()).unwrap_or(false), "{} is not empty", path.display());
        }
    }
    std::fs::create_dir_all(top).unwrap();
    std::fs::create_dir_all(glasscode).unwrap();
    // The real path, as an agent's working folder reports it (`/private/var/…` on macOS); a
    // Glasscode folder outside `top` stays as `paths::root` names it.
    let glasscode = match glasscode.strip_prefix(top) {
        Ok(rest) => real_path(top).join(rest),
        Err(_) => glasscode.to_path_buf(),
    };
    let top = real_path(top);
    let gitconfig = top.join("gitconfig");
    let remotes = url::Url::from_directory_path(top.join("remotes")).expect("an absolute folder").to_string();
    write(
        &gitconfig,
        &format!(
            "[user]\n\tname = Test\n\temail = test@example.com\n[init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n[tag]\n\tgpgsign = false\n[core]\n\tautocrlf = false\n\thooksPath = {}\n[advice]\n\tdetachedHead = false\n[protocol \"file\"]\n\tallow = always\n[url \"{remotes}\"]\n\tinsteadOf = https://github.com/\n",
            top.join("no-hooks").to_string_lossy().replace('\\', "/")
        ),
    );
    let git = Git { config: gitconfig.clone(), next: std::cell::Cell::new(spec.start_ms / 1000 - 86_400) };
    let projects: Vec<BuiltProject> = spec.projects.iter().map(|project| make_project(&git, &top, &glasscode, project)).collect();

    let accounts = vec![
        Account { id: CLAUDE_ACCOUNT.into(), agent: AgentKind::Claude, label: "Test".into(), plan: None, system_default: false },
        Account { id: CODEX_ACCOUNT.into(), agent: AgentKind::Codex, label: "Test".into(), plan: None, system_default: false },
    ];
    let claude_dir = glasscode.join("accounts").join(CLAUDE_ACCOUNT);
    let codex_dir = glasscode.join("accounts").join(CODEX_ACCOUNT);
    // The folders the app makes when it starts (`paths::ensure_dirs`): hooks write into `events/`.
    for dir in [&claude_dir, &codex_dir, &glasscode.join("events"), &glasscode.join("handoff"), &glasscode.join("worktrees")] {
        std::fs::create_dir_all(dir).unwrap();
    }
    if spec.agent_memories {
        for project in &projects {
            write_claude_memory(&claude_dir, project);
        }
        write_codex_memories(&codex_dir);
    }

    let mut conversations = Vec::new();
    let mut sessions = Vec::new();
    if let (true, Some(project)) = (spec.conversations, projects.first()) {
        let cwd = &project.root;
        let id = "c0ffee00-0000-4000-8000-000000000001";
        let transcript = agents::claude_transcript(&claude_dir, cwd, id);
        let mut sink = FileSink { files: [(Out::Transcript, transcript.clone())].into(), ..FileSink::default() };
        agents::play_session(&agents::claude(Mode::Interactive, &[Ending::Completed, Ending::Question]), agents::claude_vars(id, cwd, &transcript, Mode::Interactive), spec.start_ms, &["fix the install steps in the readme", "and the setup doc?"], &mut sink);
        conversations.push(Conversation { agent: AgentKind::Claude, id: id.into(), path: transcript.clone(), cwd: cwd.clone() });

        let id = "019a0000-0000-7000-8000-000000000001";
        let start = spec.start_ms + 3_600_000;
        let rollout = agents::codex_rollout(&codex_dir, id, start);
        let mut sink = FileSink { files: [(Out::Rollout, rollout.clone()), (Out::History, codex_dir.join("history.jsonl"))].into(), ..FileSink::default() };
        agents::play_session(&agents::codex(Mode::Interactive, &[Ending::Completed]), agents::codex_vars(id, cwd, Mode::Interactive), start, &["review the retry backoff"], &mut sink);
        conversations.push(Conversation { agent: AgentKind::Codex, id: id.into(), path: rollout.clone(), cwd: cwd.clone() });

        for (n, conversation) in conversations.iter().enumerate() {
            let account = if conversation.agent == AgentKind::Claude { CLAUDE_ACCOUNT } else { CODEX_ACCOUNT };
            sessions.push(Session {
                id: format!("s-test-{}", n + 1),
                project_id: project.id.clone(),
                worktree: project.root.clone(),
                branch: "main".into(),
                title: format!("{} session", conversation.agent.name()),
                agent: conversation.agent,
                account_id: Some(account.into()),
                agent_session_id: Some(conversation.id.clone()),
                transcript_path: Some(conversation.path.clone()),
                lineage: vec![],
                initial_prompt: None,
                team_task: None,
                review: None,
                model: None,
                mode: None,
                created_at: spec.start_ms / 1000,
            });
        }
    }

    let state = State {
        projects: projects.iter().map(|p| Project { id: p.id.clone(), name: p.name.clone(), root: p.root.clone(), expanded: true, github_account: None }).collect(),
        accounts: accounts.clone(),
        sessions,
        defaults: Defaults { claude: Some(CLAUDE_ACCOUNT.into()), codex: Some(CODEX_ACCOUNT.into()) },
        ..State::default()
    };
    write(&glasscode.join("state.json"), &serde_json::to_string_pretty(&state).unwrap());

    // The stand-ins' scenarios when a test names none.
    let scenarios = top.join("scenarios");
    let turns = [Ending::Completed, Ending::Question, Ending::Pushed, Ending::Completed];
    for (name, scenario) in [
        ("claude", agents::claude(Mode::Interactive, &turns)),
        ("claude-print", agents::claude(Mode::Print, &turns)),
        ("codex", agents::codex(Mode::Interactive, &turns)),
        ("codex-exec", agents::codex(Mode::Print, &turns)),
    ] {
        scenario.save(&scenarios.join(format!("{name}.json"))).unwrap();
    }

    // A home folder for the programs a test starts, with credentials that are not credentials
    // (M22 refuses reads of them).
    let user = top.join("user");
    write(&user.join(".ssh").join("id_ed25519"), "not a key: a planted file of the Glasscode test kit\n");
    // The stand-ins run a scenario's `run` steps only below this file: in the top folder, and in a
    // Glasscode folder outside it (the run's test root of a feature build), whose worktrees are
    // the home's too. A Home's drop empties that folder, the marker with it.
    let marker = "A scratch home of the Glasscode test kit (crates/glasscode_core/src/testkit/home.rs).\n";
    write(&top.join(MARKER), marker);
    if !glasscode.starts_with(&top) {
        write(&glasscode.join(MARKER), marker);
    }
    write(&user.join(".config").join("gh").join("hosts.yml"), "github.com:\n    user: test\n    oauth_token: not-a-token\n");

    Built { gh_log: top.join("gh-calls.jsonl"), top, glasscode, user, projects, accounts, conversations, scenarios, gitconfig }
}

/// A scratch home in `top` (for the binary): its Glasscode folder is `top/glasscode`, and nothing
/// of this process points at it.
pub fn build_at(top: &Path, spec: &HomeSpec) -> Built {
    build_into(top, &top.join("glasscode"), spec)
}

/// One active [`Home`] at a time: there is one `paths::root` per process.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// The variables a Home sets for this process, and puts back.
const VARIABLES: [&str; 3] = ["GLASSCODE_HOME", "CLAUDE_CONFIG_DIR", "CODEX_HOME"];

/// A scratch home that is this process's Glasscode folder until dropped.
pub struct Home {
    built: Built,
    _top: tempfile::TempDir,
    before: Vec<(&'static str, Option<OsString>)>,
}

impl Home {
    /// The default home ([`HomeSpec::default`]). Not `Default`: making one builds repositories and
    /// changes the process's Glasscode folder.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Home {
        Home::with(&HomeSpec::default())
    }

    pub fn with(spec: &HomeSpec) -> Home {
        assert!(!ACTIVE.swap(true, Ordering::SeqCst), "one test kit Home at a time: there is one Glasscode folder per process");
        // Outside the core's own tests: temporary folders by their real path (`/private/var/…` on
        // macOS), so that the run's test root, made below if not before, and an agent's working
        // folder name the same places.
        let temp = std::env::temp_dir();
        if !cfg!(test) && cfg!(unix) && real_path(&temp) != temp {
            // SAFETY: as for the variables below.
            unsafe { std::env::set_var("TMPDIR", real_path(&temp)) };
        }
        let top = tempfile::tempdir().expect("a temporary folder");
        // The core's tests read GLASSCODE_HOME first; a test of another crate cannot be redirected
        // once it has the run's test root, so the home's Glasscode folder is that root.
        let glasscode = if cfg!(test) {
            top.path().join("glasscode")
        } else {
            let root = paths::use_test_root();
            empty(&root);
            root
        };
        let built = build_into(top.path(), &glasscode, spec);
        let before = VARIABLES.iter().map(|name| (*name, std::env::var_os(name))).collect();
        let values = [built.glasscode.clone(), built.glasscode.join("accounts").join(CLAUDE_ACCOUNT), built.glasscode.join("accounts").join(CODEX_ACCOUNT)];
        for (name, value) in VARIABLES.iter().zip(values) {
            // SAFETY: the tests of the kit's users run one at a time (`--test-threads=1`).
            unsafe { std::env::set_var(name, value) };
        }
        let home = Home { built, _top: top, before };
        assert!(paths::same_folder(&paths::root(), &home.built.glasscode), "the Glasscode folder is {}, not the home's {}", paths::root().display(), home.built.glasscode.display());
        home
    }

    pub fn built(&self) -> &Built {
        &self.built
    }

    /// The folder that holds everything of this home.
    pub fn top(&self) -> &Path {
        &self.built.top
    }

    /// The Glasscode folder (`paths::root` while this home lives).
    pub fn glasscode(&self) -> &Path {
        &self.built.glasscode
    }

    pub fn project(&self, name: &str) -> &BuiltProject {
        self.built.projects.iter().find(|p| p.name == name).unwrap_or_else(|| panic!("no project {name} in this home"))
    }

    pub fn account(&self, agent: AgentKind) -> &Account {
        self.built.accounts.iter().find(|a| a.agent == agent).unwrap_or_else(|| panic!("no {} account in this home", agent.name()))
    }

    /// `CLAUDE_CONFIG_DIR` of the scratch Claude account.
    pub fn claude_config_dir(&self) -> PathBuf {
        self.built.glasscode.join("accounts").join(CLAUDE_ACCOUNT)
    }

    /// `CODEX_HOME` of the scratch Codex account.
    pub fn codex_home(&self) -> PathBuf {
        self.built.glasscode.join("accounts").join(CODEX_ACCOUNT)
    }

    /// The variables of a program a test starts in this home: the stand-ins first on `PATH`, and
    /// the scratch folders. They add no credential, but do not take away what the process has:
    /// a command built from them needs [`clear_credentials`] too, as [`Home::command`] does.
    pub fn agent_env(&self) -> HashMap<String, String> {
        agent_env(&self.built)
    }

    /// `program` with [`Home::agent_env`], and without the variables that carry the person's
    /// credentials ([`CREDENTIAL_VARIABLES`]).
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
        let mut command = crate::sys::command(program);
        clear_credentials(&mut command);
        command.envs(self.agent_env());
        command
    }

    /// Runs git in `dir` with the home's configuration.
    pub fn git(&self, dir: &Path, args: &[&str]) -> String {
        Git { config: self.built.gitconfig.clone(), next: std::cell::Cell::new(clock_seconds()) }.run(dir, args)
    }
}

fn clock_seconds() -> u64 {
    crate::clock::now_ms() / 1000
}

/// Variables a program a test starts never gets: the vendors' and the forges' tokens, the SSH agent
/// and cloud keys (a hook and a `run` step are real shells).
pub const CREDENTIAL_VARIABLES: [&str; 15] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "GOOGLE_APPLICATION_CREDENTIALS",
];

/// Takes the [`CREDENTIAL_VARIABLES`] away from what `command` inherits.
pub fn clear_credentials(command: &mut std::process::Command) {
    for name in CREDENTIAL_VARIABLES {
        command.env_remove(name);
    }
}

/// The environment of a program started in the home `built` (see [`Home::agent_env`]).
pub fn agent_env(built: &Built) -> HashMap<String, String> {
    let text = |path: &Path| path.to_string_lossy().into_owned();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(std::iter::once(agents::scripts_dir()).chain(std::env::split_paths(&path))).expect("a PATH");
    let mut env = HashMap::from([
        ("PATH".to_string(), path.to_string_lossy().into_owned()),
        ("GLASSCODE_HOME".to_string(), text(&built.glasscode)),
        ("CLAUDE_CONFIG_DIR".to_string(), text(&built.glasscode.join("accounts").join(CLAUDE_ACCOUNT))),
        ("CODEX_HOME".to_string(), text(&built.glasscode.join("accounts").join(CODEX_ACCOUNT))),
        ("HOME".to_string(), text(&built.user)),
        ("GIT_CONFIG_GLOBAL".to_string(), text(&built.gitconfig)),
        ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
        (agents::SCENARIOS_VARIABLE.to_string(), text(&built.scenarios)),
        ("GLASSCODE_TEST_GH_LOG".to_string(), text(&built.gh_log)),
        // The person's own home, which the stand-ins refuse as well as the one they find themselves.
        ("GLASSCODE_TEST_PERSON_HOME".to_string(), text(&paths::home())),
    ]);
    if cfg!(windows) {
        env.insert("USERPROFILE".to_string(), text(&built.user));
    }
    env
}

/// Removes what is in `dir`, which must be this process's test root.
fn empty(dir: &Path) {
    let in_temp = dir.parent().is_some_and(|parent| real_path(parent) == real_path(&std::env::temp_dir()));
    assert!(in_temp && dir.file_name().is_some_and(|name| name.to_string_lossy().starts_with("glasscode-test-home-")), "{} is not a test root", dir.display());
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let _ = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        for (name, value) in self.before.drain(..) {
            // SAFETY: as in `with`.
            match value {
                Some(value) => unsafe { std::env::set_var(name, value) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
        if !cfg!(test) {
            empty(&self.built.glasscode);
        }
        ACTIVE.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing of a home is the person's own, and the process's Glasscode folder is the home's
    /// while it lives (rule 11, exec-m4).
    #[test]
    fn a_home_touches_none_of_the_persons_folders() {
        let before = paths::root();
        let home = Home::with(&HomeSpec::bare());
        assert_eq!(paths::root(), home.glasscode());
        for path in [home.top().to_path_buf(), home.glasscode().to_path_buf(), home.claude_config_dir(), home.codex_home(), home.built().user.clone()] {
            assert!(!is_real(&path), "{}", path.display());
        }
        for account in &home.built().accounts {
            assert!(!account.system_default && account.effective_dir().starts_with(home.glasscode()), "{account:?}");
        }
        let env = home.agent_env();
        for name in ["GLASSCODE_HOME", "CLAUDE_CONFIG_DIR", "CODEX_HOME", "HOME"] {
            assert!(!is_real(Path::new(&env[name])), "{name}: {}", env[name]);
        }
        assert_eq!(std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from), Some(home.claude_config_dir()));
        // The person's folders are recognised in any spelling.
        for folder in real_folders() {
            assert!(is_real(&folder) && is_real(&folder.join("projects")), "{}", folder.display());
        }
        assert!(!is_real(&paths::home().join(".claude-other")));
        drop(home);
        assert_eq!(paths::root(), before, "the folder before the home again");
        // One at a time.
        let _first = Home::with(&HomeSpec::bare());
        assert!(std::panic::catch_unwind(|| Home::with(&HomeSpec::bare())).is_err());
    }

    /// A variable that carries a credential is set here; a program started with the kit's command
    /// does not see it (hooks and `run` steps are real shells).
    #[test]
    fn a_started_program_gets_none_of_the_persons_credentials() {
        let home = Home::with(&HomeSpec::bare());
        let before: Vec<(&str, Option<OsString>)> = CREDENTIAL_VARIABLES.iter().map(|name| (*name, std::env::var_os(name))).collect();
        for name in CREDENTIAL_VARIABLES {
            // SAFETY: the core's tests run one at a time (`--test-threads=1`).
            unsafe { std::env::set_var(name, "planted-by-the-test") };
        }
        let mut command = if cfg!(windows) { home.command("cmd") } else { home.command("env") };
        if cfg!(windows) {
            command.args(["/c", "set"]);
        }
        let out = command.output().unwrap();
        for (name, value) in before {
            // SAFETY: as above.
            match value {
                Some(value) => unsafe { std::env::set_var(name, value) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
        let listed = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success() && listed.contains("GLASSCODE_HOME="), "{listed}");
        assert!(!listed.contains("planted-by-the-test"), "{listed}");
        // A command made from agent_env alone keeps them; clear_credentials takes them away.
        assert!(CREDENTIAL_VARIABLES.iter().all(|name| !home.agent_env().contains_key(*name)));
        let mut own = crate::sys::command("x");
        clear_credentials(&mut own);
        let removed: Vec<String> = own.get_envs().filter(|(_, value)| value.is_none()).map(|(name, _)| name.to_string_lossy().into_owned()).collect();
        assert_eq!(removed.len(), CREDENTIAL_VARIABLES.len(), "{removed:?}");
    }

    /// A folder that does not exist yet behind a link into the person's folders is theirs (review
    /// of PR 85: `make-test-home lnk/new` built a home inside `~/.claude`).
    #[cfg(unix)]
    #[test]
    fn a_new_folder_behind_a_link_is_the_persons() {
        let fake = tempfile::tempdir().unwrap();
        let person = fake.path().canonicalize().unwrap().join("person");
        std::fs::create_dir_all(person.join(".claude")).unwrap();
        let link = fake.path().join("lnk");
        std::os::unix::fs::symlink(person.join(".claude"), &link).unwrap();
        let before = std::env::var_os("HOME");
        // SAFETY: the core's tests run one at a time (`--test-threads=1`).
        unsafe { std::env::set_var("HOME", &person) };
        let (behind, beside, plain) = (is_real(&link.join("new2")), is_real(&fake.path().join("other").join("new")), is_real(&person.join(".claude").join("a").join("b")));
        match before {
            // SAFETY: as above.
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        assert!(behind, "a new folder behind the link is in the person's .claude");
        assert!(!beside && plain);
    }

    #[test]
    fn home_builds_two_clones_of_one_repo() {
        let home = Home::new();
        let (app, copy) = (home.project("app"), home.project("app-copy"));
        assert_ne!(app.root, copy.root);
        assert_eq!(app.origin, copy.origin, "one repository");
        // The remote is named as on GitHub (`remote get-url` would show where insteadOf sends it).
        for project in [app, copy] {
            assert_eq!(home.git(&project.root, &["config", "--get", "remote.origin.url"]), "https://github.com/acme/app.git");
        }
        assert_eq!(home.git(&app.root, &["rev-parse", "HEAD"]), home.git(&copy.root, &["rev-parse", "HEAD"]));
        // The stand-in GitHub answers through the bare repository, never the network.
        home.git(&copy.root, &["fetch", "-q", "origin"]);
        // A worktree of app, under Glasscode's worktrees folder.
        let worktree = &app.worktrees[0];
        assert!(worktree.starts_with(paths::worktrees_dir()), "{}", worktree.display());
        assert_eq!(home.git(worktree, &["branch", "--show-current"]), "feat/login");
        assert_eq!(paths::class_of(worktree).class(), Some(paths::PathClass::Worktree));
        // The fork and its upstream; the shallow clone.
        let billing = home.project("billing");
        assert_eq!(home.git(&billing.root, &["config", "--get", "remote.upstream.url"]), "https://github.com/acme/billing.git");
        assert_eq!(home.git(&billing.root, &["config", "--get", "remote.origin.url"]), "https://github.com/alex/billing.git");
        assert!(home.git(&billing.root, &["remote", "get-url", "origin"]).starts_with("file:///"), "nothing reaches the network");
        assert_ne!(billing.upstream.as_ref(), Some(&billing.origin));
        let docs = home.project("docs");
        assert_eq!(home.git(&docs.root, &["rev-parse", "--is-shallow-repository"]), "true");
        assert_eq!(home.git(&docs.root, &["rev-list", "--count", "HEAD"]), "1");
        assert_eq!(home.git(&app.root, &["rev-list", "--count", "HEAD"]), "3");
        // The projects live outside the Glasscode folder, as a person's do.
        assert!(!app.root.starts_with(home.glasscode()));
        assert!(app.root.join(".env").is_file() && home.git(&app.root, &["status", "--porcelain"]).is_empty(), "the .env is ignored");
    }

    /// The same spec makes the same commits: a home is a fixture.
    #[test]
    fn a_home_is_the_same_each_time() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let spec = HomeSpec::bare();
        let a = build_at(&first.path().join("h"), &spec);
        let b = build_at(&second.path().join("h"), &spec);
        let head = |built: &Built| Git { config: built.gitconfig.clone(), next: std::cell::Cell::new(0) }.run(&built.projects[0].root, &["rev-parse", "HEAD"]);
        assert_eq!(head(&a), head(&b));
        // Nothing of the binary's home is this process's.
        assert_ne!(paths::root(), a.glasscode);
        // A folder that is not empty is refused.
        assert!(std::panic::catch_unwind(|| build_at(&first.path().join("h"), &spec)).is_err());
    }

    /// What Glasscode itself reads of a home: its state, its accounts' conversations, the agents'
    /// memory folders.
    #[test]
    fn glasscode_reads_the_home() {
        let home = Home::new();
        let state = crate::store::load();
        assert_eq!(state.projects.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["app", "app-copy", "billing", "docs"]);
        assert_eq!(state.sessions.len(), 2);
        let app = home.project("app");
        let claude = crate::history::conversations(AgentKind::Claude, &state.accounts, &app.root);
        assert_eq!(claude.iter().map(|c| (c.id.as_str(), c.title.as_str())).collect::<Vec<_>>(), [("c0ffee00-0000-4000-8000-000000000001", "Point the readme at make setup")]);
        let codex = crate::history::conversations(AgentKind::Codex, &state.accounts, &app.root);
        assert_eq!(codex.iter().map(|c| (c.id.as_str(), c.title.as_str())).collect::<Vec<_>>(), [("019a0000-0000-7000-8000-000000000001", "review the retry backoff")]);
        // The other clone of the same repository has no conversation of its own.
        assert!(crate::history::conversations(AgentKind::Claude, &state.accounts, &home.project("app-copy").root).is_empty());
        for session in &state.sessions {
            let path = session.transcript_path.as_ref().unwrap();
            assert!(path.is_file() && path.starts_with(home.glasscode()), "{}", path.display());
        }
        let memory = home.claude_config_dir().join("projects").join(crate::history::claude_folder_name(&app.root)).join("memory");
        assert!(std::fs::read_to_string(memory.join("MEMORY.md")).unwrap().contains("(test-command.md)"));
        assert!(home.codex_home().join("memories").join("notes.md").is_file());
        // Every file the home put in the Glasscode folder has a class of the layout.
        let mut stack = vec![home.glasscode().to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                assert!(paths::class_of(&path).class().is_some(), "{} has no class", path.display());
                if path.is_dir() && !path.ends_with(".git") {
                    stack.push(path);
                }
            }
        }
    }
}
