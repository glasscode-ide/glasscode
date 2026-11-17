//! The voice screen (slice U19): it covers Today while voice is on. The orb (`backdrop::orb` in
//! core, the mock's `drawOrb()`), what voice does now (Listening, Thinking, Speaking, Muted), the
//! last words, the last action, Mute and End. Esc does what End does.
//!
//! What it shows comes from the voice state that `glasscode_ui.rs` keeps (`GlasscodeState`: live,
//! mode, caption), which the voice helper reports. That state has no microphone level and no mute,
//! so the orb's level follows what voice does, and Mute shows only for the made-up states of the
//! debug hook. A caption that starts with "→" (the helper's "→ sent to <session>") is the last
//! action; any other caption is the last words.
//!
//! The voice helper is a macOS program, so the microphone buttons that open the screen (in the
//! start box of Today and at the right end of the status bar) show only where
//! `glasscode_ui::VOICE` is true. On Windows only the debug hook opens the screen.
//!
//! The orb asks for no draw of its own. Each draw of the window runs `Workspace::render` and the
//! layout of the window (about 17 ms on Today, U24), so a clock of its own would add a second
//! stream of draws. While the backdrop moves, each of its frames draws the window; the screen's
//! render then asks for the next orb image, which is made on a background thread and kept without
//! a draw, and the next backdrop frame paints it: the orb moves at the backdrop's rate. When the
//! backdrop stands still (the choice None, less motion, the window in the back or hidden) the orb
//! is a still image of the level in steps of a quarter, made again only when the step, the look or
//! the size changes. The screen stands in place of Today's content, so Today's layout does not run
//! while it shows. There is no GPUI animation (`with_animation` draws the window on each display
//! frame, U25).
//!
//! `GLASSCODE_DEBUG_OPEN=voice[:prism|aurora|dusk|none][:smooth|dither][:light|dark][:90|100|115|130]`
//! opens the screen with made-up states in a loop (debug builds). The look words are saved, as on
//! the backdrop page.

use std::sync::Arc;
use std::time::{Duration, Instant};

use glasscode_core::backdrop::{self as pixels, Finish, Frame, Preset};
use glasscode_core::model::{AgentKind, BackdropChoice, ThemeChoice, UiScale};
use gpui::{
    AnyElement, Bounds, ClickEvent, Context, Corners, Div, FocusHandle, FontWeight, KeyDownEvent,
    MouseButton, MouseDownEvent, Pixels, RenderImage, ScrollHandle, Stateful, Window, canvas, div,
    point, prelude::*, px, size,
};

use crate::backdrop;
use crate::glasscode_ui::VOICE;
use crate::icons;
use crate::shell::View;
use crate::theme::{self, sz};
use crate::ui;
use crate::workspace::{Side, Workspace};

// ----- what voice does ---------------------------------------------------------------------------

/// What voice does now, as the screen names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Connecting,
    Listening,
    Thinking,
    Speaking,
    Muted,
    /// The helper ended its paid session after a quiet time and waits for "Hey Glasscode".
    Asleep,
    Off,
}

impl Mode {
    /// The helper's `mode` while it is `live` or not.
    pub fn from_helper(live: bool, mode: &str) -> Mode {
        if !live {
            return Mode::Off;
        }
        match mode {
            // Live with "idle": the helper started and has not reported yet.
            "connecting" | "idle" => Mode::Connecting,
            "thinking" => Mode::Thinking,
            "speaking" => Mode::Speaking,
            "asleep" => Mode::Asleep,
            _ => Mode::Listening,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Mode::Connecting => "Connecting",
            Mode::Listening => "Listening",
            Mode::Thinking => "Thinking",
            Mode::Speaking => "Speaking",
            Mode::Muted => "Muted",
            Mode::Asleep => "Asleep",
            Mode::Off => "Voice is off",
        }
    }

    /// How much the orb moves (0 to 1) when no microphone level is known: the helper reports
    /// none, so it follows what voice does.
    pub fn level(self) -> f32 {
        match self {
            Mode::Connecting => 0.2,
            Mode::Listening => 0.4,
            Mode::Thinking => 0.15,
            Mode::Speaking => 0.6,
            Mode::Muted | Mode::Asleep => 0.05,
            Mode::Off => 0.,
        }
    }

    /// The sentence under the state before any words came.
    fn waiting(self) -> &'static str {
        match self {
            Mode::Connecting => "Starting voice…",
            Mode::Listening => "Say something.",
            Mode::Thinking | Mode::Speaking => "",
            Mode::Muted => "The microphone is off.",
            Mode::Asleep => "Say “Hey Glasscode” to wake it.",
            Mode::Off => "Voice stopped.",
        }
    }
}

/// What a caption of the helper is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Caption {
    /// What was said.
    Words(String),
    /// What voice did: the helper writes "→ sent to <session>" after it sent a message.
    Action(String),
}

/// The caption `text` as words or an action; nothing for an empty one.
pub fn read_caption(text: &str) -> Option<Caption> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(match text.strip_prefix('→') {
        Some(done) => {
            let done = done.trim();
            let mut chars = done.chars();
            let first = chars
                .next()
                .map(|c| c.to_uppercase().collect::<String>())
                .unwrap_or_default();
            Caption::Action(format!("{first}{}", chars.as_str()))
        }
        None => Caption::Words(text.to_string()),
    })
}

/// The last thing voice did, with the brand of the agent it started, when known.
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub agent: Option<AgentKind>,
    pub text: String,
}

/// What the screen shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    pub mode: Mode,
    /// The orb's level, 0 to 1.
    pub level: f32,
    pub words: Option<String>,
    pub action: Option<Action>,
    /// Mute does something: only for the made-up states, as the helper has no mute.
    pub can_mute: bool,
}

/// The level in steps of a quarter, for the still orb: a new still image comes only when the step
/// changes, not with each sample.
pub fn level_step(level: f32) -> u8 {
    if level.is_nan() {
        return 0;
    }
    (level.clamp(0., 1.) * 4.).round() as u8
}

// ----- the made-up states of the debug hook -------------------------------------------------------

/// How the level of a made-up step moves.
#[derive(Clone, Copy)]
enum Level {
    Fixed(f32),
    /// Around `base`, up to `swing` either way: someone talking.
    Talk {
        base: f32,
        swing: f32,
    },
}

struct Step {
    mode: Mode,
    level: Level,
    /// None keeps the words before.
    words: Option<&'static str>,
    action: Option<(AgentKind, &'static str)>,
    seconds: f32,
}

/// The loop of the debug hook (the mock's `vScript`, with the names of the placeholder project).
const SCRIPT: [Step; 5] = [
    Step {
        mode: Mode::Listening,
        level: Level::Talk {
            base: 0.55,
            swing: 0.4,
        },
        words: Some("“Start a Codex session in acme/app and update its dependencies.”"),
        action: None,
        seconds: 4.,
    },
    Step {
        mode: Mode::Thinking,
        level: Level::Fixed(0.15),
        words: Some("Fetching main in acme/app and making a worktree…"),
        action: None,
        seconds: 2.,
    },
    Step {
        mode: Mode::Speaking,
        level: Level::Fixed(0.6),
        words: Some(
            "Done. Codex works in acme/app on chore/update-deps. Also, Claude still waits for your answer on the login fix.",
        ),
        action: Some((
            AgentKind::Codex,
            "Started Codex · acme/app · chore/update-deps",
        )),
        seconds: 5.5,
    },
    Step {
        mode: Mode::Muted,
        level: Level::Fixed(0.05),
        words: None,
        action: None,
        seconds: 3.,
    },
    Step {
        mode: Mode::Listening,
        level: Level::Talk {
            base: 0.3,
            swing: 0.12,
        },
        words: Some("Say something, or press Esc."),
        action: None,
        seconds: 4.,
    },
];

/// Where the loop is `seconds` after it began: the step, the loops done, the seconds into it.
fn script_at(seconds: f32) -> (usize, u32, f32) {
    let seconds = if seconds.is_finite() {
        seconds.max(0.)
    } else {
        0.
    };
    let total: f32 = SCRIPT.iter().map(|s| s.seconds).sum();
    let loops = (seconds / total).floor();
    let mut into = seconds - loops * total;
    for (i, step) in SCRIPT.iter().enumerate() {
        if into < step.seconds {
            return (i, loops as u32, into);
        }
        into -= step.seconds;
    }
    (
        SCRIPT.len() - 1,
        loops as u32,
        SCRIPT[SCRIPT.len() - 1].seconds,
    )
}

/// What the made-up loop shows `seconds` after it began; `held` is Mute pressed.
fn script_shown(seconds: f32, held: bool) -> Shown {
    let (index, loops, _) = script_at(seconds);
    // The steps from this one back, into the loop before once there was one.
    let back = || {
        (0..SCRIPT.len())
            .map(move |k| (index + SCRIPT.len() - k) % SCRIPT.len())
            .take_while(move |&i| loops > 0 || i <= index)
            .map(|i| &SCRIPT[i])
    };
    let words = back().find_map(|s| s.words).map(str::to_string);
    let action = back().find_map(|s| s.action).map(|(agent, text)| Action {
        agent: Some(agent),
        text: text.to_string(),
    });
    let step = &SCRIPT[index];
    let (mode, level) = if held {
        (Mode::Muted, Mode::Muted.level())
    } else {
        let level = match step.level {
            Level::Fixed(level) => level,
            Level::Talk { base, swing } => {
                base + swing * (seconds * 2.3).sin() * (seconds * 0.9 + 0.6).sin()
            }
        };
        (step.mode, level.clamp(0., 1.))
    };
    Shown {
        mode,
        level,
        words,
        action,
        can_mute: true,
    }
}

/// The made-up loop's clock.
pub struct Play {
    /// When the loop began, moved on by the time it was held muted and by skips.
    began: Instant,
    /// Mute pressed then: the loop waits.
    held: Option<Instant>,
}

impl Play {
    fn new(now: Instant) -> Play {
        Play {
            began: now,
            held: None,
        }
    }

    fn seconds(&self, now: Instant) -> f32 {
        self.held
            .unwrap_or(now)
            .saturating_duration_since(self.began)
            .as_secs_f32()
    }

    fn shown(&self, now: Instant) -> Shown {
        script_shown(self.seconds(now), self.held.is_some())
    }

    /// Mute or Unmute. Unmute in the loop's own Muted step goes on to the next step.
    fn mute(&mut self, now: Instant) {
        if let Some(since) = self.held.take() {
            self.began += now.saturating_duration_since(since);
            return;
        }
        let (index, _, into) = script_at(self.seconds(now));
        if SCRIPT[index].mode == Mode::Muted {
            let rest = Duration::from_secs_f32((SCRIPT[index].seconds - into).max(0.));
            self.began = self.began.checked_sub(rest).unwrap_or(self.began);
        } else {
            self.held = Some(now);
        }
    }
}

// ----- the orb's image -----------------------------------------------------------------------------

/// The orb's side, in logical pixels: the mock's min(16em, 46 %) of a centre `width` x `height`,
/// at most 36 % of the height so that the words fit under it in a low window, at the scale.
pub fn orb_side(width: f32, height: f32, scale: f32) -> f32 {
    (240. * scale)
        .min(width * 0.46)
        .min(height * 0.36)
        .max(72. * scale)
}

/// Texels of the smooth orb (the mock's 112), stretched over its side.
const SMOOTH_TEXELS: u32 = 112;
/// A new orb image while the backdrop moves, at most this often: about one for each of its frames.
const ORB_EVERY: Duration = Duration::from_millis(90);
/// The time of the still orb.
const STILL_SECONDS: f32 = 2.;
/// The backdrop's strength behind the screen: the mock dims Today under the voice screen to about
/// 40 %, so that the orb stands out.
const STRENGTH: f32 = 0.4;
/// A centre narrower than this (logical pixels at 100 %) gets tighter margins and smaller words.
const NARROW: f32 = 360.;

/// What an orb image is made from.
#[derive(Clone, Copy, Debug, PartialEq)]
struct OrbLook {
    preset: Preset,
    finish: Finish,
    light: bool,
    /// The side in device pixels.
    device: u32,
    /// The dither cell in device pixels: the backdrop's 3 logical pixels.
    cell: u32,
    /// The still image's level step; none while the orb moves.
    step: Option<u8>,
}

impl OrbLook {
    /// Texels across the image.
    fn texels(&self) -> u32 {
        match self.finish {
            Finish::Smooth => SMOOTH_TEXELS,
            Finish::Dither => (self.device / self.cell).max(1),
        }
    }

    /// The side on screen, in device pixels: the dither image is painted one pixel to one device
    /// pixel, so it is whole cells.
    fn painted(&self) -> u32 {
        match self.finish {
            Finish::Smooth => self.device,
            Finish::Dither => self.texels() * self.cell,
        }
    }
}

/// The device pixels of the dither cell at the window's `scale_factor` (the backdrop's).
fn dither_cell(scale_factor: f32) -> u32 {
    ((3. * scale_factor).round() as u32).max(1)
}

#[derive(Clone)]
struct Picture {
    image: Arc<RenderImage>,
    look: OrbLook,
}

fn to_image(frame: Frame) -> Option<Arc<RenderImage>> {
    let buffer = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

/// Makes an orb image (on a background thread), with a border of one texel that repeats the
/// clear edge, so that the atlas's next image does not bleed in (U02, proof 2).
fn make(look: OrbLook, level: f32, seconds: f32) -> Option<Picture> {
    let n = look.texels();
    let frame = pixels::orb(look.preset, look.finish, look.light, level, seconds, n);
    let frame = match look.finish {
        Finish::Smooth => pixels::pad(&frame),
        Finish::Dither => pixels::enlarge(&frame, look.cell, n * look.cell, n * look.cell),
    };
    Some(Picture {
        image: to_image(frame)?,
        look,
    })
}

fn drop_image(image: Arc<RenderImage>, window: &mut Window) {
    if let Err(error) = window.drop_image(image) {
        log::warn!("voice: drop_image failed: {error:#}");
    }
}

/// The orb's image on screen, and the one before it, which leaves the atlas when the next one
/// comes (as the backdrop's `Slot`).
#[derive(Default)]
struct Orb {
    now: Option<Picture>,
    old: Option<Arc<RenderImage>>,
    making: bool,
    asked: Option<Instant>,
    /// What the last render wanted.
    wanted: Option<OrbLook>,
}

impl Orb {
    fn set(&mut self, picture: Picture, window: &mut Window) {
        if let Some(old) = self.old.take() {
            drop_image(old, window);
        }
        self.old = self.now.replace(picture).map(|p| p.image);
    }

    fn clear(&mut self, window: &mut Window) {
        for image in [self.old.take(), self.now.take().map(|p| p.image)]
            .into_iter()
            .flatten()
        {
            drop_image(image, window);
        }
        self.asked = None;
        self.wanted = None;
    }
}

// ----- the state ------------------------------------------------------------------------------------

/// The voice screen's state, in `GlasscodeState`.
#[derive(Default)]
pub struct Screen {
    /// The screen is open: it covers Today whenever Today is on screen.
    pub open: bool,
    /// The made-up states of the debug hook; none: the voice helper's state.
    play: Option<Play>,
    /// When the screen opened: the orb's clock.
    began: Option<Instant>,
    /// The last caption read, and the last words and action taken from the captions.
    caption: Option<String>,
    words: Option<String>,
    action: Option<String>,
    /// The helper was live while the screen was open: when it stops, the screen closes.
    was_live: bool,
    /// The made-up step, Mute and the still level step at the last tick: a draw only on a change.
    ticked: Option<(usize, bool, Option<u8>)>,
    focus: Option<FocusHandle>,
    /// The screen takes the keyboard at its next draw, for Esc.
    focus_pending: bool,
    orb: Orb,
}

impl Screen {
    /// Open, fresh: no words, no action, the orb's clock at 0.
    fn start(&mut self, play: Option<Play>) {
        let now = Instant::now();
        if !self.open {
            self.began = Some(now);
            self.caption = None;
            self.words = None;
            self.action = None;
            self.ticked = None;
        }
        self.open = true;
        self.was_live = false;
        self.play = play;
        self.focus_pending = true;
    }
}

// ----- the debug hook --------------------------------------------------------------------------------

/// What a `GLASSCODE_DEBUG_OPEN=voice:…` value asks for.
#[derive(Debug, Default, PartialEq)]
struct Hook {
    choice: Option<BackdropChoice>,
    finish: Option<Finish>,
    theme: Option<ThemeChoice>,
    scale: Option<UiScale>,
}

fn parse_hook(mode: &str) -> Hook {
    let mut hook = Hook::default();
    for word in mode.split(':').skip(1) {
        match word {
            "prism" => hook.choice = Some(BackdropChoice::Prism),
            "aurora" => hook.choice = Some(BackdropChoice::Aurora),
            "dusk" => hook.choice = Some(BackdropChoice::Dusk),
            "none" => hook.choice = Some(BackdropChoice::None),
            "smooth" => hook.finish = Some(Finish::Smooth),
            "dither" => hook.finish = Some(Finish::Dither),
            "light" => hook.theme = Some(ThemeChoice::Light),
            "dark" => hook.theme = Some(ThemeChoice::Dark),
            other => hook.scale = theme::scale_named(other).or(hook.scale),
        }
    }
    hook
}

// ----- the screen ------------------------------------------------------------------------------------

impl Workspace {
    /// The voice screen stands in place of Today's content (`render_today`).
    pub fn voice_shows(&self) -> bool {
        self.glasscode.screen.open && self.view == View::Today
    }

    /// Opens the voice screen over Today, and starts voice (the helper) when it is off.
    pub fn voice_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.view != View::Today {
            self.go(View::Today, window, cx);
        }
        // Images left when voice stopped on its own (no window there to let them go).
        if !self.glasscode.screen.open {
            self.glasscode.screen.orb.clear(window);
        }
        self.glasscode.screen.start(None);
        if VOICE && !self.glasscode.live {
            self.glasscode_toggle(cx);
        }
        cx.notify();
    }

    /// End (and Esc): voice stops (the helper, or the made-up states) and Today shows again.
    pub fn voice_end(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let screen = &mut self.glasscode.screen;
        let made_up = screen.play.take().is_some();
        screen.open = false;
        screen.focus_pending = false;
        screen.orb.clear(window);
        let focused = screen.focus.as_ref().is_some_and(|f| f.is_focused(window));
        if !made_up && self.glasscode.live {
            self.glasscode_toggle(cx);
        }
        if focused {
            window.focus(self.focus_handle_ref(), cx);
        }
        cx.notify();
    }

    /// Mute or Unmute: the made-up states only (the helper has no mute).
    fn voice_mute(&mut self, cx: &mut Context<Self>) {
        if let Some(play) = self.glasscode.screen.play.as_mut() {
            play.mute(Instant::now());
            cx.notify();
        }
    }

    /// The microphone of the status bar: it ends voice while the screen shows, else opens it.
    pub fn voice_mic(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.voice_shows() {
            self.voice_end(window, cx);
        } else {
            self.voice_open(window, cx);
        }
    }

    /// Opens the screen with the made-up states (`GLASSCODE_DEBUG_OPEN=voice…`).
    pub fn open_voice_hook(&mut self, mode: &str, cx: &mut Context<Self>) {
        let hook = parse_hook(mode);
        if hook != Hook::default() {
            self.set_appearance(
                |a| {
                    a.backdrop = hook.choice.unwrap_or(a.backdrop);
                    a.finish = hook.finish.unwrap_or(a.finish);
                    a.theme = hook.theme.unwrap_or(a.theme);
                    a.scale = hook.scale.unwrap_or(a.scale);
                },
                cx,
            );
        }
        self.show_today(cx);
        self.glasscode.screen.start(Some(Play::new(Instant::now())));
        cx.notify();
    }

    /// The orb stands still: no backdrop, less motion, or the window in the back.
    fn voice_still(&self, window_active: bool, cx: &gpui::App) -> bool {
        self.state.appearance.backdrop.preset().is_none() || !window_active || cx.reduce_motion()
    }

    /// From the 250 ms tick of `glasscode_tick`. The made-up loop draws only when its step, Mute
    /// or (for the still orb) the level step changes; the screen closes when the helper stops.
    pub(crate) fn voice_tick(&mut self, cx: &mut Context<Self>) {
        if !self.glasscode.screen.open {
            return;
        }
        let still = self.voice_still(self.window_active, cx);
        let live = self.glasscode.live;
        let screen = &mut self.glasscode.screen;
        match &screen.play {
            Some(play) => {
                let now = Instant::now();
                let (index, _, _) = script_at(play.seconds(now));
                let shown = play.shown(now);
                let seen = (
                    index,
                    play.held.is_some(),
                    still.then(|| level_step(shown.level)),
                );
                if screen.ticked != Some(seen) {
                    screen.ticked = Some(seen);
                    cx.notify();
                }
            }
            None if live => screen.was_live = true,
            None => {
                if screen.was_live {
                    // The helper ended (its session closed, or it could not start).
                    screen.open = false;
                    screen.was_live = false;
                    self.show_toast("Voice ended.".to_string(), cx);
                    cx.notify();
                }
            }
        }
    }

    /// What the screen shows now. The helper's captions are read here: each new one is the last
    /// words or the last action.
    fn voice_shown(&mut self, now: Instant) -> Shown {
        if let Some(play) = &self.glasscode.screen.play {
            return play.shown(now);
        }
        let caption = self.glasscode.caption.clone();
        let screen = &mut self.glasscode.screen;
        if screen.caption != caption {
            match caption.as_deref().and_then(read_caption) {
                Some(Caption::Words(words)) => screen.words = Some(words),
                Some(Caption::Action(action)) => screen.action = Some(action),
                None => {}
            }
            screen.caption = caption;
        }
        let mode = Mode::from_helper(self.glasscode.live, &self.glasscode.mode);
        Shown {
            mode,
            level: mode.level(),
            words: screen.words.clone(),
            action: screen
                .action
                .clone()
                .map(|text| Action { agent: None, text }),
            can_mute: false,
        }
    }

    /// The orb's image for this draw, asking for the next one when it is due.
    fn voice_orb(
        &mut self,
        level: f32,
        side: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Option<Picture>, OrbLook) {
        let appearance = self.state.appearance;
        let scale_factor = window.scale_factor();
        let still = self.voice_still(window.is_window_active(), cx) || !window.is_visible();
        let look = OrbLook {
            preset: appearance.backdrop.preset().unwrap_or(Preset::Prism),
            finish: appearance.finish,
            light: theme::is_light(),
            device: (side * scale_factor).round().max(1.) as u32,
            cell: dither_cell(scale_factor),
            step: still.then(|| level_step(level)),
        };
        let seconds = self
            .glasscode
            .screen
            .began
            .map_or(0., |at| at.elapsed().as_secs_f32());
        let orb = &mut self.glasscode.screen.orb;
        orb.wanted = Some(look);
        let shown = orb.now.as_ref().map(|p| p.look);
        let due = !orb.making
            && (shown != Some(look)
                || (!still && orb.asked.is_none_or(|at| at.elapsed() >= ORB_EVERY)));
        if due {
            orb.making = true;
            orb.asked = Some(Instant::now());
            // A still image, or the first one, draws at once; a moving one waits for the next
            // backdrop frame.
            let draw = still || shown.is_none();
            let (level, seconds) = if still {
                (look.step.unwrap_or(0) as f32 / 4., STILL_SECONDS)
            } else {
                (level, seconds)
            };
            let task = cx.background_spawn(async move { make(look, level, seconds) });
            cx.spawn_in(window, async move |this, cx| {
                let made = task.await;
                this.update_in(cx, |this, window, cx| {
                    this.voice_orb_made(made, draw, window, cx)
                })
                .ok();
            })
            .detach();
        }
        (self.glasscode.screen.orb.now.clone(), look)
    }

    fn voice_orb_made(
        &mut self,
        made: Option<Picture>,
        draw: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let screen = &mut self.glasscode.screen;
        screen.orb.making = false;
        let Some(picture) = made else {
            log::warn!("voice: an orb image could not be made");
            return;
        };
        if !screen.open {
            drop_image(picture.image, window);
            return;
        }
        // The look changed while it was made (still now, a new size): draw, so that the render
        // asks for the right one.
        let draw = draw || screen.orb.wanted != Some(picture.look);
        screen.orb.set(picture, window);
        if draw {
            cx.notify();
        }
    }

    /// The width and height of the centre, in logical pixels: the window less the sidebars, the
    /// title bar and the status bar.
    fn voice_centre(&self, window: &Window) -> (f32, f32) {
        let mut width = self.window_width;
        if self.view_has_left() && !self.state.layout.left_hidden {
            width -= self.shown_width(Side::Left);
        }
        if self.view_has_right() && self.show_right {
            width -= self.shown_width(Side::Right);
        }
        let height = f32::from(window.viewport_size().height)
            - f32::from(sz(crate::shell::TITLE_HEIGHT))
            - f32::from(sz(crate::status_bar::BAR_HEIGHT));
        (width.max(0.), height.max(0.))
    }

    /// The voice screen, in place of Today's content.
    pub fn render_voice(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let layer = self.backdrop_layer(STRENGTH, window, cx);
        let shown = self.voice_shown(Instant::now());
        let (width, height) = self.voice_centre(window);
        let side = orb_side(width, height, theme::scale());
        let (picture, look) = self.voice_orb(shown.level, side, window, cx);
        let focus = self
            .glasscode
            .screen
            .focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        if std::mem::take(&mut self.glasscode.screen.focus_pending) {
            window.focus(&focus, cx);
        }
        let scroll = window
            .use_keyed_state("voice-scroll", cx, |_, _| ScrollHandle::new())
            .read(cx)
            .clone();

        // A narrow centre (900 px at 130 % with both sidebars) gives the words the room.
        let narrow = width / theme::scale() < NARROW;
        // Auto margins centre the column when it fits; when it does not, it starts at the top
        // and scrolls, where `justify_center` would push the orb out of reach above.
        let column = div()
            .w_full()
            .my_auto()
            .flex()
            .flex_col()
            .items_center()
            .gap(sz(if narrow { 12. } else { 18. }))
            .child(orb_element(picture, look, window.scale_factor()))
            .child(self.voice_card(&shown, narrow, cx));
        let content = div()
            .w_full()
            .min_h_full()
            .flex()
            .flex_col()
            .px(sz(if narrow { 10. } else { 22. }))
            .py(sz(if narrow { 14. } else { 24. }))
            .child(column);
        div()
            .id("voice-screen")
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme::bg())
            .text_color(theme::text())
            .track_focus(&focus)
            .key_context("VoiceScreen")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.voice_end(window, cx);
                }
            }))
            // A click on the screen gives it the keyboard back, for Esc.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    if let Some(focus) = this.glasscode.screen.focus.as_ref() {
                        window.focus(focus, cx);
                    }
                }),
            )
            .child(layer)
            .child(ui::scroll_area("voice-scroll", &scroll, content))
            .into_any_element()
    }

    /// Under the orb, on strong glass: the state, the last words, the last action, Mute and End.
    fn voice_card(&self, shown: &Shown, narrow: bool, cx: &mut Context<Self>) -> Div {
        let words = shown
            .words
            .clone()
            .unwrap_or_else(|| shown.mode.waiting().to_string());
        let muted = shown.mode == Mode::Muted;
        let action = shown.action.as_ref().map(|action| {
            div()
                .flex()
                .flex_none()
                .max_w_full()
                .items_center()
                .gap(sz(7.))
                .px(sz(11.))
                .py(sz(5.))
                .rounded(sz(14.))
                .border_1()
                .border_color(theme::border())
                .bg(theme::tint())
                .text_size(sz(12.5))
                .text_color(theme::text_soft())
                .children(
                    action
                        .agent
                        .map(|agent| icons::brand(icons::agent_brand(agent), 14. * theme::scale())),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(action.text.clone()),
                )
        });
        let buttons = div()
            .flex()
            .flex_wrap()
            .justify_center()
            .gap(sz(10.))
            .when(shown.can_mute, |d| {
                d.child(
                    mute_button(muted)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.voice_mute(cx))),
                )
            })
            .child(ui::primary_button("voice-end", "End voice").on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| this.voice_end(window, cx)),
            ));
        let hint = div()
            .flex()
            .items_center()
            .gap(sz(6.))
            .text_size(sz(11.5))
            .text_color(theme::card_soft_text())
            .child(ui::kbd("Esc"))
            .child("to leave");
        backdrop::glass_strong(sz(16.))
            .when(theme::is_light(), |d| d.shadow_md())
            .flex()
            .flex_col()
            .items_center()
            .gap(sz(12.))
            .w_full()
            .max_w(sz(540.))
            .px(sz(if narrow { 12. } else { 22. }))
            .pt(sz(16.))
            .pb(sz(14.))
            .child(
                div()
                    .text_size(sz(11.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::card_soft_text())
                    .child(shown.mode.word().to_uppercase()),
            )
            .child(
                div()
                    .w_full()
                    .min_h(sz(48.))
                    .text_center()
                    .text_size(sz(if narrow { 15. } else { 17. }))
                    .line_height(sz(if narrow { 21. } else { 24. }))
                    .text_color(theme::text())
                    .child(words),
            )
            .children(action)
            .child(buttons)
            .child(hint)
    }
}

/// Mute, or Unmute while muted, with the microphone (struck through for Mute).
fn mute_button(muted: bool) -> Stateful<Div> {
    let (glyph, label) = if muted {
        (icons::MIC, "Unmute")
    } else {
        (icons::MIC_OFF, "Mute")
    };
    div()
        .id("voice-mute")
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(sz(7.))
        .h(sz(ui::BUTTON_HEIGHT))
        .px(sz(12.))
        .rounded(sz(8.))
        .border_1()
        .border_color(theme::border_strong())
        .bg(theme::raised())
        .text_size(sz(13.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text())
        .cursor_pointer()
        .hover(|s| s.bg(theme::hover()))
        .child(icons::icon(glyph, sz(14.), sz(14.), theme::text_soft()))
        .child(label)
}

/// The orb: its image painted over a square of its side. The side came from the window's size at
/// the scale already, so it is `px`, not `sz`.
fn orb_element(picture: Option<Picture>, look: OrbLook, scale_factor: f32) -> AnyElement {
    let side = look.painted() as f32 / scale_factor;
    div()
        .flex_none()
        .size(px(side))
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    let Some(picture) = picture else { return };
                    let (bounds, image_bounds) = orb_bounds(bounds, &picture.look, scale_factor);
                    if let Err(error) = window.paint_image(
                        bounds,
                        image_bounds,
                        Corners::default(),
                        picture.image,
                        0,
                        false,
                    ) {
                        log::warn!("voice: paint_image failed: {error:#}");
                    }
                },
            )
            .size_full(),
        )
        .into_any_element()
}

/// Where the orb paints in its square `bounds`, and where the whole image goes: its border of one
/// texel lies outside. A dither image starts on a device pixel, so that each cell stays sharp. An
/// image made for an older size (for one frame after a resize) is stretched over the square.
fn orb_bounds(
    bounds: Bounds<Pixels>,
    look: &OrbLook,
    scale_factor: f32,
) -> (Bounds<Pixels>, Bounds<Pixels>) {
    let side = f32::from(bounds.size.width);
    let snap = |v: Pixels| px((f32::from(v) * scale_factor).round() / scale_factor);
    let origin = match look.finish {
        Finish::Smooth => bounds.origin,
        Finish::Dither => point(snap(bounds.origin.x), snap(bounds.origin.y)),
    };
    let area = Bounds::new(origin, size(px(side), px(side)));
    // The dither image's texels are its pixels.
    let across = match look.finish {
        Finish::Smooth => look.texels(),
        Finish::Dither => look.painted(),
    };
    let texel = side / across.max(1) as f32;
    let image = Bounds::new(
        point(origin.x - px(texel), origin.y - px(texel)),
        size(px(side + 2. * texel), px(side + 2. * texel)),
    );
    (area, image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_helpers_mode_names_the_state() {
        assert_eq!(
            Mode::from_helper(false, "speaking"),
            Mode::Off,
            "not live is off, whatever the mode"
        );
        assert_eq!(Mode::from_helper(true, "connecting"), Mode::Connecting);
        assert_eq!(
            Mode::from_helper(true, "idle"),
            Mode::Connecting,
            "live before the helper reports"
        );
        assert_eq!(Mode::from_helper(true, "listening"), Mode::Listening);
        assert_eq!(Mode::from_helper(true, "thinking"), Mode::Thinking);
        assert_eq!(Mode::from_helper(true, "speaking"), Mode::Speaking);
        assert_eq!(Mode::from_helper(true, "asleep"), Mode::Asleep);
        assert_eq!(Mode::from_helper(true, "something new"), Mode::Listening);
    }

    #[test]
    fn the_level_follows_the_state() {
        let modes = [
            Mode::Connecting,
            Mode::Listening,
            Mode::Thinking,
            Mode::Speaking,
            Mode::Muted,
            Mode::Asleep,
            Mode::Off,
        ];
        for mode in modes {
            assert!((0. ..=1.).contains(&mode.level()), "{mode:?}");
            assert!(!mode.word().is_empty());
        }
        assert!(Mode::Speaking.level() > Mode::Listening.level());
        assert!(Mode::Listening.level() > Mode::Thinking.level());
        assert!(Mode::Thinking.level() > Mode::Muted.level());
        assert_eq!(Mode::Off.level(), 0.);
    }

    #[test]
    fn a_caption_is_words_or_an_action() {
        assert_eq!(read_caption(""), None);
        assert_eq!(read_caption("   "), None);
        assert_eq!(
            read_caption("Codex is on it."),
            Some(Caption::Words("Codex is on it.".into()))
        );
        assert_eq!(
            read_caption("→ sent to Fix the login"),
            Some(Caption::Action("Sent to Fix the login".into()))
        );
        assert_eq!(read_caption("→"), Some(Caption::Action(String::new())));
        assert_eq!(
            read_caption("→ ünïcode"),
            Some(Caption::Action("Ünïcode".into()))
        );
    }

    #[test]
    fn the_level_moves_in_quarter_steps() {
        assert_eq!(level_step(0.), 0);
        assert_eq!(level_step(0.12), 0);
        assert_eq!(level_step(0.13), 1);
        assert_eq!(level_step(0.5), 2);
        assert_eq!(level_step(1.), 4);
        assert_eq!(level_step(7.), 4);
        assert_eq!(level_step(-1.), 0);
        assert_eq!(level_step(f32::NAN), 0);
    }

    #[test]
    fn the_loop_plays_each_state_and_comes_round() {
        let total: f32 = SCRIPT.iter().map(|s| s.seconds).sum();
        let modes: Vec<Mode> = [0.5, 4.5, 6.5, 12.5, 15.5]
            .iter()
            .map(|&s| script_shown(s, false).mode)
            .collect();
        assert_eq!(
            modes,
            [
                Mode::Listening,
                Mode::Thinking,
                Mode::Speaking,
                Mode::Muted,
                Mode::Listening
            ]
        );
        assert_eq!(
            script_shown(total + 0.5, false).mode,
            Mode::Listening,
            "round again"
        );
        for seconds in [-3., f32::NAN, f32::INFINITY] {
            assert_eq!(script_at(seconds).0, 0, "{seconds}");
        }
        // The action shows from the step that did it, and stays in the next loop.
        assert_eq!(script_shown(1., false).action, None);
        let action = script_shown(7., false).action.expect("an action");
        assert_eq!(action.agent, Some(AgentKind::Codex));
        assert_eq!(script_shown(total + 1., false).action, Some(action));
        // The Muted step keeps the words before it.
        assert_eq!(
            script_shown(12.5, false).words,
            script_shown(10., false).words
        );
        assert!(script_shown(0., false).can_mute);
    }

    #[test]
    fn listening_moves_the_level() {
        let levels: Vec<f32> = (0..16)
            .map(|i| script_shown(i as f32 * 0.25, false).level)
            .collect();
        assert!(levels.iter().all(|l| (0. ..=1.).contains(l)), "{levels:?}");
        let (low, high) = levels
            .iter()
            .fold((1f32, 0f32), |(lo, hi), l| (lo.min(*l), hi.max(*l)));
        assert!(high - low > 0.3, "{levels:?}");
        // Thinking stands at its level.
        assert_eq!(
            script_shown(4.5, false).level,
            script_shown(5.5, false).level
        );
    }

    #[test]
    fn mute_holds_the_loop_and_unmute_goes_on() {
        let start = Instant::now();
        let at = |s: f32| start + Duration::from_secs_f32(s);
        let mut play = Play::new(start);
        play.mute(at(1.));
        for s in [1., 5., 30.] {
            let shown = play.shown(at(s));
            assert_eq!(shown.mode, Mode::Muted, "held at {s} s");
            assert_eq!(shown.level, Mode::Muted.level());
            assert_eq!(shown.words, script_shown(1., false).words, "the words stay");
        }
        play.mute(at(30.));
        assert!(
            (play.seconds(at(30.5)) - 1.5).abs() < 0.01,
            "the loop goes on where it was"
        );
        assert_eq!(play.shown(at(30.5)).mode, Mode::Listening);
        // In the loop's own Muted step, Unmute goes on to the next step.
        let mut play = Play::new(start);
        assert_eq!(play.shown(at(12.)).mode, Mode::Muted);
        play.mute(at(12.));
        assert!(play.held.is_none());
        assert_eq!(play.shown(at(12.1)).mode, Mode::Listening);
    }

    #[test]
    fn the_orb_fits_the_centre() {
        // Wide: the mock's 16em at the scale.
        assert_eq!(orb_side(1400., 900., 1.), 240.);
        assert_eq!(orb_side(1400., 900., 1.3), 312.);
        // 900 x 560 at 130 %: a third of the low centre.
        let low = orb_side(380., 469., 1.3);
        assert!(low <= 469. * 0.36 && low >= 72. * 1.3, "{low}");
        // Narrow: 46 % of the width.
        assert!((orb_side(300., 900., 1.) - 138.).abs() < 0.01);
        // Never below the least.
        assert_eq!(orb_side(0., 0., 1.), 72.);
    }

    #[test]
    fn the_dither_orb_is_whole_cells_on_device_pixels() {
        assert_eq!(dither_cell(1.), 3);
        assert_eq!(dither_cell(1.5), 5);
        assert_eq!(dither_cell(2.), 6);
        assert_eq!(dither_cell(0.1), 1);
        let look = OrbLook {
            preset: Preset::Prism,
            finish: Finish::Dither,
            light: false,
            device: 361,
            cell: 5,
            step: None,
        };
        assert_eq!((look.texels(), look.painted()), (72, 360));
        let smooth = OrbLook {
            finish: Finish::Smooth,
            ..look
        };
        assert_eq!((smooth.texels(), smooth.painted()), (SMOOTH_TEXELS, 361));
        // The dither square starts on a device pixel; its border is one device pixel.
        let near = |a: Pixels, b: f32| (f32::from(a) - b).abs() < 0.001;
        let bounds = Bounds::new(point(px(100.3), px(50.6)), size(px(240.), px(240.)));
        let (area, image) = orb_bounds(bounds, &look, 1.5);
        assert!(
            near(area.origin.x, 100.) && near(area.origin.y, 76. / 1.5),
            "{area:?}"
        );
        assert!(near(area.size.width, 240.) && near(area.size.height, 240.));
        assert!(near(area.origin.x - image.origin.x, 1. / 1.5), "{image:?}");
        // Smooth: the border is one texel.
        let bounds = Bounds::new(point(px(10.), px(20.)), size(px(361.), px(361.)));
        let (area, image) = orb_bounds(bounds, &smooth, 1.);
        assert_eq!(area.origin, bounds.origin);
        assert!(
            near(image.size.width, 361. + 2. * 361. / SMOOTH_TEXELS as f32),
            "{image:?}"
        );
    }

    #[test]
    fn the_hook_names_a_look() {
        assert_eq!(parse_hook("voice"), Hook::default());
        assert_eq!(
            parse_hook("voice:dusk:dither:light:130"),
            Hook {
                choice: Some(BackdropChoice::Dusk),
                finish: Some(Finish::Dither),
                theme: Some(ThemeChoice::Light),
                scale: Some(UiScale::P130)
            }
        );
        assert_eq!(
            parse_hook("voice:none:pink").choice,
            Some(BackdropChoice::None)
        );
    }
}
