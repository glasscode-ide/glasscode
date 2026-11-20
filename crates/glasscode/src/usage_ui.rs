//! Usage: every Claude and Codex account on this Mac, how close each is to its limits, and
//! where the tokens go (by day, project and session). The numbers come from the CLIs' own files
//! (`glasscode_core::usage`), read in the background 20 seconds after launch, then again every
//! 15 minutes (every 3 while the page is open), as the status bar and Today show usage too.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gpui::{AnyElement, ClickEvent, Context, Div, FontWeight, Hsla, SharedString, Window, div, prelude::*, px, relative};
use glasscode_core::model::AgentKind;
use glasscode_core::usage::{self, AccountInput, AccountReport, Limit, Report, Scanner, SessionUsage, Tokens};

use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

const CHART_HEIGHT: f32 = 200.;
/// A meter past this says "Near the limit".
const NEAR: f64 = 80.;

/// The categorical palette (fixed order, validated for light and dark surfaces): an account
/// keeps its colour whatever the filter shows.
const LIGHT: [u32; 8] = [0x2a78d6, 0xeb6834, 0x1baf7a, 0xeda100, 0xe87ba4, 0x008300, 0x4a3aa7, 0xe34948];
const DARK: [u32; 8] = [0x3987e5, 0xd95926, 0x199e70, 0xc98500, 0xd55181, 0x008300, 0x9085e9, 0xe66767];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AgentFilter {
    #[default]
    All,
    Claude,
    Codex,
}

pub struct UsageUi {
    pub report: Option<Report>,
    pub loading: bool,
    pub last_scan: Option<Instant>,
    /// When the app started: the first scan is due 20 seconds after (`usage::scan_due`).
    pub started: Instant,
    pub agent: AgentFilter,
    /// Days shown: 7, 14 or 30.
    pub range: u32,
    pub table: bool,
    pub hover: Option<usize>,
    debug_done: bool,
    scanner: Arc<Mutex<Option<Scanner>>>,
    /// v2 (M41): the (account, window) over the usage threshold, so `usage.threshold` is said
    /// once per crossing.
    pub near: std::collections::BTreeSet<(String, u64)>,
}

impl Default for UsageUi {
    fn default() -> Self {
        Self {
            report: None,
            loading: false,
            last_scan: None,
            started: Instant::now(),
            agent: AgentFilter::All,
            range: 14,
            table: false,
            hover: None,
            debug_done: false,
            scanner: Arc::new(Mutex::new(None)),
            near: Default::default(),
        }
    }
}

fn now() -> i64 {
    glasscode_core::model::now() as i64
}

/// "46.7B", "3.4M", "120K".
fn fmt(n: u64) -> String {
    let n = n as f64;
    if n >= 1e9 {
        format!("{:.1}B", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.1}M", n / 1e6)
    } else if n >= 1e3 {
        format!("{:.0}K", n / 1e3)
    } else {
        format!("{n:.0}")
    }
}

/// A round number at or above `n` for the chart's top line (1, 2 or 5 times a power of ten).
fn nice_ceiling(n: u64) -> u64 {
    if n == 0 {
        return 4;
    }
    let mut step = 1u64;
    while step.saturating_mul(10) <= n {
        step *= 10;
    }
    [1, 2, 5, 10].into_iter().map(|m| m * step).find(|v| *v >= n).unwrap_or(n)
}

/// "at 14:20" today, else "Mon 08:00".
fn reset_text(at: i64, now: i64) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let (_, _, _, h, m, wd) = usage::local_parts(at);
    if usage::local_day(at) == usage::local_day(now) {
        format!("at {h:02}:{m:02}")
    } else if at - now < 6 * 86400 {
        format!("{} {h:02}:{m:02}", DAYS[wd as usize % 7])
    } else {
        let (_, mo, d, _, _, _) = usage::local_parts(at);
        format!("{} {d}", ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][(mo as usize + 11) % 12])
    }
}

/// "Oct 20" for "2026-10-20".
fn day_label(day: &str) -> String {
    glasscode_core::linear::short_date(day)
}

fn dark_theme() -> bool {
    theme::bg().l < 0.5
}

fn series_color(slot: usize) -> Hsla {
    theme::c(if dark_theme() { DARK[slot % 8] } else { LIGHT[slot % 8] })
}

fn card() -> Div {
    div().flex().flex_col().p_4().rounded(theme::sz(14.)).border_1().border_color(theme::border()).bg(theme::raised())
}

/// One account as the page shows it.
struct Shown<'a> {
    slot: usize,
    label: String,
    plan: Option<String>,
    agent: AgentKind,
    report: Option<&'a AccountReport>,
}

impl Workspace {
    pub fn usage_tick(&mut self, cx: &mut Context<Self>) {
        // Debug builds: GLASSCODE_DEBUG_USAGE=1|table|claude|codex|30|hover:<day> opens the page so.
        if cfg!(debug_assertions) && !self.usage.debug_done {
            self.usage.debug_done = true;
            if let Ok(how) = std::env::var("GLASSCODE_DEBUG_USAGE") {
                self.enter(crate::shell::View::Usage);
                for part in how.split(',') {
                    match part {
                        "table" => self.usage.table = true,
                        "claude" => self.usage.agent = AgentFilter::Claude,
                        "codex" => self.usage.agent = AgentFilter::Codex,
                        "7" | "14" | "30" => self.usage.range = part.parse().unwrap_or(14),
                        _ => self.usage.hover = part.strip_prefix("hover:").and_then(|n| n.parse().ok()).or(self.usage.hover),
                    }
                }
            }
        }
        let open = self.view == crate::shell::View::Usage;
        if self.usage.loading {
            return;
        }
        if usage::scan_due(open, self.usage.started.elapsed(), self.usage.last_scan.map(|t| t.elapsed())) {
            self.usage_refresh(cx);
        }
    }

    pub fn usage_refresh(&mut self, cx: &mut Context<Self>) {
        if self.usage.loading {
            return;
        }
        self.usage.loading = true;
        self.usage.last_scan = Some(Instant::now());
        let inputs: Vec<AccountInput> = self
            .state
            .accounts
            .iter()
            .filter(|a| a.agent != AgentKind::Shell)
            .map(|a| AccountInput {
                id: a.id.clone(),
                agent: a.agent,
                dir: a.effective_dir(),
                sessions: self.state.sessions.iter().filter(|s| s.account_id.as_deref() == Some(a.id.as_str())).map(|s| s.id.clone()).collect(),
            })
            .collect();
        let scanner = self.usage.scanner.clone();
        let task = cx.background_spawn(async move {
            let mut guard = scanner.lock().unwrap_or_else(|e| e.into_inner());
            let scanner = guard.get_or_insert_with(Scanner::load);
            let report = scanner.scan(&inputs, now());
            if report.files_read > 0 {
                scanner.save();
            }
            report
        });
        cx.spawn(async move |this, cx| {
            let report = task.await;
            let _ = this.update(cx, |this, cx| {
                this.usage.loading = false;
                // v2 (M41): an account's window that crossed the threshold, for the usage-handoff
                // built-in (journal_note writes nothing with the engine at v1).
                let mut events = Vec::new();
                for account in &report.accounts {
                    let label = this.state.account(&account.id).map_or_else(|| account.id.clone(), |a| a.label.clone());
                    events.extend(glasscode_core::journal::usage_events(&mut this.usage.near, &account.id, &label, &account.limits));
                }
                for event in events {
                    this.journal_note(event);
                }
                this.usage.report = Some(report);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Which project a session folder belongs to: a Glasscode session's or project's, else
    /// Glasscode's worktree folder for a project, else the folder's own name.
    fn usage_project(&self, cwd: &str) -> String {
        let path = Path::new(cwd);
        let named = |id: &str| self.state.project(id).map(|p| p.name.clone());
        if let Some(name) = self.state.sessions.iter().filter(|s| path.starts_with(&s.worktree)).max_by_key(|s| s.worktree.as_os_str().len()).and_then(|s| named(&s.project_id)) {
            return name;
        }
        if let Some(p) = self.state.projects.iter().filter(|p| path.starts_with(&p.root)).max_by_key(|p| p.root.as_os_str().len()) {
            return p.name.clone();
        }
        if let Ok(rest) = path.strip_prefix(glasscode_core::paths::worktrees_dir()) {
            if let Some(first) = rest.components().next() {
                let folder = first.as_os_str().to_string_lossy().into_owned();
                // `<project>`, or the folder the project's settings file names (`app-wt`).
                if let Some(p) = self.state.projects.iter().find(|p| self.worktrees_of(p, &folder)) {
                    return p.name.clone();
                }
                return folder;
            }
        }
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| cwd.to_string())
    }

    /// Whether a usage record's folder belongs to a project switched off in Settings › Git and projects.
    /// Its tokens are then left out of the By project and Top sessions breakdowns. Mirrors the
    /// folder→project resolution in `usage_project`.
    fn usage_hidden(&self, cwd: &str) -> bool {
        let path = Path::new(cwd);
        if let Some(s) = self.state.sessions.iter().filter(|s| path.starts_with(&s.worktree)).max_by_key(|s| s.worktree.as_os_str().len()) {
            return self.state.is_hidden(&s.project_id);
        }
        if let Some(p) = self.state.projects.iter().filter(|p| path.starts_with(&p.root)).max_by_key(|p| p.root.as_os_str().len()) {
            return self.state.is_hidden(&p.id);
        }
        if let Ok(rest) = path.strip_prefix(glasscode_core::paths::worktrees_dir()) {
            if let Some(first) = rest.components().next() {
                let folder = first.as_os_str().to_string_lossy().into_owned();
                if let Some(p) = self.state.projects.iter().find(|p| self.worktrees_of(p, &folder)) {
                    return self.state.is_hidden(&p.id);
                }
            }
        }
        false
    }

    fn usage_session_title(&self, s: &SessionUsage) -> String {
        if let Some(session) = self.state.sessions.iter().find(|x| x.agent_session_id.as_deref() == Some(s.id.as_str())) {
            return self.session_title(session);
        }
        if let Some(title) = &s.title {
            return title.clone();
        }
        let folder = Path::new(&s.cwd).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if folder.is_empty() { format!("Session {}", s.id.chars().take(8).collect::<String>()) } else { format!("In {folder}") }
    }

    pub fn render_usage(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let now = now();
        let days = usage::last_days(self.usage.range, now);
        let range_label = format!("last {} days", self.usage.range);
        let filter = self.usage.agent;
        // Every account keeps the colour of its place among all of them.
        let shown: Vec<Shown> = self
            .state
            .accounts
            .iter()
            .filter(|a| a.agent != AgentKind::Shell)
            .enumerate()
            .filter(|(_, a)| match filter {
                AgentFilter::All => true,
                AgentFilter::Claude => a.agent == AgentKind::Claude,
                AgentFilter::Codex => a.agent == AgentKind::Codex,
            })
            .map(|(slot, a)| {
                let report = self.usage.report.as_ref().and_then(|r| r.accounts.iter().find(|x| x.id == a.id));
                let plan = a.plan.clone().or_else(|| report.and_then(|r| r.plan.clone()).map(|p| capitalize(&p)));
                Shown { slot, label: format!("{} · {}", a.agent.name(), a.label), plan, agent: a.agent, report }
            })
            .collect();

        let header = self.usage_header(cx);
        let body: AnyElement = if self.usage.report.is_none() {
            card()
                .gap_2()
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(format!("Reading {}'s transcripts…", crate::platform::this_computer())))
                .child(div().text_size(theme::sz(12.5)).text_color(theme::muted()).child("The first time takes a few seconds; after that only what is new is read."))
                .into_any_element()
        } else {
            let cards = div().flex().flex_wrap().gap_3().children(shown.iter().map(|s| self.usage_account_card(s, &days, &range_label, now)));
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(cards)
                .child(self.usage_chart(&shown, &days, cx))
                .child(div().flex().flex_wrap().gap_3().child(self.usage_by_project(&shown, &days, &range_label)).child(self.usage_top_sessions(&shown, &days, &range_label)))
                .child(div().text_size(theme::sz(12.5)).text_color(theme::muted()).child(format!(
                    "Limits come from each CLI: the rate limits Codex writes into its session logs, and the ones Claude Code gives a session's status line. Tokens are counted from the transcripts on {} (input, output and cache). Glasscode never reads login tokens.",
                    crate::platform::this_computer()
                )))
                .into_any_element()
        };
        div()
            .id("usage-page")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .bg(theme::bg())
            .child(div().max_w(theme::sz(1200.)).mx_auto().px_7().py_6().flex().flex_col().gap_5().child(header).child(body))
            .into_any_element()
    }

    fn usage_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let segment = |items: Vec<(SharedString, bool, Box<dyn Fn(&mut Workspace)>)>, cx: &mut Context<Self>| {
            div().flex().items_center().gap_0p5().p_0p5().rounded_lg().border_1().border_color(theme::border()).bg(theme::raised()).children(items.into_iter().enumerate().map(|(i, (label, on, pick))| {
                div()
                    .id(SharedString::from(format!("usage-seg-{label}-{i}")))
                    .flex()
                    .items_center()
                    .h(theme::sz(28.))
                    .px_3()
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(theme::sz(13.))
                    .when(on, |d| d.bg(theme::selected()).text_color(theme::text_strong()).font_weight(FontWeight::SEMIBOLD))
                    .when(!on, |d| d.text_color(theme::muted()).hover(|s| s.text_color(theme::text_strong())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        pick(this);
                        this.usage.hover = None;
                        cx.notify();
                    }))
                    .child(label)
            }))
        };
        let agent = self.usage.agent;
        let agents = segment(
            [(AgentFilter::All, "All accounts"), (AgentFilter::Claude, "Claude"), (AgentFilter::Codex, "Codex")]
                .into_iter()
                .map(|(f, label)| (SharedString::from(label), agent == f, Box::new(move |this: &mut Workspace| this.usage.agent = f) as Box<dyn Fn(&mut Workspace)>))
                .collect(),
            cx,
        );
        let range = self.usage.range;
        let ranges = segment(
            [7u32, 14, 30]
                .into_iter()
                .map(|n| (SharedString::from(format!("{n} days")), range == n, Box::new(move |this: &mut Workspace| this.usage.range = n) as Box<dyn Fn(&mut Workspace)>))
                .collect(),
            cx,
        );
        let status = match (&self.usage.report, self.usage.loading) {
            (_, true) => "Reading…".to_string(),
            (Some(r), false) => {
                let ago = (now() - r.scanned_at).max(0);
                if ago < 60 { "Updated just now".into() } else { format!("Updated {}", crate::session_start::ago_secs(ago)) }
            }
            (None, false) => String::new(),
        };
        div()
            .flex()
            .flex_wrap()
            .items_end()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(theme::sz(320.))
                    .gap_1()
                    .child(div().text_size(theme::sz(24.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child("Usage"))
                    .child(div().text_size(theme::sz(13.5)).text_color(theme::muted()).child(format!("Every Claude and Codex account on {}: how close each is to its limits, and where the tokens go.", crate::platform::this_computer()))),
            )
            .child(agents)
            .child(ranges)
            .child(ui::button("usage-refresh", if status.is_empty() { "Refresh".to_string() } else { status }).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.usage_refresh(cx))))
            .into_any_element()
    }

    fn usage_account_card(&self, s: &Shown, days: &[String], range_label: &str, now: i64) -> AnyElement {
        let color = series_color(s.slot);
        let limits: Vec<Limit> = s.report.map(|r| r.limits.clone()).unwrap_or_default();
        let tokens: u64 = s.report.map(|r| days.iter().filter_map(|d| r.days.get(d)).map(Tokens::total).sum()).unwrap_or(0);
        let sessions = s.report.map(|r| r.sessions.iter().filter(|x| days.iter().any(|d| x.days.get(d).is_some_and(|t| t.total() > 0))).count()).unwrap_or(0);
        let seen = s.report.and_then(|r| r.limits_seen);
        let meters: Vec<AnyElement> = limits
            .iter()
            .map(|l| {
                let pct = l.used_percent.clamp(0., 100.);
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(div().flex().items_baseline().gap_2().text_size(theme::sz(13.)).child(div().text_color(theme::text_soft()).child(l.label())).child(div().ml_auto().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(format!("{pct:.0}%"))))
                    .child(div().h(theme::sz(8.)).rounded(theme::sz(4.)).bg(theme::hover()).child(div().h_full().w(relative((pct / 100.) as f32)).rounded(theme::sz(4.)).bg(color)))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .text_size(theme::sz(12.))
                            .text_color(theme::muted())
                            .child(l.resets_at.map(|r| format!("Resets {}", reset_text(r, now))).unwrap_or_default())
                            .when(pct >= NEAR, |d| d.child(div().ml_auto().flex().items_center().gap_1().text_color(theme::text()).child(div().text_color(theme::modified()).child("▲")).child("Near the limit"))),
                    )
                    .into_any_element()
            })
            .collect();
        let no_limits = match s.agent {
            AgentKind::Claude => "Claude gives its 5-hour and weekly limits only to a session's status line. They show here once a Claude session started from Glasscode has run on this account.",
            _ => "No limits read yet: Codex writes them into its session log with each reply.",
        };
        let stat = |value: String, label: String| div().flex().flex_col().child(div().text_size(theme::sz(17.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(value)).child(div().text_size(theme::sz(12.)).text_color(theme::muted()).child(label));
        card()
            .flex_1()
            .min_w(theme::sz(300.))
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().size(theme::sz(10.)).rounded(theme::sz(3.)).bg(color))
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(s.label.clone()))
                    .child(div().flex_1())
                    .when_some(s.plan.clone(), |d, plan| d.child(div().px_2().rounded_full().border_1().border_color(theme::border()).text_size(theme::sz(11.5)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_soft()).child(plan))),
            )
            .when(meters.is_empty(), |d| d.child(div().text_size(theme::sz(12.5)).text_color(theme::muted()).child(no_limits)))
            .children(meters)
            .when_some(seen.filter(|_| !limits.is_empty()), |d, at| {
                let (_, _, _, h, m, _) = usage::local_parts(at);
                d.child(div().text_size(theme::sz(11.5)).text_color(theme::faint()).child(if usage::local_day(at) == usage::local_day(now) { format!("As of {h:02}:{m:02}") } else { format!("As of {}", day_label(&usage::local_day(at))) }))
            })
            .child(
                div()
                    .flex()
                    .gap_6()
                    .pt_3()
                    .border_t_1()
                    .border_color(theme::border())
                    .child(stat(fmt(tokens), format!("tokens · {range_label}")))
                    .child(stat(sessions.to_string(), "sessions".into()))
                    .child(stat(crate::platform::this_computer_label().into(), "runs on".into())),
            )
            .into_any_element()
    }

    fn usage_chart(&self, shown: &[Shown], days: &[String], cx: &mut Context<Self>) -> AnyElement {
        let per_day: Vec<Vec<u64>> = days.iter().map(|d| shown.iter().map(|s| s.report.and_then(|r| r.days.get(d)).map(Tokens::total).unwrap_or(0)).collect()).collect();
        let max = per_day.iter().map(|v| v.iter().sum::<u64>()).max().unwrap_or(0);
        let top = nice_ceiling(max);
        let n = days.len();
        let legend = div().flex().flex_wrap().gap_3().text_size(theme::sz(12.5)).text_color(theme::text_soft()).children(shown.iter().map(|s| div().flex().items_center().gap_1p5().child(div().size(theme::sz(10.)).rounded(theme::sz(3.)).bg(series_color(s.slot))).child(s.label.clone())));
        let head = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .child(div().flex().flex_col().flex_1().min_w(theme::sz(220.)).child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child("Tokens per day")).child(div().text_size(theme::sz(12.5)).text_color(theme::muted()).child("Input, output and cache, from each account's local transcripts")))
            .child(legend)
            .child(ui::button("usage-table", if self.usage.table { "Show chart" } else { "Show as table" }).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.usage.table = !this.usage.table;
                cx.notify();
            })));
        if self.usage.table {
            let row = |cells: Vec<(String, bool)>, header: bool| {
                div().flex().py_1p5().border_b_1().border_color(theme::border()).text_size(theme::sz(13.)).children(cells.into_iter().enumerate().map(move |(i, (text, strong))| {
                    div()
                        .when(i == 0, |d| d.w(theme::sz(110.)).flex_none())
                        .when(i > 0, |d| d.flex_1().min_w(theme::sz(90.)).text_right())
                        .text_color(if header { theme::muted() } else if strong { theme::text_strong() } else { theme::text_soft() })
                        .when(header || strong, |d| d.font_weight(FontWeight::SEMIBOLD))
                        .child(text)
                }))
            };
            let mut header: Vec<(String, bool)> = vec![("Day".into(), false)];
            header.extend(shown.iter().map(|s| (s.label.clone(), false)));
            header.push(("Total".into(), false));
            let rows = days.iter().zip(per_day.iter()).rev().map(|(d, values)| {
                let mut cells: Vec<(String, bool)> = vec![(day_label(d), false)];
                cells.extend(values.iter().map(|v| (fmt(*v), false)));
                cells.push((fmt(values.iter().sum()), true));
                row(cells, false)
            });
            return card().gap_3().child(head).child(div().flex().flex_col().child(row(header, true)).children(rows)).into_any_element();
        }
        let ticks: Vec<u64> = (0..=4).map(|i| top / 4 * i).collect();
        let axis = div()
            .relative()
            .flex_none()
            .w(theme::sz(44.))
            .h(theme::sz(CHART_HEIGHT))
            .children(ticks.iter().map(|t| div().absolute().right_0().bottom(theme::sz(*t as f32 / top as f32 * CHART_HEIGHT - 7.)).text_size(theme::sz(11.5)).text_color(theme::muted()).child(fmt(*t))));
        let grid = ticks.iter().map(|t| div().absolute().left_0().right_0().bottom(theme::sz(*t as f32 / top as f32 * CHART_HEIGHT)).h(px(1.)).bg(theme::border()));
        let hover = self.usage.hover;
        let bars = div().absolute().top_0().left_0().right_0().bottom_0().flex().items_end().children(per_day.iter().enumerate().map(|(i, values)| {
            let mut segments: Vec<AnyElement> = Vec::new();
            let last = values.iter().rposition(|v| *v > 0);
            for (k, value) in values.iter().enumerate().rev() {
                if *value == 0 {
                    continue;
                }
                let h = (*value as f32 / top as f32 * CHART_HEIGHT).max(2.);
                segments.push(div().flex_none().w_full().h(theme::sz(h)).bg(series_color(shown[k].slot)).when(Some(k) == last, |d| d.rounded_t(theme::sz(3.))).into_any_element());
            }
            div()
                .id(("usage-day", i))
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_col()
                .justify_end()
                .gap(theme::sz(2.))
                .px(theme::sz(if n > 20 { 1.5 } else { 4. }))
                .rounded_md()
                .when(hover == Some(i), |d| d.bg(theme::hover()))
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if *hovered {
                        this.usage.hover = Some(i);
                    } else if this.usage.hover == Some(i) {
                        this.usage.hover = None;
                    }
                    cx.notify();
                }))
                .children(segments)
        }));
        let tip = hover.filter(|i| *i < n).map(|i| {
            let values = &per_day[i];
            let side = if i < n / 2 { div().absolute().top(theme::sz(4.)).left(relative((i as f32 + 1.) / n as f32)) } else { div().absolute().top(theme::sz(4.)).right(relative((n - i) as f32 / n as f32)) };
            side.min_w(theme::sz(210.))
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme::border_strong())
                .bg(theme::raised())
                .shadow_lg()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(day_label(&days[i])))
                .children(shown.iter().zip(values.iter()).map(|(s, v)| {
                    div().flex().items_center().gap_2().text_size(theme::sz(12.5)).text_color(theme::text_soft()).child(div().size(theme::sz(9.)).rounded(theme::sz(3.)).bg(series_color(s.slot))).child(div().flex_1().child(s.label.clone())).child(div().text_color(theme::text_strong()).child(fmt(*v)))
                }))
                .child(div().mt_1().pt_1().border_t_1().border_color(theme::border()).flex().text_size(theme::sz(12.5)).text_color(theme::muted()).child(div().flex_1().child("Total")).child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(fmt(values.iter().sum()))))
        });
        let every = if n > 20 { 5 } else if n > 10 { 2 } else { 1 };
        let labels = div().flex().mt_1p5().children(days.iter().enumerate().map(|(i, d)| {
            div().flex_1().min_w_0().text_center().text_size(theme::sz(11.5)).text_color(theme::muted()).overflow_hidden().whitespace_nowrap().child(if (n - 1 - i) % every == 0 { day_label(d) } else { String::new() })
        }));
        card()
            .gap_4()
            .child(head)
            .child(
                div()
                    .flex()
                    .gap_2p5()
                    .child(axis)
                    .child(div().flex_1().min_w_0().flex().flex_col().child(div().relative().h(theme::sz(CHART_HEIGHT)).children(grid).child(bars).children(tip)).child(labels)),
            )
            .into_any_element()
    }

    fn usage_by_project(&self, shown: &[Shown], days: &[String], range_label: &str) -> AnyElement {
        let mut totals: Vec<(String, u64)> = Vec::new();
        for s in shown {
            for session in s.report.map(|r| r.sessions.as_slice()).unwrap_or_default() {
                let t: u64 = days.iter().filter_map(|d| session.days.get(d)).map(Tokens::total).sum();
                if t == 0 || self.usage_hidden(&session.cwd) {
                    continue;
                }
                let name = self.usage_project(&session.cwd);
                match totals.iter_mut().find(|(n, _)| *n == name) {
                    Some((_, v)) => *v += t,
                    None => totals.push((name, t)),
                }
            }
        }
        totals.sort_by(|a, b| b.1.cmp(&a.1));
        if totals.len() > 6 {
            let rest: u64 = totals[5..].iter().map(|(_, v)| v).sum();
            totals.truncate(5);
            totals.push(("Other".into(), rest));
        }
        let all: u64 = totals.iter().map(|(_, v)| v).sum();
        let max = totals.first().map(|(_, v)| *v).unwrap_or(1).max(1);
        let one = series_color(0);
        card()
            .flex_1()
            .min_w(theme::sz(340.))
            .gap_1()
            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child("By project"))
            .child(div().text_size(theme::sz(12.5)).text_color(theme::muted()).child(format!("Share of the tokens · {range_label}")))
            .when(totals.is_empty(), |d| d.child(div().pt_2().text_size(theme::sz(13.)).text_color(theme::muted()).child("No tokens in this range.")))
            .children(totals.into_iter().map(|(name, v)| {
                div()
                    .pt_3()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .flex()
                            .text_size(theme::sz(13.))
                            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme::text()).child(name))
                            .child(div().text_color(theme::text_strong()).child(fmt(v)))
                            .child(div().w(theme::sz(48.)).text_right().text_color(theme::muted()).child(format!("{:.0}%", v as f64 / all.max(1) as f64 * 100.))),
                    )
                    .child(div().h(theme::sz(10.)).rounded_r(theme::sz(4.)).bg(theme::hover()).child(div().h_full().w(relative(v as f32 / max as f32)).rounded_r(theme::sz(4.)).bg(one)))
            }))
            .into_any_element()
    }

    fn usage_top_sessions(&self, shown: &[Shown], days: &[String], range_label: &str) -> AnyElement {
        let mut rows: Vec<(String, String, Hsla, u64)> = Vec::new();
        for s in shown {
            for session in s.report.map(|r| r.sessions.as_slice()).unwrap_or_default() {
                let t: u64 = days.iter().filter_map(|d| session.days.get(d)).map(Tokens::total).sum();
                if t > 0 && !self.usage_hidden(&session.cwd) {
                    rows.push((self.usage_session_title(session), s.label.clone(), series_color(s.slot), t));
                }
            }
        }
        rows.sort_by(|a, b| b.3.cmp(&a.3));
        rows.truncate(6);
        card()
            .flex_1()
            .min_w(theme::sz(340.))
            .gap_1()
            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child("Sessions that used the most"))
            .child(div().text_size(theme::sz(12.5)).text_color(theme::muted()).child(capitalize(range_label)))
            .when(rows.is_empty(), |d| d.child(div().pt_2().text_size(theme::sz(13.)).text_color(theme::muted()).child("No sessions in this range.")))
            .child(div().pt_1().flex().flex_col().children(rows.into_iter().map(|(title, account, color, t)| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .py_2()
                    .border_b_1()
                    .border_color(theme::border())
                    .text_size(theme::sz(13.))
                    .child(div().flex_none().size(theme::sz(8.)).rounded(theme::sz(3.)).bg(color))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme::text()).child(title))
                    .child(div().flex_none().text_color(theme::muted()).child(account))
                    .child(div().flex_none().w(theme::sz(64.)).text_right().text_color(theme::text_strong()).child(fmt(t)))
            })))
            .into_any_element()
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(fmt(46_727_574_883), "46.7B");
        assert_eq!(fmt(3_400_000), "3.4M");
        assert_eq!(fmt(120_400), "120K");
        assert_eq!(fmt(512), "512");
        assert_eq!(nice_ceiling(0), 4);
        assert_eq!(nice_ceiling(3_200_000_000), 5_000_000_000);
        assert_eq!(nice_ceiling(1_000), 1_000);
        assert_eq!(nice_ceiling(1_001), 2_000);
    }
}
