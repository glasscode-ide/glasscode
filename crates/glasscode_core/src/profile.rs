//! Session profiles (M22; D11, D12, D19, D21, D27): what a session may touch, generated from the
//! path classes of `paths::layout()` and never from a path written here.
//!
//! Three origins:
//! - **person**: the person's own sessions. Claude runs in its sandbox with the credentials and
//!   Glasscode's own files denied, `gh` outside it (guarded); Codex runs as today.
//! - **automation**: sessions a playbook or the manager started, or sent text to, for their whole
//!   life (`gate::SessionOrigins`). The sandbox cannot be left, bypass is off, no cross-session
//!   messages, no claude.ai connectors, no `gh`, no web tools, and every tool call passes the
//!   `PreToolUse` gate hook (`glasscode hook gate`, [`gate_tool`]).
//! - **quarantine**: sessions sent to untrusted input (D27): as automation, with the network to the
//!   forge only (Claude) or off (Codex).
//!
//! Each limit names its plane (design section 7): the sandbox is enforced for shell commands on
//! macOS and Linux; the file-tool deny rules and the hooks are guarded. Windows has no sandbox,
//! so there the profile carries only the deny rules and the hooks.
//!
//! What runs here is pure except [`gate_tool`] and [`copy_claude_memory_once`], which the hook
//! process and a background thread call.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use crate::gate::SessionOrigin;
use crate::gate::{Level, ProjectPolicy};
use crate::model::AgentKind;
use crate::paths::{self, Classified, PathClass};

/// The git metadata of a session's worktree: the repository's `.git` (its common folder), the
/// worktree's own git folder (`<common>/worktrees/<name>`, or the common one for the main
/// checkout), and its branch.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFacts {
    pub common_dir: PathBuf,
    pub git_dir: PathBuf,
    pub branch: String,
}

impl GitFacts {
    /// The git metadata of `worktree`, from its `.git` (a folder, or a file naming the worktree's
    /// git folder) and the branch given. None when it has none. It reads one small file.
    pub fn of(worktree: &Path, branch: &str) -> Option<GitFacts> {
        let dot = worktree.join(".git");
        if dot.is_dir() {
            return Some(GitFacts { common_dir: dot.clone(), git_dir: dot, branch: branch.to_string() });
        }
        let text = std::fs::read_to_string(&dot).ok()?;
        let git_dir = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
        let git_dir = if git_dir.is_relative() { worktree.join(git_dir) } else { git_dir };
        let common_dir = std::fs::read_to_string(git_dir.join("commondir")).ok().map(|c| {
            let c = PathBuf::from(c.trim());
            if c.is_relative() { git_dir.join(c) } else { c }
        })?;
        Some(GitFacts { common_dir, git_dir, branch: branch.to_string() })
    }
}

/// The program the hooks run, by its absolute path, and the words before `hook` (the app's own
/// binary takes `cli`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookProgram {
    pub program: PathBuf,
    pub prefix: Vec<String>,
}

impl HookProgram {
    /// The command line of `glasscode hook <words>`, quoted for the shell the CLIs run hooks with.
    pub fn command(&self, words: &[&str]) -> String {
        let mut parts = vec![quote(&self.program.to_string_lossy())];
        parts.extend(self.prefix.iter().map(|p| quote(p)));
        parts.push("hook".into());
        parts.extend(words.iter().map(|w| quote(w)));
        parts.join(" ")
    }
}

/// Whether the program a hook would run is the one expected: its path is absolute, and its bytes
/// hash to `sha256` (hex) when one is given (crit-15). A launcher that was changed is not used.
pub fn hook_program_checked(program: &Path, sha256: Option<&str>) -> bool {
    if !program.is_absolute() {
        return false;
    }
    match sha256 {
        None => program.is_file(),
        Some(expected) => std::fs::read(program).is_ok_and(|bytes| {
            use sha2::{Digest, Sha256};
            let got: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
            got == expected
        }),
    }
}

fn quote(text: &str) -> String {
    crate::remote::shell_quote(text)
}

/// What the app knows of a session at its launch, for its profile.
#[derive(Clone, Debug, Default)]
pub struct ProfileFacts {
    pub worktree: PathBuf,
    /// The project's other worktrees: denied one by one to automation and quarantine sessions,
    /// never through their parent folder (P07).
    pub other_worktrees: Vec<PathBuf>,
    pub git: Option<GitFacts>,
    /// Its agent notes (`agent-notes/<key>/<agent>`), where Claude's auto-memory goes (D19).
    pub notes: Option<PathBuf>,
    /// The `.env` files of the worktree at launch: denied by path (a deny glob stops every new
    /// folder in Codex's workspace, P07), so one made later is not covered.
    pub env_files: Vec<PathBuf>,
    pub hook: Option<HookProgram>,
    /// The session's hook spool (`events/<id>.jsonl`): an argument of the hook commands, never
    /// an environment variable (crit-15).
    pub events: PathBuf,
    /// What `glasscode hook permission` answers from (`settings/<id>.permission.json`).
    pub permission_file: PathBuf,
    /// The package registries the project names.
    pub registries: Vec<String>,
    /// The forge's hosts.
    pub forge: Vec<String>,
    pub default_branch: Option<String>,
    /// `control.sock`: sessions may connect, nothing more.
    pub socket: PathBuf,
}

/// What `glasscode hook permission` answers from: the project's approved allow-commands, and the
/// commands it never allows. The app writes it at launch (a Hook IO file, which sessions cannot
/// read or write).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PermissionFile {
    pub allow: Vec<String>,
    pub never: Vec<String>,
}

/// A session's profile (design section 8).
#[derive(Clone, Debug, PartialEq)]
pub struct SessionProfile {
    pub origin: SessionOrigin,
    /// What goes into Claude's `--settings`, over Glasscode's own reporting settings.
    pub claude_settings: Value,
    pub claude_args: Vec<String>,
    pub codex_args: Vec<String>,
    pub env_set: Vec<(String, String)>,
    pub env_remove: Vec<String>,
    pub label: &'static str,
    pub permission: PermissionFile,
    /// Codex's folder is pre-trusted for this session (person only, trust-19).
    pub pretrust: bool,
    /// The sites a session the person does not drive may open, in a browser with no sign-ins
    /// (M23, the project's `browser_hosts`). Empty: no browser. The person's sessions have theirs.
    pub browser_hosts: Vec<String>,
}

/// The classes no session reads or writes (design section 5), with their files and folders.
const DENIED: [PathClass; 8] = [PathClass::Authority, PathClass::Words, PathClass::Ledger, PathClass::HookIo, PathClass::Accounts, PathClass::Credentials, PathClass::Cache, PathClass::BrainWork];
/// The classes sessions read but never write.
const READ_ONLY: [PathClass; 2] = [PathClass::TrustedMemory, PathClass::SessionTools];

/// The paths of the layout's rows of these classes, as this machine has them.
pub fn class_paths(classes: &[PathClass]) -> Vec<PathBuf> {
    paths::layout().iter().filter(|row| classes.contains(&row.class)).map(|row| row.path()).collect()
}

/// A permission rule's path: absolute rules start with `//` (one `/` is relative to the settings
/// file, P07), and a folder covers everything under it.
fn rule(tool: &str, path: &Path, folder: bool) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = if text.starts_with('/') { format!("/{text}") } else { format!("//{text}") };
    if folder { format!("{tool}({}/**)", text.trim_end_matches('/')) } else { format!("{tool}({text})") }
}

/// Whether a layout path is a folder: the rows without an extension are folders.
fn is_folder(path: &Path) -> bool {
    path.extension().is_none() || path.is_dir()
}

/// The profile of a session with this origin, for its agent, under its project's policy.
pub fn session_profile(origin: SessionOrigin, agent: AgentKind, policy: &ProjectPolicy, facts: &ProfileFacts) -> SessionProfile {
    let restricted = origin != SessionOrigin::Person;
    let mut denied = class_paths(&DENIED);
    if restricted {
        denied.extend(class_paths(&[PathClass::Handoff]));
        denied.extend(facts.other_worktrees.iter().cloned());
    }
    let read_only = class_paths(&READ_ONLY);
    let label = match origin {
        SessionOrigin::Person => "person",
        SessionOrigin::Automation => "automation",
        SessionOrigin::Quarantine => "quarantine",
    };
    let permission = PermissionFile { allow: policy.allow_commands.iter().filter(|c| is_simple_command(c)).cloned().collect(), never: never_commands() };
    let memories_off = policy.level == Level::Careful || policy.local_only;
    let claude_settings = claude_settings(origin, policy, facts, &denied, &read_only);
    let claude_args = if restricted { vec!["--strict-mcp-config".to_string()] } else { Vec::new() };
    let mut codex_args = Vec::new();
    if restricted {
        codex_args.extend(codex_permissions(origin, facts, &denied, &read_only));
        codex_args.extend(["--ask-for-approval".to_string(), "on-request".to_string()]);
    }
    // D19: Codex's own memories are off for Careful and local-only projects.
    if memories_off {
        codex_args.extend(["-c".to_string(), "features.memories=false".to_string()]);
    }
    let _ = agent;
    SessionProfile {
        origin,
        claude_settings,
        claude_args,
        codex_args,
        env_set: Vec::new(),
        // No session gets a GitHub token in its environment (trust-m4): the person's `gh` runs
        // outside the sandbox with its own login; automation's forge work is Glasscode's ops.
        env_remove: vec!["GH_TOKEN".into(), "GITHUB_TOKEN".into()],
        label,
        permission,
        pretrust: !restricted,
        browser_hosts: if restricted { policy.browser_hosts.clone() } else { Vec::new() },
    }
}

/// What Claude's `--settings` get from the profile.
fn claude_settings(origin: SessionOrigin, policy: &ProjectPolicy, facts: &ProfileFacts, denied: &[PathBuf], read_only: &[PathBuf]) -> Value {
    let restricted = origin != SessionOrigin::Person;
    let mut deny_read: Vec<PathBuf> = denied.to_vec();
    deny_read.extend(facts.env_files.iter().cloned());
    let mut deny = Vec::new();
    for path in &deny_read {
        let folder = is_folder(path) && !facts.env_files.contains(path);
        for tool in ["Read", "Edit", "Write"] {
            deny.push(rule(tool, path, folder));
        }
    }
    for path in read_only {
        for tool in ["Edit", "Write"] {
            deny.push(rule(tool, path, is_folder(path)));
        }
    }
    if restricted {
        deny.extend(["WebFetch".to_string(), "WebSearch".to_string()]);
    }
    // Approved allow-commands never prompt (D12, step 6): Claude's own matcher, which knows
    // compound commands, checks them.
    let allow: Vec<String> = policy.allow_commands.iter().filter(|c| is_simple_command(c)).flat_map(|c| [format!("Bash({c})"), format!("Bash({c}:*)")]).collect();
    let mut domains: Vec<String> = facts.forge.clone();
    if origin != SessionOrigin::Quarantine {
        domains.extend(facts.registries.iter().cloned());
    }
    let mut allow_write = vec![facts.worktree.to_string_lossy().into_owned()];
    if let Some(notes) = &facts.notes {
        allow_write.push(notes.to_string_lossy().into_owned());
    }
    let mut sandbox = json!({
        "enabled": true,
        "autoAllowBashIfSandboxed": true,
        "filesystem": { "denyRead": deny_read.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>(), "allowWrite": allow_write },
        "network": { "allowedDomains": domains, "allowUnixSockets": [facts.socket.to_string_lossy()] },
    });
    if restricted {
        sandbox["allowUnsandboxedCommands"] = json!(false);
        sandbox["failIfUnavailable"] = json!(true);
    } else {
        // `gh` cannot start inside the sandbox (`~/.config/gh` is a Credentials path, P07): the
        // person's runs outside it. Guarded, not a wall.
        sandbox["excludedCommands"] = json!(["gh"]);
    }
    let mut settings = json!({
        "sandbox": sandbox,
        "permissions": { "deny": deny, "allow": allow },
        // A project's own settings cannot turn Glasscode's hooks off (P08).
        "disableAllHooks": false,
    });
    if let Some(notes) = &facts.notes {
        // D19: Claude's auto-memory goes into the project's agent notes (P04: written without a prompt).
        settings["autoMemoryDirectory"] = json!(notes.to_string_lossy());
    }
    if restricted {
        settings["permissions"]["disableBypassPermissionsMode"] = json!("disable");
        settings["crossSessionInbound"] = json!("refuse");
        settings["disableClaudeAiConnectors"] = json!(true);
    }
    if let Some(hook) = &facts.hook {
        let events = facts.events.to_string_lossy().into_owned();
        let mut hooks = serde_json::Map::new();
        if restricted {
            let branch = facts.git.as_ref().map(|g| g.branch.clone()).unwrap_or_default();
            let default = facts.default_branch.clone().unwrap_or_default();
            let worktree = facts.worktree.to_string_lossy().into_owned();
            let mut words = vec!["gate", "--origin", origin_key(origin), "--events", &events, "--branch", &branch, "--default", &default, "--worktree", &worktree];
            // Its browser (M23): the project's sites only, which its server keeps it to.
            if !policy.browser_hosts.is_empty() {
                words.push("--browser");
            }
            let gate = hook.command(&words);
            hooks.insert("PreToolUse".into(), json!([{ "matcher": "*", "hooks": [{ "type": "command", "command": gate, "timeout": 10 }] }]));
        }
        let permission = hook.command(&["permission", "--events", &events, "--file", &facts.permission_file.to_string_lossy()]);
        hooks.insert("PermissionRequest".into(), json!([{ "matcher": "*", "hooks": [{ "type": "command", "command": permission, "timeout": 2 }] }]));
        settings["hooks"] = Value::Object(hooks);
    }
    settings
}

fn origin_key(origin: SessionOrigin) -> &'static str {
    match origin {
        SessionOrigin::Person => "person",
        SessionOrigin::Automation => "automation",
        SessionOrigin::Quarantine => "quarantine",
    }
}

/// An origin by its key.
pub fn origin_of(key: &str) -> Option<SessionOrigin> {
    match key {
        "person" => Some(SessionOrigin::Person),
        "automation" => Some(SessionOrigin::Automation),
        "quarantine" => Some(SessionOrigin::Quarantine),
        _ => None,
    }
}

/// Codex's permission profile for automation and quarantine (decision O16 on P07, narrowed): the
/// worktree and the agent notes writable; of the repository's git metadata only the worktree's
/// own git folder, the objects, and its own branch's ref with its lock and log (not `refs` or
/// `logs` as a whole, not `packed-refs`, `config` or `hooks`); the other worktrees denied one by
/// one; the denied classes and the launch's `.env` files denied by path; the trusted memory and
/// the session tools read-only; the network off, the control socket allowed.
fn codex_permissions(origin: SessionOrigin, facts: &ProfileFacts, denied: &[PathBuf], read_only: &[PathBuf]) -> Vec<String> {
    let mut fs: Vec<(String, &str)> = Vec::new();
    let mut put = |path: &Path, access: &'static str| {
        let key = path.to_string_lossy().into_owned();
        fs.retain(|(k, _)| *k != key);
        fs.push((key, access));
    };
    for path in denied {
        put(path, "deny");
    }
    for path in read_only {
        put(path, "read");
    }
    put(&facts.worktree, "write");
    if let Some(notes) = &facts.notes {
        put(notes, "write");
    }
    for env in &facts.env_files {
        put(env, "deny");
    }
    if let Some(git) = &facts.git
        && !git.branch.is_empty()
    {
        let heads = git.common_dir.join("refs").join("heads");
        let own = heads.join(&git.branch);
        let mut lock = own.clone().into_os_string();
        lock.push(".lock");
        put(&git.git_dir, "write");
        put(&git.common_dir.join("objects"), "write");
        put(&own, "write");
        put(Path::new(&lock), "write");
        put(&git.common_dir.join("logs").join("refs").join("heads").join(&git.branch), "write");
    }
    let table = |entries: &[(String, &str)]| format!("{{{}}}", entries.iter().map(|(k, v)| format!("{}={}", json!(k), json!(v))).collect::<Vec<_>>().join(", "));
    // Quarantine has no network; automation neither (Codex has no domain list): its forge work
    // is Glasscode's ops (D13), and its builds use what the worktree has.
    let _ = origin;
    let network = format!("{{\"enabled\"=false, \"unix_sockets\"={{{}=\"allow\"}}}}", json!(facts.socket.to_string_lossy()));
    ["-c".into(), r#"default_permissions="glasscode""#.into(), "-c".into(), r#"permissions.glasscode.extends=":workspace""#.into(), "-c".into(), format!("permissions.glasscode.filesystem={}", table(&fs)), "-c".into(), format!("permissions.glasscode.network={network}")].to_vec()
}

// ----- headless runs (M23) ----------------------------------------------------------------------

/// What a non-interactive run is for (design section 8). Each gets [`headless_profile`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadlessKind {
    BrainChat,
    BrainWake,
    AutoReview,
    Study,
    /// Memory's learning (M62): what the person said, and failures fixed.
    Reflect,
}

/// The one profile of every non-interactive run that reads outside text (alt-m1, trust-m2): no
/// tool that runs a command, no file, web or MCP tool, no slash commands, no session kept; Codex
/// read-only with the network off. The input comes on stdin as labelled data (`reads::as_data`),
/// and the answer is bound to `schema` (a JSON Schema): Claude's `--json-schema`, Codex's
/// `--output-schema <file>` with the last message in `answer`.
///
/// The manager's brain ([`HeadlessKind::BrainChat`], [`HeadlessKind::BrainWake`]) is the same wall
/// with one door, Glasscode's MCP server ([`brain_profile`]); its schema and files are not used.
pub fn headless_profile(kind: HeadlessKind, agent: AgentKind, schema: &Value, schema_file: &Path, answer_file: &Path) -> SessionProfile {
    if matches!(kind, HeadlessKind::BrainChat | HeadlessKind::BrainWake) {
        return brain_profile(kind == HeadlessKind::BrainWake);
    }
    let claude_args: Vec<String> = [
        "-p",
        "--restricted",
        "--tools",
        "",
        "--strict-mcp-config",
        "--permission-mode",
        "dontAsk",
        "--disable-slash-commands",
        "--no-session-persistence",
        "--output-format",
        "json",
        "--json-schema",
    ]
    .iter()
    .map(|a| a.to_string())
    .chain(std::iter::once(schema.to_string()))
    .collect();
    let codex_args: Vec<String> = ["exec", "--sandbox", "read-only", "--skip-git-repo-check", "--ephemeral", "--color", "never", "-c", "features.memories=false", "--output-schema"]
        .iter()
        .map(|a| a.to_string())
        .chain([schema_file.to_string_lossy().into_owned(), "-o".to_string(), answer_file.to_string_lossy().into_owned(), "-".to_string()])
        .collect();
    let _ = agent;
    SessionProfile {
        origin: SessionOrigin::Quarantine,
        claude_settings: json!({ "disableAllHooks": true }),
        claude_args,
        codex_args,
        env_set: Vec::new(),
        env_remove: vec!["GH_TOKEN".into(), "GITHUB_TOKEN".into()],
        label: "headless",
        permission: PermissionFile::default(),
        pretrust: false,
        browser_hosts: Vec::new(),
    }
}

/// The manager's brain (D4, P01, P02; M50): no shell and no built-in tool but Claude's tool search,
/// no MCP server but Glasscode's, its tools allowed and anything else refused unasked, no slash
/// command and no hook; Codex read-only with the network, shell, apps, plugins, agents, memories,
/// browser, computer use and the person's skills and config off. Both talk JSON lines: Claude
/// stream-json both ways, Codex `exec --json`. A wake keeps no session (`wake`) and runs no cron
/// of its own. What `runtime::claude::args` and `runtime::codex::args` add is per launch: the MCP
/// config, the settings file, the instructions, the model and the conversation.
pub fn brain_profile(wake: bool) -> SessionProfile {
    let mut claude_args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--restricted",
        "--tools",
        "ToolSearch",
        "--strict-mcp-config",
        "--allowedTools",
        "mcp__glasscode__*",
        "--permission-mode",
        "dontAsk",
        "--permission-prompts",
        "none",
        "--disable-slash-commands",
    ]
    .map(String::from)
    .to_vec();
    if wake {
        claude_args.push("--no-session-persistence".into());
    }
    let mut codex_args: Vec<String> = ["exec", "--json", "--skip-git-repo-check", "--color", "never", "-s", "read-only", "--ignore-user-config", "--ignore-rules"].map(String::from).to_vec();
    for feature in ["shell_tool", "apps", "plugins", "multi_agent", "memories", "browser_use", "computer_use"] {
        codex_args.extend(["--disable".to_string(), feature.to_string()]);
    }
    for setting in ["skills.include_instructions=false", "skills.bundled.enabled=false", "sandbox_workspace_write.network_access=false", "notify=[]", "include_permissions_instructions=false", "include_environment_context=false"] {
        codex_args.extend(["-c".to_string(), setting.to_string()]);
    }
    if wake {
        codex_args.push("--ephemeral".into());
    }
    let env_set = if wake { vec![("CLAUDE_CODE_DISABLE_CRON".to_string(), "1".to_string()), ("CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS".to_string(), "15000".to_string())] } else { Vec::new() };
    SessionProfile {
        origin: SessionOrigin::Quarantine,
        claude_settings: json!({ "disableAllHooks": true, "crossSessionInbound": "refuse" }),
        claude_args,
        codex_args,
        env_set,
        // The forge is Glasscode's ops: the brain holds no token of its own (D13).
        env_remove: vec!["GH_TOKEN".into(), "GITHUB_TOKEN".into()],
        label: if wake { "brain-wake" } else { "brain-chat" },
        permission: PermissionFile::default(),
        pretrust: false,
        browser_hosts: Vec::new(),
    }
}

// ----- pushes from sessions the manager drives (M23, D13, P07) -----------------------------------

/// The commits a session made, by its turn reports in the journal (`turn.ended`, M30): each
/// report's commits are `<short sha> <subject>`. It reads the journal: never on the main thread.
pub fn session_commits(session: &str) -> Vec<String> {
    let filter = crate::journal::Filter { session: Some(session.to_string()), kind: Some("turn.ended".to_string()), ..Default::default() };
    crate::journal::query(&filter, &crate::journal::Scope::All, 100_000)
        .iter()
        .flat_map(|event| event.data["commits"].as_array().cloned().unwrap_or_default())
        .filter_map(|line| line.as_str().and_then(|l| l.split_whitespace().next()).map(str::to_string))
        .collect()
}

/// A push from a session the manager drives sends only that session's own commits on its own
/// branch (D13, P07: a Claude session can move any local branch tip, so a tip it found is never
/// trusted): every commit the push would send (`pushed`, full ids) must be one of the commits its
/// turns made (`made`, short ids). The refusal names the others.
pub fn own_commits_only(pushed: &[String], made: &[String]) -> Result<(), String> {
    let made: Vec<&str> = made.iter().map(String::as_str).filter(|m| m.len() >= 7).collect();
    let foreign: Vec<&str> = pushed.iter().map(String::as_str).filter(|sha| !made.iter().any(|m| sha.starts_with(m))).collect();
    if foreign.is_empty() {
        return Ok(());
    }
    let shown: Vec<String> = foreign.iter().take(5).map(|s| s.chars().take(9).collect()).collect();
    Err(format!("the branch holds {} commit{} this session did not make ({}), so nothing was pushed", foreign.len(), if foreign.len() == 1 { "" } else { "s" }, shown.join(", ")))
}

// ----- the PreToolUse gate (`glasscode hook gate`) ----------------------------------------------

/// What the gate hook knows of the session it guards (its command line's arguments).
#[derive(Clone, Debug, Default)]
pub struct GateCtx {
    pub origin: Option<SessionOrigin>,
    pub branch: String,
    pub default_branch: String,
    pub worktree: PathBuf,
    /// The session has the throwaway browser of its project's sites (M23).
    pub browser: bool,
}

/// The `PreToolUse` gate of automation and quarantine sessions (alt-1): Rust decides on the tool's
/// name and input. Err is the refusal, which the hook prints and exits 2 with. Guarded: it stops
/// the usual case; the sandbox, the deny rules and the forge's rules are the walls.
pub fn gate_tool(ctx: &GateCtx, tool: &str, input: &Value) -> Result<(), String> {
    if ctx.origin.is_none_or(|o| o == SessionOrigin::Person) {
        return Ok(());
    }
    let text = |key: &str| input.get(key).and_then(Value::as_str).unwrap_or_default();
    match tool {
        "Bash" => gate_command(ctx, text("command")),
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "Glob" | "Grep" | "LS" => {
            let writes = matches!(tool, "Edit" | "Write" | "MultiEdit" | "NotebookEdit");
            for key in ["file_path", "notebook_path", "path"] {
                let path = text(key);
                if !path.is_empty() {
                    gate_path(ctx, &expand(path, &ctx.worktree), writes)?;
                }
            }
            Ok(())
        }
        // Text to other sessions, and the list of them, cross the project boundary (P16).
        "SendMessage" | "ListAgents" => Err("a session the manager drives does not message other sessions".into()),
        "WebFetch" | "WebSearch" => Err("a session the manager drives does not fetch from the web".into()),
        // Its browser of the project's sites, whose server opens nothing else (M23).
        t if ctx.browser && t.starts_with("mcp__glasscode-browser__") => Ok(()),
        t if t.starts_with("mcp__") && !t.starts_with("mcp__glasscode__") => Err(format!("a session the manager drives has no {t}")),
        _ => Ok(()),
    }
}

fn expand(path: &str, cwd: &Path) -> PathBuf {
    let home = paths::home();
    let path = if path == "~" {
        home
    } else if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("$HOME/")) {
        home.join(rest)
    } else {
        PathBuf::from(path)
    };
    if path.is_relative() { cwd.join(path) } else { path }
}

/// A path a tool or a command touches: refused when sessions may not read it (or not write it).
fn gate_path(ctx: &GateCtx, path: &Path, writes: bool) -> Result<(), String> {
    let _ = ctx;
    match paths::class_of(path) {
        Classified::Class(class) if DENIED.contains(&class) => Err(format!("{} is Glasscode's or a credential: sessions do not read it", path.display())),
        Classified::Class(PathClass::Handoff) => Err(format!("{} holds other work's handoffs", path.display())),
        Classified::Class(class) if writes && READ_ONLY.contains(&class) => Err(format!("{} is read-only for sessions", path.display())),
        Classified::InsideUnlisted => Err(format!("{} is Glasscode's own: sessions do not touch it", path.display())),
        _ => Ok(()),
    }
}

/// The words of a shell command, split at white space and at the shell's operators.
fn words(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    for c in command.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => quote = Some(c),
            (None, c) if c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '<' | '>' | '`') => {
                if !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
                if !c.is_whitespace() {
                    out.push(c.to_string());
                }
            }
            (None, c) => word.push(c),
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// The commands of a line: its words between the operators.
fn commands(command: &str) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = vec![Vec::new()];
    for word in words(command) {
        if matches!(word.as_str(), ";" | "&" | "|" | "(" | ")" | "`") {
            out.push(Vec::new());
        } else if let Some(last) = out.last_mut() {
            last.push(word);
        }
    }
    out.into_iter()
        .map(|c| c.into_iter().skip_while(|w| w.contains('=') && !w.starts_with('-') || matches!(w.as_str(), "sudo" | "env" | "command" | "exec" | "nohup" | "time")).collect::<Vec<_>>())
        .filter(|c| !c.is_empty())
        .collect()
}

fn program(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

fn gate_command(ctx: &GateCtx, command: &str) -> Result<(), String> {
    for words in commands(command) {
        let prog = program(&words[0]);
        let rest: Vec<&str> = words[1..].iter().map(String::as_str).collect();
        match prog {
            "gh" => {
                if rest.windows(2).any(|w| w == ["pr", "merge"]) || rest.iter().any(|w| w.contains("/merge")) {
                    return Err("merging is the person's: a session never merges (the forge's rules decide)".into());
                }
                // M61: ready for review only after the independent check.
                if rest.windows(2).any(|w| w == ["pr", "ready"]) || rest.iter().any(|w| w.contains("markPullRequestReadyForReview") || w.contains("ready_for_review")) {
                    return Err("marking a pull request ready is `glasscode pr ready`: an independent check reads it first".into());
                }
                if rest.windows(2).any(|w| w == ["pr", "create"]) && !rest.iter().any(|w| *w == "--draft" || *w == "-d") {
                    return Err("a session opens a draft (`glasscode pr open-draft`, or `gh pr create --draft`); `glasscode pr ready` marks it ready after the check".into());
                }
            }
            "git" => {
                if let Some(at) = rest.iter().position(|w| *w == "push") {
                    let push = &rest[at + 1..];
                    if push.iter().any(|w| matches!(*w, "--force" | "-f" | "--force-with-lease" | "--force-if-includes" | "--mirror" | "--delete" | "-d" | "--all") || w.starts_with("--force-with-lease=") || (w.starts_with('+') && w.len() > 1)) {
                        return Err("a session never force-pushes or deletes a remote branch".into());
                    }
                    let defaults: Vec<&str> = if ctx.default_branch.is_empty() { vec!["main", "master"] } else { vec![ctx.default_branch.as_str()] };
                    let to_default = push.iter().filter(|w| !w.starts_with('-')).any(|w| {
                        let target = w.rsplit(':').next().unwrap_or(w).trim_start_matches("refs/heads/");
                        defaults.contains(&target)
                    });
                    if to_default {
                        return Err("a session never pushes to the default branch".into());
                    }
                }
            }
            // Another agent started from a session's shell is outside its profile and its project.
            "claude" | "codex" => return Err("a session the manager drives does not start other agents".into()),
            _ => {}
        }
        for word in &rest {
            let looks_like_path = word.starts_with('/') || word.starts_with("~/") || word.starts_with("$HOME") || word.starts_with("./") || word.starts_with("../") || word.contains('/');
            if looks_like_path && !word.starts_with('-') {
                gate_path(ctx, &expand(word, &ctx.worktree), false)?;
            }
        }
    }
    Ok(())
}

// ----- the PermissionRequest hook (`glasscode hook permission`) ---------------------------------

/// What a `PermissionRequest` hook answers (D12): allow, deny, or nothing (the CLI asks the person
/// as before). It never returns `updatedPermissions`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionAnswer {
    Allow,
    Deny(String),
    Nothing,
}

impl PermissionAnswer {
    /// The hook's stdout: the decision, or nothing at all.
    pub fn output(&self) -> Option<String> {
        let decision = match self {
            PermissionAnswer::Allow => json!({ "behavior": "allow" }),
            PermissionAnswer::Deny(why) => json!({ "behavior": "deny", "message": why }),
            PermissionAnswer::Nothing => return None,
        };
        Some(json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision } }).to_string())
    }
}

/// Commands no permission hook ever allows, whatever the allow-commands say.
fn never_commands() -> Vec<String> {
    ["gh pr merge", "git push --force", "git push -f"].map(String::from).to_vec()
}

/// Whether a command line is one simple command: no operator, substitution or redirection.
pub fn is_simple_command(command: &str) -> bool {
    let c = command.trim();
    !c.is_empty() && !c.contains(['&', '|', ';', '>', '<', '`', '\n']) && !c.contains("$(") && !c.contains("${")
}

/// The answer to a permission prompt: allowed when it is one simple command that starts with an
/// approved allow-command (by whole words), denied when it is a command never allowed, else
/// nothing. A command with an operator, a substitution or a redirection is declined (nothing).
pub fn permission_answer(tool: &str, input: &Value, file: &PermissionFile) -> PermissionAnswer {
    if tool != "Bash" {
        return PermissionAnswer::Nothing;
    }
    let command = input.get("command").and_then(Value::as_str).unwrap_or_default().trim();
    if !is_simple_command(command) {
        return PermissionAnswer::Nothing;
    }
    let starts = |prefix: &str| command == prefix || command.starts_with(&format!("{prefix} "));
    if let Some(never) = file.never.iter().find(|n| starts(n)) {
        return PermissionAnswer::Deny(format!("“{never}” is never allowed from a session"));
    }
    if file.allow.iter().any(|a| starts(a)) { PermissionAnswer::Allow } else { PermissionAnswer::Nothing }
}

// ----- the app's own git calls ------------------------------------------------------------------

/// What the app's own git calls on worktrees pass with the v2 engine (trust-m7): no hooks, no
/// fsmonitor, plain ssh, whatever the worktree's configuration (which a session can write) says.
pub const GIT_OVERRIDES: [&str; 6] = ["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=", "-c", "core.sshCommand=ssh"];

// ----- Claude's auto-memory (D19) ---------------------------------------------------------------

/// Claude's own memory folder of a repository: the vendor's layout, in `agents`.
pub use crate::agents::claude_memory_dir;

/// Copy Claude's existing memory of a repository into the agent notes, once: only when the notes
/// have none yet; the old folder is never moved or changed. It reads and writes the disk: never
/// on the main thread. The number of files copied.
pub fn copy_claude_memory_once(from: &Path, notes: &Path) -> std::io::Result<usize> {
    if notes.join("MEMORY.md").exists() || !from.is_dir() {
        return Ok(0);
    }
    std::fs::create_dir_all(notes)?;
    let mut copied = 0;
    for entry in std::fs::read_dir(from)?.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|e| e == "md") {
            let to = notes.join(entry.file_name());
            if !to.exists() {
                std::fs::copy(&path, &to)?;
                copied += 1;
            }
        }
    }
    Ok(copied)
}

// ----- Codex's trust of Glasscode's own hooks (D11, P06) ------------------------------------------

/// The `config.toml` of an account with a `hooks.state` trust entry for each of Glasscode's own
/// hooks (key and hash as `codex app-server` `hooks/list` reports them), replacing an older entry
/// for the same key and keeping everything else. Codex then runs those hooks without the bypass
/// flag, and a repository's hooks stay untrusted.
pub fn with_hook_trust(config: &str, entries: &[(String, String)]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut skip = false;
    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            skip = entries.iter().any(|(key, _)| trimmed == format!("[hooks.state.{}]", json!(key)));
        }
        if !skip {
            out.push(line.to_string());
        }
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    let mut text = out.join("\n");
    for (key, hash) in entries {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&format!("[hooks.state.{}]\ntrusted_hash = {}", json!(key), json!(hash)));
    }
    text.push('\n');
    text
}

#[cfg(test)]
mod tests;
