//! glasscoded — Glasscode's server daemon.
//!
//! Runs on a developer's always-on machine (usually Linux, reached over Tailscale) and keeps
//! their Claude Code and Codex sessions running while the laptop is closed. The Mac app (and
//! later a phone app) talks to it through `ssh <server> glasscoded rpc`.

mod daemon;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use glasscode_core::paths;
use glasscode_core::remote::{CheckLevel, Request, Response, ServerState};
use serde_json::Value;

const HELP: &str = "\
glasscoded — keeps Glasscode's Claude and Codex sessions running on this machine

Usage:
  glasscoded install        install to ~/.glasscode/bin and run as a systemd user service
  glasscoded uninstall      remove the service (running sessions stay in tmux)
  glasscoded serve          run the daemon in the foreground (what the service runs)
  glasscoded status         list sessions
  glasscoded attach <name>  open a session's terminal here (detach: Ctrl-] then d)
  glasscoded doctor         check that this machine is ready to keep sessions running
  glasscoded rpc            bridge stdin/stdout to the daemon (used by the Mac app over ssh)
  glasscoded gate           what a paired phone's SSH key runs: only rpc and attach
  glasscoded relay <url> <secret>  reach this server through a self-hosted relay (see relay/README.md)
  glasscoded relay off      stop using the relay (reach the server over SSH or Tailscale)
  glasscoded relay status   whether the relay is configured and connected
  glasscoded update         install a newer release now (it also checks by itself every few hours)
  glasscoded version
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The `glasscode` command for sessions here (~/.local/bin/glasscode runs this).
    if args.first().map(String::as_str) == Some("cli") {
        std::process::exit(glasscode_core::cli::main(args[1..].to_vec()));
    }
    let result = match args.first().map(String::as_str) {
        Some("serve") => daemon::serve(),
        Some("rpc") => rpc(),
        Some("gate") => gate(),
        Some("relay") => relay_cmd(&args[1..]),
        Some("update") => update::command(),
        Some("install") => install::install(),
        Some("uninstall") => install::uninstall(),
        Some("status") => status(),
        Some("attach") => attach(args.get(1).map(String::as_str)),
        Some("doctor") => doctor(),
        Some("version" | "--version" | "-V") => {
            println!("glasscoded {} (protocol {})", glasscode_core::update::describe(env!("CARGO_PKG_VERSION"), glasscode_core::update::BUILD), glasscode_core::remote::PROTOCOL_VERSION);
            Ok(())
        }
        _ => {
            print!("{HELP}");
            Ok(())
        }
    };
    if let Err(err) = result {
        eprintln!("glasscoded: {err:#}");
        std::process::exit(1);
    }
}

/// Connect to the daemon, starting it in the background when it is not running (for
/// machines where `install` has not been run yet).
fn connect() -> anyhow::Result<UnixStream> {
    let socket = paths::daemon_socket();
    if let Ok(stream) = UnixStream::connect(&socket) {
        return Ok(stream);
    }
    paths::ensure_dirs()?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(paths::root().join("glasscoded.log"))?;
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("serve").stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    // Its own session, so it is not tied to this SSH connection.
    // SAFETY: setsid is async-signal-safe and only affects the child.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    command.spawn().context("could not start the daemon")?;
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(stream) = UnixStream::connect(&socket) {
            return Ok(stream);
        }
    }
    bail!("the daemon did not start; see {}", paths::root().join("glasscoded.log").display())
}

/// Copy everything that arrives, as it arrives. (`std::io::copy` would use `splice` here,
/// which holds request lines back until the other side closes.)
fn pump(mut from: impl std::io::Read, mut to: impl Write) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if to.write_all(&buf[..n]).and_then(|_| to.flush()).is_err() {
                    break;
                }
            }
        }
    }
}

fn rpc() -> anyhow::Result<()> {
    let stream = connect()?;
    let from_daemon = stream.try_clone()?;
    let replies = std::thread::spawn(move || pump(from_daemon, std::io::stdout()));
    pump(std::io::stdin(), &stream);
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = replies.join();
    Ok(())
}

/// Run by sshd for a phone's key (`command="… gate"` in authorized_keys): the phone may talk
/// to the daemon and open a session's terminal, nothing else.
fn gate() -> anyhow::Result<()> {
    let original = std::env::var("SSH_ORIGINAL_COMMAND").unwrap_or_default();
    let words: Vec<&str> = original.split_whitespace().collect();
    let words = match words.first() {
        Some(first) if first.ends_with("glasscoded") => &words[1..],
        _ => &words[..],
    };
    match words {
        ["rpc"] => rpc(),
        ["attach", id] if id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') => attach(Some(id)),
        _ => bail!("this key can only use Glasscode (rpc, attach <session>)"),
    }
}

fn request(method: &str, params: Value) -> anyhow::Result<Value> {
    let mut stream = connect()?;
    let mut line = serde_json::to_string(&Request { id: 1, method: method.into(), params })?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut reader = BufReader::new(stream);
    let mut answer = String::new();
    reader.read_line(&mut answer)?;
    let response: Response = serde_json::from_str(&answer)?;
    match response.error {
        Some(err) => bail!("{err}"),
        None => Ok(response.result.unwrap_or(Value::Null)),
    }
}

fn relay_cmd(args: &[String]) -> anyhow::Result<()> {
    match args.first().map(String::as_str) {
        Some("off") => {
            if relay_link::disable()? {
                println!("Relay turned off; devices now reach this server over SSH or Tailscale. The daemon stops its listener within a few seconds.");
            } else {
                println!("No relay was configured.");
            }
            Ok(())
        }
        Some("status") | None => {
            match request("relay.status", Value::Null).ok().and_then(|v| serde_json::from_value::<Option<relay_link::RelayStatus>>(v).ok()).flatten() {
                Some(s) if s.signed_in => {
                    let link = if s.connected { "connected" } else { "not connected" };
                    println!("Relay: configured, {link}, {} device(s) online", s.online.len());
                    if let Some(error) = s.error {
                        println!("  {error}");
                    }
                }
                _ => println!("Relay: not configured. Set one up with: glasscoded relay <url> <secret>"),
            }
            Ok(())
        }
        Some(url) => {
            let secret = args.get(1).map(String::as_str).unwrap_or("");
            let config = relay_link::configure(url, secret)?;
            println!("Relay set: {}", config.url);
            println!("This machine's device id: {}", config.device_id);
            println!();
            println!("Point the phone at it in glasses/GlasscodeGlasses/Sources/GlasscodeConfig.swift:");
            println!("  relayURL    = \"{}\"", config.url);
            println!("  macDeviceID = \"{}\"", config.device_id);
            println!("  relaySecret = \"<the same secret>\"   (and set useRelay = true)");
            println!();
            println!("The daemon picks it up within a few seconds (or restart it).");
            Ok(())
        }
    }
}

fn status() -> anyhow::Result<()> {
    let state: ServerState = serde_json::from_value(request("state", Value::Null)?)?;
    match request("relay.status", Value::Null).ok().and_then(|v| serde_json::from_value::<Option<relay_link::RelayStatus>>(v).ok()).flatten() {
        Some(relay) if relay.signed_in => {
            let link = if relay.connected { "connected to the relay" } else { "not connected to the relay" };
            println!("Relay: signed in, {link}, {} device(s) allowed, {} online", relay.devices_allowed, relay.online.len());
            if let Some(error) = relay.error {
                println!("  {error}");
            }
        }
        _ => println!("Relay: not configured (SSH/Tailscale)"),
    }
    if state.sessions.is_empty() {
        println!("No sessions. Projects: {}", state.projects.len());
        return Ok(());
    }
    println!("{:<34} {:<8} {:<10} {}", "SESSION", "AGENT", "STATE", "FOLDER");
    for s in &state.sessions {
        let status = match (s.running, s.activity) {
            (false, _) => "stopped".to_string(),
            (true, Some(a)) => format!("{a:?}").to_lowercase(),
            (true, None) => "running".into(),
        };
        println!("{:<34} {:<8} {:<10} {}", s.tmux_name, s.session.agent.name(), status, s.session.worktree.display());
    }
    Ok(())
}

fn attach(name: Option<&str>) -> anyhow::Result<()> {
    let name = name.context("usage: glasscoded attach <session>")?;
    let state: ServerState = serde_json::from_value(request("state", Value::Null)?)?;
    let session = state
        .sessions
        .iter()
        .find(|s| s.tmux_name == name || s.session.id.starts_with(name) || s.session.title == name)
        .with_context(|| format!("no session {name}; see `glasscoded status`"))?;
    let attach: glasscode_core::remote::Attach = serde_json::from_value(request("sessions.attach", serde_json::json!({ "session_id": session.session.id }))?)?;
    let err = Command::new(&attach.argv[0]).args(&attach.argv[1..]).exec();
    bail!("could not run tmux: {err}")
}

fn doctor() -> anyhow::Result<()> {
    let env = daemon::session_env();
    let mut failed = false;
    for check in doctor::run(&env) {
        let mark = match check.level {
            CheckLevel::Ok => "✓",
            CheckLevel::Warn => "!",
            CheckLevel::Fail => {
                failed = true;
                "✗"
            }
        };
        println!("{mark} {}: {}", check.name, check.detail);
        if let Some(fix) = check.fix {
            println!("    fix: {fix}");
        }
    }
    if failed {
        std::process::exit(2);
    }
    Ok(())
}
