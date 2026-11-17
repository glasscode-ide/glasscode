//! Glasscode keeps itself current: every few hours it looks for a newer signed release,
//! downloads and checks it in the background, and offers "Restart to update". It installs
//! when the person restarts or quits, never on its own: a restart would stop the agents
//! running here.
//!
//! TODO(glasscode): the standalone fork has no release server, so `update::newer` reports
//! "no update" and the background check never fetches. The local install path (staged app →
//! restart/quit swap) is kept for when a release is staged by other means.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{AnyElement, ClickEvent, Context, FontWeight, div, prelude::*};
#[cfg(any(target_os = "macos", windows))]
use glasscode_core::update::{self, BUILD};
#[cfg(target_os = "macos")]
use glasscode_core::update::mac_app as app;
#[cfg(windows)]
use glasscode_core::update::windows_app as app;

use crate::theme;
use crate::workspace::Workspace;

const EVERY: Duration = Duration::from_secs(3600);

pub struct UpdateUi {
    /// Downloaded and checked, waiting for a restart: (build, version, staged app).
    pub ready: Option<(u64, String, PathBuf)>,
    pub error: Option<String>,
    pub(crate) checking: bool,
    last_check: Instant,
}

impl UpdateUi {
    pub fn new() -> Self {
        // The first check comes half a minute after launch.
        let first = std::env::var("GLASSCODE_UPDATE_FIRST_CHECK").ok().and_then(|s| s.parse().ok()).unwrap_or(30);
        UpdateUi { ready: None, error: None, checking: false, last_check: Instant::now() - EVERY + Duration::from_secs(first) }
    }
}

#[cfg(any(target_os = "macos", windows))]
impl Workspace {
    pub fn update_start(&mut self) {
        if let Some(install) = app::current_install() {
            app::clean_up(&install);
            self.update.ready = app::staged();
        }
    }

    pub fn update_tick(&mut self, cx: &mut Context<Self>) {
        if self.update.ready.is_some() && std::env::var_os("GLASSCODE_TEST_UPDATE_RESTART").is_some() {
            return self.update_restart(cx);
        }
        if self.update.ready.is_some() && std::env::var_os("GLASSCODE_TEST_UPDATE_QUIT").is_some() {
            self.save();
            return cx.quit();
        }
        let u = &self.update;
        if BUILD == 0 || u.checking || u.ready.is_some() || u.last_check.elapsed() < EVERY || app::current_install().is_none() {
            return;
        }
        self.update.checking = true;
        self.update.last_check = Instant::now();
        let channel = self.state.prefs.update_channel().to_string();
        let task = cx.background_spawn(async move {
            match update::newer(app::PRODUCT, &channel, BUILD)? {
                Some(release) => Ok(Some((release.build, release.version.clone(), app::stage(&release)?))),
                None => Ok(None),
            }
        });
        cx.spawn(async move |this, cx| {
            let result: anyhow::Result<Option<(u64, String, PathBuf)>> = task.await;
            let _ = this.update(cx, |this, cx| {
                this.update.checking = false;
                match result {
                    Ok(ready) => {
                        this.update.error = None;
                        if let Some((_, version, _)) = &ready {
                            this.show_toast(format!("A Glasscode update ({version}) is ready: restart to use it, or it installs when you quit"), cx);
                        }
                        let restart = ready.is_some() && std::env::var_os("GLASSCODE_TEST_UPDATE_RESTART").is_some();
                        this.update.ready = ready;
                        if restart {
                            this.update_restart(cx);
                            return;
                        }
                    }
                    Err(error) => {
                        log::warn!("update check: {error:#}");
                        this.update.error = Some(format!("{error:#}"));
                        // Try again in an hour rather than six.
                        this.update.last_check = Instant::now() - EVERY + Duration::from_secs(3600);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Install the waiting update and open the new version.
    pub fn update_restart(&mut self, cx: &mut Context<Self>) {
        // Taken, so quitting next does not install it a second time.
        let (Some((_, _, staged)), Some(install)) = (self.update.ready.take(), app::current_install()) else { return };
        // Tests install and quit only: a relaunch would start without their environment.
        let relaunch = std::env::var_os("GLASSCODE_TEST_UPDATE_RESTART").is_none();
        match app::install(&staged, &install).and_then(|_| if relaunch { app::relaunch_after_exit(&install) } else { Ok(()) }) {
            Ok(()) => {
                self.save();
                cx.quit();
            }
            Err(error) => self.show_toast(format!("Could not update: {error:#}"), cx),
        }
    }

    /// Settings' "Check now": look for an update at once.
    pub fn update_check_now(&mut self, cx: &mut Context<Self>) {
        if BUILD == 0 || app::current_install().is_none() {
            self.show_toast("This is a development build: it does not update itself.", cx);
            return;
        }
        self.update.last_check = Instant::now() - EVERY - Duration::from_secs(1);
        self.update_tick(cx);
        cx.notify();
    }

    /// Quitting: a waiting update goes in place, for the next start (unless that is off).
    pub fn update_on_quit(&mut self) {
        if self.state.prefs.install_on_quit_off {
            return;
        }
        if let (Some((_, _, staged)), Some(install)) = (self.update.ready.take(), app::current_install())
            && let Err(error) = app::install(&staged, &install)
        {
            log::warn!("update on quit: {error:#}");
        }
    }
}

/// Only the Mac and Windows apps can stage and install an update so far, so elsewhere there is
/// nothing to look for or put in place.
#[cfg(not(any(target_os = "macos", windows)))]
impl Workspace {
    pub fn update_start(&mut self) {}

    pub fn update_tick(&mut self, _cx: &mut Context<Self>) {}

    pub fn update_restart(&mut self, _cx: &mut Context<Self>) {}

    pub fn update_check_now(&mut self, _cx: &mut Context<Self>) {}

    pub fn update_on_quit(&mut self) {}
}

impl Workspace {
    pub fn update_pill(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.update.ready.as_ref()?;
        Some(
            div()
                .id("update-pill")
                .flex()
                .items_center()
                .gap_1p5()
                .h(theme::sz(26.))
                .px_2p5()
                .rounded_md()
                .bg(theme::accent())
                .text_color(theme::on_accent())
                .text_size(theme::sz(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .cursor_pointer()
                .hover(|s| s.opacity(0.9))
                .child("Restart to update")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.update_restart(cx)))
                .into_any_element(),
        )
    }
}
