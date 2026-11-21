//! The manager's orb at small sizes: the voice screen's orb (`voice_view.rs`, the mock's
//! `drawOrb`) as the manager's face, wherever it shows as a person: its bubble, the header of its
//! panel, over what it says in the conversation, in Today's start box.
//!
//! - **A still image, made once.** For each look (the backdrop's preset, light or dark, the side
//!   in device pixels) one image is made on a background thread and kept; a draw only paints it.
//!   Until it is there, the manager's plain mark stands in its place.
//! - **Smooth at every size.** The dither finish draws cells of three logical pixels: at 18 to
//!   44 pixels that is six to fourteen cells across, and the ball does not read. The small orb is
//!   smooth whatever the backdrop's finish is.
//! - **It moves only while the manager thinks.** Then a few images of the orb at a higher level
//!   play forward and back at 6 frames each second, half of [`motion::WAIT_FPS`] (the rule of
//!   `motion.rs`: slow, and only while something is on its way). Idle, and when something waits
//!   for the person, it stands still.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use glasscode_core::backdrop::{self as pixels, Finish, Preset};
use gpui::{Animation, AnimationExt as _, AnyElement, Bounds, Context, Corners, ElementId, RenderImage, Window, canvas, div, point, prelude::*, size};

use crate::motion;
use crate::theme;
use crate::workspace::Workspace;

/// The images of the moving orb: played forward and back.
const FRAMES: u8 = 8;
/// The time from one of them to the next, in the orb's own seconds.
const FRAME_STEP: f32 = 0.16;
/// The still orb: its level, and the moment of the orb's motion it shows.
const STILL_LEVEL: f32 = 0.2;
const STILL_SECONDS: f32 = 2.;
/// The frames each second of the moving orb: one for each of its images, half of what
/// `motion::WAIT_FPS` allows. Each is a draw of the whole window, and a turn can last minutes.
const MOVING_FPS: f32 = 6.;
/// The level of the moving orb.
const MOVING_LEVEL: f32 = 0.7;
/// The ball is 0.8 of the image's half side (`pixels::orb`), with its glow outside: the image is
/// painted this much larger than the orb's box, so that the ball fills the box.
const OUTSET: f32 = 0.11;
/// No image is made larger than this (device pixels a side): the small orb is at most a bubble.
const MOST_DEVICE: u32 = 256;

/// What an image is made from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    preset: u8,
    light: bool,
    /// The side of the image in device pixels (the box and the outset).
    device: u32,
    /// 0: the still orb; 1 to [`FRAMES`]: an image of the moving one.
    frame: u8,
}

impl Key {
    fn preset(&self) -> Preset {
        match self.preset {
            1 => Preset::Aurora,
            2 => Preset::Dusk,
            _ => Preset::Prism,
        }
    }

    /// The orb's level and second for this image.
    fn moment(&self) -> (f32, f32) {
        match self.frame {
            0 => (STILL_LEVEL, STILL_SECONDS),
            n => (MOVING_LEVEL, STILL_SECONDS + FRAME_STEP * f32::from(n)),
        }
    }
}

fn preset_number(preset: Preset) -> u8 {
    match preset {
        Preset::Prism => 0,
        Preset::Aurora => 1,
        Preset::Dusk => 2,
    }
}

/// The images that are made, those on their way, and those the last draw asked for.
#[derive(Default)]
pub struct Orbs {
    images: HashMap<Key, Arc<RenderImage>>,
    making: HashSet<Key>,
    /// Asked for while drawing (a draw cannot start a task): `Workspace::orbs_make` takes them.
    wanted: RefCell<HashSet<Key>>,
    /// The window's device pixels for each logical one, as the last frame had it.
    scale_factor: Cell<f32>,
}

/// An orb image, on a background thread: smooth, with the border of one texel that keeps the
/// atlas's next image out of its edge (`pixels::pad`).
fn make(key: Key) -> Option<Arc<RenderImage>> {
    let (level, seconds) = key.moment();
    let frame = pixels::pad(&pixels::orb(key.preset(), Finish::Smooth, key.light, level, seconds, key.device));
    let buffer = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

/// The side of the image for an orb whose box is `side` logical pixels at 100 %.
fn device_side(side: f32, scale: f32, scale_factor: f32) -> u32 {
    ((side * (1. + 2. * OUTSET) * scale * scale_factor).round() as u32).clamp(1, MOST_DEVICE)
}

/// The image of the moving orb at `t` (0 to 1) of its loop: forward, then back, so that the
/// loop has no jump.
fn frame_at(t: f32) -> u8 {
    let t = t.clamp(0., 1.);
    let there = if t < 0.5 { t * 2. } else { (1. - t) * 2. };
    1 + (there * f32::from(FRAMES - 1)).round() as u8
}

/// The image painted over a box of `side`: larger than the box by the outset, so the ball fills
/// it and the glow lies around it.
fn painted(image: Arc<RenderImage>, device: u32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let out = bounds.size.width * OUTSET;
            let area = Bounds::new(point(bounds.origin.x - out, bounds.origin.y - out), size(bounds.size.width + out * 2., bounds.size.height + out * 2.));
            // The image's border of one texel lies outside what is shown.
            let texel = area.size.width / device.max(1) as f32;
            let whole = Bounds::new(point(area.origin.x - texel, area.origin.y - texel), size(area.size.width + texel * 2., area.size.height + texel * 2.));
            if let Err(error) = window.paint_image(area, whole, Corners::default(), image.clone(), 0, false) {
                log::warn!("orb: paint_image failed: {error:#}");
            }
        },
    )
    .size_full()
}

impl Workspace {
    /// The manager's orb in a box of `side` logical pixels (at 100 %; the scale is applied here).
    /// `moving`: the manager is thinking, and the orb moves while it does; `id` names that
    /// animation, one for each place the orb shows. Until its image is made, `fallback` shows.
    pub(crate) fn manager_orb(&self, id: impl Into<ElementId>, side: f32, moving: bool, fallback: impl IntoElement) -> AnyElement {
        let orbs = &self.manager.orbs;
        let key = Key {
            preset: preset_number(self.state.appearance.backdrop.preset().unwrap_or(Preset::Prism)),
            light: theme::is_light(),
            device: device_side(side, theme::scale(), orbs.scale_factor.get().max(1.)),
            frame: 0,
        };
        let holder = div().flex_none().size(theme::sz(side));
        let get = |frame: u8| {
            let key = Key { frame, ..key };
            let image = orbs.images.get(&key).cloned();
            if image.is_none() {
                orbs.wanted.borrow_mut().insert(key);
            }
            image
        };
        let Some(still) = get(0) else {
            return holder.child(fallback).into_any_element();
        };
        if moving {
            let frames: Vec<Option<Arc<RenderImage>>> = (1..=FRAMES).map(get).collect();
            // The loop plays once each of its images is there; until then the orb stands still.
            if let Some(frames) = frames.into_iter().collect::<Option<Vec<_>>>() {
                let device = key.device;
                let period = Duration::from_secs_f32(2. * f32::from(FRAMES) / MOVING_FPS);
                return holder
                    .with_animation(id, Animation::new(period).repeat().with_max_fps(MOVING_FPS.min(motion::WAIT_FPS)), move |holder, t| {
                        holder.child(painted(frames[usize::from(frame_at(t) - 1)].clone(), device))
                    })
                    .into_any_element();
            }
        }
        holder.child(painted(still, key.device)).into_any_element()
    }

    /// At the end of a frame: make the orb images its draw asked for and did not have, each on a
    /// background thread, and draw again when one is there.
    pub(crate) fn orbs_make(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let orbs = &mut self.manager.orbs;
        orbs.scale_factor.set(window.scale_factor());
        let wanted: Vec<Key> = orbs.wanted.borrow_mut().drain().collect();
        for key in wanted {
            if orbs.images.contains_key(&key) || !orbs.making.insert(key) {
                continue;
            }
            let task = cx.background_spawn(async move { make(key) });
            cx.spawn(async move |this, cx| {
                let made = task.await;
                let _ = this.update(cx, |ws, cx| {
                    ws.manager.orbs.making.remove(&key);
                    match made {
                        Some(image) => {
                            ws.manager.orbs.images.insert(key, image);
                            cx.notify();
                        }
                        None => log::warn!("orb: an image could not be made"),
                    }
                });
            })
            .detach();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_moving_orb_goes_forward_and_back_over_each_image() {
        assert_eq!(frame_at(0.), 1);
        assert_eq!(frame_at(0.5), FRAMES);
        assert_eq!(frame_at(1.), 1);
        assert_eq!(frame_at(0.25), frame_at(0.75), "the way back is the way there");
        let seen: HashSet<u8> = (0..=100).map(|n| frame_at(n as f32 / 100.)).collect();
        assert_eq!(seen.len(), usize::from(FRAMES));
        assert!(seen.iter().all(|frame| (1..=FRAMES).contains(frame)));
        assert_eq!(frame_at(7.), 1, "a time past the loop is its end");
    }

    #[test]
    fn an_image_is_as_large_as_its_box_on_the_screen_and_never_huge() {
        // 44 logical pixels at 100 % on a display of 1.5: the box and the outset, in device pixels.
        assert_eq!(device_side(44., 1., 1.5), 81);
        assert_eq!(device_side(18., 1.3, 1.), 29);
        assert_eq!(device_side(4000., 1., 2.), MOST_DEVICE);
        assert_eq!(device_side(0., 1., 1.), 1);
    }

    #[test]
    fn the_still_orb_is_calm_and_the_moving_one_is_not() {
        let key = Key { preset: 1, light: false, device: 40, frame: 0 };
        assert_eq!(key.moment(), (STILL_LEVEL, STILL_SECONDS));
        assert_eq!(key.preset(), Preset::Aurora);
        let (level, seconds) = Key { frame: 3, ..key }.moment();
        assert!(level > STILL_LEVEL && seconds > STILL_SECONDS);
        // An image is the orb's pixels with a border of one texel, and has a ball in it.
        let image = make(key).expect("an image");
        assert_eq!(image.size(0).width.0, 42);
        for preset in [Preset::Prism, Preset::Aurora, Preset::Dusk] {
            assert_eq!(Key { preset: preset_number(preset), ..key }.preset(), preset);
        }
    }
}
