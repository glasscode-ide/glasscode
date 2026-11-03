//! The Browser dialog: the Glasscode Browser's state (running, the agents' tabs), and starting
//! over.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{AnyElement, AppContext as _, ClickEvent, Context, Window, div, prelude::*, px};
use glasscode_core::browser;

use crate::dialogs::Modal;
use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

#[derive(Clone, Debug, Default)]
pub struct BrowserStatus {
    pub chrome: bool,
    pub running: Option<String>,
    pub tabs: Vec<browser::Tab>,
}

pub struct BrowserUi {
    pub status: Option<BrowserStatus>,
    pub busy: Option<String>,
    pub error: Option<String>,
    pub confirm_reset: bool,
    checked: Instant,
    checking: bool,
    /// Each session's tab (session id → target id): its header shows a Browser button.
    pub session_tabs: HashMap<String, String>,
    tabs_checked: Instant,
    tabs_checking: bool,
}

impl BrowserUi {
    pub fn new() -> Self {
        BrowserUi {
            status: None,
            busy: None,
            error: None,
            confirm_reset: false,
            checked: Instant::now() - Duration::from_secs(60),
            checking: false,
            session_tabs: HashMap::new(),
            tabs_checked: Instant::now() - Duration::from_secs(60),
            tabs_checking: false,
        }
    }
}

fn read_status() -> BrowserStatus {
    let running = browser::running();
    BrowserStatus {
        chrome: browser::chrome().is_some(),
        tabs: if running.is_some() { browser::tabs() } else { Vec::new() },
        running: running.map(|r| r.product),
    }
}

/// Open `url` in a new tab of the Glasscode Browser and bring it forward (started first if needed).
fn open_tab(url: &str) -> anyhow::Result<()> {
    let running = browser::ensure_running()?;
    let target = browser::cdp::once(&running.ws_url, "Target.createTarget", serde_json::json!({ "url": url }))?;
    if let Some(id) = target["targetId"].as_str() {
        let _ = browser::cdp::once(&running.ws_url, "Target.activateTarget", serde_json::json!({ "targetId": id }));
    }
    Ok(())
}

impl Workspace {
    pub fn open_browser_dialog(&mut self, cx: &mut Context<Self>) {
        self.browser.confirm_reset = false;
        self.browser.error = None;
        self.modal = Some(Modal::Browser);
        self.browser_refresh(cx);
        cx.notify();
    }

    /// While the dialog is open, every few seconds; which sessions have a tab, every two.
    pub fn browser_tick(&mut self, cx: &mut Context<Self>) {
        if matches!(self.modal, Some(Modal::Browser)) && self.browser.checked.elapsed() > Duration::from_secs(3) {
            self.browser_refresh(cx);
        }
        if !self.browser.tabs_checking && self.browser.tabs_checked.elapsed() > Duration::from_secs(2) {
            self.browser.tabs_checking = true;
            self.browser.tabs_checked = Instant::now();
            let task = cx.background_spawn(async move { browser::session_tabs() });
            cx.spawn(async move |this, cx| {
                let tabs = task.await;
                let _ = this.update(cx, |this, cx| {
                    this.browser.tabs_checking = false;
                    if this.browser.session_tabs != tabs {
                        this.browser.session_tabs = tabs;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    /// Bring a session's tab to the front, for the person to look or lend a hand.
    pub fn show_session_tab(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let Some(target) = self.browser.session_tabs.get(session_id).cloned() else { return };
        let task = cx.background_spawn(async move { browser::show_tab(&target) });
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                let _ = this.update(cx, |this, cx| {
                    // The tab or the browser is gone: check again now.
                    this.browser.tabs_checked = Instant::now() - Duration::from_secs(60);
                    this.show_toast(format!("Could not show the browser tab: {error:#}"), cx);
                });
            }
        })
        .detach();
    }

    fn browser_refresh(&mut self, cx: &mut Context<Self>) {
        if self.browser.checking {
            return;
        }
        self.browser.checking = true;
        self.browser.checked = Instant::now();
        let task = cx.background_spawn(async move { read_status() });
        cx.spawn(async move |this, cx| {
            let status = task.await;
            let _ = this.update(cx, |this, cx| {
                this.browser.checking = false;
                this.browser.status = Some(status);
                cx.notify();
            });
        })
        .detach();
    }

    fn browser_do(&mut self, busy: &str, work: impl FnOnce() -> anyhow::Result<()> + Send + 'static, cx: &mut Context<Self>) {
        self.browser.busy = Some(busy.into());
        self.browser.error = None;
        let task = cx.background_spawn(async move { work() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.browser.busy = None;
                if let Err(error) = result {
                    this.browser.error = Some(format!("{error:#}"));
                }
                this.browser_refresh(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

pub fn render_browser_dialog(ws: &Workspace, _window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let b = &ws.browser;
    let status = b.status.clone().unwrap_or_default();
    let note = |t: &str| div().text_size(px(12.)).text_color(theme::muted()).child(t.to_string());
    let section = |title: &str| div().flex().flex_col().gap_2().child(ui::caps_label(title.to_uppercase()));
    let mut body: Vec<AnyElement> = Vec::new();

    if !status.chrome && b.status.is_some() {
        body.push(div().text_size(px(12.5)).text_color(theme::removed()).child("Google Chrome is not installed; the Glasscode Browser runs on it.").into_any_element());
    }

    // Running, and signed in.
    let state = match &status.running {
        Some(product) => format!("Running · {product} · {} tab{}", status.tabs.len(), if status.tabs.len() == 1 { "" } else { "s" }),
        None => "Not running (it starts when you open it, or when an agent needs it)".to_string(),
    };
    body.push(
        section("Glasscode Browser")
            .child(note("A Chrome window of its own that the agents on this Mac use. Your own Chrome is not touched."))
            .child(div().text_size(px(12.5)).child(state))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(ui::primary_button("browser-open", "Open browser").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.browser_do("Opening…", || open_tab("about:blank"), cx);
                    })))
                    .when(status.running.is_some(), |d| {
                        d.child(ui::button("browser-quit", "Quit browser").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.browser_do("Quitting…", || {
                                browser::stop();
                                Ok(())
                            }, cx);
                        })))
                    }),
            )
            .when_some(b.busy.clone(), |d, busy| d.child(div().text_size(px(12.)).text_color(theme::text_soft()).child(busy)))
            .when_some(b.error.clone(), |d, e| d.child(div().text_size(px(12.)).text_color(theme::removed()).child(e)))
            .into_any_element(),
    );

    // The agents, each in its own background tab; the person's tabs after them.
    let on = browser::agents_enabled();
    let mut listed = status.tabs.clone();
    listed.sort_by_key(|t| t.session.is_none());
    let tabs: Vec<AnyElement> = listed
        .iter()
        .take(12)
        .enumerate()
        .map(|(i, tab)| {
            let owner = match &tab.session {
                Some(id) => ws.state.sessions.iter().find(|s| s.id == *id).map(|s| s.title.clone()).unwrap_or_else(|| "A session".into()),
                None => "Yours".into(),
            };
            let target = tab.id.clone();
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(12.))
                .child(div().flex_none().w(px(150.)).truncate().text_color(if tab.session.is_some() { theme::text_strong() } else { theme::muted() }).child(owner))
                .child(div().flex_none().max_w(px(200.)).truncate().child(if tab.title.is_empty() { "(untitled)".to_string() } else { tab.title.clone() }))
                .child(div().flex_1().min_w_0().truncate().font_family(theme::MONO_FONT).text_color(theme::muted()).child(tab.url.clone()))
                .child(ui::button(("browser-show", i), "Show").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    let target = target.clone();
                    this.browser_do("Showing…", move || browser::show_tab(&target), cx);
                })))
                .into_any_element()
        })
        .collect();
    body.push(
        section("Agents")
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(ui::checkbox("browser-agents", on).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let _ = browser::set_agents_enabled(!on);
                        this.browser_refresh(cx);
                        cx.notify();
                    })))
                    .child(div().text_size(px(12.5)).child("New Claude and Codex sessions on this Mac can use it (each in its own tab, without asking)")),
            )
            .child(note("Agents work in background tabs, many at once, and never bring the browser forward: carry on with your work. Show brings a tab to the front for you. Sessions started before this keep what they had; open a new one for the change."))
            .when(!tabs.is_empty(), |d| d.child(div().flex().flex_col().gap_1().children(tabs)))
            .into_any_element(),
    );

    // Start over.
    body.push(
        section("Start over")
            .child(note("Quits the Glasscode Browser and forgets everything it knows: sign-ins, history, extensions."))
            .child(div().flex().child(if b.confirm_reset {
                div()
                    .flex()
                    .gap_2()
                    .child(ui::danger_button("browser-reset-yes", "Yes, forget its sign-ins").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.browser.confirm_reset = false;
                        this.browser_do("Resetting…", browser::reset, cx);
                    })))
                    .child(ui::button("browser-reset-no", "Cancel").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.browser.confirm_reset = false;
                        cx.notify();
                    })))
                    .into_any_element()
            } else {
                ui::button("browser-reset", "Reset browser…")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.browser.confirm_reset = true;
                        cx.notify();
                    }))
                    .into_any_element()
            }))
            .into_any_element(),
    );

    div().flex().flex_col().gap_5().children(body).into_any_element()
}
