//! CLI versions Glasscode tracks and can install for you.
//!
//! Claude Code and Codex are the two built-in CLIs — always present, never removable.
//! For those two, Glasscode keeps a small pin file (`~/.glasscode/cli-versions.json`)
//! so a version you picked sticks across restarts, compares it to what is installed,
//! and offers to install it. On top of them you can add any other CLI — an npm package
//! (installed with `npm install -g`) or a fully custom one (your own install command and
//! a command that prints its version) — kept in `~/.glasscode/clis.json`.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::agents::{installed_version, resolve_program};
use crate::paths;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ApprovedVersions {
    #[serde(default)]
    pub claude: Option<String>,
    #[serde(default)]
    pub codex: Option<String>,
}

pub fn load_approved() -> ApprovedVersions {
    std::fs::read(paths::cli_versions_file())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Write the pinned versions (so a newer one the person picked sticks across restarts).
pub fn save_approved(approved: &ApprovedVersions) -> anyhow::Result<()> {
    paths::ensure_dirs()?;
    std::fs::write(paths::cli_versions_file(), serde_json::to_vec_pretty(approved)?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Custom CLIs the person added themselves.
// ---------------------------------------------------------------------------

/// How a custom CLI is installed and how its version is read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CliSource {
    /// An npm package installed with `npm install -g <package>`. `bin` is the
    /// command it puts on PATH, when that differs from the package's last path
    /// segment (e.g. `@anthropic-ai/claude-code` → `claude`).
    Npm {
        package: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bin: Option<String>,
    },
    /// A CLI with a hand-written install command and a command that prints its
    /// version (whatever the tool is — Homebrew, pipx, a curl installer, …).
    Command { install: String, version: String },
}

/// A CLI the person added on top of the two built-ins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomCli {
    pub name: String,
    pub source: CliSource,
}

pub fn load_clis() -> Vec<CustomCli> {
    std::fs::read(paths::clis_file())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_clis(clis: &[CustomCli]) -> anyhow::Result<()> {
    paths::ensure_dirs()?;
    std::fs::write(paths::clis_file(), serde_json::to_vec_pretty(clis)?)?;
    Ok(())
}

/// Add (or replace, by name) a custom CLI.
pub fn add_cli(cli: CustomCli) -> anyhow::Result<()> {
    let mut clis = load_clis();
    clis.retain(|c| c.name != cli.name);
    clis.push(cli);
    save_clis(&clis)
}

/// Remove a custom CLI by name. The installed binary itself is left alone.
pub fn remove_cli(name: &str) -> anyhow::Result<()> {
    let mut clis = load_clis();
    let before = clis.len();
    clis.retain(|c| c.name != name);
    if clis.len() == before {
        return Ok(());
    }
    save_clis(&clis)
}

/// The command an npm package puts on PATH, when not overridden: the last path
/// segment of the package name (`@scope/name` → `name`, `name` → `name`).
pub fn default_bin(package: &str) -> String {
    package.rsplit('/').next().unwrap_or(package).to_string()
}

// ---------------------------------------------------------------------------
// npm registry.
// ---------------------------------------------------------------------------

/// The npm package behind a built-in CLI, for reading its newest published version.
pub fn package(name: &str) -> &'static str {
    match name {
        "Claude Code" | "claude" => "@anthropic-ai/claude-code",
        _ => "@openai/codex",
    }
}

/// The newest version of an npm package published to npm (the `latest` dist-tag),
/// or `None` when offline.
pub fn latest_published(package: &str) -> Option<String> {
    let url = format!("https://registry.npmjs.org/{package}/latest");
    let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(8)).build();
    let body = agent.get(&url).call().ok()?.into_string().ok()?;
    let value: serde_json::Value = serde_json::from_str(&body).ok()?;
    value["version"].as_str().map(str::to_string)
}

/// Compare two dotted versions ("2.1.280" < "2.1.312").
pub fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| v.split(['.', '-']).map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parts(a) < parts(b)
}

/// Pull a version number out of arbitrary `--version` output: the first token
/// that looks like `X.Y` or `X.Y.Z` (a leading `v` is dropped). Many tools print
/// to stderr or wrap it in words ("gh version 2.62.0 (…)"), so pass both streams.
pub fn extract_version(text: &str) -> Option<String> {
    for raw in text.split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == ',') {
        let tok = raw.trim_start_matches('v');
        let mut parts = tok.split('.');
        let (Some(a), Some(b)) = (parts.next(), parts.next()) else {
            continue;
        };
        let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
        // X is all digits; Y starts with a digit (tolerates "0-beta" style suffixes).
        if digits(a) && b.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return Some(tok.to_string());
        }
    }
    None
}

/// The shell that runs a version command, and its arguments before the command. Unix keeps
/// `/bin/sh -c`: `env` is the login environment already, so a login shell would only be slower.
/// Windows has no `/bin/sh`, and the person writes the command for the default shell.
fn version_shell() -> (String, Vec<String>) {
    if cfg!(windows) {
        let shell = crate::sys::default_shell();
        (shell.program, shell.command_args)
    } else {
        ("/bin/sh".to_string(), vec!["-c".to_string()])
    }
}

/// Run a shell command that prints a version and read the version out of it.
/// Capped by a short timeout so a hung command cannot wedge the background check.
fn version_via_command(command: &str, env: &HashMap<String, String>) -> Option<String> {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();
    let command = command.to_string();
    let env = env.clone();
    std::thread::spawn(move || {
        let (shell, args) = version_shell();
        let output = crate::sys::command(shell).args(args).arg(&command).envs(&env).output();
        let _ = tx.send(output.ok());
    });
    let output = rx.recv_timeout(std::time::Duration::from_secs(6)).ok()??;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push(' ');
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    extract_version(&text)
}

// ---------------------------------------------------------------------------
// Status of one CLI (built-in or custom).
// ---------------------------------------------------------------------------

/// What a `CliStatus` describes, which decides how it installs and updates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliKind {
    /// Claude Code — npm, or its native installer when that is the copy on PATH.
    ClaudeCode,
    /// Codex — always npm.
    Codex,
    /// A custom npm package.
    Npm { package: String },
    /// A custom CLI with a hand-written install command.
    Command { install: String },
}

#[derive(Clone, Debug)]
pub struct CliStatus {
    pub name: String,
    pub installed: Option<String>,
    /// The pinned version (built-ins only); custom CLIs are never pinned.
    pub approved: Option<String>,
    /// The newest version published to npm (filled in the background; npm-backed only).
    pub latest: Option<String>,
    /// The binary actually first on PATH (the one sessions use), so an update
    /// targets it instead of landing in a copy PATH never reaches.
    pub path: Option<PathBuf>,
    /// Claude Code and Codex are built in and cannot be removed.
    pub builtin: bool,
    pub kind: CliKind,
}

impl CliStatus {
    /// The npm package this CLI installs from, if any (for reading the latest version).
    pub fn npm_package(&self) -> Option<String> {
        match &self.kind {
            CliKind::ClaudeCode => Some("@anthropic-ai/claude-code".to_string()),
            CliKind::Codex => Some("@openai/codex".to_string()),
            CliKind::Npm { package } => Some(package.clone()),
            CliKind::Command { .. } => None,
        }
    }

    /// Whether the Claude Code binary we would update is an npm global (as opposed
    /// to Claude's native installer location). Only meaningful for `ClaudeCode`.
    fn claude_is_npm(&self) -> bool {
        // On Windows the copy on PATH is npm's `claude.cmd` in its global folder (by default
        // `AppData\Roaming\npm`), not a file below `node_modules`; the native installer gives a
        // `claude.exe` (in `.local\bin`). A Unix path has no `.cmd`.
        self.path
            .as_ref()
            .map(|p| p.to_string_lossy().contains("node_modules") || p.extension().is_some_and(|e| e.eq_ignore_ascii_case("cmd")))
            .unwrap_or(false)
    }

    pub fn is_ok(&self) -> bool {
        match (&self.installed, &self.approved) {
            (Some(installed), Some(approved)) => installed == approved,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }

    pub fn summary(&self) -> String {
        match (&self.installed, &self.approved) {
            (None, _) => format!("{} not installed", self.name),
            (Some(v), Some(a)) if v != a => format!("{} {v} (pinned: {a})", self.name),
            (Some(v), Some(_)) => format!("{} {v} ✓ pinned", self.name),
            (Some(v), None) => format!("{} {v}", self.name),
        }
    }

    /// Shell command that installs a given version.
    pub fn install_command_for(&self, version: &str) -> String {
        match &self.kind {
            CliKind::ClaudeCode if self.claude_is_npm() => {
                format!("npm install -g @anthropic-ai/claude-code@{version}")
            }
            CliKind::ClaudeCode => format!("claude install {version}"),
            CliKind::Codex => format!("npm install -g @openai/codex@{version}"),
            CliKind::Npm { package } => format!("npm install -g {package}@{version}"),
            CliKind::Command { install } => install.clone(),
        }
    }

    /// Install the pinned version (built-ins only, when it differs from installed).
    pub fn install_command(&self) -> Option<String> {
        self.approved.as_ref().map(|v| self.install_command_for(v))
    }

    /// Install the newest published version (or just run the install command).
    pub fn install_latest_command(&self) -> String {
        match &self.kind {
            CliKind::ClaudeCode if self.claude_is_npm() => {
                "npm install -g @anthropic-ai/claude-code@latest".to_string()
            }
            CliKind::ClaudeCode => "claude install latest".to_string(),
            CliKind::Codex => "npm install -g @openai/codex@latest".to_string(),
            CliKind::Npm { package } => format!("npm install -g {package}@latest"),
            CliKind::Command { install } => install.clone(),
        }
    }

    /// A newer version than the one installed is available (npm-backed CLIs only).
    pub fn update_available(&self) -> bool {
        match (&self.latest, &self.installed) {
            (Some(latest), Some(installed)) => newer(installed, latest),
            (Some(_), None) => true,
            _ => false,
        }
    }
}

/// The program and arguments that run an install command in a terminal: the default shell runs
/// it, then waits for Enter so the person can read how it went before closing the terminal.
pub fn terminal_command(command: &str) -> (String, Vec<String>) {
    let shell = crate::sys::default_shell();
    // PowerShell (Windows) has no `echo;` for an empty line and no `read`.
    let end = if cfg!(windows) { "Write-Host ''; Write-Host 'Done. You can close this terminal.'; Read-Host" } else { "echo; echo 'Done. You can close this terminal.'; read" };
    let mut args = shell.command_args;
    args.push(format!("{command}; {end}"));
    (shell.program, args)
}

/// The status of a custom CLI: is it installed, and at what version.
fn custom_status(cli: &CustomCli, env: &HashMap<String, String>) -> CliStatus {
    match &cli.source {
        CliSource::Npm { package, bin } => {
            let bin = bin.clone().unwrap_or_else(|| default_bin(package));
            CliStatus {
                name: cli.name.clone(),
                installed: installed_version(&bin, env),
                approved: None,
                latest: None,
                path: resolve_program(&bin, env),
                builtin: false,
                kind: CliKind::Npm { package: package.clone() },
            }
        }
        CliSource::Command { install, version } => CliStatus {
            name: cli.name.clone(),
            installed: version_via_command(version, env),
            approved: None,
            latest: None,
            path: None,
            builtin: false,
            kind: CliKind::Command { install: install.clone() },
        },
    }
}

pub fn check(env: &HashMap<String, String>) -> Vec<CliStatus> {
    let approved = load_approved();
    let mut out = vec![
        CliStatus {
            name: "Claude Code".to_string(),
            installed: installed_version("claude", env),
            approved: approved.claude,
            latest: None,
            path: resolve_program("claude", env),
            builtin: true,
            kind: CliKind::ClaudeCode,
        },
        CliStatus {
            name: "Codex".to_string(),
            installed: installed_version("codex", env),
            approved: approved.codex,
            latest: None,
            path: resolve_program("codex", env),
            builtin: true,
            kind: CliKind::Codex,
        },
    ];
    out.extend(load_clis().iter().map(|cli| custom_status(cli, env)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude(installed: Option<&str>, latest: Option<&str>) -> CliStatus {
        CliStatus {
            name: "Claude Code".to_string(),
            installed: installed.map(str::to_string),
            approved: Some("2.1.280".into()),
            latest: latest.map(str::to_string),
            path: None,
            builtin: true,
            kind: CliKind::ClaudeCode,
        }
    }

    #[test]
    fn version_order_and_update() {
        assert!(newer("2.1.280", "2.1.312"));
        assert!(newer("2.1.9", "2.1.10"));
        assert!(!newer("2.1.312", "2.1.280"));
        assert!(!newer("2.1.280", "2.1.280"));
        assert!(claude(Some("2.1.280"), Some("2.1.312")).update_available());
        assert!(!claude(Some("2.1.312"), Some("2.1.312")).update_available());
        assert!(claude(None, Some("2.1.312")).update_available());
        assert!(!claude(Some("2.1.280"), None).update_available());
    }

    #[test]
    fn claude_native_vs_npm_install() {
        // No resolved path → native installer; an npm-global path → update that copy in place.
        assert_eq!(claude(Some("2.1.280"), Some("2.1.312")).install_latest_command(), "claude install latest");
        let npm = CliStatus {
            path: Some(PathBuf::from("/opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe")),
            ..claude(Some("2.1.280"), Some("2.1.312"))
        };
        assert_eq!(npm.install_latest_command(), "npm install -g @anthropic-ai/claude-code@latest");
        assert_eq!(npm.install_command_for("2.1.312"), "npm install -g @anthropic-ai/claude-code@2.1.312");
        assert_eq!(package("Claude Code"), "@anthropic-ai/claude-code");
    }

    #[test]
    fn custom_npm_install_commands() {
        let s = CliStatus {
            name: "ripgrep-cli".to_string(),
            installed: None,
            approved: None,
            latest: Some("14.1.0".into()),
            path: None,
            builtin: false,
            kind: CliKind::Npm { package: "@acme/rg".into() },
        };
        assert_eq!(s.install_latest_command(), "npm install -g @acme/rg@latest");
        assert_eq!(s.install_command_for("14.1.0"), "npm install -g @acme/rg@14.1.0");
        assert_eq!(s.npm_package().as_deref(), Some("@acme/rg"));
        assert!(!s.is_ok()); // not installed
        assert!(s.update_available()); // latest known, nothing installed
    }

    #[test]
    fn custom_command_install_ignores_version_arg() {
        let s = CliStatus {
            name: "mytool".to_string(),
            installed: Some("1.2.3".into()),
            approved: None,
            latest: None,
            path: None,
            builtin: false,
            kind: CliKind::Command { install: "brew install mytool".into() },
        };
        assert_eq!(s.install_latest_command(), "brew install mytool");
        assert_eq!(s.install_command_for("9.9.9"), "brew install mytool");
        assert_eq!(s.npm_package(), None); // no latest fetched for command CLIs
        assert!(s.is_ok()); // installed, never pinned
        assert!(!s.update_available());
    }

    #[test]
    fn default_bin_strips_scope() {
        assert_eq!(default_bin("@anthropic-ai/claude-code"), "claude-code");
        assert_eq!(default_bin("@openai/codex"), "codex");
        assert_eq!(default_bin("prettier"), "prettier");
    }

    #[test]
    fn extract_version_from_messy_output() {
        assert_eq!(extract_version("1.2.3").as_deref(), Some("1.2.3"));
        assert_eq!(extract_version("v2.1.0").as_deref(), Some("2.1.0"));
        assert_eq!(extract_version("gh version 2.62.0 (2024-11-12)").as_deref(), Some("2.62.0"));
        assert_eq!(extract_version("tool 3.4").as_deref(), Some("3.4"));
        assert_eq!(extract_version("no numbers here"), None);
        assert_eq!(extract_version(""), None);
    }

    // ----- Windows (S13) -----

    #[test]
    fn claude_is_npm_for_the_npm_folder_of_windows() {
        let at = |path: &str| CliStatus { path: Some(PathBuf::from(path)), ..claude(Some("2.1.280"), Some("2.1.312")) };
        let npm = at(r"C:\Users\dev\AppData\Roaming\npm\claude.cmd");
        assert!(npm.claude_is_npm());
        assert_eq!(npm.install_latest_command(), "npm install -g @anthropic-ai/claude-code@latest");
        let native = at(r"C:\Users\dev\.local\bin\claude.exe");
        assert!(!native.claude_is_npm());
        assert_eq!(native.install_latest_command(), "claude install latest");
        // The Mac as before: below `node_modules` is npm, `.local/bin` is the native installer.
        assert!(at("/opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js").claude_is_npm());
        assert!(!at("/Users/dev/.local/bin/claude").claude_is_npm());
    }

    #[test]
    fn version_via_command_reads_the_version_of_git() {
        let env: HashMap<String, String> = std::env::vars().collect();
        let version = version_via_command("git --version", &env).expect("git prints its version");
        assert!(version.starts_with(|c: char| c.is_ascii_digit()) && version.contains('.'), "{version}");
    }

    #[test]
    fn terminal_command_ends_with_the_done_text() {
        let shell = crate::sys::default_shell();
        let (program, args) = terminal_command("npm install -g @openai/codex@latest");
        assert_eq!(program, shell.program);
        let script = if cfg!(windows) {
            "npm install -g @openai/codex@latest; Write-Host ''; Write-Host 'Done. You can close this terminal.'; Read-Host"
        } else {
            "npm install -g @openai/codex@latest; echo; echo 'Done. You can close this terminal.'; read"
        };
        assert_eq!(args, shell.command_args.into_iter().chain([script.to_string()]).collect::<Vec<_>>());
    }

    #[test]
    fn terminal_command_runs_the_command_then_waits_for_enter() {
        use std::io::Write as _;
        use std::process::Stdio;

        let (program, args) = terminal_command("git --version");
        let mut child = crate::sys::command(program).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("the shell starts");
        // Enter, as the person presses it at the end.
        child.stdin.take().unwrap().write_all(b"\n").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(child.wait_with_output()));
        let output = rx.recv_timeout(std::time::Duration::from_secs(60)).expect("the shell stops after Enter").unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{output:?}");
        assert!(text.contains("git version"), "{text}");
        assert!(text.contains("Done. You can close this terminal."), "{text}");
    }

    /// A PATH of one folder, as the login environment gives it.
    #[cfg(windows)]
    fn env_with_path(dir: &std::path::Path) -> HashMap<String, String> {
        HashMap::from([("PATH".to_string(), dir.to_string_lossy().into_owned())])
    }

    #[cfg(windows)]
    #[test]
    fn installed_version_runs_the_cmd_files_of_npm() {
        let dir = tempfile::tempdir().unwrap();
        // npm puts a shell script without an extension next to each `.cmd`; Windows cannot start it.
        for (name, output) in [("claude", "2.1.312 (Claude Code)"), ("codex", "codex-cli 0.46.0")] {
            std::fs::write(dir.path().join(name), "#!/bin/sh\necho 9.9.9\n").unwrap();
            std::fs::write(dir.path().join(format!("{name}.cmd")), format!("@echo {output}\r\n")).unwrap();
        }
        let env = env_with_path(dir.path());
        assert_eq!(resolve_program("claude", &env), Some(dir.path().join("claude.cmd")));
        assert_eq!(installed_version("claude", &env).as_deref(), Some("2.1.312"));
        assert_eq!(installed_version("codex", &env).as_deref(), Some("0.46.0"));
        assert_eq!(installed_version("glasscode-no-such-cli", &env), None);
    }

    #[cfg(windows)]
    #[test]
    fn installed_version_runs_a_native_exe() {
        // The curl of Windows stands in for the `claude.exe` of the native installer: a program
        // that prints a version first and words after it ("curl 8.9.1 (Windows) libcurl/…").
        let curl = std::path::Path::new(&std::env::var_os("SystemRoot").unwrap()).join(r"System32\curl.exe");
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(&curl, dir.path().join("claude.exe")).unwrap();
        let env = env_with_path(dir.path());
        assert_eq!(resolve_program("claude", &env), Some(dir.path().join("claude.exe")));
        let output = crate::sys::command(&curl).arg("--version").output().unwrap();
        let expected = String::from_utf8_lossy(&output.stdout).split_whitespace().nth(1).map(str::to_string);
        assert!(expected.as_deref().is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit())), "{expected:?}");
        assert_eq!(installed_version("claude", &env), expected);
    }

    #[test]
    #[ignore = "needs Claude Code and Codex installed"]
    fn installed_versions_of_the_real_clis() {
        let env = crate::env::login_shell_env();
        for name in ["claude", "codex"] {
            let version = installed_version(name, &env);
            println!("{name}: {version:?}");
            assert!(version.is_some_and(|v| v.contains('.')), "{name}");
        }
    }

    #[test]
    #[ignore = "needs the network"]
    fn latest_published_reads_npm() {
        for package in [package("Claude Code"), package("Codex")] {
            let version = latest_published(package);
            println!("{package}: {version:?}");
            assert!(version.is_some_and(|v| v.contains('.')), "{package}");
        }
    }
}
