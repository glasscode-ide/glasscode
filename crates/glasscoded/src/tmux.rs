//! Sessions live in tmux on a dedicated socket, so they outlive SSH connections, the laptop
//! and the daemon itself (a daemon restart or update never stops an agent).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, bail};
use glasscode_core::remote::shell_quote;

/// Server options for Glasscode's tmux. Status bar off (the app shows its own header),
/// a prefix agents do not use (Claude Code uses Ctrl-B), no Escape delay, true color,
/// extended keys (Shift+Enter), and the newest client's size when the laptop and the phone
/// are attached at once.
const CONFIG: &str = "\
set -g default-terminal tmux-256color
set -as terminal-features ',*:RGB'
set -g extended-keys on
set -as terminal-features ',*:extkeys'
set -g escape-time 0
set -g focus-events on
set -g set-clipboard on
set -g history-limit 50000
set -g mouse on
set -g status off
set -g prefix C-]
unbind C-b
bind C-] send-prefix
set -g window-size latest
setw -g aggressive-resize on
set -g remain-on-exit off
";

#[derive(Clone)]
pub struct Tmux {
    /// `-L` socket name (`glasscode`; tests use their own).
    pub socket: String,
    pub config: PathBuf,
}

impl Tmux {
    pub fn new(root: &Path) -> Tmux {
        let socket = std::env::var("GLASSCODE_TMUX_SOCKET").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "glasscode".into());
        Tmux { socket, config: root.join("tmux.conf") }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new("tmux");
        cmd.args(["-L", &self.socket, "-f"]).arg(&self.config);
        // Never let a parent tmux (when started by hand inside one) capture our commands.
        cmd.env_remove("TMUX").env_remove("TMUX_PANE");
        cmd
    }

    fn run(&self, args: &[&str]) -> anyhow::Result<String> {
        let output = self.command().args(args).output().context("tmux is not installed")?;
        if !output.status.success() {
            bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub fn version() -> Option<String> {
        let output = Command::new("tmux").arg("-V").output().ok()?;
        output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    pub fn write_config(&self) -> anyhow::Result<()> {
        if std::fs::read_to_string(&self.config).ok().as_deref() != Some(CONFIG) {
            std::fs::write(&self.config, CONFIG)?;
            // A running server keeps its options; apply the file to it too.
            if self.server_running() {
                let _ = self.run(&["source-file", &self.config.to_string_lossy()]);
            }
        }
        Ok(())
    }

    fn server_running(&self) -> bool {
        self.command().args(["list-sessions"]).output().map(|o| o.status.success()).unwrap_or(false)
    }

    pub fn sessions(&self) -> Vec<String> {
        self.run(&["list-sessions", "-F", "#{session_name}"])
            .map(|out| out.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    pub fn has(&self, name: &str) -> bool {
        self.command().args(["has-session", "-t", &format!("={name}")]).output().map(|o| o.status.success()).unwrap_or(false)
    }

    /// Start `program args…` detached in a new session, with exactly `env` on top of the
    /// tmux server's environment.
    pub fn start(&self, name: &str, cwd: &Path, env: &HashMap<String, String>, program: &str, args: &[String]) -> anyhow::Result<()> {
        if self.has(name) {
            bail!("a session named {name} is already running");
        }
        let mut cmd = self.command();
        cmd.args(["new-session", "-d", "-s", name, "-x", "200", "-y", "50", "-c"]).arg(cwd);
        let mut keys: Vec<&String> = env.keys().collect();
        keys.sort();
        for key in keys {
            cmd.arg("-e").arg(format!("{key}={}", env[key]));
        }
        let line = std::iter::once(program.to_string()).chain(args.iter().cloned()).map(|a| shell_quote(&a)).collect::<Vec<_>>().join(" ");
        cmd.arg(format!("exec {line}"));
        let output = cmd.output().context("tmux is not installed")?;
        if !output.status.success() {
            bail!("tmux could not start the session: {}", String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(())
    }

    pub fn kill(&self, name: &str) {
        let _ = self.run(&["kill-session", "-t", &format!("={name}")]);
    }

    /// The last `lines` of the screen and scrollback, as text.
    pub fn capture(&self, name: &str, lines: u32) -> anyhow::Result<String> {
        self.run(&["capture-pane", "-p", "-J", "-t", &format!("={name}:"), "-S", &format!("-{lines}")])
    }

    /// What the pane shows right now (no scrollback): where an open question would be.
    pub fn screen(&self, name: &str) -> anyhow::Result<String> {
        self.run(&["capture-pane", "-p", "-J", "-t", &format!("={name}:")])
    }

    /// Type `text` into the session, then Enter when asked.
    pub fn send(&self, name: &str, text: &str, enter: bool) -> anyhow::Result<()> {
        let target = format!("={name}:");
        if !text.is_empty() {
            self.run(&["send-keys", "-t", &target, "-l", text])?;
        }
        if enter {
            // Agents read a burst of text and Enter as a paste; give the text a moment first.
            std::thread::sleep(std::time::Duration::from_millis(150));
            self.run(&["send-keys", "-t", &target, "Enter"])?;
        }
        Ok(())
    }

    /// Press keys by name (tmux key names: Up, Down, Enter, Escape, BTab, C-c).
    pub fn keys(&self, name: &str, keys: &[&str]) -> anyhow::Result<()> {
        let target = format!("={name}:");
        for key in keys {
            self.run(&["send-keys", "-t", &target, key])?;
            // Menus redraw between key presses; a short pause keeps them in step.
            std::thread::sleep(std::time::Duration::from_millis(60));
        }
        Ok(())
    }

    /// The command a client runs (through `ssh -tt`) to show the session.
    pub fn attach_argv(&self, name: &str) -> Vec<String> {
        vec!["tmux".into(), "-L".into(), self.socket.clone(), "attach-session".into(), "-t".into(), format!("={name}")]
    }
}

/// A tmux session name people can read in `tmux -L glasscode ls` (and on a phone): project,
/// branch and a short id, with the characters tmux reserves replaced.
pub fn session_name(project: &str, branch: &str, id: &str) -> String {
    let clean = |s: &str| -> String {
        let mapped: String = s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
        mapped.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
    };
    let short: String = id.chars().filter(|c| c.is_ascii_alphanumeric()).take(4).collect();
    let mut name = [clean(project), clean(branch)].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("-");
    name.truncate(40);
    format!("{}-{short}", name.trim_end_matches('-'))
}

#[cfg(test)]
mod tests {
    #[test]
    fn readable_session_names() {
        assert_eq!(super::session_name("myapp", "demo/auto-routing", "6edd7131-74bf"), "myapp-demo-auto-routing-6edd");
        assert_eq!(super::session_name("p", "", "abcd"), "p-abcd");
    }
}
