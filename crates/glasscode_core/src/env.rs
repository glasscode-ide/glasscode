//! The environment agent processes run with.
//!
//! Apps opened from Finder get a minimal PATH, so `claude` and `codex` would not be found.
//! We ask the user's login shell for its environment once at startup and use that instead.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Variables that must never leak from the process that launched Glasscode into sessions.
/// (For example, launching Glasscode from inside Claude Code sets `CLAUDECODE=1`.)
pub const SCRUBBED_VARS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_CODE_SSE_PORT",
    "CODEX_HOME",
    "CODEX_SANDBOX",
    "CODEX_THREAD_ID",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERM_SESSION_ID",
    "ITERM_SESSION_ID",
    "VSCODE_INJECTION",
    "ZED_TERM",
];

pub fn user_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/zsh".to_string())
}

/// Environment of an interactive login shell, falling back to our own environment.
pub fn login_shell_env() -> HashMap<String, String> {
    let shell = user_shell();
    let captured = capture(&shell).unwrap_or_else(|err| {
        log::warn!("could not read login shell environment: {err}");
        std::env::vars().collect()
    });
    captured
        .into_iter()
        .filter(|(key, _)| !SCRUBBED_VARS.contains(&key.as_str()) && key != "SHLVL" && key != "_")
        .collect()
}

fn capture(shell: &str) -> anyhow::Result<HashMap<String, String>> {
    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c", "/usr/bin/env -0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            anyhow::bail!("login shell took too long");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let buf = reader.join().unwrap_or_default();
    let mut vars = HashMap::new();
    for entry in buf.split(|b| *b == 0) {
        let entry = String::from_utf8_lossy(entry);
        if let Some((key, value)) = entry.split_once('=') {
            if !key.is_empty() && !key.contains('\n') {
                vars.insert(key.to_string(), value.to_string());
            }
        }
    }
    if !vars.contains_key("PATH") {
        anyhow::bail!("login shell returned no PATH");
    }
    Ok(vars)
}

/// Prefixes of variables an agent CLI sets for its own child processes. When Glasscode is
/// started from inside such a session they would leak into every session we launch (Claude
/// Code, for example, turns off transcript saving when it sees its child-session marker).
/// Values the user sets in their shell profile come back via `login_shell_env`.
const SCRUBBED_PREFIXES: &[&str] = &["CLAUDE", "CODEX", "AI_AGENT"];

/// Remove inherited variables from our own process so child processes never see them.
/// Must run at startup before any threads are spawned.
pub fn scrub_process_env() {
    let inherited: Vec<String> = std::env::vars()
        .map(|(key, _)| key)
        .filter(|key| {
            SCRUBBED_VARS.contains(&key.as_str())
                || SCRUBBED_PREFIXES.iter().any(|prefix| key.starts_with(prefix))
        })
        .collect();
    for key in inherited {
        // SAFETY: called once at the very start of `main`, before other threads exist.
        unsafe { std::env::remove_var(key) };
    }
}
