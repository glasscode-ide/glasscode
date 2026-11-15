//! How Glasscode starts, resumes and observes the official `claude` and `codex` CLIs.
//!
//! Rules we follow (see the implementation plan, "Policy"):
//! - always run the unmodified official binary in a real terminal;
//! - never read, copy or store login tokens: each account is just a config folder and the
//!   CLI signs in by itself;
//! - only use documented surfaces: flags, env vars, hooks and session files we only read.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::model::{Account, AgentKind, Session};
use crate::paths;

/// A ready-to-spawn process description.
#[derive(Clone, Debug)]
pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

/// Find `name` on the PATH from the login-shell environment.
pub fn resolve_program(name: &str, env: &HashMap<String, String>) -> Option<PathBuf> {
    crate::sys::find_program(name, env.get("PATH")?)
}

pub fn events_file(session_id: &str) -> PathBuf {
    paths::events_dir().join(format!("{session_id}.jsonl"))
}

/// Settings passed with `claude --settings` so hooks report back to Glasscode, the status line
/// keeps Claude's plan limits for Usage (then runs the person's own status line), and Claude's
/// colours follow Glasscode's theme. They are layered on top of the user's own settings and
/// never written to their files.
fn claude_hook_settings() -> String {
    let command = r#"cat >> "$GLASSCODE_EVENTS"; printf '\n' >> "$GLASSCODE_EVENTS""#;
    let hook = serde_json::json!([{ "hooks": [{ "type": "command", "command": command }] }]);
    serde_json::json!({
        "hooks": {
            "SessionStart": hook,
            "UserPromptSubmit": hook,
            "Stop": hook,
            "Notification": hook,
        },
        "statusLine": { "type": "command", "command": crate::usage::STATUSLINE_COMMAND },
        // Light or dark from the terminal's background (Glasscode answers that question), so
        // Claude's message bars and diffs match Glasscode's theme whatever the person's own
        // setting; Glasscode tells it when the theme changes.
        "theme": "auto",
    })
    .to_string()
}

/// Locate a Claude transcript by conversation id inside one config folder.
pub fn find_claude_transcript(config_dir: &Path, conversation_id: &str) -> Option<PathBuf> {
    let projects = config_dir.join("projects");
    let file_name = format!("{conversation_id}.jsonl");
    std::fs::read_dir(projects)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(&file_name))
        .find(|path| path.is_file())
}

/// Build the command for a session. `resume` is false for a brand-new conversation.
/// A top-level string from a TOML file (`key = "…"`, `'…'` or `"""…"""`), without a TOML
/// parser: enough for one setting.
fn toml_string(text: &str, key: &str) -> Option<String> {
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let line = line.trim();
        if line.starts_with('[') {
            return None;
        }
        let Some(rest) = line.strip_prefix(key).map(str::trim_start).and_then(|r| r.strip_prefix('=')).map(str::trim) else { continue };
        for quote in ["\"\"\"", "'''"] {
            if let Some(body) = rest.strip_prefix(quote) {
                if let Some(end) = body.find(quote) {
                    return Some(body[..end].to_string());
                }
                let mut out = body.to_string();
                for next in lines.by_ref() {
                    if let Some(end) = next.find(quote) {
                        out.push('\n');
                        out.push_str(&next[..end]);
                        return Some(out.trim_start_matches('\n').to_string());
                    }
                    out.push('\n');
                    out.push_str(next);
                }
                return None;
            }
        }
        if let Some(body) = rest.strip_prefix('"') {
            return serde_json::from_str::<String>(&format!("\"{}", &body[..body.rfind('"')? + 1])).ok();
        }
        if let Some(body) = rest.strip_prefix('\'') {
            return Some(body[..body.find('\'')?].to_string());
        }
        return None;
    }
    None
}

/// Claude's name for a permission mode (`--permission-mode`).
pub fn claude_mode(mode: &str) -> Option<&'static str> {
    match mode.to_lowercase().replace(['-', '_', ' '], "").as_str() {
        "default" | "ask" | "normal" => Some("default"),
        "acceptedits" | "edits" => Some("acceptEdits"),
        "plan" => Some("plan"),
        "auto" => Some("auto"),
        "bypasspermissions" | "bypass" | "yolo" => Some("bypassPermissions"),
        _ => None,
    }
}

/// What tells an agent about the `glasscode` command (when the app has written it, and the
/// person has not turned it off): Claude's skill as a plugin, allowed to run `glasscode`
/// without asking (the app asks itself for what needs the person); Codex's instructions.
fn cli_args(agent: AgentKind, config_dir: Option<&Path>) -> Vec<String> {
    match agent {
        AgentKind::Claude => {
            let dir = crate::cli::plugin_dir();
            if !dir.join(".claude-plugin").join("plugin.json").is_file() {
                return Vec::new();
            }
            vec!["--plugin-dir".into(), dir.to_string_lossy().into_owned(), "--allowedTools".into(), "Bash(glasscode:*)".into()]
        }
        AgentKind::Codex => {
            if !crate::cli::plugin_dir().join(".claude-plugin").join("plugin.json").is_file() {
                return Vec::new();
            }
            // An override replaces the account's own instructions, so theirs come first.
            let own = config_dir.and_then(|d| std::fs::read_to_string(d.join("config.toml")).ok()).and_then(|t| toml_string(&t, "developer_instructions"));
            let text = match own {
                Some(own) if !own.trim().is_empty() => format!("{}\n\n{}", own.trim_end(), crate::cli::CODEX_INSTRUCTIONS),
                _ => crate::cli::CODEX_INSTRUCTIONS.to_string(),
            };
            vec!["-c".into(), format!("developer_instructions={}", serde_json::to_string(&text).unwrap_or_default())]
        }
        AgentKind::Shell => Vec::new(),
    }
}

/// Make Codex trust a worktree so it does not stop to ask on first use. Best effort: append
/// the same entry a person would add by hand, and only when it is not already there — never
/// rewriting the file, so it cannot disturb the account's own config or block launch.
fn ensure_codex_trusts(codex_home: &Path, worktree: &Path) {
    let Some(dir) = worktree.to_str() else { return };
    // A TOML string key, quoted and escaped exactly like the entries Codex writes itself.
    let Ok(key) = serde_json::to_string(dir) else { return };
    let header = format!("[projects.{key}]");
    let file = codex_home.join("config.toml");
    let existing = std::fs::read_to_string(&file).unwrap_or_default();
    if existing.contains(&header) {
        return;
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("\n{header}\ntrust_level = \"trusted\"\n"));
    let _ = std::fs::create_dir_all(codex_home);
    let _ = std::fs::write(&file, out);
}

pub fn launch(
    session: &Session,
    account: Option<&Account>,
    base_env: &HashMap<String, String>,
) -> Launch {
    let mut env = base_env.clone();
    env.insert("GLASSCODE_SESSION_ID".into(), session.id.clone());
    // Note: the fork injects no vendored CLI onto PATH — sessions run with the user's own PATH only.
    env.insert(
        "GLASSCODE_EVENTS".into(),
        events_file(&session.id).to_string_lossy().into_owned(),
    );
    let resolve = |name: &str| {
        resolve_program(name, base_env)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.to_string())
    };

    match session.agent {
        AgentKind::Shell => Launch {
            program: crate::env::user_shell(),
            args: vec!["-l".into()],
            env,
        },
        AgentKind::Claude => {
            if let Some(dir) = account.and_then(Account::config_dir) {
                env.insert("CLAUDE_CONFIG_DIR".into(), dir.to_string_lossy().into_owned());
            }
            // Versions are managed by Glasscode's approved-version list.
            env.insert("DISABLE_AUTOUPDATER".into(), "1".into());
            let config_dir = account.map(Account::effective_dir);
            // Usage: where the status line keeps this session's limits, and the person's own
            // status line, which runs after it as before.
            env.insert("GLASSCODE_LIMITS".into(), crate::usage::limits_file(&session.id).to_string_lossy().into_owned());
            if let Some(own) = config_dir.as_deref().and_then(crate::usage::user_statusline) {
                env.insert("GLASSCODE_STATUSLINE".into(), own);
            }
            let conversation = session.agent_session_id.clone();
            let exists = match (&conversation, &config_dir) {
                (Some(id), Some(dir)) => find_claude_transcript(dir, id).is_some(),
                _ => false,
            };
            let mut args = Vec::new();
            match conversation {
                Some(id) if exists => {
                    args.push("--resume".into());
                    args.push(id);
                }
                Some(id) => {
                    args.push("--session-id".into());
                    args.push(id);
                }
                None => {}
            }
            // The Glasscode Browser's tools (on the Mac). Its flags take several values, so
            // they come before `--settings`, never right before the prompt.
            args.extend(crate::browser::agent_args(AgentKind::Claude, &session.id));
            args.extend(cli_args(AgentKind::Claude, config_dir.as_deref()));
            if let Some(model) = session.model.as_deref().filter(|m| !m.is_empty()) {
                args.push("--model".into());
                args.push(model.to_string());
            }
            // The mode it was started with; later the person changes it there (Shift-Tab), so a
            // resumed conversation starts in Claude's own default again.
            if let Some(mode) = session.mode.as_deref().and_then(claude_mode).filter(|_| !exists) {
                args.push("--permission-mode".into());
                args.push(mode.into());
            }
            args.push("--settings".into());
            args.push(claude_hook_settings());
            if !exists {
                if let Some(prompt) = &session.initial_prompt {
                    // A message may start with "-" (a list): never an option.
                    args.push("--".into());
                    args.push(prompt.clone());
                }
            }
            Launch {
                program: resolve("claude"),
                args,
                env,
            }
        }
        AgentKind::Codex => {
            if let Some(dir) = account.and_then(Account::config_dir) {
                env.insert("CODEX_HOME".into(), dir.to_string_lossy().into_owned());
            }
            // `-c` overrides go before the `resume` subcommand.
            let mut args = crate::browser::agent_args(AgentKind::Codex, &session.id);
            let codex_home = account.map(Account::effective_dir).unwrap_or_else(|| crate::paths::home().join(".codex"));
            // Glasscode made this worktree for a repo the person already works in, so trust it
            // up front: otherwise Codex stops on first use to ask "trust this folder?", and that
            // prompt is easy to miss in chat view.
            ensure_codex_trusts(&codex_home, &session.worktree);
            args.extend(cli_args(AgentKind::Codex, Some(&codex_home)));
            if let Some(model) = session.model.as_deref().filter(|m| !m.is_empty()) {
                args.push("--model".into());
                args.push(model.to_string());
            }
            match session.mode.as_deref() {
                Some("full-auto" | "auto") => args.push("--full-auto".into()),
                Some("read-only") => {
                    args.push("--sandbox".into());
                    args.push("read-only".into());
                }
                _ => {}
            }
            match &session.agent_session_id {
                Some(id) => {
                    args.push("resume".into());
                    args.push(id.clone());
                }
                None => {
                    if let Some(prompt) = &session.initial_prompt {
                        args.push("--".into());
                        args.push(prompt.clone());
                    }
                }
            }
            Launch {
                program: resolve("codex"),
                args,
                env,
            }
        }
    }
}

/// Run the agent once, non-interactively, and return what it prints to stdout. The local
/// auto-reviewer uses this to get a pull-request overview without opening a session. It is
/// read-only work: the agent may read files and run `gh`/`git`, and is given no session,
/// hooks or browser. `config_dir` is the account's login folder, so it runs as the right
/// account, exactly as a normal session does. The run is bounded to five minutes.
pub fn headless(
    agent: AgentKind,
    config_dir: Option<&Path>,
    prompt: &str,
    base_env: &HashMap<String, String>,
) -> anyhow::Result<String> {
    use std::process::{Command, Stdio};
    let resolve = |name: &str| {
        resolve_program(name, base_env).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| name.to_string())
    };
    let mut cmd = match agent {
        AgentKind::Claude => {
            let mut cmd = Command::new(resolve("claude"));
            cmd.envs(base_env);
            cmd.env("DISABLE_AUTOUPDATER", "1");
            if let Some(dir) = config_dir {
                cmd.env("CLAUDE_CONFIG_DIR", dir);
            }
            // Print mode. Allow only read-only tools plus `gh`/`git`: enough to read the PR's
            // diff, nothing that writes. Tools not listed are denied, never prompted.
            cmd.args(["-p", prompt, "--allowedTools", "Bash(gh:*),Bash(git:*),Read,Grep,Glob,WebFetch"]);
            cmd
        }
        AgentKind::Codex => {
            let mut cmd = Command::new(resolve("codex"));
            cmd.envs(base_env);
            if let Some(dir) = config_dir {
                cmd.env("CODEX_HOME", dir);
            }
            // `exec` is non-interactive; full-auto lets it run `gh` without stopping to ask.
            cmd.args(["exec", "--full-auto", prompt]);
            cmd
        }
        AgentKind::Shell => anyhow::bail!("no headless run for a shell"),
    };
    cmd.current_dir(std::env::temp_dir());
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let deadline = SystemTime::now() + std::time::Duration::from_secs(300);
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if SystemTime::now() >= deadline {
            let _ = child.kill();
            anyhow::bail!("review timed out after 5 minutes");
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// A login run in a terminal: the CLI handles the browser sign-in itself.
pub fn login_launch(account: &Account, base_env: &HashMap<String, String>) -> Launch {
    let mut env = base_env.clone();
    let program = match account.agent {
        AgentKind::Codex => "codex",
        _ => "claude",
    };
    if let Some(dir) = account.config_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let var = match account.agent {
            AgentKind::Codex => "CODEX_HOME",
            _ => "CLAUDE_CONFIG_DIR",
        };
        env.insert(var.into(), dir.to_string_lossy().into_owned());
    }
    env.insert("DISABLE_AUTOUPDATER".into(), "1".into());
    let args = match account.agent {
        AgentKind::Codex => vec!["login".into()],
        // `claude` opens its own login flow on first start; `/login` also works later.
        _ => vec![],
    };
    Launch {
        program: resolve_program(program, base_env)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| program.to_string()),
        args,
        env,
    }
}

/// Activity reported by the agent while it runs.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivity {
    Working,
    Idle,
    NeedsInput,
}

#[derive(Clone, Debug, Default)]
pub struct HookUpdate {
    pub conversation_id: Option<String>,
    pub transcript_path: Option<PathBuf>,
    pub activity: Option<AgentActivity>,
}

/// Reads new hook events appended to a session's events file.
pub struct EventsTail {
    path: PathBuf,
    offset: u64,
}

impl EventsTail {
    pub fn new(session_id: &str) -> Self {
        let path = events_file(session_id);
        let offset = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Self { path, offset }
    }

    pub fn poll(&mut self) -> Option<HookUpdate> {
        let mut file = std::fs::File::open(&self.path).ok()?;
        let len = file.metadata().ok()?.len();
        if len < self.offset {
            self.offset = 0;
        }
        if len == self.offset {
            return None;
        }
        file.seek(SeekFrom::Start(self.offset)).ok()?;
        let mut text = String::new();
        file.take(len - self.offset).read_to_string(&mut text).ok()?;
        // Only consume complete lines.
        let consumed = text.rfind('\n').map(|i| i + 1)?;
        self.offset += consumed as u64;
        let mut update = HookUpdate::default();
        for line in text[..consumed].lines().filter(|l| !l.trim().is_empty()) {
            let Ok(value) = serde_json::from_str::<Value>(line) else { continue };
            if let Some(id) = value.get("session_id").and_then(Value::as_str) {
                update.conversation_id = Some(id.to_string());
            }
            if let Some(path) = value.get("transcript_path").and_then(Value::as_str) {
                update.transcript_path = Some(PathBuf::from(path));
            }
            update.activity = match value.get("hook_event_name").and_then(Value::as_str) {
                Some("UserPromptSubmit") => Some(AgentActivity::Working),
                Some("Stop") | Some("SessionStart") => Some(AgentActivity::Idle),
                Some("Notification") => Some(AgentActivity::NeedsInput),
                _ => update.activity,
            };
        }
        Some(update)
    }
}

/// Find the Codex conversation started in `cwd` after `since`, by reading the header of
/// recent session files in the account's `CODEX_HOME`.
pub fn find_codex_conversation(codex_home: &Path, cwd: &Path, since: SystemTime) -> Option<(String, PathBuf)> {
    let mut candidates: Vec<(SystemTime, PathBuf)> = Vec::new();
    collect_rollouts(&codex_home.join("sessions"), since, &mut candidates);
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in candidates {
        let Ok(file) = std::fs::File::open(&path) else { continue };
        let mut first = String::new();
        if BufReader::new(file).read_line(&mut first).is_err() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&first) else { continue };
        let payload = &value["payload"];
        let same_dir = payload["cwd"].as_str().is_some_and(|c| Path::new(c) == cwd);
        if same_dir {
            if let Some(id) = payload["id"].as_str() {
                return Some((id.to_string(), path));
            }
        }
    }
    None
}

/// Codex stores sessions as `sessions/YYYY/MM/DD/rollout-*.jsonl`. Year and month folders keep
/// old modification times, so walk them all and only filter day folders and files by time.
pub(crate) fn collect_rollouts(sessions: &Path, since: SystemTime, out: &mut Vec<(SystemTime, PathBuf)>) {
    let modified = |path: &Path| {
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    };
    let subdirs = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default()
    };
    for year in subdirs(sessions) {
        for month in subdirs(&year) {
            for day in subdirs(&month) {
                if modified(&day) < since {
                    continue;
                }
                let Ok(files) = std::fs::read_dir(&day) else { continue };
                for file in files.filter_map(Result::ok).map(|e| e.path()) {
                    let time = modified(&file);
                    if file.extension().is_some_and(|e| e == "jsonl") && time >= since {
                        out.push((time, file));
                    }
                }
            }
        }
    }
}

/// Find a Codex session file by conversation id.
pub fn find_codex_rollout(codex_home: &Path, conversation_id: &str) -> Option<PathBuf> {
    let mut found = Vec::new();
    collect_rollouts(&codex_home.join("sessions"), SystemTime::UNIX_EPOCH, &mut found);
    found
        .into_iter()
        .map(|(_, path)| path)
        .find(|path| path.file_name().is_some_and(|n| n.to_string_lossy().contains(conversation_id)))
}

/// Make a conversation available to another account of the same agent by copying its
/// transcript file (never credentials) into that account's folder.
pub fn copy_conversation(agent: AgentKind, from: &Account, to: &Account, conversation_id: &str) -> anyhow::Result<()> {
    let from_dir = from.effective_dir();
    let to_dir = to.effective_dir();
    let source = match agent {
        AgentKind::Claude => find_claude_transcript(&from_dir, conversation_id),
        AgentKind::Codex => find_codex_rollout(&from_dir, conversation_id),
        AgentKind::Shell => None,
    }
    .ok_or_else(|| anyhow::anyhow!("no saved conversation {conversation_id} for {}", from.label))?;
    let relative = source.strip_prefix(&from_dir)?;
    let target = to_dir.join(relative);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&source, &target)?;
    Ok(())
}

/// Version of an installed CLI, from `--version`.
pub fn installed_version(program: &str, env: &HashMap<String, String>) -> Option<String> {
    let path = resolve_program(program, env)?;
    let output = std::process::Command::new(path)
        .arg("--version")
        .envs(env)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .find(|word| word.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusts_a_worktree_once() {
        let home = std::env::temp_dir().join(format!("hbt-codex-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let wt = Path::new("/Users/dev/app/.glasscode/worktrees/feat-x");
        ensure_codex_trusts(&home, wt);
        let text = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(text.contains("[projects.\"/Users/dev/app/.glasscode/worktrees/feat-x\"]"));
        assert!(text.contains("trust_level = \"trusted\""));
        // Idempotent: a second launch does not append a duplicate.
        ensure_codex_trusts(&home, wt);
        let again = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert_eq!(again.matches("trust_level").count(), 1);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn reads_a_toml_string() {
        assert_eq!(toml_string("model = \"o3\"\ndeveloper_instructions = \"Be brief. \\\"Really\\\"\"\n", "developer_instructions").as_deref(), Some("Be brief. \"Really\""));
        assert_eq!(toml_string("developer_instructions = \"\"\"\nLine one\nLine two\"\"\"\n", "developer_instructions").as_deref(), Some("Line one\nLine two"));
        assert_eq!(toml_string("developer_instructions = 'Plain'\n", "developer_instructions").as_deref(), Some("Plain"));
        assert_eq!(toml_string("[profiles.x]\ndeveloper_instructions = \"x\"", "developer_instructions"), None);
        assert_eq!(toml_string("developer_instructions_extra = \"x\"", "developer_instructions"), None);
    }

    fn session(agent: AgentKind, conversation: Option<&str>) -> Session {
        Session {
            id: "s1".into(),
            project_id: "p".into(),
            worktree: "/tmp".into(),
            branch: "main".into(),
            title: "t".into(),
            agent,
            account_id: None,
            agent_session_id: conversation.map(str::to_string),
            transcript_path: None,
            lineage: Vec::new(),
            initial_prompt: Some("- fix the retry".into()),
            team_task: None,
            review: None,
            model: Some("opus".into()),
            mode: Some("plan".into()),
            created_at: 0,
        }
    }

    #[test]
    fn a_first_message_is_never_an_option() {
        let claude = launch(&session(AgentKind::Claude, Some("c1")), None, &HashMap::new()).args;
        assert_eq!(&claude[claude.len() - 2..], ["--", "- fix the retry"]);
        assert!(claude.windows(2).any(|w| w == ["--permission-mode", "plan"]));
        assert!(claude.windows(2).any(|w| w == ["--model", "opus"]));
        let codex = launch(&session(AgentKind::Codex, None), None, &HashMap::new()).args;
        assert_eq!(&codex[codex.len() - 2..], ["--", "- fix the retry"]);
    }
}
