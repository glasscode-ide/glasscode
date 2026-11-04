//! Is this machine ready to keep sessions running while the laptop is closed?

use std::collections::HashMap;
use std::process::Command;

use glasscode_core::remote::{Check, CheckLevel};
use glasscode_core::versions;
use serde_json::Value;

use crate::tmux::Tmux;

fn check(name: &str, level: CheckLevel, detail: impl Into<String>, fix: Option<&str>) -> Check {
    Check { name: name.into(), level, detail: detail.into(), fix: fix.map(str::to_string) }
}

fn output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn run(env: &HashMap<String, String>) -> Vec<Check> {
    let mut checks = Vec::new();
    let linux = cfg!(target_os = "linux");

    match Tmux::version() {
        Some(v) => {
            let number: f32 = v.trim_start_matches("tmux ").trim_end_matches(|c: char| c.is_ascii_alphabetic()).parse().unwrap_or(0.0);
            if number >= 3.2 {
                checks.push(check("tmux", CheckLevel::Ok, v, None));
            } else {
                checks.push(check("tmux", CheckLevel::Fail, format!("{v}; 3.2 or newer is needed"), Some("sudo apt install -y tmux")));
            }
        }
        None => checks.push(check("tmux", CheckLevel::Fail, "not installed; sessions need it to survive disconnects", Some("sudo apt install -y tmux"))),
    }
    match output("git", &["--version"]) {
        Some(v) => checks.push(check("git", CheckLevel::Ok, v, None)),
        None => checks.push(check("git", CheckLevel::Fail, "not installed", Some("sudo apt install -y git"))),
    }

    for status in versions::check(env) {
        let level = match (&status.installed, status.is_ok()) {
            (None, _) => CheckLevel::Warn,
            (Some(_), true) => CheckLevel::Ok,
            (Some(_), false) => CheckLevel::Warn,
        };
        let fix = match &status.installed {
            // Claude's native installer bootstraps the binary `claude install` would need.
            None if status.name == "Claude Code" => Some("curl -fsSL https://claude.ai/install.sh | bash".to_string()),
            None => Some(status.install_latest_command()),
            Some(_) => status.install_command(),
        };
        let detail = status.summary().trim_start_matches(status.name.as_str()).trim().to_string();
        checks.push(Check { name: status.name.clone().into(), level, detail, fix: if level == CheckLevel::Ok { None } else { fix } });
    }

    // Pull request status (CI, approvals, preview, review verdict) on the iPhone comes from here.
    match glasscode_core::github::gh_ready(env) {
        Ok(()) => checks.push(check("GitHub CLI", CheckLevel::Ok, "signed in; pull request status is on", None)),
        Err(reason) => checks.push(check(
            "GitHub CLI",
            CheckLevel::Warn,
            format!("{reason}; the iPhone app shows no pull request status without it"),
            Some(if reason.contains("not installed") { "sudo apt install -y gh && gh auth login" } else { "gh auth login" }),
        )),
    }
    if crate::push::configured() {
        checks.push(check("iPhone notifications", CheckLevel::Ok, "Apple push key set up", None));
    }

    if linux {
        // Without linger, systemd stops the user's services (and tmux) after the last logout.
        let user = std::env::var("USER").unwrap_or_default();
        let linger = output("loginctl", &["show-user", &user, "-p", "Linger", "--value"]);
        match linger.as_deref() {
            Some("yes") => checks.push(check("Keep running after logout", CheckLevel::Ok, "linger is on", None)),
            _ => checks.push(check(
                "Keep running after logout",
                CheckLevel::Fail,
                "systemd linger is off, so sessions stop when the last SSH connection closes",
                Some("sudo loginctl enable-linger $USER"),
            )),
        }
        let service = output("systemctl", &["--user", "is-active", "glasscoded"]);
        match service.as_deref() {
            Some("active") => checks.push(check("glasscoded service", CheckLevel::Ok, "running under systemd; restarts on crash and boot", None)),
            _ => checks.push(check(
                "glasscoded service",
                CheckLevel::Warn,
                "not installed as a service; it does not start again after a reboot",
                Some("~/.glasscode/bin/glasscoded install"),
            )),
        }
        let sleep = output("systemctl", &["is-enabled", "sleep.target"]).or_else(|| {
            // `is-enabled` exits non-zero for "masked"; read it from stdout anyway.
            Command::new("systemctl").args(["is-enabled", "sleep.target"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        });
        if sleep.as_deref() == Some("masked") {
            checks.push(check("Never sleeps", CheckLevel::Ok, "sleep and suspend are disabled", None));
        } else {
            checks.push(check(
                "Never sleeps",
                CheckLevel::Warn,
                "the machine may suspend; running sessions pause while it sleeps",
                Some("sudo systemctl mask sleep.target suspend.target hibernate.target hybrid-sleep.target"),
            ));
        }
        let logind = std::fs::read_to_string("/etc/systemd/logind.conf").unwrap_or_default();
        let lid_ignored = logind.lines().any(|l| l.trim_start().starts_with("HandleLidSwitch=ignore"));
        if lid_ignored {
            checks.push(check("Lid can be closed", CheckLevel::Ok, "closing the lid does not suspend", None));
        } else {
            checks.push(check(
                "Lid can be closed",
                CheckLevel::Warn,
                "closing this machine's lid suspends it (laptop servers only)",
                Some("sudo sed -i 's/^#\\?HandleLidSwitch=.*/HandleLidSwitch=ignore/' /etc/systemd/logind.conf && sudo systemctl restart systemd-logind"),
            ));
        }
    }

    // Tailscale: reachable from anywhere, and its machine key must not quietly expire.
    let tailscale = output("tailscale", &["status", "--json"])
        .or_else(|| output("/Applications/Tailscale.app/Contents/MacOS/Tailscale", &["status", "--json"]));
    match tailscale.and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
        Some(status) => {
            let state = status["BackendState"].as_str().unwrap_or("");
            let name = status["Self"]["DNSName"].as_str().unwrap_or("").trim_end_matches('.').to_string();
            if state == "Running" {
                checks.push(check("Tailscale", CheckLevel::Ok, format!("connected as {name}"), None));
            } else {
                checks.push(check("Tailscale", CheckLevel::Fail, format!("not connected ({state})"), Some("sudo tailscale up")));
            }
            if let Some(expiry) = status["Self"]["KeyExpiry"].as_str() {
                checks.push(check(
                    "Tailscale key expiry",
                    CheckLevel::Warn,
                    format!("this machine's key expires {}; it then drops off the tailnet until someone signs it in", &expiry[..10.min(expiry.len())]),
                    Some("Tailscale admin console → Machines → this machine → Disable key expiry"),
                ));
            }
        }
        None => checks.push(check("Tailscale", CheckLevel::Warn, "not installed or not running (fine if the server is reachable another way)", Some("curl -fsSL https://tailscale.com/install.sh | sh && sudo tailscale up"))),
    }

    // Free disk space in the home folder.
    if let Some(df) = output("df", &["-Pk", &glasscode_core::paths::home().to_string_lossy()]) {
        if let Some(free_kb) = df.lines().nth(1).and_then(|l| l.split_whitespace().nth(3)).and_then(|v| v.parse::<u64>().ok()) {
            let gb = free_kb / 1024 / 1024;
            let level = if gb < 10 { CheckLevel::Warn } else { CheckLevel::Ok };
            checks.push(check("Disk space", level, format!("{gb} GB free"), None));
        }
    }
    checks
}
