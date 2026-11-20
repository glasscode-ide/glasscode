//! The server's part of Settings › Manager: whether this server is ready to be the manager's home
//! (`manager.test`), and the account its brain works as here.
//!
//! Since M56 (row 6), v1's copy of the manager does not run here any more. Its rounds watched
//! GitHub, follow-ups and teammates' messages, ran v1's brain and kept `data.json` in step with
//! the Mac through `manager.sync`. With v2, the server's manager is `brain.rs` (M57):
//! - the wake lane and the cards the person approved, while the home's lease says this server is
//!   home;
//! - the Mac's journal, memory, work and conversation, through the mirror's sync.
//!
//! A v1 Mac that still calls `manager.sync` hears that nothing runs here, and keeps its manager at
//! home (`SyncReply::default`: no heartbeat).

use std::collections::HashMap;

use glasscode_core::agents;
use glasscode_core::github;
use glasscode_core::manager::sync::{SyncReply, TestLine};
use glasscode_core::manager::{Settings, gh};
use glasscode_core::model::{Account, AgentKind, Defaults};
use glasscode_core::remote::ServerState;
use serde_json::{Value, json};

use crate::daemon::{Shared, lock};

/// `manager.*` requests. None: not ours.
pub fn dispatch(shared: &Shared, method: &str, params: &Value) -> Option<anyhow::Result<Value>> {
    match method {
        "manager.test" => Some(test(shared, params)),
        // A v1 Mac's sync: nothing of v1 runs here, so its manager stays at home.
        "manager.sync" => Some(serde_json::to_value(SyncReply::default()).map_err(Into::into)),
        "manager.ask" | "manager.status" => Some(Err(anyhow::anyhow!("v1's manager does not run on the server any more; the manager's chat is in Glasscode on the Mac"))),
        _ => None,
    }
}

/// The account the brain runs as here: the one the settings name when the server has it, else
/// the server's default for that agent, else any.
pub(crate) fn brain_account<'a>(settings: &Settings, accounts: &'a [Account], defaults: &Defaults) -> Option<&'a Account> {
    let of_kind = |a: &&Account| a.agent == settings.brain;
    settings
        .account
        .as_deref()
        .and_then(|id| accounts.iter().filter(of_kind).find(|a| a.id == id))
        .or_else(|| defaults.for_agent(settings.brain).and_then(|id| accounts.iter().filter(of_kind).find(|a| &a.id == id)))
        .or_else(|| accounts.iter().find(of_kind))
}

pub(crate) fn line(what: &str, ok: bool, detail: impl Into<String>) -> TestLine {
    TestLine { what: what.into(), ok, detail: detail.into() }
}

fn folder_line() -> TestLine {
    let dir = glasscode_core::paths::root().join("manager");
    let probe = dir.join(".write-test");
    let written = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&probe, b"ok")).and_then(|()| std::fs::remove_file(&probe));
    match written {
        Ok(()) => line("Its folder", true, format!("{} can be written", dir.display())),
        Err(error) => line("Its folder", false, format!("{} cannot be written: {error}", dir.display())),
    }
}

fn github_line(env: &HashMap<String, String>) -> TestLine {
    match github::gh_ready(env) {
        Ok(()) => match gh::my_login(env) {
            Ok(login) if !login.is_empty() => line("GitHub", true, format!("gh is signed in as {login}")),
            _ => line("GitHub", true, "gh is signed in"),
        },
        Err(why) => line("GitHub", false, why),
    }
}

fn brain_line(settings: &Settings, server: &ServerState, env: &HashMap<String, String>) -> TestLine {
    let what = "Its agent";
    let program = match settings.brain {
        AgentKind::Claude => "claude",
        AgentKind::Codex => "codex",
        AgentKind::Shell => return line(what, false, "Choose Claude or Codex as the agent that thinks for it."),
    };
    let name = settings.brain.name();
    let Some(version) = agents::installed_version(program, env) else {
        return line(what, false, format!("{program} is not installed on the server (or not on the login shell's PATH)."));
    };
    match brain_account(settings, &server.accounts, &server.defaults) {
        None => line(what, false, format!("{name} {version} is installed, but the server has no {name} account. Add one in the Server dialog.")),
        Some(account) if !server.signed_in.get(&account.id).copied().unwrap_or(false) => {
            line(what, false, format!("{name} {version} is installed, but \u{201c}{}\u{201d} is not signed in on the server. Sign in from the Server dialog.", account.label))
        }
        Some(account) => line(what, true, format!("{name} {version}, signed in as \u{201c}{}\u{201d}", account.label)),
    }
}

fn sessions_line(server: &ServerState) -> TestLine {
    let what = "Sessions here";
    if crate::tmux::Tmux::version().is_none() {
        return line(what, false, "tmux is not installed on the server.");
    }
    let signed_in = server.accounts.iter().filter(|a| a.agent.uses_accounts() && server.signed_in.get(&a.id).copied().unwrap_or(false)).count();
    match (signed_in, server.accounts.iter().filter(|a| a.agent.uses_accounts()).count()) {
        (0, 0) => line(what, false, "The server has no Claude or Codex account yet. Add one in the Server dialog."),
        (0, _) => line(what, false, "No account is signed in on the server. Sign in from the Server dialog."),
        (n, _) => line(what, true, format!("It can start sessions here ({n} account{} signed in, {} project{})", if n == 1 { "" } else { "s" }, server.projects.len(), if server.projects.len() == 1 { "" } else { "s" })),
    }
}

/// `manager.test`: is this server ready to be the manager's home? `settings` in the request are
/// the Mac's; without them (`glasscoded manager status`), what the Mac last said of its brain.
fn test(shared: &Shared, params: &Value) -> anyhow::Result<Value> {
    let settings: Settings = params.get("settings").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_else(|| {
        let said = crate::brain::from_mac();
        Settings { brain: said.as_ref().and_then(|s| s.agent).unwrap_or(AgentKind::Claude), account: said.and_then(|s| s.account), ..Settings::default() }
    });
    let (env, server) = {
        let daemon = lock(shared);
        (daemon.env.clone(), daemon.snapshot())
    };
    // The slow ones (they start programs) side by side, so the Mac is not kept waiting.
    let mut lines = std::thread::scope(|scope| {
        let github = scope.spawn(|| github_line(&env));
        let brain = scope.spawn(|| brain_line(&settings, &server, &env));
        let done = |handle: std::thread::ScopedJoinHandle<'_, TestLine>, what: &str| handle.join().unwrap_or_else(|_| line(what, false, "This check failed."));
        vec![folder_line(), done(github, "GitHub"), done(brain, "Its agent"), sessions_line(&server)]
    });
    // The v2 manager here (M57): what decides whether its wake lane runs on this server.
    lines.extend(crate::brain::test_lines(shared));
    Ok(serde_json::to_value(lines)?)
}

// ----- `glasscoded manager …` -------------------------------------------------------------------

/// `glasscoded manager status`: the server test's lines, through the daemon's socket.
pub fn command(args: &[String]) -> anyhow::Result<()> {
    match args.first().map(String::as_str) {
        Some("status") => {
            let lines: Vec<TestLine> = serde_json::from_value(crate::request("manager.test", json!({}))?)?;
            for line in lines {
                println!("{} {}: {}", if line.ok { "ok  " } else { "FAIL" }, line.what, line.detail);
            }
            Ok(())
        }
        _ => anyhow::bail!("usage: glasscoded manager status (the manager's chat is in Glasscode on the Mac)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_brain_runs_as_the_servers_account() {
        let account = |id: &str, agent| Account { id: id.into(), agent, label: id.into(), plan: None, system_default: false };
        let accounts = vec![account("codex-1", AgentKind::Codex), account("claude-1", AgentKind::Claude), account("claude-2", AgentKind::Claude)];
        let defaults = Defaults { claude: Some("claude-2".into()), codex: None };
        let settings = |brain, account: Option<&str>| Settings { brain, account: account.map(str::to_string), ..Settings::default() };
        let pick = |settings: &Settings| brain_account(settings, &accounts, &defaults).map(|a| a.id.clone());
        // The one the settings name, when the server has it.
        assert_eq!(pick(&settings(AgentKind::Claude, Some("claude-1"))).as_deref(), Some("claude-1"));
        // The Mac's account is not here: the server's default.
        assert_eq!(pick(&settings(AgentKind::Claude, Some("mac-only"))).as_deref(), Some("claude-2"));
        // No default: any account of that agent.
        assert_eq!(pick(&settings(AgentKind::Codex, None)).as_deref(), Some("codex-1"));
        assert_eq!(brain_account(&settings(AgentKind::Claude, None), &[], &defaults).map(|a| a.id.clone()), None);
    }

    /// M56 row 6: v1's sync is answered with no heartbeat, so a v1 Mac keeps its manager at home.
    #[test]
    fn a_v1_mac_hears_that_nothing_of_v1_runs_here() {
        let reply: SyncReply = serde_json::from_value(serde_json::to_value(SyncReply::default()).unwrap()).unwrap();
        let heartbeat = glasscode_core::manager::sync::heartbeat_here(&reply, 1_000);
        assert_eq!(heartbeat, 0);
        let settings = Settings { enabled: true, home: glasscode_core::manager::Home::Server, ..Settings::default() };
        assert!(!glasscode_core::manager::sync::runs_on_server(&settings, heartbeat, 1_000, 30));
    }
}
