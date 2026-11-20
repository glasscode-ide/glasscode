//! Memory's git (M20.1 step 4, mem-13). Every call names the repository itself (`--git-dir`,
//! `--work-tree`) and drops the git variables it inherited, so it never acts on another
//! repository, and it reads none of the person's configuration: no system or global file, no
//! hooks, no signing, no line-ending or filter settings. The author and committer are
//! `Glasscode <glasscode@localhost>`, and the message ends with a trailer `Who: you|manager|session:<id>`.
//!
//! Commits are made with plumbing from the commit Glasscode recorded (a private index, `read-tree`,
//! the changed blobs, `write-tree`, `commit-tree`), never from the working tree or from `HEAD`: a
//! planted edit or a commit made with plain git is never swept into one of Glasscode's.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, bail};

/// Who made a change: the commit's `Who:` trailer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Who {
    You,
    Manager,
    Session(String),
}

impl Who {
    pub fn trailer(&self) -> String {
        match self {
            Who::You => "you".into(),
            Who::Manager => "manager".into(),
            Who::Session(id) => format!("session:{id}"),
        }
    }

    pub fn is_person(&self) -> bool {
        *self == Who::You
    }
}

/// A file the working tree has that the commit has not, or the other way round.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    Added,
    Modified,
    Deleted,
}

/// What a commit's message holds.
pub fn message(subject: &str, who: &Who) -> String {
    let subject: String = subject.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{subject}\n\nWho: {}\n", who.trailer())
}

/// The variables of git that point it at a repository, an index or a configuration: never taken
/// from the process that runs memory's git.
const DROPPED: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
    "GIT_EXTERNAL_DIFF",
    "GIT_DIFF_OPTS",
    "GIT_PAGER",
    "GIT_EDITOR",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_ASKPASS",
    "GIT_TRACE",
];

/// Settings given on every call, over anything the repository's own configuration says.
const SETTINGS: &[&str] = &[
    crate::sys::NO_HOOKS,
    "commit.gpgsign=false",
    "tag.gpgsign=false",
    "core.autocrlf=false",
    "core.safecrlf=false",
    "core.eol=lf",
    "core.quotepath=off",
    "core.fsmonitor=false",
    "core.untrackedCache=false",
    "core.precomposeunicode=true",
    crate::sys::NO_ATTRIBUTES_FILE,
    crate::sys::NO_EXCLUDES_FILE,
    "core.pager=cat",
    "diff.external=",
    "gc.auto=0",
    "maintenance.auto=false",
    "init.defaultBranch=main",
    "init.templateDir=",
    "user.name=Glasscode",
    "user.email=glasscode@localhost",
];

/// Attributes for every path of memory, above any `.gitattributes` in the tree: no filter, no
/// line-ending change, no identifier expansion, no encoding. Written into `.git/info/attributes`.
const ATTRIBUTES: &str = "* -filter -ident -text -eol -working-tree-encoding\n";

/// One memory repository: its working tree and its `.git`.
#[derive(Clone, Debug)]
pub struct Git {
    work: PathBuf,
    git_dir: PathBuf,
}

impl Git {
    pub fn new(work: &Path) -> Git {
        Git { work: work.to_path_buf(), git_dir: work.join(".git") }
    }

    pub fn work(&self) -> &Path {
        &self.work
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// Whether the repository is there.
    pub fn exists(&self) -> bool {
        self.git_dir.join("HEAD").is_file() && self.git_dir.join("objects").is_dir()
    }

    fn command(&self, index: Option<&Path>) -> Command {
        let mut cmd = crate::sys::command("git");
        for name in DROPPED {
            cmd.env_remove(name);
        }
        cmd.env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_TERMINAL_PROMPT", "0").env("GIT_OPTIONAL_LOCKS", "0").env("LC_ALL", "C");
        if let Some(index) = index {
            cmd.env("GIT_INDEX_FILE", index);
        }
        cmd.arg("--git-dir").arg(&self.git_dir).arg("--work-tree").arg(&self.work);
        for setting in SETTINGS {
            cmd.arg("-c").arg(setting);
        }
        cmd.current_dir(&self.work);
        cmd.stdin(Stdio::null());
        cmd
    }

    /// Run git with `args`, `input` on its standard input; its output, or its error words.
    fn run_with(&self, args: &[&str], index: Option<&Path>, input: Option<&[u8]>, envs: &[(&str, String)]) -> anyhow::Result<Vec<u8>> {
        let output = self.output(args, index, input, envs)?;
        if !output.status.success() {
            bail!("git {}: {}", args.first().copied().unwrap_or(""), String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(output.stdout)
    }

    /// Run git and give back how it ended.
    fn output(&self, args: &[&str], index: Option<&Path>, input: Option<&[u8]>, envs: &[(&str, String)]) -> anyhow::Result<std::process::Output> {
        let mut cmd = self.command(index);
        cmd.args(args);
        for (name, value) in envs {
            cmd.env(name, value);
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        if input.is_some() {
            cmd.stdin(Stdio::piped());
        }
        let mut child = cmd.spawn().context("could not run git")?;
        let writer = match (input, child.stdin.take()) {
            (Some(bytes), Some(mut stdin)) => {
                let bytes = bytes.to_vec();
                Some(std::thread::spawn(move || {
                    use std::io::Write as _;
                    let _ = stdin.write_all(&bytes);
                }))
            }
            _ => None,
        };
        let output = child.wait_with_output().context("could not run git")?;
        if let Some(writer) = writer {
            let _ = writer.join();
        }
        Ok(output)
    }

    pub(crate) fn run(&self, args: &[&str]) -> anyhow::Result<String> {
        Ok(String::from_utf8_lossy(&self.run_with(args, None, None, &[])?).trim().to_string())
    }

    /// A new repository in the working tree (made when missing), with branch `main` and memory's
    /// attributes.
    pub fn init(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.work).with_context(|| format!("could not make {}", self.work.display()))?;
        if !self.exists() {
            // `init` takes no --work-tree; the folder is named twice instead.
            let mut cmd = crate::sys::command("git");
            for name in DROPPED {
                cmd.env_remove(name);
            }
            cmd.env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_TERMINAL_PROMPT", "0");
            for setting in SETTINGS {
                cmd.arg("-c").arg(setting);
            }
            let output = cmd.arg("init").arg("-q").arg("--template=").arg(&self.work).current_dir(&self.work).stdin(Stdio::null()).output().context("could not run git")?;
            if !output.status.success() {
                bail!("git init: {}", String::from_utf8_lossy(&output.stderr).trim());
            }
        }
        self.keep_settings()
    }

    /// Memory's attributes in `.git/info/attributes`, written again when something changed them.
    pub fn keep_settings(&self) -> anyhow::Result<()> {
        let info = self.git_dir.join("info");
        let file = info.join("attributes");
        if std::fs::read_to_string(&file).ok().as_deref() != Some(ATTRIBUTES) {
            std::fs::create_dir_all(&info)?;
            std::fs::write(&file, ATTRIBUTES)?;
        }
        Ok(())
    }

    /// Store `bytes` as a blob; its id. Only text that passed the scan comes here.
    pub fn blob(&self, bytes: &[u8]) -> anyhow::Result<String> {
        let out = self.run_with(&["hash-object", "-w", "--no-filters", "--stdin"], None, Some(bytes), &[])?;
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }

    /// The tree of `base` (or of nothing) with `changes`: each path to a blob id, or removed.
    pub fn tree_with(&self, base: Option<&str>, changes: &[(String, Option<String>)]) -> anyhow::Result<String> {
        let index = TempIndex::new(&self.git_dir)?;
        match base {
            Some(base) => self.run_with(&["read-tree", base], Some(index.path()), None, &[])?,
            None => self.run_with(&["read-tree", "--empty"], Some(index.path()), None, &[])?,
        };
        let mut info = String::new();
        for (path, blob) in changes {
            match blob {
                Some(blob) => info.push_str(&format!("100644 {blob}\t{path}\n")),
                None => info.push_str(&format!("0 {}\t{path}\n", "0".repeat(40))),
            }
        }
        if !info.is_empty() {
            self.run_with(&["update-index", "--index-info"], Some(index.path()), Some(info.as_bytes()), &[])?;
        }
        let tree = self.run_with(&["write-tree"], Some(index.path()), None, &[])?;
        let tree = String::from_utf8_lossy(&tree).trim().to_string();
        if changes.iter().all(|(_, blob)| blob.is_none()) && base.is_none() {
            // git knows the empty tree without storing it; a commit of it needs the object.
            self.run_with(&["hash-object", "-w", "-t", "tree", "--stdin"], None, Some(b""), &[])?;
        }
        Ok(tree)
    }

    /// A commit of `tree` after `parent` (none: a new history), by Glasscode, at the clock's time.
    pub fn commit_tree(&self, tree: &str, parent: Option<&str>, message: &str) -> anyhow::Result<String> {
        let date = format!("@{} +0000", crate::clock::now_ms() / 1000);
        let envs = [
            ("GIT_AUTHOR_NAME", "Glasscode".to_string()),
            ("GIT_AUTHOR_EMAIL", "glasscode@localhost".to_string()),
            ("GIT_COMMITTER_NAME", "Glasscode".to_string()),
            ("GIT_COMMITTER_EMAIL", "glasscode@localhost".to_string()),
            ("GIT_AUTHOR_DATE", date.clone()),
            ("GIT_COMMITTER_DATE", date),
        ];
        let mut args = vec!["commit-tree", tree];
        if let Some(parent) = parent {
            args.extend(["-p", parent]);
        }
        let out = self.run_with(&args, None, Some(message.as_bytes()), &envs)?;
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }

    /// The tree of a commit.
    pub fn tree_of(&self, commit: &str) -> anyhow::Result<String> {
        self.run(&["rev-parse", "--verify", "-q", &format!("{commit}^{{tree}}")])
    }

    /// Whether `commit` is a commit of this repository.
    pub fn has_commit(&self, commit: &str) -> bool {
        self.exists() && self.run_with(&["cat-file", "-e", &format!("{commit}^{{commit}}")], None, None, &[]).is_ok()
    }

    /// Where a ref points, when it exists.
    pub fn read_ref(&self, name: &str) -> Option<String> {
        self.run(&["rev-parse", "--verify", "-q", name]).ok().filter(|sha| !sha.is_empty())
    }

    pub fn update_ref(&self, name: &str, commit: &str) -> anyhow::Result<()> {
        self.run(&["update-ref", "-m", "glasscode", name, commit]).map(|_| ())
    }

    pub fn delete_ref(&self, name: &str) -> anyhow::Result<()> {
        self.run(&["update-ref", "-d", name]).map(|_| ())
    }

    /// Every ref, with where it points.
    pub fn refs(&self) -> anyhow::Result<Vec<(String, String)>> {
        let out = self.run(&["for-each-ref", "--format=%(refname) %(objectname)"])?;
        Ok(out.lines().filter_map(|line| line.split_once(' ').map(|(name, sha)| (name.to_string(), sha.to_string()))).collect())
    }

    /// `HEAD` names `main`, as a person's `git log` expects.
    pub fn head_on_main(&self) -> anyhow::Result<()> {
        self.run(&["symbolic-ref", "HEAD", "refs/heads/main"]).map(|_| ())
    }

    /// The files that differ between two commits (removed ones too), with `/`.
    pub fn changed_between(&self, from: &str, to: &str) -> anyhow::Result<Vec<String>> {
        let out = self.run_with(&["diff-tree", "-r", "-z", "--name-only", "--no-commit-id", from, to], None, None, &[])?;
        Ok(split_z(&out))
    }

    /// The files of a commit, with `/`.
    pub fn files(&self, commit: &str) -> anyhow::Result<Vec<String>> {
        let out = self.run_with(&["ls-tree", "-r", "-z", "--name-only", commit], None, None, &[])?;
        Ok(split_z(&out))
    }

    /// The contents of `paths` in `commit`, each None when the commit has no such file.
    pub fn read(&self, commit: &str, paths: &[String]) -> anyhow::Result<Vec<Option<Vec<u8>>>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut input = String::new();
        for path in paths {
            if path.contains('\n') {
                bail!("a memory path has a line break");
            }
            input.push_str(&format!("{commit}:{path}\n"));
        }
        let out = self.run_with(&["cat-file", "--batch"], None, Some(input.as_bytes()), &[])?;
        let mut results = Vec::with_capacity(paths.len());
        let mut at = 0;
        for _ in paths {
            let line_end = out[at..].iter().position(|b| *b == b'\n').map(|n| at + n).context("git cat-file ended early")?;
            let header = String::from_utf8_lossy(&out[at..line_end]).into_owned();
            at = line_end + 1;
            if header.ends_with(" missing") || header.ends_with(" ambiguous") {
                results.push(None);
                continue;
            }
            let mut words = header.split(' ');
            let (_, kind, size) = (words.next(), words.next(), words.next());
            let size: usize = size.and_then(|s| s.parse().ok()).with_context(|| format!("git cat-file said {header:?}"))?;
            let body = out.get(at..at + size).context("git cat-file ended early")?.to_vec();
            at += size + 1;
            results.push((kind == Some("blob")).then_some(body));
        }
        Ok(results)
    }

    /// The files of commit `commit` that hold `needle` (fixed text).
    pub fn grep(&self, commit: &str, needle: &str) -> anyhow::Result<Vec<String>> {
        let output = self.output(&["grep", "-l", "-z", "-F", "-e", needle, commit, "--"], None, None, &[])?;
        match output.status.code() {
            Some(0) => Ok(split_z(&output.stdout).into_iter().map(|hit| hit.strip_prefix(&format!("{commit}:")).unwrap_or(&hit).to_string()).collect()),
            // No match.
            Some(1) if output.stderr.is_empty() => Ok(Vec::new()),
            _ => bail!("git grep: {}", String::from_utf8_lossy(&output.stderr).trim()),
        }
    }

    /// How the working tree differs from `commit`: files changed, removed and added (untracked
    /// files included), whatever the index says and without writing any object. `skip` are paths
    /// that are never compared.
    pub fn changes(&self, commit: &str, skip: &[&str]) -> anyhow::Result<Vec<(String, Change)>> {
        let index = TempIndex::new(&self.git_dir)?;
        self.run_with(&["read-tree", commit], Some(index.path()), None, &[])?;
        // Exit 1 when files need an update: the refresh is all that is wanted.
        let _ = self.run_with(&["update-index", "-q", "--refresh", "--ignore-missing"], Some(index.path()), None, &[]);
        let mut changes = Vec::new();
        let out = self.run_with(&["diff-files", "--no-ext-diff", "--name-status", "-z"], Some(index.path()), None, &[])?;
        let fields = split_z(&out);
        for pair in fields.chunks(2) {
            if let [status, path] = pair {
                let change = if status.starts_with('D') { Change::Deleted } else { Change::Modified };
                changes.push((path.clone(), change));
            }
        }
        let out = self.run_with(&["ls-files", "--others", "-z", "--exclude=.DS_Store", "--exclude=Thumbs.db"], Some(index.path()), None, &[])?;
        changes.extend(split_z(&out).into_iter().map(|path| (path, Change::Added)));
        changes.retain(|(path, _)| !skip.contains(&path.as_str()));
        changes.sort();
        changes.dedup();
        Ok(changes)
    }

    /// The diff of one tracked file of the working tree against `commit`, for the person.
    pub fn diff(&self, commit: &str, path: &str) -> anyhow::Result<String> {
        let index = TempIndex::new(&self.git_dir)?;
        self.run_with(&["read-tree", commit], Some(index.path()), None, &[])?;
        let out = self.run_with(&["diff", "--no-color", "--no-ext-diff", "--no-textconv", commit, "--", path], Some(index.path()), None, &[])?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// The default index set to `commit`, so `git status` in memory shows only pending edits. Left
    /// alone while another git holds the index.
    pub fn reset_index(&self, commit: &str) {
        if self.git_dir.join("index.lock").exists() {
            return;
        }
        let _ = self.run(&["read-tree", commit]);
    }

    /// `git fsck`: whether every object is whole and reachable objects are all there.
    pub fn fsck(&self) -> anyhow::Result<()> {
        let output = self.output(&["fsck", "--no-progress", "--no-dangling"], None, None, &[])?;
        if !output.status.success() {
            bail!("git fsck: {} {}", String::from_utf8_lossy(&output.stdout).trim(), String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(())
    }

    /// Delete every object no ref reaches, and the history of every ref, so text removed from the
    /// history is in no object any more (forgetting a secret, mem-13).
    pub fn purge(&self) -> anyhow::Result<()> {
        self.run(&["reflog", "expire", "--expire=now", "--expire-unreachable=now", "--all"])?;
        self.run(&["gc", "-q", "--prune=now"])?;
        self.run(&["prune", "--expire=now"])?;
        let _ = std::fs::remove_file(self.git_dir.join("ORIG_HEAD"));
        let _ = std::fs::remove_dir_all(self.git_dir.join("logs"));
        Ok(())
    }

    /// Remove a stale lock of the index another git left: one older than ten minutes, when no
    /// memory write of Glasscode runs (the caller holds the writer's lock).
    pub fn clear_stale_lock(&self) {
        let lock = self.git_dir.join("index.lock");
        let old = std::fs::metadata(&lock).and_then(|meta| meta.modified()).ok().and_then(|at| at.elapsed().ok()).is_some_and(|age| age > std::time::Duration::from_secs(600));
        if old {
            log::warn!("memory: removing a stale {}", lock.display());
            let _ = std::fs::remove_file(&lock);
        }
        // Private indexes of writes that died.
        if let Ok(entries) = std::fs::read_dir(&self.git_dir) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("glasscode-index-") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

/// A private index file in `.git`, removed when dropped.
struct TempIndex(PathBuf);

impl TempIndex {
    fn new(git_dir: &Path) -> anyhow::Result<TempIndex> {
        let mut bytes = [0u8; 6];
        crate::sys::random_bytes(&mut bytes);
        let name: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let path = git_dir.join(format!("glasscode-index-{}-{name}", std::process::id()));
        Ok(TempIndex(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("lock"));
    }
}

fn split_z(bytes: &[u8]) -> Vec<String> {
    bytes.split(|b| *b == 0).filter(|part| !part.is_empty()).map(|part| String::from_utf8_lossy(part).into_owned()).collect()
}

/// A file of a project's repository as its last commit has it (`HEAD:<path>`), for the citations of
/// facts about code. None when there is no such file or no repository. It runs git: never on the
/// main thread.
pub fn project_file(repo: &Path, path: &str) -> Option<String> {
    let mut cmd = crate::sys::command("git");
    for name in DROPPED {
        cmd.env_remove(name);
    }
    cmd.env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_TERMINAL_PROMPT", "0").env("GIT_OPTIONAL_LOCKS", "0");
    let output = cmd.arg("-C").arg(repo).args(["-c", crate::sys::NO_ATTRIBUTES_FILE, "show", "--no-textconv"]).arg(format!("HEAD:{path}")).stdin(Stdio::null()).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plumbing_commits_from_the_recorded_commit_only() {
        let dir = tempfile::tempdir().unwrap();
        let git = Git::new(&dir.path().join("memory"));
        git.init().unwrap();
        assert!(git.exists());
        let a = git.blob(b"one\n").unwrap();
        let tree = git.tree_with(None, &[("you/profile.md".into(), Some(a))]).unwrap();
        let first = git.commit_tree(&tree, None, &message("first", &Who::You)).unwrap();
        git.update_ref("refs/heads/main", &first).unwrap();
        git.head_on_main().unwrap();
        // A planted file in the tree is not in the next commit.
        std::fs::create_dir_all(git.work().join("you")).unwrap();
        std::fs::write(git.work().join("you/profile.md"), "one\n").unwrap();
        std::fs::write(git.work().join("planted.md"), "planted\n").unwrap();
        let b = git.blob(b"two\n").unwrap();
        let tree = git.tree_with(Some(&first), &[("projects/app-3f9a1c/PROJECT.md".into(), Some(b))]).unwrap();
        let second = git.commit_tree(&tree, Some(&first), &message("second", &Who::Session("s-1".into()))).unwrap();
        assert_eq!(git.files(&second).unwrap(), ["projects/app-3f9a1c/PROJECT.md", "you/profile.md"]);
        let read = git.read(&second, &["you/profile.md".into(), "planted.md".into(), "projects/app-3f9a1c/PROJECT.md".into()]).unwrap();
        assert_eq!(read, [Some(b"one\n".to_vec()), None, Some(b"two\n".to_vec())]);
        // The working tree against a commit: the planted file is added, the missing one removed.
        let changes = git.changes(&second, &["inbox/rejected.md"]).unwrap();
        assert_eq!(changes, [("planted.md".to_string(), Change::Added), ("projects/app-3f9a1c/PROJECT.md".to_string(), Change::Deleted)]);
        // The commit's author and trailer.
        let log = git.run(&["log", "-1", "--format=%an <%ae>|%cn|%B", &second]).unwrap();
        assert_eq!(log, "Glasscode <glasscode@localhost>|Glasscode|second\n\nWho: session:s-1");
        assert_eq!(git.grep(&second, "two").unwrap(), ["projects/app-3f9a1c/PROJECT.md"]);
        assert!(git.grep(&second, "nowhere").unwrap().is_empty());
        git.fsck().unwrap();
    }

    #[test]
    fn a_repository_around_memory_is_never_used() {
        // memory/ inside another repository, with GIT_DIR pointing at that one.
        let dir = tempfile::tempdir().unwrap();
        let outer = Git::new(dir.path());
        outer.init().unwrap();
        let before = outer.refs().unwrap();
        // SAFETY: the core tests run one at a time (`--test-threads=1`).
        unsafe { std::env::set_var("GIT_DIR", outer.git_dir()) };
        let git = Git::new(&dir.path().join("memory"));
        let made = git.init().and_then(|_| {
            let blob = git.blob(b"x")?;
            let tree = git.tree_with(None, &[("you/profile.md".into(), Some(blob))])?;
            let commit = git.commit_tree(&tree, None, "x")?;
            git.update_ref("refs/heads/main", &commit)
        });
        unsafe { std::env::remove_var("GIT_DIR") };
        made.unwrap();
        assert!(git.exists());
        assert_eq!(outer.refs().unwrap(), before, "the outer repository has no new ref");
        assert!(git.read_ref("refs/heads/main").is_some());
    }
}
