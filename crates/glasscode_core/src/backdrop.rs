//! The moving backdrop behind the screens and the voice orb, as pixels (slice U04 of
//! `docs/ui-redesign-slices.md`). A port of `draw()` and `drawOrb()` in
//! `docs/ui-redesign-mock.html`, with its three looks. No window and no clock: the caller gives
//! the size and the time, so the application can make frames on a background thread and the
//! tests can check them.

use std::f32::consts::PI;

use serde::{Deserialize, Serialize};

/// The three looks of the mock: a ribbon of colour across a haze.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Preset {
    #[default]
    Prism,
    Aurora,
    Dusk,
}

/// How the colour is drawn: smooth, or in three levels with a 4 x 4 ordered dither.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Finish {
    #[default]
    Smooth,
    Dither,
}

/// An image as BGRA bytes, 4 for each pixel, row by row from the top, with straight (not
/// premultiplied) alpha.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

impl Frame {
    /// No pixels: one of the sides is 0.
    fn empty(width: u32, height: u32) -> Frame {
        Frame {
            width,
            height,
            bgra: Vec::new(),
        }
    }

    /// The BGRA bytes of one pixel.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * self.width + x) * 4) as usize;
        [
            self.bgra[at],
            self.bgra[at + 1],
            self.bgra[at + 2],
            self.bgra[at + 3],
        ]
    }
}

/// The colours of a look (`PAL` in the mock): the haze above the ribbon, its lit edge, the three
/// colours across the band, 0 to 255, and where the ribbon lies (its slope, and how far down its
/// left end is).
struct Look {
    haze: [f32; 3],
    edge: [f32; 3],
    band: [[f32; 3]; 3],
    slope: f32,
    top: f32,
}

const PRISM: Look = Look {
    haze: [18., 70., 170.],
    edge: [170., 215., 255.],
    band: [[255., 236., 170.], [255., 150., 30.], [236., 44., 24.]],
    slope: 0.5,
    top: 0.1,
};

const AURORA: Look = Look {
    haze: [10., 90., 90.],
    edge: [150., 255., 210.],
    band: [[210., 255., 190.], [60., 200., 170.], [90., 60., 200.]],
    slope: 0.3,
    top: 0.22,
};

const DUSK: Look = Look {
    haze: [70., 40., 150.],
    edge: [210., 170., 255.],
    band: [[255., 214., 230.], [240., 96., 150.], [120., 40., 170.]],
    slope: 0.22,
    top: 0.3,
};

impl Preset {
    pub const ALL: [Preset; 3] = [Preset::Prism, Preset::Aurora, Preset::Dusk];

    pub fn name(self) -> &'static str {
        match self {
            Preset::Prism => "Prism",
            Preset::Aurora => "Aurora",
            Preset::Dusk => "Dusk",
        }
    }

    fn look(self) -> &'static Look {
        match self {
            Preset::Prism => &PRISM,
            Preset::Aurora => &AURORA,
            Preset::Dusk => &DUSK,
        }
    }
}

/// The mock's 4 x 4 Bayer table.
const BAYER: [u8; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];

/// The dither's levels in each channel, above black.
const LEVELS: f32 = 3.;

/// The dither threshold of a pixel, 0 to 1.
fn threshold(x: usize, y: usize) -> f32 {
    (BAYER[(y & 3) * 4 + (x & 3)] as f32 + 0.5) / 16.
}

/// A channel of 0 to 1 in `LEVELS` steps, with the pixel's threshold.
fn dither(value: f32, threshold: f32) -> f32 {
    ((value * LEVELS + threshold).floor() / LEVELS).min(1.)
}

/// One frame of the backdrop at `seconds`, `width` x `height` texels: the mock's `draw()`, with
/// a soft top edge (see below). `strength` dims the ribbon (Today 0.85, sessions 0.6, Settings 0.8, the first run 1.0). In
/// the light theme the colour goes on white. Every alpha byte is 255.
pub fn render(
    preset: Preset,
    finish: Finish,
    light: bool,
    strength: f32,
    seconds: f32,
    width: u32,
    height: u32,
) -> Frame {
    if width == 0 || height == 0 {
        return Frame::empty(width, height);
    }
    let look = preset.look();
    let (w, h) = (width as usize, height as usize);
    let aspect = width as f32 / height as f32;
    let t = seconds;
    // For each column: the ribbon's top, its thickness, and its brightness.
    let columns: Vec<(f32, f32, f32)> = (0..w)
        .map(|x| {
            let u = x as f32 / w as f32;
            let top = look.top
                + look.slope * u * aspect.min(2.) / 1.6
                + 0.05 * (u * 3.1 + t * 0.23).sin()
                + 0.02 * (u * 7.3 - t * 0.31).sin();
            let thick = 0.085 + 0.035 * (u * 2.3 - t * 0.17).sin();
            let fade =
                (0.55 + 0.45 * (u * PI * 0.9 + 0.35 + 0.2 * (t * 0.11).sin()).sin()) * strength;
            (top, thick, fade * 0.92)
        })
        .collect();
    let mut bgra = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let v = y as f32 / h as f32;
        for (x, &(top, thick, fade)) in columns.iter().enumerate() {
            let q = (v - top) / thick;
            let mut rgb = ribbon(look, q);
            // The texel that the ribbon's top edge crosses. The colour jumps there, and an
            // image stretched over the window shows each row of texels as a step (6 pixels in
            // the smooth finish). Mixing the two sides by how much of the texel lies below the
            // edge keeps the edge where it is, without the steps.
            let span = 1. / (h as f32 * thick);
            if q < 0. && q + span > 0. {
                let below = (q + span) / span;
                let band = ribbon(look, 0.);
                rgb = [0, 1, 2].map(|i| rgb[i] * (1. - below) + band[i] * below);
            }
            let rgb = rgb.map(|c| c / 255. * fade);
            let rgb = match finish {
                Finish::Smooth => rgb,
                Finish::Dither => {
                    let threshold = threshold(x, y);
                    rgb.map(|c| dither(c, threshold))
                }
            };
            bgra.extend_from_slice(&on_ground(rgb, light, 255));
        }
    }
    Frame {
        width,
        height,
        bgra,
    }
}

/// The colour at `q` across the ribbon (0 at its top edge, 1 at its bottom), 0 to 255.
fn ribbon(look: &Look, q: f32) -> [f32; 3] {
    if q < 0. {
        // Above it: the haze, lit near the edge.
        let (k, e) = ((q * 0.95).exp() * 0.8, (q * 3.2).exp());
        [0, 1, 2].map(|i| look.haze[i] * k + (look.edge[i] - look.haze[i]) * e)
    } else if q < 1. {
        let s = q * 2.;
        let (a, b, f) = if s < 1. {
            (look.band[0], look.band[1], s)
        } else {
            (look.band[1], look.band[2], s - 1.)
        };
        [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * f)
    } else {
        // Below it: the last colour of the band, fading out.
        let k = (-(q - 1.) * 4.5).exp();
        look.band[2].map(|c| c * k)
    }
}

/// A colour of 0 to 1 in each channel on the theme's ground, as BGRA: in the dark theme it
/// lightens a near black, in the light theme it goes on white (its strongest channel at full).
fn on_ground([r, g, b]: [f32; 3], light: bool, alpha: u8) -> [u8; 4] {
    let [r, g, b] = if light {
        let base = 255. * (1. - r.max(g).max(b).min(1.));
        [base + r * 255., base + g * 255., base + b * 255.]
    } else {
        [9. + r * 246., 9. + g * 246., 11. + b * 244.]
    };
    [byte(b), byte(g), byte(r), alpha]
}

/// As a canvas stores it: rounded, and cut to 0 to 255.
fn byte(value: f32) -> u8 {
    value.round().clamp(0., 255.) as u8
}

/// A box blur of three passes in each direction, with the edge pixel repeated past the sides.
/// Three box passes come close to a Gaussian blur of about `radius` texels.
pub fn blur(frame: &Frame, radius: u32) -> Frame {
    let (w, h) = (frame.width as usize, frame.height as usize);
    if w == 0 || h == 0 || radius == 0 {
        return frame.clone();
    }
    let radius = radius as usize;
    let mut a = frame.bgra.clone();
    let mut b = vec![0u8; a.len()];
    for _ in 0..3 {
        // Along the rows, then along the columns.
        box_pass(&a, &mut b, h, w, w, 1, radius);
        box_pass(&b, &mut a, w, h, 1, w, radius);
    }
    Frame {
        width: frame.width,
        height: frame.height,
        bgra: a,
    }
}

/// One pass of a box blur over `lines` lines of `len` pixels each. Pixel `i` of line `line` is
/// at `line * line_step + i * step`, counted in pixels.
fn box_pass(
    src: &[u8],
    dst: &mut [u8],
    lines: usize,
    len: usize,
    line_step: usize,
    step: usize,
    radius: usize,
) {
    let span = (2 * radius + 1) as u32;
    let last = len as isize - 1;
    for line in 0..lines {
        let at = |i: isize| (line * line_step + i.clamp(0, last) as usize * step) * 4;
        let mut sum = [0u32; 4];
        for i in -(radius as isize)..=radius as isize {
            let p = at(i);
            for c in 0..4 {
                sum[c] += src[p + c] as u32;
            }
        }
        for i in 0..len as isize {
            let out = at(i);
            let (add, sub) = (at(i + radius as isize + 1), at(i - radius as isize));
            for c in 0..4 {
                dst[out + c] = ((sum[c] + span / 2) / span) as u8;
                sum[c] = sum[c] + src[add + c] as u32 - src[sub + c] as u32;
            }
        }
    }
}

/// `frame` with a border of one texel that repeats its edge. The sampler of the graphics card
/// is bilinear, and images sit next to each other in its atlas: without the border, the outer
/// pixels of a scaled image take colour from the next one (U02, proof 2).
pub fn pad(frame: &Frame) -> Frame {
    if frame.width == 0 || frame.height == 0 {
        return frame.clone();
    }
    let (w, h) = (frame.width as usize, frame.height as usize);
    let mut bgra = Vec::with_capacity((w + 2) * (h + 2) * 4);
    for y in 0..h + 2 {
        let y = y.saturating_sub(1).min(h - 1);
        let row = &frame.bgra[y * w * 4..(y + 1) * w * 4];
        bgra.extend_from_slice(&row[..4]);
        bgra.extend_from_slice(row);
        bgra.extend_from_slice(&row[row.len() - 4..]);
    }
    Frame {
        width: frame.width + 2,
        height: frame.height + 2,
        bgra,
    }
}

/// `frame` with each texel as a `cell` x `cell` square, cut to `width` x `height` pixels, with a
/// border of one pixel that repeats the edge (as `pad`). For the dither finish: painted one
/// pixel to one device pixel, the bilinear sampler has nothing to soften.
pub fn enlarge(frame: &Frame, cell: u32, width: u32, height: u32) -> Frame {
    if frame.width == 0 || frame.height == 0 || width == 0 || height == 0 {
        return Frame::empty(0, 0);
    }
    let cell = cell.max(1);
    let (pw, ph) = (width + 2, height + 2);
    let texel =
        |x: u32, size: u32, texels: u32| (x.saturating_sub(1).min(size - 1) / cell).min(texels - 1);
    let mut bgra = Vec::with_capacity((pw * ph * 4) as usize);
    let mut row = Vec::with_capacity((pw * 4) as usize);
    let mut row_of = None;
    for y in 0..ph {
        let ty = texel(y, height, frame.height);
        // A row of the result is the same for each pixel row of a cell.
        if row_of != Some(ty) {
            row.clear();
            for x in 0..pw {
                let at = ((ty * frame.width + texel(x, width, frame.width)) * 4) as usize;
                row.extend_from_slice(&frame.bgra[at..at + 4]);
            }
            row_of = Some(ty);
        }
        bgra.extend_from_slice(&row);
    }
    Frame {
        width: pw,
        height: ph,
        bgra,
    }
}

/// Logical pixels for each texel of the smooth backdrop (the mock's cell).
const CELL: f32 = 6.;

/// The most texels of the smooth backdrop: enough for a window 1920 pixels wide (U02).
pub const MAX_TEXELS: (u32, u32) = (320, 200);

/// The smooth backdrop's size in texels for an area of `width` x `height` logical pixels: one
/// texel for each 6 pixels, at most `MAX_TEXELS` with the area's shape kept, at least 2 x 2.
pub fn texels_for(width: f32, height: f32) -> (u32, u32) {
    let (w, h) = (width.max(0.) / CELL, height.max(0.) / CELL);
    let shrink = (MAX_TEXELS.0 as f32 / w)
        .min(MAX_TEXELS.1 as f32 / h)
        .min(1.);
    (
        (w * shrink).round().max(2.) as u32,
        (h * shrink).round().max(2.) as u32,
    )
}

/// The voice orb at `seconds`, `size` x `size` texels: the mock's `drawOrb()`. A lit ball with
/// the look's ribbon inside, which moves more as `level` (the microphone, 0 to 1) rises. Outside
/// the ball the smooth finish has a soft glow in the edge colour and the dither finish nothing:
/// there the alpha is below 255. Outside the circle that touches the frame's sides the alpha is
/// 0. A `level` outside 0 to 1 counts as its nearest end, and one that is not a number as 0.
pub fn orb(
    preset: Preset,
    finish: Finish,
    light: bool,
    level: f32,
    seconds: f32,
    size: u32,
) -> Frame {
    if size == 0 {
        return Frame::empty(size, size);
    }
    let look = preset.look();
    let t = seconds;
    let smooth = finish == Finish::Smooth;
    // `clamp` keeps a NaN, and a NaN would draw no ball at all.
    let level = if level.is_nan() {
        0.
    } else {
        level.clamp(0., 1.)
    };
    let level = level * (0.7 + 0.3 * (t * 8.3).sin() * (t * 3.1).sin());
    // The ball's radius, with the frame's half side as 1.
    let radius = 0.8 + 0.05 * level;
    // The mock's glow reaches 0.22 of the radius past the ball, which at a high level is past
    // the frame's side: there the canvas cut it straight. It ends at the circle instead.
    let glow_width = 0.22f32.min(1. / radius - 1.);
    let n = size as usize;
    // One texel, in the units of `rr` (the ball's radius as 1).
    let texel = 2. / n as f32 / radius;
    let edge = look.edge.map(byte);
    let mut bgra = Vec::with_capacity(n * n * 4);
    for y in 0..n {
        for x in 0..n {
            let u = (x as f32 + 0.5) / n as f32 * 2. - 1.;
            let v = (y as f32 + 0.5) / n as f32 * 2. - 1.;
            // Outside the circle that touches the sides nothing shows: in a frame of a few
            // texels the ball's soft edge, one texel wide, would reach past it.
            if u.hypot(v) >= 1. {
                bgra.extend_from_slice(&[edge[2], edge[1], edge[0], 0]);
                continue;
            }
            let rr = u.hypot(v) / radius;
            // How much of the texel the ball covers. The smooth finish blends the ball's edge
            // over one texel: the image is stretched several times over, and a hard edge shows
            // each texel as a step. The dither finish keeps its hard edge, as its cells are seen.
            let cover = if smooth {
                ((1. - rr) / texel + 0.5).clamp(0., 1.)
            } else if rr < 1. {
                1.
            } else {
                0.
            };
            // Outside the ball the smooth finish glows in the edge colour.
            let glow = if smooth && cover < 1. {
                let a = (1. - (rr.max(1.) - 1.) / glow_width).max(0.);
                a * a * (0.25 + 0.5 * level) * if light { 0.6 } else { 1. }
            } else {
                0.
            };
            if cover <= 0. {
                bgra.extend_from_slice(&[edge[2], edge[1], edge[0], byte(glow * 255.)]);
                continue;
            }
            let rgb = ball(look, level, t, u, v, rr.min(1.));
            let rgb = if smooth {
                rgb
            } else {
                let threshold = threshold(x, y);
                rgb.map(|c| dither(c, threshold))
            };
            let inside = on_ground(rgb, light, 255);
            if cover >= 1. {
                bgra.extend_from_slice(&inside);
                continue;
            }
            // The ball over its glow, with straight alpha.
            let alpha = cover + (1. - cover) * glow;
            let [b, g, r] = [0, 1, 2].map(|i| {
                let glow_channel = edge[2 - i] as f32 * (1. - cover) * glow;
                byte((inside[i] as f32 * cover + glow_channel) / alpha)
            });
            bgra.extend_from_slice(&[b, g, r, byte(alpha * 255.)]);
        }
    }
    Frame {
        width: size,
        height: size,
        bgra,
    }
}

/// The colour of the orb's ball at `(u, v)`, `rr` from its centre (its radius as 1), 0 to 1 in
/// each channel: the look's ribbon on a lit sphere, as the mock's `drawOrb()`.
fn ball(look: &Look, level: f32, t: f32, u: f32, v: f32, rr: f32) -> [f32; 3] {
    let z = (1. - rr * rr).max(0.).sqrt();
    let k = 0.55 + 0.45 * z;
    let (su, sv) = (u / k, v / k);
    let line = 0.3 * su * (t * 0.45).sin()
        + (0.1 + 0.22 * level) * (su * 3.1 + t * 1.6).sin()
        + 0.07 * (su * 6.3 - t * 2.2 + sv * 2.1).sin();
    let q = (sv - line) / (0.24 + 0.2 * level);
    let rgb = if q < 0. {
        let (kk, e) = ((q * 1.1).exp(), (q * 3.4).exp());
        [0, 1, 2].map(|i| look.haze[i] * kk + (look.edge[i] - look.haze[i]) * e)
    } else if q < 1. {
        let s = q * 2.;
        let (a, b, f) = if s < 1. {
            (look.band[0], look.band[1], s)
        } else {
            (look.band[1], look.band[2], s - 1.)
        };
        [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * f)
    } else {
        let kk = (-(q - 1.) * 2.2).exp();
        look.band[2].map(|c| c * kk)
    };
    let (shade, rim) = (0.5 + 0.5 * z, (1. - z).powi(3) * 0.7);
    [0, 1, 2].map(|i| ((rgb[i] * shade + look.haze[i] * 0.45 + look.edge[i] * rim) / 255.).min(1.))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const SECONDS: [f32; 4] = [0., 7., 23., 61.];

    fn every_look() -> impl Iterator<Item = (Preset, Finish, bool)> {
        Preset::ALL.into_iter().flat_map(|preset| {
            [Finish::Smooth, Finish::Dither]
                .into_iter()
                .flat_map(move |finish| [false, true].map(|light| (preset, finish, light)))
        })
    }

    #[test]
    fn the_same_input_gives_the_same_bytes() {
        for (preset, finish, light) in every_look() {
            for seconds in SECONDS {
                let a = render(preset, finish, light, 0.85, seconds, 96, 60);
                let b = render(preset, finish, light, 0.85, seconds, 96, 60);
                assert_eq!(a, b, "{preset:?} {finish:?} light {light} at {seconds} s");
                assert_eq!(a.bgra.len(), 96 * 60 * 4);
            }
        }
        // And the time moves it.
        let (a, b) = (
            render(Preset::Prism, Finish::Smooth, false, 0.85, 0., 96, 60),
            render(Preset::Prism, Finish::Smooth, false, 0.85, 7., 96, 60),
        );
        assert_ne!(a, b);
        assert_eq!(
            orb(Preset::Dusk, Finish::Smooth, false, 0.4, 3., 40),
            orb(Preset::Dusk, Finish::Smooth, false, 0.4, 3., 40)
        );
    }

    #[test]
    fn each_pixel_is_opaque() {
        for (preset, finish, light) in every_look() {
            for seconds in SECONDS {
                let frame = render(preset, finish, light, 1.0, seconds, 80, 50);
                assert!(
                    frame.bgra.chunks(4).all(|p| p[3] == 255),
                    "{preset:?} {finish:?} light {light} at {seconds} s"
                );
            }
        }
    }

    #[test]
    fn in_the_light_theme_the_ground_is_white() {
        // The bottom left corner is far below the ribbon in each look.
        for preset in Preset::ALL {
            for finish in [Finish::Smooth, Finish::Dither] {
                for seconds in SECONDS {
                    let frame = render(preset, finish, true, 0.85, seconds, 320, 200);
                    assert_eq!(
                        frame.pixel(0, 199),
                        [255, 255, 255, 255],
                        "{preset:?} {finish:?} at {seconds} s"
                    );
                }
            }
        }
        // In the dark theme the same place is the near black ground.
        let frame = render(Preset::Prism, Finish::Smooth, false, 0.85, 0., 320, 200);
        assert_eq!(frame.pixel(0, 199), [11, 9, 9, 255]);
    }

    #[test]
    fn dither_has_at_most_four_values_in_each_channel() {
        for preset in Preset::ALL {
            for light in [false, true] {
                for seconds in SECONDS {
                    let frame = render(preset, Finish::Dither, light, 1.0, seconds, 160, 100);
                    for channel in 0..3 {
                        let values: BTreeSet<u8> =
                            frame.bgra.chunks(4).map(|p| p[channel]).collect();
                        assert!(
                            values.len() <= 4,
                            "{preset:?} light {light} at {seconds} s, channel {channel}: {values:?}"
                        );
                    }
                }
            }
        }
        // The smooth finish has many.
        let frame = render(Preset::Prism, Finish::Smooth, false, 1.0, 0., 160, 100);
        assert!(
            frame
                .bgra
                .chunks(4)
                .map(|p| p[2])
                .collect::<BTreeSet<u8>>()
                .len()
                > 20
        );
    }

    #[test]
    fn a_blur_of_one_colour_is_that_colour() {
        let flat = Frame {
            width: 37,
            height: 23,
            bgra: [40, 120, 200, 255].repeat(37 * 23),
        };
        for radius in [1, 2, 3, 9, 40] {
            assert_eq!(blur(&flat, radius), flat, "radius {radius}");
        }
    }

    #[test]
    fn a_blur_spreads_a_line_and_keeps_its_weight() {
        // A white column on black: after the blur it is wider and dimmer, and the row's sum stays
        // about the same.
        let (w, h) = (31u32, 5u32);
        let mut bgra = [0, 0, 0, 255].repeat((w * h) as usize);
        for y in 0..h {
            let at = ((y * w + 15) * 4) as usize;
            bgra[at..at + 3].copy_from_slice(&[255, 255, 255]);
        }
        let out = blur(
            &Frame {
                width: w,
                height: h,
                bgra,
            },
            2,
        );
        let row: Vec<u32> = (0..w).map(|x| out.pixel(x, 2)[0] as u32).collect();
        assert!(row[15] < 255 && row[13] > 0 && row[17] > 0, "{row:?}");
        assert_eq!(row[0], 0, "{row:?}");
        assert!((240..=270).contains(&row.iter().sum::<u32>()), "{row:?}");
    }

    #[test]
    fn nothing_to_draw_gives_an_empty_frame() {
        for (w, h) in [(0, 0), (0, 10), (10, 0)] {
            let frame = render(Preset::Aurora, Finish::Dither, false, 0.8, 1., w, h);
            assert!(frame.bgra.is_empty(), "{w} x {h}");
            assert!(blur(&frame, 3).bgra.is_empty());
            assert!(pad(&frame).bgra.is_empty());
            assert!(enlarge(&frame, 3, 10, 10).bgra.is_empty());
        }
        assert!(
            orb(Preset::Prism, Finish::Smooth, false, 0.5, 0., 0)
                .bgra
                .is_empty()
        );
        let one = render(Preset::Prism, Finish::Smooth, false, 0.8, 1., 1, 1);
        assert_eq!(one.bgra.len(), 4);
        assert_eq!(blur(&one, 3), one);
    }

    #[test]
    fn the_border_repeats_the_edge() {
        let px = |i: u8| [i, i, i, 255];
        let frame = Frame {
            width: 3,
            height: 2,
            bgra: [px(1), px(2), px(3), px(4), px(5), px(6)].concat(),
        };
        let padded = pad(&frame);
        assert_eq!((padded.width, padded.height), (5, 4));
        let row = |y: u32| (0..5).map(|x| padded.pixel(x, y)[0]).collect::<Vec<u8>>();
        assert_eq!(row(0), [1, 1, 2, 3, 3]);
        assert_eq!(row(1), [1, 1, 2, 3, 3]);
        assert_eq!(row(2), [4, 4, 5, 6, 6]);
        assert_eq!(row(3), [4, 4, 5, 6, 6]);
    }

    #[test]
    fn enlarge_makes_squares_and_a_border() {
        let px = |i: u8| [i, i, i, 255];
        let frame = Frame {
            width: 2,
            height: 2,
            bgra: [px(1), px(2), px(3), px(4)].concat(),
        };
        // Cells of 3 pixels, cut to 5 x 4, plus the border: 7 x 6.
        let big = enlarge(&frame, 3, 5, 4);
        assert_eq!((big.width, big.height), (7, 6));
        let row = |y: u32| (0..7).map(|x| big.pixel(x, y)[0]).collect::<Vec<u8>>();
        assert_eq!(row(0), [1, 1, 1, 1, 2, 2, 2]);
        assert_eq!(row(3), [1, 1, 1, 1, 2, 2, 2]);
        assert_eq!(row(4), [3, 3, 3, 3, 4, 4, 4]);
        assert_eq!(row(5), [3, 3, 3, 3, 4, 4, 4]);
        // A dither frame keeps its values.
        let dither = render(Preset::Dusk, Finish::Dither, false, 1.0, 5., 40, 25);
        let values = |f: &Frame| f.bgra.chunks(4).map(|p| p[1]).collect::<BTreeSet<u8>>();
        assert_eq!(values(&enlarge(&dither, 5, 200, 125)), values(&dither));
    }

    #[test]
    fn the_size_follows_the_area_up_to_the_most() {
        assert_eq!(texels_for(1920., 1200.), (320, 200));
        assert_eq!(texels_for(1440., 900.), (240, 150));
        assert_eq!(texels_for(2560., 1600.), (320, 200));
        // A wide area keeps its shape.
        assert_eq!(texels_for(3840., 1000.), (320, 83));
        assert_eq!(texels_for(900., 516.), (150, 86));
        assert_eq!(texels_for(0., 0.), (2, 2));
        assert_eq!(texels_for(f32::NAN, -5.), (2, 2));
    }

    #[test]
    fn the_orb_is_a_lit_ball() {
        for (preset, finish, light) in every_look() {
            let frame = orb(preset, finish, light, 0.5, 2., 60);
            assert_eq!(
                (frame.width, frame.height, frame.bgra.len()),
                (60, 60, 60 * 60 * 4)
            );
            assert_eq!(frame.pixel(30, 30)[3], 255, "the middle is opaque");
            // The corner is outside the ball and its glow.
            assert_eq!(
                frame.pixel(0, 0)[3],
                0,
                "{preset:?} {finish:?} light {light}"
            );
        }
        // Just outside the ball the smooth finish glows and the dither finish does not.
        let ring = |finish| orb(Preset::Prism, finish, false, 0.5, 2., 100).pixel(50, 4)[3];
        assert!(ring(Finish::Smooth) > 0);
        assert_eq!(ring(Finish::Dither), 0);
    }

    const LEVELS_ODD: [f32; 8] = [
        0.,
        0.5,
        1.,
        -5.,
        50.,
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
    ];

    /// Each pixel whose centre lies on or outside the circle that touches the frame's sides.
    fn outside_the_circle(size: u32) -> impl Iterator<Item = (u32, u32)> {
        let side = size as f32;
        (0..size).flat_map(move |y| {
            (0..size).filter_map(move |x| {
                let u = (x as f32 + 0.5) / side * 2. - 1.;
                let v = (y as f32 + 0.5) / side * 2. - 1.;
                (u.hypot(v) >= 1.).then_some((x, y))
            })
        })
    }

    #[test]
    fn the_orb_is_clear_outside_its_circle() {
        for (preset, finish, light) in every_look() {
            for level in LEVELS_ODD {
                for seconds in [0., 1.3, 7., 23.] {
                    for size in (1..=16).chain([60, 112]) {
                        let frame = orb(preset, finish, light, level, seconds, size);
                        assert_eq!(frame.bgra.len(), (size * size * 4) as usize);
                        for (x, y) in outside_the_circle(size) {
                            assert_eq!(
                                frame.pixel(x, y)[3],
                                0,
                                "{preset:?} {finish:?} light {light}, level {level} at {seconds} s, {size} texels: ({x}, {y})"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_orb_gives_the_same_bytes_for_the_same_input() {
        for (preset, finish, light) in every_look() {
            for level in [0., 0.3, 1.] {
                for seconds in SECONDS {
                    assert_eq!(
                        orb(preset, finish, light, level, seconds, 48),
                        orb(preset, finish, light, level, seconds, 48),
                        "{preset:?} {finish:?} light {light}, level {level} at {seconds} s"
                    );
                }
            }
        }
        // The time and the level move it.
        let at = |level, seconds| orb(Preset::Aurora, Finish::Smooth, false, level, seconds, 48);
        assert_ne!(at(0.5, 0.), at(0.5, 1.));
        assert_ne!(at(0.1, 1.), at(0.9, 1.));
    }

    #[test]
    fn a_level_out_of_range_draws_as_its_nearest_end() {
        for (preset, finish, light) in every_look() {
            let at = |level| orb(preset, finish, light, level, 3., 40);
            assert_eq!(at(50.), at(1.), "{preset:?} {finish:?} light {light}");
            assert_eq!(at(f32::INFINITY), at(1.));
            assert_eq!(at(-5.), at(0.));
            assert_eq!(at(f32::NEG_INFINITY), at(0.));
            assert_eq!(at(f32::NAN), at(0.));
        }
    }

    #[test]
    fn the_smooth_ball_has_a_soft_edge_and_the_dither_ball_a_hard_one() {
        // The glow is at most 0.75 opaque: above that and below full lies the ball's soft edge.
        let edge = |finish| {
            orb(Preset::Dusk, finish, false, 0.5, 4., 112)
                .bgra
                .chunks(4)
                .filter(|p| p[3] > 192 && p[3] < 255)
                .count()
        };
        assert!(edge(Finish::Smooth) > 100, "{}", edge(Finish::Smooth));
        assert_eq!(edge(Finish::Dither), 0);
    }

    #[test]
    fn a_louder_level_makes_a_larger_ball() {
        let opaque = |level| {
            orb(Preset::Prism, Finish::Smooth, false, level, 5., 112)
                .bgra
                .chunks(4)
                .filter(|p| p[3] == 255)
                .count()
        };
        assert!(
            opaque(1.) > opaque(0.),
            "{} against {}",
            opaque(1.),
            opaque(0.)
        );
    }

    #[test]
    fn the_dither_orb_has_the_backdrops_levels() {
        for preset in Preset::ALL {
            for light in [false, true] {
                for level in [0., 0.6, 1.] {
                    let frame = orb(preset, Finish::Dither, light, level, 9., 80);
                    // The ball only: outside it the colour does not show (alpha 0).
                    let ball: Vec<&[u8]> = frame.bgra.chunks(4).filter(|p| p[3] == 255).collect();
                    assert!(!ball.is_empty());
                    for channel in 0..3 {
                        let values: BTreeSet<u8> = ball.iter().map(|p| p[channel]).collect();
                        assert!(
                            values.len() <= 4,
                            "{preset:?} light {light}, level {level}, channel {channel}: {values:?}"
                        );
                    }
                    assert!(frame.bgra.chunks(4).all(|p| p[3] == 0 || p[3] == 255));
                }
            }
        }
    }
}
