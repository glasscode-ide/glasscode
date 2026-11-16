# UI Redesign: Proofs (U02)

| | |
|---|---|
| Document | GC-UI-002, issue 1 |
| Date | 2026-11-16 |
| Slice | U02 of `docs/ui-redesign-slices.md` |
| Debug page | `GLASSCODE_DEBUG_OPEN=proofs[:<what>]`, code in `crates/glasscode/src/proofs.rs` |
| Used by | U04 (backdrop and glass), U05 (global scale), decisions P5 and P6 |

## Contents

1. Summary
2. How the numbers were measured
3. Proof 1: backdrop
4. Proof 2: glass
5. Proof 3: dither
6. Proof 4: scale
7. What this document does not prove

## 1. Summary

| Proof | Measured (optimized build unless stated) | Recommendation |
|---|---|---|
| 1. Backdrop: a 320 x 200 image 15 times each second, `drop_image` on the old one | 602 s, 9101 frames (15.2 each second). Private bytes 173.2 → 173.1 MB, graphics memory 57.8 → 57.8 MB. Processor 5.97 % of one core, 3.5 % more than a still page. Without `drop_image`: +4.3 MB each second. | U04: an image of the window's logical size / 6 (at most 320 x 200) plus a border of one texel, 15 frames each second timed against a clock, the old image dropped one frame later. **U04's limit of 3 % is at risk** (see section 3): the image costs 3.5 %, and drawing the whole main window again 15 times each second costs about 5.4 % more (an indication: one pair of runs that counted `notify` calls, not draws). |
| 2. Glass: `paint_image` with the card's bounds, the backdrop's `image_bounds` and corner radii | The crop works, with rounded corners. Each edge of the content is off by up to half a texel (measured: up to 2 device pixels; 4 px over a card at most). Without a border, the outer 3 device pixels of the image take up to 45 % of the colour next to it in the atlas; with a border of one texel, none. Backdrop + blurred copy + 3 cards: 13.96 % of one core, the blur 4.6 ms a frame. | U04: one `canvas` for each card that calls `paint_image(card, backdrop_image_bounds, radius, blurred)`; a border of one texel on each image; a cheaper blur (half size, a lower rate). |
| 3. Dither at the full pixel size, 8 times each second | 1920 x 1080: 6.92 % of one core (+4.5 %), private +63 MB, graphics +25 MB. 3840 x 2160 image (painted into a 2560 x 1494 window): 24.04 % (+21.6 %), private +278 MB, graphics +140 MB. Memory flat. 1:1 is exact (4 values in each channel). | **P6: the full-size image, no vendor patch**; U04 lowers the dither rate above 4 million device pixels. The nearest-filter patch touches three vendor crates and the macOS shader. |
| 4. Scale | 1115 `px(` in 33 files, in ten slices; 1058 more sizes are GPUI short forms in rems. The zoom trial (11 lines in `gpui_windows`) gave sharp text, correct hit tests and a correct terminal; popups were cut until the display's scale factor was zoomed too. | **P5: the helper `theme::sz` and `window.set_rem_size`**, no vendor patch (from reading the code; the helper was not tried here); the zoom is a fallback for Windows. |

**Stop conditions.** The two conditions of U02 are not met: memory did not increase after
`drop_image`, and a crop with rounded corners works. **Section 10 item 6 of the plan applies**
("a measured cost is above the limit in the slice"): U04's limit is 3 % of one core for the
smooth backdrop, and this proof measured 3.5 % more than a still page, or 5.97 % for the whole
process (with the application's own ticks). The plan does not say which of the two it means;
both are above 3 %. The owner decides before U04 starts: a higher limit, a lower rate
(10 frames each second, about 2.3 %, computed), or both.

## 2. How the numbers were measured

**Machine.** A laptop with Windows 11 Pro (build 26200), a 6-core and 12-thread AMD processor,
an NVIDIA laptop graphics card and the processor's own graphics, and 16 GB of memory.
The main display is 2560 x 1600 pixels at 150 % (scale factor 1.5).
The second display is 3440 x 1440 pixels at 125 %.
No display can show a window of 3840 x 2160 pixels.

**Builds.**

| Name in this document | Command | Why |
|---|---|---|
| Optimized | `CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true cargo build --release -p glasscode` | The release profile, but with `debug_assertions` on, because the debug hooks work only then. The processor numbers come from this build. |
| Debug | `cargo build -p glasscode` | The build of the Verify command. Its pixel loops are not optimized, so its processor numbers are much higher than a release build. |

**Start.** Each run started `glasscode.exe` directly (not through `cargo run`, which gives the
process number of cargo), with these variables:

- `GLASSCODE_HOME=<an empty temporary folder>` with only `{"appearance":{"theme":"dark"}}` in
  `state.json`. No project, no session, no login is read.
- `GLASSCODE_DEBUG_BACKGROUND=hidden`, so the main window does not show and does not draw. The
  proofs page is the only window.
- `GLASSCODE_DEBUG_OPEN=proofs:<what>`.

The redraw runs of section 3 differ: the main window is shown, and the home has one project (a
test Git repository in a temporary folder) with one shell session.

The proofs window opens at 1280 x 800 logical pixels (1920 x 1200 device pixels at 150 %).
The dither page opens at 1280 x 720 (1920 x 1080 device pixels).

**Samples.** A PowerShell script read the process every 15 seconds:
`Get-Process` (`TotalProcessorTime`, `WorkingSet64`, `PrivateMemorySize64`) and the performance
counters `\GPU Process Memory(pid_<n>_*)\Dedicated Usage` and `Shared Usage`, summed over the
graphics adapters.
"Processor" is the processor time of the process in the run divided by the duration of the run,
in percent of one core (100 % is one full core of 12).
The first sample is 10 seconds after the start.

**Two sets of runs.** The first set used the first version of the page (a plain wait of 66 ms
between frames); the second set the version that waits until a clock time (section 3, item 4).
Each table says which set a row is from where it matters. Later changes to the page (the glass
colours of U01 on the demonstration cards, and the skip of a late frame) do not touch the
backdrop and dither runs: they painted every frame that they made (9101 of 9101, 1444 of 1444).
The demonstration run painted 2719 of 2732, and the redraw run (section 3) used the earlier loop
and asked for 15 calls each second but made 11.7.

**Frames.** The page counts the frames that it makes and the frames in which it painted the
image (in the paint callback), and writes the counts to the log each 30 seconds. The rates in
this document are the painted frames divided by the run time, not the rate the page asks for.

## 3. Proof 1: backdrop

**Task.** Paint a new `RenderImage` of 320 x 200 from a byte buffer 15 times each second for
10 minutes. Call `window.drop_image` for the old one. Record memory at the start and the end, and
the processor use.

**What the page does** (`proofs:backdrop`). Every 66 ms (timed against a clock, item 4) the page
makes the next frame of the Prism ribbon (the `draw()` mathematics of the mock, ported to Rust
in `proofs.rs`) as 320 x 200 BGRA bytes (250 KB), wraps it in `RenderImage::new(vec![Frame])`,
and paints it over the full window with `paint_image`.
The image before it is dropped at the next frame, not at once (see "Order of the drop").

**Results.** Window 1920 x 1200 device pixels. "Start" is the first sample (10 s after the
start of the process), "end" the last.

| Run | Build | Time | Frames painted (rate) | Processor, % of one core | Private bytes, MB, start → end | Working set, MB | Graphics memory (dedicated), MB |
|---|---|---|---|---|---|---|---|
| `proofs:still`: one frame only (baseline) | optimized | 180 s | 1 made | 2.43 | 171.9 → 171.7 | 81.9 → 81.5 | 39.1 → 48.4 |
| **`proofs:backdrop`** | optimized | **602 s** | 9101 (**15.2 each second**) | **5.97** | **173.2 → 173.1** | 80.4 → 79.8 | **57.8 → 57.8** |
| `proofs:backdrop`, first run (plain 66 ms wait) | optimized | 602 s | 7653 (12.7 each second) | 6.00 | 172.4 → 172.2 | 80.7 → 65.8 | 57.8 → 57.8 |
| `proofs:backdrop:keep`: no `drop_image` (control) | optimized | 120 s | 1544 (12.8 each second) | 6.15 | 214.7 → **728.4** | 80.8 → 80.6 | 97.8 → **609.8** |
| `proofs:backdrop` | debug | 180 s | 1953 (10.8 each second) | 20.14 | 173.3 → 173.3 | 90.1 → 90.2 | 57.8 → 57.8 |

The rows in bold are the run that the slice asks for: 10 minutes, 15 frames each second.

1. **Memory does not increase after `drop_image`.** Over 10 minutes and 9101 images, private
   bytes stayed at 173 MB (±0.2 MB between samples) and graphics memory at 57.8 MB.
2. **The measurement can see a leak.** Without `drop_image` the same page grew by 4.3 MB each
   second, in private bytes and in graphics memory: 12.8 tiles of 250 KB each second, in new
   atlas textures of 1024 x 1024 (4 MB). After 2 minutes it held 514 MB more.
3. **Processor.** 5.97 % of one core for the process, 3.5 % more than the baseline. Making a
   frame (the ribbon mathematics for 64 000 texels) took 1.01 ms on average, so 1.5 % of one
   core at 15 frames each second; the other 2.0 % is GPUI drawing the window and sending the
   image to the graphics card. The debug build needs 13.7 ms to make a frame and cannot keep
   15 frames each second; use an optimized build for numbers.
4. **Timers on Windows.** A loop of "wait 66 ms, then make a frame" gave 12.7 frames each second:
   a Windows timer ends on a tick of about 15.6 ms, so 66 ms becomes 78 ms. The page now waits
   until a clock time (`next += interval`), which gives 15.2. When a frame is late, it skips to
   the next slot: an earlier version made late frames at once, back to back, and in the debug
   build (65 ms to make a frame of the demonstration page) the window then drew 1 frame in 20 s.
   U04 must do both. (The optimized backdrop and dither runs painted every frame that they
   made.)
5. **Order of the drop.** The page drops the old image one frame later, after the new one has
   been painted (`Slot` in `proofs.rs`). Reason: the atlas frees a texture when its last tile
   goes (`directx_atlas.rs`, `remove`). If the old image goes before the new one is painted and
   no other picture is in the atlas, a texture of 4 MB is freed and made again 15 times each
   second. The cost is one more image of 250 KB.

**The whole window draws again.** The backdrop needs a new frame of the whole window 15 times
each second. In the proofs window that is a small page. To see the cost of the real window,
`proofs:redraw` calls `cx.notify()` on the main `Workspace` at the same rate, with no image
(main window shown at 2160 x 1350 device pixels, one project, one shell session, optimized
build, 181 s each):

| Run | Processor, % of one core |
|---|---|
| Main window shown, no `notify` (`proofs:still`) | 6.45 |
| Main window shown, `cx.notify()` on the `Workspace` 11.7 times each second (`proofs:redraw`, 2110 calls in 180 s) | 10.68 |

The page counted the `notify` calls, not the draws of the main window, so the number of draws
is not measured. If each call gave one draw, a draw of this main window costs about 3.6 ms of
processor time, and 15 each second cost about 5.4 % of one core. Take these two numbers as an
indication only: it is one pair of 3-minute runs, the loop asked for 15 calls each second and
made 11.7 (not explained), and the baseline's graphics memory moved from 42 to 66 MB in its
run. They are still more than the image itself costs, and a window with more sessions costs
more.

**Stop condition "memory increases after `drop_image`": not met.**

**Recommendation for U04 (backdrop).**

- Size: 320 x 200 texels is enough for a window of 1920 logical pixels (6 pixels for each texel,
  as the mock). Use the window's logical size / 6, at most 320 x 200, plus the border of one
  texel of section 4.
- Rate: 15 frames each second against a clock (item 4). The ribbon moves slowly; 10 frames each
  second would cost a third less if U04 needs room under its limit of 3 %.
- **The limit of 3 % is at risk.** The image alone costs 3.5 % of one core at 15 frames each
  second (1.5 % to make it, 2.0 % to draw a small page). If each frame draws the whole
  `Workspace` again, that adds about 5.4 % (the redraw run, an indication only). U04 must put the backdrop in an
  entity of its own, so that a new frame does not run `Workspace::render`, and measure the
  result in the real window. If it is still above 3 %, the choices are 10 frames each second,
  or a higher limit (the owner decides).
- Keep the current image and the image before it; drop the one before after the new one is
  painted (item 5). Do the same for the blurred image.
- Stop the frames when the window is not active or hidden (U04 step 4): the baseline row shows
  that a still page costs only the application's own ticks.

## 4. Proof 2: glass

**Task.** Call `paint_image` with the bounds of a card, `image_bounds` of the full backdrop, and
corner radii. Make sure that the card shows the correct part of the image, not stretched, with
rounded corners, and no colour bleed at the edges.

**What the page does** (`proofs:glass`). A test pattern of 320 x 200 texels (a colour ramp with a
light line every 20 texels) covers the window. Four cards at places and sizes that are not whole
pixels (for example 97.3, 83.6, 341.7 x 203.2) paint the same image with
`paint_image(card, full_window, Corners::all(18 px), image, 0, grey)`.
`:bare` leaves the cards out; `:grey` paints them in grey so that they can be seen;
`:pad` gives the image a border of one texel (see below).
A plain green image of the same size is painted under the pattern (where it is not seen), so
that it is the pattern's neighbour in the sprite atlas.

**How it was checked.** Screenshots of the window at 1920 x 1200 device pixels, with cards and
without cards (`:bare`). Inside each card (away from the rounded corners), a script compared the
two screenshots, and found the shift of the card's content that matches the backdrop best, for
the full card and for a band of 40 pixels at each edge.

**Results.** Window 1280 x 800 logical (1920 x 1200 device) pixels, so one texel is 4 logical
(6 device) pixels. Debug build; the result does not depend on the build.

| Card (logical pixels) | Crop error that the page computes (x, y) | Measured shift of the content, device pixels: left, right, top, bottom band | After the best whole-pixel shift |
|---|---|---|---|
| 1: 97.3, 83.6, 341.7 x 203.2 | 1.33, 1.33 px | (-2, 2), (1, 2), (-1, 1), (-1, 2) | not the same (a stretch) |
| 2: 511.5, 140.25, 280 x 160 | 0.67, 0.00 px | (1, 0) in each band | the same, pixel for pixel |
| 3: 180.8, 420.4, 500 x 220 | 0.67, 0.67 px | (-1, -1) in each band | the same, pixel for pixel |
| 4: 760.1, 400.7, 333.3 x 250.9 | 1.33, 0.67 px | (0, 0), (-2, 0), (-1, -1), (-1, 1) | not the same (a stretch) |

The page computes the error with the same rounding as `Window::paint_image` (`crop_error` in
`proofs.rs`), and the screenshots agree with it to the pixel (1.33 logical = 2 device pixels).

1. **Correct part of the image: yes, to within 0.5 texel at each edge.** `paint_image` rounds the
   start of the crop and its size to whole texels, each by itself (`vendor/gpui/src/window.rs`,
   the `sub_tile` calculation), then paints that part over the card. So each edge of the content
   can move by up to half a texel, and the far edge by up to one texel (a stretch of up to one
   texel over the card). With 320 texels over 1280 pixels that is up to 2 px at an edge and 4 px
   over a card. The largest that the four cards showed was 1.33 px.
2. **Rounded corners: yes.** The radii go to the sprite, and the shader cuts them with the same
   antialiased edge as a `div` (seen in the `:grey` screenshots).
3. **Colour bleed at the edges of the image: yes, without a border.** The outer 3 device pixels
   on each side of the full-window image mix with texels outside the image. At the left edge the
   first column was (17, 178, 52) instead of (30, 122, 90): about 45 % of the green neighbour
   image. At the other three edges the outer pixel had about 35 % of its value: (81, 45, 35)
   instead of (229, 122, 90), from empty atlas texels. The cause: the sampler is linear with the
   address mode WRAP (`directx_renderer.rs`), atlas tiles touch with no gap (`padding: 0` in
   `directx_atlas.rs`), and at 6 device pixels for each texel the first pixel samples less than
   half a texel from the tile's edge. The inside of a card does not bleed: next to a crop are
   the image's own texels.
4. **With a border of one texel (`:pad`) the bleed is gone.** The image is 322 x 202 with the edge
   texels repeated, and `image_bounds` is one texel larger on each side than the backdrop area.
   All four window edges then showed the exact colour, and the card crops were the same as
   without the border.

**Cost of glass.** The demonstration page (`proofs`) makes the backdrop and a blurred copy
(box blur, three passes each way, radius 3 texels) 15 times each second, and paints three glass
cards. Optimized build, 181 s: 15.1 frames each second, **13.96 %** of one core (11.5 % more
than the still page), memory flat (private 174.9 → 176.0 MB, graphics 57.8 → 57.8 MB).
Making a frame took 5.67 ms, of which the blur is about 4.6 ms (the backdrop alone takes
1.0 ms). The blur in `proofs.rs` is simple and not tuned.

**Stop condition "a crop with rounded corners is not possible": not met.**

**Recommendation for U04 (glass method).**

- Make each backdrop image (smooth and blurred) with a border of one texel that repeats the edge
  texels, and paint it with `image_bounds` = the backdrop area plus one texel on each side.
- A glass card is a `div` with, as its first child, an absolute `canvas` (full size) whose paint
  calls `window.paint_image(card_bounds, backdrop_image_bounds, Corners::all(radius), blurred, 0,
  false)`. The tint (`theme::glass()`), the border and the highlight go on top of it.
- The card needs the backdrop's `image_bounds` in window coordinates at paint time. When the
  backdrop covers the window, that is `window.viewport_size()` from the origin; otherwise keep
  the backdrop layer's bounds from its prepaint in the `Workspace`.
- The crop error (up to half a texel) is not visible under a blur and a tint of 64 % or more. If
  U04 wants it smaller, a blurred image with twice the texels halves it.
- The blur must cost much less than here: blur a copy at half size (160 x 100; the sampler scales
  it up, and the crop error becomes up to one texel of the full image), and make the blurred
  image at a lower rate than the backdrop (for example each third frame). Measure it against
  the limit of 3 % before the cards go into screens.

## 5. Proof 3: dither

**Task.** The sampler is bilinear, so a small dither image becomes soft. Measure a dither image at
the full pixel size of the window, 8 times each second: memory and processor use at 1920 x 1080
and at 3840 x 2160.

**What the page does** (`proofs:dither[:<w>x<h>]`). Every 125 ms the page makes a dither frame of
the ribbon (4 x 4 Bayer table, 3 levels, as in the mock) at the device pixel size of the window,
or at the size in the mode. The colour is worked out once for each dither cell of
round(3 x scale factor) device pixels (5 pixels at 150 %), and the cell is copied into the full
buffer. The image is painted 1:1 over the window.

**1:1 is exact.** In a screenshot of the 1920 x 1080 page, a crop of 1100 x 600 pixels had
exactly the 4 dither values in each channel (9, 91, 173, 255): no value from bilinear mixing.

**Results** (optimized build, 180 s each, memory flat from the first to the last sample in each
run). The extra memory is compared with the backdrop run of section 3 (private 173 MB, graphics
57.8 MB).

| Run | Image | Painted area | Rate | Processor, % of one core | Make a frame | Private bytes | Graphics memory, dedicated / shared |
|---|---|---|---|---|---|---|---|
| Baseline (`proofs:still`) | | | | 2.43 | | 172 MB | 39–48 MB / 2 MB |
| Smooth backdrop (section 3) | 320 x 200 | 1920 x 1200 | 15.2 | 5.97 | 1.0 ms | 173 MB | 57.8 MB / 2 MB |
| **`proofs:dither`** | **1920 x 1080** (7.9 MiB) | 1920 x 1080, 1:1 | 8.0 | **6.92** | 3.3 ms | **235 MB (+63)** | **83.0 MB (+25)** / 17 MB |
| `proofs:dither`, window maximized | 2560 x 1494 (14.6 MiB) | 2560 x 1494, 1:1 | 7.1 | 9.28 | 7.4 ms | 318–336 MB (+150) | 130.8 MB (+73) / 31 MB |
| **`proofs:dither:3840x2160`**, window maximized | **3840 x 2160** (31.6 MiB) | 2560 x 1494, scaled down | 8.0 | **24.04** | 20.2 ms | **451 MB (+278)** | **198.3 MB (+140)** / 65 MB |

The maximized run is from the first set of runs, with the plain 66 ms wait (7.1 frames each
second); the other dither rows ran at 8.0.

**Measured and computed for 3840 x 2160.** A window of 3840 x 2160 pixels was not possible
(section 2). The last row makes, uploads and paints a real 3840 x 2160 image 8 times each
second, so the processor time, the memory and the upload are measured. Only the area that the
graphics card fills is smaller (2560 x 1494). In a real 4K window the graphics card fills 2.2
times more pixels, which costs graphics time, not processor time; that part is not measured.
On a 4K display at 200 %, the dither cell is 6 device pixels, not 5, so making a frame costs a
little less than measured here.

**What the memory is made of.** Each full-size image is width x height x 4 bytes: 8.3 MB at
1920 x 1080 and 33.2 MB at 3840 x 2160. The process held 7.6 to 9.8 times that in private
bytes and 3.0 to 4.8 times in dedicated graphics memory (the three rows above): the image on
screen, the one before it and the one being made, each with its bytes in the process and its
own atlas texture (an image larger than 1024 x 1024 gets a texture of its own), and the
driver's copies for the upload. So, for a window of any size, count about **8 to 10 x the image
size** in private bytes and **3 to 5 x** in graphics memory.

**The other method: a nearest filter (a vendor patch).** With a nearest filter, the dither image
could be one texel for each cell (384 x 216 at 1920 x 1080), and cost about what the smooth
backdrop costs, at any window size. From reading the code, the patch touches three vendor crates:

- `vendor/gpui`: a filter choice for `paint_image`, and a field in `PolychromeSprite`
  (`scene.rs`), which both renderers read.
- `vendor/gpui_windows`: a second sampler state (`directx_renderer.rs`; today one sampler,
  `s_sprite`, serves every sprite kind) and its use in `polychrome_sprite_fragment`
  (`shaders.hlsl`).
- `vendor/gpui_apple`: the same in `shaders.metal`, which has a `constexpr` linear sampler in
  the sprite fragment functions. This part cannot be tested here.

**Recommendation for P6 (dither method): the full-size image, no vendor patch.** Dither is a
finish that the person chooses (Smooth is the default). Its memory does not grow, and at
1920 x 1080 it costs 4.5 % of one core more than a still page, about what the smooth backdrop
costs (3.5 %). At 3840 x 2160 it costs 21.6 % of one core and 280 MB more; so U04 must lower
the dither rate for large windows (for example 4 frames each second above 4 million device
pixels, about half the processor time, computed). If the owner wants 8 frames each second at 4K
for the same cost as Smooth, the nearest filter is the method, with the three vendor crates
above and a macOS test.

## 6. Proof 4: scale

**Task.** Count how the work divides for the helper `theme::sz`. Then try a zoom in
`vendor/gpui_windows`: multiply the scale factor of the window by a zoom value, in a branch that
is not merged. Record if text, hit tests, the terminal and popup windows stay correct.

### 6.1 How the work divides for `theme::sz`

Counted on `ui-redesign` at `2a70c8e` (after U01 and U07), in `crates/glasscode/src`, without
the two debug pages `proofs.rs` and `kit.rs`.
Each file goes to the first slice that owns it after U05 (section 8 of the slice plan); U23 gets
the remainder.

| Slice | Files and their `px(` count | `px(` |
|---|---|---|
| U05 | `ui.rs` 53, `themes_ui.rs` 40, `text_area.rs` 23, `composer.rs` 15, `terminal/mod.rs` 13, `text_input.rs` 10, `window_controls.rs` 2 | 156 |
| U10 | `sidebar.rs` 32, `server_ui.rs` 28 (sidebar part only), `sessions_ui.rs` 21 | 81 |
| U11 | `today_ui.rs` 100 | 100 |
| U12 | `chat_ui.rs` 119 (frame and message box only), `github_ui.rs` 53, `right_panel.rs` 21 | 193 |
| U13 | `files_ui.rs` 68 | 68 |
| U14 | `tickets_view.rs` 127, `tickets_ui.rs` 16 | 143 |
| U15 | `palette_ui.rs` 10 | 10 |
| U16 | `settings_ui.rs` 40, `dialogs.rs` 37 (account frame only), `browser_ui.rs` 9, `github_accounts_ui.rs` 7, `update_ui.rs` 2 | 95 |
| U19 | `glasscode_ui.rs` 47 | 47 |
| U23 | `usage_ui.rs` 65, `workspace.rs` 63, `notify_ui.rs` 34, `worktree_dialog.rs` 28, `session_start.rs` 13, `main.rs` 8, `switch_ui.rs` 6, `pair_ui.rs` 4, `icons.rs` 1, and the parts of `chat_ui.rs`, `server_ui.rs` and `dialogs.rs` that U10, U12 and U16 do not own | 222 |
| **All** | 33 files | **1115** |

Of the 1115: 1101 are the function `px(…)`, 14 are the method `.px(…)` (horizontal padding);
23 are lines of 1 px, which stay.
There are also 407 `.text_size(…)` calls; nearly all take a `px(…)`.

**A second unit that `sz` does not reach.** GPUI's short forms (`.p_2()`, `.gap_3()`, `.w_4()`,
`.px_3()`, `.rounded_md()` …) are in rems, not pixels (`vendor/gpui_macros/src/styles.rs`:
`rems(0.5)` and so on). The application has 962 spacing and size short forms and 96 `rounded_*`
short forms, and never calls `set_rem_size`, so the rem stays 16 px. If U05 also calls
`window.set_rem_size(px(16. * scale))` (`vendor/gpui/src/window.rs`), these 1058 sizes follow
the scale with no edit; without it they do not. This comes from reading the code: U02 did not
try `set_rem_size`. (`text_input.rs`, `text_area.rs` and `composer.rs` already convert their
font size with `window.rem_size()`.)

So the helper means: `sz` and `set_rem_size` in U05, then about 1080 mechanical edits in 33 files,
divided over ten slices as above, all on Windows and macOS with no vendor change.

### 6.2 The zoom trial in `vendor/gpui_windows` (not merged)

The patch was an uncommitted change in the slice's worktree, built, tried, and then removed
(`git checkout -- vendor/`). It is not in the pull request. This is all of it: the first three
files are "the window only"; `display.rs` was added for a second try.

```diff
--- a/vendor/gpui_windows/src/util.rs
+++ b/vendor/gpui_windows/src/util.rs
@@ -155,6 +155,14 @@ pub(crate) fn logical_point(x: f32, y: f32, scale_factor: f32) -> Point<Pixels>
+/// Trial only: a zoom on top of the display's scale factor, from `GLASSCODE_ZOOM` (1 when unset).
+pub(crate) fn zoom() -> f32 {
+    static ZOOM: OnceLock<f32> = OnceLock::new();
+    *ZOOM.get_or_init(|| {
+        std::env::var("GLASSCODE_ZOOM").ok().and_then(|z| z.parse().ok()).filter(|z: &f32| *z > 0.).unwrap_or(1.)
+    })
+}
--- a/vendor/gpui_windows/src/window.rs
+++ b/vendor/gpui_windows/src/window.rs
@@ -129,7 +129,7 @@ impl WindowsWindowState {
-            monitor_dpi / USER_DEFAULT_SCREEN_DPI as f32
+            monitor_dpi / USER_DEFAULT_SCREEN_DPI as f32 * zoom()
--- a/vendor/gpui_windows/src/events.rs
+++ b/vendor/gpui_windows/src/events.rs
@@ -870,7 +870,7 @@ impl WindowsWindowInner {
-        let new_scale_factor = new_dpi / USER_DEFAULT_SCREEN_DPI as f32;
+        let new_scale_factor = new_dpi / USER_DEFAULT_SCREEN_DPI as f32 * zoom();
--- a/vendor/gpui_windows/src/display.rs
+++ b/vendor/gpui_windows/src/display.rs
@@ -240,5 +240,5 @@ fn get_scale_factor_for_monitor(monitor: HMONITOR) -> Result<f32> {
-    Ok(dpi_x as f32 / USER_DEFAULT_SCREEN_DPI as f32)
+    Ok(dpi_x as f32 / USER_DEFAULT_SCREEN_DPI as f32 * crate::zoom())
```

Every other place reads the window's scale factor from `state.scale_factor`, so mouse
positions, the minimum size, the renderer, text and the input method's caret follow it.

**Results** (debug build, main display at 150 %, `GLASSCODE_ZOOM=1.3` gives a scale factor of
1.95, `0.9` gives 1.35):

| Check | Window only | Window and display |
|---|---|---|
| Size | The proofs window, 1920 x 1200 device pixels, became 984.6 x 615.4 logical pixels. | The same. The main window opened over the full work area, because its first size (1440 x 900 logical) is larger than the display in logical pixels (1313 x 783). |
| Text | Sharp at 11, 13, 16 and 24 px: the glyphs are drawn at the new scale factor. | The same. |
| Hit tests | Button 3 of `proofs:scale` counted 2 of 2 clicks. A click at device pixel (545, 719) of the window gave (279.5, 368.7) = (545 / 1.95, 719 / 1.95). The text box took typed text with the caret at its end. | At 0.9: the Today item of the rail opened Today; the maximize button (drawn by Glasscode) maximized the window. |
| Terminal | At 1.3: the shell prompt, typed input and the output of `echo zoom-check; 1..3` were correct, and the lines wrapped at the new column count. | Correct at 0.9 and 1.3 (prompt only). |
| Popup window (notification) | **Cut.** The window stayed 570 x 132 device pixels (380 x 88 logical at the display's 1.5), but its content was laid out at 1.95 (292 x 68 logical): the Open button was cut at the top and the right side. | **Correct.** 741 x 172 device pixels (380 x 88 logical at 1.95), at the bottom right above the taskbar. |
| 1 px lines | Uneven at 1.95, as they are at 1.5 (neither is a whole number). | The same. |

**What a real patch needs in addition.** The trial reads the zoom once, at the start. Settings
must change it while the application runs: a public setter in GPUI, a new layout of each open
window, and a decision about the window size (keep the device size or the logical size). The
first window size must be made to fit (seen above). And macOS needs the same in
`vendor/gpui_macos`, where the scale factor comes from the window's backing scale and the Metal
layer; that patch was not tried, and only the macOS build of the automatic build can check it
from here.

### 6.3 Recommendation for P5 (scale method)

**The helper `theme::sz` with `window.set_rem_size`, as the plan says now; no vendor patch.**

| | Helper `sz` + `set_rem_size` | Zoom in the platform crates |
|---|---|---|
| Work in the application | About 1080 edits in 33 files, divided over ten slices that change those files anyway (table 6.1) | None |
| Vendor change | None | 4 files in `gpui_windows` (11 lines for the trial; more for a change while running), plus `gpui_macos`, not tried |
| macOS | The same code, compiled by the automatic build | A second patch that cannot be tested here |
| Text, icons, hit tests | Expected to be correct (GPUI lays out and draws at the device scale), but not tried in U02 | Correct on Windows (trial) |
| Risk | A size that a slice forgets stays at 100 % until U23's sweep | Window sizes and popups on each platform; the first size must fit |

Note that the zoom was tried and the helper was not: the helper's side of this table comes
from reading the code. U05's test (`sz(10.)` for the four scales) and its demonstration at 90 %
and 130 % are the first proof of it.

The zoom works on Windows, and its patch is small. But a scale that the person changes in
Settings needs more than the trial, and the same work on macOS without a way to test it here.
The helper costs edits, but each screen slice converts only the files that it rewrites anyway.
If U05 meets a problem that the helper cannot solve, the trial above shows that the zoom is a
fallback for Windows.

## 7. What this document does not prove

- **macOS.** Nothing here ran on macOS. The Metal renderer has the same bilinear sampler
  (`vendor/gpui_apple/src/shaders.metal`), but its address mode is not WRAP, so the edge bleed
  can differ there; the border of one texel is safe on both.
- **A real 3840 x 2160 window.** No display here can show one. Section 5 says which 4K numbers are
  measured and which are computed.
- **Light theme costs.** The runs used the dark theme. The light theme does the same work for
  each pixel, plus one `max` and one subtraction.
- **The order "drop at once".** Proof 1 dropped each image one frame later. Dropping it before the
  new image is painted was not measured (section 3 says why it was not chosen).
- **The cost in a busy window.** The redraw run (section 3) used one shell session. A window
  with many sessions, a chat view and the Today board will cost more to draw.
- **The helper `theme::sz` and `set_rem_size`.** Section 6 counts the work, but neither was
  tried; the zoom was.
- **The idle cost.** The still page cost 2.43 % of one core with the main window hidden, and
  6.45 % with the main window shown and one shell session. That is the application's own ticks
  (250, 350 and 700 ms), not the backdrop; review focus item 4 of the plan expects almost zero.
- **A background thread.** The frames were made on the main thread. Making them on a background
  thread moves the cost but does not remove it; it was not tried.
