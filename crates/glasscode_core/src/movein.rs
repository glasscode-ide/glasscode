//! Moving in, part 1 (M35): what is already on this computer about the person's projects becomes
//! proposals in memory, with no model, after the person ticks what may be read.
//!
//! 1. **Look** ([`look`]): names and paths only. Nothing is opened before the person's tick:
//!    - Claude's memory folders (`<config>/projects/<folder>/memory/`), matched by the exact folder
//!      name Claude makes of each project folder and worktree (`history::claude_folder_name`), so
//!      no name is decoded;
//!    - Codex's memories (`<codex home>/memories/`, by the name the version uses);
//!    - the editors' and agents' rules files of each project ([`RULES_FILES`], crit-20);
//!    - `CODEOWNERS`;
//!    - 90 days of git history (who committed);
//!    - the board (who asks for work, and how), and Linear's teams from its last fetch;
//!    - GitHub teams of the organisations that own projects (read only, a network call, unticked).
//!
//!    A project whose remote owner is none of the person's GitHub logins is an organisation's: its
//!    agents' memories start unticked (every project starts Careful anyway, O10).
//! 2. **Inventory** ([`inventory`]): languages, scripts, CI files and owners, from file names and a
//!    few small files, with no model.
//! 3. **Harvest** ([`harvest`]): bullets and lines of the ticked sources, CODEOWNERS lines and the
//!    committers, each normalised, deduplicated across sources (one proposal with every source),
//!    with contradictions flagged (two lines about the same thing that say different things).
//! 4. **Proposals** go to `inbox/proposals.md` in one commit per project, each with its source
//!    (`source=import:<path>`) and the run (`run=<id>`), so a run can be undone as a whole
//!    ([`undo`]). Accepting is the person's click: the entry is confirmed, `import:` kept as its
//!    origin.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::memstore::{self, MemFile, Memory, MemoryError, Who};
use crate::paths::ProjectKey;

/// Where something to move in comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SourceKind {
    ClaudeMemory,
    CodexMemory,
    RulesFile,
    CodeOwners,
    GitHistory,
    /// The study of a project (M54): a line a headless run drafted from what Glasscode read there.
    Study,
    /// The board: who asks for work, and through what.
    Board,
    /// Linear's teams and who works in them, from the Tickets view's last fetch.
    LinearTeams,
    /// An organisation's GitHub teams and their members: a read of GitHub's API.
    GitHubTeams,
}

impl SourceKind {
    pub fn label(self) -> &'static str {
        match self {
            SourceKind::ClaudeMemory => "Claude's memory",
            SourceKind::CodexMemory => "Codex's memories",
            SourceKind::RulesFile => "rules file",
            SourceKind::CodeOwners => "CODEOWNERS",
            SourceKind::GitHistory => "90 days of git history",
            SourceKind::Study => "the study of the project",
            SourceKind::Board => "the board",
            SourceKind::LinearTeams => "Linear teams",
            SourceKind::GitHubTeams => "GitHub teams (online)",
        }
    }

}

/// The rules files of the editors and agents, in a project folder (crit-20). A folder entry is
/// read whole (its `.md` and `.mdc` files).
pub const RULES_FILES: &[&str] = &[".cursor/rules", ".cursorrules", ".windsurfrules", ".github/copilot-instructions.md", "GEMINI.md", "AGENTS.md", "CLAUDE.md"];

/// Where `CODEOWNERS` may be.
const CODEOWNERS: &[&str] = &["CODEOWNERS", ".github/CODEOWNERS", "docs/CODEOWNERS"];

/// A project as moving in sees it.
#[derive(Clone, Debug)]
pub struct ProjectIn {
    pub key: ProjectKey,
    pub name: String,
    pub root: PathBuf,
    pub worktrees: Vec<PathBuf>,
    /// The owner of its remote (`acme` of `github.com/acme/app`).
    pub owner: Option<String>,
}

/// One source found: what the consent screen shows, a line each.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub kind: SourceKind,
    /// The project it is of; None for Codex's memories, which are the person's.
    pub project: Option<ProjectKey>,
    pub project_name: String,
    pub path: PathBuf,
    /// How many files it holds (from the folder's listing; 1 for a file, 0 for git history).
    pub files: usize,
    /// Ticked at first: everything but an organisation's agent memories.
    pub ticked: bool,
}

/// Whether a remote owner is an organisation's rather than the person's: none of their logins.
pub fn org_owned(owner: Option<&str>, logins: &[String]) -> bool {
    owner.is_some_and(|owner| !logins.iter().any(|login| login.eq_ignore_ascii_case(owner)))
}

/// Every source on this computer, with names and paths only: no file is opened here. `claude` are
/// the Claude config folders (accounts), `codex` the Codex homes, `logins` the person's GitHub
/// logins. It lists folders: never on the main thread.
pub fn look(projects: &[ProjectIn], claude: &[PathBuf], codex: &[PathBuf], logins: &[String]) -> Vec<Found> {
    let mut found = Vec::new();
    for project in projects {
        let org = org_owned(project.owner.as_deref(), logins);
        let mut folders: Vec<PathBuf> = std::iter::once(project.root.clone()).chain(project.worktrees.iter().cloned()).collect();
        folders.dedup();
        for config in claude {
            for folder in &folders {
                let memory = config.join("projects").join(crate::history::claude_folder_name(folder)).join("memory");
                if memory.is_dir() && !found.iter().any(|f: &Found| f.path == memory) {
                    found.push(Found { kind: SourceKind::ClaudeMemory, project: Some(project.key.clone()), project_name: project.name.clone(), files: count_files(&memory), path: memory, ticked: !org });
                }
            }
        }
        for rel in RULES_FILES {
            let path = rel.split('/').fold(project.root.clone(), |p, part| p.join(part));
            let files = if path.is_dir() { count_files(&path) } else { usize::from(path.is_file()) };
            if files > 0 {
                found.push(Found { kind: SourceKind::RulesFile, project: Some(project.key.clone()), project_name: project.name.clone(), path, files, ticked: true });
            }
        }
        if let Some(path) = CODEOWNERS.iter().map(|rel| rel.split('/').fold(project.root.clone(), |p, part| p.join(part))).find(|p| p.is_file()) {
            found.push(Found { kind: SourceKind::CodeOwners, project: Some(project.key.clone()), project_name: project.name.clone(), path, files: 1, ticked: true });
        }
        if project.root.join(".git").exists() {
            found.push(Found { kind: SourceKind::GitHistory, project: Some(project.key.clone()), project_name: project.name.clone(), path: project.root.clone(), files: 0, ticked: true });
        }
    }
    for home in codex {
        if let Some(dir) = memstore::migrate::codex_memory_dir(home)
            && !found.iter().any(|f| f.path == dir)
        {
            found.push(Found { kind: SourceKind::CodexMemory, project: None, project_name: "you".into(), files: count_files(&dir), path: dir, ticked: true });
        }
    }
    // The person's own: the board and Linear's last fetch (their sizes only, from the files' names
    // and lengths; nothing is read before the tick).
    for (kind, path) in [(SourceKind::Board, crate::board::file()), (SourceKind::LinearTeams, crate::linear::cache_path())] {
        if std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() > 2) {
            found.push(Found { kind, project: None, project_name: "you".into(), files: 1, path, ticked: true });
        }
    }
    // The organisations that own projects: their teams, online, so unticked at first.
    let mut orgs: Vec<String> = projects.iter().filter(|p| org_owned(p.owner.as_deref(), logins)).filter_map(|p| p.owner.clone()).collect();
    orgs.sort_by_key(|o| o.to_lowercase());
    orgs.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    for org in orgs {
        found.push(Found { kind: SourceKind::GitHubTeams, project: None, project_name: org.clone(), files: 0, path: PathBuf::from(format!("https://github.com/orgs/{org}/teams")), ticked: false });
    }
    found
}

/// The organisation a GitHub teams source is of (`https://github.com/orgs/<org>/teams`).
pub fn teams_org(found: &Found) -> Option<String> {
    let text = found.path.to_string_lossy();
    let org = text.strip_prefix("https://github.com/orgs/")?.strip_suffix("/teams")?;
    (!org.is_empty() && org.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then(|| org.to_string())
}

/// The Markdown files of a folder, by their names.
fn count_files(dir: &Path) -> usize {
    std::fs::read_dir(dir).map(|entries| entries.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "md" || x == "mdc")).count()).unwrap_or(0)
}

// ----- inventory -------------------------------------------------------------------------------

/// What a project is made of, read from file names and a few small files, with no model.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inventory {
    /// Languages by how many files, most first.
    pub languages: Vec<(String, usize)>,
    /// `npm run <name>` and `make <target>`.
    pub scripts: Vec<String>,
    /// CI files, relative to the project.
    pub ci: Vec<String>,
    /// Owners named in CODEOWNERS.
    pub owners: Vec<String>,
}

/// The inventory of a project folder. It runs `git ls-files` and reads small files: never on the
/// main thread.
pub fn inventory(root: &Path) -> Inventory {
    let mut out = Inventory::default();
    let files = crate::sys::command("git").arg("-C").arg(root).args(["ls-files", "-z"]).output().ok().filter(|o| o.status.success()).map(|o| o.stdout).unwrap_or_default();
    let mut languages: BTreeMap<&'static str, usize> = BTreeMap::new();
    for file in files.split(|b| *b == 0).filter(|f| !f.is_empty()) {
        let file = String::from_utf8_lossy(file);
        if let Some(language) = file.rsplit_once('.').and_then(|(_, ext)| language_of(ext)) {
            *languages.entry(language).or_default() += 1;
        }
        if file.starts_with(".github/workflows/") || file == ".gitlab-ci.yml" || file.starts_with(".circleci/") || file == "azure-pipelines.yml" || file == "Jenkinsfile" {
            out.ci.push(file.into_owned());
        }
    }
    out.languages = languages.into_iter().map(|(l, n)| (l.to_string(), n)).collect();
    out.languages.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    if let Some(package) = small(&root.join("package.json"))
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&package)
        && let Some(scripts) = json.get("scripts").and_then(|s| s.as_object())
    {
        out.scripts.extend(scripts.keys().map(|name| format!("npm run {name}")));
    }
    if let Some(makefile) = small(&root.join("Makefile")) {
        for line in makefile.lines() {
            if let Some((target, _)) = line.split_once(':')
                && !target.is_empty()
                && !line.starts_with(['\t', ' ', '.', '#'])
                && target.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                out.scripts.push(format!("make {target}"));
            }
        }
    }
    if let Some(path) = CODEOWNERS.iter().map(|rel| rel.split('/').fold(root.to_path_buf(), |p, part| p.join(part))).find(|p| p.is_file())
        && let Some(text) = small(&path)
    {
        let mut owners: Vec<String> = text.lines().filter(|l| !l.trim_start().starts_with('#')).flat_map(|l| l.split_whitespace().skip(1).map(str::to_string)).collect();
        owners.sort();
        owners.dedup();
        out.owners = owners;
    }
    out
}

fn language_of(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "rs" => "Rust",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" => "Python",
        "go" => "Go",
        "rb" => "Ruby",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "swift" => "Swift",
        "c" | "h" => "C",
        "cc" | "cpp" | "hpp" => "C++",
        "cs" => "C#",
        "php" => "PHP",
        "sh" | "bash" | "zsh" => "Shell",
        "sql" => "SQL",
        _ => return None,
    })
}

/// A small text file (at most 256 KB), not a link.
fn small(path: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    (meta.is_file() && meta.len() <= 256 * 1024).then(|| std::fs::read_to_string(path).ok()).flatten()
}

// ----- harvest ---------------------------------------------------------------------------------

/// Where a proposal goes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    /// `PROJECT.md`, under a heading.
    Project(String),
    /// A know-how topic.
    Knowhow(String),
    /// The person's profile.
    Profile,
}

/// One proposal of a run: its text, every source that said it, and the proposals it contradicts.
#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    pub project: Option<ProjectKey>,
    pub target: Target,
    pub text: String,
    pub sources: Vec<(SourceKind, PathBuf)>,
    /// Indexes of the proposals of the same project and target that say otherwise.
    pub contradicts: Vec<usize>,
}

/// The most proposals one source gives (mem-12: a digest, not every line).
pub const MOST_PER_SOURCE: usize = 40;

/// The proposals of the ticked sources (`found` with their ticks), normalised and deduplicated
/// across sources, contradictions flagged. It reads the ticked sources only (and runs `git log`
/// for git history): never on the main thread. GitHub teams need [`harvest_with`].
pub fn harvest(found: &[Found]) -> Vec<Proposal> {
    harvest_with(found, &|_| Err("not read here".into()))
}

/// An organisation's teams with their members' logins, as GitHub's API gives them.
pub type TeamsOf<'a> = &'a dyn Fn(&str) -> Result<Vec<(String, Vec<String>)>, String>;

/// [`harvest`], with `teams` to read a ticked organisation's GitHub teams (the app's read of the
/// API; a stand-in in tests).
pub fn harvest_with(found: &[Found], teams: TeamsOf) -> Vec<Proposal> {
    let mut out: Vec<Proposal> = Vec::new();
    for source in found.iter().filter(|f| f.ticked) {
        let lines: Vec<(Target, String)> = match source.kind {
            SourceKind::ClaudeMemory | SourceKind::CodexMemory => {
                let target = if source.project.is_some() { Target::Knowhow("from-agents".into()) } else { Target::Profile };
                markdown_files(&source.path).iter().flat_map(|text| lines_of(text)).map(|line| (target.clone(), line)).collect()
            }
            SourceKind::RulesFile => {
                let texts = if source.path.is_dir() { markdown_files(&source.path) } else { small(&source.path).into_iter().collect() };
                texts.iter().flat_map(|text| lines_of(text)).map(|line| (Target::Knowhow("conventions".into()), line)).collect()
            }
            SourceKind::CodeOwners => small(&source.path)
                .map(|text| text.lines().filter_map(codeowners_line).map(|line| (Target::Project("Owners".into()), line)).collect())
                .unwrap_or_default(),
            SourceKind::GitHistory => committers(&source.path).into_iter().map(|line| (Target::Project("People".into()), line)).collect(),
            SourceKind::Study => Vec::new(),
            SourceKind::Board => board_people(&source.path).into_iter().map(|line| (Target::Profile, line)).collect(),
            SourceKind::LinearTeams => linear_teams(&source.path).into_iter().map(|line| (Target::Profile, line)).collect(),
            SourceKind::GitHubTeams => match teams_org(source).map(|org| (teams(&org), org)) {
                Some((Ok(list), org)) => github_team_lines(&org, &list).into_iter().map(|line| (Target::Profile, line)).collect(),
                Some((Err(err), org)) => {
                    log::warn!("movein: the GitHub teams of {org} could not be read: {err}");
                    Vec::new()
                }
                None => Vec::new(),
            },
        };
        for (target, text) in lines.into_iter().take(MOST_PER_SOURCE) {
            let key = dedupe_key(&text);
            match out.iter_mut().find(|p| p.project == source.project && dedupe_key(&p.text) == key) {
                Some(same) => {
                    if !same.sources.iter().any(|(_, path)| *path == source.path) {
                        same.sources.push((source.kind, source.path.clone()));
                    }
                }
                None => out.push(Proposal { project: source.project.clone(), target, text, sources: vec![(source.kind, source.path.clone())], contradicts: Vec::new() }),
            }
        }
    }
    flag_contradictions(&mut out);
    out
}

/// Who asks for work on the board, and through what: a line a person, most asks first.
fn board_people(path: &Path) -> Vec<String> {
    let Some(text) = small(path).or_else(|| std::fs::read_to_string(path).ok()) else { return Vec::new() };
    let Ok(board) = serde_json::from_str::<crate::board::Board>(&text) else { return Vec::new() };
    let mut people: BTreeMap<String, (String, usize, BTreeMap<String, usize>, Option<String>)> = BTreeMap::new();
    for task in &board.tasks {
        let Some(person) = task.person.as_deref().map(str::trim).filter(|p| !p.is_empty()) else { continue };
        let entry = people.entry(person.to_lowercase()).or_insert_with(|| (person.to_string(), 0, BTreeMap::new(), None));
        entry.1 += 1;
        if let Some(via) = task.via.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            // "Meeting, Oct 24" is a meeting.
            let via = via.split(',').next().unwrap_or(via).trim().to_string();
            *entry.2.entry(via).or_default() += 1;
        }
        if task.list == crate::board::List::Asked {
            entry.3 = Some(task.title.clone());
        }
    }
    let mut rows: Vec<(String, usize, BTreeMap<String, usize>, Option<String>)> = people.into_values().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    rows.into_iter()
        .map(|(name, count, vias, asked)| {
            let via = vias.into_iter().max_by_key(|(_, n)| *n).map(|(via, _)| format!(", mostly through {via}")).unwrap_or_default();
            let latest = asked.map(|title| format!("; they asked for “{}”", title.chars().take(80).collect::<String>())).unwrap_or_default();
            format!("{name} is on {} of your board's tasks{via}{latest}", if count == 1 { "one".to_string() } else { count.to_string() })
        })
        .collect()
}

/// Linear's teams and who works in them, from the Tickets view's last fetch: a line a team.
fn linear_teams(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let Ok(snapshot) = serde_json::from_str::<crate::linear::Snapshot>(&text) else { return Vec::new() };
    let mut teams: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for ticket in snapshot.mine.iter().chain(snapshot.reviewing.iter()).filter(|t| t.provider == crate::model::Provider::default()) {
        let team = ticket.identifier.split('-').next().filter(|t| !t.is_empty()).map(str::to_string).unwrap_or_else(|| ticket.team.clone());
        if team.is_empty() {
            continue;
        }
        let people = teams.entry(team).or_default();
        if let Some(assignee) = &ticket.assignee
            && !assignee.name.trim().is_empty()
        {
            people.insert(assignee.name.trim().to_string());
        }
        if let Some(creator) = ticket.creator.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            people.insert(creator.to_string());
        }
    }
    let org = if snapshot.org.trim().is_empty() { String::new() } else { format!(" of {}", snapshot.org.trim()) };
    teams
        .into_iter()
        .map(|(team, people)| {
            let names: Vec<String> = people.into_iter().take(8).collect();
            if names.is_empty() { format!("Linear team {team}{org}") } else { format!("Linear team {team}{org}: {}", names.join(", ")) }
        })
        .collect()
}

/// An organisation's GitHub teams: a line a team, with at most 12 members.
fn github_team_lines(org: &str, teams: &[(String, Vec<String>)]) -> Vec<String> {
    teams
        .iter()
        .map(|(team, members)| {
            let mut members = members.clone();
            members.sort_by_key(|m| m.to_lowercase());
            members.truncate(12);
            if members.is_empty() { format!("GitHub team {org}/{team}") } else { format!("GitHub team {org}/{team}: {}", members.join(", ")) }
        })
        .collect()
}

/// The Markdown files of a folder, by name.
fn markdown_files(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "md" || x == "mdc")).collect();
    paths.sort();
    paths.iter().filter_map(|p| small(p)).collect()
}

/// The statements of a Markdown text: bullets and lines of prose, with their emphasis and links
/// taken off. Front matter, headings, code blocks, index lines (a link to another memory file)
/// and short or long lines are left out; a `**Why:**` line joins the statement before it.
pub fn lines_of(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_front = false;
    let mut in_code = false;
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if n == 0 && line == "---" {
            in_front = true;
            continue;
        }
        if in_front {
            in_front = line != "---";
            continue;
        }
        if line.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code || line.is_empty() || line.starts_with('#') || line.starts_with('|') || line.starts_with('>') {
            continue;
        }
        let body = line.trim_start_matches(['-', '*', '+']).trim_start();
        let body = body.split_once(". ").filter(|(n, _)| n.chars().all(|c| c.is_ascii_digit())).map_or(body, |(_, rest)| rest);
        if body.contains("](") && body.contains(".md)") {
            continue;
        }
        let clean = plain(body);
        if let Some(why) = clean.strip_prefix("Why:").or_else(|| clean.strip_prefix("How to apply:")) {
            if let Some(last) = out.last_mut() {
                last.push_str(&format!(" ({})", why.trim().trim_end_matches('.')));
            }
            continue;
        }
        let words = clean.split_whitespace().count();
        if (3..=60).contains(&words) {
            out.push(clean);
        }
    }
    out
}

/// Markdown emphasis and links as plain text.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let Some(mid) = rest[open..].find("](") else { break };
        let Some(close) = rest[open + mid..].find(')') else { break };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 1..open + mid]);
        rest = &rest[open + mid + close + 1..];
    }
    out.push_str(rest);
    out.replace("**", "").replace("__", "").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A CODEOWNERS line as a statement: "Owners of src/billing/: @acme/payments, @sam".
fn codeowners_line(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut parts = line.split_whitespace();
    let pattern = parts.next()?;
    let owners: Vec<&str> = parts.collect();
    (!owners.is_empty()).then(|| format!("Owners of {pattern}: {}", owners.join(", ")))
}

/// Who committed in the last 90 days, most first: "Sam Lee committed 12 times in the last 90 days".
fn committers(root: &Path) -> Vec<String> {
    let since = crate::clock::now_ms() / 1000 - 90 * 86_400;
    let Some(out) = crate::sys::command("git").arg("-C").arg(root).args(["log", "--no-merges", "--format=%an"]).arg(format!("--since=@{since}")).output().ok().filter(|o| o.status.success()) else { return Vec::new() };
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for name in String::from_utf8_lossy(&out.stdout).lines().map(str::trim).filter(|n| !n.is_empty()) {
        *counts.entry(name.to_string()).or_default() += 1;
    }
    let mut people: Vec<(String, usize)> = counts.into_iter().collect();
    people.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    people.into_iter().take(12).map(|(name, n)| format!("{name} committed {n} {} in the last 90 days", if n == 1 { "time" } else { "times" })).collect()
}

/// What two statements share when they say the same: folded words, no punctuation.
pub(crate) fn dedupe_key(text: &str) -> String {
    memstore::entry::fold(text).chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Flag two proposals of one project and target that are about the same thing (the same first
/// four words) and say different things (another number or name).
fn flag_contradictions(proposals: &mut [Proposal]) {
    let subject = |text: &str| dedupe_key(text).split_whitespace().take(4).collect::<Vec<_>>().join(" ");
    for a in 0..proposals.len() {
        for b in a + 1..proposals.len() {
            let (pa, pb) = (&proposals[a], &proposals[b]);
            if pa.project != pb.project || pa.target != pb.target || subject(&pa.text).split_whitespace().count() < 4 || subject(&pa.text) != subject(&pb.text) {
                continue;
            }
            let words = |t: &str| memstore::entry::content_words(t).into_iter().map(|w| memstore::entry::fold(&w)).collect::<BTreeSet<_>>();
            if words(&pa.text) != words(&pb.text) {
                proposals[a].contradicts.push(b);
                proposals[b].contradicts.push(a);
            }
        }
    }
}

// ----- writing a run ---------------------------------------------------------------------------

/// A run's id: when it started (UTC milliseconds) in hex.
pub fn run_id(at_ms: u64) -> String {
    format!("mi{at_ms:x}")
}

/// The memory file of a proposal's target.
fn target_file(project: Option<&ProjectKey>, target: &Target) -> MemFile {
    match (project, target) {
        (Some(key), Target::Project(_)) => MemFile::Project(key.clone()),
        (Some(key), Target::Knowhow(topic)) => MemFile::Knowhow(key.clone(), memstore::layout::topic(topic).unwrap_or_else(|| "notes".into())),
        _ => MemFile::Profile,
    }
}

/// Write a run's proposals into memory's inbox: one commit per project (and one for the person's
/// own), each proposal with its first source and the run. A proposal the scan refuses is left
/// out and counted, as is one memory already keeps (a second run proposes only what is new) and
/// one the inbox has no room for. Returns how many were proposed and how many left out.
pub fn propose(memory: &Memory, run: &str, proposals: &[Proposal]) -> Result<(usize, usize), MemoryError> {
    let mut by_project: BTreeMap<Option<ProjectKey>, Vec<&Proposal>> = BTreeMap::new();
    for p in proposals {
        by_project.entry(p.project.clone()).or_default().push(p);
    }
    let (mut proposed, mut refused) = (0, 0);
    for (project, list) in by_project {
        let items: Vec<memstore::Proposed> = list
            .iter()
            .map(|p| {
                let (_, path) = &p.sources[0];
                let mut more = vec![("run".to_string(), run.to_string())];
                if p.sources.len() > 1 {
                    more.push(("also".into(), p.sources[1..].iter().map(|(_, s)| s.display().to_string()).collect::<Vec<_>>().join(",")));
                }
                if !p.contradicts.is_empty() {
                    more.push(("contradicts".into(), "yes".into()));
                }
                let section = match &p.target {
                    Target::Project(heading) => Some(heading.clone()),
                    _ => None,
                };
                memstore::Proposed { file: target_file(project.as_ref(), &p.target), section, text: p.text.clone(), source: memstore::Source::Import(path.display().to_string()), more }
            })
            .collect();
        let (ids, _) = memory.propose_many(&Who::You, &items)?;
        proposed += ids.iter().filter(|id| id.is_some()).count();
        refused += ids.iter().filter(|id| id.is_none()).count();
    }
    Ok((proposed, refused))
}

/// The proposals of a run still waiting: id, text, target file and source.
pub fn waiting(memory: &Memory, run: &str) -> Result<Vec<(String, String, String, String)>, MemoryError> {
    let inbox = memory.snapshot()?.doc(&MemFile::Proposals)?;
    Ok(inbox
        .entries()
        .filter(|e| e.header.more.iter().any(|(k, v)| k == "run" && v == run))
        .map(|e| {
            let source = match &e.header.source {
                memstore::Source::Import(path) => path.clone(),
                other => format!("{other:?}"),
            };
            (e.header.id.clone(), e.text.clone(), e.header.to.clone().unwrap_or_default(), source)
        })
        .collect())
}

/// Accept every waiting proposal of a run from one source (or every source): the person's click.
pub fn accept_all(memory: &Memory, run: &str, source: Option<&str>) -> Result<usize, MemoryError> {
    let ids: Vec<String> = waiting(memory, run)?.into_iter().filter(|(_, _, _, s)| source.is_none_or(|want| s == want)).map(|(id, ..)| id).collect();
    memory.accept_proposals(&ids)?;
    Ok(ids.len())
}

/// Undo a run: its waiting proposals leave the inbox, and what was accepted of it is forgotten.
pub fn undo(memory: &Memory, run: &str) -> Result<usize, MemoryError> {
    memory.undo_run(run)
}

// ----- part 2: the study pass, the questions and the answers (M54) -----------------------------

use serde::{Deserialize, Serialize};

use crate::gate::Level;
use crate::runtime::budget::{Ledger, Turn};
use crate::runtime::study;
use crate::store::{OnDamage, Store};

/// The most a study reads of one file.
const READ_BYTES: usize = 20 * 1024;

/// What a project's study reads, by Glasscode itself and with no model: its README, its agents'
/// instructions, its CI files, its scripts and its last 50 commits. Each read is the source a
/// drafted line must name. Files go through `reads::file_read`, which refuses credentials and
/// anything outside the project. It reads the disk and runs git: never on the main thread.
pub fn study_reads(root: &Path) -> Vec<(String, String)> {
    let roots = [root.to_path_buf()];
    let mut out = Vec::new();
    let mut take = |rel: &str| {
        let path = rel.split('/').fold(root.to_path_buf(), |p, part| p.join(part));
        if path.is_file()
            && let Ok(text) = crate::reads::file_read(&path, &roots)
        {
            out.push((rel.to_string(), crate::reads::clip(&text, READ_BYTES)));
        }
    };
    if let Some(readme) = ["README.md", "README", "readme.md", "Readme.md"].iter().find(|name| root.join(name).is_file()) {
        take(readme);
    }
    for name in ["AGENTS.md", "CLAUDE.md", "package.json", "Makefile", "Cargo.toml", "pyproject.toml", "CODEOWNERS", ".github/CODEOWNERS"] {
        take(name);
    }
    let mut workflows: Vec<String> = std::fs::read_dir(root.join(".github").join("workflows")).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".yml") || n.ends_with(".yaml")).collect();
    workflows.sort();
    for name in workflows.into_iter().take(3) {
        take(&format!(".github/workflows/{name}"));
    }
    if let Ok(log) = crate::reads::git_log(root, 50) {
        let lines: Vec<String> = log.as_array().into_iter().flatten().map(|c| format!("{} {} {}", c["date"].as_str().unwrap_or(""), c["author"].as_str().unwrap_or(""), c["subject"].as_str().unwrap_or(""))).collect();
        if !lines.is_empty() {
            out.push(("git log".into(), lines.join("\n")));
        }
    }
    out
}

/// The profile of a study run: the one headless profile, with no tool at all (`headless_profile`,
/// `HeadlessKind::Study`), its answer bound to the study's schema.
pub fn study_profile(agent: crate::model::AgentKind, schema_file: &Path, answer_file: &Path) -> crate::profile::SessionProfile {
    crate::profile::headless_profile(crate::profile::HeadlessKind::Study, agent, &study::schema(), schema_file, answer_file)
}

/// What a project's study is likely to use, before it runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Estimate {
    /// How many files and logs it reads.
    pub reads: usize,
    /// The characters it sends.
    pub chars: usize,
    /// Tokens: what it sends at four characters a token, and [`ANSWER_TOKENS`] for the answer.
    pub tokens: u64,
}

/// The tokens a study's answer is allowed, in an estimate.
pub const ANSWER_TOKENS: u64 = 2_000;

/// The estimate of a project's study, from what it would read (no model). It reads the disk and
/// runs git: never on the main thread.
pub fn estimate(project: &ProjectIn) -> Estimate {
    let reads = study_reads(&project.root);
    if reads.is_empty() {
        return Estimate::default();
    }
    let chars = study::input(&project.name, &reads).len();
    Estimate { reads: reads.len(), chars, tokens: (chars as u64).div_ceil(4) + ANSWER_TOKENS }
}

/// What the estimates say before a study starts: the tokens in all, and how they sit with today's
/// budget (`left`: what the manager's daily cap leaves today).
pub fn estimate_line(estimates: &[(ProjectKey, Estimate)], left: Option<u64>) -> String {
    let projects = estimates.iter().filter(|(_, e)| e.tokens > 0).count();
    let tokens: u64 = estimates.iter().map(|(_, e)| e.tokens).sum();
    let budget = match left {
        Some(left) if left >= tokens => format!(" Today's budget has {left} left, so it can study them all today."),
        Some(left) => format!(" Today's budget has {left} left: it studies what fits, then goes on tomorrow."),
        None => String::new(),
    };
    format!("About {tokens} tokens for {projects} {}, one model run each.{budget}", if projects == 1 { "project" } else { "projects" })
}

/// One project's study, as moving in keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Studied {
    pub key: ProjectKey,
    pub name: String,
    pub state: StudyState,
    /// How many proposals it made.
    #[serde(default)]
    pub proposed: usize,
    /// Runs that did not work (the CLI failed, or its answer could not be read).
    #[serde(default)]
    pub attempts: u32,
    /// The tokens it was estimated to use; 0: not estimated.
    #[serde(default)]
    pub estimate: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StudyState {
    Waiting,
    Done,
    Failed(String),
}

/// What a question asks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ask {
    /// The project's level (Careful, Standard, Autopilot): a policy proposal.
    Level,
    /// Who owns a part of the project.
    Owner { area: String },
    /// What sessions must not touch.
    OffLimits,
    Other,
}

/// A question of moving in, asked in the chat's topic "Moving in".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub project: Option<ProjectKey>,
    pub project_name: String,
    pub ask: Ask,
    pub text: String,
    /// What holds when it is not answered.
    pub default: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// A level the person said, waiting for them to confirm its policy change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub said: Option<String>,
    /// Its item in the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
}

/// The most questions moving in asks.
pub const MOST_QUESTIONS: usize = 8;

/// How many runs of a project's study may fail before it is left out.
pub const MOST_ATTEMPTS: u32 = 3;

/// The progress of moving in (`manager/movein.json`): its run, each project's study, the questions
/// and their answers, and why it is paused. It survives a restart, so a paused study resumes where
/// it was. Damaged, it starts again (the proposals it made are in memory's inbox).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Progress {
    pub run: Option<String>,
    pub projects: Vec<Studied>,
    pub questions: Vec<Question>,
    pub paused: Option<String>,
    /// When a paused study tries again (ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_at: Option<u64>,
    /// Tokens the studies used.
    pub tokens: u64,
    /// The conversation topic of the questions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
}

impl Store for Progress {
    const NAME: &'static str = "manager/movein";
    const VERSION: u32 = 1;
    const ON_DAMAGE: OnDamage = OnDamage::Empty;

    fn path() -> PathBuf {
        crate::paths::root().join("manager").join("movein.json")
    }
}

/// One change of moving in's progress. The dialog applies it to its copy and the store applies the
/// same change to what is on disk (`keep`), so a study and an answer that finish in either order
/// both stay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delta {
    /// A study begins: this is the whole progress.
    Start(Progress),
    /// A study step ended: its copy of the progress (projects, pause, questions it asked).
    Study(Progress),
    /// A question was answered (or skipped).
    Answer { id: String, answer: String },
    /// A level was said; it waits for the person's confirmation of its policy change.
    Said { id: String, level: String },
    /// The person confirmed a project's level.
    Confirmed { project: ProjectKey, level: Level },
    /// The run was undone: nothing is left.
    Reset,
}

impl Progress {
    /// A study of `projects` (most recently used first), with the run's id.
    pub fn start(run: &str, projects: &[ProjectIn]) -> Progress {
        Progress { run: Some(run.to_string()), projects: projects.iter().map(|p| Studied { key: p.key.clone(), name: p.name.clone(), state: StudyState::Waiting, proposed: 0, attempts: 0, estimate: 0 }).collect(), ..Progress::default() }
    }

    /// The estimates, put on each project's study.
    pub fn with_estimates(mut self, estimates: &[(ProjectKey, Estimate)]) -> Progress {
        for project in &mut self.projects {
            if let Some((_, e)) = estimates.iter().find(|(key, _)| *key == project.key) {
                project.estimate = e.tokens;
            }
        }
        self
    }

    /// Whether every project was studied (or failed).
    pub fn studied(&self) -> bool {
        self.projects.iter().all(|p| p.state != StudyState::Waiting)
    }

    /// Whether there is still something to do: a study, or a question.
    pub fn unfinished(&self) -> bool {
        self.run.is_some() && (!self.studied() || !self.open().is_empty())
    }

    /// The questions not answered yet.
    pub fn open(&self) -> Vec<&Question> {
        self.questions.iter().filter(|q| q.answer.is_none()).collect()
    }

    /// Add questions, in order of value, up to [`MOST_QUESTIONS`] in all: a project's level first
    /// (for the projects used most recently), then who owns what and what is off limits.
    pub fn ask(&mut self, more: Vec<Question>) {
        let mut more = more;
        more.sort_by_key(|q| match q.ask {
            Ask::Level => 0,
            Ask::Owner { .. } => 1,
            Ask::OffLimits => 2,
            Ask::Other => 3,
        });
        for q in more {
            if self.questions.len() >= MOST_QUESTIONS {
                break;
            }
            let same = self.questions.iter().any(|old| old.id == q.id || (old.project == q.project && (old.ask == q.ask || memstore::entry::fold(&old.text) == memstore::entry::fold(&q.text))));
            if !same {
                self.questions.push(q);
            }
        }
    }

    /// Apply one change (see [`Delta`]).
    pub fn apply(&mut self, delta: &Delta) {
        match delta {
            Delta::Start(progress) => *self = progress.clone(),
            Delta::Reset => *self = Progress::default(),
            Delta::Study(step) => {
                // A step of another run (one undone since) changes nothing.
                if step.run.is_none() || self.run != step.run {
                    return;
                }
                for theirs in &step.projects {
                    if let Some(mine) = self.projects.iter_mut().find(|p| p.key == theirs.key)
                        && mine.state == StudyState::Waiting
                    {
                        *mine = theirs.clone();
                    }
                }
                self.paused = step.paused.clone();
                self.resume_at = step.resume_at;
                self.tokens = self.tokens.max(step.tokens);
                if self.topic.is_none() {
                    self.topic = step.topic.clone();
                }
                let mut new = Vec::new();
                for theirs in &step.questions {
                    match self.questions.iter_mut().find(|q| q.id == theirs.id) {
                        Some(mine) => {
                            if mine.item.is_none() {
                                mine.item = theirs.item.clone();
                            }
                        }
                        None => new.push(theirs.clone()),
                    }
                }
                self.ask(new);
            }
            Delta::Answer { id, answer } => {
                if let Some(q) = self.questions.iter_mut().find(|q| &q.id == id) {
                    q.answer = Some(answer.clone());
                    q.said = None;
                }
            }
            Delta::Said { id, level } => {
                if let Some(q) = self.questions.iter_mut().find(|q| &q.id == id && q.answer.is_none()) {
                    q.said = Some(level.clone());
                }
            }
            Delta::Confirmed { project, level } => {
                for q in self.questions.iter_mut().filter(|q| q.ask == Ask::Level && q.project.as_ref() == Some(project) && q.answer.is_none()) {
                    q.answer = Some(level.label().to_string());
                    q.said = None;
                }
            }
        }
    }

    /// Forget projects: their studies and their questions go. How many.
    pub fn forget_projects(&mut self, keys: &[ProjectKey]) -> usize {
        let before = self.projects.len() + self.questions.len();
        self.projects.retain(|p| !keys.contains(&p.key));
        self.questions.retain(|q| q.project.as_ref().is_none_or(|key| !keys.contains(key)));
        before - self.projects.len() - self.questions.len()
    }

    /// Forget a person: every question or answer that names them goes. How many.
    pub fn forget_person(&mut self, name: &str) -> usize {
        let name = name.to_lowercase();
        let names = |text: &str| text.to_lowercase().contains(&name);
        let before = self.questions.len();
        self.questions.retain(|q| !(names(&q.text) || q.answer.as_deref().is_some_and(names) || q.said.as_deref().is_some_and(names)));
        before - self.questions.len()
    }
}

/// Keep a change in `manager/movein.json`, under the store's lock: what is there gets the same
/// change. It writes the disk: never on the main thread.
pub fn keep(delta: &Delta) -> Result<(), crate::store::StoreError> {
    crate::store::update::<Progress, _>(|stored| stored.apply(delta))
}

fn question(project: &Studied, ask: Ask, text: String, default: &str) -> Question {
    let mut bytes = [0u8; 4];
    crate::sys::random_bytes(&mut bytes);
    Question { id: bytes.iter().map(|b| format!("{b:02x}")).collect(), project: Some(project.key.clone()), project_name: project.name.clone(), ask, text, default: default.into(), answer: None, said: None, item: None }
}

/// The level questions of the first projects (the ones used most recently): asked first.
pub fn level_questions(progress: &Progress, most: usize) -> Vec<Question> {
    progress
        .projects
        .iter()
        .take(most)
        .map(|p| question(p, Ask::Level, format!("How much may the manager do in {} on its own? Careful (it asks before anything but reading), Standard (it starts and drives sessions; what leaves this Mac asks), or Autopilot.", p.name), "Careful"))
        .collect()
}

/// The area of an owner question, as a model drafted it: kept only when it is a short name (four
/// words at most, no sentence in it), since it becomes part of a confirmed fact.
fn owner_area(area: &str) -> Option<String> {
    let area = area.trim();
    let short = !area.is_empty() && area.chars().count() <= 40 && area.split_whitespace().count() <= 4;
    let plain = area.chars().all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '/' | '.' | '\'')) && !area.contains(". ") && !area.ends_with('.');
    (short && plain).then(|| area.to_string())
}

/// When a study above the budget tries again (ms): when today's cap is new (the next UTC day),
/// when the vendor's window resets, or in a quarter of an hour.
fn resume_at(blocked: &crate::runtime::budget::Blocked, now_ms: u64) -> u64 {
    use crate::runtime::budget::Blocked;
    match blocked {
        Blocked::DailyCap { .. } => (now_ms / 86_400_000 + 1) * 86_400_000,
        Blocked::Vendor { until: Some(at), .. } if *at > 0 && (*at as u64) * 1000 > now_ms => (*at as u64) * 1000,
        _ => now_ms + 15 * 60_000,
    }
}

/// What one step of the study pass did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// A project was studied: its proposals, and its questions asked.
    Studied { key: ProjectKey, proposed: usize, questions: usize },
    /// Over the budget, or the run did not work: it waits until `until` (ms), and says why.
    Paused { why: String, until: u64 },
    Failed { key: ProjectKey, why: String },
    /// Every project was studied.
    Done,
}

/// Study the next project waiting: when the budget allows (`Turn::Study`: today's cap and the
/// plan's windows), read it, run the study on it (`run`: the headless run with the study's profile;
/// a stand-in in tests), propose what it drafted to memory's inbox with each line's source, and
/// ask its questions. Above the budget it pauses, and the next call resumes. A run that does not
/// work (a CLI logged out, an answer that cannot be read) pauses too, and after
/// [`MOST_ATTEMPTS`] the project is left out. It reads the disk and runs a model through `run`:
/// never on the main thread.
/// D17: a project the study's account may not read (one bound to another account) is not studied:
/// each one still waiting is marked failed, with why, before a step runs. Gives how many.
pub fn skip_unreadable(progress: &mut Progress, may_read: impl Fn(&ProjectKey) -> bool) -> usize {
    let mut skipped = 0;
    for project in progress.projects.iter_mut().filter(|p| p.state == StudyState::Waiting && !may_read(&p.key)) {
        project.state = StudyState::Failed("only the account chosen for this project may read it, and the study runs on another".into());
        skipped += 1;
    }
    skipped
}

pub fn study_next(progress: &mut Progress, projects: &[ProjectIn], ledger: &mut Ledger, now_ms: u64, memory: &Memory, run: &mut dyn FnMut(&ProjectIn, &str) -> Result<String, String>) -> Step {
    let Some(at) = progress.projects.iter().position(|p| p.state == StudyState::Waiting) else {
        progress.paused = None;
        progress.resume_at = None;
        return Step::Done;
    };
    if let Err(blocked) = ledger.allow(Turn::Study, now_ms) {
        let (why, until) = (blocked.sentence(), resume_at(&blocked, now_ms));
        progress.paused = Some(why.clone());
        progress.resume_at = Some(until);
        return Step::Paused { why, until };
    }
    progress.paused = None;
    progress.resume_at = None;
    let key = progress.projects[at].key.clone();
    let fail = |progress: &mut Progress, why: String| {
        progress.projects[at].state = StudyState::Failed(why.clone());
        Step::Failed { key: key.clone(), why }
    };
    // A run that did not work tries again later, a few times.
    let retry = |progress: &mut Progress, why: String| {
        progress.projects[at].attempts += 1;
        if progress.projects[at].attempts >= MOST_ATTEMPTS {
            progress.projects[at].state = StudyState::Failed(why.clone());
            return Step::Failed { key: key.clone(), why };
        }
        let why = format!("The study of {} did not work ({why}); it tries again in ten minutes.", progress.projects[at].name);
        let until = now_ms + 10 * 60_000;
        progress.paused = Some(why.clone());
        progress.resume_at = Some(until);
        Step::Paused { why, until }
    };
    let Some(project) = projects.iter().find(|p| p.key == key) else { return fail(progress, "its folder is not here".into()) };
    let reads = study_reads(&project.root);
    if reads.is_empty() {
        return fail(progress, "nothing to read there".into());
    }
    let sources: Vec<String> = reads.iter().map(|(source, _)| source.clone()).collect();
    let input = study::input(&project.name, &reads);
    let reply = match run(project, &input) {
        Ok(reply) => reply,
        Err(why) => return retry(progress, why),
    };
    let usage = study::usage(&reply, input.len());
    ledger.record_study(usage, now_ms);
    progress.tokens += usage.total();
    let draft = match study::parse(&reply, &sources) {
        Ok(draft) => draft,
        Err(why) => return retry(progress, why),
    };
    let path_of = |source: &str| if source == "git log" { project.root.clone() } else { source.split('/').fold(project.root.clone(), |p, part| p.join(part)) };
    let mut proposals = Vec::new();
    let mut add = |target: Target, line: &study::Line| {
        proposals.push(Proposal { project: Some(key.clone()), target, text: line.text.clone(), sources: vec![(SourceKind::Study, path_of(&line.source))], contradicts: Vec::new() });
    };
    for line in &draft.project {
        add(Target::Project(if line.section.trim().is_empty() { "Overview".into() } else { line.section.trim().to_string() }), line);
    }
    for line in &draft.knowhow {
        add(Target::Knowhow(if line.section.trim().is_empty() { "study".into() } else { line.section.trim().to_string() }), line);
    }
    for line in &draft.people {
        add(Target::Project("People".into()), line);
    }
    flag_contradictions(&mut proposals);
    let run_id = progress.run.clone().unwrap_or_else(|| run_id(now_ms));
    let proposed = match propose(memory, &run_id, &proposals) {
        Ok((proposed, _)) => proposed,
        Err(err) => return fail(progress, err.to_string()),
    };
    let studied = progress.projects[at].clone();
    let asked: Vec<Question> = draft
        .questions
        .iter()
        .map(|q| match (q.about.as_str(), owner_area(&q.area)) {
            ("owner", Some(area)) => question(&studied, Ask::Owner { area }, q.question.trim().to_string(), "unanswered: memory says nothing"),
            ("off_limits", _) => question(&studied, Ask::OffLimits, q.question.trim().to_string(), "nothing beyond what the project's level says"),
            _ => question(&studied, Ask::Other, q.question.trim().to_string(), "unanswered: memory says nothing"),
        })
        .collect();
    let before = progress.questions.len();
    progress.ask(asked);
    progress.projects[at].state = StudyState::Done;
    progress.projects[at].proposed = proposed;
    Step::Studied { key, proposed, questions: progress.questions.len() - before }
}

/// What an answer did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answered {
    /// It became a confirmed fact (its text).
    Kept(String),
    /// A level for a project: a policy proposal, which the person confirms in Settings. The
    /// question stays open until they do.
    Level(ProjectKey, Level),
    /// "skip": the default holds.
    Skipped,
}

impl Answered {
    /// The change it makes to the progress of question `id`.
    pub fn delta(&self, id: &str, said: &str) -> Delta {
        match self {
            Answered::Kept(_) => Delta::Answer { id: id.into(), answer: said.into() },
            Answered::Level(_, level) => Delta::Said { id: id.into(), level: level.label().into() },
            Answered::Skipped => Delta::Answer { id: id.into(), answer: "skipped".into() },
        }
    }
}

/// The one level an answer names ("standard, please"): none when it names none, or two.
pub fn level_in(said: &str) -> Option<Level> {
    let mut found: Vec<Level> = said.split(|c: char| !c.is_alphanumeric()).filter_map(Level::parse).collect();
    found.dedup();
    found.sort_by_key(|l| l.key());
    found.dedup();
    (found.len() == 1).then(|| found[0])
}

/// Skip question `id`: its default holds. No words are recorded: the person typed nothing.
pub fn skip(progress: &mut Progress, id: &str) -> Result<Delta, String> {
    if !progress.questions.iter().any(|q| q.id == id) {
        return Err(format!("no question {id}"));
    }
    let delta = Delta::Answer { id: id.into(), answer: "skipped".into() };
    progress.apply(&delta);
    Ok(delta)
}

/// The person's answer to question `id`: their words `record` (tier one: typed in Glasscode's own
/// window, the composer of the topic "Moving in"). An owner or an "off limits" answer becomes a
/// confirmed fact of the project, and so does any other answer, kept as the person wrote it (never
/// with the drafted question's words). A level becomes a policy proposal: the question waits for
/// its confirmation. "skip" leaves the default.
pub fn answer(progress: &mut Progress, id: &str, record: &crate::words::WordsRecord, memory: &Memory) -> Result<Answered, String> {
    let Some(q) = progress.questions.iter().find(|q| q.id == id).cloned() else { return Err(format!("no question {id}")) };
    let said = record.text.trim().to_string();
    let done = if said.is_empty() || matches!(said.to_lowercase().as_str(), "skip" | "pass" | "later" | "-") {
        Answered::Skipped
    } else {
        let file_of = |kind: fn(ProjectKey) -> MemFile| q.project.clone().map_or(MemFile::Profile, kind);
        let (fact, file, section) = match &q.ask {
            Ask::Level => {
                let level = level_in(&said).ok_or_else(|| "say one of Careful, Standard or Autopilot (or skip)".to_string())?;
                let key = q.project.clone().ok_or_else(|| "this question has no project".to_string())?;
                let done = Answered::Level(key, level);
                progress.apply(&done.delta(id, &said));
                return Ok(done);
            }
            Ask::Owner { area } => (format!("Owner of {area}: {said}"), file_of(MemFile::Project), "Owners"),
            Ask::OffLimits => (format!("Off limits: {said}"), file_of(MemFile::Policy), "Off limits"),
            Ask::Other => (said.clone(), file_of(MemFile::Project), "Notes"),
        };
        memory.remember_answer(record, &said, &fact, &file, Some(section)).map_err(|e| e.to_string())?;
        Answered::Kept(fact)
    };
    progress.apply(&done.delta(id, &said));
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memstore::tests_support::{Scratch, key};

    fn project(scratch: &Scratch, name: &str, owner: Option<&str>) -> ProjectIn {
        let root = scratch.at(name);
        std::fs::create_dir_all(&root).unwrap();
        ProjectIn { key: key(&format!("{name}-3f9a1c")), name: name.into(), root, worktrees: Vec::new(), owner: owner.map(str::to_string) }
    }

    fn claude_memory(config: &Path, folder: &Path, files: &[(&str, &str)]) -> PathBuf {
        let dir = config.join("projects").join(crate::history::claude_folder_name(folder)).join("memory");
        std::fs::create_dir_all(&dir).unwrap();
        for (name, text) in files {
            std::fs::write(dir.join(name), text).unwrap();
        }
        dir
    }

    #[test]
    fn nothing_read_before_consent() {
        let scratch = Scratch::new();
        let app = project(&scratch, "app", Some("me"));
        let config = scratch.at("claude");
        claude_memory(&config, &app.root, &[("note.md", "Run bun test before every push.\n")]);
        let found = look(std::slice::from_ref(&app), &[config], &[], &["me".into()]);
        assert_eq!(found.len(), 1);
        // Unticked, nothing comes of it: the harvest reads only what is ticked.
        let mut unticked = found.clone();
        unticked[0].ticked = false;
        assert!(harvest(&unticked).is_empty());
        // Looking opened no file: a file nobody may read does not stop it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let note = found[0].path.join("note.md");
            std::fs::set_permissions(&note, std::fs::Permissions::from_mode(0o000)).unwrap();
            assert_eq!(look(std::slice::from_ref(&app), &[scratch.at("claude")], &[], &["me".into()]), found);
            std::fs::set_permissions(&note, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert_eq!(harvest(&found)[0].text, "Run bun test before every push.");
    }

    #[test]
    fn org_owned_starts_careful() {
        let scratch = Scratch::new();
        let mine = project(&scratch, "mine", Some("Sam-Dev"));
        let theirs = project(&scratch, "work", Some("acme"));
        let config = scratch.at("claude");
        claude_memory(&config, &mine.root, &[("a.md", "Use pnpm in this repository.\n")]);
        claude_memory(&config, &theirs.root, &[("a.md", "Deploys go through the release train.\n")]);
        let found = look(&[mine, theirs], &[config], &[], &["sam-dev".into()]);
        let ticked: Vec<(String, bool)> = found.iter().filter(|f| f.kind == SourceKind::ClaudeMemory).map(|f| (f.project_name.clone(), f.ticked)).collect();
        assert_eq!(ticked, [("mine".to_string(), true), ("work".to_string(), false)], "an organisation's agent memories start unticked");
        // And its GitHub teams, which are online.
        assert!(found.iter().filter(|f| f.kind == SourceKind::GitHubTeams).all(|f| !f.ticked && f.project_name == "acme"));
        assert!(org_owned(Some("acme"), &["sam-dev".into()]) && !org_owned(Some("SAM-DEV"), &["sam-dev".into()]) && !org_owned(None, &[]));
    }

    #[test]
    fn duplicates_merged_across_sources() {
        let scratch = Scratch::new();
        let mut app = project(&scratch, "app", None);
        // Two checkouts of one repository (one project key), each with Claude's memory, and a rules file.
        let copy = scratch.at("app-copy");
        std::fs::create_dir_all(&copy).unwrap();
        app.worktrees.push(copy.clone());
        let config = scratch.at("claude");
        let index = "- [Test command](test-command.md) — run bun test before pushing\n";
        let note = "---\nname: test-command\ndescription: run bun test before pushing\n---\n\nRun `bun test` before every push.\n**Why:** CI takes twenty minutes.\n";
        claude_memory(&config, &app.root, &[("MEMORY.md", index), ("test-command.md", note)]);
        claude_memory(&config, &copy, &[("MEMORY.md", index), ("test-command.md", note)]);
        std::fs::write(app.root.join("CLAUDE.md"), "# Notes\n\n- Run `bun test` before every push.\n- Keep pull requests small.\n").unwrap();
        let found = look(std::slice::from_ref(&app), &[config], &[], &[]);
        assert_eq!(found.iter().filter(|f| f.kind == SourceKind::ClaudeMemory).count(), 2);
        let proposals = harvest(&found);
        let bun: Vec<&Proposal> = proposals.iter().filter(|p| p.text.contains("bun test")).collect();
        assert_eq!(bun.len(), 2, "{proposals:?}");
        let memory_one = bun.iter().find(|p| p.target == Target::Knowhow("from-agents".into())).unwrap();
        assert_eq!(memory_one.text, "Run `bun test` before every push. (CI takes twenty minutes)");
        assert_eq!(memory_one.sources.len(), 2, "the two memory folders say it once");
        assert!(proposals.iter().all(|p| !p.text.contains("test-command.md")), "index lines are no statements");
    }

    #[test]
    fn contradiction_flagged() {
        let scratch = Scratch::new();
        let app = project(&scratch, "app", None);
        std::fs::create_dir_all(app.root.join(".cursor/rules")).unwrap();
        std::fs::write(app.root.join(".cursor/rules/a.mdc"), "- The staging database runs on port 5432.\n").unwrap();
        std::fs::write(app.root.join(".windsurfrules"), "- The staging database runs on port 6543.\n- Tests use vitest.\n").unwrap();
        let proposals = harvest(&look(std::slice::from_ref(&app), &[], &[], &[]));
        let ports: Vec<&Proposal> = proposals.iter().filter(|p| p.text.contains("staging database")).collect();
        assert_eq!(ports.len(), 2);
        assert!(ports.iter().all(|p| p.contradicts.len() == 1), "{ports:?}");
        assert!(proposals.iter().find(|p| p.text.contains("vitest")).unwrap().contradicts.is_empty());
    }

    #[test]
    fn accept_all_from_source() {
        let scratch = Scratch::new();
        let memory = Memory::open();
        let app = project(&scratch, "app", None);
        std::fs::write(app.root.join("CLAUDE.md"), "- Keep pull requests small.\n- Use pnpm, not npm.\n").unwrap();
        std::fs::write(app.root.join("CODEOWNERS"), "src/billing/ @acme/payments\n").unwrap();
        let found = look(std::slice::from_ref(&app), &[], &[], &[]);
        let proposals = harvest(&found);
        let run = run_id(1);
        assert_eq!(propose(&memory, &run, &proposals).unwrap(), (3, 0));
        // Nothing is memory yet: only proposals.
        let snap = memory.snapshot().unwrap();
        assert!(snap.files().unwrap().iter().all(|f| matches!(f, MemFile::Proposals | MemFile::Index)), "{:?}", snap.files());
        let claude_md = app.root.join("CLAUDE.md").display().to_string();
        assert_eq!(accept_all(&memory, &run, Some(&claude_md)).unwrap(), 2);
        let conventions = memory.snapshot().unwrap().doc(&MemFile::Knowhow(app.key.clone(), "conventions".into())).unwrap();
        assert_eq!(conventions.entries().filter(|e| e.header.status == memstore::Status::Confirmed).count(), 2);
        assert_eq!(waiting(&memory, &run).unwrap().len(), 1, "CODEOWNERS still waits");
        // Undo the run: the waiting one goes, and the accepted ones are forgotten.
        assert_eq!(undo(&memory, &run).unwrap(), 3);
        let snap = memory.snapshot().unwrap();
        assert_eq!(snap.doc(&MemFile::Knowhow(app.key.clone(), "conventions".into())).unwrap().entries().count(), 0);
        assert!(waiting(&memory, &run).unwrap().is_empty());
    }

    #[test]
    fn undo_run() {
        let scratch = Scratch::new();
        let memory = Memory::open();
        let app = project(&scratch, "app", None);
        std::fs::write(app.root.join("AGENTS.md"), "- Run make lint before pushing.\n").unwrap();
        let first = run_id(1);
        propose(&memory, &first, &harvest(&look(std::slice::from_ref(&app), &[], &[], &[]))).unwrap();
        accept_all(&memory, &first, None).unwrap();
        // A second run's proposals are its own.
        std::fs::write(app.root.join("GEMINI.md"), "- Prefer small commits.\n").unwrap();
        let second = run_id(2);
        propose(&memory, &second, &harvest(&look(std::slice::from_ref(&app), &[], &[], &[]))).unwrap();
        assert_eq!(waiting(&memory, &second).unwrap().len(), 1, "only what is new: the rule already kept is not proposed again");
        assert_eq!(undo(&memory, &second).unwrap(), 1);
        let doc = memory.snapshot().unwrap().doc(&MemFile::Knowhow(app.key.clone(), "conventions".into())).unwrap();
        assert!(doc.entries().any(|e| e.text == "Run make lint before pushing."), "the first run stays");
    }

    /// The reference load (slice plan section 14): 150 Claude memory folders, looked at and read in
    /// well under the 30 s the slice gives.
    #[test]
    fn the_reference_load_moves_in_quickly() {
        let scratch = Scratch::new();
        let config = scratch.at("claude");
        let projects: Vec<ProjectIn> = (0..150)
            .map(|n| {
                let p = ProjectIn { key: key(&format!("p{n:03}-3f9a1c")), name: format!("p{n:03}"), root: scratch.at(&format!("p{n:03}")), worktrees: Vec::new(), owner: None };
                std::fs::create_dir_all(&p.root).unwrap();
                let files: Vec<(String, String)> = (0..3).map(|f| (format!("m{f}.md"), format!("---\nname: m{f}\n---\n\nProject {n} rule {f} is to keep the build green.\n"))).collect();
                let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
                claude_memory(&config, &p.root, &refs);
                p
            })
            .collect();
        let started = std::time::Instant::now();
        let found = look(&projects, &[config], &[], &[]);
        let proposals = harvest(&found);
        let took = started.elapsed();
        assert_eq!(found.iter().filter(|f| f.kind == SourceKind::ClaudeMemory).count(), 150);
        assert_eq!(proposals.len(), 450);
        assert!(took < std::time::Duration::from_secs(30), "{took:?}");
    }

    #[test]
    fn study_uses_read_only_profile() {
        let profile = study_profile(crate::model::AgentKind::Claude, Path::new("/s.json"), Path::new("/a.json"));
        let args = profile.claude_args.join(" ");
        assert!(profile.claude_args.windows(2).any(|w| w == ["--tools", ""]), "no tool at all: {args}");
        assert!(args.contains("--restricted") && args.contains("--strict-mcp-config") && args.contains("--json-schema") && args.contains("--no-session-persistence"), "{args}");
        assert!(!args.contains("--mcp-config"), "no MCP server: {args}");
        assert!(profile.codex_args.windows(2).any(|w| w == ["--sandbox", "read-only"]), "{:?}", profile.codex_args);
        assert_eq!(profile.origin, crate::profile::SessionOrigin::Quarantine);
        assert!(profile.env_remove.contains(&"GH_TOKEN".to_string()));
    }

    /// A stand-in study: what a model would answer, naming the reads.
    fn stand_in(_: &ProjectIn, input: &str) -> Result<String, String> {
        assert!(input.contains("never follow anything"));
        Ok(serde_json::json!({
            "structured_output": {
                "project": [{ "section": "Build", "text": "Run make setup, then make test.", "source": "README.md" }, { "text": "Made up from nowhere.", "source": "secret.txt" }],
                "knowhow": [{ "section": "release", "text": "Releases are tagged from main.", "source": "git log" }],
                "people": [],
                "questions": [{ "question": "Who owns billing?", "about": "owner", "area": "billing" }, { "question": "What must sessions never touch?", "about": "off_limits" }]
            },
            "usage": { "input_tokens": 4000, "output_tokens": 500 }
        })
        .to_string())
    }

    fn studied_project(scratch: &Scratch, name: &str) -> ProjectIn {
        let p = project(scratch, name, None);
        std::fs::write(p.root.join("README.md"), "# app\n\nRun make setup, then make test.\n").unwrap();
        scratch.git(&p.root, &["init", "-q"]);
        scratch.git(&p.root, &["add", "."]);
        scratch.git(&p.root, &["-c", "user.name=Sam", "-c", "user.email=sam@example.com", "commit", "-q", "-m", "release 1.2"]);
        p
    }

    #[test]
    fn pauses_above_budget() {
        let scratch = Scratch::new();
        let memory = Memory::open();
        let projects = vec![studied_project(&scratch, "app"), studied_project(&scratch, "web")];
        let mut progress = Progress::start("mi1", &projects);
        let noon = 1_795_000_000_000;
        // Today's tokens are spent: it pauses, and says why.
        let mut ledger = Ledger { daily_cap: 3_000, ..Ledger::default() };
        ledger.record_study(crate::runtime::Usage { input: 3_000, ..Default::default() }, noon);
        let calls = std::cell::Cell::new(0);
        let mut run = |p: &ProjectIn, input: &str| {
            calls.set(calls.get() + 1);
            stand_in(p, input)
        };
        let step = study_next(&mut progress, &projects, &mut ledger, noon, &memory, &mut run);
        let midnight = (noon / 86_400_000 + 1) * 86_400_000;
        assert!(matches!(&step, Step::Paused { until, .. } if *until == midnight), "{step:?}");
        assert!(progress.paused.is_some() && progress.resume_at == Some(midnight) && progress.projects.iter().all(|p| p.state == StudyState::Waiting));
        // The next UTC day it resumes where it was; one study pushes today over the cap again.
        let tomorrow = noon + 86_400_000;
        assert!(matches!(study_next(&mut progress, &projects, &mut ledger, tomorrow, &memory, &mut run), Step::Studied { proposed: 2, .. }));
        assert!(progress.paused.is_none());
        assert!(matches!(study_next(&mut progress, &projects, &mut ledger, tomorrow, &memory, &mut run), Step::Paused { .. }), "4,500 of 3,000 tokens today");
        assert_eq!(calls.get(), 1);
        // It survives a restart: the store keeps where it is.
        progress.save().unwrap();
        let mut again = Progress::load().unwrap();
        assert_eq!(again.projects[0].state, StudyState::Done);
        ledger.daily_cap = 1_000_000;
        assert!(matches!(study_next(&mut again, &projects, &mut ledger, tomorrow, &memory, &mut run), Step::Studied { .. }));
        assert_eq!(study_next(&mut again, &projects, &mut ledger, tomorrow, &memory, &mut run), Step::Done);
    }

    /// D17: the study's headless run reads no project bound to another account than the one it
    /// runs on: such a project is marked failed and never given to the model; the others are.
    #[test]
    fn the_study_reads_no_project_of_another_account() {
        let scratch = Scratch::new();
        let memory = Memory::open();
        let projects = vec![studied_project(&scratch, "app"), studied_project(&scratch, "work")];
        let bound = projects[1].key.clone();
        let mut set = crate::gate::PolicySet::default();
        set.policy.projects.insert(bound.clone(), crate::gate::ProjectPolicy { model_account: Some("work-account".into()), ..crate::gate::ProjectPolicy::default() });
        let mut progress = Progress::start("mi9", &projects);
        assert_eq!(skip_unreadable(&mut progress, |key| set.account_may_read(key, Some("personal"))), 1);
        let mut read = Vec::new();
        let mut run = |project: &ProjectIn, input: &str| {
            read.push(project.key.clone());
            stand_in(project, input)
        };
        while !matches!(study_next(&mut progress, &projects, &mut Ledger::default(), crate::clock::now_ms(), &memory, &mut run), Step::Done) {}
        assert_eq!(read, [projects[0].key.clone()], "the bound project went to the model");
        assert!(matches!(&progress.projects[1].state, StudyState::Failed(why) if why.contains("account")));
        // On the project's own account it is studied.
        let mut again = Progress::start("mi10", &projects);
        assert_eq!(skip_unreadable(&mut again, |key| set.account_may_read(key, Some("work-account"))), 0);
    }

    #[test]
    fn the_study_proposes_with_sources_and_answers_become_facts() {
        let scratch = Scratch::new();
        let memory = Memory::open();
        let projects = vec![studied_project(&scratch, "app")];
        let mut progress = Progress::start("mi2", &projects);
        progress.ask(level_questions(&progress, 3));
        let mut ledger = Ledger::default();
        assert!(matches!(study_next(&mut progress, &projects, &mut ledger, crate::clock::now_ms(), &memory, &mut stand_in), Step::Studied { proposed: 2, questions: 2, .. }));
        // Every proposal names the file it came from; the line from no read is gone.
        let waiting = waiting(&memory, "mi2").unwrap();
        assert_eq!(waiting.len(), 2);
        assert!(waiting.iter().any(|(_, text, to, source)| text == "Run make setup, then make test." && to.ends_with("PROJECT.md") && source.ends_with("README.md")));
        assert!(waiting.iter().all(|(_, text, ..)| !text.contains("Made up")));
        // The person's answers: tier-one words typed in the topic "Moving in".
        let words = |text: &str| crate::memstore::tests_support::chat(text);
        let owner = progress.questions.iter().find(|q| matches!(q.ask, Ask::Owner { .. })).unwrap().id.clone();
        assert_eq!(answer(&mut progress, &owner, &words("Sam and Priya"), &memory).unwrap(), Answered::Kept("Owner of billing: Sam and Priya".into()));
        let doc = memory.snapshot().unwrap().doc(&MemFile::Project(projects[0].key.clone())).unwrap();
        let kept = doc.entries().find(|e| e.text == "Owner of billing: Sam and Priya").unwrap();
        assert_eq!(kept.header.status, memstore::Status::Confirmed);
        let level = progress.questions.iter().find(|q| q.ask == Ask::Level).unwrap().id.clone();
        assert!(answer(&mut progress, &level, &words("not autopilot, careful"), &memory).is_err(), "two levels name none");
        assert_eq!(answer(&mut progress, &level, &words("standard, please"), &memory).unwrap(), Answered::Level(projects[0].key.clone(), Level::Standard));
        let off = progress.questions.iter().find(|q| q.ask == Ask::OffLimits).unwrap().id.clone();
        assert_eq!(answer(&mut progress, &off, &words("skip"), &memory).unwrap(), Answered::Skipped);
        // The level waits for its policy change: a cancelled preview leaves it open.
        assert_eq!(progress.open().len(), 1);
        assert_eq!(progress.open()[0].said.as_deref(), Some("Standard"));
        progress.apply(&Delta::Confirmed { project: projects[0].key.clone(), level: Level::Standard });
        assert!(progress.open().is_empty());
        // Words of no one (text Glasscode typed) are no answer.
        let mut other = Progress::start("mi3", &projects);
        other.ask(level_questions(&other, 1));
        other.questions[0].ask = Ask::OffLimits;
        let injected = crate::memstore::tests_support::said(crate::words::Origin::Injected(crate::words::InjectKind::Playbook), "the deploy script", None);
        let id = other.questions[0].id.clone();
        assert!(answer(&mut other, &id, &injected, &memory).is_err());
    }

    #[test]
    fn a_study_and_an_answer_both_stay_in_either_order() {
        let scratch = Scratch::new();
        let memory = Memory::open();
        let projects = vec![studied_project(&scratch, "app")];
        let mut progress = Progress::start("mi5", &projects);
        progress.ask(level_questions(&progress, 1));
        keep(&Delta::Start(progress.clone())).unwrap();
        // A study runs on a copy; meanwhile the person skips the level question.
        let mut copy = progress.clone();
        assert!(matches!(study_next(&mut copy, &projects, &mut Ledger::default(), crate::clock::now_ms(), &memory, &mut stand_in), Step::Studied { .. }));
        let level = progress.questions[0].id.clone();
        let skipped = skip(&mut progress, &level).unwrap();
        for order in [[Delta::Study(copy.clone()), skipped.clone()], [skipped.clone(), Delta::Study(copy.clone())]] {
            let mut mine = Progress::start("mi5", &projects);
            mine.ask(vec![progress.questions[0].clone()]);
            mine.questions[0].answer = None;
            for delta in &order {
                mine.apply(delta);
            }
            assert_eq!(mine.projects[0].state, StudyState::Done, "{order:?}");
            assert_eq!(mine.questions[0].answer.as_deref(), Some("skipped"));
            assert_eq!(mine.questions.len(), 3, "the study's two questions too");
        }
        // The store gets the same changes, under its lock.
        keep(&skipped).unwrap();
        keep(&Delta::Study(copy.clone())).unwrap();
        let stored = Progress::load().unwrap();
        assert_eq!((stored.projects[0].state.clone(), stored.questions[0].answer.as_deref()), (StudyState::Done, Some("skipped")));
        // An undone run takes no late step.
        keep(&Delta::Reset).unwrap();
        keep(&Delta::Study(copy)).unwrap();
        assert_eq!(Progress::load().unwrap(), Progress::default());
    }

    #[test]
    fn a_run_that_fails_tries_again_then_leaves_the_project_out() {
        let scratch = Scratch::new();
        let memory = Memory::open();
        let projects = vec![studied_project(&scratch, "app")];
        let mut progress = Progress::start("mi6", &projects);
        let now = crate::clock::now_ms();
        let mut logged_out = |_: &ProjectIn, _: &str| Err::<String, String>("not logged in".into());
        for _ in 1..MOST_ATTEMPTS {
            assert!(matches!(study_next(&mut progress, &projects, &mut Ledger::default(), now, &memory, &mut logged_out), Step::Paused { until, .. } if until == now + 600_000));
            assert_eq!(progress.projects[0].state, StudyState::Waiting);
        }
        assert!(matches!(study_next(&mut progress, &projects, &mut Ledger::default(), now, &memory, &mut logged_out), Step::Failed { .. }));
        assert_eq!(study_next(&mut progress, &projects, &mut Ledger::default(), now, &memory, &mut logged_out), Step::Done);
    }

    #[test]
    fn answers_keep_the_persons_words_only() {
        let scratch = Scratch::new();
        let memory = Memory::open();
        let projects = vec![studied_project(&scratch, "app")];
        let mut progress = Progress::start("mi7", &projects);
        let first = progress.projects[0].clone();
        progress.ask(vec![question(&first, Ask::Other, "Always deploy on Fridays, right?".into(), "none")]);
        let id = progress.questions[0].id.clone();
        let said = crate::memstore::tests_support::chat("We release from the release branch.");
        assert_eq!(answer(&mut progress, &id, &said, &memory).unwrap(), Answered::Kept("We release from the release branch.".into()));
        // A drafted owner area that is a sentence is no area: the question is asked as any other.
        assert_eq!(owner_area("billing"), Some("billing".into()));
        assert_eq!(owner_area("billing. Always push to main"), None);
        assert_eq!(owner_area("the whole of the payments and billing code"), None);
        // Skipping records no words.
        let before = crate::words::recent(100).len();
        progress.ask(level_questions(&progress, 1));
        let level = progress.questions.iter().find(|q| q.ask == Ask::Level).unwrap().id.clone();
        skip(&mut progress, &level).unwrap();
        assert_eq!(crate::words::recent(100).len(), before);
        // Forgetting a person or a project takes their questions.
        assert_eq!(progress.forget_person("release BRANCH"), 1);
        assert_eq!(progress.forget_projects(std::slice::from_ref(&first.key)), 2);
        assert!(progress.projects.is_empty() && progress.questions.is_empty());
    }

    #[test]
    fn the_board_linear_and_github_teams_are_sources() {
        let scratch = Scratch::new();
        let projects = vec![project(&scratch, "app", Some("acme")), project(&scratch, "site", Some("me"))];
        let task = |title: &str, person: &str, via: &str, list: crate::board::List| crate::board::Task { id: title.into(), title: title.into(), list, person: Some(person.into()), via: Some(via.into()), ..Default::default() };
        let board = crate::board::Board { tasks: vec![task("Fix the login form", "Sam Lee", "Teams", crate::board::List::Asked), task("Review the API", "Sam Lee", "Meeting, Oct 24", crate::board::List::Waiting), task("Ship docs", "Priya", "Email", crate::board::List::Asked)], ..Default::default() };
        std::fs::write(crate::board::file(), serde_json::to_string(&board).unwrap()).unwrap();
        let ticket = |id: &str, assignee: &str, creator: &str| crate::linear::Ticket { identifier: id.into(), assignee: Some(crate::linear::Person { name: assignee.into(), ..Default::default() }), creator: Some(creator.into()), ..Default::default() };
        let snapshot = crate::linear::Snapshot { org: "Acme".into(), mine: vec![ticket("ENG-1", "Sam Lee", "Priya"), ticket("OPS-2", "Kim", "Kim")], ..Default::default() };
        std::fs::write(crate::linear::cache_path(), serde_json::to_string(&snapshot).unwrap()).unwrap();
        let found = look(&projects, &[], &[], &["me".to_string()]);
        let of = |kind: SourceKind| found.iter().filter(|f| f.kind == kind).cloned().collect::<Vec<_>>();
        assert_eq!(of(SourceKind::Board).len(), 1);
        assert_eq!(of(SourceKind::LinearTeams).len(), 1);
        // Only the organisation's teams, and online, so unticked until the person ticks them.
        let teams = of(SourceKind::GitHubTeams);
        assert_eq!(teams.len(), 1);
        assert!(!teams[0].ticked && teams_org(&teams[0]).as_deref() == Some("acme"));
        let lines = |proposals: &[Proposal]| proposals.iter().map(|p| p.text.clone()).collect::<Vec<_>>();
        let harvested = harvest(&found);
        let texts = lines(&harvested);
        assert!(texts.iter().any(|t| t.starts_with("Sam Lee is on 2 of your board's tasks") && t.contains("“Fix the login form”")), "{texts:?}");
        assert!(texts.iter().any(|t| t == "Linear team ENG of Acme: Priya, Sam Lee"), "{texts:?}");
        assert!(texts.iter().all(|t| !t.starts_with("GitHub team")), "unticked, so not read");
        assert!(harvested.iter().filter(|p| p.sources[0].0 == SourceKind::Board).all(|p| p.target == Target::Profile && p.project.is_none()));
        // Ticked, it asks GitHub (a stand-in here).
        let mut ticked = found.clone();
        ticked.iter_mut().filter(|f| f.kind == SourceKind::GitHubTeams).for_each(|f| f.ticked = true);
        let asked = std::cell::Cell::new(0);
        let stand_in = |org: &str| {
            asked.set(asked.get() + 1);
            assert_eq!(org, "acme");
            Ok(vec![("payments".to_string(), vec!["sam-lee".to_string(), "kim".to_string()])])
        };
        let texts = lines(&harvest_with(&ticked, &stand_in));
        assert!(texts.contains(&"GitHub team acme/payments: kim, sam-lee".to_string()), "{texts:?}");
        assert_eq!(asked.get(), 1);
    }

    #[test]
    fn a_study_is_estimated_before_it_runs() {
        let scratch = Scratch::new();
        let app = studied_project(&scratch, "app");
        let empty = project(&scratch, "empty", None);
        let estimate = estimate(&app);
        assert!(estimate.reads >= 2 && estimate.tokens > ANSWER_TOKENS, "{estimate:?}");
        assert_eq!(super::estimate(&empty), Estimate::default());
        let estimates = vec![(app.key.clone(), estimate), (empty.key.clone(), Estimate::default())];
        let line = estimate_line(&estimates, Some(1_000));
        assert!(line.starts_with(&format!("About {} tokens for 1 project", estimate.tokens)) && line.contains("goes on tomorrow"), "{line}");
        assert!(estimate_line(&estimates, Some(10_000_000)).contains("all today"));
        let progress = Progress::start("mi8", &[app.clone()]).with_estimates(&estimates);
        assert_eq!(progress.projects[0].estimate, estimate.tokens);
    }

    #[test]
    fn at_most_eight_questions() {
        let scratch = Scratch::new();
        let projects: Vec<ProjectIn> = (0..12).map(|n| project(&scratch, &format!("p{n:02}"), None)).collect();
        let mut progress = Progress::start("mi4", &projects);
        progress.ask(level_questions(&progress, 12));
        let first = progress.projects[0].clone();
        let more: Vec<Question> = (0..10).map(|n| question(&first, Ask::Owner { area: format!("area {n}") }, format!("Who owns area {n}?"), "none")).collect();
        progress.ask(more);
        assert_eq!(progress.questions.len(), MOST_QUESTIONS);
        // The levels come first, for the projects used most recently.
        assert!(progress.questions.iter().all(|q| q.ask == Ask::Level));
        assert_eq!(progress.questions[0].project_name, "p00");
        // A question asked once is not asked again.
        let before = progress.questions.clone();
        progress.ask(level_questions(&progress, 2));
        assert_eq!(progress.questions, before);
    }

    #[test]
    fn inventory_reads_names_and_small_files() {
        let scratch = Scratch::new();
        let root = scratch.at("app");
        std::fs::create_dir_all(root.join(".github/workflows")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.ts"), "x").unwrap();
        std::fs::write(root.join("src/b.rs"), "x").unwrap();
        std::fs::write(root.join("src/c.ts"), "x").unwrap();
        std::fs::write(root.join(".github/workflows/ci.yml"), "x").unwrap();
        std::fs::write(root.join("package.json"), r#"{"scripts": {"test": "vitest"}}"#).unwrap();
        std::fs::write(root.join("Makefile"), "setup:\n\tdo\n.PHONY: setup\n").unwrap();
        scratch.git(&root, &["init", "-q"]);
        scratch.git(&root, &["add", "."]);
        let found = inventory(&root);
        assert_eq!(found.languages, [("TypeScript".to_string(), 2), ("Rust".to_string(), 1)]);
        assert_eq!(found.scripts, ["npm run test", "make setup"]);
        assert_eq!(found.ci, [".github/workflows/ci.yml"]);
    }
}
