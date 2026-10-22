//! Where Glasscode keeps its files. Everything lives under `~/.glasscode` so a server
//! can mirror the same layout.

use std::path::PathBuf;

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

pub fn root() -> PathBuf {
    if let Ok(dir) = std::env::var("GLASSCODE_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    home().join(".glasscode")
}

pub fn state_file() -> PathBuf {
    root().join("state.json")
}

pub fn accounts_dir() -> PathBuf {
    root().join("accounts")
}

pub fn worktrees_dir() -> PathBuf {
    root().join("worktrees")
}

pub fn events_dir() -> PathBuf {
    root().join("events")
}

pub fn handoff_dir() -> PathBuf {
    root().join("handoff")
}

/// How the Mac app reaches its server (`ServerConfig`).
pub fn server_file() -> PathBuf {
    root().join("server.json")
}

/// The daemon's socket on the server; `glasscoded rpc` connects SSH sessions to it.
pub fn daemon_socket() -> PathBuf {
    root().join("glasscoded.sock")
}

/// Which sessions the daemon keeps running (restarted after a server reboot).
pub fn daemon_file() -> PathBuf {
    root().join("glasscoded.json")
}

pub fn cli_versions_file() -> PathBuf {
    root().join("cli-versions.json")
}

/// The CLIs the person added themselves (on top of the built-in Claude Code and Codex).
pub fn clis_file() -> PathBuf {
    root().join("clis.json")
}

pub fn ensure_dirs() -> std::io::Result<()> {
    for dir in [root(), accounts_dir(), worktrees_dir(), events_dir(), handoff_dir()] {
        std::fs::create_dir_all(&dir)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Account folders hold login tokens: keep them private.
        let _ = std::fs::set_permissions(accounts_dir(), std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}
