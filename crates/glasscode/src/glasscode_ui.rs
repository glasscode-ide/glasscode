//! The voice screen's state (macOS): whether voice is on, what it does, and the last words. The
//! voice is the manager's own (`manager_voice.rs`: its LiveKit room, the brain where the manager
//! runs). The microphone buttons and the voice screen (`voice_view.rs`) switch it on and off.
//!
//! The ElevenLabs copilot (`voice/server.mjs`, the `glasscode-voice` helper and its five tools)
//! went with the glasses plan (G16, decision O4), and so did the right panel's Glasscode tab
//! (G10): the glasses talk to the manager through the device door, and the title bar's glasses
//! icon says whether they are connected.

use gpui::Context;

use crate::workspace::Workspace;

/// What the voice screen shows, kept in step with the manager's voice each tick.
pub struct GlasscodeState {
    pub live: bool,
    /// "connecting", "listening", "thinking" or "speaking".
    pub mode: String,
    /// The manager's last words while voice is on.
    pub caption: Option<String>,
    /// The voice screen over Today (`voice_view.rs`), which shows the state above.
    pub screen: crate::voice_view::Screen,
}

impl GlasscodeState {
    pub fn new() -> Self {
        GlasscodeState { live: false, mode: "idle".into(), caption: None, screen: Default::default() }
    }
}

/// The voice helper is a macOS program (Swift and LiveKit), so Windows has no voice yet: no
/// settings rows, no helper, and no microphone button for the voice screen (only its debug hook
/// opens it there).
pub const VOICE: bool = !cfg!(windows);

/// The voice screen's word for what the manager's voice does: "idle" there means the room is
/// joined and waits for the person, which the screen shows as listening.
pub fn mode_of(on: bool, state: &str) -> &'static str {
    match (on, state) {
        (false, _) => "idle",
        (true, "") => "connecting",
        (true, "thinking") => "thinking",
        (true, "speaking") => "speaking",
        (true, _) => "listening",
    }
}

impl Workspace {
    /// Voice on or off: the manager's voice. The voice screen's microphone and End call this
    /// (`voice_view.rs`).
    pub fn glasscode_toggle(&mut self, cx: &mut Context<Self>) {
        self.manager_voice_toggle(cx);
        // It could not start: say what is missing and where to set it.
        if let Some(problem) = self.manager.voice.problem.clone() {
            self.show_toast(problem, cx);
        }
        self.glasscode_sync();
        cx.notify();
    }

    /// Keep the screen's state in step with the manager's voice (four times a second).
    pub fn glasscode_tick(&mut self, cx: &mut Context<Self>) {
        // The voice screen's made-up states (its debug hook, on each platform) and its close when
        // the voice stops: on this tick, so the screen has no clock of its own.
        self.voice_tick(cx);
        if !VOICE {
            return;
        }
        if self.glasscode_sync() {
            cx.notify();
        }
    }

    /// Read the manager's voice into the screen's state; whether something changed.
    fn glasscode_sync(&mut self) -> bool {
        let on = self.manager.voice.on();
        let mode = mode_of(on, &self.manager.voice.state).to_string();
        let caption = on
            .then(|| self.mchat.items.iter().rev().find(|i| i.author == glasscode_core::conversation::Author::Chat).map(|i| i.text.clone()))
            .flatten()
            .filter(|t| !t.trim().is_empty());
        let changed = self.glasscode.live != on || self.glasscode.mode != mode || self.glasscode.caption != caption;
        self.glasscode.live = on;
        self.glasscode.mode = mode;
        self.glasscode.caption = caption;
        changed
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_screen_follows_the_managers_voice() {
        assert_eq!(super::mode_of(false, "speaking"), "idle");
        assert_eq!(super::mode_of(true, ""), "connecting");
        assert_eq!(super::mode_of(true, "idle"), "listening");
        assert_eq!(super::mode_of(true, "thinking"), "thinking");
        assert_eq!(super::mode_of(true, "speaking"), "speaking");
    }
}
