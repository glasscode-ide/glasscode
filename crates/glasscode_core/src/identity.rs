//! Who is who for Manager v2 (design D22): which project a folder is ([`ProjectKey`], with its
//! aliases in `authority/projects.json`), which piece of work a session is on ([`WorkKey`]), and
//! the hybrid logical clock that orders what several machines write ([`Hlc`], with the
//! [`MachineId`] of `store`).
//!
//! A project key is a readable slug and a short hash, such as `glasscode-3f9a1c`: the name of the
//! project's folder in memory and in the agent notes. It is not the app's project id, which is a
//! random UUID per folder and per machine. Two clones of one repository, the same repository on the
//! Mac and on the server, and a project removed and added again have one key.
//!
//! The rules for what changes:
//! - **A changed remote** (a repository renamed or moved, a remote renamed): the checkout keeps
//!   its root commit and its folder, so it keeps its key, and the new remote becomes an alias.
//! - **A fork**: its `origin` is its own. When its `upstream` is a known project, or it shares only
//!   a root commit with one, the person is asked once ([`KeyMatch::AskFork`]) and the answer is
//!   pinned to the folder ([`Projects::pin`]). Nothing is merged without asking.
//! - **A monorepo** is one project: a project's folder is the main checkout of its repository, and a
//!   linked worktree or a subfolder resolves to it.
//! - **A bound project** keeps its key. Sessions can change a checkout's remotes and git settings,
//!   so the key of a project the app has bound is read from the binding ([`Projects::key_for`]),
//!   never resolved again from what the checkout says now. The app learns new aliases
//!   ([`Projects::attach`]) when a project is added or added again, not from a session's run; what
//!   it learns is still git state a session can write, so a remote another key has is never
//!   taken (a guard against a planted remote, not a wall).

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use crate::paths::{MachineId, ProjectKey};
use crate::model::Session;
use crate::store::{OnDamage, Store};

// ----- project keys ----------------------------------------------------------------------------

/// What makes a folder a project it was before, as [`resolve`] reads it on one machine.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Aliases {
    /// Normalized remotes of its checkouts (`github::normalize_remote`): `github.com/acme/app`.
    pub remotes: Vec<String>,
    /// Root commits of its history.
    pub roots: Vec<String>,
    /// Its folders on this machine.
    pub paths: Vec<PathBuf>,
    /// Folders the person tied to it by hand (their answer to [`KeyMatch::AskFork`]): the explicit
    /// mapping, which comes before everything else.
    pub pinned: Vec<PathBuf>,
    /// The app's project ids for it on this machine.
    pub project_ids: Vec<String>,
}

/// What [`resolve`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyMatch {
    /// A project it knows.
    Known(ProjectKey),
    /// Maybe the same project as `candidate`, maybe another: the person is asked once. `fresh` is
    /// the key it gets as a project of its own.
    AskFork { candidate: ProjectKey, fresh: ProjectKey, by: Shared },
    /// A project of its own, not seen before.
    New(ProjectKey),
}

/// What an [`KeyMatch::AskFork`] folder shares with the project it may be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shared {
    /// Its `upstream` remote is the project's, while a remote of its own is not: a fork.
    Upstream,
    /// Only a root commit: a copy, or a fork without an `upstream` remote.
    RootCommit,
    /// Only its folder, which now holds a repository with another remote and no root commit to
    /// compare (a shallow clone, or no commit yet): another repository in an old folder, or the
    /// same one renamed.
    Folder,
}

/// What a folder says about which project it is: read with git by [`facts`], and matched by
/// [`matching`] without git.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// The project's folder: the main checkout of its repository (a linked worktree and a
    /// subfolder give the same one), or the folder itself when it is not in git.
    pub path: PathBuf,
    /// Whether the folder is in git.
    pub git: bool,
    /// The folder's own remotes, normalized, strongest first: the `glasscode.repo` setting, then
    /// `origin`; without `origin`, the only remote that is not `upstream`, whatever its name.
    pub remotes: Vec<String>,
    /// The `upstream` remote, normalized: the repository a fork came from.
    pub upstream: Option<String>,
    /// Remotes that are neither: with no `origin`, several others, none of which is known to be
    /// the folder's own.
    pub others: Vec<String>,
    /// The root commits; several in a history that merged another one. None in a shallow clone,
    /// whose boundary commits are not its roots, and none before the first commit.
    pub roots: Vec<String>,
    /// A shallow clone (`git rev-parse --is-shallow-repository`).
    pub shallow: bool,
}

impl Facts {
    /// The remotes that name the project itself: its own, or, with no other remote at all, its
    /// `upstream` (a clone that kept only that one). The key is made from the first, and
    /// [`Projects::attach`] records them.
    pub fn project_remotes(&self) -> Vec<String> {
        match &self.upstream {
            Some(upstream) if self.remotes.is_empty() && self.others.is_empty() => vec![upstream.clone()],
            _ => self.remotes.clone(),
        }
    }
}

/// The key of the project in `dir`: [`facts`] and then [`matching`]. It runs git and reads the
/// disk: never on the main thread. A project the app has bound already is read with
/// [`Projects::key_for`]. An error when git could not read a repository: no key is made from it.
pub fn resolve(dir: &Path, known: &[(ProjectKey, Aliases)]) -> anyhow::Result<KeyMatch> {
    Ok(matching(&facts(dir)?, known))
}

/// What git says of `dir` (four short `git` runs; the root commits walk the whole history once).
/// A folder git says is no repository, with no `.git` in it or above it, is a plain folder. Git
/// failing on a folder that has one (git missing, too old, a repository of another owner) is an
/// error, never a plain folder: a key made from its path would be bound for good.
pub fn facts(dir: &Path) -> anyhow::Result<Facts> {
    let fail = |what: &str| -> anyhow::Error { anyhow::anyhow!("git could not read {} ({what})", dir.display()) };
    let Some(lines) = git_lines(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir", "--show-toplevel", "--is-shallow-repository"]) else {
        if dir.ancestors().any(|folder| folder.join(".git").exists()) {
            return Err(fail("it has a .git"));
        }
        return Ok(Facts { path: std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()), ..Facts::default() });
    };
    let [common, top, shallow] = lines.as_slice() else { return Err(fail("git rev-parse said something else")) };
    let common = PathBuf::from(common);
    // A linked worktree shares the main checkout's `.git`; a submodule's git folder is inside its
    // parent's, so it is its own top level.
    let path = match common.parent() {
        Some(main) if common.file_name().is_some_and(|name| name == ".git") => main.to_path_buf(),
        _ => PathBuf::from(top),
    };
    let shallow = shallow == "true";

    let mut remotes = Vec::new();
    if let Some(repo) = crate::github::repo_override(dir) {
        remotes.push(format!("github.com/{}", repo.to_lowercase()));
    }
    // `git remote -v` gives the URLs with the person's `insteadOf` rewrites applied; a partial
    // clone's line ends in its filter (`(fetch) [blob:none]`).
    let listed: Vec<(String, String)> = git_lines(dir, &["remote", "-v"])
        .ok_or_else(|| fail("git remote -v failed"))?
        .iter()
        .filter_map(|line| {
            let (name, rest) = line.split_once('\t')?;
            let (url, _) = rest.split_once(" (fetch)")?;
            Some((name.to_string(), crate::github::normalize_remote(url)?))
        })
        .collect();
    let upstream = listed.iter().find(|(name, _)| name == "upstream").map(|(_, url)| url.clone());
    let origin = listed.iter().find(|(name, _)| name == "origin").map(|(_, url)| url.clone());
    let mut others: Vec<String> = Vec::new();
    match origin {
        Some(origin) => remotes.push(origin),
        None => {
            // A remote renamed from `origin` is still the folder's own when it is the only one.
            let rest: Vec<&String> = listed.iter().filter(|(name, _)| name != "upstream").map(|(_, url)| url).collect();
            match rest.as_slice() {
                [own] => remotes.push((*own).clone()),
                several => others.extend(several.iter().map(|url| (*url).clone())),
            }
        }
    }
    remotes.dedup();
    let upstream = upstream.filter(|upstream| !remotes.contains(upstream));
    others.retain(|other| !remotes.contains(other) && Some(other) != upstream.as_ref());
    others.dedup();

    let mut roots = if shallow { Vec::new() } else { git_lines(dir, &["rev-list", "--max-parents=0", "HEAD"]).unwrap_or_default() };
    roots.sort();
    roots.dedup();
    Ok(Facts { path, git: true, remotes, upstream, others, roots, shallow })
}

/// The lines `git` prints in `dir`, or None when it fails.
fn git_lines(dir: &Path, args: &[&str]) -> Option<Vec<String>> {
    let output = crate::sys::command("git").arg("-C").arg(dir).args(args).env("GIT_TERMINAL_PROMPT", "0").env("GIT_OPTIONAL_LOCKS", "0").output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).lines().map(str::trim).filter(|line| !line.is_empty()).map(String::from).collect())
}

/// The key of a folder by its facts, checking the aliases of `known` in this order:
/// 1. a folder the person pinned (the explicit mapping), unless the folder now holds another
///    repository (see 5);
/// 2. the folder's own remotes, `glasscode.repo` and then `origin`;
/// 3. its `upstream`: a fork, so the person is asked, unless the folder has no other remote;
/// 4. a root commit: known when the project also lists the folder (its remote changed), else asked;
/// 5. the folder, unless it holds another repository now: known, but not when both have root
///    commits and none in common (another history), and asked when both have remotes and share
///    neither a remote nor a root commit (no root commit to tell them apart).
///
/// When two keys match at one step, the first in `known` wins: on the server, the Mac's map comes
/// first. It runs no git, but it resolves the folders it compares (`paths::folder_form`): never on
/// the main thread.
pub fn matching(facts: &Facts, known: &[(ProjectKey, Aliases)]) -> KeyMatch {
    let folder = crate::paths::folder_form(&facts.path);
    let here = |folders: &[PathBuf]| folders.iter().any(|other| crate::paths::folder_form(other) == folder);
    let listed = |aliases: &Aliases| here(&aliases.paths) || here(&aliases.pinned);
    let shares_root = |aliases: &Aliases| facts.roots.iter().any(|root| aliases.roots.contains(root));
    let shares_remote = |aliases: &Aliases| facts.project_remotes().iter().any(|remote| aliases.remotes.contains(remote));
    // The folder holds another repository than the project's.
    let other_history = |aliases: &Aliases| !facts.roots.is_empty() && !aliases.roots.is_empty() && !shares_root(aliases);
    let other_remote = |aliases: &Aliases| !facts.project_remotes().is_empty() && !aliases.remotes.is_empty() && !shares_remote(aliases) && !shares_root(aliases);
    let fresh = || fresh_key(facts, known);

    if let Some((key, _)) = known.iter().find(|(_, aliases)| here(&aliases.pinned) && !other_history(aliases) && !other_remote(aliases)) {
        return KeyMatch::Known(key.clone());
    }
    for remote in &facts.remotes {
        if let Some((key, _)) = known.iter().find(|(_, aliases)| aliases.remotes.contains(remote)) {
            return KeyMatch::Known(key.clone());
        }
    }
    if let Some(upstream) = &facts.upstream
        && let Some((key, _)) = known.iter().find(|(_, aliases)| aliases.remotes.contains(upstream))
    {
        if facts.remotes.is_empty() && facts.others.is_empty() {
            return KeyMatch::Known(key.clone());
        }
        return KeyMatch::AskFork { candidate: key.clone(), fresh: fresh(), by: Shared::Upstream };
    }
    let by_root: Vec<&(ProjectKey, Aliases)> = known.iter().filter(|(_, aliases)| shares_root(aliases)).collect();
    if let Some((key, _)) = by_root.iter().find(|(_, aliases)| listed(aliases)) {
        return KeyMatch::Known(key.clone());
    }
    if let Some((key, _)) = by_root.first() {
        return KeyMatch::AskFork { candidate: key.clone(), fresh: fresh(), by: Shared::RootCommit };
    }
    if let Some((key, aliases)) = known.iter().find(|(_, aliases)| listed(aliases) && !other_history(aliases)) {
        if other_remote(aliases) {
            return KeyMatch::AskFork { candidate: key.clone(), fresh: fresh(), by: Shared::Folder };
        }
        return KeyMatch::Known(key.clone());
    }
    KeyMatch::New(fresh())
}

/// The key a folder gets as a project of its own: the slug of its repository's name (or of the
/// folder's, without a remote) and six hex digits of its first alias, the first of
/// [`Facts::project_remotes`] or else its folder. More digits when that key is another project's
/// already.
fn fresh_key(facts: &Facts, known: &[(ProjectKey, Aliases)]) -> ProjectKey {
    let (name, alias) = match facts.project_remotes().first() {
        Some(remote) => (remote.rsplit('/').next().unwrap_or(remote).to_string(), remote.clone()),
        None => (facts.path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default(), facts.path.to_string_lossy().into_owned()),
    };
    key_for(&name, &alias, |key| known.iter().any(|(other, _)| other == key))
}

/// `<slug>-<hex>`: a safe folder name for any name (`paths::ProjectKey`'s rule), with as many hex
/// digits of `alias`'s hash, from six, as it takes to be free of `taken`.
fn key_for(name: &str, alias: &str, taken: impl Fn(&ProjectKey) -> bool) -> ProjectKey {
    let hash = hex(&Sha256::digest(alias.as_bytes()));
    let mut fallback = None;
    for digits in (6..=32).step_by(2) {
        let text = format!("{}-{}", slug(name, 64 - 1 - digits, "project"), &hash[..digits]);
        let key = ProjectKey::parse(&text).expect("a slug and hex digits are a folder name");
        if !taken(&key) {
            return key;
        }
        fallback.get_or_insert(key);
    }
    fallback.expect("at least one key was made")
}

/// A name in `a-z`, `0-9` and single dashes, at most `max` long, never starting or ending with a
/// dash; `empty` when nothing of it is left. Dots go too: `con.app` is not a device name of
/// Windows, and no name ends in a dot.
fn slug(name: &str, max: usize, empty: &str) -> String {
    let mut out = String::new();
    for c in name.chars().map(|c| c.to_ascii_lowercase()) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(max);
    let out = out.trim_end_matches('-');
    if out.is_empty() { empty.to_string() } else { out.to_string() }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ----- authority/projects.json -----------------------------------------------------------------

/// `authority/projects.json`: every project key the person's machines have used, with its aliases
/// and, per machine, its folders and the app's project ids there. A removed project keeps its key,
/// so adding it again finds it. Written by the app only; it flows from the Mac to the server (M57),
/// where glasscoded reads it as its own machine's view. It fails closed: a wrong key must never
/// come from an older generation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Projects {
    pub keys: BTreeMap<ProjectKey, Entry>,
}

/// One project key in [`Projects`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    /// The normalized remotes of its checkouts, on any machine.
    pub remotes: Vec<String>,
    pub roots: Vec<String>,
    pub machines: BTreeMap<MachineId, OnMachine>,
}

/// What one machine knows of a project key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OnMachine {
    pub paths: Vec<PathBuf>,
    /// Folders the person tied to the key ([`Projects::pin`]).
    pub pinned: Vec<PathBuf>,
    pub project_ids: Vec<String>,
}

impl Store for Projects {
    const NAME: &'static str = "authority/projects";
    const VERSION: u32 = 1;
    const ON_DAMAGE: OnDamage = OnDamage::Refuse;
    // A key from an older generation would put a project's memory in another's folder.
    const FALLS_BACK: bool = false;

    fn path() -> PathBuf {
        crate::paths::authority_dir().join("projects.json")
    }
}

impl Projects {
    /// The keys as [`resolve`] reads them on `machine`: the remotes and roots of every machine,
    /// and the folders and project ids of this one only (a Mac's folder means nothing on the server).
    pub fn known(&self, machine: &MachineId) -> Vec<(ProjectKey, Aliases)> {
        self.keys
            .iter()
            .map(|(key, entry)| {
                let local = entry.machines.get(machine).cloned().unwrap_or_default();
                let aliases = Aliases { remotes: entry.remotes.clone(), roots: entry.roots.clone(), paths: local.paths, pinned: local.pinned, project_ids: local.project_ids };
                (key.clone(), aliases)
            })
            .collect()
    }

    /// The key bound to the app's project `project_id` on `machine`.
    pub fn key_of(&self, machine: &MachineId, project_id: &str) -> Option<&ProjectKey> {
        self.keys.iter().find(|(_, entry)| entry.machines.get(machine).is_some_and(|local| local.project_ids.iter().any(|id| id == project_id))).map(|(key, _)| key)
    }

    /// The key of a project of the app: its binding when there is one, else what [`resolve`] finds
    /// in its folder. It runs git when there is no binding: never on the main thread.
    pub fn key_for(&self, machine: &MachineId, project_id: &str, dir: &Path) -> anyhow::Result<KeyMatch> {
        match self.key_of(machine, project_id) {
            Some(key) => Ok(KeyMatch::Known(key.clone())),
            None => resolve(dir, &self.known(machine)),
        }
    }

    /// Record a folder's facts as aliases of `key` (after [`KeyMatch::Known`] or
    /// [`KeyMatch::New`]), and bind the project id to it: its [`Facts::project_remotes`] (so the
    /// alias a key was made from is one of it), its root commits and its folder. Aliases are only
    /// added, so a renamed remote is learnt; a remote that another key has stays that key's alone,
    /// so a remote set in a bound project's checkout never takes another project's clones. Root
    /// commits may be shared (a fork the person called a project of its own). The folder and the
    /// project id leave any other key of this machine, as they are one project at a time.
    pub fn attach(&mut self, key: &ProjectKey, facts: &Facts, machine: &MachineId, project_id: Option<&str>) {
        let remotes: Vec<String> = facts.project_remotes().into_iter().filter(|remote| !self.keys.iter().any(|(other, entry)| other != key && entry.remotes.contains(remote))).collect();
        for (_, entry) in self.keys.iter_mut().filter(|(other, _)| *other != key) {
            if let Some(local) = entry.machines.get_mut(machine) {
                local.paths.retain(|path| !crate::paths::same_folder(path, &facts.path));
                local.pinned.retain(|path| !crate::paths::same_folder(path, &facts.path));
                local.project_ids.retain(|id| Some(id.as_str()) != project_id);
            }
        }
        let entry = self.keys.entry(key.clone()).or_default();
        for remote in remotes {
            add(&mut entry.remotes, remote);
        }
        for root in &facts.roots {
            add(&mut entry.roots, root.clone());
        }
        let local = entry.machines.entry(machine.clone()).or_default();
        if !local.paths.iter().any(|path| crate::paths::same_folder(path, &facts.path)) {
            local.paths.push(facts.path.clone());
        }
        if let Some(id) = project_id {
            add(&mut local.project_ids, id.to_string());
        }
    }

    /// The person's answer to [`KeyMatch::AskFork`]: the folder is `key` (the candidate, or the
    /// fresh key), attached and pinned to it, so it is not asked again.
    pub fn pin(&mut self, key: &ProjectKey, facts: &Facts, machine: &MachineId, project_id: Option<&str>) {
        self.attach(key, facts, machine, project_id);
        let local = self.keys.get_mut(key).and_then(|entry| entry.machines.get_mut(machine)).expect("attach made it");
        if !local.pinned.iter().any(|path| crate::paths::same_folder(path, &facts.path)) {
            local.pinned.push(facts.path.clone());
        }
    }
}

fn add<T: PartialEq>(list: &mut Vec<T>, item: T) {
    if !list.contains(&item) {
        list.push(item);
    }
}

// ----- work keys -------------------------------------------------------------------------------

/// What a piece of work is named by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkKind {
    Branch,
    Ticket,
    Pr,
    Review,
}

/// One piece of work of a project: a branch, a ticket, a pull request carried on with, or one
/// reviewed. Its `id` is `feat/login`, `ENG-42` or `acme/app#42`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WorkKey {
    pub project: ProjectKey,
    pub kind: WorkKind,
    pub id: String,
}

impl WorkKey {
    /// The work of a session, by what it was started from, in this order: the pull request a review
    /// session reviews (`Review(acme/app#42)`); the pull request it was started to carry on with,
    /// `pr` (`Pr(acme/app#42)`); the ticket it was started from, `ticket` (`State::ticket_of`,
    /// `Ticket(ENG-42)`); else its branch (`Branch(feat/login)`). A pull request the session opens
    /// later is not `pr`: the work keeps its key, and the pull request is linked to it (M32).
    pub fn for_session(project: &ProjectKey, session: &Session, ticket: Option<&str>, pr: Option<(&str, u64)>) -> WorkKey {
        let key = |kind, id| WorkKey { project: project.clone(), kind, id };
        let pull = |repo: &str, number: u64| format!("{}#{number}", repo.trim().to_lowercase());
        if let Some(review) = &session.review {
            return key(WorkKind::Review, pull(&review.repo, review.number));
        }
        if let Some((repo, number)) = pr {
            return key(WorkKind::Pr, pull(repo, number));
        }
        match ticket.map(str::trim).filter(|ticket| !ticket.is_empty()) {
            Some(ticket) => key(WorkKind::Ticket, ticket.to_uppercase()),
            None => key(WorkKind::Branch, session.branch.clone()),
        }
    }

    /// The name of its file (`work/<name>.md` in memory, M20.1): a lower-case slug of the id and six
    /// hex digits of the exact id, so that `feat/x` and `feat-x`, and `Feat` and `feat`, are two
    /// files on every disk. The id itself is written inside the file.
    pub fn file_name(&self) -> String {
        let (kind, prefix) = match self.kind {
            WorkKind::Branch => ("branch", ""),
            WorkKind::Ticket => ("ticket", "ticket-"),
            WorkKind::Pr => ("pr", "pr-"),
            WorkKind::Review => ("review", "review-"),
        };
        let hash = hex(&Sha256::digest(format!("{kind}:{}", self.id).as_bytes()));
        format!("{}-{}", slug(&format!("{prefix}{}", self.id), 40, "work"), &hash[..6])
    }
}

// ----- the hybrid logical clock ----------------------------------------------------------------

/// A stamp of the hybrid logical clock: what orders the records several machines write (memory
/// entries, work items, conversation items), never their wall times. Each local write takes
/// [`Hlc::now`] of the last stamp, and each record received is [`Hlc::merge`]d, so a write made
/// after seeing another's comes after it, whatever the two clocks say. Stamps order by wall time,
/// then count, then machine, so every machine orders two stamps alike.
///
/// **A clock that goes back** does not take the stamps back: they stay at the last wall time and
/// count up, so the order is kept. When it went back by more than [`MAX_AHEAD_MS`], this machine's
/// stamps are [`Hlc::too_far_ahead`] of its own clock, and of every peer whose clock is right: the
/// peers quarantine its records, and this machine can tell by asking the same of its own last
/// stamp. That lasts until its clock passes the stamp again; nothing here shortens it (M57 shows it,
/// and decides any way out).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Hlc {
    /// Milliseconds since the Unix epoch (UTC), from `clock`: the largest this machine has seen.
    pub wall_ms: u64,
    /// Stamps at the same `wall_ms`.
    pub count: u32,
    pub machine: MachineId,
}

/// How far ahead of this machine's clock a stamp may be: a record stamped later is quarantined
/// and shown ([`Hlc::too_far_ahead`]), and [`Hlc::merge`] does not take its time. An hour lets a
/// clock that is minutes off sync (a server ten minutes ahead), and keeps one set to another year
/// from carrying every later stamp with it.
pub const MAX_AHEAD_MS: u64 = 60 * 60 * 1000;

impl Hlc {
    /// The stamp before any stamp of `machine`: where its clock starts.
    pub fn zero(machine: MachineId) -> Hlc {
        Hlc { wall_ms: 0, count: 0, machine }
    }

    /// The stamp of a local write after `last` (this machine's last stamp): later than it even when
    /// the clock went back.
    pub fn now(last: &Hlc) -> Hlc {
        last.after(crate::clock::now_ms(), None)
    }

    /// This machine's stamp on receiving `seen`: later than both. A stamp more than
    /// [`MAX_AHEAD_MS`] ahead of this machine's clock is not taken in; its record is quarantined.
    pub fn merge(&self, seen: &Hlc) -> Hlc {
        let now = crate::clock::now_ms();
        let seen = (!seen.ahead_of(now)).then_some(seen);
        self.after(now, seen)
    }

    /// Whether the stamp is more than [`MAX_AHEAD_MS`] ahead of this machine's clock: its record
    /// is quarantined and shown, not merged.
    pub fn too_far_ahead(&self) -> bool {
        self.ahead_of(crate::clock::now_ms())
    }

    fn ahead_of(&self, now: u64) -> bool {
        self.wall_ms > now.saturating_add(MAX_AHEAD_MS)
    }

    /// The next stamp of this machine at clock time `now`, after `self` and `seen`.
    fn after(&self, now: u64, seen: Option<&Hlc>) -> Hlc {
        let wall = self.wall_ms.max(now).max(seen.map_or(0, |seen| seen.wall_ms));
        let at_wall = seen.filter(|seen| seen.wall_ms == wall).map(|seen| seen.count);
        let count = match (wall == self.wall_ms, at_wall) {
            (true, Some(theirs)) => self.count.max(theirs).checked_add(1),
            (true, None) => self.count.checked_add(1),
            (false, Some(theirs)) => theirs.checked_add(1),
            (false, None) => Some(0),
        };
        match count {
            Some(count) => Hlc { wall_ms: wall, count, machine: self.machine.clone() },
            // Four billion stamps in one millisecond: the next millisecond.
            None => Hlc { wall_ms: wall.saturating_add(1), count: 0, machine: self.machine.clone() },
        }
    }
}

impl Ord for Hlc {
    fn cmp(&self, other: &Hlc) -> Ordering {
        (self.wall_ms, self.count, &self.machine).cmp(&(other.wall_ms, other.count, &other.machine))
    }
}

impl PartialOrd for Hlc {
    fn partial_cmp(&self, other: &Hlc) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// `1763560000000.3@0123456789abcdef`: one word, for headers such as memory's `at=<hlc>`.
impl std::fmt::Display for Hlc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}@{}", self.wall_ms, self.count, self.machine)
    }
}

impl std::str::FromStr for Hlc {
    type Err = String;

    fn from_str(text: &str) -> Result<Hlc, String> {
        let bad = || format!("{text:?} is not a clock stamp");
        let (time, machine) = text.split_once('@').ok_or_else(bad)?;
        let (wall, count) = time.split_once('.').ok_or_else(bad)?;
        let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        if !digits(wall) || !digits(count) {
            return Err(bad());
        }
        Ok(Hlc { wall_ms: wall.parse().map_err(|_| bad())?, count: count.parse().map_err(|_| bad())?, machine: MachineId::parse(machine).ok_or_else(bad)? })
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, AtomicU64, Ordering as Atomic};

    use super::*;
    use crate::clock::Clock;

    /// One test's scratch place: a Glasscode folder of its own, and git that reads none of the
    /// person's settings (an `insteadOf` there would change the remotes) and finds no repository
    /// above it. Everything goes back when it is dropped.
    struct Scratch {
        dir: tempfile::TempDir,
        before: Vec<(&'static str, Option<OsString>)>,
    }

    impl Scratch {
        fn new() -> Scratch {
            let dir = tempfile::tempdir().unwrap();
            let config = dir.path().join("gitconfig");
            std::fs::write(&config, "").unwrap();
            let home = dir.path().join("glasscode");
            std::fs::create_dir(&home).unwrap();
            let set: [(&'static str, OsString); 4] = [
                ("GLASSCODE_HOME", home.into()),
                ("GIT_CONFIG_GLOBAL", config.into()),
                ("GIT_CONFIG_NOSYSTEM", "1".into()),
                ("GIT_CEILING_DIRECTORIES", dir.path().into()),
            ];
            let before = set.iter().map(|(name, _)| (*name, std::env::var_os(name))).collect();
            for (name, value) in &set {
                // SAFETY: the core tests run one at a time (`--test-threads=1`).
                unsafe { std::env::set_var(name, value) };
            }
            Scratch { dir, before }
        }

        fn at(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            for (name, value) in self.before.drain(..) {
                // SAFETY: as in `new`.
                match value {
                    Some(value) => unsafe { std::env::set_var(name, value) },
                    None => unsafe { std::env::remove_var(name) },
                }
            }
        }
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = crate::sys::command("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?} in {}: {}", dir.display(), String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// A repository with two commits, and `origin` when given. Its history is its own: the first
    /// commit is numbered (two empty commits made in one second are otherwise one commit).
    fn repo(at: &Path, origin: Option<&str>) {
        static MADE: AtomicU64 = AtomicU64::new(0);
        std::fs::create_dir_all(at).unwrap();
        git(at, &["init", "-q"]);
        git(at, &["commit", "-q", "--allow-empty", "-m", &format!("one, repository {}", MADE.fetch_add(1, Atomic::SeqCst))]);
        git(at, &["commit", "-q", "--allow-empty", "-m", "two"]);
        if let Some(url) = origin {
            git(at, &["remote", "add", "origin", url]);
        }
    }

    /// A clone of `from` (no network: a local clone), with `origin` set to `url`, or removed.
    fn clone(from: &Path, to: &Path, origin: Option<&str>) {
        let parent = to.parent().unwrap();
        std::fs::create_dir_all(parent).unwrap();
        git(parent, &["clone", "-q", &from.to_string_lossy(), &to.to_string_lossy()]);
        match origin {
            Some(url) => git(to, &["remote", "set-url", "origin", url]),
            None => git(to, &["remote", "remove", "origin"]),
        };
    }

    /// The tests' repositories are all readable: the errors are tested on their own.
    fn facts(dir: &Path) -> Facts {
        super::facts(dir).unwrap()
    }

    fn resolve(dir: &Path, known: &[(ProjectKey, Aliases)]) -> KeyMatch {
        super::resolve(dir, known).unwrap()
    }

    fn mac() -> MachineId {
        MachineId::parse("0123456789abcdef").unwrap()
    }

    fn server() -> MachineId {
        MachineId::parse("fedcba9876543210").unwrap()
    }

    /// What the app does when a project is added (or added again) on `machine`: its binding, or
    /// what the folder resolves to, attached with its project id unless the person must be asked
    /// first.
    fn add(projects: &mut Projects, machine: &MachineId, project_id: &str, dir: &Path) -> KeyMatch {
        let facts = facts(dir);
        let found = match projects.key_of(machine, project_id) {
            Some(key) => KeyMatch::Known(key.clone()),
            None => matching(&facts, &projects.known(machine)),
        };
        if let KeyMatch::Known(key) | KeyMatch::New(key) = &found {
            projects.attach(key, &facts, machine, Some(project_id));
        }
        found
    }

    fn new_key(found: &KeyMatch) -> ProjectKey {
        match found {
            KeyMatch::New(key) => key.clone(),
            other => panic!("not new: {other:?}"),
        }
    }

    #[test]
    fn two_clones_of_one_repository_give_one_key() {
        let scratch = Scratch::new();
        let (a, b) = (scratch.at("a").join("app"), scratch.at("b").join("app-copy"));
        repo(&a, Some("git@github.com:acme/app.git"));
        clone(&a, &b, Some("https://github.com/Acme/App.git"));
        let mut projects = Projects::default();
        let key = new_key(&add(&mut projects, &mac(), "p1", &a));
        assert!(key.as_str().starts_with("app-") && key.as_str().len() == "app-".len() + 6, "{key}");
        assert_eq!(add(&mut projects, &mac(), "p2", &b), KeyMatch::Known(key.clone()));
        // A linked worktree and a subfolder are the main checkout's project.
        let worktree = scratch.at("worktrees").join("feature");
        git(&b, &["worktree", "add", "-q", &worktree.to_string_lossy(), "-b", "feature"]);
        std::fs::create_dir_all(b.join("src").join("deep")).unwrap();
        for dir in [worktree.clone(), b.join("src").join("deep")] {
            let found = facts(&dir);
            assert!(crate::paths::same_folder(&found.path, &b), "{}: {}", dir.display(), found.path.display());
            assert_eq!(matching(&found, &projects.known(&mac())), KeyMatch::Known(key.clone()), "{}", dir.display());
        }
        // One remote, two folders and two ids, kept through a save.
        let entry = &projects.keys[&key];
        assert_eq!((entry.remotes.as_slice(), entry.machines[&mac()].paths.len(), entry.machines[&mac()].project_ids.len()), (["github.com/acme/app".to_string()].as_slice(), 2, 2));
        projects.save().unwrap();
        let loaded = Projects::load().unwrap();
        assert_eq!(loaded, projects);
        assert_eq!((loaded.key_of(&mac(), "p1"), loaded.key_of(&mac(), "p2")), (Some(&key), Some(&key)));
    }

    #[test]
    fn a_fork_plus_its_upstream_gives_ask_fork() {
        let scratch = Scratch::new();
        let upstream = scratch.at("upstream").join("app");
        repo(&upstream, Some("git@github.com:acme/app.git"));
        let mut projects = Projects::default();
        let key = new_key(&add(&mut projects, &mac(), "p1", &upstream));
        let fork = scratch.at("fork").join("app");
        clone(&upstream, &fork, Some("git@github.com:alex/app.git"));
        git(&fork, &["remote", "add", "upstream", "https://github.com/acme/app"]);
        let facts_of_fork = facts(&fork);
        assert_eq!((facts_of_fork.remotes.as_slice(), facts_of_fork.upstream.as_deref()), (["github.com/alex/app".to_string()].as_slice(), Some("github.com/acme/app")));
        let KeyMatch::AskFork { candidate, fresh, by } = matching(&facts_of_fork, &projects.known(&mac())) else { panic!("a fork is asked about") };
        assert_eq!((candidate, by), (key.clone(), Shared::Upstream));
        assert_ne!(fresh, key);
        assert!(fresh.as_str().starts_with("app-"), "{fresh}");

        // Asked once: "a project of its own" is pinned, and not asked again.
        projects.pin(&fresh, &facts_of_fork, &mac(), Some("p2"));
        assert_eq!(resolve(&fork, &projects.known(&mac())), KeyMatch::Known(fresh.clone()));
        // The fork did not take the upstream's remote: a clone of the upstream is the upstream.
        let direct = scratch.at("direct").join("app");
        clone(&upstream, &direct, Some("git@github.com:acme/app.git"));
        assert_eq!(resolve(&direct, &projects.known(&mac())), KeyMatch::Known(key.clone()));

        // Another fork, answered "the same project": pinned to the upstream's key, and its remote
        // is an alias of it.
        let second = scratch.at("second").join("app");
        clone(&upstream, &second, Some("git@github.com:sam/app.git"));
        git(&second, &["remote", "add", "upstream", "git@github.com:acme/app.git"]);
        assert!(matches!(resolve(&second, &projects.known(&mac())), KeyMatch::AskFork { by: Shared::Upstream, .. }));
        projects.pin(&key, &facts(&second), &mac(), Some("p3"));
        assert_eq!(resolve(&second, &projects.known(&mac())), KeyMatch::Known(key.clone()));
        assert!(projects.keys[&key].remotes.contains(&"github.com/sam/app".to_string()));

        // With no remote of its own, the upstream is the project.
        let only_upstream = scratch.at("only-upstream").join("app");
        clone(&upstream, &only_upstream, None);
        git(&only_upstream, &["remote", "add", "upstream", "git@github.com:acme/app.git"]);
        assert_eq!(resolve(&only_upstream, &projects.known(&mac())), KeyMatch::Known(key));
    }

    #[test]
    fn a_renamed_remote_and_an_ssh_alias_give_one_key() {
        let scratch = Scratch::new();
        let main = scratch.at("main").join("app");
        repo(&main, Some("git@github.com:acme/app.git"));
        let mut projects = Projects::default();
        let key = new_key(&add(&mut projects, &mac(), "p1", &main));
        // A per-account SSH alias.
        let work = scratch.at("work").join("app");
        clone(&main, &work, Some("git@github.com-work:acme/app.git"));
        assert_eq!(resolve(&work, &projects.known(&mac())), KeyMatch::Known(key.clone()));
        // `origin` renamed: the only remote is still the folder's own.
        let renamed = scratch.at("renamed").join("app");
        clone(&main, &renamed, Some("ssh://git@gh-work/acme/app"));
        git(&renamed, &["remote", "rename", "origin", "github"]);
        assert_eq!(resolve(&renamed, &projects.known(&mac())), KeyMatch::Known(key.clone()));
        // The repository renamed on GitHub: the checkout keeps its root commit and its folder, so its
        // key, and the new remote is learnt.
        git(&main, &["remote", "set-url", "origin", "git@github.com:acme/app-two.git"]);
        assert_eq!(add(&mut projects, &mac(), "p1", &main), KeyMatch::Known(key.clone()));
        assert_eq!(projects.keys[&key].remotes, ["github.com/acme/app", "github.com/acme/app-two"]);
        // A new clone of the renamed repository, in a folder never seen: by its remote.
        let fresh = scratch.at("fresh").join("app-two");
        clone(&main, &fresh, Some("https://github.com/acme/app-two.git"));
        assert_eq!(resolve(&fresh, &projects.known(&mac())), KeyMatch::Known(key));
    }

    #[test]
    fn no_remote_keys_by_path() {
        let scratch = Scratch::new();
        let notes = scratch.at("work").join("notes-app");
        repo(&notes, None);
        let mut projects = Projects::default();
        let found = facts(&notes);
        assert!(found.git && found.remotes.is_empty() && found.upstream.is_none() && found.roots.len() == 1, "{found:?}");
        let key = new_key(&add(&mut projects, &mac(), "p1", &notes));
        // The slug of the folder, and the hash of the folder.
        let expected = key_for("notes-app", &found.path.to_string_lossy(), |_| false);
        assert_eq!(key, expected);
        assert_eq!(resolve(&notes, &projects.known(&mac())), KeyMatch::Known(key.clone()));
        // Another repository without a remote is another project, even with the same name.
        let other = scratch.at("elsewhere").join("notes-app");
        repo(&other, None);
        let other_key = new_key(&resolve(&other, &projects.known(&mac())));
        assert_ne!(other_key, key);
        assert!(other_key.as_str().starts_with("notes-app-"));
        // A copy of it elsewhere shares only its root commit: asked, once.
        let copy = scratch.at("copy").join("notes-app");
        clone(&notes, &copy, None);
        let KeyMatch::AskFork { candidate, fresh, by: Shared::RootCommit } = resolve(&copy, &projects.known(&mac())) else { panic!("a copy is asked about") };
        assert_eq!(candidate, key);
        projects.pin(&fresh, &facts(&copy), &mac(), Some("p2"));
        assert_eq!(resolve(&copy, &projects.known(&mac())), KeyMatch::Known(fresh.clone()));
        assert_eq!(resolve(&notes, &projects.known(&mac())), KeyMatch::Known(key.clone()), "the first is still itself");
        // A second copy, answered "the same project".
        let again = scratch.at("again").join("notes-app");
        clone(&notes, &again, None);
        assert!(matches!(resolve(&again, &projects.known(&mac())), KeyMatch::AskFork { by: Shared::RootCommit, .. }));
        projects.pin(&key, &facts(&again), &mac(), Some("p3"));
        assert_eq!(resolve(&again, &projects.known(&mac())), KeyMatch::Known(key.clone()));
        // Another history in its folder is another project.
        std::fs::remove_dir_all(&notes).unwrap();
        repo(&notes, None);
        assert!(matches!(resolve(&notes, &projects.known(&mac())), KeyMatch::New(new) if new != key));
    }

    #[test]
    fn a_shallow_clone_skips_the_root_commit() {
        let scratch = Scratch::new();
        let main = scratch.at("main").join("app");
        repo(&main, None);
        let mut projects = Projects::default();
        let key = new_key(&add(&mut projects, &mac(), "p1", &main));
        let root = facts(&main).roots;
        assert_eq!(root.len(), 1);
        // A whole clone shares the root commit: asked.
        let whole = scratch.at("whole").join("app");
        clone(&main, &whole, None);
        assert_eq!(facts(&whole).roots, root);
        assert!(matches!(resolve(&whole, &projects.known(&mac())), KeyMatch::AskFork { by: Shared::RootCommit, .. }));
        // A shallow clone's first commit is its boundary, not the root: not an alias.
        let shallow = scratch.at("shallow").join("app");
        let url = url::Url::from_file_path(&main).unwrap().to_string();
        std::fs::create_dir(scratch.at("shallow")).unwrap();
        git(&scratch.at("shallow"), &["clone", "-q", "--depth", "1", &url, &shallow.to_string_lossy()]);
        git(&shallow, &["remote", "remove", "origin"]);
        assert_eq!(git(&shallow, &["rev-parse", "--is-shallow-repository"]), "true");
        assert_ne!(git(&shallow, &["rev-list", "--max-parents=0", "HEAD"]), root[0], "the boundary commit");
        let found = facts(&shallow);
        assert!(found.shallow && found.roots.is_empty(), "{found:?}");
        assert!(matches!(matching(&found, &projects.known(&mac())), KeyMatch::New(new) if new != key));
        projects.attach(&new_key(&matching(&found, &projects.known(&mac()))), &found, &mac(), Some("p2"));
        assert!(projects.keys.values().all(|entry| entry.roots == root || entry.roots.is_empty()), "no boundary commit is kept");
    }

    /// A history that merged another one has several roots, and each is an alias.
    #[test]
    fn several_root_commits_are_all_aliases() {
        let scratch = Scratch::new();
        let (main, other) = (scratch.at("main").join("app"), scratch.at("other").join("lib"));
        repo(&main, None);
        repo(&other, None);
        let other_root = facts(&other).roots;
        git(&main, &["pull", "-q", "--no-rebase", "--allow-unrelated-histories", "--no-edit", &other.to_string_lossy(), "main"]);
        let found = facts(&main);
        assert_eq!(found.roots.len(), 2, "{found:?}");
        assert!(found.roots.contains(&other_root[0]));
        let mut projects = Projects::default();
        let key = new_key(&add(&mut projects, &mac(), "p1", &main));
        assert_eq!(projects.keys[&key].roots.len(), 2);
        assert!(matches!(resolve(&other, &projects.known(&mac())), KeyMatch::AskFork { candidate, by: Shared::RootCommit, .. } if candidate == key));
    }

    #[test]
    fn a_non_git_folder_keys_by_path() {
        let scratch = Scratch::new();
        let folder = scratch.at("Notes");
        std::fs::create_dir(&folder).unwrap();
        let found = facts(&folder);
        assert!(!found.git && found.remotes.is_empty() && found.roots.is_empty(), "{found:?}");
        assert!(crate::paths::same_folder(&found.path, &folder));
        let mut projects = Projects::default();
        let key = new_key(&add(&mut projects, &mac(), "p1", &folder));
        assert!(key.as_str().starts_with("notes-"), "{key}");
        assert_eq!(resolve(&folder, &projects.known(&mac())), KeyMatch::Known(key.clone()));
        let other = scratch.at("notes");
        if !crate::paths::same_folder(&other, &folder) {
            std::fs::create_dir(&other).unwrap();
            assert!(matches!(resolve(&other, &projects.known(&mac())), KeyMatch::New(new) if new != key));
        }
        // "Set up git" later: the same folder, so the same project, and its root is learnt.
        repo(&folder, None);
        assert_eq!(add(&mut projects, &mac(), "p1", &folder), KeyMatch::Known(key.clone()));
        assert_eq!(projects.keys[&key].roots.len(), 1);
    }

    #[test]
    fn a_re_added_project_gets_its_old_key() {
        let scratch = Scratch::new();
        let (app, plain) = (scratch.at("app"), scratch.at("plain"));
        repo(&app, Some("git@github.com:acme/app.git"));
        std::fs::create_dir(&plain).unwrap();
        let mut projects = Projects::default();
        let key = new_key(&add(&mut projects, &mac(), "p1", &app));
        let plain_key = new_key(&add(&mut projects, &mac(), "p2", &plain));
        projects.save().unwrap();
        // Removed from Glasscode: nothing of the map changes. Added again with new project ids,
        // after a restart.
        let mut projects = Projects::load().unwrap();
        assert_eq!(add(&mut projects, &mac(), "p3", &app), KeyMatch::Known(key.clone()));
        assert_eq!(add(&mut projects, &mac(), "p4", &plain), KeyMatch::Known(plain_key.clone()));
        assert_eq!((projects.key_of(&mac(), "p3"), projects.key_of(&mac(), "p4")), (Some(&key), Some(&plain_key)));
        // The folder deleted and cloned again in its place.
        std::fs::remove_dir_all(&app).unwrap();
        repo(&app, Some("https://github.com/acme/app"));
        assert_eq!(add(&mut projects, &mac(), "p5", &app), KeyMatch::Known(key));
    }

    #[test]
    fn mac_and_server_ids_map_to_one_key() {
        let scratch = Scratch::new();
        let (on_mac, on_server) = (scratch.at("Users").join("alex").join("app"), scratch.at("home").join("alex").join("app"));
        repo(&on_mac, Some("git@github.com:acme/app.git"));
        clone(&on_mac, &on_server, Some("https://github.com/acme/app.git"));
        // The server alone, before any map reaches it, makes the same key from the remote.
        let alone = new_key(&resolve(&on_server, &[]));
        let mut projects = Projects::default();
        let key = new_key(&add(&mut projects, &mac(), "mac-1", &on_mac));
        assert_eq!(alone, key);
        // The server's project, with the map from the Mac: one key, each machine's id and folder.
        assert_eq!(add(&mut projects, &server(), "srv-1", &on_server), KeyMatch::Known(key.clone()));
        assert_eq!((projects.key_of(&mac(), "mac-1"), projects.key_of(&server(), "srv-1")), (Some(&key), Some(&key)));
        assert_eq!(projects.key_of(&mac(), "srv-1"), None, "an id is its machine's");
        let on_each = |machine: &MachineId| projects.known(machine).into_iter().find(|(k, _)| *k == key).unwrap().1;
        assert!(on_each(&server()).paths.iter().all(|path| crate::paths::same_folder(path, &on_server)), "a Mac's folder means nothing on the server");
        assert_eq!(on_each(&mac()).project_ids, ["mac-1"]);

        // The Mac's mapping wins: the server's own map named the repository otherwise.
        let mut own = Projects::default();
        let stray = ProjectKey::parse("app-ffffff").unwrap();
        own.attach(&stray, &facts(&on_server), &server(), Some("srv-1"));
        let known: Vec<(ProjectKey, Aliases)> = projects.known(&server()).into_iter().chain(own.known(&server())).collect();
        assert_eq!(resolve(&on_server, &known), KeyMatch::Known(key.clone()));
        // A project the app has bound keeps its key, whatever its checkout says now (a session can
        // change its remote).
        git(&on_mac, &["remote", "set-url", "origin", "git@github.com:acme/other.git"]);
        assert_eq!(projects.key_for(&mac(), "mac-1", &on_mac).unwrap(), KeyMatch::Known(key));
    }

    /// A session sets a bound project's `origin` to another project's remote: the project keeps
    /// its key, does not take the remote, and the other project's clones stay the other's.
    #[test]
    fn a_planted_remote_takes_no_other_project() {
        let scratch = Scratch::new();
        let (app, other) = (scratch.at("app"), scratch.at("other"));
        repo(&app, Some("git@github.com:acme/app.git"));
        repo(&other, Some("git@github.com:acme/other.git"));
        let mut projects = Projects::default();
        let key = new_key(&add(&mut projects, &mac(), "p1", &app));
        let other_key = new_key(&add(&mut projects, &mac(), "p2", &other));
        git(&app, &["remote", "set-url", "origin", "git@github.com:acme/other.git"]);
        assert_eq!(projects.key_for(&mac(), "p1", &app).unwrap(), KeyMatch::Known(key.clone()));
        // Added again (the binding), its facts attached: the other's remote is not taken.
        assert_eq!(add(&mut projects, &mac(), "p1", &app), KeyMatch::Known(key.clone()));
        assert_eq!(projects.keys[&key].remotes, ["github.com/acme/app"]);
        assert_eq!(projects.keys[&other_key].remotes, ["github.com/acme/other"]);
        let fresh = scratch.at("fresh").join("other");
        clone(&other, &fresh, Some("https://github.com/acme/other"));
        assert_eq!(resolve(&fresh, &projects.known(&mac())), KeyMatch::Known(other_key));
    }

    /// A partial clone's `git remote -v` line ends in its filter; its remote is still read (review
    /// of PR 83).
    #[test]
    fn a_partial_clone_keeps_its_remote() {
        let scratch = Scratch::new();
        let main = scratch.at("main").join("app");
        repo(&main, Some("git@github.com:acme/app.git"));
        std::fs::write(main.join("f.txt"), "hello").unwrap();
        git(&main, &["add", "f.txt"]);
        git(&main, &["commit", "-q", "-m", "a file"]);
        git(&main, &["config", "uploadpack.allowFilter", "true"]);
        let mut projects = Projects::default();
        let key = new_key(&add(&mut projects, &mac(), "p1", &main));
        let partial = scratch.at("partial").join("app");
        std::fs::create_dir(scratch.at("partial")).unwrap();
        let url = url::Url::from_file_path(&main).unwrap().to_string();
        git(&scratch.at("partial"), &["clone", "-q", "--filter=blob:none", &url, &partial.to_string_lossy()]);
        git(&partial, &["remote", "set-url", "origin", "https://github.com/acme/app.git"]);
        assert!(git(&partial, &["remote", "-v"]).contains("(fetch) [blob:none]"), "a partial clone");
        assert_eq!(facts(&partial).remotes, ["github.com/acme/app"]);
        assert_eq!(resolve(&partial, &projects.known(&mac())), KeyMatch::Known(key.clone()));
        assert_eq!(resolve(&partial, &[]), KeyMatch::New(key), "the same key on a machine of its own");
    }

    /// A project's folder deleted and used again by another repository that has no root commit
    /// to compare (a shallow clone, no commit yet) is asked about, never merged into the old key
    /// (review of PR 83).
    #[test]
    fn a_folder_used_again_by_another_repository_is_not_merged() {
        let scratch = Scratch::new();
        let demo = scratch.at("code").join("demo");
        repo(&demo, Some("git@github.com:acme/app.git"));
        let lib = scratch.at("source").join("lib");
        repo(&lib, Some("git@github.com:zed/lib.git"));
        let mut projects = Projects::default();
        let key = new_key(&add(&mut projects, &mac(), "p1", &demo));
        // A shallow clone of another repository in its folder.
        std::fs::remove_dir_all(&demo).unwrap();
        let url = url::Url::from_file_path(&lib).unwrap().to_string();
        git(&scratch.at("code"), &["clone", "-q", "--depth", "1", &url, &demo.to_string_lossy()]);
        git(&demo, &["remote", "set-url", "origin", "git@github.com:zed/lib.git"]);
        assert!(matches!(add(&mut projects, &mac(), "p2", &demo), KeyMatch::AskFork { candidate, by: Shared::Folder, .. } if candidate == key));
        assert_eq!(projects.keys[&key].remotes, ["github.com/acme/app"], "nothing attached");
        let whole = scratch.at("whole").join("lib");
        clone(&lib, &whole, Some("git@github.com:zed/lib.git"));
        assert!(matches!(resolve(&whole, &projects.known(&mac())), KeyMatch::New(new) if new != key));
        // A repository with no commit yet and a remote.
        std::fs::remove_dir_all(&demo).unwrap();
        std::fs::create_dir(&demo).unwrap();
        git(&demo, &["init", "-q"]);
        git(&demo, &["remote", "add", "origin", "git@github.com:zed/lib.git"]);
        assert!(matches!(resolve(&demo, &projects.known(&mac())), KeyMatch::AskFork { by: Shared::Folder, .. }));
        // The same with only an `upstream` remote, which would be the remote attached (review of PR
        // 83, round 2).
        git(&demo, &["remote", "rename", "origin", "upstream"]);
        assert_eq!(facts(&demo).project_remotes(), ["github.com/zed/lib"]);
        assert!(matches!(add(&mut projects, &mac(), "p2", &demo), KeyMatch::AskFork { by: Shared::Folder, .. }));
        assert_eq!(projects.keys[&key].remotes, ["github.com/acme/app"], "nothing attached");

        // A pinned folder used again: by another history, a project of its own; by a repository
        // with no commit yet and another remote, asked.
        let fork = scratch.at("fork").join("app");
        clone(&demo_source(&scratch), &fork, Some("git@github.com:alex/app.git"));
        let pinned = ProjectKey::parse("app-a1a1a1").unwrap();
        projects.pin(&pinned, &facts(&fork), &mac(), Some("p3"));
        assert_eq!(resolve(&fork, &projects.known(&mac())), KeyMatch::Known(pinned.clone()));
        std::fs::remove_dir_all(&fork).unwrap();
        repo(&fork, Some("git@github.com:zed/unrelated.git"));
        assert!(matches!(resolve(&fork, &projects.known(&mac())), KeyMatch::New(new) if new != pinned));
        std::fs::remove_dir_all(&fork).unwrap();
        repo(&fork, None);
        assert!(matches!(resolve(&fork, &projects.known(&mac())), KeyMatch::New(new) if new != pinned), "another history, no remote");
        std::fs::remove_dir_all(&fork).unwrap();
        std::fs::create_dir(&fork).unwrap();
        git(&fork, &["init", "-q"]);
        git(&fork, &["remote", "add", "origin", "git@github.com:zed/unrelated.git"]);
        assert!(matches!(resolve(&fork, &projects.known(&mac())), KeyMatch::AskFork { candidate, by: Shared::Folder, .. } if candidate == pinned));
    }

    /// A repository of its own, for a fork to be cloned from.
    fn demo_source(scratch: &Scratch) -> PathBuf {
        let source = scratch.at("demo-source").join("app");
        if !source.exists() {
            repo(&source, Some("git@github.com:acme/app.git"));
        }
        source
    }

    /// The pinned folder comes before everything else, and a pin can be moved (review of PR 83).
    #[test]
    fn a_pin_is_the_explicit_mapping() {
        let scratch = Scratch::new();
        let upstream = scratch.at("upstream").join("app");
        repo(&upstream, Some("git@github.com:acme/app.git"));
        let mut projects = Projects::default();
        let key = new_key(&add(&mut projects, &mac(), "p1", &upstream));
        // A fork called a project of its own keeps that answer when its `origin` changes to a
        // remote nobody knows, while its `upstream` is still the upstream's.
        let fork = scratch.at("fork").join("app");
        clone(&upstream, &fork, Some("git@github.com:alex/app.git"));
        git(&fork, &["remote", "add", "upstream", "git@github.com:acme/app.git"]);
        let KeyMatch::AskFork { fresh, .. } = resolve(&fork, &projects.known(&mac())) else { panic!("a fork is asked about") };
        projects.pin(&fresh, &facts(&fork), &mac(), Some("p2"));
        git(&fork, &["remote", "set-url", "origin", "git@github.com:alex/app-moved.git"]);
        assert_eq!(resolve(&fork, &projects.known(&mac())), KeyMatch::Known(fresh.clone()));

        // A folder whose own remote another key has, pinned by the person to the upstream's key:
        // the pin decides, and the other key keeps its remote.
        let other = scratch.at("other").join("tool");
        repo(&other, Some("git@github.com:acme/tool.git"));
        let tool = new_key(&add(&mut projects, &mac(), "p3", &other));
        let odd = scratch.at("odd").join("app");
        clone(&upstream, &odd, Some("git@github.com:acme/tool.git"));
        assert_eq!(resolve(&odd, &projects.known(&mac())), KeyMatch::Known(tool.clone()), "by its remote, before the pin");
        projects.pin(&key, &facts(&odd), &mac(), Some("p4"));
        assert_eq!(resolve(&odd, &projects.known(&mac())), KeyMatch::Known(key.clone()));
        assert!(!projects.keys[&key].remotes.contains(&"github.com/acme/tool".to_string()));

        // Pinned to one key and then to another: the second, though the first comes first.
        let (first, second) = (ProjectKey::parse("aaa-000001").unwrap(), ProjectKey::parse("bbb-000002").unwrap());
        let plain = scratch.at("plain");
        std::fs::create_dir(&plain).unwrap();
        projects.pin(&first, &facts(&plain), &mac(), Some("p5"));
        projects.pin(&second, &facts(&plain), &mac(), Some("p5"));
        assert_eq!(projects.known(&mac())[0].0, first, "the first key comes first");
        assert_eq!(resolve(&plain, &projects.known(&mac())), KeyMatch::Known(second.clone()));
        assert_eq!(projects.key_of(&mac(), "p5"), Some(&second));
    }

    /// A fork whose own remote is not called `origin` is still asked about; with several remotes
    /// and none called `origin`, none is taken for its own, and it is asked about too (review of
    /// PR 83).
    #[test]
    fn a_fork_without_origin_is_asked_about() {
        let scratch = Scratch::new();
        let upstream = scratch.at("upstream").join("app");
        repo(&upstream, Some("git@github.com:acme/app.git"));
        let mut projects = Projects::default();
        let key = new_key(&add(&mut projects, &mac(), "p1", &upstream));
        let fork = scratch.at("fork").join("app");
        clone(&upstream, &fork, Some("git@github.com:alex/app.git"));
        git(&fork, &["remote", "rename", "origin", "mine"]);
        git(&fork, &["remote", "add", "upstream", "https://github.com/acme/app"]);
        let found = facts(&fork);
        assert_eq!((found.remotes.as_slice(), found.upstream.as_deref()), (["github.com/alex/app".to_string()].as_slice(), Some("github.com/acme/app")));
        assert!(matches!(matching(&found, &projects.known(&mac())), KeyMatch::AskFork { candidate, by: Shared::Upstream, .. } if candidate == key));
        git(&fork, &["remote", "add", "backup", "git@gitlab.example.com:alex/app.git"]);
        let found = facts(&fork);
        assert!(found.remotes.is_empty() && found.others.len() == 2, "{found:?}");
        assert!(found.project_remotes().is_empty(), "the upstream is not the project when other remotes are there");
        assert!(matches!(matching(&found, &projects.known(&mac())), KeyMatch::AskFork { by: Shared::Upstream, .. }));
    }

    /// A clone that kept only its `upstream` remote is that project, and its key lists the remote
    /// it was made from, so a real clone added later is the same project (review of PR 83).
    #[test]
    fn a_key_made_from_upstream_lists_it() {
        let scratch = Scratch::new();
        let main = scratch.at("main").join("app");
        repo(&main, Some("git@github.com:acme/app.git"));
        let only = scratch.at("only").join("app");
        clone(&main, &only, None);
        git(&only, &["remote", "add", "upstream", "git@github.com:acme/app.git"]);
        let mut projects = Projects::default();
        let key = new_key(&add(&mut projects, &mac(), "p1", &only));
        assert_eq!(projects.keys[&key].remotes, ["github.com/acme/app"]);
        assert_eq!(resolve(&main, &projects.known(&mac())), KeyMatch::Known(key.clone()));
        assert_eq!(resolve(&main, &[]), KeyMatch::New(key), "the key a server makes alone");
    }

    /// `glasscode.repo` comes before `origin` (review of PR 83).
    #[test]
    fn glasscode_repo_comes_before_origin() {
        let scratch = Scratch::new();
        let mirror = scratch.at("mirror").join("app");
        repo(&mirror, Some("git@gitlab.example.com:mirror/app.git"));
        git(&mirror, &["config", "glasscode.repo", "Acme/App"]);
        let found = facts(&mirror);
        assert_eq!(found.remotes, ["github.com/acme/app", "gitlab.example.com/mirror/app"]);
        let (by_mirror, by_github) = (ProjectKey::parse("aaa-000001").unwrap(), ProjectKey::parse("bbb-000002").unwrap());
        let known = [
            (by_mirror, Aliases { remotes: vec!["gitlab.example.com/mirror/app".into()], ..Aliases::default() }),
            (by_github.clone(), Aliases { remotes: vec!["github.com/acme/app".into()], ..Aliases::default() }),
        ];
        assert_eq!(matching(&found, &known), KeyMatch::Known(by_github));
    }

    /// `attach` takes the folder and the project id off every other key of the machine (review of
    /// PR 83).
    #[test]
    fn attach_moves_the_folder_and_the_id() {
        let _scratch = Scratch::new();
        let (first, second) = (ProjectKey::parse("aaa-000001").unwrap(), ProjectKey::parse("bbb-000002").unwrap());
        let facts = Facts { path: PathBuf::from("/work/app"), ..Facts::default() };
        let mut projects = Projects::default();
        projects.attach(&first, &facts, &mac(), Some("p1"));
        projects.attach(&first, &facts, &server(), Some("s1"));
        projects.attach(&second, &facts, &mac(), Some("p1"));
        let on_mac = |key: &ProjectKey| projects.keys[key].machines[&mac()].clone();
        assert!(on_mac(&first).paths.is_empty() && on_mac(&first).project_ids.is_empty(), "{:?}", on_mac(&first));
        assert_eq!((on_mac(&second).paths.len(), projects.key_of(&mac(), "p1")), (1, Some(&second)));
        assert_eq!(projects.key_of(&server(), "s1"), Some(&first), "another machine's are its own");
    }

    /// Git that cannot run is an error for a repository, never a plain folder (review of PR 83).
    #[test]
    fn git_that_cannot_run_is_an_error() {
        let scratch = Scratch::new();
        let app = scratch.at("app");
        repo(&app, Some("git@github.com:acme/app.git"));
        let plain = scratch.at("plain");
        std::fs::create_dir(&plain).unwrap();
        let empty = scratch.at("no-programs");
        std::fs::create_dir(&empty).unwrap();
        let before = std::env::var_os("PATH");
        // SAFETY: the core tests run one at a time; it is put back below.
        unsafe { std::env::set_var("PATH", &empty) };
        let (repository, folder) = (super::facts(&app), super::facts(&plain));
        let subfolder = super::facts(&app.join("src"));
        match before {
            Some(path) => unsafe { std::env::set_var("PATH", path) },
            None => unsafe { std::env::remove_var("PATH") },
        }
        assert!(repository.is_err() && subfolder.is_err(), "{repository:?} {subfolder:?}");
        assert!(super::resolve(&app, &[]).is_ok(), "git is back");
        let folder = folder.unwrap();
        assert!(!folder.git && crate::paths::same_folder(&folder.path, &plain));
    }

    /// The disks of macOS and Windows ignore letter case, and so do the folders of a key.
    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn a_folder_in_another_letter_case_is_the_same() {
        let scratch = Scratch::new();
        let folder = scratch.at("Notes");
        std::fs::create_dir(&folder).unwrap();
        let key = ProjectKey::parse("notes-000001").unwrap();
        let stored = scratch.at("NOTES");
        let known = [(key.clone(), Aliases { paths: vec![stored], ..Aliases::default() })];
        assert_eq!(resolve(&folder, &known), KeyMatch::Known(key.clone()));
        // A folder that is gone is compared as written, letter case ignored too.
        let gone = Facts { path: scratch.at("Gone"), ..Facts::default() };
        let known = [(key.clone(), Aliases { paths: vec![scratch.at("gONE")], ..Aliases::default() })];
        assert_eq!(matching(&gone, &known), KeyMatch::Known(key));
    }

    #[test]
    fn keys_are_safe_folder_names() {
        let free = |_: &ProjectKey| false;
        for (name, slug) in [
            ("glasscode", "glasscode"),
            ("con.app", "con-app"),
            ("aux.js", "aux-js"),
            ("nul", "nul"),
            (".github", "github"),
            ("My Repo!!", "my-repo"),
            ("über", "ber"),
            ("日本", "project"),
            ("", "project"),
            ("a.", "a"),
            ("next.js", "next-js"),
        ] {
            let key = key_for(name, "github.com/acme/x", free);
            assert_eq!(key.as_str().rsplit_once('-').unwrap().0, slug, "{name:?}");
            assert_eq!(ProjectKey::parse(key.as_str()).as_ref(), Some(&key));
        }
        let long = key_for(&"x".repeat(200), "alias", free);
        assert_eq!(long.as_str().len(), 64);
        // A key that is taken by another project gets more digits.
        let first = key_for("app", "github.com/acme/app", free);
        let second = key_for("app", "github.com/acme/app", |key| *key == first);
        assert!(second.as_str().starts_with(first.as_str()) && second.as_str().len() == first.as_str().len() + 2, "{first} {second}");
        // The same alias gives the same key on every machine.
        assert_eq!(key_for("app", "github.com/acme/app", free), first);
    }

    fn session(branch: &str) -> Session {
        serde_json::from_value(serde_json::json!({
            "id": "s1", "project_id": "p1", "worktree": "/work/app", "branch": branch, "title": "Claude · main", "agent": "claude", "created_at": 1
        }))
        .unwrap()
    }

    #[test]
    fn work_keys_for_sessions() {
        let project = ProjectKey::parse("app-3f9a1c").unwrap();
        let ids = |key: WorkKey| (key.kind, key.id);
        let mut review = session("review/42");
        review.review = Some(crate::model::ReviewRef { repo: "Acme/App".into(), number: 42, ..Default::default() });
        assert_eq!(ids(WorkKey::for_session(&project, &review, Some("ENG-1"), None)), (WorkKind::Review, "acme/app#42".to_string()));
        assert_eq!(ids(WorkKey::for_session(&project, &session("fix-login"), None, Some(("acme/app", 7)))), (WorkKind::Pr, "acme/app#7".to_string()));
        assert_eq!(ids(WorkKey::for_session(&project, &session("eng-42-login"), Some(" eng-42 "), None)), (WorkKind::Ticket, "ENG-42".to_string()));
        assert_eq!(ids(WorkKey::for_session(&project, &session("feat/login"), Some(""), None)), (WorkKind::Branch, "feat/login".to_string()));
        // A ticket's work keeps its key when its branch is renamed; a branch's work does not.
        let ticket = |branch| WorkKey::for_session(&project, &session(branch), Some("ENG-42"), None);
        assert_eq!(ticket("eng-42").file_name(), ticket("eng-42-login-form").file_name());
        let branch = |name| WorkKey::for_session(&project, &session(name), None, None);
        assert_ne!(branch("feat/login").file_name(), branch("feat/login-form").file_name());
        let key = WorkKey::for_session(&project, &session("feat/login"), None, None);
        assert_eq!(serde_json::to_value(&key).unwrap(), serde_json::json!({ "project": "app-3f9a1c", "kind": "branch", "id": "feat/login" }));
    }

    /// Two branches that differ only by `/` or by letter case get two files, on a disk that
    /// ignores case too.
    #[test]
    fn work_files_differ_by_slash_and_case() {
        let project = ProjectKey::parse("app-3f9a1c").unwrap();
        let branch = |name: &str| WorkKey { project: project.clone(), kind: WorkKind::Branch, id: name.to_string() };
        let names: Vec<String> = ["feat/x", "feat-x", "Feat-X", "feat_x", "feat.x"].into_iter().map(|name| branch(name).file_name()).collect();
        let lower: std::collections::BTreeSet<String> = names.iter().map(|name| name.to_lowercase()).collect();
        assert_eq!(lower.len(), names.len(), "{names:?}");
        assert!(names.iter().all(|name| name.starts_with("feat-x-")), "{names:?}");
        for key in [branch("feat/x"), branch(&"long/".repeat(30)), branch("日本"), WorkKey { project: project.clone(), kind: WorkKind::Pr, id: "acme/app#42".into() }, WorkKey { project: project.clone(), kind: WorkKind::Ticket, id: "CON".into() }] {
            let name = key.file_name();
            assert!(ProjectKey::parse(&name).is_some() && name.len() <= 47, "{name}");
        }
        assert!(branch("feat/x").file_name() != WorkKey { kind: WorkKind::Ticket, ..branch("feat/x") }.file_name());
    }

    /// The test kit's virtual clock in miniature: a time that tests move, in a zone they change.
    struct Virtual {
        ms: AtomicU64,
        offset_minutes: AtomicI64,
    }

    impl Virtual {
        fn at(ms: u64) -> Arc<Virtual> {
            Arc::new(Virtual { ms: AtomicU64::new(ms), offset_minutes: AtomicI64::new(0) })
        }

        fn set(&self, ms: u64) {
            self.ms.store(ms, Atomic::SeqCst);
        }
    }

    impl Clock for Virtual {
        fn now_ms(&self) -> u64 {
            self.ms.load(Atomic::SeqCst)
        }

        fn local_minutes(&self, at_ms: u64) -> Option<i64> {
            Some((at_ms / 60_000) as i64 + self.offset_minutes.load(Atomic::SeqCst))
        }
    }

    const DAY: u64 = 24 * 60 * 60 * 1000;
    const NOV_19: u64 = 1_763_510_400_000;

    #[test]
    fn hlc_is_monotonic_across_clock_jump() {
        let clock = Virtual::at(NOV_19);
        let _clock = crate::clock::use_test_clock(clock.clone());
        let mut last = Hlc::zero(mac());
        let mut stamps = Vec::new();
        // The clock moves on, stands still, goes back a day, jumps a day ahead, and comes back.
        for ms in [NOV_19, NOV_19, NOV_19 + 5, NOV_19 - DAY, NOV_19 - DAY + 1, NOV_19 + DAY, NOV_19 + 3, NOV_19 + DAY + 1] {
            clock.set(ms);
            let next = Hlc::now(&last);
            assert!(next > last, "{last} then {next} at {ms}");
            assert!(next.wall_ms >= ms, "never behind the clock");
            stamps.push(next.clone());
            last = next;
        }
        // Set back a day, the stamps stay where the clock had been.
        assert_eq!((stamps[3].wall_ms, stamps[3].count), (NOV_19 + 5, 1));
        assert_eq!((stamps[5].wall_ms, stamps[5].count), (NOV_19 + DAY, 0));
        // Merging an older stamp still moves on.
        let old = Hlc { wall_ms: NOV_19 - DAY, count: 9, machine: server() };
        assert!(last.merge(&old) > last);
        // Four billion stamps in one millisecond: the next millisecond.
        let full = Hlc { wall_ms: NOV_19 + 2 * DAY, count: u32::MAX, machine: mac() };
        assert_eq!((Hlc::now(&full).wall_ms, Hlc::now(&full).count), (NOV_19 + 2 * DAY + 1, 0));
    }

    /// A server ten minutes ahead: an edit the Mac makes after seeing the server's comes after it,
    /// although the Mac's clock says earlier; two edits made apart are ordered alike everywhere.
    #[test]
    fn a_server_ten_minutes_ahead_does_not_win_what_comes_after() {
        let clock = Virtual::at(NOV_19);
        let _clock = crate::clock::use_test_clock(clock.clone());
        let ten = 10 * 60 * 1000;
        let (mut on_mac, mut on_server) = (Hlc::zero(mac()), Hlc::zero(server()));
        clock.set(NOV_19 + ten);
        on_server = Hlc::now(&on_server);
        let server_edit = on_server.clone();
        clock.set(NOV_19 + 1);
        on_mac = on_mac.merge(&server_edit);
        let mac_edit = Hlc::now(&on_mac);
        assert!(mac_edit > server_edit, "{mac_edit} after {server_edit}");
        assert_eq!(mac_edit.machine, mac());
        // Concurrent edits, made without seeing each other, at the same wall time: the machine
        // decides, the same way on both.
        let a = Hlc { wall_ms: NOV_19, count: 0, machine: mac() };
        let b = Hlc { wall_ms: NOV_19, count: 0, machine: server() };
        assert_eq!(a.cmp(&b), Ordering::Less);
        assert_eq!(b.cmp(&a), Ordering::Greater);
        let merged = on_server.merge(&mac_edit);
        assert!(merged > mac_edit && merged > on_server && merged.machine == server());
    }

    /// The rule of a clock set back by more than the bound: the stamps keep their order, they are
    /// too far ahead of the clock (so peers quarantine them, and this machine can tell), until the
    /// clock passes the last one (review of PR 83).
    #[test]
    fn a_clock_set_back_a_day_keeps_the_order_and_says_so() {
        let clock = Virtual::at(NOV_19 + DAY);
        let _clock = crate::clock::use_test_clock(clock.clone());
        let last = Hlc::now(&Hlc::zero(mac()));
        clock.set(NOV_19);
        let next = Hlc::now(&last);
        assert!(next > last && next.wall_ms == NOV_19 + DAY);
        assert!(next.too_far_ahead(), "this machine can tell its clock went back");
        // A peer with a right clock does not take its time in.
        let peer = Hlc::now(&Hlc::zero(server()));
        assert_eq!(peer.merge(&next).wall_ms, NOV_19);
        // Within the bound, nothing is said; past the stamp, the stamps follow the clock again.
        clock.set(NOV_19 + DAY - MAX_AHEAD_MS);
        assert!(!Hlc::now(&next).too_far_ahead());
        clock.set(NOV_19 + DAY + 1);
        assert_eq!(Hlc::now(&next).wall_ms, NOV_19 + DAY + 1);
    }

    /// A received stamp at the same wall time with a lower count still gives a stamp after this
    /// machine's own (review of PR 83).
    #[test]
    fn merge_counts_past_both() {
        let clock = Virtual::at(NOV_19 - 1);
        let _clock = crate::clock::use_test_clock(clock.clone());
        let mine = Hlc { wall_ms: NOV_19, count: 5, machine: mac() };
        let theirs = Hlc { wall_ms: NOV_19, count: 2, machine: server() };
        assert_eq!(mine.merge(&theirs), Hlc { wall_ms: NOV_19, count: 6, machine: mac() });
        assert_eq!(theirs.merge(&mine), Hlc { wall_ms: NOV_19, count: 6, machine: server() });
    }

    /// Stamps are UTC: a time-zone change while asleep moves none of them.
    #[test]
    fn stamps_ignore_the_time_zone() {
        let clock = Virtual::at(NOV_19);
        let _clock = crate::clock::use_test_clock(clock.clone());
        let before = Hlc::now(&Hlc::zero(mac()));
        // Asleep for eight hours, then woken nine hours east.
        clock.set(NOV_19 + 8 * 60 * 60 * 1000);
        clock.offset_minutes.store(9 * 60, Atomic::SeqCst);
        let after = Hlc::now(&before);
        assert_eq!((before.wall_ms, after.wall_ms), (NOV_19, NOV_19 + 8 * 60 * 60 * 1000));
        assert!(after > before);
    }

    /// A stamp too far ahead is quarantined, not merged; one within the bound is.
    #[test]
    fn a_stamp_too_far_ahead_is_not_merged() {
        let clock = Virtual::at(NOV_19);
        let _clock = crate::clock::use_test_clock(clock.clone());
        let last = Hlc::now(&Hlc::zero(mac()));
        let far = Hlc { wall_ms: NOV_19 + MAX_AHEAD_MS + 1, count: 0, machine: server() };
        assert!(far.too_far_ahead());
        let merged = last.merge(&far);
        assert!(merged > last && merged.wall_ms == NOV_19, "{merged}");
        let near = Hlc { wall_ms: NOV_19 + MAX_AHEAD_MS, count: 4, machine: server() };
        assert!(!near.too_far_ahead());
        assert_eq!(last.merge(&near), Hlc { wall_ms: near.wall_ms, count: 5, machine: mac() });
    }

    #[test]
    fn hlc_text_and_json_round_trip() {
        let stamp = Hlc { wall_ms: NOV_19, count: 3, machine: mac() };
        assert_eq!(stamp.to_string(), "1763510400000.3@0123456789abcdef");
        assert_eq!(stamp.to_string().parse::<Hlc>(), Ok(stamp.clone()));
        for bad in ["", "1.2", "1@abc", "x.1@abc", "1.x@abc", "1.2@", "1.2@../x", "-1.2@abc", "1.2.3@abc"] {
            assert!(bad.parse::<Hlc>().is_err(), "{bad:?}");
        }
        assert_eq!(serde_json::to_value(&stamp).unwrap(), serde_json::json!({ "wall_ms": NOV_19, "count": 3, "machine": "0123456789abcdef" }));
        assert!(serde_json::from_value::<Hlc>(serde_json::json!({ "wall_ms": 1, "count": 0, "machine": "../x" })).is_err());
    }

    /// The store's file: keys and machine ids as object keys, refused when one is no folder name.
    #[test]
    fn the_projects_file_reads_back() {
        let _scratch = Scratch::new();
        let mut projects = Projects::default();
        let key = ProjectKey::parse("app-3f9a1c").unwrap();
        let facts = Facts { path: PathBuf::from("/work/app"), git: true, remotes: vec!["github.com/acme/app".into()], roots: vec!["9df6c54822bd7fe0b1b53ad934bffa17a381e133".into()], ..Facts::default() };
        projects.attach(&key, &facts, &mac(), Some("p1"));
        let value = serde_json::to_value(&projects).unwrap();
        assert_eq!(
            value,
            serde_json::json!({ "keys": { "app-3f9a1c": { "remotes": ["github.com/acme/app"], "roots": ["9df6c54822bd7fe0b1b53ad934bffa17a381e133"], "machines": { "0123456789abcdef": { "paths": ["/work/app"], "pinned": [], "project_ids": ["p1"] } } } } })
        );
        assert_eq!(serde_json::from_value::<Projects>(value).unwrap(), projects);
        assert!(serde_json::from_value::<Projects>(serde_json::json!({ "keys": { "../x": {} } })).is_err());
        assert!(serde_json::from_value::<Projects>(serde_json::json!({ "keys": { "app-1": { "machines": { "con": {} } } } })).is_err());
        assert_eq!(Projects::path(), crate::paths::authority_dir().join("projects.json"));
        assert_eq!(crate::paths::class_of(&Projects::path()), crate::paths::Classified::Class(crate::paths::PathClass::Authority));
        // Damaged, it refuses, with or without an older generation: no key comes from an old one.
        projects.save().unwrap();
        projects.save().unwrap();
        std::fs::write(Projects::path(), "{ not json").unwrap();
        assert!(matches!(Projects::load(), Err(crate::store::StoreError::Damaged { store: "authority/projects", .. })));
    }
}
