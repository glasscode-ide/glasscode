//! What an op's words name, and what it touches ([`targets`]), resolved by Rust from what the app
//! holds in memory (ops-9: no git, no files): its projects, worktrees and sessions, and who calls.
//! The app's handlers resolve projects, worktrees and tasks here too, so that the handler and the
//! gate (M21) always agree on what a word names, and a target is never taken from a caller's
//! `--project` alone (trust-3): a session or a worktree is in the project it is in.
//!
//! The rules are v1's. Only the person may name a project by the start of its name or a task by
//! words from its title (ops-17); any other caller gives an exact name or id, or is told what the
//! word could mean. While the engine is v1, the app counts every caller as the person, as v1 did.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Candidate, ErrorCode, Op, OpError, Targets};
use crate::board::{Board, Outside};
use crate::paths::ProjectKey;

/// A project as the app holds it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectView {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    /// Hidden in Settings › Git and projects: no word names it.
    #[serde(default)]
    pub hidden: bool,
    /// Its key (M11's `identity`), when the app has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<ProjectKey>,
    /// Its GitHub repository ("acme/app"), when the app's last scan read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// A worktree of a project, as the app's last list of them has it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorktreeView {
    /// Its project's id.
    pub project: String,
    pub path: PathBuf,
    #[serde(default)]
    pub branch: String,
}

/// A session and where it runs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionPlace {
    pub id: String,
    /// Its project's id.
    pub project: String,
    pub worktree: PathBuf,
}

/// What words and targets are resolved against.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct World {
    /// The person names things by the start of a name or words from a title (ops-17).
    pub person: bool,
    /// The session the command runs in.
    pub caller_session: Option<String>,
    /// The project a server session runs in, by name, as glasscoded says (`Caller.project`).
    pub caller_project: Option<String>,
    /// Where the command was run: relative paths are from here.
    pub cwd: Option<PathBuf>,
    /// The person's home folder, for "~" (`paths::home`).
    pub home: PathBuf,
    /// Every project, the hidden ones too (only their own sessions reach them).
    pub projects: Vec<ProjectView>,
    pub worktrees: Vec<WorktreeView>,
    /// Every session, the hidden projects' too.
    pub sessions: Vec<SessionPlace>,
}

impl World {
    pub fn project(&self, id: &str) -> Option<&ProjectView> {
        self.projects.iter().find(|p| p.id == id)
    }

    fn visible(&self) -> impl Iterator<Item = &ProjectView> {
        self.projects.iter().filter(|p| !p.hidden)
    }

    fn hidden(&self, project: &str) -> bool {
        self.project(project).is_some_and(|p| p.hidden)
    }

    /// The visible projects' names, for "Projects: app, shop."
    pub fn project_names(&self) -> String {
        let names: Vec<&str> = self.visible().map(|p| p.name.as_str()).collect();
        if names.is_empty() {
            return "none yet (`glasscode project import <folder>`)".into();
        }
        names.join(", ")
    }

    /// "~/code/app" for a folder in the home folder.
    pub fn home_relative(&self, path: &Path) -> String {
        let text = path.display().to_string();
        let home = self.home.display().to_string();
        match text.strip_prefix(&home) {
            Some(rest) => format!("~{rest}"),
            None => text,
        }
    }

    /// A path as the command line gave it: "~" is the home folder, a relative one is from where
    /// the command was run (`base`).
    pub fn expand(&self, path: &str, base: Option<&Path>) -> PathBuf {
        if path == "~" {
            return self.home.clone();
        }
        if let Some(rest) = path.strip_prefix("~/") {
            return self.home.join(rest);
        }
        let p = PathBuf::from(path);
        match base {
            Some(base) if p.is_relative() => base.join(p),
            _ => p,
        }
    }
}

/// A project's id by its id, name, folder or folder name; for the person, also by the start of
/// its name. Hidden projects are never named.
pub fn find_project(world: &World, word: &str) -> Result<String, OpError> {
    let w = word.trim();
    let path = world.expand(w, None);
    let exact = world.visible().find(|p| p.id == w || p.name.eq_ignore_ascii_case(w) || p.root == path || p.root.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(w)));
    if let Some(p) = exact {
        return Ok(p.id.clone());
    }
    let starts: Vec<&ProjectView> = world.visible().filter(|p| p.name.to_lowercase().starts_with(&w.to_lowercase())).collect();
    match (starts.as_slice(), world.person) {
        ([one], true) => Ok(one.id.clone()),
        ([], _) | (_, true) => Err(OpError::new(ErrorCode::NotFound, format!("No project “{w}”. Projects: {}.", world.project_names()))),
        (could, false) => Err(OpError::new(
            ErrorCode::Ambiguous { candidates: could.iter().take(6).map(|p| Candidate { id: p.id.clone(), title: p.name.clone(), project: None }).collect() },
            format!(
                "“{w}” is not a project's name; it could mean {}. Name a project by its whole name or id: only the person, at a terminal, names one by the start of its name.",
                could.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
            ),
        )),
    }
}

/// The project a command is for: `--project`, else the calling session's, else the one a server
/// session runs in, else the one the command was run in, else the only one.
pub fn project_for(world: &World, given: Option<&str>) -> Result<String, OpError> {
    if let Some(word) = given {
        return find_project(world, word);
    }
    if let Some(s) = world.caller_session.as_deref().and_then(|id| world.sessions.iter().find(|s| s.id == id)) {
        return Ok(s.project.clone());
    }
    // A server session: its project by name (the Mac has its own copy under the same name).
    if let Some(p) = world.caller_project.as_deref().and_then(|name| world.visible().find(|p| p.name.eq_ignore_ascii_case(name))) {
        return Ok(p.id.clone());
    }
    if let Some(cwd) = &world.cwd {
        let by_worktree = world.worktrees.iter().find(|w| !world.hidden(&w.project) && cwd.starts_with(&w.path)).map(|w| w.project.clone());
        let by_root = world.visible().filter(|p| cwd.starts_with(&p.root)).max_by_key(|p| p.root.as_os_str().len()).map(|p| p.id.clone());
        if let Some(id) = by_worktree.or(by_root) {
            return Ok(id);
        }
    }
    let visible: Vec<&ProjectView> = world.visible().collect();
    match visible.as_slice() {
        [one] => Ok(one.id.clone()),
        _ => Err(OpError::usage(format!("Say which project: --project <name> ({}).", world.project_names()))),
    }
}

/// A worktree by its folder, its branch, its folder's name or the end of its path, in `project`
/// when one is given. Paths are from where the command was run.
pub fn find_worktree<'a>(world: &'a World, word: &str, project: Option<&str>) -> Result<&'a WorktreeView, OpError> {
    let w = word.trim().trim_end_matches('/');
    let path = world.expand(w, world.cwd.as_deref());
    let hits: Vec<&WorktreeView> = world
        .worktrees
        .iter()
        .filter(|wt| !world.hidden(&wt.project) && project.is_none_or(|p| p == wt.project))
        .filter(|wt| {
            let name = wt.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            wt.path == path || wt.branch == w || name == w || wt.path.to_string_lossy().ends_with(&format!("/{w}"))
        })
        .collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(OpError::new(ErrorCode::NotFound, format!("No worktree “{w}”. `glasscode worktree list` shows them."))),
        many => Err(OpError::usage(format!(
            "“{w}” matches {} worktrees: {}. Use its path.",
            many.len(),
            many.iter().map(|wt| world.home_relative(&wt.path)).collect::<Vec<_>>().join(", ")
        ))),
    }
}

/// A task or item of the board by its id, key or reference (ENG-42, #958); for the person, also by
/// the start of its id, a link, or words from its title (`Board::resolve`). Any other caller is
/// told which task those words would name.
pub fn find_task(board: &Board, outside: &[Outside], word: &str, person: bool) -> Result<String, OpError> {
    let resolved = board.resolve(outside, word).map_err(|why| OpError::new(ErrorCode::Failed, why));
    if person {
        return resolved;
    }
    let w = word.trim().trim_start_matches("task:");
    let reference = if w.chars().all(|c| c.is_ascii_digit()) { format!("#{w}") } else { w.to_string() };
    let exact = board.task(w).is_some()
        || board.recurring.iter().any(|r| r.id == w || format!("recurring:{}", r.id) == w)
        || outside.iter().any(|o| o.key == word || o.reference.eq_ignore_ascii_case(&reference))
        || board.focus.iter().any(|k| k == word);
    match resolved {
        Ok(key) if exact => Ok(key),
        Ok(key) => {
            let title = board.task(key.trim_start_matches("task:")).map(|t| t.title.clone()).unwrap_or_else(|| key.clone());
            Err(OpError::new(
                ErrorCode::Ambiguous { candidates: vec![Candidate { id: key.trim_start_matches("task:").to_string(), title: title.clone(), project: None }] },
                format!("“{w}” is not a task's id; it could mean {} ({title}). Name a task by its id: only the person, at a terminal, names one by words from its title.", key.trim_start_matches("task:")),
            ))
        }
        Err(e) => Err(e),
    }
}

/// What an op touches: the projects, sessions, repositories and folders its words name, resolved
/// here from ids, and whether anything leaves this machine. The words are resolved as the op's
/// handler resolves them, so an op whose words name nothing fails here as it fails there.
pub fn targets(op: &Op, world: &World) -> Result<Targets, OpError> {
    let mut found = Found { world, out: Targets::default(), projects: Vec::new() };
    let name = op.name();
    // The session it names (a whole id: `parse` resolved it).
    if op.spec.args.iter().any(|a| a.name == "session" && a.kind == super::ArgKind::Session)
        && let Some(id) = op.text("session")
    {
        found.session(id);
    }
    // A project it names, or the one it is for.
    let project = match name {
        "session.new" | "pr.review" | "pr.build" | "pr.status" | "pr.rerun" | "pr.comment" | "pr.label" | "ticket.start" | "worktree.new" => Some(project_for(world, op.text("project"))?),
        "project.remove" | "project.set_account" => Some(find_project(world, op.text("project").unwrap_or_default())?),
        "session.list" | "worktree.list" | "worktree.clean" | "git.status" | "git.log" | "git.diff" => op.text("project").map(|p| find_project(world, p)).transpose()?,
        // A repository named outright is the project that has it, if any; else the project's.
        "pr.view" | "pr.checks" | "pr.diff" | "review.threads" | "ci.log" => match op.text("repo") {
            Some(repo) => world.visible().find(|p| p.repo.as_deref().is_some_and(|r| r.eq_ignore_ascii_case(repo))).map(|p| p.id.clone()),
            None => Some(project_for(world, op.text("project"))?),
        },
        _ => None,
    };
    // Any other op that names a project is about it too (trust-3), whichever op it is: that
    // project's policy decides with the caller's own (its never list, the one account that may
    // read it, a pause). Its handler resolves the word itself, some by id or name among every
    // project, the hidden ones too: so a project the word names either way is a target, and a
    // word that names none here fails in the handler, never here.
    let project = match project {
        None if op.spec.args.iter().any(|a| a.name == "project" && a.kind == super::ArgKind::Project) => op.text("project").and_then(|word| named_project(world, word)),
        project => project,
    };
    if let Some(id) = &project {
        found.project(id);
    }
    // A worktree it names: in that project when there is one, and in the project it is in.
    if let Some(word) = op.text("worktree") {
        let wt = find_worktree(world, word, project.as_deref())?;
        found.project(&wt.project);
        found.path(&wt.path);
    }
    match name {
        // No project named: the calling session's, else every one.
        "worktree.clean" if project.is_none() => match world.caller_session.as_deref().and_then(|id| world.sessions.iter().find(|s| s.id == id)) {
            Some(s) => found.project(&s.project),
            None => world.projects.iter().for_each(|p| found.project(&p.id)),
        },
        "project.import" => {
            let source = op.text("source").unwrap_or_default().trim();
            found.path(&world.expand(source, world.cwd.as_deref()));
            // A clone comes from the network: an address, or owner/name on GitHub.
            let owner_name = source.split('/').count() == 2 && !source.starts_with(['.', '/', '~']);
            found.out.leaves_machine = source.contains("://") || source.starts_with("git@") || owner_name;
        }
        "file.read" => {
            let path = world.expand(op.text("path").unwrap_or_default().trim(), world.cwd.as_deref());
            if let Some(p) = world.projects.iter().filter(|p| path.starts_with(&p.root)).max_by_key(|p| p.root.as_os_str().len()) {
                found.project(&p.id.clone());
            }
            found.path(&path);
        }
        // A screen, or a session.
        "app.open" | "ui.open" => {
            if let Some(s) = op.text("place").and_then(|word| world.sessions.iter().find(|s| s.id == word)) {
                found.session(&s.id);
            }
        }
        _ => {}
    }
    // What goes to GitHub, Linear or a teammate leaves this machine.
    if op.spec.effect == super::EffectClass::Outward || op.spec.host == super::OpHost::Network {
        found.out.leaves_machine = true;
        if let Some(repo) = op.text("repo").filter(|_| name.starts_with("pr.") || name.starts_with("review.") || name.starts_with("ci."))
            && !found.out.repos.iter().any(|r| r.eq_ignore_ascii_case(repo))
        {
            found.out.repos.push(repo.to_string());
        }
        if name.starts_with("pr.") || name.starts_with("review.") || name.starts_with("ci.") {
            for id in found.projects.clone() {
                if let Some(repo) = world.project(&id).and_then(|p| p.repo.clone())
                    && !found.out.repos.contains(&repo)
                {
                    found.out.repos.push(repo);
                }
            }
        }
    }
    Ok(found.out)
}

/// The project a word names for the gate: by id or name among every project (hidden ones too, as
/// some handlers find them), else as [`find_project`] reads it.
fn named_project(world: &World, word: &str) -> Option<String> {
    let w = word.trim();
    world.projects.iter().find(|p| p.id == w || p.name.eq_ignore_ascii_case(w)).map(|p| p.id.clone()).or_else(|| find_project(world, w).ok())
}

/// The targets found so far.
struct Found<'a> {
    world: &'a World,
    out: Targets,
    /// The ids of the projects among them (their keys may be unknown).
    projects: Vec<String>,
}

impl Found<'_> {
    fn path(&mut self, path: &Path) {
        if !self.out.paths.iter().any(|p| p == path) {
            self.out.paths.push(path.to_path_buf());
        }
    }

    /// A project: its key when the app knows it, and always its folder.
    fn project(&mut self, id: &str) {
        let world = self.world;
        let Some(p) = world.project(id) else { return };
        if !self.projects.iter().any(|known| known == id) {
            self.projects.push(id.to_string());
        }
        if let Some(key) = &p.key
            && !self.out.projects.contains(key)
        {
            self.out.projects.push(key.clone());
        }
        self.path(&p.root);
    }

    /// A session, the project it is in, and its worktree. A server session ("server:…") is only
    /// its id.
    fn session(&mut self, id: &str) {
        if !self.out.sessions.iter().any(|s| s == id) {
            self.out.sessions.push(id.to_string());
        }
        let world = self.world;
        if let Some(s) = world.sessions.iter().find(|s| s.id == id) {
            self.project(&s.project);
            self.path(&s.worktree);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{ParseCtx, SessionView, parse};

    const APP_SESSION: &str = "8765f51a-8ad5-4e73-9df0-27adaa817aa9";
    const SHOP_SESSION: &str = "d740a312-2980-49ad-96b0-17b1bc7bcb50";

    fn key(text: &str) -> Option<ProjectKey> {
        ProjectKey::parse(text)
    }

    /// Two projects, app and shop, each with a worktree and a session in it, and a hidden one.
    fn world(person: bool) -> World {
        let project = |id: &str, name: &str, hidden: bool| ProjectView {
            id: id.into(),
            name: name.into(),
            root: PathBuf::from(format!("/code/{name}")),
            hidden,
            key: key(&format!("{name}-3f9a1c")),
            repo: Some(format!("acme/{name}")),
        };
        World {
            person,
            caller_session: Some(APP_SESSION.into()),
            caller_project: None,
            cwd: Some(PathBuf::from("/tmp")),
            home: PathBuf::from("/home/me"),
            projects: vec![project("p-app", "app", false), project("p-shop", "shop", false), project("p-secret", "secret", true)],
            worktrees: vec![
                WorktreeView { project: "p-app".into(), path: "/wt/app/feat-api".into(), branch: "feat/api".into() },
                WorktreeView { project: "p-shop".into(), path: "/wt/shop/feat-checkout".into(), branch: "feat/checkout".into() },
                WorktreeView { project: "p-secret".into(), path: "/wt/secret/x".into(), branch: "x".into() },
            ],
            sessions: vec![
                SessionPlace { id: APP_SESSION.into(), project: "p-app".into(), worktree: "/wt/app/feat-api".into() },
                SessionPlace { id: SHOP_SESSION.into(), project: "p-shop".into(), worktree: "/wt/shop/feat-checkout".into() },
            ],
        }
    }

    fn op(line: &[&str], person: bool) -> Op {
        let ctx = ParseCtx {
            caller_is_person: person,
            caller_session: Some(APP_SESSION.into()),
            sessions: vec![
                SessionView { id: APP_SESSION.into(), title: "API".into(), ..Default::default() },
                SessionView { id: SHOP_SESSION.into(), title: "Checkout".into(), ..Default::default() },
            ],
            ..Default::default()
        };
        parse(&line.iter().map(|w| w.to_string()).collect::<Vec<_>>(), &ctx).unwrap()
    }

    #[test]
    fn projects_by_name_id_folder_and_for_the_person_by_their_start() {
        let (person, agent) = (world(true), world(false));
        for word in ["app", "APP", "p-app", "/code/app"] {
            assert_eq!(find_project(&agent, word).as_deref(), Ok("p-app"), "{word}");
        }
        assert_eq!(find_project(&person, "sh").as_deref(), Ok("p-shop"));
        let refused = find_project(&agent, "sh").unwrap_err();
        assert!(matches!(&refused.code, ErrorCode::Ambiguous { candidates } if candidates.len() == 1 && candidates[0].id == "p-shop"), "{refused:?}");
        // Hidden projects are never named, and not listed.
        let hidden = find_project(&person, "secret").unwrap_err();
        assert_eq!((hidden.code, hidden.text.as_str()), (ErrorCode::NotFound, "No project “secret”. Projects: app, shop."));
    }

    #[test]
    fn the_project_a_command_is_for() {
        let mut w = world(true);
        // The calling session's, before the project a server session says, and before the folder.
        w.caller_project = Some("shop".into());
        w.cwd = Some("/wt/shop/feat-checkout/src".into());
        assert_eq!(project_for(&w, None).as_deref(), Ok("p-app"));
        w.caller_session = None;
        assert_eq!(project_for(&w, None).as_deref(), Ok("p-shop"));
        w.caller_project = None;
        w.cwd = Some("/code/app/src".into());
        assert_eq!(project_for(&w, None).as_deref(), Ok("p-app"));
        w.cwd = None;
        assert_eq!(project_for(&w, None).unwrap_err().text, "Say which project: --project <name> (app, shop).");
        assert_eq!(project_for(&w, Some("shop")).as_deref(), Ok("p-shop"));
    }

    #[test]
    fn worktrees_by_folder_branch_or_name() {
        let w = world(false);
        for word in ["feat/api", "feat-api", "/wt/app/feat-api", "app/feat-api/"] {
            assert_eq!(find_worktree(&w, word, None).map(|wt| wt.project.as_str()), Ok("p-app"), "{word}");
        }
        assert!(find_worktree(&w, "feat/api", Some("p-shop")).is_err());
        assert_eq!(find_worktree(&w, "x", None).unwrap_err().code, ErrorCode::NotFound);
    }

    #[test]
    fn tasks_by_id_for_everyone_and_by_title_for_the_person() {
        let mut board = Board::default();
        let today = crate::day::from_civil(2026, 11, 20);
        let origin = crate::board::Origin { who: "You".into(), session: None, session_title: None };
        let id = board.add(crate::board::Draft { title: "Renew the certificate".into(), ..Default::default() }, origin, today);
        assert_eq!(find_task(&board, &[], &id, false), Ok(format!("task:{id}")));
        assert_eq!(find_task(&board, &[], "renew the certificate", true), Ok(format!("task:{id}")));
        let refused = find_task(&board, &[], "renew the certificate", false).unwrap_err();
        assert!(matches!(&refused.code, ErrorCode::Ambiguous { candidates } if candidates[0].id == id), "{refused:?}");
        assert_eq!(find_task(&board, &[], "nothing like it", false).unwrap_err().code, ErrorCode::Failed);
    }

    /// A target is the project its session or worktree is in, never the one the caller says
    /// (trust-3): not `--project`, not the project a server session claims.
    #[test]
    fn targets_never_trust_caller_project() {
        let mut w = world(false);
        w.caller_project = Some("app".into());
        let (app, shop) = (key("app-3f9a1c").unwrap(), key("shop-3f9a1c").unwrap());
        // The caller runs in app and sends to a session in shop: the target is shop.
        let sent = targets(&op(&["session", "send", SHOP_SESSION, "hi"], false), &w).unwrap();
        assert_eq!(sent.projects, vec![shop.clone()]);
        assert_eq!(sent.sessions, vec![SHOP_SESSION.to_string()]);
        assert!(sent.paths.contains(&PathBuf::from("/wt/shop/feat-checkout")) && !sent.paths.contains(&PathBuf::from("/code/app")));
        // A worktree is in its own project, whatever project the caller is in.
        let removed = targets(&op(&["worktree", "remove", "feat/checkout"], false), &w).unwrap();
        assert_eq!(removed.projects, vec![shop.clone()]);
        // `--project` names a worktree's project only when the worktree is in it.
        assert!(targets(&op(&["session", "new", "--project", "app", "--worktree", "feat/checkout"], false), &w).is_err());
        // With no --project, the calling session's project, not the one the caller claims.
        w.caller_project = Some("shop".into());
        let started = targets(&op(&["session", "new"], false), &w).unwrap();
        assert_eq!(started.projects, vec![app.clone()]);
        // A pull request is in the repository of the project it is for, and leaves the machine.
        let commented = targets(&op(&["pr", "comment", "958", "Looks", "good", "--project", "shop"], false), &w).unwrap();
        assert_eq!((commented.projects, commented.repos, commented.leaves_machine), (vec![shop], vec!["acme/shop".to_string()], true));
        // An unknown or hidden project is no target: the op fails, as its handler does.
        assert!(targets(&op(&["pr", "comment", "958", "x", "--project", "secret"], false), &w).is_err());
        // The board is the person's own: nothing of a project.
        assert_eq!(targets(&op(&["board", "add", "x"], false), &w).unwrap(), Targets::default());
    }

    /// The typed reads of M33.2 name what they read: a repository and its project, a worktree's
    /// folder, a file and the project it is in.
    #[test]
    fn reads_name_their_repository_folder_or_file() {
        let w = world(true);
        let typed = |name: &str, args: serde_json::Value| targets(&crate::ops::typed(name, args).unwrap(), &w).unwrap();
        let checks = typed("pr.checks", serde_json::json!({ "pr": 12, "repo": "acme/shop" }));
        assert_eq!((checks.projects, checks.repos, checks.leaves_machine), (vec![key("shop-3f9a1c").unwrap()], vec!["acme/shop".to_string()], true));
        // No repository named: the calling session's project's.
        assert_eq!(typed("ci.log", serde_json::json!({ "run": 9001 })).repos, ["acme/app"]);
        let status = typed("git.status", serde_json::json!({ "worktree": "feat/checkout" }));
        assert_eq!((status.paths, status.leaves_machine), (vec![PathBuf::from("/code/shop"), PathBuf::from("/wt/shop/feat-checkout")], false));
        let file = typed("file.read", serde_json::json!({ "path": "/code/app/src/main.rs" }));
        assert_eq!((file.projects, file.paths), (vec![key("app-3f9a1c").unwrap()], vec![PathBuf::from("/code/app"), PathBuf::from("/code/app/src/main.rs")]));
        assert_eq!(typed("ui.open", serde_json::json!({ "place": APP_SESSION })).sessions, [APP_SESSION]);
    }

    /// A value of each kind of argument, for an op built in a test.
    fn sample(kind: crate::ops::ArgKind) -> serde_json::Value {
        use crate::ops::ArgKind as K;
        match kind {
            K::Switch => serde_json::json!(true),
            K::Count | K::Number | K::Pr => serde_json::json!(1),
            K::Day | K::Until => serde_json::json!("2099-01-02"),
            K::Session => serde_json::json!(APP_SESSION),
            K::Ticket => serde_json::json!("ENG-1"),
            K::List => serde_json::json!("todo"),
            K::Section | K::Movable => serde_json::json!(crate::board::Section::ALL.iter().find(|s| s.movable()).map(|s| s.key()).unwrap_or_default()),
            K::Many => serde_json::json!(["x"]),
            _ => serde_json::json!("x"),
        }
    }

    /// trust-3: an op that names a project is about that project, whichever op it is. The gate
    /// then reads that project's policy (its never list, the one account whose model may read it,
    /// a pause), not only the caller's own or the default: the manager, on another account, does
    /// not search the memory or read the journal of a project kept to one account (D17).
    #[test]
    fn every_project_an_op_names_is_a_target() {
        let w = world(false);
        let shop = key("shop-3f9a1c").unwrap();
        let naming: Vec<&crate::ops::OpSpec> = crate::ops::OPS.iter().filter(|op| op.args.iter().any(|a| a.name == "project" && a.kind == crate::ops::ArgKind::Project)).collect();
        assert!(naming.iter().any(|op| op.name == "memory.search") && naming.iter().any(|op| op.name == "journal.events"));
        let mut missed = Vec::new();
        for spec in naming {
            let mut args = serde_json::Map::new();
            for arg in spec.args.iter().filter(|a| a.required) {
                args.insert(arg.name.into(), sample(arg.kind));
            }
            args.insert("project".into(), serde_json::json!("shop"));
            let op = crate::ops::typed(spec.name, serde_json::Value::Object(args)).unwrap_or_else(|e| panic!("{}: {e:?}", spec.name));
            match targets(&op, &w) {
                Ok(found) if !found.projects.contains(&shop) => missed.push(spec.name),
                Ok(_) => {}
                // An op whose other words name nothing here fails as its handler would.
                Err(error) => assert!(matches!(error.code, ErrorCode::NotFound | ErrorCode::Usage), "{}: {error:?}", spec.name),
            }
        }
        assert!(missed.is_empty(), "these ops name a project that is not their target: {missed:?}");
        // And the gate reads shop's policy: kept to another account, the manager may not search it.
        let op = crate::ops::typed("memory.search", serde_json::json!({ "query": "renewal", "project": "shop" })).unwrap();
        let found = targets(&op, &w).unwrap();
        let mut set = crate::gate::PolicySet { manager_account: Some("claude-personal".into()), ..Default::default() };
        set.policy.projects.insert(shop.clone(), crate::gate::ProjectPolicy { model_account: Some("claude-work".into()), ..Default::default() });
        let manager = crate::caller::Caller::Manager { lane: crate::caller::Lane::Chat, wake: None, project: None };
        assert!(matches!(crate::gate::decide(&manager, &op, &found, &set, None), crate::gate::Decision::Never { .. }), "{found:?}");
    }

    /// The rule for any op that names a project never fails an op its handler would run: a hidden
    /// project named by its id or name is a target (Settings' `policy.set` and the journal name
    /// one), and a word that names no project here is the handler's to refuse.
    #[test]
    fn a_named_project_is_a_target_without_failing_the_op() {
        let w = world(true);
        let secret = key("secret-3f9a1c").unwrap();
        for (name, args) in [("policy.set", serde_json::json!({ "project": "p-secret", "level": "careful" })), ("journal.events", serde_json::json!({ "project": "secret" }))] {
            let op = crate::ops::typed(name, args).unwrap();
            let found = targets(&op, &w).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert!(found.projects.contains(&secret), "{name}: {found:?}");
        }
        let op = crate::ops::typed("journal.events", serde_json::json!({ "project": "gone" })).unwrap();
        assert!(targets(&op, &w).is_ok_and(|t| t.projects.is_empty()));
    }

    #[test]
    fn every_op_has_targets_or_fails_as_its_handler_would() {
        // Every example line of every op resolves, or fails with a usage or lookup error: never a
        // panic, and never targets for a session the world does not have.
        let w = world(true);
        for spec in crate::ops::OPS {
            let ctx = ParseCtx { caller_is_person: true, caller_session: Some(APP_SESSION.into()), v2: true, ..Default::default() };
            let words: Vec<String> = spec.cli.iter().map(|w| w.to_string()).collect();
            if let Ok(op) = parse(&words, &ctx)
                && let Ok(found) = targets(&op, &w)
            {
                assert!(found.sessions.iter().all(|s| w.sessions.iter().any(|known| known.id == *s)), "{}", spec.name);
            }
        }
    }
}
