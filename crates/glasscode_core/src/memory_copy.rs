//! The server's read-only copy of the trusted memory (slice M20.3, step 3). The Mac builds a
//! separate tree from its memory that leaves out every project marked "stays on this Mac" and
//! every `org/<employer>/` folder used only by such projects, commits it on a history of its own
//! (`git commit-tree` over the filtered tree, never over the Mac's commits), and pushes that
//! history to the server, which checks it out read-only (`memory.copy.*` in glasscoded). So no
//! local-only text is ever in an object the server receives.
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
    cmd.arg("-C").arg(repo).args(["-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false"]).env("GIT_TERMINAL_PROMPT", "0");
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

/// The files outside the projects' folders that name memory files: the index (a line per file,
/// with its title and headings) and the inbox (a proposal or a rejection for a file, under a
/// heading that names it).
const NAMING: [&str; 3] = ["INDEX.md", "inbox/proposals.md", "inbox/rejected.md"];

/// `text` of the file `rel` without what it says about `excluded`: the index made again without
/// the lines of their files; the inbox without its entries for them and the sections named after
/// them. None: nothing to take out (the file goes as it is).
fn without(rel: &str, text: &str, excluded: &[String]) -> Option<String> {
    use crate::memstore::entry::Line;
    let names = |path: &str| {
        let path = path.trim();
        excluded.iter().any(|ex| path == ex || path.strip_prefix(ex.as_str()).is_some_and(|rest| rest.starts_with('/')))
    };
    if excluded.is_empty() {
        return None;
    }
    if rel == "INDEX.md" {
        // Made again from its lines of the files the copy keeps: anything else in it (a line the
        // parser does not know, text added by hand) could name anything, so it does not go.
        let mut lines = crate::memstore::index::parse(text);
        lines.retain(|file, _| !names(file));
        return Some(crate::memstore::index::render(&lines));
    }
    let doc = crate::memstore::Doc::parse(text);
    let mut kept = crate::memstore::Doc { lines: Vec::new(), newline: doc.newline };
    let mut dropping = false;
    for line in &doc.lines {
        if let Line::Text(heading) = line
            && heading.starts_with('#')
        {
            dropping = heading.strip_prefix("## ").is_some_and(|name| names(name));
        }
        let for_excluded = matches!(line, Line::Entry(entry) if entry.header.to.as_deref().is_some_and(|to| names(to)));
        if !dropping && !for_excluded {
            kept.lines.push(line.clone());
        }
    }
    (kept.lines.len() != doc.lines.len()).then(|| kept.render())
}

/// The server copy's commit for `commit` of the memory `repo`: its tree without `excluded`,
/// committed with `previous` (the copy's last commit) as its only parent, so the copy's history
/// holds filtered trees only. The same tree as `previous` gives `previous` back.
pub fn server_commit(repo: &Path, commit: &str, excluded: &[String], previous: Option<&str>) -> anyhow::Result<String> {
    let index = tempfile::NamedTempFile::new().context("no temporary index")?;
    let with_index = |args: &[&str]| {
        let mut cmd = git(repo);
        cmd.env("GIT_INDEX_FILE", index.path()).args(args);
        cmd
    };
    run(with_index(&["read-tree", commit]))?;
    for path in excluded {
        run(with_index(&["rm", "-r", "-q", "--cached", "--ignore-unmatch", "--", path]))?;
    }
    // The files the copy keeps that name what it leaves out: their lines about it go too.
    for rel in NAMING {
        let Ok(text) = run({
            let mut cmd = git(repo);
            cmd.args(["show", &format!("{commit}:{rel}")]);
            cmd
        }) else {
            continue;
        };
        let Some(kept) = without(rel, &text, excluded) else { continue };
        let file = tempfile::NamedTempFile::new().context("no temporary file")?;
        std::fs::write(file.path(), kept).context("could not write the filtered file")?;
        let blob = run({
            let mut cmd = git(repo);
            cmd.args(["hash-object", "-w", "--"]).arg(file.path());
            cmd
        })?;
        run(with_index(&["update-index", "--cacheinfo", &format!("100644,{blob},{rel}")]))?;
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
        let out = std::process::Command::new("git").arg("-C").arg(repo).args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"]).args(args).output().unwrap();
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

    /// The index and the inbox name memory files: what they say of a local-only project (its
    /// files' titles and headings, a proposal for it) is not in the copy; the rest is.
    #[test]
    fn the_index_and_the_inbox_leave_out_local_only_projects() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("memory");
        std::fs::create_dir_all(repo.join("projects/secret-111111")).unwrap();
        std::fs::create_dir_all(repo.join("projects/open-222222")).unwrap();
        std::fs::create_dir_all(repo.join("inbox")).unwrap();
        std::fs::write(repo.join("projects/secret-111111/decisions.md"), "# Decisions\n\n## BLUEHERON launch\n").unwrap();
        std::fs::write(repo.join("projects/open-222222/decisions.md"), "# Decisions\n").unwrap();
        let index = "# Index\n\nGenerated by Glasscode from the memory files; it changes with them.\n\n## projects/open-222222\n\n- projects/open-222222/decisions.md: Decisions, 0 entries\n\n## projects/secret-111111\n\n- projects/secret-111111/decisions.md: Decisions (BLUEHERON launch), 0 entries\n";
        std::fs::write(repo.join("INDEX.md"), index).unwrap();
        let entry = |id: &str, to: &str, text: &str| {
            let mut header = crate::memstore::Header::new(id.into(), crate::memstore::Status::Confirmed, crate::memstore::Kind::Fact, crate::memstore::Source::Words("w-1".into()), crate::identity::Hlc::zero(crate::paths::MachineId::parse("0123456789abcdef").unwrap()));
            header.to = Some(to.into());
            crate::memstore::Entry { text: text.into(), header }
        };
        let mut inbox = crate::memstore::Doc::new("Proposals", &[]);
        inbox.add(Some("projects/secret-111111/decisions.md"), entry("m-aaaa", "projects/secret-111111/decisions.md", "The pilot is BLUEHERON."));
        inbox.add(Some("projects/open-222222/decisions.md"), entry("m-bbbb", "projects/open-222222/decisions.md", "Builds with cargo."));
        std::fs::write(repo.join("inbox/proposals.md"), inbox.render()).unwrap();
        sh(&repo, &["init", "-q", "-b", "main"]);
        sh(&repo, &["add", "-A"]);
        sh(&repo, &["commit", "-qm", "memory"]);
        let head = sh(&repo, &["rev-parse", "HEAD"]);
        let excluded = excluded_paths(&[key("secret-111111")], &[(key("secret-111111"), None), (key("open-222222"), None)]);
        let copy = server_commit(&repo, &head, &excluded, None).unwrap();
        let server = dir.path().join("server.git");
        sh(dir.path(), &["init", "-q", "--bare", server.to_str().unwrap()]);
        sh(&repo, &["push", "-q", server.to_str().unwrap(), &format!("{copy}:{SERVER_REF}")]);
        let all = sh(&server, &["cat-file", "--batch-all-objects", "--batch"]);
        assert!(!all.contains("BLUEHERON") && !all.contains("secret-111111"), "{all}");
        let index = sh(&repo, &["show", &format!("{copy}:INDEX.md")]);
        assert!(index.contains("- projects/open-222222/decisions.md: Decisions, 0 entries"), "{index}");
        let inbox = sh(&repo, &["show", &format!("{copy}:inbox/proposals.md")]);
        assert!(inbox.contains("Builds with cargo.") && inbox.contains("## projects/open-222222/decisions.md"), "{inbox}");
        // Nothing left out: the files go as they are.
        assert_eq!(without("inbox/proposals.md", &inbox, &excluded), None);
        assert_eq!(without("INDEX.md", index.as_str(), &[]), None);
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
