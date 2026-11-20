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
use std::time::{Duration, SystemTime};

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

/// The file with a session's Claude settings on Windows (`claude --settings <file>`).
pub fn settings_file(session_id: &str) -> PathBuf {
    paths::settings_dir().join(format!("{session_id}.json"))
}

/// Remove what Glasscode keeps for a session's agent: its hook events (with their counter and
/// lock) and (on Windows) its Claude settings.
pub fn remove_session_files(session_id: &str) {
    let events = events_file(session_id);
    let _ = std::fs::remove_file(crate::cli::spool_counter(&events));
    let _ = std::fs::remove_file(crate::cli::spool_lock(&events));
    let _ = std::fs::remove_file(events);
    let _ = std::fs::remove_file(settings_file(session_id));
}

/// Settings passed with `claude --settings` so hooks report back to Glasscode, the status line
/// keeps Claude's plan limits for Usage (then runs the person's own status line), and Claude's
/// colours follow Glasscode's theme. They are layered on top of the user's own settings and
/// never written to their files.
///
/// In a v2 home (the journal runs: `journal::known_enabled`) every hook is `glasscode hook` by the
/// absolute path of this program, on every platform, which appends each payload in one write
/// under the spool's lock (events-20), with a timeout, and `SessionEnd` reports too. With the
/// engine at v1 the hooks are as they always were (rule 2).
///
/// With the v2 feature "turn_hooks" on too, the hooks of P05 report as well: `StopFailure`,
/// `SubagentStart`, `SubagentStop`, `PermissionRequest`, `PermissionDenied`, `CwdChanged` and
/// `PostCompact` (M30). Each exists in the approved versions; an event a version does not know
/// would stop an interactive start on a dialog, so they stay behind the switch.
///
/// `StopFailure` alone (`failure_hook`) reports in every v2 home whose Claude is at least
/// `versions::MIN_CLAUDE`, the version P05 checked it on: a failed turn ends there, and the work
/// ledger's "three failed turns in a row" counts it (M32).
fn claude_hook_settings(v2: Option<&Path>, turn_hooks: bool, failure_hook: bool, brief: bool) -> String {
    // Claude Code on Windows runs these with Git Bash, or PowerShell when there is none; the
    // `glasscode` command (first on each session's PATH) does the same work in either.
    let (command, statusline) = if cfg!(windows) {
        (if v2.is_some() { "glasscode hook --stamp" } else { "glasscode hook" }.to_string(), "glasscode statusline")
    } else if let Some(program) = v2 {
        (format!("{} cli hook --stamp", crate::remote::shell_quote(&program.to_string_lossy())), crate::usage::STATUSLINE_COMMAND)
    } else {
        (r#"cat >> "$GLASSCODE_EVENTS"; printf '\n' >> "$GLASSCODE_EVENTS""#.to_string(), crate::usage::STATUSLINE_COMMAND)
    };
    let hook = if v2.is_some() { serde_json::json!([{ "hooks": [{ "type": "command", "command": command, "timeout": 10 }] }]) } else { serde_json::json!([{ "hooks": [{ "type": "command", "command": command }] }]) };
    let mut settings = serde_json::json!({
        "hooks": {
            "SessionStart": hook,
            "UserPromptSubmit": hook,
            "Stop": hook,
            "Notification": hook,
        },
        "statusLine": { "type": "command", "command": statusline },
        // Light or dark from the terminal's background (Glasscode answers that question), so
        // Claude's message bars and diffs match Glasscode's theme whatever the person's own
        // setting; Glasscode tells it when the theme changes.
        "theme": "auto",
    });
    if v2.is_some() && turn_hooks {
        for event in TURN_HOOKS {
            settings["hooks"][event] = hook.clone();
        }
    }
    if v2.is_some() && failure_hook {
        settings["hooks"]["StopFailure"] = hook.clone();
    }
    if v2.is_some() {
        settings["hooks"]["SessionEnd"] = hook.clone();
        // A project's own settings cannot turn the reporting hooks off (P08).
        settings["disableAllHooks"] = Value::Bool(false);
    }
    if brief {
        // The memory brief (M20.2, P03): on start, resume and after a compaction, the current brief
        // is this hook's `additionalContext`, by the absolute path of the command (D11).
        let brief_command = if cfg!(windows) {
            "glasscode memory brief --hook".to_string()
        } else if let Some(program) = v2 {
            format!("{} cli memory brief --hook", crate::remote::shell_quote(&program.to_string_lossy()))
        } else {
            format!("{} memory brief --hook", crate::remote::shell_quote(&crate::cli::command_file().to_string_lossy()))
        };
        settings["hooks"]["SessionStart"] = serde_json::json!([{ "hooks": [hook[0]["hooks"][0].clone(), { "type": "command", "command": brief_command, "timeout": 20 }] }]);
    }
    settings.to_string()
}

/// The hooks M30's turns read, beyond the four of v1 and `SessionEnd` (P05).
pub const TURN_HOOKS: [&str; 7] = ["StopFailure", "SubagentStart", "SubagentStop", "PermissionRequest", "PermissionDenied", "CwdChanged", "PostCompact"];

/// The program whose `cli hook` the hooks of a v2 home run: this one (the app on the Mac,
/// glasscoded on the server), by its absolute path. None with the engine at v1, or before the
/// journal decided (no disk read here).
fn v2_hook_program() -> Option<PathBuf> {
    (crate::journal::known_enabled() == Some(true)).then(|| std::env::current_exe().ok()).flatten()
}

/// Whether the Claude on `env`'s PATH knows `StopFailure`: it is at least `versions::MIN_CLAUDE`.
/// The launch runs on the main thread, so the version is the one already read (Settings ›
/// Updates' check, or a read this starts in the background): until it is known, the session
/// starts without the hook, as it did before.
fn claude_knows_stop_failure(env: &HashMap<String, String>) -> bool {
    crate::versions::version_soon(Path::new("claude"), env).is_some_and(|version| crate::versions::below_minimum(AgentKind::Claude, &version).is_none())
}

/// What `--settings` gets: the settings themselves, or on Windows the path of the session's
/// settings file, as a file has no quotation marks for cmd.exe or a command line to break.
fn claude_settings_arg(session_id: &str, profile: Option<&crate::profile::SessionProfile>, env: &HashMap<String, String>) -> String {
    let v2 = v2_hook_program();
    // The version is read only in a v2 home: with the engine at v1 nothing new runs (rule 2).
    let failure_hook = v2.is_some() && claude_knows_stop_failure(env);
    let settings = claude_hook_settings(v2.as_deref(), crate::journal::feature_on("turn_hooks"), failure_hook, brief_on());
    let settings = match profile {
        Some(profile) => with_profile(&settings, &profile.claude_settings),
        None => settings,
    };
    if !cfg!(windows) {
        return settings;
    }
    let file = settings_file(session_id);
    match write_if_changed(&file, &settings) {
        Ok(()) => file.to_string_lossy().into_owned(),
        Err(error) => {
            log::warn!("could not write the Claude settings of session {session_id} to {}: {error}; they go on the command line", file.display());
            settings
        }
    }
}

fn write_if_changed(file: &Path, text: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(file).is_ok_and(|old| old == text) {
        return Ok(());
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(file, text)
}

/// An agent's CLI (`claude`, `codex`) on the PATH of `env`, as the program to start and the
/// arguments that come before Glasscode's. An npm `.cmd` file on Windows gives way to the program
/// it starts (`sys::script_target`): through cmd.exe, a first message with `&`, `%` or `"` would
/// not arrive as it was written.
fn agent_program(name: &str, env: &HashMap<String, String>) -> (String, Vec<String>) {
    let Some(found) = resolve_program(name, env) else { return (name.to_string(), Vec::new()) };
    let path = env.get("PATH").map(String::as_str).unwrap_or_default();
    match crate::sys::script_target(&found, path.as_ref()) {
        Some((program, first)) => (program.to_string_lossy().into_owned(), first),
        None => (found.to_string_lossy().into_owned(), Vec::new()),
    }
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

/// Whether sessions get v2's memory brief: the engine is v2 and its "brief" switch is on (rule 2:
/// with v1, nothing of it reaches a session). As the journal read the switch at the start: no disk
/// read here.
fn brief_on() -> bool {
    crate::journal::feature_on("brief")
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
            let mut text = match own {
                Some(own) if !own.trim().is_empty() => format!("{}\n\n{}", own.trim_end(), crate::cli::CODEX_INSTRUCTIONS),
                _ => crate::cli::CODEX_INSTRUCTIONS.to_string(),
            };
            // v2's memory (M20.2): fixed text only, as Codex keeps a thread's first instructions on
            // resume (P03); the brief itself it asks for, until its SessionStart hook is trusted.
            if brief_on() {
                text = format!("{text}\n\n{}\n\n{}", crate::cli::MEMORY_INSTRUCTIONS, crate::cli::MEMORY_BRIEF_ASK);
            }
            vec!["-c".into(), format!("developer_instructions={}", serde_json::to_string(&text).unwrap_or_default())]
        }
        AgentKind::Shell => Vec::new(),
    }
}

/// Make Codex trust a worktree so it does not stop to ask on first use. Best effort: append
/// the same entry a person would add by hand, and only when the file has none for the folder
/// yet — never rewriting the file, so it cannot disturb the account's own config or block launch.
fn ensure_codex_trusts(codex_home: &Path, worktree: &Path) {
    let Some(dir) = worktree.to_str() else { return };
    let file = codex_home.join("config.toml");
    let existing = std::fs::read_to_string(&file).unwrap_or_default();
    // Compared as TOML reads the keys, not as text: Codex quotes a Windows path in single quotes,
    // and a second table for the same key would make the file invalid, so Codex would not start.
    // A key in another spelling of the folder (case, `\\?\`) is the folder for Codex on Windows.
    if codex_projects(&existing).iter().any(|key| crate::sys::same_path(Path::new(key), worktree)) {
        return;
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("\n[projects.{}]\ntrust_level = \"trusted\"\n", toml_key(dir)));
    let _ = std::fs::create_dir_all(codex_home);
    let _ = std::fs::write(&file, out);
}

/// A Codex `config.toml` without the trust tables Glasscode added for folders below `under` (its
/// worktrees: `ensure_codex_trusts`), each with its lines up to the next table; everything else
/// stays as it is. How many went. For uninstall (M25).
pub fn without_codex_trust(text: &str, under: &Path) -> (String, usize) {
    let mut out: Vec<&str> = Vec::new();
    let mut skipping = false;
    let mut gone = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            let key = trimmed
                .strip_prefix('[')
                .map(str::trim_start)
                .and_then(|r| r.strip_prefix("projects"))
                .map(str::trim_start)
                .and_then(|r| r.strip_prefix('.'))
                .and_then(|r| toml_quoted(r.trim_start()))
                .filter(|(_, after)| after.trim_start().starts_with(']'))
                .map(|(key, _)| key);
            skipping = key.is_some_and(|key| Path::new(&key).starts_with(under));
            if skipping {
                gone += 1;
                // The blank line written before it goes with it.
                while out.last().is_some_and(|l| l.trim().is_empty()) {
                    out.pop();
                }
                continue;
            }
        }
        if !skipping {
            out.push(line);
        }
    }
    if gone == 0 {
        return (text.to_string(), 0);
    }
    let mut kept = out.join("\n");
    if !kept.is_empty() {
        kept.push('\n');
    }
    (kept, gone)
}

/// A folder as a TOML key, quoted as Codex (toml_edit) quotes the entries it writes itself: in
/// single quotes when double quotes would need escapes (each `\` of a Windows path) and single
/// quotes can hold it, else in double quotes with JSON's escapes, which TOML reads the same.
fn toml_key(text: &str) -> String {
    let literal = text.contains(['\\', '"']) && !text.contains(|c: char| c == '\'' || (c.is_control() && c != '\t'));
    if literal { format!("'{text}'") } else { serde_json::to_string(text).unwrap_or_default() }
}

/// A quoted TOML key at the start of `text` (`"…"` with escapes, or `'…'`), and what follows it.
fn toml_quoted(text: &str) -> Option<(String, &str)> {
    if let Some(body) = text.strip_prefix('\'') {
        let end = body.find('\'')?;
        return Some((body[..end].to_string(), &body[end + 1..]));
    }
    let body = text.strip_prefix('"')?;
    let mut escaped = false;
    for (i, c) in body.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => return Some((serde_json::from_str(&text[..i + 2]).ok()?, &body[i + 1..])),
            _ => {}
        }
    }
    None
}

/// The folders a Codex `config.toml` has a project entry for, in each form TOML allows: a
/// `[projects."…"]` table, a `"…" = { … }` line in `[projects]`, or a dotted key.
fn codex_projects(text: &str) -> Vec<String> {
    enum Table {
        Top,
        Projects,
        Other,
    }
    let mut keys = Vec::new();
    let mut table = Table::Top;
    for line in text.lines().map(str::trim) {
        if let Some(header) = line.strip_prefix('[') {
            let rest = header.trim_start().strip_prefix("projects").map(str::trim_start);
            table = if rest.is_some_and(|r| r.starts_with(']')) { Table::Projects } else { Table::Other };
            if let Some((key, after)) = rest.and_then(|r| r.strip_prefix('.')).and_then(|r| toml_quoted(r.trim_start()))
                && after.trim_start().starts_with(']')
            {
                keys.push(key);
            }
            continue;
        }
        let entry = match table {
            Table::Top => line.strip_prefix("projects").map(str::trim_start).and_then(|r| r.strip_prefix('.')).map(str::trim_start),
            Table::Projects => Some(line),
            Table::Other => None,
        };
        if let Some((key, after)) = entry.and_then(toml_quoted)
            && after.trim_start().starts_with(['=', '.'])
        {
            keys.push(key);
        }
    }
    keys
}

/// Glasscode's own `--settings` with a session profile's over them (M22): the profile's hooks are
/// added to the reporting hooks, every other key of the profile is set.
pub fn with_profile(settings: &str, profile: &Value) -> String {
    let mut out: Value = serde_json::from_str(settings).unwrap_or_else(|_| serde_json::json!({}));
    if let (Some(base), Value::Object(over)) = (out.as_object_mut(), profile) {
        for (key, value) in over {
            match (key.as_str(), base.get_mut(key), value) {
                ("hooks", Some(Value::Object(hooks)), Value::Object(more)) => {
                    for (event, entries) in more {
                        match (hooks.get_mut(event), entries) {
                            (Some(Value::Array(have)), Value::Array(add)) => have.extend(add.iter().cloned()),
                            _ => {
                                hooks.insert(event.clone(), entries.clone());
                            }
                        }
                    }
                }
                _ => {
                    base.insert(key.clone(), value.clone());
                }
            }
        }
    }
    out.to_string()
}

/// Claude's own memory folder of a repository in an account folder (the vendor's layout, D19):
/// `projects/<the path with every character but letters and digits as ->/memory`.
pub fn claude_memory_dir(account_dir: &Path, repo_root: &Path) -> PathBuf {
    let slug: String = repo_root.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    account_dir.join("projects").join(slug).join("memory")
}

/// A profile's environment, after everything else the caller adds (the project's GitHub
/// account's token included): what it sets, and what it takes out.
pub fn apply_profile_env(launch: &mut Launch, profile: &crate::profile::SessionProfile) {
    for key in &profile.env_remove {
        launch.env.remove(key);
    }
    for (key, value) in &profile.env_set {
        launch.env.insert(key.clone(), value.clone());
    }
}

pub fn launch(
    session: &Session,
    account: Option<&Account>,
    base_env: &HashMap<String, String>,
) -> Launch {
    launch_with(session, account, base_env, None)
}

/// [`launch`] under a session profile (M22, the v2 engine with "profiles" on): its settings, flags
/// and environment. With none, the launch is today's, byte for byte.
pub fn launch_with(
    session: &Session,
    account: Option<&Account>,
    base_env: &HashMap<String, String>,
    profile: Option<&crate::profile::SessionProfile>,
) -> Launch {
    let restricted = profile.is_some_and(|p| p.origin != crate::profile::SessionOrigin::Person);
    let mut env = base_env.clone();
    env.insert("GLASSCODE_SESSION_ID".into(), session.id.clone());
    // Note: the fork injects no vendored CLI onto PATH — sessions run with the user's own PATH only.
    env.insert(
        "GLASSCODE_EVENTS".into(),
        events_file(&session.id).to_string_lossy().into_owned(),
    );

    match session.agent {
        AgentKind::Shell => {
            let shell = crate::sys::default_shell();
            Launch {
                program: shell.program,
                args: shell.session_args,
                env,
            }
        }
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
            let (program, mut args) = agent_program("claude", base_env);
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
            // Not for a session the manager drives: no signed-in browser for it (D28, M23).
            if !restricted {
                args.extend(crate::browser::agent_args(AgentKind::Claude, &session.id));
            }
            args.extend(cli_args(AgentKind::Claude, config_dir.as_deref()));
            if let Some(model) = session.model.as_deref().filter(|m| !m.is_empty()) {
                args.push("--model".into());
                args.push(model.to_string());
            }
            // The mode it was started with; later the person changes it there (Shift-Tab), so a
            // resumed conversation starts in Claude's own default again.
            // A session the manager drives never starts in bypass (its settings disable it too).
            if let Some(mode) = session.mode.as_deref().and_then(claude_mode).filter(|_| !exists).filter(|m| !(restricted && *m == "bypassPermissions")) {
                args.push("--permission-mode".into());
                args.push(mode.into());
            }
            if let Some(profile) = profile {
                args.extend(profile.claude_args.iter().cloned());
            }
            args.push("--settings".into());
            args.push(claude_settings_arg(&session.id, profile, base_env));
            if !exists {
                if let Some(prompt) = &session.initial_prompt {
                    // A message may start with "-" (a list): never an option.
                    args.push("--".into());
                    args.push(prompt.clone());
                }
            }
            Launch { program, args, env }
        }
        AgentKind::Codex => {
            if let Some(dir) = account.and_then(Account::config_dir) {
                env.insert("CODEX_HOME".into(), dir.to_string_lossy().into_owned());
            }
            // The long `-c developer_instructions=…` and a first message reach Codex unchanged only
            // without cmd.exe, as for Claude.
            let (program, mut args) = agent_program("codex", base_env);
            // `-c` overrides go before the `resume` subcommand.
            if !restricted {
                args.extend(crate::browser::agent_args(AgentKind::Codex, &session.id));
            }
            let codex_home = account.map(Account::effective_dir).unwrap_or_else(|| crate::paths::home().join(".codex"));
            // Glasscode made this worktree for a repo the person already works in, so trust it
            // up front: otherwise Codex stops on first use to ask "trust this folder?", and that
            // prompt is easy to miss in chat view.
            // Not for automation and quarantine sessions (trust-19): what a repository carries for
            // Codex gets its prompt there.
            if profile.is_none_or(|p| p.pretrust) {
                ensure_codex_trusts(&codex_home, &session.worktree);
            }
            args.extend(cli_args(AgentKind::Codex, Some(&codex_home)));
            if let Some(model) = session.model.as_deref().filter(|m| !m.is_empty()) {
                args.push("--model".into());
                args.push(model.to_string());
            }
            // The person's mode as before; a session the manager drives runs in its profile's
            // permission profile instead, never with danger-full-access or the bypass.
            if !restricted {
                args.extend(codex_mode_args(session.mode.as_deref()));
            }
            if let Some(profile) = profile {
                args.extend(profile.codex_args.iter().cloned());
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
            Launch { program, args, env }
        }
    }
}

/// The Codex options for a session's mode. Codex 0.167 removed `--full-auto`, which was a short
/// form of "the workspace-write sandbox, and the model asks when it needs to" (`--sandbox
/// workspace-write --ask-for-approval on-request`); passing it made Codex exit before starting.
fn codex_mode_args(mode: Option<&str>) -> Vec<String> {
    let args: &[&str] = match mode {
        Some("full-auto" | "auto") => &["--sandbox", "workspace-write", "--ask-for-approval", "on-request"],
        Some("read-only") => &["--sandbox", "read-only"],
        _ => &[],
    };
    args.iter().map(|a| a.to_string()).collect()
}

/// The arguments of a headless run (after the program's own first words), under its profile.
pub fn headless_args(agent: AgentKind, profile: &crate::profile::SessionProfile) -> Vec<String> {
    match agent {
        AgentKind::Claude => profile.claude_args.clone(),
        AgentKind::Codex => profile.codex_args.clone(),
        AgentKind::Shell => Vec::new(),
    }
}

/// Run the agent once, non-interactively, under a headless profile (M23,
/// `profile::headless_profile`): no command, file, web or MCP tool, with `input` on stdin as
/// labelled data, and the answer as the profile binds it (Claude's JSON on stdout; Codex's last
/// message in its answer file, `answer`). `config_dir` is the account's login folder, so it runs
/// as that account. No GitHub token reaches it. The run is bounded to five minutes.
pub fn headless_run(
    agent: AgentKind,
    config_dir: Option<&Path>,
    profile: &crate::profile::SessionProfile,
    input: &str,
    answer: Option<&Path>,
    base_env: &HashMap<String, String>,
) -> anyhow::Result<String> {
    use std::io::Write as _;
    use std::process::Stdio;
    let mut cmd = headless_command(agent, config_dir, profile, base_env)?;
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let deadline = SystemTime::now() + std::time::Duration::from_secs(300);
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if SystemTime::now() >= deadline {
            let _ = child.kill();
            anyhow::bail!("the run timed out after 5 minutes");
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    match answer.filter(|_| agent == AgentKind::Codex) {
        Some(file) => Ok(std::fs::read_to_string(file)?.trim().to_string()),
        None => Ok(String::from_utf8_lossy(&output.stdout).trim().to_string()),
    }
}

/// The command of a headless run ([`headless_run`]). It runs in the manager's work folder
/// (`paths::manager_work_dir`), a folder of Glasscode's that no session writes, and never in a
/// repository or a shared temporary folder: `claude -p` runs the hooks of its folder's
/// `.claude/settings.json` and connects its `.mcp.json` servers without asking (exec-24).
fn headless_command(agent: AgentKind, config_dir: Option<&Path>, profile: &crate::profile::SessionProfile, base_env: &HashMap<String, String>) -> anyhow::Result<std::process::Command> {
    let (program, first) = match agent {
        AgentKind::Claude => agent_program("claude", base_env),
        AgentKind::Codex => agent_program("codex", base_env),
        AgentKind::Shell => anyhow::bail!("no headless run for a shell"),
    };
    let mut cmd = crate::sys::command(program);
    cmd.args(first);
    cmd.envs(base_env);
    for key in &profile.env_remove {
        cmd.env_remove(key);
    }
    match agent {
        AgentKind::Claude => {
            cmd.env("DISABLE_AUTOUPDATER", "1");
            if let Some(dir) = config_dir {
                cmd.env("CLAUDE_CONFIG_DIR", dir);
            }
        }
        _ => {
            if let Some(dir) = config_dir {
                cmd.env("CODEX_HOME", dir);
            }
        }
    }
    cmd.args(headless_args(agent, profile));
    let folder = crate::paths::manager_work_dir();
    std::fs::create_dir_all(&folder)?;
    cmd.current_dir(folder);
    Ok(cmd)
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
    /// The lines read, as they were (the journal's `HookIntake` takes them).
    pub lines: Vec<String>,
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

    /// Read on from `offset`, where a reader stopped before a restart (the journal's cursors,
    /// events-4), so the lines written meanwhile are read too. An offset past the file's end (it
    /// was made again) starts at its end, as [`EventsTail::new`] does.
    pub fn resume(session_id: &str, offset: Option<u64>) -> Self {
        let mut tail = Self::new(session_id);
        if let Some(offset) = offset.filter(|offset| *offset <= tail.offset) {
            tail.offset = offset;
        }
        tail
    }

    /// Where it has read up to.
    pub fn offset(&self) -> u64 {
        self.offset
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
            update.lines.push(line.to_string());
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

/// The whole lines of a session's spool between two offsets (what a tail would have read there).
pub fn spool_lines(session_id: &str, from: u64, to: u64) -> Vec<String> {
    spool_lines_of(&events_file(session_id), from, to)
}

/// The whole lines of a file between two offsets.
pub fn spool_lines_of(path: &Path, from: u64, to: u64) -> Vec<String> {
    let Ok(mut file) = std::fs::File::open(path) else { return Vec::new() };
    if to <= from || file.seek(SeekFrom::Start(from)).is_err() {
        return Vec::new();
    }
    let mut text = String::new();
    if file.take(to - from).read_to_string(&mut text).is_err() {
        return Vec::new();
    }
    let whole = text.rfind('\n').map(|i| i + 1).unwrap_or(0);
    text[..whole].lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect()
}

/// How long Claude may take from its launch to its first hook event before Glasscode says that it
/// waits on a question: as long as `session_send` gives a session that just started to show its
/// message box.
pub const CLAUDE_START_GRACE: Duration = Duration::from_secs(20);

/// What Glasscode sees of a Claude session that has not started yet.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClaudeStart {
    pub since_launch: Duration,
    pub running: bool,
    /// A hook event came since the launch.
    pub hooked: bool,
    /// Anything is on its screen.
    pub screen_drawn: bool,
    /// Claude's own empty message box is on its screen.
    pub message_box: bool,
    /// It waited at the last look.
    pub waiting: bool,
}

impl ClaudeStart {
    /// Claude's terminal waits on a question that comes before Claude starts (trust this folder,
    /// a new MCP server, the sign-in), which the Chat view cannot show. Claude runs its hooks only
    /// once it has started (`SessionStart` first), so: its process runs, no hook event came since
    /// the launch, the grace is over, and the screen shows something that is not Claude's empty
    /// message box. The screen keeps a session whose hooks never report (turned off, or failing)
    /// from looking like it waits, and a slow start (nothing drawn yet) too.
    pub fn waits(&self) -> bool {
        if !self.running || self.hooked || self.message_box {
            return false;
        }
        // Once it waits, a screen cleared between two questions does not end it.
        self.waiting || (self.since_launch >= CLAUDE_START_GRACE && self.screen_drawn)
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
        // Codex writes the folder as it was given (`C:\Users\dev\app`, in the case of the caller);
        // on Windows the same folder can come in another spelling.
        let same_dir = payload["cwd"].as_str().is_some_and(|c| crate::sys::same_path(Path::new(c), cwd));
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

/// Where the last turn of a Codex conversation stands. Codex has no hooks; its rollout has
/// `task_started` when a turn starts and `task_complete` (or `turn_aborted`, when it is stopped)
/// when it ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CodexTurn {
    /// No turn event read yet.
    #[default]
    Unknown,
    Running,
    Ended,
}

/// The turn event a rollout line is, if it is one.
pub fn codex_turn_event(line: &str) -> Option<CodexTurn> {
    // Most lines are messages and tool output, some very long: parse only those that can be one.
    if !["task_started", "task_complete", "turn_aborted"].iter().any(|kind| line.contains(kind)) {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    if value["type"].as_str() != Some("event_msg") {
        return None;
    }
    match value["payload"]["type"].as_str()? {
        "task_started" => Some(CodexTurn::Running),
        "task_complete" | "turn_aborted" => Some(CodexTurn::Ended),
        _ => None,
    }
}

/// A rollout line that is part of a turn's state (`task_started`, `task_complete`,
/// `turn_aborted`, `error`), parsed; None for the rest.
pub fn codex_turn_line(line: &str) -> Option<Value> {
    if !["task_started", "task_complete", "turn_aborted", "\"error\""].iter().any(|kind| line.contains(kind)) {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    let kind = value["payload"]["type"].as_str()?;
    (value["type"].as_str() == Some("event_msg") && matches!(kind, "task_started" | "task_complete" | "turn_aborted" | "error")).then_some(value)
}

/// How much of a rollout the first read takes: the turn that matters is at its end.
const ROLLOUT_TAIL: u64 = 256 * 1024;

/// Follows the turns of a Codex conversation in its rollout, reading only what was added since
/// the last poll (the first time only the end of the file: a rollout can be very large).
pub struct RolloutTail {
    path: PathBuf,
    offset: u64,
    pub turn: CodexTurn,
    /// The turn's own lines (`task_started`, `task_complete`, `turn_aborted`, `error`), kept for
    /// the reader that asked for them ([`RolloutTail::keeping_events`], M30's turns).
    events: Option<Vec<Value>>,
}

impl RolloutTail {
    /// A conversation that this launch started: its turns so far count.
    pub fn new(path: &Path) -> Self {
        Self { path: path.to_path_buf(), offset: 0, turn: CodexTurn::Unknown, events: None }
    }

    /// Keep each turn line it reads, for [`RolloutTail::take_events`].
    pub fn keeping_events(mut self) -> Self {
        self.events = Some(Vec::new());
        self
    }

    /// The turn lines read since the last call.
    pub fn take_events(&mut self) -> Vec<Value> {
        self.events.as_mut().map(std::mem::take).unwrap_or_default()
    }

    /// A conversation that this launch resumed: a turn that an earlier run left open in the file
    /// is not running, so only what comes from now counts (nothing is told before its first turn,
    /// in case Codex writes it somewhere else).
    pub fn resumed(path: &Path) -> Self {
        let offset = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        Self { path: path.to_path_buf(), offset, turn: CodexTurn::Unknown, events: None }
    }

    /// Read what was added; true when the turn changed.
    pub fn poll(&mut self) -> bool {
        let Ok(file) = std::fs::File::open(&self.path) else { return false };
        let Ok(len) = file.metadata().map(|m| m.len()) else { return false };
        if len < self.offset {
            // Replaced (or truncated): start over.
            self.offset = 0;
            self.turn = CodexTurn::Unknown;
        }
        if len == self.offset {
            return false;
        }
        // A first read of a long file starts in the middle of a line, which is left out.
        let (start, mut cut) = if self.offset == 0 && len > ROLLOUT_TAIL { (len - ROLLOUT_TAIL, true) } else { (self.offset, false) };
        let mut reader = BufReader::new(file);
        if reader.seek(SeekFrom::Start(start)).is_err() {
            return false;
        }
        let before = self.turn;
        let mut at = start;
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                // Whole lines only: a line still being written comes next time.
                Ok(n) if line.ends_with(b"\n") => at += n as u64,
                _ => break,
            }
            if std::mem::take(&mut cut) {
                continue;
            }
            let text = String::from_utf8_lossy(&line);
            if let Some(turn) = codex_turn_event(&text) {
                self.turn = turn;
            }
            if let Some(events) = self.events.as_mut()
                && let Some(event) = codex_turn_line(&text)
            {
                events.push(event);
            }
        }
        self.offset = at;
        self.turn != before
    }
}

/// What a Codex session is doing, from the last turn of its conversation: working while a turn
/// runs and its process is alive, idle once the turn ended. `None` (nothing to tell) before the
/// first turn event, and when the process is gone: a turn it left open is not running, and the
/// app clears the activity of a terminal that exited.
pub fn codex_activity(turn: CodexTurn, process_running: bool) -> Option<AgentActivity> {
    match (turn, process_running) {
        (CodexTurn::Unknown, _) | (_, false) => None,
        (CodexTurn::Running, true) => Some(AgentActivity::Working),
        (CodexTurn::Ended, true) => Some(AgentActivity::Idle),
    }
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
    let output = crate::sys::command(path)
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

    /// exec-24: every headless run (a study pass of moving in, an automatic review) starts in the
    /// manager's work folder, never in a repository or a shared temporary folder, with the flags
    /// that keep it to no tool and set its permission mode itself.
    #[test]
    fn every_headless_run_starts_in_the_work_folder() {
        crate::paths::use_test_root();
        let work = crate::paths::manager_work_dir();
        let env: HashMap<String, String> = [("PATH".to_string(), String::new())].into();
        for kind in [crate::profile::HeadlessKind::Study, crate::profile::HeadlessKind::AutoReview] {
            for agent in [AgentKind::Claude, AgentKind::Codex] {
                let profile = crate::profile::headless_profile(kind, agent, &serde_json::json!({ "type": "object" }), Path::new("/s.json"), Path::new("/a.txt"));
                let cmd = headless_command(agent, None, &profile, &env).unwrap();
                assert_eq!(cmd.get_current_dir(), Some(work.as_path()), "{kind:?} {agent:?}");
                let argv: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
                let has = |pair: &[&str]| argv.windows(pair.len()).any(|w| w.iter().zip(pair).all(|(a, b)| a == b));
                let wanted: &[&[&str]] = match agent {
                    AgentKind::Claude => &[&["--permission-mode", "dontAsk"], &["--strict-mcp-config"], &["--tools", ""], &["--restricted"]],
                    _ => &[&["--sandbox", "read-only"], &["--ephemeral"]],
                };
                for pair in wanted {
                    assert!(has(pair), "{kind:?} {agent:?}: {pair:?} in {argv:?}");
                }
            }
        }
    }

    #[test]
    fn codex_modes_use_options_the_cli_still_has() {
        let auto = ["--sandbox", "workspace-write", "--ask-for-approval", "on-request"].map(String::from).to_vec();
        assert_eq!(codex_mode_args(Some("full-auto")), auto);
        assert_eq!(codex_mode_args(Some("auto")), auto);
        assert_eq!(codex_mode_args(Some("read-only")), ["--sandbox", "read-only"].map(String::from).to_vec());
        assert!(codex_mode_args(Some("default")).is_empty());
        assert!(codex_mode_args(None).is_empty());
        // The headless run is read-only with the network off now (M23, trust-m2).
        let profile = crate::profile::headless_profile(crate::profile::HeadlessKind::AutoReview, AgentKind::Codex, &serde_json::json!({}), Path::new("/s.json"), Path::new("/a.txt"));
        let headless = headless_args(AgentKind::Codex, &profile);
        assert!(headless.windows(2).any(|w| w == ["--sandbox", "read-only"]));
        for args in [codex_mode_args(Some("full-auto")), codex_mode_args(Some("auto")), headless] {
            assert!(!args.iter().any(|a| a == "--full-auto"), "codex 0.167 rejects --full-auto: {args:?}");
        }
    }

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

    fn config(home: &Path) -> (String, toml::Table) {
        let text = std::fs::read_to_string(home.join("config.toml")).unwrap();
        let table = toml::from_str(&text).unwrap_or_else(|e| panic!("not TOML: {e}\n{text}"));
        (text, table)
    }

    #[test]
    fn trusts_a_windows_folder_once_in_valid_toml() {
        let home = tempfile::tempdir().unwrap();
        let wt = Path::new(r"C:\Users\dev\app");
        ensure_codex_trusts(home.path(), wt);
        let (text, table) = config(home.path());
        // In single quotes, as Codex writes a key with `\` itself.
        assert!(text.contains(r"[projects.'C:\Users\dev\app']"), "{text}");
        assert_eq!(table["projects"][r"C:\Users\dev\app"]["trust_level"].as_str(), Some("trusted"));
        ensure_codex_trusts(home.path(), wt);
        let (again, _) = config(home.path());
        assert_eq!(again, text);
    }

    #[test]
    fn an_entry_that_is_there_in_any_quotation_stays_the_only_one() {
        // Codex's own form (the person said no: it stays no), an older Glasscode's, a line in
        // `[projects]`, a dotted key.
        let forms = [
            "[projects.'C:\\Users\\dev\\app']\ntrust_level = \"untrusted\"\n",
            "model = \"o3\"\n\n[projects.\"C:\\\\Users\\\\dev\\\\app\"]\ntrust_level = \"trusted\"\n",
            "[projects]\n\"C:\\\\Users\\\\dev\\\\app\" = { trust_level = \"trusted\" }\n",
            "projects.'C:\\Users\\dev\\app'.trust_level = \"trusted\"\n",
            "[ projects . 'C:\\Users\\dev\\app' ] # by hand\ntrust_level = \"trusted\"\n",
        ];
        for form in forms {
            let home = tempfile::tempdir().unwrap();
            std::fs::write(home.path().join("config.toml"), form).unwrap();
            ensure_codex_trusts(home.path(), Path::new(r"C:\Users\dev\app"));
            let (text, _) = config(home.path());
            assert_eq!(text, form);
        }
        // Another folder, and a key that only starts like it, get an entry of their own.
        let home = tempfile::tempdir().unwrap();
        let form = "[projects.'C:\\Users\\dev\\app.x']\ntrust_level = \"trusted\"\n\n[projects.\"/Users/dev/app\"]\ntrust_level = \"trusted\"\n";
        std::fs::write(home.path().join("config.toml"), form).unwrap();
        ensure_codex_trusts(home.path(), Path::new(r"C:\Users\dev\app"));
        let (text, table) = config(home.path());
        assert_eq!(table["projects"].as_table().map(toml::Table::len), Some(3), "{text}");
    }

    #[cfg(windows)]
    #[test]
    fn a_key_in_another_spelling_of_the_folder_is_the_folder_on_windows() {
        // Codex 0.153 takes such a key for the folder (experiment of S07).
        let home = tempfile::tempdir().unwrap();
        let form = "[projects.'c:\\users\\DEV\\app']\ntrust_level = \"trusted\"\n";
        std::fs::write(home.path().join("config.toml"), form).unwrap();
        ensure_codex_trusts(home.path(), Path::new(r"C:\Users\dev\app"));
        assert_eq!(config(home.path()).0, form);
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
        // On Windows the launch writes the settings file of the session; the test leaves none.
        let settings = settings_file("s1");
        let had_settings = settings.exists();
        let claude = launch(&session(AgentKind::Claude, Some("c1")), None, &HashMap::new()).args;
        if !had_settings {
            let _ = std::fs::remove_file(&settings);
            // The folder too, when nothing else is in it.
            let _ = std::fs::remove_dir(paths::settings_dir());
        }
        assert_eq!(&claude[claude.len() - 2..], ["--", "- fix the retry"]);
        assert!(claude.windows(2).any(|w| w == ["--permission-mode", "plan"]));
        assert!(claude.windows(2).any(|w| w == ["--model", "opus"]));
        // An account of its own: the launch trusts the worktree in the account's config, which
        // without one is the person's own `~/.codex/config.toml`.
        let account = Account { id: format!("codex-test-{}", std::process::id()), agent: AgentKind::Codex, label: "Test".into(), plan: None, system_default: false };
        let codex = launch(&session(AgentKind::Codex, None), Some(&account), &HashMap::new()).args;
        if let Some(dir) = account.config_dir() {
            let _ = std::fs::remove_dir_all(dir);
        }
        assert_eq!(&codex[codex.len() - 2..], ["--", "- fix the retry"]);
    }

    #[test]
    fn the_memory_brief_is_a_second_session_start_hook_only_when_on() {
        let off: Value = serde_json::from_str(&claude_hook_settings(None, false, false, false)).unwrap();
        assert_eq!(off["hooks"]["SessionStart"][0]["hooks"].as_array().map(Vec::len), Some(1), "v1: as before");
        let on: Value = serde_json::from_str(&claude_hook_settings(None, false, false, true)).unwrap();
        let hooks = on["hooks"]["SessionStart"][0]["hooks"].as_array().unwrap();
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0], off["hooks"]["SessionStart"][0]["hooks"][0]);
        assert!(hooks[1]["command"].as_str().unwrap().ends_with("memory brief --hook"));
        for event in ["UserPromptSubmit", "Stop", "Notification"] {
            assert_eq!(on["hooks"][event], off["hooks"][event], "{event}");
        }
        // In a v2 home, by this program's path, as the other hooks.
        let v2: Value = serde_json::from_str(&claude_hook_settings(Some(Path::new("/opt/Glass code/glasscode")), false, false, true)).unwrap();
        let brief = v2["hooks"]["SessionStart"][0]["hooks"][1]["command"].as_str().unwrap().to_string();
        if !cfg!(windows) {
            assert_eq!(brief, "'/opt/Glass code/glasscode' cli memory brief --hook");
        }
    }

    #[test]
    fn in_a_v2_home_every_hook_is_one_write_of_glasscode_hook() {
        let settings: Value = serde_json::from_str(&claude_hook_settings(Some(Path::new("/opt/Glass code/glasscode")), false, false, false)).unwrap();
        let hooks = settings["hooks"].as_object().unwrap();
        assert_eq!(hooks.keys().map(String::as_str).collect::<std::collections::BTreeSet<_>>(), ["Notification", "SessionEnd", "SessionStart", "Stop", "UserPromptSubmit"].into_iter().collect());
        for (event, hook) in hooks {
            let hook = &hook[0]["hooks"][0];
            assert_eq!(hook["timeout"], 10, "{event}");
            if cfg!(windows) {
                assert_eq!(hook["command"], "glasscode hook --stamp", "{event}");
            } else {
                assert_eq!(hook["command"], "'/opt/Glass code/glasscode' cli hook --stamp", "{event}");
            }
        }
        assert_eq!(settings["disableAllHooks"], false);
        // With "turn_hooks", P05's hooks too; never without the v2 engine.
        let turns: Value = serde_json::from_str(&claude_hook_settings(Some(Path::new("/opt/glasscode")), true, false, false)).unwrap();
        assert_eq!(turns["hooks"].as_object().unwrap().len(), 5 + TURN_HOOKS.len());
        assert!(TURN_HOOKS.iter().all(|event| turns["hooks"][event][0]["hooks"][0]["command"] == turns["hooks"]["Stop"][0]["hooks"][0]["command"]));
        assert_eq!(hooks.len(), 5, "not without turn_hooks");
        // On a Claude that knows it, `StopFailure` reports in every v2 home (M32's failed turns).
        let failure: Value = serde_json::from_str(&claude_hook_settings(Some(Path::new("/opt/glasscode")), false, true, false)).unwrap();
        assert_eq!(failure["hooks"].as_object().unwrap().len(), 6);
        assert_eq!(failure["hooks"]["StopFailure"], failure["hooks"]["Stop"]);
    }

    /// The failure hook follows the Claude on the session's PATH: at the minimum version it knows
    /// `StopFailure`; an older one, or one whose version cannot be read, does not get it.
    #[cfg(unix)]
    #[test]
    fn stop_failure_is_hooked_only_on_a_claude_that_knows_it() {
        use std::os::unix::fs::PermissionsExt;
        for (says, knows) in [(format!("{} (Claude Code)", crate::versions::MIN_CLAUDE), true), ("2.1.200 (Claude Code)".to_string(), false), ("no version here".to_string(), false)] {
            let dir = tempfile::tempdir().unwrap();
            let program = dir.path().join("claude");
            std::fs::write(&program, format!("#!/bin/sh\necho '{says}'\n")).unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            let env: HashMap<String, String> = [("PATH".to_string(), dir.path().display().to_string())].into();
            // Not known yet: no hook, and a read starts in the background; then it is known.
            assert!(!claude_knows_stop_failure(&env), "{says}: nothing runs on the launch's thread");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while knows && crate::versions::cached_version(Path::new("claude"), &env).is_none() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            crate::versions::program_version(Path::new("claude"), &env);
            assert_eq!(claude_knows_stop_failure(&env), knows, "{says}");
        }
        assert!(!claude_knows_stop_failure(&HashMap::new()), "no Claude at all");
    }

    #[test]
    fn claude_hooks_report_four_events_and_keep_the_status_line() {
        let settings: Value = serde_json::from_str(&claude_hook_settings(None, true, true, false)).unwrap();
        assert!(settings.get("disableAllHooks").is_none());
        assert_eq!(settings["hooks"].as_object().map(|hooks| hooks.len()), Some(4));
        let command = |event: &str| settings["hooks"][event][0]["hooks"][0]["command"].as_str().unwrap_or_default().to_string();
        if cfg!(windows) {
            for event in ["SessionStart", "UserPromptSubmit", "Stop", "Notification"] {
                assert_eq!(command(event), "glasscode hook", "{event}");
            }
            assert_eq!(settings["statusLine"]["command"], "glasscode statusline");
        } else {
            assert!(command("Stop").starts_with(r#"cat >> "$GLASSCODE_EVENTS""#), "{}", command("Stop"));
            assert_eq!(settings["statusLine"]["command"], crate::usage::STATUSLINE_COMMAND);
        }
        assert_eq!(settings["theme"], "auto");
    }

    /// A rollout line of a kind as the screen check listed them ("event_msg task_started",
    /// "response_item message user", "world_state"), with made-up text.
    fn rollout_line(kind: &str) -> String {
        let mut words = kind.split(' ');
        let outer = words.next().unwrap_or_default();
        let payload = match (outer, words.next(), words.next()) {
            ("event_msg", Some(event), _) => serde_json::json!({"type": event}),
            ("response_item", Some(item), role) => serde_json::json!({"type": item, "role": role, "content": [{"type": "output_text", "text": "Some text"}]}),
            _ => serde_json::json!({}),
        };
        format!("{}\n", serde_json::json!({"timestamp": "2026-01-01T00:00:00.000Z", "type": outer, "payload": payload}))
    }

    fn rollout(kinds: &[&str]) -> String {
        kinds.iter().map(|kind| rollout_line(kind)).collect()
    }

    fn append_to(path: &Path, text: &str) {
        use std::io::Write;
        std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap().write_all(text.as_bytes()).unwrap();
    }

    /// The start of each turn of the screen check's two rollouts, up to the person's message.
    const TURN_START: &[&str] = &["event_msg task_started", "response_item message developer", "response_item message user", "world_state", "turn_context", "response_item message user", "event_msg item_completed"];
    /// The rest of the turn that answered.
    const GOOD_TURN_END: &[&str] = &["event_msg item_completed", "event_msg item_completed", "response_item reasoning", "response_item message assistant", "token_usage_record", "event_msg token_count", "event_msg task_complete"];

    #[test]
    fn a_codex_turn_works_from_its_start_to_its_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-2026-01-01T00-00-00-c1.jsonl");
        // No file yet: nothing to tell.
        let mut tail = RolloutTail::new(&path);
        assert!(!tail.poll());
        assert_eq!((tail.turn, codex_activity(tail.turn, true)), (CodexTurn::Unknown, None));
        // The header alone: still nothing.
        append_to(&path, &rollout(&["session_meta"]));
        assert!(!tail.poll());
        assert_eq!(codex_activity(tail.turn, true), None);
        // A turn in progress.
        append_to(&path, &rollout(TURN_START));
        assert!(tail.poll());
        assert_eq!((tail.turn, codex_activity(tail.turn, true)), (CodexTurn::Running, Some(AgentActivity::Working)));
        // Its end, still being written: it counts once the line is whole.
        let end = rollout_line("event_msg task_complete");
        append_to(&path, &end[..end.len() - 10]);
        assert!(!tail.poll());
        assert_eq!(tail.turn, CodexTurn::Running);
        append_to(&path, &end[end.len() - 10..]);
        assert!(tail.poll());
        assert_eq!((tail.turn, codex_activity(tail.turn, true)), (CodexTurn::Ended, Some(AgentActivity::Idle)));
        // A turn that answered, then one the person stopped.
        append_to(&path, &rollout(TURN_START));
        assert!(tail.poll());
        assert_eq!(tail.turn, CodexTurn::Running);
        append_to(&path, &rollout(GOOD_TURN_END));
        assert!(tail.poll());
        assert_eq!(tail.turn, CodexTurn::Ended);
        append_to(&path, &rollout(TURN_START));
        append_to(&path, &rollout(&["event_msg turn_aborted"]));
        assert!(!tail.poll(), "started and stopped between two polls: ended, as before");
        assert_eq!(codex_activity(tail.turn, true), Some(AgentActivity::Idle));
    }

    #[test]
    fn a_codex_turn_works_only_while_its_process_runs() {
        // A turn with no end, in the file of a process that is gone: not working.
        assert_eq!(codex_activity(CodexTurn::Running, false), None);
        assert_eq!(codex_activity(CodexTurn::Ended, false), None);
        assert_eq!(codex_activity(CodexTurn::Running, true), Some(AgentActivity::Working));
    }

    #[test]
    fn a_resumed_codex_conversation_counts_only_its_new_turns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        // An earlier run was stopped in the middle of a turn: it never wrote the end.
        std::fs::write(&path, rollout(&["session_meta"]) + &rollout(TURN_START)).unwrap();
        let mut tail = RolloutTail::resumed(&path);
        assert!(!tail.poll());
        assert_eq!(codex_activity(tail.turn, true), None);
        append_to(&path, &rollout(TURN_START));
        assert!(tail.poll());
        assert_eq!(codex_activity(tail.turn, true), Some(AgentActivity::Working));
        append_to(&path, &rollout(GOOD_TURN_END));
        assert!(tail.poll());
        assert_eq!(codex_activity(tail.turn, true), Some(AgentActivity::Idle));
    }

    #[test]
    fn the_first_read_of_a_long_rollout_takes_only_its_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        // A turn started long ago, then more than the tail of other lines.
        let filler = serde_json::json!({"type": "response_item", "payload": {"type": "function_call_output", "output": "x".repeat(4000)}}).to_string() + "\n";
        let mut text = rollout(&["session_meta", "event_msg task_started"]);
        while (text.len() as u64) < ROLLOUT_TAIL * 3 {
            text.push_str(&filler);
        }
        std::fs::write(&path, &text).unwrap();
        let mut tail = RolloutTail::new(&path);
        assert!(!tail.poll());
        assert_eq!(tail.turn, CodexTurn::Unknown, "the start of the file was read");
        assert_eq!(tail.offset, text.len() as u64);
        // The end of a long file is read, and after it only what is new.
        let mut tail = RolloutTail::new(&path);
        append_to(&path, &rollout(&["event_msg task_complete", "event_msg task_started"]));
        assert!(tail.poll());
        assert_eq!(tail.turn, CodexTurn::Running);
        append_to(&path, &rollout(&["event_msg task_complete"]));
        assert!(tail.poll());
        assert_eq!(tail.turn, CodexTurn::Ended);
    }

    #[test]
    fn only_turn_events_are_turns() {
        assert_eq!(codex_turn_event(&rollout_line("event_msg task_started")), Some(CodexTurn::Running));
        assert_eq!(codex_turn_event(&rollout_line("event_msg task_complete")), Some(CodexTurn::Ended));
        assert_eq!(codex_turn_event(&rollout_line("event_msg turn_aborted")), Some(CodexTurn::Ended));
        assert_eq!(codex_turn_event(&rollout_line("event_msg token_count")), None);
        // The words in a message are not an event.
        let message = serde_json::json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "why is there no task_complete event?"}]}});
        assert_eq!(codex_turn_event(&message.to_string()), None);
        assert_eq!(codex_turn_event("{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_sta"), None);
    }

    #[test]
    fn claude_waits_on_a_question_before_it_starts() {
        // A question on screen, no hook event yet.
        let asking = |since_launch: Duration| ClaudeStart { since_launch, running: true, screen_drawn: true, ..ClaudeStart::default() };
        let after = CLAUDE_START_GRACE + Duration::from_secs(1);
        // Before the grace: a normal start, whatever is on screen.
        assert!(!asking(Duration::from_secs(3)).waits());
        assert!(!asking(CLAUDE_START_GRACE - Duration::from_millis(1)).waits());
        // After it, with no hook event and a screen that is not Claude's message box: it waits.
        assert!(asking(CLAUDE_START_GRACE).waits());
        assert!(asking(after).waits());
        // After the first hook event: it has started.
        assert!(!ClaudeStart { hooked: true, ..asking(after) }.waits());
        assert!(!ClaudeStart { hooked: true, waiting: true, ..asking(after) }.waits());
        // Its message box on screen: started, and its hooks do not report.
        assert!(!ClaudeStart { message_box: true, ..asking(after) }.waits());
        assert!(!ClaudeStart { message_box: true, waiting: true, ..asking(after) }.waits());
        // Nothing drawn yet: still starting.
        assert!(!ClaudeStart { screen_drawn: false, ..asking(after) }.waits());
        // Its process is gone.
        assert!(!ClaudeStart { running: false, ..asking(after) }.waits());
        assert!(!ClaudeStart { running: false, waiting: true, ..asking(after) }.waits());
        // Between two questions the screen is cleared for a moment: it still waits.
        assert!(ClaudeStart { screen_drawn: false, waiting: true, ..asking(after) }.waits());
    }

    #[test]
    fn a_shell_session_starts_the_default_shell() {
        let shell = launch(&session(AgentKind::Shell, None), None, &HashMap::new());
        let expected = crate::sys::default_shell();
        assert_eq!((shell.program, shell.args), (expected.program, expected.session_args));
    }

    #[cfg(windows)]
    #[test]
    fn a_shell_session_on_windows_is_powershell_without_a_login_flag() {
        let shell = launch(&session(AgentKind::Shell, None), None, &HashMap::new());
        let name = Path::new(&shell.program).file_name().unwrap_or_default().to_ascii_lowercase();
        assert!(name == "pwsh.exe" || name == "powershell.exe", "{}", shell.program);
        assert!(!shell.args.iter().any(|a| a == "-l"), "{:?}", shell.args);
    }
}
