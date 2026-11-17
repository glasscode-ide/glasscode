//! Where Glasscode keeps its files. Everything lives under `~/.glasscode` so a server
//! can mirror the same layout.

use std::path::PathBuf;
use std::sync::OnceLock;

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// A test run's own Glasscode folder (see [`use_test_root`]).
static TEST_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Tests: from now on [`root`] is a new temporary folder of this test process, so that a test never
/// reads or writes the person's `~/.glasscode` (a test run once gave its `accounts` folder a new
/// time). The core's own tests have it always; a test of another crate that reaches `root` calls
/// it first. Setting `GLASSCODE_HOME` instead would race with the tests that run beside it.
pub fn use_test_root() -> PathBuf {
    TEST_ROOT
        .get_or_init(|| {
            let dir = std::env::temp_dir().join(format!("glasscode-test-home-{}", std::process::id()));
            if let Err(err) = std::fs::create_dir_all(&dir) {
                panic!("could not make the tests' Glasscode folder {}: {err}", dir.display());
            }
            dir
        })
        .clone()
}

pub fn root() -> PathBuf {
    let set = std::env::var("GLASSCODE_HOME").ok().filter(|dir| !dir.is_empty()).map(PathBuf::from);
    if cfg!(test) {
        // The core's tests: one that sets a folder of its own (one at a time) gets it, every
        // other one the run's.
        return set.unwrap_or_else(use_test_root);
    }
    TEST_ROOT.get().cloned().or(set).unwrap_or_else(|| home().join(".glasscode"))
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

/// The Claude settings of each session on Windows (`claude --settings <file>`).
pub fn settings_dir() -> PathBuf {
    root().join("settings")
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

/// Copies of text the editor could not save or ask about when Glasscode quit (`save::keep_copy`).
pub fn unsaved_dir() -> PathBuf {
    root().join("unsaved")
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

#[cfg(all(test, windows))]
mod tests {
    // `.glasscode` goes in the user's profile folder (C:\Users\<name>), the home folder of Windows.
    #[test]
    fn home_is_the_user_profile_folder() {
        let profile = std::env::var_os("USERPROFILE").expect("Windows sets USERPROFILE");
        let home = super::home();
        assert!(home.as_os_str().eq_ignore_ascii_case(&profile), "{} is not {}", home.display(), profile.display());
    }
}

#[cfg(test)]
mod test_root {
    use super::*;

    /// A test never reaches the person's `~/.glasscode`: without a folder of its own it gets the
    /// run's temporary one, which is one folder for the whole run.
    #[test]
    fn a_test_gets_a_temporary_glasscode_folder() {
        let first = use_test_root();
        assert_eq!(use_test_root(), first);
        assert!(first.is_dir() && first.starts_with(std::env::temp_dir()), "{}", first.display());
        assert_ne!(first, home().join(".glasscode"));
        if std::env::var_os("GLASSCODE_HOME").is_none() {
            assert_eq!((root(), accounts_dir()), (first.clone(), first.join("accounts")));
        }
    }
}
