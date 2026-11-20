//! Smaller choices from Settings that have no better home (in `state.json`).

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Prefs {
    /// The editor "Open in editor" and file links use: "zed", "code" or "cursor"; none: the
    /// first of Cursor, VS Code and Zed that is installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor: Option<String>,
    /// Off: closing a session whose agent is working does not ask first.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub confirm_close_off: bool,
    /// How many times in a row Glasscode asks an agent to fix CI or conflicts (3 unless chosen).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix_tries: Option<u32>,
    /// How long it waits for CI's other checks once one failed, in minutes (10 unless chosen).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_wait_minutes: Option<u32>,
    /// Glasscode's release channel: "stable" (unless chosen) or "beta".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_channel: Option<String>,
    /// A downloaded update waits for "Restart to update" instead of going in at quit.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub install_on_quit_off: bool,
    /// No notification when CI fails (or conflicts appear) on one of the person's PRs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub notify_ci_off: bool,
    /// Removing one worktree does not tick "also delete its branch" by itself (it does when
    /// every commit of the branch is elsewhere, unless this is on).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep_branch_on_remove: bool,
    /// New work branches from the default branch as it is on this Mac, without fetching it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fresh_main_off: bool,
    /// New sessions start on the server while it is connected: the last session was started
    /// there (the new-session dialog remembers where the person ran the last one).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub new_on_server: bool,
    /// Reach Glasscode from anywhere: with a self-hosted relay configured, this Mac stays
    /// reachable through it so the phone app works off your LAN. Off: Glasscode only connects on
    /// the same network (direct LAN or Tailscale).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub glasscode_relay_off: bool,
}

impl Prefs {
    /// Where a new session starts: on the server only when the person last chose it there and
    /// it is connected now. Otherwise this Mac, and the choice stays as it is.
    pub fn starts_on_server(&self, connected: bool) -> bool {
        self.new_on_server && connected
    }

    /// The person picked where to run (a "Run on" card): the next new session starts there too.
    /// True when that changed what is remembered.
    pub fn choose_run_on(&mut self, on_server: bool) -> bool {
        let changed = self.new_on_server != on_server;
        self.new_on_server = on_server;
        changed
    }

    pub fn update_channel(&self) -> &str {
        match self.update_channel.as_deref() {
            Some("beta") => "beta",
            _ => "stable",
        }
    }

    pub fn fix_tries(&self) -> u32 {
        self.fix_tries.filter(|n| (1..=10).contains(n)).unwrap_or(crate::autofix::MAX_TRIES)
    }

    pub fn ci_wait(&self) -> std::time::Duration {
        self.ci_wait_minutes.filter(|m| (1..=120).contains(m)).map(|m| std::time::Duration::from_secs(u64::from(m) * 60)).unwrap_or(crate::autofix::WAIT_FOR_CI)
    }
}

/// The editors Settings offers: (id, name, its command). The command is looked for on PATH
/// (`sys::find_program`); on Windows, VS Code and Cursor put `code.cmd` and `cursor.cmd` there.
pub const EDITORS: [(&str, &str, &str); 3] = [("zed", "Zed", "zed"), ("code", "VS Code", "code"), ("cursor", "Cursor", "cursor")];

#[cfg(test)]
mod run_on_tests {
    #[test]
    fn a_new_session_starts_where_the_person_last_chose() {
        use super::Prefs;
        let mut prefs = Prefs::default();
        // Unset: never the server, connected or not.
        assert!(!prefs.starts_on_server(true));
        assert!(!prefs.starts_on_server(false));
        // Chosen: the server while it is connected; this Mac when it is not, and the choice stays.
        assert!(prefs.choose_run_on(true));
        assert!(prefs.starts_on_server(true));
        assert!(!prefs.starts_on_server(false));
        assert!(prefs.new_on_server, "falling back to the Mac keeps the choice");
        assert!(!prefs.choose_run_on(true), "the same choice changes nothing");
        assert!(prefs.choose_run_on(false));
        assert!(!prefs.starts_on_server(true));
    }

    #[test]
    fn where_to_run_is_saved_only_when_on() {
        use super::Prefs;
        let off: Prefs = serde_json::from_str("{}").unwrap();
        assert!(!off.new_on_server, "older files load as off");
        assert!(!serde_json::to_string(&off).unwrap().contains("new_on_server"), "off is not written");
        let mut on = Prefs::default();
        on.choose_run_on(true);
        let back: Prefs = serde_json::from_str(&serde_json::to_string(&on).unwrap()).unwrap();
        assert!(back.new_on_server);
    }
}

#[cfg(all(test, windows))]
mod tests {

    use super::*;

    /// The editor gets the worktree and the file as two arguments, as "Open in editor" gives them,
    /// even with spaces in the paths and in the folder of the editor (C:\Program Files\…).
    #[test]
    fn each_editor_starts_from_its_cmd_file_with_paths_that_have_spaces() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("my editors");
        std::fs::create_dir(&bin).unwrap();
        let worktree = dir.path().join("my project");
        let file = worktree.join("src").join("a b.rs");
        for (_, name, program) in EDITORS {
            // A stand-in that writes down what it got, one argument on a line.
            let script = format!("@echo off\r\n>\"%~dp0{program}.txt\" echo(%~1\r\n>>\"%~dp0{program}.txt\" echo(%~2\r\n");
            std::fs::write(bin.join(format!("{program}.cmd")), script).unwrap();
            let found = crate::sys::find_program(program, &bin).unwrap_or_else(|| panic!("{name} is not found"));
            assert_eq!(found, bin.join(format!("{program}.cmd")));
            let status = crate::sys::command(&found).arg(&worktree).arg(&file).status().unwrap_or_else(|e| panic!("{name} does not start: {e}"));
            assert!(status.success(), "{name}: {status}");
            let got = std::fs::read_to_string(bin.join(format!("{program}.txt"))).unwrap();
            assert_eq!(got.lines().collect::<Vec<_>>(), [worktree.to_str().unwrap(), file.to_str().unwrap()], "{name}");
        }
    }
}
