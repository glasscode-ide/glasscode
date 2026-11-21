//! Glasscode, the voice copilot (macOS): its state, mirrored from the voice control server, and
//! the headless voice helper. The voice screen (`voice_view.rs`) starts and ends a live session.
//! The right panel's Glasscode tab, which mirrored the old glasses app's lens, went with the
//! glasses plan (G10): the manager is what the glasses talk to now, and the title bar's glasses
//! icon says whether they are connected.
//!
//! The voice brain (ElevenLabs) attaches to [`GlasscodeState::live`] — see the `voice/`
//! crate-adjacent app.

use std::path::PathBuf;
use std::process::Child;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context};

use crate::workspace::Workspace;

/// Glasscode's own state, mirrored from the voice control server (`/api/voice/state`):
/// whether a live session is open, what she's doing, and the last thing she said.
pub struct GlasscodeState {
    pub live: bool,
    pub mode: String, // "idle" | "connecting" | "listening" | "speaking"
    pub caption: Option<String>,
    pub last_poll: Instant,
    /// The headless voice helper process, alive only while Glasscode is live. Glasscode spawns it
    /// when she goes live and kills it when she goes idle — no browser, ever.
    pub helper: Option<Child>,
    /// The voice screen over Today (`voice_view.rs`), which shows the state above.
    pub screen: crate::voice_view::Screen,
}

impl GlasscodeState {
    pub fn new() -> Self {
        GlasscodeState {
            live: false,
            mode: "idle".into(),
            caption: None,
            last_poll: Instant::now() - Duration::from_secs(2),
            helper: None,
            screen: Default::default(),
        }
    }
}

const VOICE_STATE_URL: &str = "http://127.0.0.1:4517/api/voice/state";

/// The voice helper is a macOS program (Swift and LiveKit), so Windows has no voice panel yet:
/// no Glasscode tab, no settings rows, no helper, and no microphone button for the voice screen
/// (only its debug hook opens it there).
pub const VOICE: bool = !cfg!(windows);

/// Find the `glasscode-voice` helper binary: an explicit override, then inside the app bundle
/// (Contents/Resources), then this repo's dev builds.
fn helper_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LUNA_HELPER") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(macos) = exe.parent() {
            let res = macos.join("../Resources/glasscode-voice");
            if res.exists() {
                return Some(res);
            }
        }
    }
    // Development fallback: the voice helper built in-repo (path relative to this crate).
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../voice/helper/.build");
    for profile in ["release", "debug"] {
        let pb = repo.join(profile).join("glasscode-voice");
        if pb.exists() {
            return Some(pb);
        }
    }
    None
}

impl Workspace {
    /// Go live or idle. Live spawns the headless voice helper (it opens the ElevenLabs session,
    /// does the mic + speaker, and drives sessions through the control plane); idle kills it.
    /// The helper reports live/mode/caption to the shared state, which [`glasscode_tick`] mirrors
    /// back — so the browser never enters the picture. The voice screen's microphone and End
    /// call this (`voice_view.rs`).
    pub fn glasscode_toggle(&mut self, cx: &mut Context<Self>) {
        self.glasscode.live = !self.glasscode.live;
        let live = self.glasscode.live;
        if live {
            self.glasscode.mode = "connecting".into();
            self.glasscode.caption = None;
            self.start_helper();
            cx.background_spawn(async move {
                let _ = ureq::post(VOICE_STATE_URL).set("Content-Type", "application/json").send_string("{\"intentLive\":true}");
            })
            .detach();
        } else {
            self.stop_helper();
            self.glasscode.caption = None;
            self.glasscode.mode = "idle".into();
            // Clean the shared state even if the helper was killed before it could report out.
            cx.background_spawn(async move {
                let _ = ureq::post(VOICE_STATE_URL)
                    .set("Content-Type", "application/json")
                    .send_string("{\"intentLive\":false,\"live\":false,\"mode\":\"idle\",\"caption\":\"\"}");
            })
            .detach();
        }
        cx.notify();
    }

    /// Spawn the voice helper (killing any stale one first).
    fn start_helper(&mut self) {
        self.stop_helper();
        if !VOICE {
            return;
        }
        let Some(path) = helper_path() else {
            log::warn!("glasscode: voice helper binary not found (set LUNA_HELPER or bundle it)");
            return;
        };
        let mut cmd = glasscode_core::sys::command(&path);
        cmd.env("LUNA_CONTROL", "http://127.0.0.1:4517");
        // A dev (.build) binary has a broken @rpath because the repo path contains a space;
        // point the loader at the SwiftPM products dir so it finds the LiveKit frameworks.
        let s = path.to_string_lossy();
        if s.contains("/.build/debug/") || s.contains("/.build/release/") {
            if let Some(build_dir) = path.parent().and_then(|p| p.parent()) {
                let profile = if s.contains("/release/") { "Release" } else { "Debug" };
                cmd.env("DYLD_FRAMEWORK_PATH", build_dir.join("out/Products").join(profile));
            }
        }
        match cmd.spawn() {
            Ok(child) => self.glasscode.helper = Some(child),
            Err(e) => log::warn!("glasscode: failed to spawn voice helper: {e}"),
        }
    }

    /// Kill the voice helper if one is running.
    fn stop_helper(&mut self) {
        if let Some(mut child) = self.glasscode.helper.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Keep the panel in step with the helper. The helper *process* is the source of truth for
    /// "live" (it's the voice runtime); the shared state gives us what she's doing right now
    /// (listening / speaking) and the last thing she said. Polled from the 700 ms workspace
    /// tick; throttled to ~1 s. Runs only while the tab is open or she's live.
    pub fn glasscode_tick(&mut self, cx: &mut Context<Self>) {
        // The voice screen's made-up states (its debug hook, on each platform) and its close when
        // the helper stops: on this tick, so the screen has no clock of its own.
        self.voice_tick(cx);
        // Nothing to poll: no helper and no voice server here.
        if !VOICE {
            return;
        }
        // Reap the helper if it exited on its own (session ended or errored out).
        let helper_alive = match self.glasscode.helper.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(None) => true,
                _ => {
                    self.glasscode.helper = None;
                    false
                }
            },
            None => false,
        };
        if self.glasscode.live != helper_alive {
            self.glasscode.live = helper_alive;
            if !helper_alive {
                self.glasscode.mode = "idle".into();
                self.glasscode.caption = None;
            }
            cx.notify();
        }

        if !self.glasscode.live || self.glasscode.last_poll.elapsed() < Duration::from_millis(200) {
            return;
        }
        self.glasscode.last_poll = Instant::now();
        let task = cx.background_spawn(async move {
            let body = ureq::get(VOICE_STATE_URL).call().ok()?.into_string().ok()?;
            let v: serde_json::Value = serde_json::from_str(&body).ok()?;
            let mode = v.get("mode").and_then(|x| x.as_str()).unwrap_or("idle").to_string();
            let caption = v.get("caption").and_then(|x| x.as_str()).unwrap_or("").to_string();
            Some((mode, caption))
        });
        cx.spawn(async move |this, cx| {
            let Some((mode, caption)) = task.await else { return };
            let _ = this.update(cx, |this, cx| {
                // Only trust the reported mode and caption while the helper is actually live.
                if this.glasscode.live && (this.glasscode.mode != mode || this.glasscode.caption.as_deref().unwrap_or("") != caption) {
                    this.glasscode.mode = mode;
                    this.glasscode.caption = if caption.is_empty() { None } else { Some(caption) };
                    cx.notify();
                }
            });
        })
        .detach();
    }
}
