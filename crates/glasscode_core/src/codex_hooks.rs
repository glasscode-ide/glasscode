//! Glasscode's own Codex hook and Codex's trust of it (M22, D11, P06): a Codex session gets
//! memory's brief from a `SessionStart` hook, as a Claude session does, instead of asking for it.
//!
//! Codex runs a hook only when its definition is trusted:
//! - **Where the hook lives.** The hook is in the account's `hooks.json` (`CODEX_HOME`).
//! - **Where its trust lives.** The trust is a `[hooks.state."<key>"] trusted_hash = "<hash>"`
//!   entry in the account's `config.toml`, with the key and hash `codex app-server` reports in
//!   `hooks/list`. A session flag cannot grant trust, and a repository cannot trust its own hooks.
//! - **When it is trusted.** Glasscode adds the hook, asks `hooks/list`, and writes the trust of
//!   its own hook only. A repository's hooks stay untrusted until the person reviews them; a
//!   changed Glasscode hook becomes "modified" until its new hash is written.
//!
//! It runs in the background at the start (`trust_brief_hook`), never while a session launches,
//! because asking `codex app-server` takes a process. A launch only reads what came of it
//! ([`brief_by_hook`]): a Codex home whose hook is trusted gets no "ask for your brief" line in
//! its instructions.

use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde_json::{Value, json};

/// What marks Glasscode's own brief hook among the account's hooks (its command's end).
pub const BRIEF_MARK: &str = "memory brief --hook";

/// The longest `codex app-server` may take to answer.
const ANSWER_LIMIT: Duration = Duration::from_secs(20);

/// The account folders whose brief hook Codex trusts, as this run found them.
static TRUSTED: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// Whether Codex runs Glasscode's brief hook for this account folder (as the start found it).
pub fn brief_by_hook(codex_home: &Path) -> bool {
    TRUSTED.lock().map(|set| set.contains(codex_home)).unwrap_or(false)
}

fn set_trusted(codex_home: &Path, trusted: bool) {
    if let Ok(mut set) = TRUSTED.lock() {
        if trusted {
            set.insert(codex_home.to_path_buf());
        } else {
            set.remove(codex_home);
        }
    }
}

/// An account's `hooks.json` with Glasscode's brief hook under `SessionStart`, its command
/// `command`: the group of Glasscode's hook is written again in its place, every other hook kept
/// as it is. The text, and the place of Glasscode's group in the `SessionStart` list.
pub fn with_brief_hook(existing: Option<&str>, command: &str) -> Result<(String, usize), String> {
    let mut file: Value = match existing.map(str::trim).filter(|t| !t.is_empty()) {
        Some(text) => serde_json::from_str(text).map_err(|e| format!("its hooks.json is not JSON ({e}), so Glasscode adds nothing to it"))?,
        None => json!({}),
    };
    if !file.is_object() {
        return Err("its hooks.json is not an object".into());
    }
    if !file["hooks"].is_object() {
        file["hooks"] = json!({});
    }
    let ours = json!({ "hooks": [{ "type": "command", "command": command, "timeout": 20 }] });
    let starts = file["hooks"]["SessionStart"].as_array().cloned().unwrap_or_default();
    let is_ours = |group: &Value| group["hooks"].as_array().is_some_and(|handlers| handlers.iter().any(|h| h["command"].as_str().is_some_and(|c| c.ends_with(BRIEF_MARK))));
    let mut starts: Vec<Value> = starts;
    let at = match starts.iter().position(is_ours) {
        Some(at) => {
            starts[at] = ours;
            at
        }
        None => {
            starts.push(ours);
            starts.len() - 1
        }
    };
    file["hooks"]["SessionStart"] = Value::Array(starts);
    Ok((serde_json::to_string_pretty(&file).unwrap_or_default() + "\n", at))
}

/// Glasscode's brief hook in what `hooks/list` answered: the hooks of the account's `hooks.json`
/// (`source` user) under `SessionStart`, in Glasscode's group. Their keys, hashes and trust.
pub fn ours_in_list(list: &Value, file: &Path, group: usize) -> Vec<(String, String, String)> {
    let prefix = format!("{}:", file.display());
    list["data"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|entry| entry["hooks"].as_array().cloned().unwrap_or_default())
        .filter(|hook| hook["source"] == "user")
        .filter(|hook| hook["eventName"].as_str().is_some_and(|e| e.replace('_', "").eq_ignore_ascii_case("sessionstart")))
        .filter_map(|hook| {
            let key = hook["key"].as_str()?.to_string();
            let rest = key.strip_prefix(&prefix)?;
            // `<event>:<group>:<handler>`.
            let mut parts = rest.rsplitn(3, ':');
            let (_handler, in_group) = (parts.next()?, parts.next()?.parse::<usize>().ok()?);
            let hash = hook["currentHash"].as_str()?.to_string();
            (in_group == group).then(|| (key.clone(), hash, hook["trustStatus"].as_str().unwrap_or_default().to_string()))
        })
        .collect()
}

/// The words after the program for `codex app-server`: its update check off (versions are
/// Glasscode's), then the subcommand.
pub fn app_server_args() -> Vec<String> {
    vec!["-c".into(), crate::agents::CODEX_NO_UPDATE_CHECK.into(), "app-server".into()]
}

/// `codex app-server`'s `hooks/list` for `cwd`, as the account at `codex_home` sees it: the program
/// is asked on stdio (`initialize`, `initialized`, `hooks/list`) and stopped. It runs a process:
/// never on the main thread.
pub fn hooks_list(program: &str, first: &[String], codex_home: &Path, cwd: &Path, env: &HashMap<String, String>) -> anyhow::Result<Value> {
    let mut cmd = Command::new(program);
    cmd.args(first).args(app_server_args()).envs(env).env("CODEX_HOME", codex_home).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().context("codex app-server did not start")?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    let stdout = child.stdout.take().context("no stdout")?;
    let messages = [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "clientInfo": { "name": "glasscode", "version": env!("CARGO_PKG_VERSION") } } }),
        json!({ "jsonrpc": "2.0", "method": "initialized" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "hooks/list", "params": { "cwds": [cwd] } }),
    ];
    for message in &messages {
        writeln!(stdin, "{message}").context("codex app-server did not take the request")?;
    }
    stdin.flush().ok();
    // The answer is read on a thread of its own, so a server that says nothing is stopped in time.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(message) = serde_json::from_str::<Value>(&line)
                && message["id"] == 2
            {
                let _ = tx.send(message);
                return;
            }
        }
    });
    let started = Instant::now();
    let answer = rx.recv_timeout(ANSWER_LIMIT.saturating_sub(started.elapsed()));
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    let answer = answer.map_err(|_| anyhow::anyhow!("codex app-server did not answer hooks/list within {} s", ANSWER_LIMIT.as_secs()))?;
    if let Some(error) = answer.get("error") {
        bail!("codex app-server refused hooks/list: {}", error["message"].as_str().unwrap_or("no reason"));
    }
    Ok(answer["result"].clone())
}

/// Make Codex run Glasscode's brief hook for the account at `codex_home`: the hook in its
/// `hooks.json`, and the trust of that hook (only) in its `config.toml`, from `hooks/list`. True
/// when Codex now runs it. It writes the account's files and runs `codex app-server`: in the
/// background only.
pub fn trust_brief_hook(program: &str, first: &[String], codex_home: &Path, cwd: &Path, command: &str, env: &HashMap<String, String>) -> anyhow::Result<bool> {
    std::fs::create_dir_all(codex_home)?;
    let hooks_file = codex_home.join("hooks.json");
    let existing = std::fs::read_to_string(&hooks_file).ok();
    let (text, group) = with_brief_hook(existing.as_deref(), command).map_err(|why| anyhow::anyhow!(why))?;
    if existing.as_deref() != Some(text.as_str()) {
        std::fs::write(&hooks_file, &text)?;
    }
    let list = hooks_list(program, first, codex_home, cwd, env)?;
    let ours = ours_in_list(&list, &hooks_file, group);
    if ours.is_empty() {
        set_trusted(codex_home, false);
        bail!("hooks/list does not show Glasscode's brief hook");
    }
    let untrusted: Vec<(String, String)> = ours.iter().filter(|(_, _, status)| status != "trusted").map(|(key, hash, _)| (key.clone(), hash.clone())).collect();
    if !untrusted.is_empty() {
        let config_file = codex_home.join("config.toml");
        let config = std::fs::read_to_string(&config_file).unwrap_or_default();
        std::fs::write(&config_file, crate::profile::with_hook_trust(&config, &untrusted))?;
    }
    set_trusted(codex_home, true);
    Ok(true)
}

// ----- uninstall (M25) ----------------------------------------------------------------------

/// An account's `hooks.json` without Glasscode's brief hook: its group goes, everything else
/// stays. None when Glasscode's hook is not in it; else the text, and where its group was.
pub fn without_brief_hook(text: &str) -> Option<(String, usize)> {
    let mut file: Value = serde_json::from_str(text).ok()?;
    let starts = file["hooks"]["SessionStart"].as_array()?.clone();
    let at = starts.iter().position(|group| group["hooks"].as_array().is_some_and(|handlers| handlers.iter().any(|h| h["command"].as_str().is_some_and(|c| c.ends_with(BRIEF_MARK)))))?;
    let mut starts = starts;
    starts.remove(at);
    if starts.is_empty() {
        if let Some(hooks) = file["hooks"].as_object_mut() {
            hooks.remove("SessionStart");
        }
    } else {
        file["hooks"]["SessionStart"] = Value::Array(starts);
    }
    Some((serde_json::to_string_pretty(&file).unwrap_or_default() + "\n", at))
}

/// An account's `config.toml` without the trust Glasscode wrote for its brief hook (the
/// `hooks.state` tables of `hooks_file`'s `SessionStart` group `group`), each with its lines up to
/// the next table; everything else stays.
pub fn without_brief_trust(config: &str, hooks_file: &Path, group: usize) -> String {
    let prefix = format!("{}:", hooks_file.display());
    let ours = |key: &str| {
        let Some(rest) = key.strip_prefix(&prefix) else { return false };
        let mut parts = rest.rsplitn(3, ':');
        let (_, in_group, event) = (parts.next(), parts.next().and_then(|g| g.parse::<usize>().ok()), parts.next().unwrap_or_default());
        in_group == Some(group) && event.replace('_', "").eq_ignore_ascii_case("sessionstart")
    };
    let mut out: Vec<&str> = Vec::new();
    let mut skip = false;
    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            skip = trimmed.strip_prefix("[hooks.state.").and_then(|k| k.strip_suffix(']')).map(|k| serde_json::from_str::<String>(k).unwrap_or_else(|_| k.to_string())).is_some_and(|key| ours(&key));
        }
        if !skip {
            out.push(line);
        }
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    let mut text = out.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `codex app-server` runs with Codex's update check off, the option before the subcommand.
    #[test]
    fn app_server_never_checks_for_updates() {
        assert_eq!(app_server_args(), ["-c", "check_for_update_on_startup=false", "app-server"]);
    }

    /// Glasscode's group goes in once, in its place; the person's own hooks stay as they are.
    #[test]
    fn the_brief_hook_joins_the_accounts_hooks() {
        let (text, at) = with_brief_hook(None, "/app/glasscode cli memory brief --hook").unwrap();
        assert_eq!(at, 0);
        let mine = r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"say hello"}]}],"Stop":[{"hooks":[{"type":"command","command":"beep"}]}]}}"#;
        let (with, at) = with_brief_hook(Some(mine), "/app/glasscode cli memory brief --hook").unwrap();
        assert_eq!(at, 1);
        let value: Value = serde_json::from_str(&with).unwrap();
        assert_eq!(value["hooks"]["SessionStart"][0]["hooks"][0]["command"], "say hello");
        assert_eq!(value["hooks"]["Stop"][0]["hooks"][0]["command"], "beep");
        // Again, or with the app moved: the same group, written in its place.
        let (again, at) = with_brief_hook(Some(&with), "/new/glasscode cli memory brief --hook").unwrap();
        let value: Value = serde_json::from_str(&again).unwrap();
        assert_eq!((at, value["hooks"]["SessionStart"].as_array().unwrap().len()), (1, 2));
        assert_eq!(value["hooks"]["SessionStart"][1]["hooks"][0]["command"], "/new/glasscode cli memory brief --hook");
        assert_eq!(with_brief_hook(Some(&again), "/new/glasscode cli memory brief --hook").unwrap().0, again, "the same hook changes nothing");
        assert!(serde_json::from_str::<Value>(&text).is_ok());
        assert!(with_brief_hook(Some("not json"), "x").is_err(), "a file Glasscode cannot read is left alone");
    }

    /// M25: uninstall takes out Glasscode's hook and its trust, and leaves the person's.
    #[test]
    fn uninstall_takes_out_only_glasscodes_hook() {
        let mine = r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"say hello"}]}]}}"#;
        let (with, at) = with_brief_hook(Some(mine), "/app/glasscode cli memory brief --hook").unwrap();
        let (without, was) = without_brief_hook(&with).unwrap();
        assert_eq!(was, at);
        assert_eq!(serde_json::from_str::<Value>(&without).unwrap(), serde_json::from_str::<Value>(mine).unwrap());
        assert!(without_brief_hook(mine).is_none());
        let file = Path::new("/h/.codex/hooks.json");
        let config = "model = \"x\"\n\n[hooks.state.\"/h/.codex/hooks.json:session_start:0:0\"]\ntrusted_hash = \"sha256:theirs\"\n\n[hooks.state.\"/h/.codex/hooks.json:session_start:1:0\"]\ntrusted_hash = \"sha256:ours\"\n\n[projects.\"/w\"]\ntrust_level = \"trusted\"\n";
        let kept = without_brief_trust(config, file, 1);
        assert!(!kept.contains("sha256:ours") && kept.contains("sha256:theirs") && kept.contains("[projects.\"/w\"]") && kept.starts_with("model = \"x\""), "{kept}");
    }

    #[test]
    fn our_hook_is_found_in_hooks_list() {
        let file = Path::new("/h/.codex/hooks.json");
        let list = json!({ "data": [{ "hooks": [
            { "key": "/h/.codex/hooks.json:session_start:0:0", "source": "user", "eventName": "sessionStart", "currentHash": "sha256:a", "trustStatus": "trusted" },
            { "key": "/h/.codex/hooks.json:session_start:1:0", "source": "user", "eventName": "sessionStart", "currentHash": "sha256:b", "trustStatus": "untrusted" },
            { "key": "/w/.codex/hooks.json:session_start:1:0", "source": "project", "eventName": "sessionStart", "currentHash": "sha256:c", "trustStatus": "untrusted" },
            { "key": "/h/.codex/hooks.json:stop:1:0", "source": "user", "eventName": "stop", "currentHash": "sha256:d", "trustStatus": "untrusted" },
        ] }] });
        assert_eq!(ours_in_list(&list, file, 1), [("/h/.codex/hooks.json:session_start:1:0".to_string(), "sha256:b".to_string(), "untrusted".to_string())]);
        assert_eq!(ours_in_list(&list, file, 2), []);
    }

    /// P06 with the Codex stand-in's `app-server`: the hook goes into hooks.json, its trust into
    /// config.toml, the person's own hooks are not trusted by it, and a second run changes nothing.
    #[cfg(unix)]
    #[test]
    fn codex_trusts_glasscodes_brief_hook() {
        let home = crate::testkit::home::Home::new();
        let codex_home = home.codex_home();
        std::fs::create_dir_all(&codex_home).unwrap();
        std::fs::write(codex_home.join("hooks.json"), r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"say hello"}]}]}}"#).unwrap();
        std::fs::write(codex_home.join("config.toml"), "model = \"x\"\n").unwrap();
        let mut env = home.agent_env();
        env.insert("GLASSCODE_TEST_CLI_HELP".into(), crate::testkit::agents::scripts_dir().join("..").join("cli-help").to_string_lossy().into_owned());
        let program = crate::testkit::agents::scripts_dir().join("codex").to_string_lossy().into_owned();
        let command = "/app/glasscode cli memory brief --hook";
        assert!(!brief_by_hook(&codex_home));
        assert!(trust_brief_hook(&program, &[], &codex_home, home.top(), command, &env).unwrap());
        assert!(brief_by_hook(&codex_home));
        let config = std::fs::read_to_string(codex_home.join("config.toml")).unwrap();
        assert!(config.starts_with("model = \"x\"\n") && config.contains("session_start:1:0\"]\ntrusted_hash = \"sha256:"), "{config}");
        assert!(!config.contains("session_start:0:0"), "the person's own hook is theirs to trust: {config}");
        let list = hooks_list(&program, &[], &codex_home, home.top(), &env).unwrap();
        let ours = ours_in_list(&list, &codex_home.join("hooks.json"), 1);
        assert_eq!(ours.iter().map(|(_, _, s)| s.as_str()).collect::<Vec<_>>(), ["trusted"]);
        trust_brief_hook(&program, &[], &codex_home, home.top(), command, &env).unwrap();
        assert_eq!(std::fs::read_to_string(codex_home.join("config.toml")).unwrap(), config, "trusted already: nothing written again");
        set_trusted(&codex_home, false);
    }
}
