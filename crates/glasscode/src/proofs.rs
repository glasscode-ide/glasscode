//! The proofs of the UI redesign (slice U02): a debug page that paints a backdrop from a byte
//! buffer, crops it into glass cards, and paints a dither image at full size, so that their cost
//! can be measured before U04 and U05 choose a method. `docs/ui-redesign-proofs.md` has the
//! numbers. U23 removes this file.
//!
//! `GLASSCODE_DEBUG_OPEN=proofs[:<what>]` opens it in a window of its own. `<what>` is:
//! - nothing: the backdrop with glass cards on it, and the counts (the demonstration)
//! - `backdrop`: proof 1, a 320 x 200 frame 15 times each second; each old frame is dropped
//! - `backdrop:keep`: the same without `drop_image`, to show that a leak can be seen
//! - `still`: the same page with one frame only, the baseline for processor use
//! - `glass[:pad][:bare][:grey]`: proof 2, a test pattern and cards that crop it; `pad` gives the
//!   image a border of one texel, `bare` leaves the cards out, `grey` paints them in grey
//! - `dither[:<w>x<h>]`: proof 3, a dither image at the pixel size of the window (or w x h),
//!   8 times each second
//! - `scale`: proof 4, text, a ruler of 1 px lines and buttons that show where a click landed
//! - `redraw`: no image; the main window draws again 15 times each second, for the cost of a
//!   backdrop that makes the whole window draw

use std::cell::{Cell, RefCell};
use std::f32::consts::PI;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, Bounds, Context, Corners, Entity, MouseButton, MouseDownEvent, Pixels,
    Point, Render, RenderImage, SharedString, TitlebarOptions, WeakEntity, Window, WindowBounds,
    WindowOptions, canvas, div, point, prelude::*, px, size,
};

use crate::text_input::TextInput;
use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

/// The backdrop's size in texels (proof 1). The mock draws 6 px cells, so a 1920 px wide window
/// has 320 of them.
const BACKDROP: (u32, u32) = (320, 200);
const RADIUS: f32 = 18.;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// The demonstration: the backdrop, glass cards on it, and the counts.
    Overview,
    /// Proof 1. `drop`: each old frame leaves the atlas; `animate`: false paints one frame only.
    Backdrop { drop: bool, animate: bool },
    /// Proof 2. `pad`: the image has a border of one texel; `bare`: no cards; `grey`: grey cards.
    Glass { pad: bool, bare: bool, grey: bool },
    /// Proof 3: a dither image of this size, or of the window's pixel size.
    Dither { size: Option<(u32, u32)> },
    /// Proof 4.
    Scale,
    /// The main window draws again at the backdrop's rate, with no image.
    Redraw,
}

impl Mode {
    fn parse(text: &str) -> Mode {
        let has = |word: &str| text.split(':').any(|w| w == word);
        let mut words = text.split(':').skip(1);
        match words.next() {
            Some("backdrop") => Mode::Backdrop {
                drop: !has("keep"),
                animate: true,
            },
            Some("still") => Mode::Backdrop {
                drop: true,
                animate: false,
            },
            Some("glass") => Mode::Glass {
                pad: has("pad"),
                bare: has("bare"),
                grey: has("grey"),
            },
            Some("dither") => Mode::Dither {
                size: words.next().and_then(|s| {
                    let (w, h) = s.split_once('x')?;
                    Some((w.parse().ok()?, h.parse().ok()?))
                }),
            },
            Some("scale") => Mode::Scale,
            Some("redraw") => Mode::Redraw,
            _ => Mode::Overview,
        }
    }

    /// How often a new frame is made, for the pages that move.
    fn interval(self) -> Option<Duration> {
        match self {
            Mode::Overview | Mode::Backdrop { animate: true, .. } | Mode::Redraw => {
                Some(Duration::from_millis(1000 / 15))
            }
            Mode::Dither { .. } => Some(Duration::from_millis(1000 / 8)),
            // One image only; the page draws again each second for the crop numbers.
            Mode::Glass { .. } => Some(Duration::from_secs(1)),
            Mode::Backdrop { animate: false, .. } | Mode::Scale => None,
        }
    }

    fn animated(self) -> bool {
        matches!(
            self,
            Mode::Overview | Mode::Backdrop { animate: true, .. } | Mode::Dither { .. }
        )
    }

    fn label(self) -> String {
        match self {
            Mode::Overview => "backdrop and glass".into(),
            Mode::Backdrop {
                drop: true,
                animate: true,
            } => "proof 1: backdrop, 15 frames each second, drop_image".into(),
            Mode::Backdrop { drop: false, .. } => {
                "proof 1 control: backdrop without drop_image".into()
            }
            Mode::Backdrop { animate: false, .. } => "baseline: one backdrop frame".into(),
            Mode::Glass { pad, bare, grey } => format!(
                "proof 2: glass crop{}{}{}",
                if pad { ", padded" } else { "" },
                if bare { ", no cards" } else { "" },
                if grey { ", grey cards" } else { "" }
            ),
            Mode::Dither { size: Some((w, h)) } => {
                format!("proof 3: dither {w} x {h}, 8 frames each second")
            }
            Mode::Dither { size: None } => {
                "proof 3: dither at the window's size, 8 frames each second".into()
            }
            Mode::Scale => "proof 4: scale".into(),
            Mode::Redraw => "the main window drawn 15 times each second, no image".into(),
        }
    }
}

/// Opens the proofs page in a window of its own (debug builds, `GLASSCODE_DEBUG_OPEN=proofs…`).
pub fn open(text: &str, workspace: WeakEntity<Workspace>, cx: &mut App) {
    let mode = Mode::parse(text);
    // 1280 x 720 is 1920 x 1080 pixels at 150 %, the size proof 3 measures.
    let content = match mode {
        Mode::Dither { .. } => size(px(1280.), px(720.)),
        // Small, so that the main window behind it stays in sight.
        Mode::Redraw => size(px(600.), px(140.)),
        _ => size(px(1280.), px(800.)),
    };
    // After the main window is made: this runs while it is being built.
    cx.defer(move |cx| {
        let bounds = Bounds::centered(None, content, cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Glasscode proofs".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Proofs::new(mode, workspace, window, cx)),
        );
        if let Err(error) = opened {
            log::warn!("could not open the proofs page: {error:#}");
        }
    });
}

/// An image on screen, and the one before it. The one before leaves the atlas at the next frame,
/// after the new one is painted: the atlas texture then always holds a tile, and is not made
/// again for each frame.
#[derive(Default)]
struct Slot {
    now: Option<Arc<RenderImage>>,
    old: Option<Arc<RenderImage>>,
}

impl Slot {
    /// Puts `image` on screen; gives how many images left the atlas.
    fn set(&mut self, image: Arc<RenderImage>, drop: bool, window: &mut Window) -> u64 {
        let mut dropped = 0;
        if let Some(old) = self.old.take().filter(|_| drop) {
            match window.drop_image(old) {
                Ok(()) => dropped = 1,
                Err(error) => log::warn!("proofs: drop_image failed: {error:#}"),
            }
        }
        self.old = self.now.replace(image);
        dropped
    }
}

pub struct Proofs {
    mode: Mode,
    started: Instant,
    backdrop: Slot,
    blurred: Slot,
    /// Glass: a tile painted before the test pattern, so that the pattern has a neighbour in
    /// the atlas whose colour would show at its edges.
    neighbour: Option<Arc<RenderImage>>,
    /// The texels of the image on screen without its border, and the border's width.
    texels: (u32, u32),
    pad: u32,
    made: u64,
    dropped: u64,
    making: Duration,
    bytes: usize,
    /// The frames the backdrop was painted in.
    painted: Rc<Cell<u64>>,
    /// Glass: the bounds each card was painted at, for the crop error.
    cards: Rc<RefCell<Vec<Bounds<Pixels>>>>,
    /// Scale: the clicks on each button, and where the last click landed.
    clicks: [u32; 4],
    last_click: Option<Point<Pixels>>,
    input: Option<Entity<TextInput>>,
    logged: Instant,
    workspace: WeakEntity<Workspace>,
}

impl Proofs {
    fn new(
        mode: Mode,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            workspace,
            mode,
            started: Instant::now(),
            backdrop: Slot::default(),
            blurred: Slot::default(),
            neighbour: None,
            texels: BACKDROP,
            pad: 0,
            made: 0,
            dropped: 0,
            making: Duration::ZERO,
            bytes: 0,
            painted: Rc::new(Cell::new(0)),
            cards: Rc::new(RefCell::new(Vec::new())),
            clicks: [0; 4],
            last_click: None,
            input: (mode == Mode::Scale)
                .then(|| cx.new(|cx| TextInput::new("Type here to see the caret", cx))),
            logged: Instant::now(),
        };
        this.make(window);
        if let Some(every) = mode.interval() {
            cx.spawn_in(window, async move |this, cx| {
                // Against a clock, not "wait, then work": a Windows timer ends on a tick of about
                // 15.6 ms, so a plain wait of 66 ms gives 12.8 frames each second, not 15.
                let mut next = Instant::now();
                loop {
                    next += every;
                    let now = Instant::now();
                    if next <= now {
                        // Late (a slow frame): skip to the next slot. Frames made back to back
                        // leave the main thread no time to draw them.
                        next = now + every;
                    }
                    cx.background_executor()
                        .timer(next.saturating_duration_since(now))
                        .await;
                    if this
                        .update_in(cx, |this, window, cx| this.tick(window, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
        }
        log::warn!("proofs: opened ({})", mode.label());
        this
    }

    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode.animated() {
            self.make(window);
        }
        if self.logged.elapsed() >= Duration::from_secs(30) {
            self.logged = Instant::now();
            log::warn!("proofs: {}", self.counts(window).join(" · "));
        }
        if self.mode == Mode::Redraw {
            // Only the main window: this page stays as it is.
            self.made += 1;
            if self.workspace.update(cx, |_, cx| cx.notify()).is_err() {
                log::warn!("proofs: the main window is gone");
            }
            return;
        }
        cx.notify();
    }

    /// Makes the images of the next frame.
    fn make(&mut self, window: &mut Window) {
        let seconds = self.started.elapsed().as_secs_f32();
        let light = theme::is_light();
        let begun = Instant::now();
        match self.mode {
            Mode::Overview | Mode::Backdrop { .. } => {
                let (w, h) = BACKDROP;
                let drop = !matches!(self.mode, Mode::Backdrop { drop: false, .. });
                let smooth = ribbon(seconds, w, h, 1, 0.85, light, false);
                if self.mode == Mode::Overview {
                    if let Some(image) = make_image(w, h, blur(&smooth, w as usize, h as usize, 3))
                    {
                        self.dropped += self.blurred.set(image, drop, window);
                    }
                }
                self.bytes = smooth.len();
                if let Some(image) = make_image(w, h, smooth) {
                    self.dropped += self.backdrop.set(image, drop, window);
                }
            }
            Mode::Dither { size } => {
                let (w, h) = size.unwrap_or_else(|| device_size(window));
                // The mock's dither cell is 3 px; here it is whole device pixels, so that each
                // cell is the same size on screen.
                let cell = ((3. * window.scale_factor()).round() as u32).max(1);
                let bgra = ribbon(seconds, w, h, cell, 0.85, light, true);
                self.texels = (w, h);
                self.bytes = bgra.len();
                if let Some(image) = make_image(w, h, bgra) {
                    self.dropped += self.backdrop.set(image, true, window);
                }
            }
            Mode::Glass { pad, .. } if self.backdrop.now.is_none() => {
                let (w, h) = BACKDROP;
                self.pad = u32::from(pad);
                let (pw, ph) = (w + 2 * self.pad, h + 2 * self.pad);
                self.neighbour = make_image(w, h, neighbour(w, h));
                let bgra = pattern(w, h, self.pad);
                self.bytes = bgra.len();
                if let Some(image) = make_image(pw, ph, bgra) {
                    self.backdrop.set(image, true, window);
                }
            }
            Mode::Glass { .. } | Mode::Scale | Mode::Redraw => return,
        }
        self.making += begun.elapsed();
        self.made += 1;
    }

    /// The counts the page shows and the log repeats.
    fn counts(&self, window: &Window) -> Vec<String> {
        let (dw, dh) = device_size(window);
        let seconds = self.started.elapsed().as_secs_f32();
        let average = if self.made == 0 {
            0.
        } else {
            self.making.as_secs_f32() * 1000. / self.made as f32
        };
        vec![
            format!("Proofs · {}", self.mode.label()),
            format!(
                "Window {} x {} px (scale {:.2}), image {} x {} texels, {} KB each",
                dw,
                dh,
                window.scale_factor(),
                self.texels.0 + 2 * self.pad,
                self.texels.1 + 2 * self.pad,
                self.bytes / 1024
            ),
            format!(
                "Frames made {}, painted {}, images dropped {}, {:.1} frames each second",
                self.made,
                self.painted.get(),
                self.dropped,
                if seconds > 0. {
                    self.painted.get() as f32 / seconds
                } else {
                    0.
                }
            ),
            format!("Making a frame: {average:.2} ms on average · running {seconds:.0} s"),
        ]
    }

    fn render_counts(&self, window: &Window) -> impl IntoElement {
        div()
            .absolute()
            .top(px(12.))
            .left(px(12.))
            .flex()
            .flex_col()
            .gap_0p5()
            .px_3()
            .py_2()
            .rounded(px(10.))
            .bg(theme::glass_strong())
            .border_1()
            .border_color(theme::glass_border())
            .text_size(px(12.))
            .children(
                self.counts(window)
                    .into_iter()
                    .map(|line| div().child(line)),
            )
    }

    /// The test pattern, painted full size, and cards that crop the same image.
    fn render_glass(
        &self,
        pad: bool,
        bare: bool,
        grey: bool,
        window: &Window,
    ) -> Vec<gpui::AnyElement> {
        let Some(image) = self.backdrop.now.clone() else {
            return Vec::new();
        };
        // Odd places and sizes on purpose: the crop must not depend on whole pixels.
        let cards = [
            (97.3, 83.6, 341.7, 203.2),
            (511.5, 140.25, 280., 160.),
            (180.8, 420.4, 500., 220.),
            (760.1, 400.7, 333.3, 250.9),
        ];
        let area = Bounds::new(point(px(0.), px(0.)), window.viewport_size());
        let full = image_bounds(area, self.texels, self.pad);
        let tile = (self.texels.0 + 2 * self.pad, self.texels.1 + 2 * self.pad);
        let mut elements = Vec::new();
        if bare {
            return elements;
        }
        // The bounds of the frame before; the cards' paint gives this frame's.
        let before = std::mem::take(&mut *self.cards.borrow_mut());
        for (i, (x, y, w, h)) in cards.into_iter().enumerate() {
            let (image, cards) = (image.clone(), self.cards.clone());
            elements.push(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(w))
                    .h(px(h))
                    .child(
                        canvas(
                            |_, _, _| (),
                            move |bounds, _, window, _| {
                                cards.borrow_mut().push(bounds);
                                if let Err(error) = window.paint_image(
                                    bounds,
                                    full,
                                    Corners::all(px(RADIUS)),
                                    image,
                                    0,
                                    grey,
                                ) {
                                    log::warn!("proofs: paint_image failed: {error:#}");
                                }
                            },
                        )
                        .size_full(),
                    )
                    .into_any_element(),
            );
            let note = match before.get(i).copied() {
                Some(bounds) => {
                    let (dx, dy) = crop_error(bounds, full, tile);
                    format!(
                        "card {}: crop error {:.2} x {:.2} px{}",
                        i + 1,
                        dx,
                        dy,
                        if pad { " (padded)" } else { "" }
                    )
                }
                None => format!("card {}", i + 1),
            };
            elements.push(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y + h + 4.))
                    .px_1()
                    .rounded(px(4.))
                    .bg(theme::glass_strong())
                    .text_size(px(11.))
                    .child(note)
                    .into_any_element(),
            );
        }
        elements
    }

    /// The demonstration: glass cards over the moving backdrop.
    fn render_overview(&self, window: &Window) -> Vec<gpui::AnyElement> {
        let Some(blurred) = self.blurred.now.clone() else {
            return Vec::new();
        };
        let area = Bounds::new(point(px(0.), px(0.)), window.viewport_size());
        let full = image_bounds(area, self.texels, 0);
        let card =
            |id: &'static str, x: f32, y: f32, w: f32, h: f32, title: &str, lines: Vec<String>| {
                let image = blurred.clone();
                div()
                    .id(id)
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(w))
                    .h(px(h))
                    .child(
                        canvas(
                            |_, _, _| (),
                            move |bounds, _, window, _| {
                                if let Err(error) = window.paint_image(
                                    bounds,
                                    full,
                                    Corners::all(px(RADIUS)),
                                    image,
                                    0,
                                    false,
                                ) {
                                    log::warn!("proofs: paint_image failed: {error:#}");
                                }
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(
                        div()
                            .absolute()
                            .size_full()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .p_4()
                            .rounded(px(RADIUS))
                            .border_1()
                            .border_color(theme::glass_border())
                            .bg(theme::glass())
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(SharedString::from(title.to_string())),
                            )
                            .children(
                                lines.into_iter().map(|line| {
                                    div().text_color(theme::card_soft_text()).child(line)
                                }),
                            ),
                    )
                    .into_any_element()
            };
        vec![
            card(
                "proofs-backdrop",
                40.,
                150.,
                520.,
                190.,
                "Backdrop",
                vec![
                    "A 320 x 200 image made from bytes 15 times each second,".into(),
                    "painted over the window by the bilinear sampler.".into(),
                    "The image before it leaves the atlas at the next frame.".into(),
                ],
            ),
            card(
                "proofs-glass",
                600.,
                150.,
                520.,
                190.,
                "Glass",
                vec![
                    "A blurred copy of the backdrop, cropped by paint_image".into(),
                    "to this card, with rounded corners, a tint and a border.".into(),
                    "GPUI has no backdrop blur; only our backdrop is behind a card.".into(),
                ],
            ),
            card(
                "proofs-open",
                40.,
                380.,
                1080.,
                230.,
                "The proofs, one at a time",
                vec![
                    "proofs:backdrop  ·  proofs:backdrop:keep  ·  proofs:still".into(),
                    "proofs:glass  ·  proofs:glass:pad  ·  proofs:glass:bare  ·  proofs:glass:grey"
                        .into(),
                    "proofs:dither  ·  proofs:dither:3840x2160".into(),
                    "proofs:scale  ·  proofs:redraw".into(),
                    "Numbers and decisions: docs/ui-redesign-proofs.md".into(),
                ],
            ),
        ]
    }

    fn render_scale(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (dw, dh) = device_size(window);
        let viewport = window.viewport_size();
        let ruler = div().flex().h(px(24.)).children((0..60).map(|i| {
            div().w(px(1.)).h_full().mr(px(1.)).bg(if i % 10 == 0 {
                theme::text()
            } else {
                theme::muted()
            })
        }));
        let buttons = div().flex().gap_2().children((0..4).map(|i| {
            ui::button(
                SharedString::from(format!("proofs-button-{i}")),
                format!("Button {} · {}", i + 1, self.clicks[i]),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.clicks[i] += 1;
                cx.notify();
            }))
        }));
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_6()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.last_click = Some(event.position);
                    cx.notify();
                }),
            )
            .child(div().text_size(px(20.)).font_weight(gpui::FontWeight::SEMIBOLD).child(self.mode.label()))
            .child(format!(
                "Scale factor {:.3} · window {:.1} x {:.1} (logical) · {} x {} device pixels",
                window.scale_factor(),
                f32::from(viewport.width),
                f32::from(viewport.height),
                dw,
                dh
            ))
            .children([11., 13., 16., 24.].map(|size| {
                div().text_size(px(size)).child(format!("{size} px: The quick brown fox jumps over the lazy dog 0123456789"))
            }))
            .child(div().text_color(theme::text_soft()).child("A ruler of 1 px lines, 1 px apart. Uneven lines mean a scale that is not a whole number."))
            .child(ruler)
            .child(buttons)
            .child(match self.last_click {
                Some(p) => format!("Last click at {:.1}, {:.1} (logical pixels)", f32::from(p.x), f32::from(p.y)),
                None => "Click anywhere: the page shows where, in logical pixels.".into(),
            })
            .when_some(self.input.clone(), |d, input| d.child(div().w(px(420.)).child(input)))
    }
}

impl Render for Proofs {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = div()
            .id("proofs")
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(theme::bg())
            .text_color(theme::text())
            .font_family(theme::UI_FONT)
            .text_size(px(13.));
        if self.mode == Mode::Scale {
            return page.child(self.render_scale(window, cx));
        }
        let (texels, pad, painted) = (self.texels, self.pad, self.painted.clone());
        let page = page
            // Under the backdrop, so it is never seen; it is only there to sit next to the
            // pattern in the atlas.
            .when_some(self.neighbour.clone(), |d, image| {
                d.child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            let corner = Bounds::new(bounds.origin, size(px(8.), px(5.)));
                            if let Err(error) = window.paint_image(
                                corner,
                                corner,
                                Corners::default(),
                                image,
                                0,
                                false,
                            ) {
                                log::warn!("proofs: paint_image failed: {error:#}");
                            }
                        },
                    )
                    .absolute()
                    .size_full(),
                )
            })
            .when_some(self.backdrop.now.clone(), |d, image| {
                d.child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            let full = image_bounds(bounds, texels, pad);
                            match window.paint_image(
                                bounds,
                                full,
                                Corners::default(),
                                image,
                                0,
                                false,
                            ) {
                                Ok(()) => painted.set(painted.get() + 1),
                                Err(error) => log::warn!("proofs: paint_image failed: {error:#}"),
                            }
                        },
                    )
                    .absolute()
                    .size_full(),
                )
            });
        let cards = match self.mode {
            Mode::Glass { pad, bare, grey } => self.render_glass(pad, bare, grey, window),
            Mode::Overview => self.render_overview(window),
            _ => Vec::new(),
        };
        page.children(cards).child(self.render_counts(window))
    }
}

/// The window's size in device pixels.
fn device_size(window: &Window) -> (u32, u32) {
    let (size, scale) = (window.viewport_size(), window.scale_factor());
    (
        (f32::from(size.width) * scale).round() as u32,
        (f32::from(size.height) * scale).round() as u32,
    )
}

/// Where the whole image goes so that its texels without the border cover `area`.
fn image_bounds(area: Bounds<Pixels>, texels: (u32, u32), pad: u32) -> Bounds<Pixels> {
    let tx = area.size.width / texels.0 as f32 * pad as f32;
    let ty = area.size.height / texels.1 as f32 * pad as f32;
    Bounds::new(
        point(area.origin.x - tx, area.origin.y - ty),
        size(area.size.width + tx * 2., area.size.height + ty * 2.),
    )
}

/// How far `paint_image` moves the edges of a crop, in logical pixels (left or right, top or
/// bottom, whichever is more). It rounds the part of the image that it shows to whole texels
/// (`Window::paint_image`), then paints that part over the crop.
fn crop_error(card: Bounds<Pixels>, image: Bounds<Pixels>, tile: (u32, u32)) -> (f32, f32) {
    let axis = |start: f32, len: f32, image_start: f32, image_len: f32, texels: u32| {
        let per_texel = image_len / texels as f32;
        let (exact_start, exact_len) = ((start - image_start) / per_texel, len / per_texel);
        let (start_r, len_r) = (exact_start.round(), exact_len.round());
        let first = (start_r - exact_start).abs();
        let last = (start_r + len_r - exact_start - exact_len).abs();
        first.max(last) * per_texel
    };
    let visible = card.intersect(&image);
    (
        axis(
            f32::from(visible.origin.x),
            f32::from(visible.size.width),
            f32::from(image.origin.x),
            f32::from(image.size.width),
            tile.0,
        ),
        axis(
            f32::from(visible.origin.y),
            f32::from(visible.size.height),
            f32::from(image.origin.y),
            f32::from(image.size.height),
            tile.1,
        ),
    )
}

fn make_image(width: u32, height: u32, bgra: Vec<u8>) -> Option<Arc<RenderImage>> {
    let buffer = image::RgbaImage::from_raw(width, height, bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

/// The mock's 4 x 4 Bayer table.
const BAYER: [u8; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];

/// Prism, the default look of the mock (`PAL.ribbon`): the haze above the ribbon, its edge,
/// and the three colours of the band.
const HAZE: [f32; 3] = [18., 70., 170.];
const EDGE: [f32; 3] = [170., 215., 255.];
const BAND: [[f32; 3]; 3] = [[255., 236., 170.], [255., 150., 30.], [236., 44., 24.]];
const SLOPE: f32 = 0.5;
const TOP: f32 = 0.1;

/// One frame of the ribbon at `seconds` (the mock's `draw()`), `width` x `height` pixels, as
/// BGRA bytes. The colours are worked out once for each `cell` x `cell` square: 1 for the
/// smooth backdrop, the dither cell in device pixels for a dither image drawn 1:1.
fn ribbon(
    seconds: f32,
    width: u32,
    height: u32,
    cell: u32,
    strength: f32,
    light: bool,
    dither: bool,
) -> Vec<u8> {
    let (width, height, cell) = (width as usize, height as usize, cell.max(1) as usize);
    let (w, h) = (width.div_ceil(cell).max(2), height.div_ceil(cell).max(2));
    let aspect = w as f32 / h as f32;
    let t = seconds;
    let mut top = vec![0f32; w];
    let mut thick = vec![0f32; w];
    let mut fade = vec![0f32; w];
    for x in 0..w {
        let u = x as f32 / w as f32;
        top[x] = TOP
            + SLOPE * u * aspect.min(2.) / 1.6
            + 0.05 * (u * 3.1 + t * 0.23).sin()
            + 0.02 * (u * 7.3 - t * 0.31).sin();
        thick[x] = 0.085 + 0.035 * (u * 2.3 - t * 0.17).sin();
        fade[x] = (0.55 + 0.45 * (u * PI * 0.9 + 0.35 + 0.2 * (t * 0.11).sin()).sin()) * strength;
    }
    let mut out = Vec::with_capacity(width * height * 4);
    let mut row = vec![0u8; width * 4];
    for cy in 0..h {
        let v = cy as f32 / h as f32;
        for cx in 0..w {
            let q = (v - top[cx]) / thick[cx];
            let rgb: [f32; 3] = if q < 0. {
                let (k, e) = ((q * 0.95).exp() * 0.8, (q * 3.2).exp());
                [0, 1, 2].map(|i| HAZE[i] * k + (EDGE[i] - HAZE[i]) * e)
            } else if q < 1. {
                let s = q * 2.;
                let (a, c, f) = if s < 1. {
                    (BAND[0], BAND[1], s)
                } else {
                    (BAND[1], BAND[2], s - 1.)
                };
                [0, 1, 2].map(|i| a[i] + (c[i] - a[i]) * f)
            } else {
                let k = (-(q - 1.) * 4.5).exp();
                [0, 1, 2].map(|i| BAND[2][i] * k)
            };
            let f = fade[cx] * 0.92;
            let threshold = (BAYER[(cy & 3) * 4 + (cx & 3)] as f32 + 0.5) / 16.;
            let [r, g, b] = rgb.map(|c| {
                if dither {
                    ((c / 255. * f * 3. + threshold).floor() / 3.).min(1.)
                } else {
                    c / 255. * f
                }
            });
            let [r, g, b] = if light {
                let base = 255. * (1. - r.max(g).max(b).min(1.));
                [base + r * 255., base + g * 255., base + b * 255.]
            } else {
                [9. + r * 246., 9. + g * 246., 11. + b * 244.]
            };
            let bgra = [b as u8, g as u8, r as u8, 255];
            for x in cx * cell..((cx + 1) * cell).min(width) {
                row[x * 4..x * 4 + 4].copy_from_slice(&bgra);
            }
        }
        for _ in cy * cell..((cy + 1) * cell).min(height) {
            out.extend_from_slice(&row);
        }
    }
    out
}

/// A box blur of three passes in each direction (U04 step 2 uses the same).
fn blur(bgra: &[u8], width: usize, height: usize, radius: usize) -> Vec<u8> {
    let mut a = bgra.to_vec();
    let mut b = vec![0u8; a.len()];
    for _ in 0..3 {
        box_pass(&a, &mut b, width, height, radius, true);
        box_pass(&b, &mut a, width, height, radius, false);
    }
    a
}

/// One pass of a box blur along rows or columns, with the edge pixel repeated.
fn box_pass(src: &[u8], dst: &mut [u8], width: usize, height: usize, radius: usize, rows: bool) {
    let (lines, len) = if rows {
        (height, width)
    } else {
        (width, height)
    };
    let at = |line: usize, i: usize| {
        if rows {
            (line * width + i) * 4
        } else {
            (i * width + line) * 4
        }
    };
    let span = (2 * radius + 1) as u32;
    for line in 0..lines {
        for channel in 0..4 {
            let get =
                |i: isize| src[at(line, i.clamp(0, len as isize - 1) as usize) + channel] as u32;
            let mut sum: u32 = (-(radius as isize)..=radius as isize).map(get).sum();
            for i in 0..len {
                dst[at(line, i) + channel] = (sum / span) as u8;
                sum += get(i as isize + radius as isize + 1);
                sum -= get(i as isize - radius as isize);
            }
        }
    }
}

/// The glass test pattern: a colour ramp with a light line every 20 texels, so that a crop that
/// moves or stretches shows as a broken line. `pad` repeats the edge texels around it.
fn pattern(width: u32, height: u32, pad: u32) -> Vec<u8> {
    let (pw, ph) = (width + 2 * pad, height + 2 * pad);
    let mut out = Vec::with_capacity((pw * ph * 4) as usize);
    for py in 0..ph {
        for px in 0..pw {
            let x = px.saturating_sub(pad).min(width - 1);
            let y = py.saturating_sub(pad).min(height - 1);
            let line = x % 20 == 10 || y % 20 == 10;
            let (r, g, b) = if line {
                (250, 250, 250)
            } else {
                (
                    (x * 200 / width) as u8 + 30,
                    (y * 200 / height) as u8 + 30,
                    90,
                )
            };
            out.extend_from_slice(&[b, g, r, 255]);
        }
    }
    out
}

/// A plain image in a colour that the pattern does not have, to show any bleed from the atlas.
fn neighbour(width: u32, height: u32) -> Vec<u8> {
    [0u8, 255, 0, 255].repeat((width * height) as usize)
}
