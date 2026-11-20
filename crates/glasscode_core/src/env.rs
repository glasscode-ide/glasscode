//! The environment agent processes run with.
//!
//! Apps opened from Finder get a minimal PATH, so `claude` and `codex` would not be found.
//! We ask the user's login shell for its environment once at startup and use that instead.
//! On Windows, the app already has the person's full environment.

use std::collections::HashMap;

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

/// The shell program for terminal sessions.
pub fn user_shell() -> String {
    crate::sys::default_shell().program
}

/// Environment of an interactive login shell, falling back to our own environment. On Windows
/// it is our own environment: no shell profile adds to it there.
pub fn login_shell_env() -> HashMap<String, String> {
    #[cfg(unix)]
    let captured = capture(&user_shell()).unwrap_or_else(|err| {
        log::warn!("could not read login shell environment: {err}");
        std::env::vars().collect()
    });
    #[cfg(windows)]
    let captured = process_env();
    let mut env: HashMap<String, String> = captured.into_iter().filter(|(key, _)| !SCRUBBED_VARS.contains(&key.as_str()) && key != "SHLVL" && key != "_").collect();
    keep_stand_ins_first(&mut env, std::env::var(STAND_INS).ok().as_deref(), std::env::var("PATH").ok().as_deref());
    env
}

/// The variable of a scratch home made for the stand-in agents (`testkit::home`, `make-test-home`).
pub const STAND_INS: &str = "GLASSCODE_TEST_SCENARIOS";

/// In a scratch home with the stand-in agents, the search path Glasscode was started with stays
/// as it is: it puts the stand-ins first, and a login shell's `path_helper` would put the real
/// `claude` and `codex` (Homebrew's) before them, so a scratch run would start the real CLIs.
fn keep_stand_ins_first(env: &mut HashMap<String, String>, stand_ins: Option<&str>, path: Option<&str>) {
    if let (Some(scenarios), Some(path)) = (stand_ins.filter(|s| !s.is_empty()), path) {
        env.insert(STAND_INS.to_string(), scenarios.to_string());
        env.insert("PATH".to_string(), path.to_string());
    }
}

/// Our own environment, with the search path under the name that the rest of Glasscode reads
/// (Windows keeps it as `Path`).
#[cfg(windows)]
fn process_env() -> HashMap<String, String> {
    std::env::vars_os()
        .filter_map(|(key, value)| {
            let (Some(name), Some(value)) = (key.to_str(), value.to_str()) else {
                log::warn!("left out the environment variable {}: it is not valid Unicode", key.display());
                return None;
            };
            let name = if name.eq_ignore_ascii_case("PATH") { "PATH" } else { name };
            Some((name.to_string(), value.to_string()))
        })
        .collect()
}

#[cfg(unix)]
fn capture(shell: &str) -> anyhow::Result<HashMap<String, String>> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let mut child = crate::sys::command(shell)
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

#[cfg(test)]
mod tests {

    /// A scratch run with the stand-ins keeps them first on the path; any other run takes the
    /// login shell's path, as before.
    #[test]
    fn a_scratch_run_keeps_the_stand_ins_first() {
        let shell = || HashMap::from([("PATH".to_string(), "/opt/homebrew/bin:/usr/bin".to_string())]);
        let mut env = shell();
        keep_stand_ins_first(&mut env, Some("/scratch/scenarios"), Some("/repo/script/test-agents:/usr/bin"));
        assert_eq!((env["PATH"].as_str(), env[STAND_INS].as_str()), ("/repo/script/test-agents:/usr/bin", "/scratch/scenarios"));
        let mut env = shell();
        keep_stand_ins_first(&mut env, None, Some("/repo/script/test-agents:/usr/bin"));
        assert_eq!(env["PATH"], "/opt/homebrew/bin:/usr/bin");
    }

    use super::*;

    #[test]
    fn the_session_environment_has_a_path_and_no_agent_marker() {
        let before = std::env::var_os("CLAUDECODE");
        // SAFETY: the core tests run one at a time (`--test-threads=1`).
        unsafe { std::env::set_var("CLAUDECODE", "1") };
        let env = login_shell_env();
        match before {
            Some(value) => unsafe { std::env::set_var("CLAUDECODE", value) },
            None => unsafe { std::env::remove_var("CLAUDECODE") },
        }
        assert!(env.get("PATH").is_some_and(|path| !path.is_empty()), "no PATH");
        assert!(!env.contains_key("CLAUDECODE"));
    }
}
