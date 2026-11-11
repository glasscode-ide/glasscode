//! The Glasscode panel — a voice copilot in the right sidebar. The moon character is drawn
//! natively (a gold disc with eyes and a smile; a glow and listening rings when she's
//! live), below it the sessions she is watching. Click the moon to take her live; click
//! again and she goes idle.
//!
//! The visual + session awareness live here; the voice brain (ElevenLabs) attaches to
//! [`GlasscodeState::live`] — see the `voice/` crate-adjacent app. For now the toggle flips her
//! live state so the whole panel can be built and seen before the audio is wired in.

use std::path::PathBuf;
use std::process::Child;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{Animation, AnimationExt, AnyElement, ClickEvent, Context, FontWeight, Hsla, SharedString, Window, div, linear_color_stop, linear_gradient, prelude::*, pulsating_between, px, rgb};

use crate::sidebar::RowState;
use crate::theme;
use crate::ui;
use crate::workspace::{RightTab, Workspace};

/// One row of the glasses browse list, mirrored into the IDE panel.
#[derive(Clone, PartialEq)]
pub struct GlassesRow {
    pub title: String,
    pub subtitle: String,
    pub selected: bool,
}

/// A snapshot of what's on the Ray-Ban Display lens, posted by the glasses app to
/// `/api/voice/state` under `view`. The IDE renders this as a live replica so the desk
/// view matches the glasses in real time — and so it reads well in a presentation.
#[derive(Clone, PartialEq)]
pub struct GlassesView {
    pub screen: String, // projects | sessionList | actions | voice | talk | loading | error
    pub crumb: String,
    pub rows: Vec<GlassesRow>,
    pub banner: String,
    pub detail: String,
    pub listening: bool,
    pub billing: bool,
    pub cost: String,
    pub lens_on: bool,
    pub connected: bool,
}

/// Glasscode's own state, mirrored from the voice control server (`/api/voice/state`):
/// whether a live session is open, what she's doing, and the last thing she said.
pub struct GlasscodeState {
    pub live: bool,
    pub mode: String, // "idle" | "connecting" | "listening" | "speaking"
    pub caption: Option<String>,
    pub last_poll: Instant,
    /// The session id Glasscode is pointing at ("this is the one you mean") — highlighted and
    /// floated to the top of her list, without opening it.
    pub suggested: Option<String>,
    /// The headless voice helper process, alive only while Glasscode is live. Glasscode spawns it
    /// on the moon click and kills it when she goes idle — no browser, ever.
    pub helper: Option<Child>,
    /// Live replica of the glasses lens — the last view the phone posted. The Glasscode tab IS
    /// this mirror now; it holds the last-known view even when the phone pauses posting, and
    /// `glasses_fresh` says whether that view is live or stale.
    pub glasses: Option<GlassesView>,
    /// Whether the mirrored view is live (posted within the freshness window). When false the
    /// last view stays on screen, dimmed, instead of vanishing.
    pub glasses_fresh: bool,
    /// The session the glasses are currently in, to detect when it changes.
    pub glasses_current: Option<String>,
    /// A session the glasses navigated to, to be opened in the IDE on the next render (which
    /// has the `Window` that `select_session` needs; the poll tick does not).
    pub pending_open: Option<String>,
    /// Whether the last control-plane poll succeeded — drives the Glasscode tab's status line.
    pub control_ok: bool,
}

impl GlasscodeState {
    pub fn new() -> Self {
        GlasscodeState {
            live: false,
            mode: "idle".into(),
            caption: None,
            last_poll: Instant::now() - Duration::from_secs(2),
            suggested: None,
            helper: None,
            glasses: None,
            glasses_fresh: false,
            glasses_current: None,
            pending_open: None,
            control_ok: true,
        }
    }
}

const VOICE_STATE_URL: &str = "http://127.0.0.1:4517/api/voice/state";

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

fn hex(v: u32) -> Hsla {
    rgb(v).into()
}
fn faded(c: Hsla, a: f32) -> Hsla {
    Hsla { a, ..c }
}
/// The warm glow gold.
fn gold() -> Hsla {
    hex(0xf3d98a)
}

/// Parse the `view` object the glasses post to `/api/voice/state` into a [`GlassesView`].
/// Returns `None` when no view is present (the glasses aren't driving).
fn parse_glasses(v: &serde_json::Value) -> Option<GlassesView> {
    let g = v.get("view")?;
    if g.is_null() {
        return None;
    }
    let s = |k: &str| g.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let b = |k: &str| g.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    let rows = g
        .get("rows")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .map(|r| GlassesRow {
                    title: r.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    subtitle: r.get("subtitle").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    selected: r.get("selected").and_then(|x| x.as_bool()).unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default();
    Some(GlassesView {
        screen: s("screen"),
        crumb: s("crumb"),
        rows,
        banner: s("banner"),
        detail: s("detail"),
        listening: b("listening"),
        billing: b("billing"),
        cost: s("cost"),
        lens_on: g.get("lensOn").and_then(|x| x.as_bool()).unwrap_or(true),
        connected: b("connected"),
    })
}

/// The moon face, drawn natively. `live` adds a pulsing glow and listening rings.
/// Kept for the moon/talk panel, which the Glasscode tab no longer shows (it is the mirror now).
#[allow(dead_code)]
fn moon(live: bool) -> AnyElement {
    let eye = |left: f32, top: f32| {
        div().absolute().left(px(left)).top(px(top)).w(px(11.)).h(px(14.)).rounded_full().bg(hex(0x3b3324))
    };
    let crater = |left: f32, top: f32, size: f32| {
        div().absolute().left(px(left)).top(px(top)).size(px(size)).rounded_full().bg(faded(hex(0xe2d199), 0.5))
    };

    let disc = div()
        .relative()
        .size(px(118.))
        .rounded_full()
        .bg(linear_gradient(135., linear_color_stop(hex(0xfdf6de), 0.), linear_color_stop(hex(0xd4bd80), 1.)))
        .shadow_lg()
        .child(crater(80., 28., 13.))
        .child(crater(92., 58., 7.))
        .child(eye(40., 42.))
        .child(eye(64., 42.))
        // the smile: a full-round box showing only its curved bottom edge
        .child(div().absolute().left(px(42.)).top(px(62.)).w(px(34.)).h(px(18.)).rounded_full().border_b_2().border_color(hex(0x3b3324)));

    // glow behind the moon — steady when idle, a slow breath when live
    let glow = div().absolute().top(px(0.)).left(px(0.)).size(px(150.)).rounded_full().bg(gold());
    let glow = if live {
        glow.with_animation(
            SharedString::from("glasscode-glow"),
            Animation::new(Duration::from_millis(2400)).repeat().with_easing(pulsating_between(0.18, 0.44)),
            |d, t| d.opacity(t),
        )
        .into_any_element()
    } else {
        glow.opacity(0.13).into_any_element()
    };

    // listening rings, only when live
    let mut rings: Vec<AnyElement> = Vec::new();
    if live {
        rings.push(
            div()
                .absolute()
                .top(px(0.))
                .left(px(0.))
                .size(px(150.))
                .rounded_full()
                .border_2()
                .border_color(faded(gold(), 0.5))
                .with_animation(SharedString::from("glasscode-ring-1"), Animation::new(Duration::from_millis(2600)).repeat().with_easing(pulsating_between(0.0, 0.55)), |d, t| d.opacity(t))
                .into_any_element(),
        );
        rings.push(
            div()
                .absolute()
                .top(px(10.))
                .left(px(10.))
                .size(px(130.))
                .rounded_full()
                .border_2()
                .border_color(faded(gold(), 0.4))
                .with_animation(SharedString::from("glasscode-ring-2"), Animation::new(Duration::from_millis(2600)).repeat().with_easing(pulsating_between(0.45, 0.0)), |d, t| d.opacity(t))
                .into_any_element(),
        );
    }

    div()
        .relative()
        .size(px(150.))
        .flex()
        .items_center()
        .justify_center()
        .child(glow)
        .children(rings)
        .child(disc)
        .into_any_element()
}

impl Workspace {
    /// Click the moon: go live or idle. Live spawns the headless voice helper (it opens the
    /// ElevenLabs session, does the mic + speaker, and drives sessions through the control
    /// plane); idle kills it. The helper reports live/mode/caption to the shared state, which
    /// [`glasscode_tick`] mirrors back into this panel — so the browser never enters the picture.
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
        let Some(path) = helper_path() else {
            log::warn!("glasscode: voice helper binary not found (set LUNA_HELPER or bundle it)");
            return;
        };
        let mut cmd = std::process::Command::new(&path);
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
                self.glasscode.suggested = None;
            }
            cx.notify();
        }

        if self.right_tab != RightTab::Glasscode && !self.glasscode.live {
            return;
        }
        if self.glasscode.last_poll.elapsed() < Duration::from_millis(200) {
            return;
        }
        self.glasscode.last_poll = Instant::now();
        let task = cx.background_spawn(async move {
            let body = ureq::get(VOICE_STATE_URL).call().ok()?.into_string().ok()?;
            let v: serde_json::Value = serde_json::from_str(&body).ok()?;
            let mode = v.get("mode").and_then(|x| x.as_str()).unwrap_or("idle").to_string();
            let caption = v.get("caption").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let suggest = v.get("suggest").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let current = v.get("current").and_then(|x| x.as_str()).map(|s| s.to_string());
            let view = parse_glasses(&v);
            // The server stamps `view.ts` (same clock as the IDE — both on this Mac), so a
            // stale snapshot left behind after the glasses go away can be aged out.
            let view_ts = v.get("view").and_then(|g| g.get("ts")).and_then(|x| x.as_u64()).unwrap_or(0);
            Some((mode, caption, suggest, current, view, view_ts))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let mut changed = false;

                // Control-plane reachability (for the Glasscode tab's status line + refresh).
                let ok = result.is_some();
                if this.glasscode.control_ok != ok {
                    this.glasscode.control_ok = ok;
                    changed = true;
                }

                if let Some((mode, caption, suggest, current, view, view_ts)) = result {
                    // Mirror whatever the phone last posted. We keep the last-known view on
                    // screen even after posting pauses (phone backgrounded, lens off) — it just
                    // dims — instead of yanking it back to a talk panel. `connected` only tells
                    // us whether the glasses hardware is live; it does NOT gate the mirror.
                    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
                    let is_fresh = view.is_some() && now.saturating_sub(view_ts) < 12000;
                    if let Some(v) = view {
                        if this.glasscode.glasses.as_ref() != Some(&v) {
                            this.glasscode.glasses = Some(v);
                            changed = true;
                        }
                    }
                    if this.glasscode.glasses_fresh != is_fresh {
                        this.glasscode.glasses_fresh = is_fresh;
                        changed = true;
                    }
                    // Auto-open the glasses' session in the IDE only while the glasses are truly
                    // driving: a fresh view from connected hardware.
                    let driving = is_fresh && this.glasscode.glasses.as_ref().map(|g| g.connected).unwrap_or(false);

                    // When the glasses open a session, open it here too — desk mode: the lens
                    // is off and you're looking at the IDE. Only while the glasses are driving.
                    if driving {
                        if current != this.glasscode.glasses_current {
                            this.glasscode.glasses_current = current.clone();
                            if let Some(id) = current {
                                if this.state.active_session.as_deref() != Some(id.as_str()) {
                                    this.glasscode.pending_open = Some(id);
                                    changed = true;
                                }
                            }
                        }
                    } else {
                        this.glasscode.glasses_current = current;
                    }

                    // Only trust the reported mode/caption while the helper is actually live.
                    if this.glasscode.live {
                        let sug = if suggest.is_empty() { None } else { Some(suggest) };
                        if this.glasscode.mode != mode
                            || this.glasscode.caption.as_deref().unwrap_or("") != caption
                            || this.glasscode.suggested != sug
                        {
                            changed = true;
                        }
                        this.glasscode.mode = mode;
                        this.glasscode.caption = if caption.is_empty() { None } else { Some(caption) };
                        this.glasscode.suggested = sug;
                    }
                }

                if changed {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The Glasscode right-panel: the moon, her status, and the sessions she is watching.
    pub fn glasscode_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let status = self.glasscode_status_bar(cx);
        // The Glasscode tab IS the glasses mirror — nothing else. Show the last-known view (dimmed
        // when stale); only before the phone has ever posted do we show a short waiting note.
        let content = match self.glasscode.glasses.clone() {
            Some(g) => self.glasses_mirror(&g),
            None => self.glasses_waiting(),
        };
        div().flex().flex_col().flex_1().min_h_0().child(status).child(content).into_any_element()
    }

    /// Shown in the Glasscode tab before the phone has posted any view at all.
    fn glasses_waiting(&self) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_2()
            .px_4()
            .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).text_color(gold()).child("Glasscode".to_string()))
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(theme::muted())
                    .text_center()
                    .child("Open Glasscode on your phone, or put on the glasses — the live mirror shows here.".to_string()),
            )
            .into_any_element()
    }

    /// Thin status bar for the Glasscode tab: control-plane reachability + a manual Refresh so the
    /// tab can be re-synced without restarting the IDE.
    fn glasscode_status_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let ok = self.glasscode.control_ok;
        div()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .pt_2()
            .pb_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(ui::dot(if ok { theme::added() } else { theme::attention() }))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(theme::muted())
                            .child(if ok { "control plane connected".to_string() } else { "control plane unreachable".to_string() }),
                    ),
            )
            .child(
                div()
                    .id("glasscode-refresh")
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|d| d.bg(theme::hover()))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.glasscode_refresh(cx)))
                    .child(div().text_size(px(12.5)).text_color(theme::text_soft()).child("\u{21bb}"))
                    .child(div().text_size(px(11.)).text_color(theme::text_soft()).child("Refresh".to_string())),
            )
            .into_any_element()
    }

    /// Force an immediate control-plane re-poll plus a session refresh, so the tab recovers
    /// on demand instead of needing the IDE restarted.
    pub fn glasscode_refresh(&mut self, cx: &mut Context<Self>) {
        self.glasscode.last_poll = Instant::now().checked_sub(Duration::from_secs(2)).unwrap_or_else(Instant::now);
        self.glasscode_tick(cx);
        self.server_tick(cx);
        cx.notify();
    }

    #[allow(dead_code)]
    fn glasscode_moon_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let live = self.glasscode.live;
        let total = self.state.sessions.len();
        let needs = self.state.sessions.iter().filter(|s| self.row_state(s) == RowState::NeedsYou).count();

        let status = if live {
            match self.glasscode.mode.as_str() {
                "connecting" => "Waking Glasscode…".to_string(),
                "speaking" => self.glasscode.caption.clone().unwrap_or_else(|| "Speaking…".to_string()),
                _ => "Listening…".to_string(),
            }
        } else {
            "Tap Glasscode to start talking".to_string()
        };
        let hint = if live { "Just talk — click Glasscode to stop." } else { "Click Glasscode to go live." };

        let stage = div()
            .id("glasscode-stage")
            .w_full()
            .flex()
            .justify_center()
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.glasscode_toggle(cx)))
            .child(moon(live));

        let head = div()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .px_4()
            .pt_3()
            .child(stage)
            .child(div().text_size(px(14.)).font_weight(FontWeight::MEDIUM).text_color(if live { gold() } else { theme::text_soft() }).text_center().child(status))
            .child(div().text_size(px(11.5)).text_color(theme::muted()).child(hint));

        let counts = div()
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .pt_3()
            .pb_1()
            .font_family(theme::MONO_FONT)
            .text_size(px(12.))
            .child(div().text_color(theme::text_strong()).child(format!("{total}")))
            .child(div().text_color(theme::muted()).child("sessions ·"))
            .child(div().text_color(if needs > 0 { theme::attention() } else { theme::muted() }).font_weight(FontWeight::SEMIBOLD).child(format!("{needs}")))
            .child(div().text_color(theme::muted()).child("need you"));

        // sessions: the one Glasscode is pointing at first, then needs-you (RowState derives Ord:
        // NeedsYou < Working < Idle).
        let suggested = self.glasscode.suggested.clone();
        let mut ordered: Vec<&glasscode_core::model::Session> = self.state.sessions.iter().collect();
        ordered.sort_by_key(|s| (suggested.as_deref() != Some(s.id.as_str()), self.row_state(s)));
        let active = self.state.active_session.clone();

        let mut rows: Vec<AnyElement> = Vec::new();
        for s in ordered.into_iter().take(8) {
            let is_sug = suggested.as_deref() == Some(s.id.as_str());
            let st = self.row_state(s);
            let (dot_color, label, label_color) = match st {
                RowState::NeedsYou => (theme::attention(), "needs you", theme::attention()),
                RowState::Working => (theme::added(), "working", theme::muted()),
                RowState::Idle => (theme::muted(), "idle", theme::muted()),
            };
            let pr = self.prs.map.get(&s.id).and_then(|p| p.pr.as_ref()).map(|pr| pr.number);
            let sub = match pr {
                Some(n) => format!("PR #{n}"),
                None => s.branch.clone(),
            };
            let on = active.as_deref() == Some(s.id.as_str());
            let id = s.id.clone();
            let title = s.title.clone();
            rows.push(
                div()
                    .id(SharedString::from(format!("glasscode-s-{}", s.id)))
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .px_2()
                    .py_1p5()
                    .rounded_lg()
                    .cursor_pointer()
                    .when(on, |d| d.bg(theme::selected()))
                    .when(is_sug, |d| d.bg(faded(gold(), 0.12)).border_1().border_color(faded(gold(), 0.7)))
                    .hover(|d| d.bg(theme::hover()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window: &mut Window, cx| {
                        this.select_session(&id, window, cx);
                        this.right_tab = RightTab::Glasscode;
                        cx.notify();
                    }))
                    .child(ui::dot(dot_color))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_size(px(13.)).text_color(if on { theme::text_strong() } else { theme::text_soft() }).overflow_hidden().whitespace_nowrap().text_ellipsis().child(title))
                            .child(div().font_family(theme::MONO_FONT).text_size(px(10.5)).text_color(theme::muted()).overflow_hidden().whitespace_nowrap().text_ellipsis().child(sub)),
                    )
                    .child(div().flex_none().text_size(px(10.)).text_color(label_color).child(label))
                    .into_any_element(),
            );
        }
        if rows.is_empty() {
            rows.push(div().px_3().py_2().text_size(px(12.5)).text_color(theme::muted()).child("No sessions yet.").into_any_element());
        }

        let list = div()
            .id("glasscode-sessions")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_2p5()
            .gap_0p5()
            .children(rows);

        div().flex().flex_col().flex_1().min_h_0().child(head).child(counts).child(list).into_any_element()
    }

    /// The IDE's live replica of the glasses lens. Mirrors the browse flow (Projects ▸
    /// Sessions ▸ Session, Voice, Talk) exactly as it appears on the Ray-Ban Display.
    fn glasses_mirror(&self, g: &GlassesView) -> AnyElement {
        let fresh = self.glasscode.glasses_fresh;
        let dot_color = if !fresh {
            theme::muted()
        } else if g.connected {
            theme::added()
        } else {
            gold()
        };
        let state = if !fresh {
            "paused · last view".to_string()
        } else if g.connected && g.lens_on {
            "on glasses".to_string()
        } else if g.connected {
            "lens off · mirrored here".to_string()
        } else {
            "live · mirrored here".to_string()
        };

        let pill = div()
            .flex()
            .items_center()
            .gap_1p5()
            .child(ui::dot(dot_color))
            .child(div().text_size(px(11.)).text_color(theme::muted()).child(state));

        let cost = if g.billing {
            div().flex_none().text_size(px(11.)).font_family(theme::MONO_FONT).text_color(theme::attention()).child(g.cost.clone())
        } else {
            div().flex_none().text_size(px(11.)).font_family(theme::MONO_FONT).text_color(theme::added()).child("free".to_string())
        };

        let head = div()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .pt_3()
            .pb_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(div().flex_none().text_size(px(16.)).font_weight(FontWeight::BOLD).text_color(gold()).child("Glasscode".to_string()))
                    .child(pill),
            )
            .child(cost);

        let body: AnyElement = match g.screen.as_str() {
            "loading" => self.mirror_message("Loading your projects…"),
            "error" => self.mirror_message("Can't reach Glasscode — is the Mac awake and signed in?"),
            "voice" => {
                let title = if g.listening { "● Listening — free" } else { "Voice" }.to_string();
                let text = if g.detail.is_empty() { "Speak — it goes to the thread.".to_string() } else { g.detail.clone() };
                self.mirror_pane(title, text, theme::added())
            }
            "talk" => {
                let text = if g.banner.is_empty() { "Ask about this session, or say \u{201C}jump to\u{2026}\u{201D}.".to_string() } else { g.banner.clone() };
                self.mirror_pane("Live with Glasscode".to_string(), text, gold())
            }
            _ => {
                let mut items: Vec<AnyElement> = Vec::new();
                if !g.banner.is_empty() {
                    items.push(div().px_1().pb_1().text_size(px(12.)).text_color(gold()).child(g.banner.clone()).into_any_element());
                }
                for r in &g.rows {
                    items.push(self.mirror_row(r));
                }
                if items.is_empty() {
                    items.push(div().px_1().text_size(px(12.5)).text_color(theme::muted()).child("Nothing here yet.".to_string()).into_any_element());
                }
                div().flex().flex_col().gap_1().children(items).into_any_element()
            }
        };

        let crumb = div()
            .px_1()
            .pb_2()
            .text_size(px(10.5))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme::muted())
            .child(g.crumb.to_uppercase());

        let lens = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p_3()
            .rounded_xl()
            .border_1()
            .border_color(faded(gold(), if g.lens_on { 0.35 } else { 0.15 }))
            .bg(faded(hex(0x0f0d0a), 0.6))
            .child(crumb)
            .child(div().id("glasses-mirror-body").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().child(body));

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(head)
            .child(div().flex().flex_col().flex_1().min_h_0().px_2().pb_2().child(lens))
            .into_any_element()
    }

    fn mirror_message(&self, text: &str) -> AnyElement {
        div().px_1().py_2().text_size(px(13.)).text_color(theme::muted()).child(text.to_string()).into_any_element()
    }

    fn mirror_pane(&self, title: String, body: String, tint: Hsla) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_1p5()
            .p_3()
            .rounded_lg()
            .bg(faded(tint, 0.1))
            .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(tint).child(title))
            .child(div().text_size(px(12.5)).text_color(theme::text_soft()).child(body))
            .into_any_element()
    }

    fn mirror_row(&self, r: &GlassesRow) -> AnyElement {
        let dot = if r.selected { gold() } else { faded(theme::muted(), 0.7) };
        div()
            .flex()
            .items_center()
            .gap_2p5()
            .px_3()
            .py_2()
            .rounded_lg()
            .when(r.selected, |d| d.bg(faded(gold(), 0.14)).border_1().border_color(faded(gold(), 0.6)))
            .child(div().flex_none().size(px(7.)).rounded_full().bg(dot))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(if r.selected { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                            .text_color(if r.selected { theme::text_strong() } else { theme::text_soft() })
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(r.title.clone()),
                    )
                    .when(!r.subtitle.is_empty(), |d| {
                        d.child(
                            div()
                                .font_family(theme::MONO_FONT)
                                .text_size(px(10.5))
                                .text_color(theme::muted())
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(r.subtitle.clone()),
                        )
                    }),
            )
            .into_any_element()
    }
}
