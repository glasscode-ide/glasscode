//! The manager's voice on this Mac (Settings › Manager › Voice › "On this Mac"): Glasscode runs
//! the LiveKit server (`livekit-server`) and the voice worker (`voice/manager/worker`, Python)
//! itself, as two child processes, while the switch is on and the manager is on. Off means off:
//! neither runs, and none is left behind by a crash (their pids are kept and cleared on start).
//!
//! - **LiveKit** listens on this Mac only (127.0.0.1) and on its tailnet address, so the phone
//!   with the glasses reaches it over Tailscale. Its own ports (`PORT`…), not LiveKit's usual
//!   7880, which another program may hold. The key and secret are made once and kept in the
//!   Keychain (`voice::local_keys`); they go to LiveKit in its environment, never in a file.
//! - **The worker** runs in its own Python (made with `uv`, Python 3.13, in
//!   `~/.glasscode/voice/venv`, the first time it is switched on). It gets the Soniox key and the
//!   talker's key from the Keychain, in its environment.
//! - Logs: `~/.glasscode/voice/livekit.log`, `worker.log`, `setup.log`.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use glasscode_core::manager::{Settings, voice};
use gpui::{AppContext as _, Context};

use crate::workspace::Workspace;

/// The room's address on this Mac, and LiveKit's two media ports.
pub const PORT: u16 = 7890;
const RTC_TCP: u16 = 7891;
const RTC_UDP: u16 = 7892;
/// After it stopped by itself: how often it is started again, and the first wait.
const RESTARTS: u32 = 5;
const RESTART_FIRST: Duration = Duration::from_secs(5);
/// The worker's Python, as `uv` names it.
const PYTHON: &str = "3.13";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Status {
    /// Switched off (or the manager is): nothing runs.
    #[default]
    Off,
    /// Being set up or started: what it is doing, in words.
    Preparing(String),
    Running,
    /// Why it does not run, in words for the person.
    Failed(String),
}

#[derive(Default)]
pub struct VoiceServer {
    pub status: Status,
    /// This Mac's tailnet address when it started: the phone's way in (None: only this Mac).
    pub tailnet: Option<Ipv4Addr>,
    livekit: Option<Child>,
    worker: Option<Child>,
    /// A start runs in the background.
    busy: bool,
    retry_at: Option<Instant>,
    retries: u32,
    /// What it was started with (`fingerprint`): it starts again when that changes.
    started_with: Option<String>,
    quit_hooked: bool,
}

impl VoiceServer {
    pub fn running(&self) -> bool {
        self.status == Status::Running && self.livekit.is_some()
    }

    /// The room's address for this Mac's own helper.
    pub fn url(&self) -> String {
        format!("ws://127.0.0.1:{PORT}")
    }

    /// The room's address for a paired phone, over Tailscale.
    pub fn phone_url(&self) -> Option<String> {
        self.tailnet.map(|ip| format!("ws://{ip}:{PORT}"))
    }

    /// End both. They are asked to end at once (also when the app is quitting: it may be gone
    /// before a thread runs); a thread waits for them, kills what has not ended, and only then
    /// forgets their pids, so a start after a crash still finds what is left.
    fn stop(&mut self) {
        let children: Vec<(Child, &'static str)> = [(self.worker.take(), "worker.pid"), (self.livekit.take(), "livekit.pid")].into_iter().filter_map(|(c, p)| c.map(|c| (c, p))).collect();
        for (child, _) in &children {
            terminate(child.id());
        }
        if !children.is_empty() {
            std::thread::spawn(move || {
                for (child, pidfile) in children {
                    end(child);
                    let _ = std::fs::remove_file(dir().join(pidfile));
                }
            });
        }
        self.started_with = None;
    }
}

/// `~/.glasscode/voice`.
fn dir() -> PathBuf {
    glasscode_core::paths::root().join("voice")
}

/// What a running voice depends on: when one of these changes, it starts again.
fn fingerprint(settings: &Settings) -> String {
    let v = &settings.voice;
    format!("{}|{}|{}|{}|{}|{}|{}|{}|{}", settings.name, v.stt, v.tts, v.voice, v.talker, v.talker_model, v.talker_url, v.language, v.soniox_region)
}

/// Whether the voice on this Mac should run now.
pub fn wanted(settings: &Settings) -> bool {
    settings.enabled && settings.voice.local
}

/// A program Glasscode ships in its bundle (Contents/Resources), for a build from the repository
/// the given folder, else on the login PATH.
fn bundled(name: &str, env_var: &str, repo: Option<PathBuf>, env: &HashMap<String, String>) -> Option<PathBuf> {
    if let Ok(path) = std::env::var(env_var) {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }
    if let Some(resources) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|m| m.join("../Resources"))) {
        let path = resources.join(name);
        if path.exists() {
            return Some(path);
        }
    }
    if let Some(path) = repo.filter(|p| p.exists()) {
        return Some(path);
    }
    glasscode_core::agents::resolve_program(name, env)
}

fn livekit_path(env: &HashMap<String, String>) -> Option<PathBuf> {
    bundled("livekit-server", "GLASSCODE_LIVEKIT_SERVER", None, env).or_else(|| ["/opt/homebrew/bin/livekit-server", "/usr/local/bin/livekit-server"].iter().map(PathBuf::from).find(|p| p.exists()))
}

fn worker_dir(env: &HashMap<String, String>) -> Option<PathBuf> {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../voice/manager/worker");
    bundled("voice-worker", "GLASSCODE_VOICE_WORKER", Some(repo), env).filter(|d| d.join("worker.py").exists())
}

fn uv_path(env: &HashMap<String, String>) -> Option<PathBuf> {
    let home = env.get("HOME").cloned().or_else(|| std::env::var("HOME").ok()).unwrap_or_default();
    glasscode_core::agents::resolve_program("uv", env).or_else(|| {
        [format!("{home}/.local/bin/uv"), format!("{home}/.cargo/bin/uv"), "/opt/homebrew/bin/uv".into(), "/usr/local/bin/uv".into()].into_iter().map(PathBuf::from).find(|p| p.exists())
    })
}

/// The last line of a log that says something, for a message.
fn last_line(path: &Path) -> String {
    let mut text = String::new();
    if File::open(path).and_then(|mut f| f.read_to_string(&mut text)).is_err() {
        return String::new();
    }
    text.lines().rev().map(str::trim).find(|l| !l.is_empty()).map(|l| l.chars().take(240).collect()).unwrap_or_default()
}

/// Ask a process group to end (the worker starts one process per call).
fn terminate(pid: u32) {
    let _ = Command::new("/bin/kill").args(["-TERM", &format!("-{pid}")]).stderr(Stdio::null()).status();
}

/// Ask a child to end, and kill it (its whole group) if it has not gone a moment later.
fn end(mut child: Child) {
    let group = format!("-{}", child.id());
    terminate(child.id());
    for _ in 0..30 {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = Command::new("/bin/kill").args(["-KILL", &group]).stderr(Stdio::null()).status();
    let _ = child.kill();
    let _ = child.wait();
}

/// A process an earlier run left (Glasscode crashed): ended, when it is still ours.
fn end_stale(pidfile: &Path, program: &str) {
    let Some(pid) = std::fs::read_to_string(pidfile).ok().and_then(|t| t.trim().parse::<u32>().ok()) else { return };
    let _ = std::fs::remove_file(pidfile);
    let out = Command::new("/bin/ps").args(["-p", &pid.to_string(), "-o", "command="]).output();
    let command = out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    if command.contains(program) {
        log::info!("voice: ending {program} ({pid}) left from an earlier run");
        let _ = Command::new("/bin/kill").args(["-TERM", &format!("-{pid}")]).stderr(Stdio::null()).status();
        let _ = Command::new("/bin/kill").args(["-TERM", &pid.to_string()]).stderr(Stdio::null()).status();
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// The worker's Python, made the first time (and again when its requirements change).
fn prepare(worker: &Path, uv: &Path) -> Result<PathBuf, String> {
    let venv = dir().join("venv");
    let python = venv.join("bin/python");
    let wanted = std::fs::read(worker.join("requirements.txt")).map_err(|e| format!("The voice worker has no requirements.txt ({e})."))?;
    let stamp = venv.join(".glasscode-requirements.txt");
    if python.exists() && std::fs::read(&stamp).is_ok_and(|had| had == wanted) {
        return Ok(python);
    }
    let log_path = dir().join("setup.log");
    let log = File::create(&log_path).map_err(|e| format!("Could not write {} ({e}).", log_path.display()))?;
    let run = |args: &[&str]| -> Result<(), String> {
        let status = Command::new(uv)
            .args(args)
            .stdin(Stdio::null())
            .stdout(log.try_clone().map(Stdio::from).unwrap_or(Stdio::null()))
            .stderr(log.try_clone().map(Stdio::from).unwrap_or(Stdio::null()))
            .status()
            .map_err(|e| format!("uv could not be started ({e})."))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("Setting up the voice failed: {}. See ~/.glasscode/voice/setup.log.", last_line(&log_path)))
        }
    };
    run(&["venv", "--allow-existing", "--python", PYTHON, &venv.to_string_lossy()])?;
    run(&["pip", "install", "--python", &python.to_string_lossy(), "-r", &worker.join("requirements.txt").to_string_lossy()])?;
    let _ = std::fs::write(&stamp, wanted);
    Ok(python)
}

/// The keys a start needs: LiveKit's pair and the services' keys.
struct Secrets {
    livekit: (String, String),
    soniox: Option<String>,
    talker: Option<String>,
}

/// The keys from the Keychain (LiveKit's made the first time).
fn secrets() -> Result<Secrets, String> {
    let livekit = voice::local_keys().map_err(|e| format!("The voice's key could not be kept in the Keychain ({e:#})."))?;
    Ok(Secrets { livekit, soniox: voice::secret(voice::SONIOX_ACCOUNT), talker: voice::secret(voice::TALKER_ACCOUNT) })
}

/// Soniox's addresses for a region ("eu": its EU ones; else its global ones, the worker's own).
fn soniox_urls(region: &str) -> Option<(&'static str, &'static str)> {
    (region.trim().eq_ignore_ascii_case("eu")).then_some(("wss://stt-rt.eu.soniox.com/transcribe-websocket", "wss://tts-rt.eu.soniox.com/tts-websocket"))
}

/// Everything a start needs, read on the main thread.
struct Plan {
    livekit: PathBuf,
    worker: PathBuf,
    uv: PathBuf,
    settings: Settings,
    path: String,
    home: String,
}

/// What a start made.
struct Started {
    livekit: Child,
    worker: Child,
    tailnet: Option<Ipv4Addr>,
}

fn spawn_logged(cmd: &mut Command, log_name: &str, pid_name: &str) -> Result<Child, String> {
    let log = File::create(dir().join(log_name)).map_err(|e| format!("Could not write ~/.glasscode/voice/{log_name} ({e})."))?;
    cmd.stdin(Stdio::null()).stdout(log.try_clone().map(Stdio::from).unwrap_or(Stdio::null())).stderr(Stdio::from(log));
    // Its own process group: ending it ends what it started.
    #[cfg(unix)]
    cmd.process_group(0);
    let child = cmd
        .spawn()
        .map_err(|e| format!("Could not start {} ({e}).", cmd.get_program().to_string_lossy()))?;
    let _ = std::fs::write(dir().join(pid_name), child.id().to_string());
    Ok(child)
}

/// Whether LiveKit answers on its address.
fn answers() -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&(Ipv4Addr::LOCALHOST, PORT).into(), Duration::from_millis(300)) else { return false };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    if stream.write_all(b"GET / HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").is_err() {
        return false;
    }
    let mut reply = String::new();
    let _ = stream.read_to_string(&mut reply);
    reply.starts_with("HTTP/1.") && reply.contains(" 200 ")
}

/// The slow part of a start, off the main thread: keys, the worker's Python, LiveKit, the worker.
fn start(plan: Plan, secrets: Secrets) -> Result<Started, String> {
    if !cfg!(unix) {
        return Err("The voice on this computer runs on a Mac only; use a voice server (Settings › Manager › Voice).".into());
    }
    std::fs::create_dir_all(dir()).map_err(|e| format!("Could not make ~/.glasscode/voice ({e})."))?;
    end_stale(&dir().join("worker.pid"), "worker.py");
    end_stale(&dir().join("livekit.pid"), "livekit-server");
    if TcpListener::bind((Ipv4Addr::LOCALHOST, PORT)).is_err() {
        return Err(format!("Port {PORT} on this Mac is used by another program, so the voice cannot start there."));
    }
    let (key, secret) = secrets.livekit.clone();
    let settings = &plan.settings;
    let v = &settings.voice;
    let stt = if v.stt.trim().is_empty() { "soniox" } else { v.stt.trim() };
    let tts = if v.tts.trim().is_empty() { "soniox" } else { v.tts.trim() };
    let soniox = secrets.soniox.clone();
    if (stt == "soniox" || tts == "soniox") && soniox.is_none() {
        return Err("The Soniox key is missing. Enter it in Settings › Manager › Voice.".into());
    }
    let talker = v.talker.trim().to_lowercase();
    let talker_key = secrets.talker.clone();
    if !talker.is_empty() && talker_key.is_none() {
        return Err("The talker's key is missing. Enter it in Settings › Manager › Voice, or set the talker to None.".into());
    }
    let python = prepare(&plan.worker, &plan.uv)?;
    let tailnet = glasscode_core::device_link::tailscale_ipv4();

    // LiveKit: this Mac and the tailnet only; the media goes to the tailnet address.
    let node_ip = tailnet.unwrap_or(Ipv4Addr::LOCALHOST);
    let binds = match tailnet {
        Some(ip) => format!("[127.0.0.1, {ip}]"),
        None => "[127.0.0.1]".into(),
    };
    let config = format!(
        "port: {PORT}\nbind_addresses: {binds}\nrtc:\n  tcp_port: {RTC_TCP}\n  udp_port: {RTC_UDP}\n  use_external_ip: false\n  node_ip: {node_ip}\nroom:\n  empty_timeout: 300\n  departure_timeout: 20\nlogging:\n  level: info\n"
    );
    let config_path = dir().join("livekit.yaml");
    std::fs::write(&config_path, config).map_err(|e| format!("Could not write {} ({e}).", config_path.display()))?;
    let mut livekit = spawn_logged(
        Command::new(&plan.livekit).arg("--config").arg(&config_path).env_clear().env("HOME", &plan.home).env("PATH", &plan.path).env("LIVEKIT_KEYS", format!("{key}: {secret}")),
        "livekit.log",
        "livekit.pid",
    )?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !answers() {
        if Instant::now() > deadline || !matches!(livekit.try_wait(), Ok(None)) {
            let why = last_line(&dir().join("livekit.log"));
            end(livekit);
            return Err(format!("LiveKit did not start on this Mac: {why}. See ~/.glasscode/voice/livekit.log."));
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    // The worker.
    let mut cmd = Command::new(&python);
    cmd.arg("-B")
        .arg(plan.worker.join("worker.py"))
        .arg("start")
        .current_dir(&plan.worker)
        .env_clear()
        .env("HOME", &plan.home)
        .env("PATH", &plan.path)
        .env("LANG", "en_US.UTF-8")
        .env("LIVEKIT_URL", format!("ws://127.0.0.1:{PORT}"))
        .env("LIVEKIT_API_KEY", &key)
        .env("LIVEKIT_API_SECRET", &secret)
        .env("MANAGER_NAME", &settings.name)
        .env("MANAGER_STT", stt)
        .env("MANAGER_TTS", tts)
        .env("MANAGER_VOICE", v.voice.trim())
        .env("MANAGER_LANGUAGE", if v.language.trim().is_empty() { "en" } else { v.language.trim() })
        .env("MANAGER_TALKER", &talker)
        .env("MANAGER_TALKER_MODEL", v.talker_model.trim())
        .env("MANAGER_TALKER_URL", v.talker_url.trim());
    if let Some(soniox) = &soniox {
        cmd.env("SONIOX_API_KEY", soniox);
    }
    if let Some((stt_url, tts_url)) = soniox_urls(&v.soniox_region) {
        cmd.env("SONIOX_STT_URL", stt_url).env("SONIOX_TTS_URL", tts_url);
    }
    if let Some(talker_key) = &talker_key {
        cmd.env("MANAGER_TALKER_KEY", talker_key);
    }
    let worker = match spawn_logged(&mut cmd, "worker.log", "worker.pid") {
        Ok(worker) => worker,
        Err(why) => {
            end(livekit);
            return Err(why);
        }
    };
    Ok(Started { livekit, worker, tailnet })
}

impl Workspace {
    /// Called from the workspace tick: starts, stops and watches the voice on this Mac.
    pub fn voice_server_tick(&mut self, cx: &mut Context<Self>) {
        let settings = &self.state.manager;
        if !wanted(settings) {
            if self.vserver.livekit.is_some() || self.vserver.worker.is_some() || self.vserver.status != Status::Off {
                self.vserver.stop();
                self.vserver.status = Status::Off;
                self.vserver.retry_at = None;
                self.vserver.retries = 0;
                cx.notify();
            }
            return;
        }
        if self.vserver.busy {
            return;
        }
        // Started with other settings (another talker, voice, name): again, with these.
        if self.vserver.started_with.as_ref().is_some_and(|had| *had != fingerprint(settings)) {
            log::info!("voice: the settings changed, starting it again");
            self.voice_server_restart(cx);
            return;
        }
        // Tailscale came up, or its address changed (the door looks now and then): the phone's
        // way in is the new one.
        if self.vserver.running() && self.door.tailnet.is_some() && self.door.tailnet != self.vserver.tailnet {
            log::info!("voice: the tailnet address changed, starting it again");
            self.voice_server_restart(cx);
            return;
        }
        // One of the two ended by itself: say why, and start again a little later.
        let livekit_gone = self.vserver.livekit.as_mut().is_some_and(|c| !matches!(c.try_wait(), Ok(None)));
        let worker_gone = self.vserver.worker.as_mut().is_some_and(|c| !matches!(c.try_wait(), Ok(None)));
        if livekit_gone || worker_gone {
            let (what, log) = if worker_gone { ("The voice worker", "worker.log") } else { ("LiveKit", "livekit.log") };
            let why = last_line(&dir().join(log));
            self.vserver.stop();
            self.vserver.status = Status::Failed(format!("{what} stopped on this Mac: {why}. See ~/.glasscode/voice/{log}."));
            self.voice_server_plan_retry();
            cx.notify();
            return;
        }
        let stopped = self.vserver.livekit.is_none();
        let retry_due = self.vserver.retry_at.is_none_or(|at| at <= Instant::now());
        let gave_up = matches!(self.vserver.status, Status::Failed(_)) && self.vserver.retry_at.is_none();
        if stopped && retry_due && !gave_up {
            self.voice_server_start(cx);
        }
    }

    /// Start it again now (a key was saved, the settings changed, or the person asked).
    pub fn voice_server_restart(&mut self, cx: &mut Context<Self>) {
        self.vserver.stop();
        self.vserver.status = Status::Off;
        self.vserver.retry_at = None;
        self.vserver.retries = 0;
        if wanted(&self.state.manager) {
            self.voice_server_start(cx);
        }
        cx.notify();
    }

    fn voice_server_plan_retry(&mut self) {
        let server = &mut self.vserver;
        if server.retries >= RESTARTS {
            server.retry_at = None;
            return;
        }
        server.retry_at = Some(Instant::now() + RESTART_FIRST * 2u32.pow(server.retries));
        server.retries += 1;
    }

    fn voice_server_start(&mut self, cx: &mut Context<Self>) {
        let env = self.env.clone();
        let (Some(livekit), Some(worker)) = (livekit_path(&env), worker_dir(&env)) else {
            self.vserver.status = Status::Failed("This build of Glasscode has no voice server (livekit-server and the voice worker).".into());
            self.vserver.retry_at = None;
            cx.notify();
            return;
        };
        let Some(uv) = uv_path(&env) else {
            self.vserver.status = Status::Failed("The voice needs uv to set up its Python: install it (brew install uv) and switch the voice on again.".into());
            self.vserver.retry_at = None;
            cx.notify();
            return;
        };
        if !self.vserver.quit_hooked {
            // However the app quits, LiveKit and the worker end with it.
            self.vserver.quit_hooked = true;
            cx.on_app_quit(|this: &mut Workspace, _| {
                this.vserver.stop();
                async {}
            })
            .detach();
        }
        let settings = self.state.manager.clone();
        let fingerprint = fingerprint(&settings);
        let home = env.get("HOME").cloned().or_else(|| std::env::var("HOME").ok()).unwrap_or_default();
        let path = env.get("PATH").cloned().unwrap_or_else(|| "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin".into());
        let plan = Plan { livekit, worker, uv, settings, path, home };
        self.vserver.busy = true;
        let first = !dir().join("venv/bin/python").exists();
        self.vserver.status = Status::Preparing(if first { "Setting up the voice on this Mac (the first time takes a minute or two)…".into() } else { "Starting the voice on this Mac…".into() });
        cx.notify();
        let task = cx.background_spawn(async move { secrets().and_then(|secrets| start(plan, secrets)) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                ws.vserver.busy = false;
                match result {
                    // Switched off while it started: it does not stay.
                    Ok(started) if !wanted(&ws.state.manager) => {
                        std::thread::spawn(move || {
                            end(started.worker);
                            end(started.livekit);
                        });
                        ws.vserver.status = Status::Off;
                    }
                    Ok(started) => {
                        ws.vserver.livekit = Some(started.livekit);
                        ws.vserver.worker = Some(started.worker);
                        ws.vserver.tailnet = started.tailnet;
                        ws.vserver.started_with = Some(fingerprint);
                        ws.vserver.status = Status::Running;
                        ws.vserver.retries = 0;
                        ws.vserver.retry_at = None;
                        log::info!("voice: running on this Mac at port {PORT}{}", started.tailnet.map(|ip| format!(", for the phone at {ip}")).unwrap_or_default());
                    }
                    Err(why) => {
                        log::warn!("voice: {why}");
                        ws.vserver.status = Status::Failed(why);
                        // A missing key or program is not fixed by trying again.
                        let fixable_by_waiting = matches!(&ws.vserver.status, Status::Failed(w) if w.contains("did not start") || w.contains("is used by"));
                        if fixable_by_waiting {
                            ws.voice_server_plan_retry();
                        } else {
                            ws.vserver.retry_at = None;
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_runs_only_while_switched_on_and_the_manager_is_on() {
        let mut s = Settings { enabled: true, ..Settings::default() };
        assert!(!wanted(&s));
        s.voice.local = true;
        assert!(wanted(&s));
        s.enabled = false;
        assert!(!wanted(&s));
    }

    #[test]
    fn a_change_that_matters_starts_it_again() {
        let mut s = Settings { enabled: true, name: "AI Mira".into(), ..Settings::default() };
        let before = fingerprint(&s);
        s.voice.talker = "anthropic".into();
        assert_ne!(before, fingerprint(&s));
        let talker = fingerprint(&s);
        s.listen = glasscode_core::manager::Listen::Hold;
        assert_eq!(talker, fingerprint(&s), "how it listens is the helper's, not the server's");
    }

    #[test]
    fn soniox_is_reached_in_the_eu_when_asked() {
        assert_eq!(soniox_urls(""), None);
        assert!(soniox_urls("EU").is_some_and(|(stt, tts)| stt.contains(".eu.") && tts.contains(".eu.")));
    }

    /// A real start: LiveKit and the worker run, the worker signs in to LiveKit, and stop ends
    /// both and frees the port. Not in the default run: it makes the worker's Python (a minute
    /// the first time) and needs a livekit-server:
    ///   GLASSCODE_LIVEKIT_SERVER=<livekit-server> cargo test -p glasscode a_real_start -- --ignored
    /// In the tests' own Glasscode folder, with made-up keys: nothing of the Keychain is used.
    #[test]
    #[ignore]
    fn a_real_start_runs_both_and_stop_ends_them() {
        glasscode_core::paths::use_test_root();
        let env: HashMap<String, String> = std::env::vars().collect();
        let livekit = livekit_path(&env).expect("set GLASSCODE_LIVEKIT_SERVER to a livekit-server");
        let worker = worker_dir(&env).expect("the worker in this repository");
        let uv = uv_path(&env).expect("uv");
        let mut settings = Settings { enabled: true, name: "AI Mira".into(), ..Settings::default() };
        settings.voice.local = true;
        let plan = Plan { livekit, worker, uv, settings, path: env.get("PATH").cloned().unwrap_or_default(), home: env.get("HOME").cloned().unwrap_or_default() };
        let secrets = Secrets { livekit: ("APIgctest".into(), "a-made-up-secret-of-enough-length-0123456789".into()), soniox: Some("a-made-up-key".into()), talker: None };
        let started = start(plan, secrets).expect("it starts");
        assert!(answers(), "LiveKit answers on its port");
        let log = dir().join("worker.log");
        let deadline = Instant::now() + Duration::from_secs(90);
        while !std::fs::read_to_string(&log).unwrap_or_default().contains("registered worker") {
            assert!(Instant::now() < deadline, "the worker did not sign in to LiveKit: {}", std::fs::read_to_string(&log).unwrap_or_default());
            std::thread::sleep(Duration::from_millis(250));
        }
        let mut server = VoiceServer { livekit: Some(started.livekit), worker: Some(started.worker), status: Status::Running, ..VoiceServer::default() };
        server.stop();
        let deadline = Instant::now() + Duration::from_secs(15);
        while answers() || dir().join("livekit.pid").exists() || dir().join("worker.pid").exists() {
            assert!(Instant::now() < deadline, "stop left something running");
            std::thread::sleep(Duration::from_millis(250));
        }
        assert!(TcpListener::bind((Ipv4Addr::LOCALHOST, PORT)).is_ok(), "the port is free again");
    }

    #[test]
    fn the_phone_reaches_it_over_the_tailnet_only() {
        let mut server = VoiceServer::default();
        assert_eq!(server.url(), format!("ws://127.0.0.1:{PORT}"));
        assert_eq!(server.phone_url(), None);
        server.tailnet = Some(Ipv4Addr::new(100, 101, 102, 103));
        assert_eq!(server.phone_url().as_deref(), Some("ws://100.101.102.103:7890"));
    }
}
