//! The session profile's tests (M22).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;
use crate::gate::{Level, ProjectPolicy};
use crate::model::AgentKind;
use crate::paths;

fn facts(root: &Path) -> ProfileFacts {
    let worktree = paths::root().join("worktrees").join("app").join("feat-a");
    ProfileFacts {
        worktree: worktree.clone(),
        other_worktrees: vec![paths::root().join("worktrees").join("app").join("feat-b")],
        git: Some(GitFacts { common_dir: root.join("repo").join(".git"), git_dir: root.join("repo").join(".git").join("worktrees").join("feat-a"), branch: "feat/a".into() }),
        notes: Some(paths::root().join("agent-notes").join("app-3f9a1c").join("claude")),
        env_files: vec![worktree.join(".env")],
        hook: Some(HookProgram { program: root.join("Glasscode"), prefix: vec!["cli".into()] }),
        events: paths::root().join("events").join("s1.jsonl"),
        permission_file: paths::root().join("settings").join("s1.permission.json"),
        registries: vec!["registry.npmjs.org".into(), "crates.io".into()],
        forge: vec!["github.com".into(), "api.github.com".into()],
        default_branch: Some("main".into()),
        socket: paths::root().join("control.sock"),
    }
}

fn profile(origin: SessionOrigin, policy: &ProjectPolicy) -> SessionProfile {
    let dir = tempfile::tempdir().unwrap();
    session_profile(origin, AgentKind::Claude, policy, &facts(dir.path()))
}

fn deny(profile: &SessionProfile) -> Vec<String> {
    profile.claude_settings["permissions"]["deny"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
}

/// D21: the profile is made from the path classes only: no entry of the layout is written in
/// `profile.rs` (outside its tests), and every denied path is a row's.
#[test]
fn profile_from_classes_only() {
    let source = include_str!("../profile.rs");
    // Its code: the comments name paths to say what is denied, the code never does.
    let source: String = source[..source.find("#[cfg(test)]").unwrap()].lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    for row in paths::layout() {
        let entry = row.entry;
        assert!(!source.contains(&format!("\"{entry}\"")) && !source.contains(&format!("\"~/{entry}")) && !source.contains(&format!("/{entry}\"")), "profile.rs names {entry}");
    }
    for literal in ["/Users/", "/home/", "~/.", "C:\\\\"] {
        assert!(!source.contains(literal), "profile.rs holds {literal}");
    }
    let p = profile(SessionOrigin::Automation, &ProjectPolicy::default());
    let rows: Vec<PathBuf> = paths::layout().iter().map(|r| r.path()).collect();
    let f = facts(Path::new("/x"));
    for path in p.claude_settings["sandbox"]["filesystem"]["denyRead"].as_array().unwrap().iter().map(|v| PathBuf::from(v.as_str().unwrap())) {
        assert!(rows.contains(&path) || f.other_worktrees.contains(&path) || f.env_files.contains(&path), "{} is no row", path.display());
    }
}

#[test]
fn automation_denies_authority_and_credentials() {
    let p = profile(SessionOrigin::Automation, &ProjectPolicy::default());
    let rules = deny(&p);
    let has = |tool: &str, path: &Path| rules.iter().any(|r| r.starts_with(&format!("{tool}(//")) && r.contains(path.to_string_lossy().trim_start_matches('/')));
    for path in [paths::authority_dir(), paths::home().join(".ssh"), paths::home().join(".config").join("gh"), paths::root().join("state.json"), paths::root().join("events"), paths::root().join("journal")] {
        for tool in ["Read", "Edit"] {
            assert!(has(tool, &path), "{tool} {}", path.display());
        }
    }
    // No `Write(...)` rule: Claude says each one never matches (Edit covers it) at every start.
    assert!(!rules.iter().any(|r| r.starts_with("Write(")), "{rules:?}");
    // The trusted memory reads, never writes; the worktree's .env at launch is denied.
    let memory = paths::memory_dir();
    assert!(has("Edit", &memory) && !has("Read", &memory));
    assert!(has("Read", &facts(Path::new("/x")).env_files[0]));
    // The other worktree too, for automation only; and the sandbox denies the same for the shell.
    let other = facts(Path::new("/x")).other_worktrees[0].clone();
    assert!(has("Read", &other));
    let other_text = other.to_string_lossy().trim_start_matches('/').to_string();
    assert!(!deny(&profile(SessionOrigin::Person, &ProjectPolicy::default())).iter().any(|r| r.contains(&other_text)));
    let sandbox = &p.claude_settings["sandbox"];
    let denied: Vec<&str> = sandbox["filesystem"]["denyRead"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert!(denied.contains(&paths::authority_dir().to_string_lossy().as_ref()));
    assert_eq!(sandbox["allowUnsandboxedCommands"], json!(false));
    assert_eq!(sandbox["failIfUnavailable"], json!(true));
    // Codex: the same classes denied by path in its permission profile.
    let codex = session_profile(SessionOrigin::Automation, AgentKind::Codex, &ProjectPolicy::default(), &facts(Path::new("/x"))).codex_args.join(" ");
    assert!(codex.contains(&format!("{}=\"deny\"", json!(paths::authority_dir().to_string_lossy()))), "{codex}");
    assert!(codex.contains(&format!("{}=\"read\"", json!(memory.to_string_lossy()))), "{codex}");
}

/// P07: every absolute rule starts with `//` (one `/` is relative to the settings file).
#[test]
fn deny_rules_are_absolute() {
    for origin in [SessionOrigin::Person, SessionOrigin::Automation, SessionOrigin::Quarantine] {
        for rule in deny(&profile(origin, &ProjectPolicy::default())) {
            if let Some((_, inside)) = rule.split_once('(') {
                assert!(inside.starts_with("//"), "{rule}");
            }
        }
    }
}

#[test]
fn quarantine_network_forge_only() {
    let q = profile(SessionOrigin::Quarantine, &ProjectPolicy::default());
    assert_eq!(q.claude_settings["sandbox"]["network"]["allowedDomains"], json!(["github.com", "api.github.com"]));
    let a = profile(SessionOrigin::Automation, &ProjectPolicy::default());
    assert_eq!(a.claude_settings["sandbox"]["network"]["allowedDomains"], json!(["github.com", "api.github.com", "registry.npmjs.org", "crates.io"]));
    // Codex: the network off for both, and no control socket (below).
    let codex = session_profile(SessionOrigin::Quarantine, AgentKind::Codex, &ProjectPolicy::default(), &facts(Path::new("/x"))).codex_args.join(" ");
    assert!(codex.contains("permissions.glasscode.network={\"enabled\"=false}"), "{codex}");
}

/// A session the manager drives cannot reach the control socket from its sandbox: there, any
/// program may leave out a session's signature and pass for one outside Glasscode. Its `glasscode`
/// commands go as request files in its own worktree, the app's to attribute
/// (`caller::by_channel_proven`). The person's sessions keep the socket.
#[test]
fn no_control_socket_in_a_restricted_sandbox() {
    let socket = facts(Path::new("/x")).socket.to_string_lossy().into_owned();
    for (origin, reaches) in [(SessionOrigin::Automation, false), (SessionOrigin::Quarantine, false), (SessionOrigin::Person, true)] {
        let p = profile(origin, &ProjectPolicy::default());
        let allowed = p.claude_settings["sandbox"]["network"]["allowUnixSockets"].as_array().unwrap().iter().any(|v| v.as_str() == Some(socket.as_str()));
        assert_eq!(allowed, reaches, "{origin:?}");
        // Codex's restricted profile opens no unix socket (its network is off); the person's
        // session has no permission profile at all.
        let codex = session_profile(origin, AgentKind::Codex, &ProjectPolicy::default(), &facts(Path::new("/x"))).codex_args.join(" ");
        assert!(!codex.contains("unix_sockets"), "{origin:?}: {codex}");
    }
}

#[test]
fn bypass_disabled_for_automation() {
    for origin in [SessionOrigin::Automation, SessionOrigin::Quarantine] {
        let p = profile(origin, &ProjectPolicy::default());
        assert_eq!(p.claude_settings["permissions"]["disableBypassPermissionsMode"], json!("disable"));
        assert_eq!(p.claude_settings["crossSessionInbound"], json!("refuse"));
        assert_eq!(p.claude_settings["disableClaudeAiConnectors"], json!(true));
        assert!(p.claude_args.contains(&"--strict-mcp-config".to_string()));
        assert!(!p.pretrust);
    }
    let person = profile(SessionOrigin::Person, &ProjectPolicy::default());
    assert!(person.claude_settings["permissions"].get("disableBypassPermissionsMode").is_none() && person.pretrust);
    // The launch drops a bypass mode for them, and Codex runs in its permission profile, never
    // with the mode flags the session record carries.
    let session = |agent: AgentKind, mode: &str| -> crate::model::Session {
        serde_json::from_value(json!({ "id": "s1", "project_id": "p", "worktree": std::env::temp_dir(), "branch": "b", "title": "t", "agent": agent, "mode": mode, "created_at": 0 })).unwrap()
    };
    let p = profile(SessionOrigin::Automation, &ProjectPolicy::default());
    let launch = crate::agents::launch_with(&session(AgentKind::Claude, "bypass"), None, &Default::default(), Some(&p));
    assert!(!launch.args.iter().any(|a| a == "bypassPermissions"), "{:?}", launch.args);
    let codex = session_profile(SessionOrigin::Automation, AgentKind::Codex, &ProjectPolicy::default(), &facts(Path::new("/x")));
    let launch = crate::agents::launch_with(&session(AgentKind::Codex, "full-auto"), None, &Default::default(), Some(&codex));
    assert!(!launch.args.iter().any(|a| a.contains("danger") || a == "--dangerously-bypass-approvals-and-sandbox"));
    assert!(!launch.args.windows(2).any(|w| w == ["--sandbox", "workspace-write"]), "the mode's flags give way to the profile: {:?}", launch.args);
}

/// crit-15 and P08: every hook command is an absolute, quoted program; the events file is an
/// argument; a project's settings cannot turn the hooks off; a changed launcher is refused.
#[test]
fn hook_command_is_absolute_and_checked() {
    let dir = tempfile::tempdir().unwrap();
    let p = session_profile(SessionOrigin::Automation, AgentKind::Claude, &ProjectPolicy::default(), &facts(dir.path()));
    assert_eq!(p.claude_settings["disableAllHooks"], json!(false));
    for event in ["PreToolUse", "PermissionRequest"] {
        let command = p.claude_settings["hooks"][event][0]["hooks"][0]["command"].as_str().unwrap();
        let program = command.trim_start_matches('\'');
        assert!(program.starts_with(&dir.path().to_string_lossy().to_string()), "{command}");
        assert!(command.contains(" hook ") || command.contains(" 'hook' "), "{command}");
        assert!(command.contains("--events") && !command.contains("$GLASSCODE"), "{command}");
    }
    assert_eq!(p.claude_settings["hooks"]["PreToolUse"][0]["matcher"], json!("*"));
    // The person's profile gets the permission hook, not the gate.
    let person = session_profile(SessionOrigin::Person, AgentKind::Claude, &ProjectPolicy::default(), &facts(dir.path()));
    assert!(person.claude_settings["hooks"].get("PreToolUse").is_none() && person.claude_settings["hooks"].get("PermissionRequest").is_some());
    // The launcher check: absolute, and its bytes as expected.
    let launcher = dir.path().join("glasscode");
    std::fs::write(&launcher, b"#!/bin/sh\nexec app cli \"$@\"\n").unwrap();
    use sha2::{Digest, Sha256};
    let sha: String = Sha256::digest(std::fs::read(&launcher).unwrap()).iter().map(|b| format!("{b:02x}")).collect();
    assert!(hook_program_checked(&launcher, Some(&sha)));
    std::fs::write(&launcher, b"#!/bin/sh\nexec evil \"$@\"\n").unwrap();
    assert!(!hook_program_checked(&launcher, Some(&sha)));
    assert!(!hook_program_checked(Path::new("glasscode"), None));
    // Merged over Glasscode's own reporting settings, the reporting hooks stay.
    let merged: Value = serde_json::from_str(&crate::agents::with_profile(r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/a/b cli hook --stamp"}]}]},"theme":"auto"}"#, &p.claude_settings)).unwrap();
    assert!(merged["hooks"]["Stop"].is_array() && merged["hooks"]["PreToolUse"].is_array() && merged["theme"] == "auto");
}

/// P08's variant A: a project's `.claude/settings.json` with `disableAllHooks: true` loses to the
/// profile's `false`, which every origin's settings carry.
#[test]
fn project_settings_cannot_disable_glasscode_hooks() {
    for origin in [SessionOrigin::Person, SessionOrigin::Automation, SessionOrigin::Quarantine] {
        let merged: Value = serde_json::from_str(&crate::agents::with_profile(r#"{"hooks":{}}"#, &profile(origin, &ProjectPolicy::default()).claude_settings)).unwrap();
        assert_eq!(merged["disableAllHooks"], json!(false), "{origin:?}");
    }
}

#[test]
fn permission_hook_never_returns_updated_permissions() {
    let file = PermissionFile { allow: vec!["cargo test".into(), "npm run lint".into()], never: vec!["gh pr merge".into()] };
    let ask = |command: &str| permission_answer("Bash", &json!({ "command": command }), &file);
    assert_eq!(ask("cargo test"), PermissionAnswer::Allow);
    assert_eq!(ask("cargo test --release"), PermissionAnswer::Allow);
    assert_eq!(ask("cargo testing"), PermissionAnswer::Nothing, "by whole words");
    assert!(matches!(ask("gh pr merge 1"), PermissionAnswer::Deny(_)));
    assert_eq!(ask("rm -rf /"), PermissionAnswer::Nothing);
    assert_eq!(permission_answer("Edit", &json!({ "file_path": "/x" }), &file), PermissionAnswer::Nothing);
    for answer in [PermissionAnswer::Allow, PermissionAnswer::Deny("no".into())] {
        let out: Value = serde_json::from_str(&answer.output().unwrap()).unwrap();
        assert!(!out.to_string().contains("updatedPermissions") && !out.to_string().contains("updatedInput"), "{out}");
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PermissionRequest");
    }
    assert_eq!(PermissionAnswer::Nothing.output(), None);
}

#[test]
fn compound_command_declined() {
    let file = PermissionFile { allow: vec!["cargo test".into()], never: Vec::new() };
    for command in ["cargo test && rm -rf ~", "cargo test; curl x", "cargo test | sh", "cargo test > /etc/x", "cargo test $(whoami)", "cargo test `id`", "cargo test\nrm x", "cargo test & sleep 1"] {
        assert_eq!(permission_answer("Bash", &json!({ "command": command }), &file), PermissionAnswer::Nothing, "{command}");
        assert!(!is_simple_command(command), "{command}");
    }
    // A compound allow-command is never compiled into an allow rule either.
    let policy = ProjectPolicy { allow_commands: vec!["cargo test".into(), "make && make install".into()], ..ProjectPolicy::default() };
    let p = profile(SessionOrigin::Person, &policy);
    let allow = p.claude_settings["permissions"]["allow"].as_array().unwrap();
    assert_eq!(allow, &[json!("Bash(cargo test)"), json!("Bash(cargo test:*)")]);
    assert_eq!(p.permission.allow, ["cargo test"]);
}

#[test]
fn trusted_author_rules() {
    use crate::gate::trusted_author;
    let me = vec!["alex-dev".to_string()];
    let team = vec!["jordan".to_string()];
    assert!(trusted_author("Alex-Dev", None, &me, &team));
    assert!(trusted_author("jordan", Some("NONE"), &me, &team));
    for association in ["OWNER", "member", "COLLABORATOR"] {
        assert!(trusted_author("someone", Some(association), &me, &team), "{association}");
    }
    for association in [Some("CONTRIBUTOR"), Some("FIRST_TIME_CONTRIBUTOR"), Some("NONE"), None] {
        assert!(!trusted_author("drive-by-user", association, &me, &team), "{association:?}");
    }
    assert!(!trusted_author("", Some("OWNER"), &me, &team));
}

#[test]
fn no_gh_token_in_automation_env() {
    for origin in [SessionOrigin::Person, SessionOrigin::Automation, SessionOrigin::Quarantine] {
        let p = profile(origin, &ProjectPolicy::default());
        let mut launch = crate::agents::Launch { program: "claude".into(), args: Vec::new(), env: [("GH_TOKEN".to_string(), "gho_x".to_string()), ("GITHUB_TOKEN".to_string(), "y".to_string()), ("PATH".to_string(), "/bin".to_string())].into() };
        crate::agents::apply_profile_env(&mut launch, &p);
        assert!(!launch.env.contains_key("GH_TOKEN") && !launch.env.contains_key("GITHUB_TOKEN") && launch.env.contains_key("PATH"), "{origin:?}");
        assert!(!p.claude_settings.to_string().contains("gho_"));
    }
}

/// D19: Claude's auto-memory goes to the project's agent notes; Codex's memories are off for
/// Careful and local-only projects; the old folder is copied once and never moved.
#[test]
fn memory_is_redirected() {
    let p = profile(SessionOrigin::Person, &ProjectPolicy::default());
    assert!(p.claude_settings["autoMemoryDirectory"].as_str().unwrap().ends_with("claude"));
    let codex = |policy: &ProjectPolicy| session_profile(SessionOrigin::Person, AgentKind::Codex, policy, &facts(Path::new("/x"))).codex_args.join(" ");
    assert!(codex(&ProjectPolicy::default()).contains("features.memories=false"), "Careful");
    assert!(!codex(&ProjectPolicy { level: Level::Standard, ..ProjectPolicy::default() }).contains("features.memories"));
    assert!(codex(&ProjectPolicy { level: Level::Autopilot, local_only: true, ..ProjectPolicy::default() }).contains("features.memories=false"));
    let dir = tempfile::tempdir().unwrap();
    let account = dir.path().join("account");
    let repo = Path::new("/work/app");
    let old = claude_memory_dir(&account, repo);
    assert!(old.ends_with("projects/-work-app/memory"));
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("MEMORY.md"), "- a lesson").unwrap();
    std::fs::write(old.join("lesson.md"), "x").unwrap();
    let notes = dir.path().join("notes");
    assert_eq!(copy_claude_memory_once(&old, &notes).unwrap(), 2);
    assert_eq!(copy_claude_memory_once(&old, &notes).unwrap(), 0, "once");
    assert!(old.join("MEMORY.md").exists(), "never moved");
}

#[test]
fn the_gate_refuses_what_a_session_never_does() {
    let ctx = GateCtx { origin: Some(SessionOrigin::Automation), branch: "feat/a".into(), default_branch: "main".into(), worktree: paths::root().join("worktrees").join("app").join("feat-a"), browser: false };
    let bash = |c: &str| gate_tool(&ctx, "Bash", &json!({ "command": c }));
    for refused in ["gh pr merge 1 --admin", "cd x && gh pr merge 1", "git push --force origin feat/a", "git push -f", "git push origin +feat/a", "git push origin main", "git push origin HEAD:main", "git push origin --delete feat/a", "cat ~/.ssh/id_ed25519", "head -c1 $HOME/.aws/credentials", "claude -p hi", "sudo gh pr merge 2", "gh pr ready 3", "gh pr create --fill"] {
        assert!(bash(refused).is_err(), "{refused}");
    }
    for allowed in ["git push origin feat/a", "git status", "cargo test", "gh pr view 1", "ls src/", "curl https://example.com/x", "gh pr create --draft --fill"] {
        assert!(bash(allowed).is_ok(), "{allowed}: {:?}", bash(allowed));
    }
    assert!(gate_tool(&ctx, "Read", &json!({ "file_path": paths::home().join(".ssh").join("id_rsa") })).is_err());
    assert!(gate_tool(&ctx, "Read", &json!({ "file_path": paths::authority_dir().join("policy.json") })).is_err());
    assert!(gate_tool(&ctx, "Edit", &json!({ "file_path": paths::memory_dir().join("x.md") })).is_err());
    assert!(gate_tool(&ctx, "Read", &json!({ "file_path": paths::memory_dir().join("x.md") })).is_ok());
    assert!(gate_tool(&ctx, "Edit", &json!({ "file_path": ctx.worktree.join("src").join("main.rs") })).is_ok());
    for tool in ["SendMessage", "ListAgents", "WebFetch", "mcp__glasscode-browser__open"] {
        assert!(gate_tool(&ctx, tool, &json!({})).is_err(), "{tool}");
    }
    assert!(gate_tool(&ctx, "mcp__glasscode__board_add", &json!({})).is_ok());
    // The person's sessions pass: their own rules apply.
    let person = GateCtx { origin: Some(SessionOrigin::Person), ..ctx.clone() };
    assert!(gate_tool(&person, "Bash", &json!({ "command": "gh pr merge 1" })).is_ok());
}

/// P07 and O16: the automation profile writes only its own branch's ref, never `refs` or `logs`
/// as a whole, `packed-refs`, `config` or `hooks`; other worktrees are denied one by one.
#[test]
fn automation_profile_cannot_move_other_branches() {
    let root = Path::new("/r");
    let p = session_profile(SessionOrigin::Automation, AgentKind::Codex, &ProjectPolicy::default(), &facts(root));
    let fs = p.codex_args.iter().find(|a| a.starts_with("permissions.glasscode.filesystem=")).unwrap();
    let git = root.join("repo").join(".git");
    let write = |path: &Path| fs.contains(&format!("{}=\"write\"", json!(path.to_string_lossy())));
    assert!(write(&git.join("refs").join("heads").join("feat/a")));
    assert!(write(&git.join("refs").join("heads").join("feat/a.lock")));
    assert!(write(&git.join("logs").join("refs").join("heads").join("feat/a")));
    assert!(write(&git.join("objects")) && write(&git.join("worktrees").join("feat-a")));
    for never in [git.join("refs"), git.join("refs").join("heads"), git.join("logs"), git.join("packed-refs"), git.join("config"), git.join("hooks"), git.clone()] {
        assert!(!write(&never), "{}", never.display());
    }
    let other = &facts(root).other_worktrees[0];
    assert!(fs.contains(&format!("{}=\"deny\"", json!(other.to_string_lossy()))));
    assert!(!fs.contains(&format!("{}=\"deny\"", json!(other.parent().unwrap().to_string_lossy()))), "never the parent");
    assert!(!fs.contains("**"), "no deny glob");
}

/// The case M01 did not try: the own branch's ref packed, with no loose file. A commit writes the
/// loose ref (and its log), which are the paths the profile grants; `packed-refs` stays as it was.
#[test]
fn automation_commit_with_packed_own_ref() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let git = |cwd: &Path, args: &[&str]| {
        let out = std::process::Command::new("git").arg("-C").arg(cwd).args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"]).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let wt = dir.path().join("feat-a");
    git(&repo, &["worktree", "add", "-q", "-b", "feat/a", wt.to_str().unwrap()]);
    git(&repo, &["pack-refs", "--all"]);
    let loose = repo.join(".git").join("refs").join("heads").join("feat").join("a");
    assert!(!loose.exists(), "the own ref is packed");
    let packed = std::fs::read(repo.join(".git").join("packed-refs")).unwrap();
    std::fs::write(wt.join("x"), "x").unwrap();
    git(&wt, &["add", "x"]);
    git(&wt, &["commit", "-q", "-m", "x"]);
    assert!(loose.is_file(), "the commit wrote the loose ref, a granted path");
    assert_eq!(std::fs::read(repo.join(".git").join("packed-refs")).unwrap(), packed, "packed-refs untouched");
    // And those are the profile's grants for this worktree.
    let facts = ProfileFacts { worktree: wt.clone(), git: GitFacts::of(&wt, "feat/a"), ..ProfileFacts::default() };
    let git_facts = facts.git.clone().unwrap();
    assert_eq!(std::fs::canonicalize(&git_facts.common_dir).unwrap(), std::fs::canonicalize(repo.join(".git")).unwrap());
    let p = session_profile(SessionOrigin::Automation, AgentKind::Codex, &ProjectPolicy::default(), &facts);
    let fs = p.codex_args.iter().find(|a| a.starts_with("permissions.glasscode.filesystem=")).unwrap();
    assert!(fs.contains(&json!(git_facts.common_dir.join("refs").join("heads").join("feat/a").to_string_lossy()).to_string()), "{fs}");
}

/// Every flag Glasscode passes is listed in `cli-flags.json` under its call site, so the probe of
/// a new CLI version checks it (alt-m4).
#[test]
fn every_cli_flag_is_listed() {
    let file: Value = serde_json::from_str(include_str!("../../../../cli-flags.json")).unwrap();
    let sites = file["call_sites"].as_array().unwrap();
    let flags_of = |ids: &[&str]| -> Vec<String> { sites.iter().filter(|s| ids.contains(&s["id"].as_str().unwrap())).flat_map(|s| s["flags"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_string())).collect() };
    for agent in [AgentKind::Claude, AgentKind::Codex] {
        let name = if agent == AgentKind::Claude { "claude" } else { "codex" };
        for (site, args) in crate::versions::argument_lists(agent) {
            // A session's launch also carries the flags of its helpers and of its profile.
            let ids: Vec<String> = if site.starts_with("agents::launch") {
                ["agents::launch", "agents::cli_args", "browser::agent_args", "profile::session_profile"].iter().map(|p| format!("{p} {name}")).collect()
            } else {
                vec![site.to_string()]
            };
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            let listed = flags_of(&ids);
            assert!(!listed.is_empty(), "{site}: no call site in cli-flags.json");
            for arg in args.iter().take_while(|a| *a != "--") {
                if arg.starts_with('-') && arg != "-" {
                    assert!(listed.contains(arg), "{site}: {arg} is not in cli-flags.json");
                }
            }
        }
    }
}

/// The stand-in CLIs of M02 take every argument list Glasscode builds (each ending in
/// `--version` or `--help`); a version that does not cannot be approved.
#[cfg(unix)]
#[test]
fn argument_contract() {
    let stand_ins = crate::testkit::agents::scripts_dir();
    for (agent, name) in [(AgentKind::Claude, "claude"), (AgentKind::Codex, "codex")] {
        let result = crate::versions::argument_contract(agent, &stand_ins.join(name), &[("PATH".to_string(), std::env::var("PATH").unwrap_or_default())].into());
        assert!(result.is_ok(), "{name}: {result:?}");
    }
    // A program that knows none of it fails the contract.
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("claude");
    std::fs::write(&bad, "#!/bin/sh\necho unknown option >&2\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(crate::versions::argument_contract(AgentKind::Claude, &bad, &Default::default()).is_err());
}

#[test]
fn codex_trusts_glasscodes_own_hooks() {
    let config = "model = \"x\"\n\n[hooks.state.\"/a/hooks.json:stop:0:0\"]\ntrusted_hash = \"sha256:old\"\n\n[projects.\"/w\"]\ntrust_level = \"trusted\"\n";
    let out = with_hook_trust(config, &[("/a/hooks.json:stop:0:0".into(), "sha256:new".into()), ("/a/hooks.json:start:0:0".into(), "sha256:s".into())]);
    assert!(out.contains("model = \"x\"") && out.contains("[projects.\"/w\"]\ntrust_level = \"trusted\""));
    assert!(!out.contains("sha256:old") && out.contains("trusted_hash = \"sha256:new\"") && out.contains("sha256:s"));
    assert_eq!(out.matches("[hooks.state.\"/a/hooks.json:stop:0:0\"]").count(), 1);
}

// ----- M23 ------------------------------------------------------------------------------------

/// D28: a session the manager drives gets no browser tools; the person's keep theirs.
#[test]
fn automation_session_has_no_browser() {
    let session: crate::model::Session = serde_json::from_value(json!({ "id": "s1", "project_id": "p", "worktree": std::env::temp_dir(), "branch": "b", "title": "t", "agent": "claude", "created_at": 0 })).unwrap();
    for (origin, browser) in [(SessionOrigin::Automation, false), (SessionOrigin::Quarantine, false)] {
        let p = profile(origin, &ProjectPolicy::default());
        let launch = crate::agents::launch_with(&session, None, &Default::default(), Some(&p));
        assert_eq!(launch.args.iter().any(|a| a.contains("glasscode-browser") || a.contains("browser-mcp")), browser, "{origin:?}: {:?}", launch.args);
    }
    // The gate refuses the browser's tools to them too, should a server be there anyway.
    let ctx = GateCtx { origin: Some(SessionOrigin::Automation), ..GateCtx::default() };
    assert!(gate_tool(&ctx, "mcp__glasscode-browser__eval", &json!({ "expression": "document.cookie" })).is_err());
    // A project that names its sites (M23): a throwaway browser kept to them, which the gate hook
    // lets through; the person's own sessions keep theirs.
    let sites = ProjectPolicy { browser_hosts: vec!["docs.rs".into()], ..ProjectPolicy::default() };
    let p = profile(SessionOrigin::Automation, &sites);
    assert_eq!(p.browser_hosts, ["docs.rs"]);
    let gate = p.claude_settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"].as_str().unwrap().to_string();
    assert!(gate.contains("--browser"), "{gate}");
    assert!(profile(SessionOrigin::Person, &sites).browser_hosts.is_empty());
    let with = GateCtx { browser: true, ..ctx };
    assert!(gate_tool(&with, "mcp__glasscode-browser__open", &json!({ "url": "https://docs.rs" })).is_ok());
    assert!(gate_tool(&with, "mcp__other__run", &json!({})).is_err());
}

/// trust-m2, alt-m1: the review run's Codex flags are read-only with no network and a schema, and
/// every one is in `cli-flags.json` and in the recorded help of the approved Codex.
#[test]
fn codex_review_flags_in_probe() {
    let p = headless_profile(HeadlessKind::AutoReview, AgentKind::Codex, &json!({ "type": "object" }), Path::new("/s.json"), Path::new("/a.json"));
    let args = &p.codex_args;
    assert_eq!(args[0], "exec");
    assert!(args.windows(2).any(|w| w == ["--sandbox", "read-only"]));
    assert!(!args.iter().any(|a| a.contains("full-auto") || a.contains("workspace-write") || a.contains("danger") || a.contains("network_access=true")));
    let file: Value = serde_json::from_str(include_str!("../../../../cli-flags.json")).unwrap();
    let site = file["call_sites"].as_array().unwrap().iter().find(|s| s["id"] == "agents::headless codex").unwrap();
    let listed: Vec<&str> = site["flags"].as_array().unwrap().iter().map(|f| f.as_str().unwrap()).collect();
    let help = include_str!("../../../../script/cli-help/codex-0.167.0.txt");
    let exec_help = &help[help.find("### $ codex exec --help").unwrap()..help.find("### $ codex resume --help").unwrap()];
    for flag in args.iter().filter(|a| a.starts_with('-') && *a != "-") {
        assert!(listed.contains(&flag.as_str()), "{flag} is not in cli-flags.json");
        assert!(exec_help.contains(flag.as_str()), "{flag} is not in codex exec --help");
    }
    // Claude's: no tool at all, and nothing of the person's.
    let c = headless_profile(HeadlessKind::AutoReview, AgentKind::Claude, &json!({}), Path::new("/s"), Path::new("/a"));
    assert!(c.claude_args.windows(2).any(|w| w == ["--tools", ""]) && c.claude_args.contains(&"--restricted".to_string()));
}

/// D4, P01, P02 (M50 part 2): the brain's lanes are headless runs with one door, Glasscode's MCP
/// server. Every flag of their profile is in the recorded help of the approved CLIs (the
/// instructions go in `--append-system-prompt`, as P03 proved), a wake
/// keeps no session and runs no cron, and no forge token reaches either.
#[test]
fn the_brain_is_a_headless_run_with_one_door() {
    let chat = headless_profile(HeadlessKind::BrainChat, AgentKind::Claude, &Value::Null, Path::new(""), Path::new(""));
    let wake = headless_profile(HeadlessKind::BrainWake, AgentKind::Claude, &Value::Null, Path::new(""), Path::new(""));
    for p in [&chat, &wake] {
        let has = |pair: &[&str]| p.claude_args.windows(pair.len()).any(|w| w.iter().zip(pair).all(|(a, b)| a == b));
        for pair in [&["--restricted"][..], &["--tools", "ToolSearch"], &["--strict-mcp-config"], &["--allowedTools", "mcp__glasscode__*"], &["--permission-mode", "dontAsk"], &["--permission-prompts", "none"], &["--disable-slash-commands"]] {
            assert!(has(pair), "{pair:?} in {}", p.label);
        }
        let codex = |pair: &[&str]| p.codex_args.windows(pair.len()).any(|w| w.iter().zip(pair).all(|(a, b)| a == b));
        for pair in [&["exec", "--json"][..], &["-s", "read-only"], &["--ignore-user-config"], &["--ignore-rules"], &["--disable", "shell_tool"], &["--disable", "browser_use"], &["--disable", "computer_use"], &["-c", "skills.include_instructions=false"], &["-c", "sandbox_workspace_write.network_access=false"]] {
            assert!(codex(pair), "{pair:?} in {}", p.label);
        }
        assert!(p.env_remove.contains(&"GH_TOKEN".to_string()) && p.env_remove.contains(&"GITHUB_TOKEN".to_string()));
        assert_eq!((p.claude_settings["disableAllHooks"].clone(), p.claude_settings["crossSessionInbound"].clone()), (json!(true), json!("refuse")));
    }
    assert!(wake.claude_args.contains(&"--no-session-persistence".to_string()) && !chat.claude_args.contains(&"--no-session-persistence".to_string()), "the chat's conversation is kept");
    assert!(wake.codex_args.contains(&"--ephemeral".to_string()) && !chat.codex_args.contains(&"--ephemeral".to_string()));
    assert!(wake.env_set.contains(&("CLAUDE_CODE_DISABLE_CRON".to_string(), "1".to_string())) && chat.env_set.is_empty());
    let claude_help = include_str!("../../../../script/cli-help/claude-2.1.322.txt");
    let help = include_str!("../../../../script/cli-help/codex-0.167.0.txt");
    let exec_help = &help[help.find("### $ codex exec --help").unwrap()..help.find("### $ codex resume --help").unwrap()];
    // No known failure: the brain depends on no flag the approved help does not list.
    let file: Value = serde_json::from_str(include_str!("../../../../cli-flags.json")).unwrap();
    assert!(file["known_failures"].as_array().unwrap().iter().all(|k| k["site"] != "runtime::claude args"));
    let spec = |agent, lane| crate::runtime::BrainSpec { agent, lane, wake: None, program: "x".into(), glasscode: "g".into(), model: "m".into(), effort: Some("low".into()), fallback_model: Some("f".into()), cwd: "/w".into(), run_dir: "/r".into(), env: Default::default(), instructions: String::new(), brain_env: vec![], resume: None };
    let files = crate::runtime::claude::Files { mcp: "/r/m.json".into(), settings: "/r/s.json".into() };
    // An option of a help text: a line that starts with it (alone, with its value, or with its
    // other name after a comma).
    let in_help = |help: &str, flag: &str| help.lines().map(str::trim_start).any(|t| t == flag || t.starts_with(&format!("{flag} ")) || t.starts_with(&format!("{flag},")));
    for lane in [crate::caller::Lane::Chat, crate::caller::Lane::Wake] {
        for flag in crate::runtime::claude::args(&spec(AgentKind::Claude, lane), &files, "s", true).iter().filter(|a| a.starts_with('-')) {
            assert!(in_help(claude_help, flag), "{flag} is not in claude --help");
        }
        for flag in crate::runtime::codex::args(&spec(AgentKind::Codex, lane), Path::new("/r/i.md"), Some("t")).iter().filter(|a| a.starts_with('-') && *a != "-") {
            assert!(in_help(exec_help, flag), "{flag} is not in codex exec --help");
        }
    }
}

/// D13 and P07: a push the manager asks for sends only the session's own commits.
#[test]
fn pushes_send_only_the_sessions_own_commits() {
    let made = vec!["1a2b3c4".to_string(), "9f8e7d6".to_string()];
    assert!(own_commits_only(&["1a2b3c4d5e6f".into(), "9f8e7d6c5b4a".into()], &made).is_ok());
    let foreign = own_commits_only(&["1a2b3c4d5e6f".into(), "deadbeef0000".into()], &made).unwrap_err();
    assert!(foreign.contains("1 commit this session did not make") && foreign.contains("deadbeef0"), "{foreign}");
    assert!(own_commits_only(&[], &[]).is_ok(), "nothing to push is nothing foreign");
    assert!(own_commits_only(&["abc".into()], &["ab".into()]).is_err(), "a short id under 7 digits counts for nothing");
}

/// alt-m1, trust-m2: a headless run reads outside text (a stranger's pull request, the pull
/// request a session the manager drives wrote for the validator, a project studied when moving
/// in) with no tool at all. Codex shows its shell (`exec_command`, `write_stdin`), its apps,
/// plugins and the account's own configuration unless they are turned off (P02's table), and its
/// read-only sandbox reads the whole disk (`~/.ssh` too). So a headless Codex run turns off what
/// the brain's does, and reads no configuration of the account's.
#[test]
fn a_headless_codex_run_has_no_shell_and_no_config_of_its_own() {
    for kind in [HeadlessKind::AutoReview, HeadlessKind::Study, HeadlessKind::Reflect] {
        let args = headless_profile(kind, AgentKind::Codex, &json!({ "type": "object" }), Path::new("/s.json"), Path::new("/a.txt")).codex_args;
        let has = |pair: &[&str]| args.windows(pair.len()).any(|w| w.iter().zip(pair).all(|(a, b)| a == b));
        for feature in ["shell_tool", "apps", "plugins", "multi_agent", "memories", "browser_use", "computer_use", "goals", "image_generation"] {
            assert!(has(&["--disable", feature]), "{kind:?}: {feature} stays on in {args:?}");
        }
        assert!(has(&["-c", r#"web_search="disabled""#]), "{kind:?}: web search stays on in {args:?}");
        for pair in [&["--ignore-user-config"][..], &["--ignore-rules"], &["--sandbox", "read-only"], &["-c", "skills.include_instructions=false"], &["-c", "skills.bundled.enabled=false"], &["-c", "sandbox_workspace_write.network_access=false"]] {
            assert!(has(pair), "{kind:?}: {pair:?} not in {args:?}");
        }
        // The answer still comes as before: bound to the schema, in the answer file, from stdin.
        assert!(has(&["--output-schema", "/s.json"]) && has(&["-o", "/a.txt"]) && args.last().map(String::as_str) == Some("-"), "{args:?}");
    }
}

/// A scratch repository for the push tests: `git` with a fixed identity and none of the person's
/// configuration, and `at` (when given) as the commit's date.
fn scratch_git(dir: &Path, args: &[&str], at: Option<u64>) -> String {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"]).args(args);
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1");
    if let Some(at) = at {
        cmd.env("GIT_COMMITTER_DATE", format!("@{at} +0000")).env("GIT_AUTHOR_DATE", format!("@{at} +0000"));
    }
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn scratch_commit(dir: &Path, file: &str, at: Option<u64>) -> String {
    std::fs::write(dir.join(file), file).unwrap();
    scratch_git(dir, &["add", file], at);
    scratch_git(dir, &["commit", "-q", "-m", file], at);
    scratch_git(dir, &["rev-parse", "HEAD"], None)
}

/// A turn of a scratch session from `head`, started at `started_ms`, closed now: the commits its
/// report records, as the journal keeps them (short ids).
fn turn_made(repo: &Path, head: &str, started_ms: u64) -> Vec<String> {
    let closed = crate::turns::Closed {
        turn: crate::turns::OpenTurn { id: "t1".into(), started_ms, head: Some(head.to_string()), ..Default::default() },
        end: crate::turns::TurnEnd::Completed,
        ended_ms: started_ms + 1_000,
        reply: None,
        background: Vec::new(),
    };
    let gathered = crate::turns::gather(AgentKind::Codex, None, &closed, repo, false);
    gathered.commits.iter().filter_map(|line| line.split_whitespace().next()).map(str::to_string).collect()
}

/// D13 and P07: inside one turn, a session the manager drives moves its own branch onto work it
/// did not make (the person's unpushed branch), by a merge, and plants a remote-tracking ref so
/// that work looks pushed. Its brokered push must still refuse: the person's commit is not the
/// session's. Its own work alone goes, even when the remote moved on since the last fetch.
#[test]
fn a_branch_moved_onto_other_work_is_not_the_sessions_own() {
    let scratch = tempfile::tempdir().unwrap();
    let (origin, repo, other) = (scratch.path().join("origin.git"), scratch.path().join("repo"), scratch.path().join("other"));
    scratch_git(scratch.path(), &["init", "-q", "--bare", origin.to_str().unwrap()], None);
    scratch_git(scratch.path(), &["init", "-q", repo.to_str().unwrap()], None);
    scratch_git(&repo, &["remote", "add", "origin", origin.to_str().unwrap()], None);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    let hour_ago = now.as_secs() - 3600;
    scratch_commit(&repo, "base", Some(hour_ago));
    scratch_git(&repo, &["push", "-q", "-u", "origin", "main"], None);
    // A teammate moves main on, and this checkout has not fetched since.
    scratch_git(scratch.path(), &["clone", "-q", origin.to_str().unwrap(), other.to_str().unwrap()], None);
    scratch_commit(&other, "teammates-work", Some(hour_ago + 30));
    scratch_git(&other, &["push", "-q", "origin", "main"], None);
    // And the remote has a ref whose commit no fetch of branches brings here (a fork's pull
    // request head on GitHub, `refs/pull/<n>/head`).
    scratch_git(&other, &["checkout", "-q", "-b", "fork"], None);
    scratch_commit(&other, "a-forks-work", Some(hour_ago + 40));
    scratch_git(&other, &["push", "-q", "origin", "fork:refs/pull/1/head"], None);
    // The person's work on another branch, never pushed.
    scratch_git(&repo, &["checkout", "-q", "-b", "person-wip"], None);
    let theirs = scratch_commit(&repo, "the-persons-work", Some(hour_ago + 60));

    // A turn that only does the session's own work: it goes.
    scratch_git(&repo, &["checkout", "-q", "-b", "session", "main"], None);
    let head = scratch_git(&repo, &["rev-parse", "HEAD"], None);
    let started_ms = now.as_millis() as u64;
    let own = scratch_commit(&repo, "the-sessions-work", None);
    let made = turn_made(&repo, &head, started_ms);
    assert_eq!(made, [own[..7].to_string()], "the turn made one commit");
    assert_eq!(own_push_tip(&repo, "session", &made).unwrap(), own);

    // The next turn merges the person's branch in, and plants a remote-tracking ref for it: a
    // Claude session can write any ref (P07).
    let head = own.clone();
    let started_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    scratch_git(&repo, &["merge", "-q", "--no-edit", "person-wip"], None);
    scratch_git(&repo, &["update-ref", "refs/remotes/origin/planted", "person-wip"], None);
    let mut made = made;
    made.extend(turn_made(&repo, &head, started_ms));
    assert!(!made.iter().any(|m| theirs.starts_with(m.as_str())), "the person's commit {theirs} is not the turn's: {made:?}");
    let refused = own_push_tip(&repo, "session", &made).unwrap_err().to_string();
    assert!(refused.contains("1 commit this session did not make") && refused.contains(&theirs[..9]), "{refused}");
}
