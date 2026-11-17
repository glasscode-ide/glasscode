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
pub const KEYS: [&str; 4] = ["setup", "branch_template", "worktree_folder", "default_agent"];

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
    Loaded { settings: ProjectSettings::default(), state: FileState::Broken, warnings: vec![why.into()] }
}

/// Read a project's file: never more than `MAX_BYTES`, never a panic.
pub fn load(root: &Path) -> Loaded {
    let path = file(root);
    let mut opened = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::default(),
        Err(e) => return broken(format!("could not be read ({e}); Glasscode uses its defaults")),
    };
    let mut bytes = Vec::new();
    if let Err(e) = opened.by_ref().take(MAX_BYTES + 1).read_to_end(&mut bytes) {
        return broken(format!("could not be read ({e}); Glasscode uses its defaults"));
    }
    if bytes.len() as u64 > MAX_BYTES {
        return broken(format!("is larger than {} KB, so it is not read; Glasscode uses its defaults", MAX_BYTES / 1024));
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
        Err(e) => return broken(format!("is not valid JSON (line {}, column {}); Glasscode uses its defaults", e.line(), e.column())),
    };
    let Value::Object(map) = value else {
        return broken("is not a JSON object ({ \"key\": value, … }); Glasscode uses its defaults");
    };
    let mut warnings = Vec::new();
    let setup = text_value(&map, "setup", &mut warnings).and_then(|s| usable_setup(s, &mut warnings));
    let branch_template = text_value(&map, "branch_template", &mut warnings);
    let worktree_folder = text_value(&map, "worktree_folder", &mut warnings);
    for (key, template) in [("branch_template", &branch_template), ("worktree_folder", &worktree_folder)] {
        let unknown = template.as_deref().map(|t| expand(t, &Vars::default()).unknown).unwrap_or_default();
        if !unknown.is_empty() {
            warnings.push(format!(
                "{key} uses {}, which Glasscode does not know (it knows {}); it stays as text",
                unknown.iter().map(|u| format!("{{{u}}}")).collect::<Vec<_>>().join(", "),
                VARIABLES.iter().map(|v| format!("{{{v}}}")).collect::<Vec<_>>().join(", ")
            ));
        }
    }
    let default_agent = text_value(&map, "default_agent", &mut warnings).and_then(|a| {
        let kind = agent_kind(&a);
        if kind.is_none() {
            warnings.push(format!("default_agent “{a}” is not claude, codex or shell; it is ignored"));
        }
        kind
    });
    Loaded { settings: ProjectSettings { setup, branch_template, worktree_folder, default_agent }, state: FileState::Read, warnings }
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

/// Characters that do not show as themselves: what the person approves must be what runs.
fn hidden(c: char) -> bool {
    c.is_control() || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
}

/// A setup command that can be shown as it is: one line, no character that hides, not too long.
fn usable_setup(setup: String, warnings: &mut Vec<String>) -> Option<String> {
    if setup.chars().count() > SETUP_MAX {
        warnings.push(format!("setup is longer than {SETUP_MAX} characters; it is ignored (call a script from it instead)"));
        return None;
    }
    if setup.chars().any(hidden) {
        warnings.push("setup has a line break or a character that does not show; it is ignored, because what you approve must be what runs".into());
        return None;
    }
    Some(setup)
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
            _ => anyhow::bail!("{} is not a JSON object; fix it by hand first", path.display()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Map::new(),
        Err(e) => return Err(e.into()),
    };
    let text = |v: &Option<String>| v.clone().map(Value::String);
    for (key, value) in [
        ("setup", text(&values.setup)),
        ("branch_template", text(&values.branch_template)),
        ("worktree_folder", text(&values.worktree_folder)),
        ("default_agent", values.default_agent.map(|a| Value::String(agent_word(a).into()))),
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
    let stem = name.split('.').next().unwrap_or(name).trim_end().to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit())
}

/// Characters that a Windows path cannot have.
const NOT_IN_PATH: [char; 7] = ['<', '>', ':', '"', '|', '?', '*'];
/// Characters that a git branch cannot have, besides control characters and spaces.
const NOT_IN_BRANCH: [char; 7] = ['~', '^', ':', '?', '*', '[', '\\'];

/// A branch from what a template made: characters that git or a Windows path cannot have become
/// `-`, no `..` or `@{`, no part that starts with `.` or `-` or ends with `.` or `.lock`, no
/// device name, at most `BRANCH_MAX` characters. None when nothing is left.
pub fn clean_branch(raw: &str) -> Option<String> {
    let mapped: String = raw
        .chars()
        .map(|c| if c.is_control() || c.is_whitespace() || hidden(c) || NOT_IN_BRANCH.contains(&c) || NOT_IN_PATH.contains(&c) { '-' } else { c })
        .collect();
    let mut name = mapped.replace("@{", "-");
    while name.contains("..") {
        name = name.replace("..", ".");
    }
    let mut name = name.split('/').filter_map(clean_ref_part).collect::<Vec<_>>().join("/");
    if name.chars().count() > BRANCH_MAX {
        let cut: String = name.chars().take(BRANCH_MAX).collect();
        name = cut.split('/').filter_map(clean_ref_part).collect::<Vec<_>>().join("/");
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
        p = p.trim_start_matches(['.', '-']).trim_end_matches(['.', '-']).to_string();
        // `get`, not an index: the end may be inside a character of more than one byte.
        if p.len() >= 5 && p.get(p.len() - 5..).is_some_and(|end| end.eq_ignore_ascii_case(".lock")) {
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
/// a leading separator cannot make it absolute (`:` is not kept); characters that a Windows path
/// cannot have become `-`; no folder ends with a dot or a space or is a device name; at most
/// `FOLDER_DEPTH_MAX` folders of `FOLDER_PART_MAX` characters. None when nothing is left.
pub fn clean_folder(raw: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    let mut depth = 0;
    for part in raw.split(['/', '\\']) {
        let Some(part) = clean_folder_part(part) else { continue };
        if depth == FOLDER_DEPTH_MAX {
            break;
        }
        out.push(part);
        depth += 1;
    }
    (depth > 0).then_some(out)
}

fn clean_folder_part(part: &str) -> Option<String> {
    let mapped: String = part.chars().filter(|c| !c.is_control() && !hidden(*c)).map(|c| if NOT_IN_PATH.contains(&c) { '-' } else { c }).collect();
    let mut p: String = mapped.trim().trim_end_matches(['.', ' ']).to_string();
    if p.chars().count() > FOLDER_PART_MAX {
        p = p.chars().take(FOLDER_PART_MAX).collect::<String>().trim_end_matches(['.', ' ']).to_string();
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
        issue: crate::linear::identifiers_in(branch).first().map(|i| i.to_lowercase()).unwrap_or_default(),
        kind: branch.split_once('/').map(|(kind, _)| kind.to_string()).unwrap_or_default(),
        slug: last.to_string(),
    }
}

/// The folder of a new worktree: `base` (Glasscode's worktree folder) and the project's template;
/// without a template, or when it leaves nothing, `<project>/<branch>` as before.
pub fn worktree_dir(base: &Path, project_name: &str, branch: &str, template: Option<&str>) -> PathBuf {
    let default = base.join(crate::git::slug(project_name)).join(crate::git::slug(branch));
    let Some(template) = template else { return default };
    match clean_folder(&expand(template, &folder_vars(project_name, branch)).text) {
        Some(rel) if stays_below(&rel) => base.join(rel),
        _ => default,
    }
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
    let issue = of.issue.map(|i| i.trim().trim_start_matches('#').to_lowercase()).filter(|i| !i.is_empty());
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

/// What is kept of an approved command: its SHA-256, in hex.
pub fn fingerprint(command: &str) -> String {
    Sha256::digest(command.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// This exact command was approved: it runs.
    Run,
    /// Ask first. `changed`: a different command was approved before.
    Ask { changed: bool },
    /// "Never for this project".
    Never,
}

pub fn decide(approval: Option<&SetupApproval>, command: &str) -> Decision {
    match approval {
        Some(a) if a.never => Decision::Never,
        Some(SetupApproval { approved: Some(hash), .. }) if *hash == fingerprint(command) => Decision::Run,
        Some(SetupApproval { approved: Some(_), .. }) => Decision::Ask { changed: true },
        _ => Decision::Ask { changed: false },
    }
}

pub fn approved(command: &str) -> SetupApproval {
    SetupApproval { approved: Some(fingerprint(command)), never: false }
}

pub fn never() -> SetupApproval {
    SetupApproval { approved: None, never: true }
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
    list.push((path.to_path_buf(), NewWorktree { root: root.to_path_buf(), loaded }));
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
        Vars { repo: "app".into(), branch: "eng-826-calendar-sync".into(), issue: "eng-826".into(), kind: "feat".into(), slug: "calendar-sync".into() }
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
            assert_eq!(expand(template, &v), Expanded { text: expected.into(), unknown: vec![] }, "{template}");
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
        assert!(loaded.warnings[0].contains("{ticket}") && loaded.warnings[0].contains("branch_template"));
        assert_eq!(loaded.settings.branch_template.as_deref(), Some("{type}/{ticket}-{ticket}"));
    }

    /// A value from a ticket's title is data: a `{name}` in it is never filled in.
    #[test]
    fn a_value_is_never_read_as_a_template() {
        let v = Vars { slug: "{repo}".into(), ..vars() };
        assert_eq!(expand("{slug}", &v).text, "{repo}");
    }

    #[test]
    fn a_branch_has_no_character_that_git_or_windows_refuses() {
        for (raw, expected) in [
            ("feat/eng-826-calendar-sync", Some("feat/eng-826-calendar-sync")),
            ("feat/a b\tc", Some("feat/a-b-c")),
            ("feat/x~y^z:w?v*u[t\\s", Some("feat/x-y-z-w-v-u-t-s")),
            ("a<b>c\"d|e", Some("a-b-c-d-e")),
            ("feat/a..b", Some("feat/a.b")),
            ("feat/....b", Some("feat/b")),
            ("feat/x@{y}", Some("feat/x-y}")),
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
        assert!(cut.chars().count() <= BRANCH_MAX && !cut.ends_with('-'), "{cut}");
        // A cut in the middle of a character of more than one byte.
        let wide = "ü".repeat(150);
        assert_eq!(clean_branch(&wide).unwrap().chars().count(), BRANCH_MAX);
    }

    /// Git itself agrees: each cleaned name passes `git check-ref-format --branch`.
    #[test]
    fn git_accepts_each_cleaned_branch() {
        let nasty = [
            "feat/eng-826 calendar", "a..b", "x@{1}", ".x/.y", "x.lock/y.lock", "-x", "a//b", "a/./b", "a/../b", "x~1^2:3",
            "?*[", "a\\b", "trailing.", "trailing/", "tab\there", "new\nline", "con/aux", "ü/é", "{unknown}/x", "a b/c d",
            "feat/-", "-/x", "@/x", "x/@", "HEAD/x",
        ];
        for raw in nasty {
            let Some(name) = clean_branch(raw) else { continue };
            let out = crate::sys::command("git").args(["check-ref-format", "--branch", &name]).output().expect("git runs");
            assert!(out.status.success(), "{raw:?} became {name:?}, which git refuses: {}", String::from_utf8_lossy(&out.stderr));
        }
    }

    #[test]
    fn a_folder_stays_below_the_worktree_folder() {
        let p = |s: &str| clean_folder(s);
        let parts = |s: &str| p(s).map(|p| p.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>());
        assert_eq!(parts("app/feat-x"), Some(vec!["app".into(), "feat-x".into()]));
        // No `..`, no `.`, no root, no drive.
        assert_eq!(parts("../../outside"), Some(vec!["outside".into()]));
        assert_eq!(parts("app/../../x"), Some(vec!["app".into(), "x".into()]));
        assert_eq!(parts("./app/./x"), Some(vec!["app".into(), "x".into()]));
        assert_eq!(parts("/etc/passwd"), Some(vec!["etc".into(), "passwd".into()]));
        assert_eq!(parts("C:\\Windows\\x"), Some(vec!["C-".into(), "Windows".into(), "x".into()]));
        assert_eq!(parts("\\\\server\\share\\x"), Some(vec!["server".into(), "share".into(), "x".into()]));
        assert_eq!(parts("~/x"), Some(vec!["~".into(), "x".into()]));
        // Characters a Windows path cannot have, control characters, trailing dots and spaces.
        assert_eq!(parts("a<b>c:d\"e|f?g*h"), Some(vec!["a-b-c-d-e-f-g-h".into()]));
        assert_eq!(parts("a\u{1}b\u{202E}c"), Some(vec!["abc".into()]));
        assert_eq!(parts("dots.../spaces   /x"), Some(vec!["dots".into(), "spaces".into(), "x".into()]));
        assert_eq!(parts("CON/lpt1.txt/Aux"), Some(vec!["CON_".into(), "lpt1.txt_".into(), "Aux_".into()]));
        // Depth and length.
        assert_eq!(parts("a/b/c/d/e/f").map(|p| p.len()), Some(FOLDER_DEPTH_MAX));
        assert_eq!(parts(&"x".repeat(300)).unwrap()[0].chars().count(), FOLDER_PART_MAX);
        for nothing in ["", "/", "..", "../..", "./.", " . ", "\u{202E}"] {
            assert_eq!(p(nothing), None, "{nothing:?}");
        }
        assert_eq!(parts(":"), Some(vec!["-".into()]));
        for s in ["../../outside", "/etc/passwd", "C:\\Windows", "a/../../b", "\\\\srv\\s"] {
            assert!(stays_below(&p(s).unwrap()), "{s}");
        }
        assert!(!stays_below(Path::new("../x")));
        assert!(!stays_below(Path::new("")));
    }

    #[test]
    fn the_worktree_folder_follows_the_template_and_stays_below_the_base() {
        let base = Path::new("base").join("worktrees");
        // Without a template: as before.
        assert_eq!(worktree_dir(&base, "my app", "feature/login", None), base.join("my-app").join("feature-login"));
        assert_eq!(worktree_dir(&base, "my app", "feature/login", Some(DEFAULT_FOLDER)), base.join("my-app").join("feature-login"));
        assert_eq!(worktree_dir(&base, "app", "feat/eng-826-sync", Some("{repo}-wt/{type}/{issue}")), base.join("app-wt").join("feat").join("eng-826"));
        assert_eq!(worktree_dir(&base, "app", "feat/eng-826-sync", Some("{slug}")), base.join("eng-826-sync"));
        // A template that tries to leave, or leaves nothing.
        assert_eq!(worktree_dir(&base, "app", "x", Some("../../{branch}")), base.join("x"));
        assert_eq!(worktree_dir(&base, "app", "x", Some("C:/{branch}")), base.join("C-").join("x"));
        assert_eq!(worktree_dir(&base, "app", "x", Some("../..")), base.join("app").join("x"));
        assert_eq!(worktree_dir(&base, "app", "x", Some("{issue}")), base.join("app").join("x"), "no issue in the branch: nothing left");
        assert!(worktree_dir(&base, "app", "x", Some("/../{repo}/..\\..\\{branch}")).starts_with(&base));
    }

    #[test]
    fn a_branch_from_the_template() {
        let labels = vec!["Bug".to_string()];
        let ticket = BranchFor { project_name: "My App", default: "eng-212-first-run-import", issue: Some("GLA-212"), title: Some("First run: import projects"), labels: &[] };
        assert_eq!(branch_name("{type}/{issue}-{slug}", &ticket).as_deref(), Some("feat/gla-212-first-run-import-projects"));
        assert_eq!(branch_name("{repo}/{branch}", &ticket).as_deref(), Some("my-app/eng-212-first-run-import"));
        let bug = BranchFor { labels: &labels, ..ticket.clone() };
        assert_eq!(branch_name("{type}/{issue}", &bug).as_deref(), Some("fix/gla-212"));
        // No ticket: the issue is left out, and the separator with it.
        let titled = BranchFor { project_name: "app", default: "", issue: None, title: Some("Flaky login test"), labels: &[] };
        assert_eq!(branch_name("{type}/{issue}-{slug}", &titled).as_deref(), Some("feat/flaky-login-test"));
        let fix = BranchFor { title: Some("Fix: the login test"), ..titled.clone() };
        assert_eq!(branch_name("{type}/{slug}", &fix).as_deref(), Some("fix/fix-login-test"));
        // Nothing tells the work apart: no name from the template.
        let bare = BranchFor { project_name: "app", default: "", issue: None, title: None, labels: &[] };
        assert_eq!(branch_name("{type}/{issue}-{slug}", &bare), None);
        // A GitHub issue's number.
        let issue = BranchFor { issue: Some("#123"), ..titled.clone() };
        assert_eq!(branch_name("{issue}-{slug}", &issue).as_deref(), Some("123-flaky-login-test"));
        // A title with characters a branch cannot have.
        let odd = BranchFor { title: Some("Make ~/.config:work? [again]"), ..titled.clone() };
        let name = branch_name("{type}/{slug}", &odd).unwrap();
        assert!(clean_branch(&name).as_deref() == Some(name.as_str()), "{name}");
        // A template that leaves nothing valid.
        assert_eq!(branch_name("..", &ticket), None);
    }

    #[test]
    fn a_project_value_wins_over_the_personal_value() {
        assert_eq!(in_effect(Some("codex"), Some("shell"), "claude"), ("codex", Source::Project));
        assert_eq!(in_effect(None, Some("shell"), "claude"), ("shell", Source::Personal));
        assert_eq!(in_effect::<&str>(None, None, "claude"), ("claude", Source::Glasscode));
    }

    #[test]
    fn a_broken_file_gives_the_defaults_and_one_warning() {
        for text in ["{ \"setup\": ", "[1, 2]", "\"text\"", "", "{ setup: 1 }"] {
            let loaded = parse(text);
            assert_eq!((loaded.settings.clone(), loaded.state, loaded.warnings.len()), (ProjectSettings::default(), FileState::Broken, 1), "{text:?}");
        }
        assert!(parse("{ \"setup\": ").warnings[0].contains("line 1"));
        // Values of the wrong kind: each is ignored with its own warning, the others stay.
        let loaded = parse(r#"{ "setup": 5, "branch_template": "{type}/{slug}", "worktree_folder": ["x"], "default_agent": null }"#);
        assert_eq!(loaded.state, FileState::Read);
        assert_eq!(loaded.settings, ProjectSettings { branch_template: Some("{type}/{slug}".into()), ..Default::default() });
        assert_eq!(loaded.warnings.len(), 2, "{:?}", loaded.warnings);
    }

    #[test]
    fn the_file_is_read_with_a_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), Loaded::default(), "no file: the defaults and no warning");
        std::fs::create_dir_all(dir.path().join(DIR)).unwrap();
        let mut big = String::from("{ \"setup\": \"echo hi\", \"pad\": \"");
        big.push_str(&"x".repeat(MAX_BYTES as usize));
        big.push_str("\" }");
        std::fs::write(file(dir.path()), &big).unwrap();
        let loaded = load(dir.path());
        assert_eq!((loaded.state, loaded.settings.setup, loaded.warnings.len()), (FileState::Broken, None, 1));
        std::fs::write(file(dir.path()), [0xff, 0xfe, b'{']).unwrap();
        assert_eq!(load(dir.path()).state, FileState::Broken);
        std::fs::write(file(dir.path()), r#"{ "setup": "  echo setup ran  ", "default_agent": "Codex" }"#).unwrap();
        let loaded = load(dir.path());
        assert_eq!((loaded.state, loaded.settings.setup.as_deref(), loaded.settings.default_agent), (FileState::Read, Some("echo setup ran"), Some(AgentKind::Codex)));
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn only_the_agents_that_exist_are_taken() {
        for (word, agent) in [("claude", Some(AgentKind::Claude)), ("CODEX", Some(AgentKind::Codex)), (" shell ", Some(AgentKind::Shell)), ("gemini", None), ("cursor", None)] {
            let loaded = parse(&format!(r#"{{ "default_agent": "{word}" }}"#));
            assert_eq!(loaded.settings.default_agent, agent, "{word}");
            assert_eq!(loaded.warnings.len(), usize::from(agent.is_none()), "{word}: {:?}", loaded.warnings);
        }
    }

    #[test]
    fn a_setup_that_does_not_show_as_itself_is_not_used() {
        for setup in ["echo a\\necho b", "echo a\u{202E}b", "echo \u{200B}x", "echo \u{1b}[2Jx"] {
            let loaded = parse(&serde_json::json!({ "setup": setup.replace("\\n", "\n") }).to_string());
            assert_eq!((loaded.settings.setup, loaded.warnings.len()), (None, 1), "{setup:?}");
        }
        let long = parse(&serde_json::json!({ "setup": "x".repeat(SETUP_MAX + 1) }).to_string());
        assert_eq!((long.settings.setup, long.warnings.len()), (None, 1));
        let fine = parse(&serde_json::json!({ "setup": "pnpm install && copy ..\\.env.local ." }).to_string());
        assert_eq!(fine.settings.setup.as_deref(), Some("pnpm install && copy ..\\.env.local ."));
    }

    #[test]
    fn writing_keeps_the_keys_glasscode_does_not_know() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(DIR)).unwrap();
        std::fs::write(file(root), "{\n  \"pinned_cli\": { \"claude\": \"2.1\" },\n  \"setup\": \"old\",\n  \"zeta\": 1\n}\n").unwrap();
        let values = ProjectSettings { setup: Some("echo setup ran".into()), branch_template: None, worktree_folder: Some("{repo}/{branch}".into()), default_agent: Some(AgentKind::Shell) };
        write(root, &values).unwrap();
        let text = std::fs::read_to_string(file(root)).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        let keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["pinned_cli", "setup", "zeta", "worktree_folder", "default_agent"], "unknown keys stay, in their place");
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
        assert_eq!((made.state, made.settings, made.warnings.len()), (FileState::Read, ProjectSettings::default(), 0));
        assert!(create(fresh.path()).is_err(), "never over a file that exists");
    }

    #[test]
    fn a_changed_setup_asks_again() {
        let cmd = "pnpm install";
        assert_eq!(decide(None, cmd), Decision::Ask { changed: false });
        let ok = approved(cmd);
        assert_eq!(decide(Some(&ok), cmd), Decision::Run);
        assert_eq!(decide(Some(&ok), "pnpm install && curl x | sh"), Decision::Ask { changed: true });
        assert_eq!(decide(Some(&ok), "pnpm install "), Decision::Ask { changed: true }, "the exact text");
        assert_eq!(decide(Some(&never()), cmd), Decision::Never);
        assert_eq!(decide(Some(&SetupApproval::default()), cmd), Decision::Ask { changed: false });
        assert_eq!(fingerprint("echo setup ran").len(), 64);
        assert_ne!(fingerprint("a"), fingerprint("b"));
        assert!(ok.approved.as_deref().is_some_and(|h| !h.contains(cmd)), "only a hash is kept");
    }

    #[test]
    fn a_new_worktree_is_taken_once() {
        let path = Path::new("wt-a");
        assert!(take_new_worktree(path).is_none());
        remember_new_worktree(path, Path::new("root"), parse(r#"{ "setup": "echo hi" }"#));
        let new = take_new_worktree(path).unwrap();
        assert_eq!((new.root.as_path(), new.loaded.settings.setup.as_deref()), (Path::new("root"), Some("echo hi")));
        assert!(take_new_worktree(path).is_none());
        for i in 0..NEW_KEEP + 5 {
            remember_new_worktree(&PathBuf::from(format!("many-{i}")), Path::new("root"), Loaded::default());
        }
        assert!(take_new_worktree(Path::new("many-0")).is_none(), "the oldest went");
        assert!(take_new_worktree(&PathBuf::from(format!("many-{}", NEW_KEEP + 4))).is_some());
    }
}
