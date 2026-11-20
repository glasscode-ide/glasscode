//! glasscoded — Glasscode's server daemon.
//!
//! Runs on a developer's always-on machine (usually Linux, reached over Tailscale) and keeps
//! their Claude Code and Codex sessions running while the laptop is closed. The Mac app (and
//! later a phone app) talks to it through `ssh <server> glasscoded rpc`.

// The daemon needs tmux, systemd and Unix sockets, so on Windows only a short main is built
// (the Windows app reaches the daemon on a Linux server over SSH).
#[cfg(unix)]
mod daemon;
#[cfg(unix)]
mod doctor;
#[cfg(unix)]
mod relay_link;
#[cfg(unix)]
mod install;
#[cfg(unix)]
mod manager;
#[cfg(unix)]
mod mirror;
#[cfg(unix)]
mod phones;
#[cfg(unix)]
mod push;
#[cfg(unix)]
mod today;
#[cfg(unix)]
mod tmux;
#[cfg(unix)]
mod update;

#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
use anyhow::{Context as _, bail};
#[cfg(unix)]
use glasscode_core::caller::Caller;
#[cfg(unix)]
use glasscode_core::paths;
#[cfg(unix)]
use glasscode_core::remote::{CheckLevel, Request, Response, ServerState};
#[cfg(unix)]
use serde_json::Value;

#[cfg(unix)]
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
  glasscoded manager status           whether the manager runs here, and what waits
  glasscoded manager ask \"<question>\" ask the manager (when it runs here); --voice for a short spoken answer
  glasscoded version
";

#[cfg(unix)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The `glasscode` command for sessions here (~/.local/bin/glasscode runs this).
    if args.first().map(String::as_str) == Some("cli") {
        std::process::exit(glasscode_core::cli::main(args[1..].to_vec()));
    }
    let result = match args.first().map(String::as_str) {
        Some("serve") => daemon::serve(),
        Some("rpc") => rpc(),
        Some("gate") => gate(&args[1..]),
        Some("relay") => relay_cmd(&args[1..]),
        Some("update") => update::command(),
        Some("install") => install::install(),
        Some("uninstall") => install::uninstall(),
        Some("status") => status(),
        Some("attach") => attach(args.get(1).map(String::as_str), None),
        Some("doctor") => doctor(),
        Some("manager") => manager::command(&args[1..]),
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
#[cfg(unix)]
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
#[cfg(unix)]
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

#[cfg(unix)]
fn rpc() -> anyhow::Result<()> {
    let stream = connect()?;
    let from_daemon = stream.try_clone()?;
    let replies = std::thread::spawn(move || pump(from_daemon, std::io::stdout()));
    pump(std::io::stdin(), &stream);
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = replies.join();
    Ok(())
}

/// Name `caller` on a new connection to the daemon (M13.2) and read its one reply line, before
/// anything else is sent or read: the phone never sees a line it did not ask for. A daemon older
/// than this binary (before its restart after an update) does not know the method: an error.
#[cfg(unix)]
fn declare_on(stream: &UnixStream, caller: &Caller) -> anyhow::Result<()> {
    use std::io::Read as _;
    let mut line = serde_json::to_string(&Request { id: 0, method: glasscode_core::caller::DECLARE_METHOD.into(), params: serde_json::to_value(caller)? })?;
    line.push('\n');
    let mut to = stream;
    to.write_all(line.as_bytes())?;
    to.flush()?;
    // One byte at a time, so nothing after the reply line is taken from the connection.
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reply = Vec::new();
    let mut from = stream;
    let mut byte = [0u8; 1];
    while reply.len() < 64 * 1024 {
        match from.read(&mut byte) {
            Ok(0) => bail!("the daemon closed the connection"),
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => reply.push(byte[0]),
            Err(err) => return Err(err).context("the daemon did not answer the caller declaration"),
        }
    }
    stream.set_read_timeout(None)?;
    let response: Response = serde_json::from_slice(&reply).context("a bad answer to the caller declaration")?;
    match response.error {
        Some(err) => bail!("{err}"),
        None => Ok(()),
    }
}

/// The device a phone's key names in its `authorized_keys` command (`gate --device <id>`), from
/// the gate's own arguments, never from what the phone asks to run. A line written before M13.2
/// names none: the device `unknown`. Such lines keep working and are never rewritten.
#[cfg(unix)]
fn gate_device(args: &[String]) -> anyhow::Result<String> {
    match args {
        [] => Ok(glasscode_core::caller::UNKNOWN_DEVICE.to_string()),
        [flag, id] if flag == "--device" && glasscode_core::caller::device_id_ok(id) => Ok(id.clone()),
        _ => bail!("this key's gate line is not one Glasscode wrote"),
    }
}

/// A phone's `rpc`: as [`rpc`], after the gate named the phone's connection (`Caller::Phone`),
/// a request line at a time, each recorded first for the words ledger when it types into a
/// session (`daemon::record_phone_input`). The daemon reads whole lines anyway, so nothing waits
/// longer than before.
#[cfg(unix)]
fn phone_rpc(device: &str) -> anyhow::Result<()> {
    let stream = connect()?;
    if let Err(err) = declare_on(&stream, &Caller::Phone { device: device.to_string() }) {
        // An older daemon, until it restarts with this binary: the phone keeps what it had.
        eprintln!("glasscoded gate: the daemon did not take the phone's caller: {err:#}");
    }
    let from_daemon = stream.try_clone()?;
    let replies = std::thread::spawn(move || pump(from_daemon, std::io::stdout()));
    let mut to = &stream;
    let mut reader = BufReader::new(std::io::stdin());
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                daemon::record_phone_input(device, &String::from_utf8_lossy(&line));
                if to.write_all(&line).and_then(|_| to.flush()).is_err() {
                    break;
                }
            }
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = replies.join();
    Ok(())
}

/// Run by sshd for a phone's key (`command="… gate --device <id>"` in authorized_keys): the
/// phone may talk to the daemon, as that device, and open a session's terminal, nothing else.
#[cfg(unix)]
fn gate(args: &[String]) -> anyhow::Result<()> {
    let device = gate_device(args)?;
    let original = std::env::var("SSH_ORIGINAL_COMMAND").unwrap_or_default();
    let words: Vec<&str> = original.split_whitespace().collect();
    let words = match words.first() {
        Some(first) if first.ends_with("glasscoded") => &words[1..],
        _ => &words[..],
    };
    match words {
        ["rpc"] => phone_rpc(&device),
        ["attach", id] if id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') => attach(Some(id), Some(&Caller::Phone { device })),
        _ => bail!("this key can only use Glasscode (rpc, attach <session>)"),
    }
}

#[cfg(unix)]
fn request(method: &str, params: Value) -> anyhow::Result<Value> {
    request_as(None, method, params)
}

/// One request on a connection of its own, as `caller` when one is given (a phone's `attach`).
#[cfg(unix)]
fn request_as(caller: Option<&Caller>, method: &str, params: Value) -> anyhow::Result<Value> {
    let mut stream = connect()?;
    if let Some(caller) = caller
        && let Err(err) = declare_on(&stream, caller)
    {
        eprintln!("glasscoded: the daemon did not take the caller: {err:#}");
    }
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

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
fn attach(name: Option<&str>, caller: Option<&Caller>) -> anyhow::Result<()> {
    let name = name.context("usage: glasscoded attach <session>")?;
    let state: ServerState = serde_json::from_value(request_as(caller, "state", Value::Null)?)?;
    let session = state
        .sessions
        .iter()
        .find(|s| s.tmux_name == name || s.session.id.starts_with(name) || s.session.title == name)
        .with_context(|| format!("no session {name}; see `glasscoded status`"))?;
    let attach: glasscode_core::remote::Attach = serde_json::from_value(request_as(caller, "sessions.attach", serde_json::json!({ "session_id": session.session.id }))?)?;
    let err = Command::new(&attach.argv[0]).args(&attach.argv[1..]).exec();
    bail!("could not run tmux: {err}")
}

#[cfg(unix)]
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

#[cfg(not(unix))]
fn main() {
    eprintln!("glasscoded operates on Linux");
    std::process::exit(1);
}
