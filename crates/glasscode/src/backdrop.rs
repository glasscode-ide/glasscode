//! The backdrop behind a screen, and the glass cards on it (slice U04).
//!
//! A screen puts `Workspace::backdrop_layer` at its back. The layer is an entity of its own,
//! `Backdrop`, that paints the newest frame. Frames are made on a background thread
//! (`glasscode_core::backdrop`), 10 each second for the smooth finish, timed against a clock.
//! A glass card (`glass`, `glass_strong`) paints a blurred copy of the same frame, cropped to the
//! card with its rounded corners, and the glass colour, edge and light on it: GPUI has no
//! backdrop blur, but the only thing behind a card is our backdrop. `docs/ui-redesign-proofs.md`
//! has the measurements behind this method.
//!
//! Frames stop when the window is not active or not shown, when the system asks for less motion,
//! and with the choice None; the frame on screen then stays. No frames are made while no layer is
//! painted.
//!
//! Each new frame draws the window again, and GPUI draws a window from its root view, which is
//! the `Workspace`: so each frame runs `Workspace::render` once (`vendor/gpui/src/window.rs`,
//! `draw_roots`). The entity of its own keeps the work of a frame out of the `Workspace`, but not
//! that draw.
//!
//! `GLASSCODE_DEBUG_OPEN=backdrop[:<words>]` opens a page with the backdrop and three cards, in a
//! debug build. Words: `prism`, `aurora`, `dusk` or `none`; `smooth` or `dither`; `light` or
//! `dark` (these are saved, as the Themes page saves them); `still` (one frame, the baseline of a
//! measurement); `bare` (no cards).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, AppContext as _, BorderStyle, Bounds, ClickEvent, Context, Corners, Div, Entity,
    FontWeight, Hsla, Pixels, Render, RenderImage, SharedString, Size, Subscription, Window,
    canvas, div, fill, point, prelude::*, px, quad, size,
};
use glasscode_core::backdrop::{self as pixels, Finish, Frame, Preset};
use glasscode_core::model::{Appearance, BackdropChoice, ThemeChoice};

use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

/// Frames each second of the smooth backdrop (U02 and the owner, 2026-11-16).
const SMOOTH_FPS: u64 = 10;
/// The dither finish paints a full-size image (decision P6); U02 measured it at 8 frames each
/// second, and above `LARGE` device pixels at half that.
const DITHER_FPS: u64 = 8;
const DITHER_FPS_LARGE: u64 = 4;
const LARGE: u32 = 4_000_000;
/// The glass's blurred copy is made at half the texels of the backdrop, for each second frame:
/// U02's simple blur at full size cost 4.6 ms a frame.
const BLUR_EVERY: u32 = 2;
/// Three box passes of radius 2 at half size: about the mock's `blur(28px)`.
const BLUR_RADIUS: u32 = 2;
/// Ticks without a painted layer before the frames stop and the images go.
const UNPAINTED_STOP: u32 = 3;

/// What a frame is made from. A new frame is needed when this changes, and each tick while the
/// backdrop moves.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Look {
    preset: Preset,
    finish: Finish,
    light: bool,
    strength: f32,
    /// The layer's size in logical pixels, and the window's scale factor.
    area: Size<Pixels>,
    scale: f32,
}

impl Look {
    /// The smooth backdrop's texels: the area / 6, at most 320 x 200.
    fn texels(&self) -> (u32, u32) {
        pixels::texels_for(f32::from(self.area.width), f32::from(self.area.height))
    }

    /// The area in device pixels, for the dither finish.
    fn device(&self) -> (u32, u32) {
        let side = |v: Pixels| (f32::from(v) * self.scale).round().max(0.) as u32;
        (side(self.area.width), side(self.area.height))
    }

    /// The mock's dither cell of 3 pixels, in whole device pixels so that each cell is the same
    /// size on screen.
    fn cell(&self) -> u32 {
        ((3. * self.scale).round() as u32).max(1)
    }

    fn drawable(&self) -> bool {
        let (w, h) = self.device();
        w > 0 && h > 0
    }

    fn interval(&self) -> Duration {
        let fps = match self.finish {
            Finish::Smooth => SMOOTH_FPS,
            Finish::Dither => {
                let (w, h) = self.device();
                if w.saturating_mul(h) > LARGE { DITHER_FPS_LARGE } else { DITHER_FPS }
            }
        };
        Duration::from_millis(1000 / fps)
    }
}

/// An image made from a frame with a border of one texel, and its texels without the border.
#[derive(Clone)]
struct Picture {
    image: Arc<RenderImage>,
    texels: (u32, u32),
}

/// One frame's images, made on a background thread.
struct Made {
    look: Look,
    smooth: Picture,
    /// The blurred copy for glass, when this frame made one.
    blurred: Option<Picture>,
    took: Duration,
}

fn to_image(frame: Frame) -> Option<Arc<RenderImage>> {
    let buffer = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

/// Makes the images of one frame (on a background thread).
fn make(look: Look, seconds: f32, with_blur: bool) -> Option<Made> {
    let begun = Instant::now();
    let Look { preset, finish, light, strength, .. } = look;
    let smooth = match finish {
        Finish::Smooth => {
            let (w, h) = look.texels();
            let frame = pixels::render(preset, finish, light, strength, seconds, w, h);
            Picture { image: to_image(pixels::pad(&frame))?, texels: (w, h) }
        }
        Finish::Dither => {
            // One texel for each dither cell, then each cell as a square of device pixels: the
            // image is painted one pixel to one device pixel.
            let ((w, h), cell) = (look.device(), look.cell());
            let frame = pixels::render(preset, finish, light, strength, seconds, w.div_ceil(cell), h.div_ceil(cell));
            Picture { image: to_image(pixels::enlarge(&frame, cell, w, h))?, texels: (w, h) }
        }
    };
    // Always from a small smooth frame: a blur of the dither frame is nearly the same, at many
    // times the cost.
    let blurred = if with_blur {
        let (w, h) = look.texels();
        let (w, h) = ((w / 2).max(2), (h / 2).max(2));
        let frame = pixels::render(preset, Finish::Smooth, light, strength, seconds, w, h);
        Some(Picture { image: to_image(pixels::pad(&pixels::blur(&frame, BLUR_RADIUS)))?, texels: (w, h) })
    } else {
        None
    };
    Some(Made { look, smooth, blurred, took: begun.elapsed() })
}

/// Where the whole image goes so that its texels without the border cover `area`.
fn image_bounds(area: Bounds<Pixels>, texels: (u32, u32)) -> Bounds<Pixels> {
    let tx = area.size.width / texels.0.max(1) as f32;
    let ty = area.size.height / texels.1.max(1) as f32;
    Bounds::new(
        point(area.origin.x - tx, area.origin.y - ty),
        size(area.size.width + tx * 2., area.size.height + ty * 2.),
    )
}

/// A picture on screen, and the one before it. The one before leaves the atlas when the next
/// one comes, after the current one was painted: the atlas texture then always holds a tile and
/// is not made again for each frame (U02, proof 1).
#[derive(Default)]
struct Slot {
    now: Option<Picture>,
    old: Option<Arc<RenderImage>>,
}

impl Slot {
    fn set(&mut self, picture: Picture, window: &mut Window) {
        if let Some(old) = self.old.take() {
            drop_image(old, window);
        }
        self.old = self.now.replace(picture).map(|p| p.image);
    }

    fn clear(&mut self, window: &mut Window) {
        for image in [self.old.take(), self.now.take().map(|p| p.image)].into_iter().flatten() {
            drop_image(image, window);
        }
    }
}

fn drop_image(image: Arc<RenderImage>, window: &mut Window) {
    if let Err(error) = window.drop_image(image) {
        log::warn!("backdrop: drop_image failed: {error:#}");
    }
}

/// The blurred copy of the frame on screen and where it lies, for the glass cards of this frame.
/// The layer sets it in its prepaint, before any card paints.
#[derive(Clone)]
struct Layer {
    image: Arc<RenderImage>,
    image_bounds: Bounds<Pixels>,
}

thread_local! {
    static LAYER: RefCell<Option<Layer>> = const { RefCell::new(None) };
}

fn set_layer(layer: Option<Layer>) {
    LAYER.with(|current| *current.borrow_mut() = layer);
}

/// Numbers for the debug page and the log.
#[derive(Default)]
struct Counts {
    since: Option<Instant>,
    /// Frames made, blurred copies made, the time spent making them.
    made: u64,
    blurred: u64,
    making: Duration,
    /// Draws of the window that painted the layer.
    painted: Rc<Cell<u64>>,
    /// Calls of `backdrop_layer`, which is one for each `Workspace::render`.
    renders: u64,
    logged: Option<Instant>,
    /// Why the frames stand still, when they do.
    still: &'static str,
}

impl Counts {
    fn lines(&self, look: Option<Look>) -> Vec<String> {
        let seconds = self.since.map_or(0., |s| s.elapsed().as_secs_f32()).max(0.001);
        let rate = |n: u64| n as f32 / seconds;
        let average = if self.made == 0 { 0. } else { self.making.as_secs_f32() * 1000. / self.made as f32 };
        let image = match look {
            Some(look) if look.finish == Finish::Dither => {
                let (w, h) = look.device();
                format!("{w} x {h} pixels (dither, cell {}), {:.1} frames each second wanted", look.cell(), 1. / look.interval().as_secs_f32())
            }
            Some(look) => {
                let (w, h) = look.texels();
                format!("{w} x {h} texels (smooth), {:.0} frames each second wanted", 1. / look.interval().as_secs_f32())
            }
            None => "no backdrop".into(),
        };
        vec![
            image,
            format!(
                "Made {} ({:.1}/s), blurred {} ({:.1}/s), painted {:.1}/s, Workspace renders {:.1}/s",
                self.made,
                rate(self.made),
                self.blurred,
                rate(self.blurred),
                rate(self.painted.get()),
                rate(self.renders)
            ),
            format!(
                "Making a frame: {average:.2} ms on average, running {seconds:.0} s{}",
                if self.still.is_empty() { String::new() } else { format!(", still: {}", self.still) }
            ),
        ]
    }
}

/// The layer behind a screen: paints the newest frame, and makes the next ones.
pub struct Backdrop {
    /// The clock of the movement.
    started: Instant,
    /// What the last render asked for; none: nothing to draw (the choice None).
    want: Option<Look>,
    /// The frames may move now.
    moving: bool,
    smooth: Slot,
    blurred: Slot,
    /// The look of the frame on screen.
    shown: Option<Look>,
    /// A frame is being made.
    making: bool,
    /// The clock runs.
    running: bool,
    /// Frames made since the last blurred copy.
    since_blur: u32,
    /// The layer was painted since the last tick (its paint sets it).
    painted: Rc<Cell<bool>>,
    /// The layer's bounds at its last prepaint.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    unpainted: u32,
    counts: Counts,
    _subscriptions: Vec<Subscription>,
}

impl Backdrop {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // A window that comes to the front or back into sight draws again, and the render
        // decides whether the frames move.
        let activation = cx.observe_window_activation(window, |_, _, cx| cx.notify());
        let this = cx.weak_entity();
        let visibility = window.observe_window_visibility(move |_, _, cx| {
            this.update(cx, |_, cx| cx.notify()).ok();
        });
        Self {
            started: Instant::now(),
            want: None,
            moving: false,
            smooth: Slot::default(),
            blurred: Slot::default(),
            shown: None,
            making: false,
            running: false,
            since_blur: 0,
            painted: Rc::new(Cell::new(false)),
            bounds: Rc::new(Cell::new(None)),
            unpainted: 0,
            counts: Counts::default(),
            _subscriptions: vec![activation, visibility],
        }
    }

    /// What the frames are made from now (each render of the layer).
    fn wants(&mut self, appearance: Appearance, strength: f32, still: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.counts.renders += 1;
        self.counts.since.get_or_insert_with(Instant::now);
        let Some(preset) = appearance.backdrop.preset() else {
            self.want = None;
            self.moving = false;
            self.counts.still = "the choice None";
            self.clear(window);
            return;
        };
        let area = self.bounds.get().map_or(window.viewport_size(), |b| b.size);
        let look = Look { preset, finish: appearance.finish, light: theme::is_light(), strength, area, scale: window.scale_factor() };
        self.want = Some(look);
        self.moving = match still_reason(still, window, cx) {
            Some(reason) => {
                self.counts.still = if reason.is_empty() { "the debug page" } else { reason };
                false
            }
            None => {
                self.counts.still = "";
                true
            }
        };
        // A new look, a new size, or the first frame: one frame now, moving or not.
        if self.shown != Some(look) && !self.making {
            self.make_frame(window, cx);
        }
        if self.moving && !self.running {
            self.start(window, cx);
        }
    }

    fn make_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(look) = self.want.filter(Look::drawable) else { return };
        self.making = true;
        let seconds = self.started.elapsed().as_secs_f32();
        let with_blur = self.blurred.now.is_none() || self.shown != Some(look) || self.since_blur + 1 >= BLUR_EVERY;
        let task = cx.background_spawn(async move { make(look, seconds, with_blur) });
        cx.spawn_in(window, async move |this, cx| {
            let made = task.await;
            this.update_in(cx, |this, window, cx| this.show(made, window, cx)).ok();
        })
        .detach();
    }

    fn show(&mut self, made: Option<Made>, window: &mut Window, cx: &mut Context<Self>) {
        self.making = false;
        let Some(made) = made else {
            log::warn!("backdrop: a frame could not be made");
            return;
        };
        // The choice became None while it was being made.
        if self.want.is_none() {
            return;
        }
        self.counts.made += 1;
        self.counts.making += made.took;
        self.smooth.set(made.smooth, window);
        match made.blurred {
            Some(blurred) => {
                self.counts.blurred += 1;
                self.since_blur = 0;
                self.blurred.set(blurred, window);
            }
            None => self.since_blur += 1,
        }
        self.shown = Some(made.look);
        cx.notify();
    }

    /// Starts the clock: a frame each interval, against the clock and not "wait, then work",
    /// because a Windows timer ends on a tick of about 15.6 ms (U02, proof 1).
    ///
    /// Cost (U04, optimized build, the debug page in the main window with an empty home): the
    /// smooth backdrop at 10 frames each second uses about 4.2 to 4.9 % of one core more than the
    /// same page standing still. Making a frame is about 1 ms on a background thread; the rest
    /// is the draw. GPUI draws a window from its root view, and that is the `Workspace`, so each
    /// new frame runs `Workspace::render` (`draw_roots` in `vendor/gpui/src/window.rs`). The
    /// owner's limit is 6 % above the still page while the window is active. Wave 5 measures it
    /// again on the real Today screen; above 8 % there, a slice of its own changes the window so
    /// that a frame no longer draws the `Workspace` (a small root, with the `Workspace` cached).
    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.running = true;
        self.unpainted = 0;
        cx.spawn_in(window, async move |this, cx| {
            let mut next = Instant::now();
            loop {
                let Ok(Some(every)) = this.update(cx, |this, _| this.want.map(|look| look.interval())) else { break };
                next += every;
                let now = Instant::now();
                if next <= now {
                    // Late (a slow frame): skip to the next slot, so that frames do not come back
                    // to back and leave the window no time to draw them.
                    next = now + every;
                }
                cx.background_executor().timer(next.saturating_duration_since(now)).await;
                match this.update_in(cx, |this, window, cx| this.tick(window, cx)) {
                    Ok(true) => {}
                    _ => break,
                }
            }
        })
        .detach();
    }

    /// One slot of the clock; false stops it.
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let painted = self.painted.replace(false);
        if !painted {
            if self.making {
                // The frame is not ready yet: skip this slot.
                return true;
            }
            self.unpainted += 1;
            if self.unpainted < UNPAINTED_STOP {
                return true;
            }
            // No layer in the window now: no frames, and the images go.
            self.running = false;
            self.counts.still = "no layer";
            self.clear(window);
            return false;
        }
        self.unpainted = 0;
        self.log(false);
        // A render since the last slot may have stopped the movement (and said why).
        if let Some(reason) = still_reason(false, window, cx) {
            self.moving = false;
            self.counts.still = reason;
        }
        if !self.moving || self.want.is_none() {
            self.running = false;
            self.log(true);
            return false;
        }
        if !self.making {
            self.make_frame(window, cx);
        }
        true
    }

    /// Writes the numbers to the log each 30 seconds while the frames move (for measurements).
    fn log(&mut self, force: bool) {
        let due = self.counts.logged.is_none_or(|at| at.elapsed() >= Duration::from_secs(30));
        if force || due {
            self.counts.logged = Some(Instant::now());
            log::warn!("backdrop: {}", self.counts.lines(self.want).join(" · "));
        }
    }

    fn clear(&mut self, window: &mut Window) {
        self.smooth.clear(window);
        self.blurred.clear(window);
        self.shown = None;
        set_layer(None);
    }
}

/// Why the frames may not move now, if they may not.
fn still_reason(still: bool, window: &Window, cx: &gpui::App) -> Option<&'static str> {
    if still {
        Some("")
    } else if !window.is_window_active() {
        Some("the window is not active")
    } else if !window.is_visible() {
        Some("the window is not shown")
    } else if cx.reduce_motion() {
        Some("less motion")
    } else {
        None
    }
}

impl Render for Backdrop {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let smooth = self.smooth.now.clone();
        let blurred = self.blurred.now.clone();
        let (painted, painted_count, bounds_seen) = (self.painted.clone(), self.counts.painted.clone(), self.bounds.clone());
        let wanted = self.want.map(|look| look.area);
        let entity = cx.entity_id();
        let ground = theme::bg();
        canvas(
            move |bounds, window, _| {
                bounds_seen.set(Some(bounds));
                // The frame was made for a different size (the first one, or after a resize):
                // render again, and the render asks for a frame of this size.
                if wanted.is_some_and(|area| area != bounds.size) {
                    window.on_next_frame(move |_, cx| cx.notify(entity));
                }
                set_layer(blurred.map(|p| Layer { image: p.image, image_bounds: image_bounds(bounds, p.texels) }));
            },
            move |bounds, _, window, _| {
                painted.set(true);
                painted_count.set(painted_count.get() + 1);
                match smooth {
                    Some(p) => {
                        if let Err(error) = window.paint_image(bounds, image_bounds(bounds, p.texels), Corners::default(), p.image, 0, false) {
                            log::warn!("backdrop: paint_image failed: {error:#}");
                        }
                    }
                    // Before the first frame, and with the choice None.
                    None => window.paint_quad(fill(bounds, ground)),
                }
            },
        )
        .size_full()
    }
}

/// A glass card: a blurred crop of the backdrop, the glass colour, its edge and the light along
/// its top. Its children go on top. Text on it is `text`, `text_soft` or `card_soft_text`.
pub fn glass(radius: Pixels) -> Div {
    card(radius, theme::glass())
}

/// Glass under long text (chat, settings, the start box).
pub fn glass_strong(radius: Pixels) -> Div {
    card(radius, theme::glass_strong())
}

fn card(radius: Pixels, tint: Hsla) -> Div {
    let (edge, light) = (theme::glass_border(), theme::glass_highlight());
    div().relative().rounded(radius).child(
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let corners = Corners::all(radius);
                // No layer in this frame (or no frame yet): the tint alone.
                if let Some(layer) = LAYER.with(|current| current.borrow().clone()) {
                    if let Err(error) = window.paint_image(bounds, layer.image_bounds, corners, layer.image, 0, false) {
                        log::warn!("backdrop: paint_image failed: {error:#}");
                    }
                }
                window.paint_quad(quad(bounds, corners, tint, px(1.), edge, BorderStyle::Solid));
                let inset = radius.min(bounds.size.width / 2.);
                let top = Bounds::new(point(bounds.origin.x + inset, bounds.origin.y), size(bounds.size.width - inset * 2., px(1.)));
                window.paint_quad(fill(top, light));
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full(),
    )
}

/// The backdrop's layer and its debug page (the `Workspace` field).
#[derive(Default)]
pub struct Backdrops {
    /// The layer, made at its first use.
    layer: Option<Entity<Backdrop>>,
    /// The debug page, while it is open.
    page: Option<Page>,
    /// Debug page: the frames stand still (the baseline of a measurement).
    still: bool,
    /// Debug page: the numbers it shows, and when they were read. Once a second, so that a
    /// measurement does not count new text for each frame.
    counts: Option<(Instant, Vec<String>)>,
}

struct Page {
    cards: bool,
}

/// What a `GLASSCODE_DEBUG_OPEN=backdrop:…` value asks for.
#[derive(Debug, Default, PartialEq)]
struct Hook {
    choice: Option<BackdropChoice>,
    finish: Option<Finish>,
    theme: Option<ThemeChoice>,
    still: bool,
    bare: bool,
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
            "still" => hook.still = true,
            "bare" => hook.bare = true,
            _ => {}
        }
    }
    hook
}

const CHOICES: [(BackdropChoice, &str); 4] =
    [(BackdropChoice::Prism, "Prism"), (BackdropChoice::Aurora, "Aurora"), (BackdropChoice::Dusk, "Dusk"), (BackdropChoice::None, "None")];

impl Workspace {
    /// An absolute layer for the back of a screen. Strength: Today 0.85, sessions 0.6, Settings
    /// 0.8, the first run 1.0.
    pub fn backdrop_layer(&mut self, strength: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (appearance, still) = (self.state.appearance, self.backdrop.still);
        let layer = self.backdrop.layer.get_or_insert_with(|| cx.new(|cx| Backdrop::new(window, cx))).clone();
        layer.update(cx, |layer, cx| layer.wants(appearance, strength, still, window, cx));
        div().absolute().top_0().left_0().size_full().child(layer).into_any_element()
    }

    /// Opens the debug page (`GLASSCODE_DEBUG_OPEN=backdrop…`).
    pub fn open_backdrop_page(&mut self, mode: &str, cx: &mut Context<Self>) {
        let hook = parse_hook(mode);
        self.backdrop.page = Some(Page { cards: !hook.bare });
        self.backdrop.still = hook.still;
        self.set_appearance(
            |a| {
                a.backdrop = hook.choice.unwrap_or(a.backdrop);
                a.finish = hook.finish.unwrap_or(a.finish);
                a.theme = hook.theme.unwrap_or(a.theme);
            },
            cx,
        );
    }

    /// The debug page, over everything below the title bar, while it is open.
    pub fn render_backdrop_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let cards = self.backdrop.page.as_ref()?.cards;
        let layer = self.backdrop_layer(0.85, window, cx);
        let appearance = self.state.appearance;
        if self.backdrop.counts.as_ref().is_none_or(|(at, _)| at.elapsed() >= Duration::from_secs(1)) {
            let lines = self.backdrop.layer.as_ref().map(|layer| {
                let layer = layer.read(cx);
                layer.counts.lines(layer.want)
            });
            self.backdrop.counts = Some((Instant::now(), lines.unwrap_or_default()));
        }
        let counts = self.backdrop.counts.as_ref().map(|(_, lines)| lines.clone());

        let choices = ui::segmented().children(CHOICES.map(|(choice, name)| {
            ui::segment(SharedString::from(format!("backdrop-choice-{name}")), appearance.backdrop == choice, name)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_appearance(|a| a.backdrop = choice, cx)))
        }));
        let finishes = ui::segmented().children([(Finish::Smooth, "Smooth"), (Finish::Dither, "Dither")].map(|(finish, name)| {
            ui::segment(SharedString::from(format!("backdrop-finish-{name}")), appearance.finish == finish, name)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_appearance(|a| a.finish = finish, cx)))
        }));
        let themes = ui::segmented().children([(ThemeChoice::Light, "Light"), (ThemeChoice::Dark, "Dark")].map(|(choice, name)| {
            ui::segment(SharedString::from(format!("backdrop-theme-{name}")), appearance.theme == choice, name)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_appearance(|a| a.theme = choice, cx)))
        }));
        let header = glass_strong(px(14.))
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .px_4()
            .py_2p5()
            .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Backdrop"))
            .child(choices)
            .child(finishes)
            .child(themes)
            .child(div().flex_1())
            .child(ui::button("backdrop-close", "Close").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.backdrop.page = None;
                this.backdrop.still = false;
                this.backdrop.counts = None;
                cx.notify();
            })));

        let title = |text: &'static str| div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(text);
        let soft = |text: String| div().text_color(theme::card_soft_text()).child(text);
        let glass_card = glass(px(18.))
            .flex()
            .flex_col()
            .gap_1()
            .w(px(360.))
            .p_4()
            .child(title("Glass"))
            .child(soft("A blurred copy of the backdrop, cropped to this card".into()))
            .child(soft("with its rounded corners, under theme::glass().".into()))
            .child(div().mt_2().text_color(theme::text_soft()).child("text_soft: a row of a list on glass"));
        let strong_card = glass_strong(px(18.))
            .flex()
            .flex_col()
            .gap_1()
            .w(px(360.))
            .p_4()
            .child(title("Glass strong"))
            .child(div().text_color(theme::text()).child(
                "Under long text: a chat, the settings, the start box. The colour is more opaque, so that a long \
                 paragraph reads as well over the brightest part of the ribbon as over the dark ground.",
            ))
            .child(soft("card_soft_text: the second line".into()));
        let counts_card = glass(px(18.))
            .flex()
            .flex_col()
            .gap_1()
            .w(px(744.))
            .p_4()
            .child(title("Frames"))
            .children(counts.unwrap_or_default().into_iter().map(soft));

        Some(
            div()
                .id("backdrop-page")
                .absolute()
                // Below the title bar, which stays in use.
                .top(px(44.))
                .left_0()
                .right_0()
                .bottom_0()
                .bg(theme::bg())
                .text_color(theme::text())
                .occlude()
                .child(layer)
                .child(
                    div()
                        .relative()
                        .size_full()
                        .flex()
                        .flex_col()
                        .gap_6()
                        .p_6()
                        .child(header)
                        .when(cards, |d| {
                            d.child(div().flex().flex_wrap().gap_6().child(glass_card).child(strong_card)).child(counts_card)
                        }),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{CLASSIC, CONTRAST, GLASS_DARK, GLASS_LIGHT, Palette, Translucent, c, contrast};
    use gpui::Rgba;

    #[test]
    fn the_hook_names_a_look() {
        assert_eq!(parse_hook("backdrop"), Hook::default());
        let hook = parse_hook("backdrop:dusk:dither:light:still:bare");
        assert_eq!(
            hook,
            Hook { choice: Some(BackdropChoice::Dusk), finish: Some(Finish::Dither), theme: Some(ThemeChoice::Light), still: true, bare: true }
        );
        assert_eq!(parse_hook("backdrop:none:pink").choice, Some(BackdropChoice::None));
    }

    #[test]
    fn the_image_covers_the_area_without_its_border() {
        let area = Bounds::new(point(px(0.), px(44.)), size(px(1200.), px(800.)));
        let full = image_bounds(area, (240, 160));
        // One texel is 5 logical pixels: the border lies one texel outside the area.
        assert_eq!(full, Bounds::new(point(px(-5.), px(39.)), size(px(1210.), px(810.))));
    }

    #[test]
    fn the_dither_rate_drops_for_a_large_window() {
        let look = |w: f32, h: f32, finish| Look { preset: Preset::Prism, finish, light: false, strength: 0.85, area: size(px(w), px(h)), scale: 1.5 };
        assert_eq!(look(1440., 900., Finish::Smooth).interval(), Duration::from_millis(100));
        // 2160 x 1350 device pixels: under 4 million.
        assert_eq!(look(1440., 900., Finish::Dither).interval(), Duration::from_millis(125));
        // 3840 x 2160 device pixels.
        assert_eq!(look(2560., 1440., Finish::Dither).interval(), Duration::from_millis(250));
        assert_eq!((look(1440., 900., Finish::Dither).device(), look(1440., 900., Finish::Dither).cell()), ((2160, 1350), 5));
        assert!(!look(0., 900., Finish::Smooth).drawable());
    }

    /// The WCAG luminance of a byte, for the search for the worst pixel.
    fn linear(byte: u8) -> f32 {
        let v = byte as f32 / 255.;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    }

    /// The pixel of `frame` on which `glass` gives the least contrast to the palette's text: the
    /// brightest under light text, the darkest under dark text. Returns it as a colour.
    fn worst_pixel(frame: &Frame, glass: Translucent, light: bool) -> Hsla {
        let g = [(glass.rgb >> 16) as u8, (glass.rgb >> 8) as u8, glass.rgb as u8].map(|v| v as f32);
        let a = glass.alpha;
        let mut best = (if light { f32::MAX } else { f32::MIN }, [0u8; 3]);
        for p in frame.bgra.chunks(4) {
            let rgb = [p[2], p[1], p[0]];
            let over = [0, 1, 2].map(|i| (rgb[i] as f32 * (1. - a) + g[i] * a).round() as u8);
            let lum = 0.2126 * linear(over[0]) + 0.7152 * linear(over[1]) + 0.0722 * linear(over[2]);
            if (light && lum < best.0) || (!light && lum > best.0) {
                best = (lum, rgb);
            }
        }
        let [r, g, b] = best.1.map(|v| v as f32 / 255.);
        Rgba { r, g, b, a: 1. }.into()
    }

    /// Section 4 rule 12 of the plan: text on glass reads over the backdrop at its brightest, in
    /// each palette, for each look and strength, at several moments.
    #[test]
    fn text_on_glass_is_readable_over_each_backdrop() {
        let palettes: [&Palette; 4] = [&GLASS_LIGHT, &GLASS_DARK, &CLASSIC, &CONTRAST];
        let mut lowest = (f32::MAX, f32::MAX);
        for preset in Preset::ALL {
            for strength in [0.6, 0.8, 0.85] {
                for seconds in [0., 7., 23., 61.] {
                    let frames = [false, true].map(|light| pixels::render(preset, Finish::Smooth, light, strength, seconds, 320, 200));
                    for p in palettes {
                        let frame = &frames[usize::from(p.light)];
                        for (name, glass) in [("glass", p.glass), ("glass_strong", p.glass_strong)] {
                            let surface = worst_pixel(frame, glass, p.light).blend(glass.into());
                            let text = contrast(c(p.text), surface);
                            let soft = contrast(c(p.card_soft_text), surface);
                            lowest = (lowest.0.min(text), lowest.1.min(soft));
                            let at = format!("{}, {name}, {preset:?} at strength {strength}, {seconds} s", p.name);
                            assert!(text >= 7.0, "text {text:.2}: {at}");
                            assert!(soft >= 4.5, "card_soft_text {soft:.2}: {at}");
                        }
                    }
                }
            }
        }
        eprintln!("lowest contrast over the backdrops: text {:.2}, card_soft_text {:.2}", lowest.0, lowest.1);
    }
}
