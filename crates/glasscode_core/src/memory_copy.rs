//! The server's read-only copy of the trusted memory (slice M20.3, step 3). The Mac builds a
//! separate tree from its memory that leaves out every project marked "stays on this Mac" and
//! every `org/<employer>/` folder used only by such projects, commits it on a history of its own
//! (`git commit-tree` over the filtered tree, never over the Mac's commits), and pushes that
//! history to the server, which checks it out read-only (`memory.copy.*` in glasscoded). The
//! generated `INDEX.md` stays here (it names every file and heading), and in the files that go, a
//! line about what stays is taken out: an entry kept to a left-out employer (`org=`) or proposed
//! for a left-out file (`to=`), and any line that names a left-out folder, project key or employer
//! (the inbox's per-file headings). So no local-only text, name or path is ever in an object the
//! server receives.
//!
//! (It lives beside `memstore` until M20.1 lands; then the Mac's memory head gives the commit.)

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use serde_json::json;

use crate::paths::ProjectKey;
use crate::remote::{RemoteClient, ServerConfig};

/// The ref the server copy's history is pushed to, on the server's copy repository.
pub const SERVER_REF: &str = "refs/glasscode/memory-server";

fn git(repo: &Path) -> std::process::Command {
    let mut cmd = crate::sys::command("git");
    cmd.arg("-C").arg(repo).args(["-c", crate::sys::NO_HOOKS, "-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"]).env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

fn run(mut cmd: std::process::Command) -> anyhow::Result<String> {
    let out = cmd.output().context("could not run git")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The folders of the memory tree that never leave this Mac: each local-only project's
/// `projects/<key>`, and each `org/<employer>` whose projects are all local-only. `employers`
/// names each project's employer (none: it has none).
pub fn excluded_paths(local_only: &[ProjectKey], employers: &[(ProjectKey, Option<String>)]) -> Vec<String> {
    let mut paths: Vec<String> = local_only.iter().map(|key| format!("projects/{}", key.as_str())).collect();
    let mut orgs: Vec<&str> = employers.iter().filter_map(|(_, e)| e.as_deref()).collect();
    orgs.sort();
    orgs.dedup();
    for org in orgs {
        let users: Vec<&ProjectKey> = employers.iter().filter(|(_, e)| e.as_deref() == Some(org)).map(|(k, _)| k).collect();
        if users.iter().all(|key| local_only.contains(key)) {
            paths.push(format!("org/{org}"));
        }
    }
    paths
}

/// Whether `rel` is one of `excluded` or below one.
fn under(rel: &str, excluded: &[String]) -> bool {
    excluded.iter().any(|path| rel == path || rel.starts_with(&format!("{path}/")))
}

/// The files of `commit` (outside `excluded`) with a line about what is excluded, each with its
/// text without those lines: an entry kept to an excluded employer (`org=`) or proposed for an
/// excluded file (`to=`), and any line (an entry, a heading such as the inbox's per-file one, prose)
/// that names an excluded folder, a local-only project's key or an excluded employer.
fn pointing_at(repo: &Path, commit: &str, excluded: &[String]) -> anyhow::Result<Vec<(String, String)>> {
    let orgs: Vec<&str> = excluded.iter().filter_map(|path| path.strip_prefix("org/")).collect();
    let mut needles: Vec<&str> = excluded.iter().map(String::as_str).collect();
    needles.extend(excluded.iter().filter_map(|path| path.rsplit('/').next()));
    let names = |text: &str| needles.iter().any(|needle| text.contains(needle));
    let mut grep = git(repo);
    grep.args(["grep", "-l", "-F", "-e", "org=", "-e", "to="]);
    for needle in &needles {
        grep.args(["-e", needle]);
    }
    grep.args([commit, "--"]);
    let out = grep.output().context("could not run git")?;
    // `git grep` exits 1 when nothing matches.
    let listed = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut out = Vec::new();
    for line in listed.lines() {
        let rel = line.strip_prefix(&format!("{commit}:")).unwrap_or(line);
        if under(rel, excluded) || rel == "INDEX.md" {
            continue;
        }
        let shown = git(repo).args(["show", &format!("{commit}:{rel}")]).output().context("could not run git")?;
        if !shown.status.success() {
            bail!("{}", String::from_utf8_lossy(&shown.stderr).trim());
        }
        let text = String::from_utf8_lossy(&shown.stdout).into_owned();
        let mut doc = crate::memstore::entry::Doc::parse(&text);
        let before = doc.lines.len();
        doc.lines.retain(|line| match line {
            crate::memstore::entry::Line::Entry(entry) => {
                !entry.header.org.as_ref().is_some_and(|org| orgs.contains(&org.to_string().as_str())) && !entry.header.to.as_deref().is_some_and(|to| under(to, excluded)) && !names(&entry.render())
            }
            crate::memstore::entry::Line::Text(text) => !names(text),
            crate::memstore::entry::Line::Tombstone { .. } => true,
        });
        if doc.lines.len() != before {
            out.push((rel.to_string(), doc.render()));
        }
    }
    Ok(out)
}

/// The server copy's commit for `commit` of the memory `repo`: its tree without `excluded`,
/// committed with `previous` (the copy's last commit) as its only parent, so the copy's history
/// holds filtered trees only. The same tree as `previous` gives `previous` back.
pub fn server_commit(repo: &Path, commit: &str, excluded: &[String], previous: Option<&str>) -> anyhow::Result<String> {
    // A path in a folder of its own, with no file there yet: git writes a new index beside it and
    // moves that into its place, which Windows does not allow over a file that this process holds
    // open ("unable to write new index file").
    let folder = tempfile::tempdir().context("no temporary index")?;
    let index = folder.path().join("index");
    let with_index = |args: &[&str]| {
        let mut cmd = git(repo);
        cmd.env("GIT_INDEX_FILE", &index).args(args);
        cmd
    };
    run(with_index(&["read-tree", commit]))?;
    for path in excluded {
        run(with_index(&["rm", "-r", "-q", "--cached", "--ignore-unmatch", "--", path]))?;
    }
    // INDEX.md names every file and its headings, the kept-back ones too: it stays here.
    run(with_index(&["rm", "-q", "--cached", "--ignore-unmatch", "--", "INDEX.md"]))?;
    // An entry elsewhere that points at what stays here (a profile entry kept to an employer left
    // out, `org=`; a proposal for a file left out, `to=`) is taken out of its file's copy.
    for (path, text) in pointing_at(repo, commit, excluded)? {
        let blob = {
            let mut cmd = git(repo);
            cmd.args(["hash-object", "-w", "--stdin"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
            let mut child = cmd.spawn().context("could not run git")?;
            {
                use std::io::Write as _;
                child.stdin.take().context("no stdin for git")?.write_all(text.as_bytes())?;
            }
            let out = child.wait_with_output()?;
            if !out.status.success() {
                bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
            }
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        run(with_index(&["update-index", "--cacheinfo", &format!("100644,{blob},{path}")]))?;
    }
    let tree = run(with_index(&["write-tree"]))?;
    if let Some(previous) = previous {
        if run({
            let mut cmd = git(repo);
            cmd.args(["rev-parse", &format!("{previous}^{{tree}}")]);
            cmd
        })
        .ok()
        .as_deref()
            == Some(tree.as_str())
        {
            return Ok(previous.to_string());
        }
    }
    let mut commit_tree = git(repo);
    commit_tree
        .env("GIT_AUTHOR_NAME", "Glasscode")
        .env("GIT_AUTHOR_EMAIL", "glasscode@localhost")
        .env("GIT_COMMITTER_NAME", "Glasscode")
        .env("GIT_COMMITTER_EMAIL", "glasscode@localhost")
        .args(["commit-tree", &tree, "-m", "Memory for the server (local-only projects left out)\n\nWho: manager"]);
    if let Some(previous) = previous {
        commit_tree.args(["-p", previous]);
    }
    run(commit_tree)
}

/// Send the server copy: its commit for `commit` of the memory `repo` (`previous`: the last one
/// sent), pushed to the server's copy repository, which then checks it out read-only. Returns the
/// commit sent.
pub fn send(client: &RemoteClient, config: &ServerConfig, repo: &Path, commit: &str, excluded: &[String], previous: Option<&str>) -> anyhow::Result<String> {
    let copy = server_commit(repo, commit, excluded, previous)?;
    if previous == Some(copy.as_str()) {
        return Ok(copy);
    }
    let target: PathBuf = client.call("memory.copy.target", json!({}))?;
    let mut push = git(repo);
    push.env("GIT_SSH_COMMAND", config.git_ssh_command()).args(["push", "--quiet", "--force", "--no-verify", &config.git_url(&target), &format!("{copy}:{SERVER_REF}")]);
    run(push).context("could not send the memory to the server")?;
    client.call::<serde_json::Value>("memory.copy.update", json!({ "commit": copy }))?;
    Ok(copy)
}

/// Check out `commit` of the copy repository `repo` into `into`, read-only for everyone (the
/// server side of [`send`]). What was there goes.
pub fn check_out_read_only(repo: &Path, commit: &str, into: &Path) -> anyhow::Result<()> {
    if into.exists() {
        make_writable(into);
        std::fs::remove_dir_all(into).with_context(|| format!("could not clear {}", into.display()))?;
    }
    std::fs::create_dir_all(into)?;
    let mut checkout = git(repo);
    checkout.arg("--work-tree").arg(into).args(["checkout", "-f", commit, "--", "."]);
    // A bare copy repository: the index it uses is its own.
    run(checkout)?;
    make_read_only(into);
    Ok(())
}

fn walk(dir: &Path, f: &mut impl FnMut(&Path, bool)) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let is_dir = path.is_dir();
            if is_dir {
                walk(&path, f);
            }
            f(&path, is_dir);
        }
    }
}

fn set_readonly(path: &Path, readonly: bool) {
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(readonly);
        let _ = std::fs::set_permissions(path, perms);
    }
}

fn make_read_only(dir: &Path) {
    walk(dir, &mut |path, _| set_readonly(path, true));
    set_readonly(dir, true);
}

/// Remove the read-only copy and its repository (a server that takes memory entries now, M57).
pub fn remove_server_copy(dir: &Path, repo: &Path) {
    if dir.exists() {
        make_writable(dir);
        let _ = std::fs::remove_dir_all(dir);
    }
    let _ = std::fs::remove_dir_all(repo);
}

fn make_writable(dir: &Path) {
    set_readonly(dir, false);
    // Folders first, so their files can be reached and changed.
    walk(dir, &mut |path, is_dir| {
        if is_dir {
            set_readonly(path, false);
        }
    });
    walk(dir, &mut |path, _| set_readonly(path, false));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(repo: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(repo).args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"]).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn key(text: &str) -> ProjectKey {
        ProjectKey::parse(text).unwrap()
    }

    #[test]
    fn local_only_projects_and_their_employer_are_left_out() {
        let employers = vec![(key("secret-111111"), Some("acme".to_string())), (key("open-222222"), Some("globex".to_string())), (key("mixed-333333"), Some("globex".to_string()))];
        let paths = excluded_paths(&[key("secret-111111"), key("mixed-333333")], &employers);
        assert!(paths.contains(&"projects/secret-111111".to_string()));
        assert!(paths.contains(&"org/acme".to_string()), "used only by a local-only project");
        assert!(!paths.iter().any(|p| p == "org/globex"), "an open project uses it too");
    }

    /// M20.3 with the real memory writer: no text, name or path of a "stays on this Mac" project
    /// is in any object the server receives. Not its files' text, a know-how topic's file name or
    /// heading, its key, its employer's name or people, a work file's name, a proposal for it, a
    /// profile entry kept to its employer, a forgotten entry, nor the index that lists them.
    #[test]
    fn nothing_of_a_local_only_project_reaches_the_server() {
        use crate::memstore::tests_support::{Scratch, private_memory};
        use crate::memstore::{Memory, MemoryHead};
        use crate::store::Store as _;
        let scratch = Scratch::new();
        let memory = Memory::open();
        let private = private_memory(&memory);
        let head = MemoryHead::load().unwrap().commit.unwrap();
        let all_mac = scratch.git(memory.dir(), &["cat-file", "--batch-all-objects", "--batch"]);
        for word in &private.secret_words {
            assert!(all_mac.contains(word.as_str()), "the test wrote {word} on the Mac");
        }
        let copy = server_commit(memory.dir(), &head, &private.excluded, None).unwrap();
        let server = scratch.at("server.git");
        sh(&scratch.at(""), &["init", "-q", "--bare", server.to_str().unwrap()]);
        sh(memory.dir(), &["push", "-q", server.to_str().unwrap(), &format!("{copy}:{SERVER_REF}")]);
        let all = sh(&server, &["cat-file", "--batch-all-objects", "--batch"]);
        for word in &private.secret_words {
            assert!(!all.contains(word.as_str()), "{word} reached the server");
        }
        for word in &private.open_words {
            assert!(all.contains(word.as_str()), "{word} goes");
        }
    }

    #[test]
    fn local_only_project_never_in_server_objects() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("memory");
        std::fs::create_dir_all(repo.join("projects/secret-111111")).unwrap();
        std::fs::create_dir_all(repo.join("projects/open-222222")).unwrap();
        std::fs::create_dir_all(repo.join("org/acme")).unwrap();
        std::fs::write(repo.join("projects/secret-111111/PROJECT.md"), "- The launch codename is BLUEHERON.\n").unwrap();
        std::fs::write(repo.join("org/acme/people.md"), "- Pat runs ACMEPAYROLL.\n").unwrap();
        std::fs::write(repo.join("projects/open-222222/PROJECT.md"), "- Builds with cargo.\n").unwrap();
        std::fs::write(repo.join("INDEX.md"), "index\n").unwrap();
        sh(&repo, &["init", "-q", "-b", "main"]);
        sh(&repo, &["add", "-A"]);
        sh(&repo, &["commit", "-qm", "memory"]);
        let head = sh(&repo, &["rev-parse", "HEAD"]);
        let excluded = excluded_paths(&[key("secret-111111")], &[(key("secret-111111"), Some("acme".into())), (key("open-222222"), None)]);
        let copy = server_commit(&repo, &head, &excluded, None).unwrap();
        assert_ne!(copy, head, "a history of its own");

        // What the server would receive: a fresh repository fetching only the copy's commit.
        let server = dir.path().join("server.git");
        sh(dir.path(), &["init", "-q", "--bare", server.to_str().unwrap()]);
        sh(&repo, &["push", "-q", server.to_str().unwrap(), &format!("{copy}:{SERVER_REF}")]);
        let all = sh(&server, &["cat-file", "--batch-all-objects", "--batch"]);
        assert!(!all.contains("BLUEHERON") && !all.contains("ACMEPAYROLL"), "no local-only text in any object the server has");
        assert!(all.contains("Builds with cargo"));

        // The same tree again: no new commit. A change: a new commit on the copy's own history.
        assert_eq!(server_commit(&repo, &head, &excluded, Some(&copy)).unwrap(), copy);
        std::fs::write(repo.join("projects/open-222222/PROJECT.md"), "- Builds with cargo and make.\n").unwrap();
        sh(&repo, &["commit", "-qam", "more"]);
        let next = server_commit(&repo, &sh(&repo, &["rev-parse", "HEAD"]), &excluded, Some(&copy)).unwrap();
        assert_eq!(sh(&repo, &["rev-parse", &format!("{next}^")]), copy);

        // Checked out read-only on the server.
        sh(&repo, &["push", "-q", server.to_str().unwrap(), &format!("{next}:{SERVER_REF}")]);
        let into = dir.path().join("memory-server");
        check_out_read_only(&server, &next, &into).unwrap();
        assert!(into.join("projects/open-222222/PROJECT.md").is_file());
        assert!(!into.join("projects/secret-111111").exists());
        assert!(std::fs::write(into.join("projects/open-222222/PROJECT.md"), "x").is_err(), "server_copy_is_read_only");
        // An update replaces it.
        check_out_read_only(&server, &copy, &into).unwrap();
        assert_eq!(std::fs::read_to_string(into.join("projects/open-222222/PROJECT.md")).unwrap(), "- Builds with cargo.\n");
    }
}
