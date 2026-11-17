//! The status bar (slice U17). On the left: a usage meter for each account with limits and a
//! refresh, then the items the bar had (branch, changes, GitHub account, CLI versions, saved
//! sessions, agent and account). On the right: the memory of the sessions' programs, their
//! terminals (with the orphans) and their ports. Each opens a popover over the bar: Usage (the
//! limits of each account and "Open usage"), Resources (project, worktree and program, with
//! processor and memory, and × to end a program after one confirm line) and Ports (Open and Kill
//! for the ports of sessions, the others folded). A click outside the popover, or Esc, closes it.
//!
//! The programs and the ports are read in the background (`sys::process_use` blocks about
//! 0.25 s): every 15 seconds while the window is active, every 2 seconds while Resources or Ports
//! is open, and soon after a terminal starts or goes. The window draws again only when what it
//! shows changed: a draw of the window is about 17 ms of layout (U24).
//!
//! An orphan terminal is a program that a terminal of this Glasscode started (the shell or the
//! agent of a session, `TerminalView::child_pid`) and that no terminal holds any more (its
//! session was closed, moved or removed, or its terminal was restarted), yet that still runs as a
//! child of this Glasscode, with the name it had while its terminal held it, 10 seconds after its
//! terminal let it go (before that it may still be ending). A program under a shell that ended
//! has no parent any more, so nothing tells it from any other program: it is no orphan, and its
//! port is one of the other ports.
//!
//! The state lives here, on the UI thread: `Workspace` has no field for it, and U17 changes only
//! its own files (`shell::frames` and the backdrop keep theirs so too).
//!
//! Debug builds: `GLASSCODE_DEBUG_OPEN=status[:usage|resources|ports][:light|dark|classic|contrast][:90|100|115|130]`
//! opens a popover at the start.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use glasscode_core::model::{AgentKind, Session, ThemeChoice, UiScale};
use glasscode_core::resources::{self, Root};
use glasscode_core::sys::{self, Listener, ProcessUse};
use glasscode_core::usage::Limit;
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, Div, ElementId, FocusHandle, FontWeight,
    KeyDownEvent, MouseButton, MouseDownEvent, Pixels, ScrollHandle, SharedString, Stateful,
    Transformation, Window, canvas, deferred, div, percentage, prelude::*, px,
};

use crate::icons;
use crate::shell::{Popover, View};
use crate::theme::{self, sz};
use crate::ui;
use crate::workspace::Workspace;

/// How often the clock looks whether a read is due.
const TICK: Duration = Duration::from_secs(1);
/// A read while Resources or Ports is open, and otherwise (the counts of the bar).
pub const READ_OPEN: Duration = Duration::from_secs(2);
pub const READ_CLOSED: Duration = Duration::from_secs(15);
/// A terminal's program that still runs this long after its terminal let it go is an orphan.
pub const ORPHAN_AFTER: Duration = Duration::from_secs(10);
/// A meter past this is near its limit (as on the Usage page and Today's card).
const NEAR: f64 = 80.;

/// The bar's height (`shell::render_status_bar`).
pub const BAR_HEIGHT: f32 = 26.;
const POPOVER_WIDTH: f32 = 400.;
const ROW: f32 = 28.;
const GROUP: f32 = 30.;
const HEAD: f32 = 42.;
const FOOT: f32 = 48.;
const BODY_PAD: f32 = 6.;
const METER_WIDTH: f32 = 32.;
/// From the popover to the window's edge and to the bar.
const MARGIN: f32 = 8.;

// ----- formats ---------------------------------------------------------------------------------

const MIB: f64 = 1024. * 1024.;

/// Memory in a row of a popover: "5.3 MB", "412 MB", "1.2 GB" (binary units, as Task Manager).
pub fn bytes(n: u64) -> String {
    let mb = n as f64 / MIB;
    if mb < 9.95 {
        format!("{mb:.1} MB")
    } else if mb < 999.5 {
        format!("{mb:.0} MB")
    } else {
        format!("{:.1} GB", mb / 1024.)
    }
}

/// Memory on the bar, in coarse steps, so that a small change does not draw the window again:
/// "86 MB", "410 MB" (to 10 MB), "1.2 GB".
pub fn bytes_coarse(n: u64) -> String {
    let mb = n as f64 / MIB;
    if mb < 99.5 {
        format!("{mb:.0} MB")
    } else if mb < 995. {
        format!("{} MB", ((mb / 10.).round() * 10.) as u64)
    } else {
        format!("{:.1} GB", mb / 1024.)
    }
}

/// Processor use, where 100 is one full core: "0.4%", "14%".
pub fn cpu(percent: f32) -> String {
    let p = if percent.is_finite() {
        percent.max(0.)
    } else {
        0.
    };
    if p < 9.95 {
        format!("{p:.1}%")
    } else {
        format!("{p:.0}%")
    }
}

/// The time to a reset in few words: "<1 m", "45 m", "2 h", "2 h 10 m", "1 d 4 h", "3 d".
pub fn until(secs: i64) -> String {
    let minutes = secs.max(0) / 60;
    let (days, hours, minutes) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    if days >= 2 {
        format!("{days} d")
    } else if days == 1 {
        if hours == 0 {
            "1 d".into()
        } else {
            format!("1 d {hours} h")
        }
    } else if hours > 0 {
        if minutes == 0 {
            format!("{hours} h")
        } else {
            format!("{hours} h {minutes} m")
        }
    } else if minutes > 0 {
        format!("{minutes} m")
    } else {
        "<1 m".into()
    }
}

/// The confirm line for one program and the programs under it.
pub fn end_question(what: &str, under: usize) -> String {
    match under {
        0 => format!("End {what}?"),
        n => format!("End {what} and {n} under it?"),
    }
}

/// The confirm line for the orphan terminals.
pub fn end_orphans_question(count: usize, under: usize) -> String {
    let what = if count == 1 {
        "1 orphan terminal".to_string()
    } else {
        format!("{count} orphan terminals")
    };
    match under {
        0 => format!("End {what}?"),
        n => format!(
            "End {what} and {n} under {}?",
            if count == 1 { "it" } else { "them" }
        ),
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

// ----- usage -----------------------------------------------------------------------------------

/// One limit of an account as a meter.
#[derive(Clone, Debug, PartialEq)]
pub struct Meter {
    /// "5 hours", "Week", or the window's own label.
    pub window: String,
    pub fraction: f32,
    /// Near the limit.
    pub hot: bool,
    pub percent: String,
    /// The time to its reset, when the CLI said.
    pub left: Option<String>,
    pub week: bool,
}

pub fn meter(limit: &Limit, now: i64) -> Meter {
    let used = if limit.used_percent.is_finite() {
        limit.used_percent.clamp(0., 100.)
    } else {
        0.
    };
    Meter {
        window: match limit.window_minutes {
            300 => "5 hours".into(),
            10080 => "Week".into(),
            _ => limit.label(),
        },
        fraction: (used / 100.) as f32,
        hot: used >= NEAR,
        percent: format!("{used:.0}%"),
        left: limit.resets_at.map(|at| until(at - now)),
        week: limit.window_minutes == 10080,
    }
}

impl Meter {
    /// Its words on the bar ("84% · 2 h 10 m", "59% week"); `short`: the percent alone.
    pub fn bar_text(&self, short: bool) -> String {
        match (&self.left, short, self.week) {
            (_, true, _) => self.percent.clone(),
            (_, false, true) => format!("{} week", self.percent),
            (Some(left), false, false) => format!("{} · {left}", self.percent),
            (None, false, false) => self.percent.clone(),
        }
    }

    /// Its words in the popover: the percent and the time to the reset.
    pub fn text(&self) -> String {
        match &self.left {
            Some(left) => format!("{} · {left}", self.percent),
            None => self.percent.clone(),
        }
    }
}

/// A Claude or Codex account with the meters of the limits the usage scan last read (the shortest
/// window first, as the report keeps them).
#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    pub agent: AgentKind,
    pub title: String,
    pub plan: Option<String>,
    pub meters: Vec<Meter>,
}

/// The accounts the bar shows: those with a limit, each with its first two windows (5 hours and
/// week). None at all when no limit is known.
pub fn bar_accounts(accounts: &[Account]) -> Vec<Account> {
    accounts
        .iter()
        .filter(|a| !a.meters.is_empty())
        .map(|a| Account {
            meters: a.meters.iter().take(2).cloned().collect(),
            ..a.clone()
        })
        .collect()
}

// ----- the bar at narrow widths ----------------------------------------------------------------

/// The optional parts of the bar that show (`fit_at`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fit {
    /// The words after each meter's percent.
    pub words: bool,
    /// The second meter (the week) of each account.
    pub week: bool,
    pub cli: bool,
    pub saved: bool,
    pub github: bool,
    pub account: bool,
}

/// The texts of the bar's items, to measure.
pub struct Texts<'a> {
    pub branch: &'a str,
    pub changes: &'a str,
    pub github: Option<&'a str>,
    pub cli: Option<&'a str>,
    pub saved: &'a str,
    pub account: Option<&'a str>,
}

// Estimates at 100 %, for 11.5 px text; generous, so that an item goes before the bar clips one.
const CHAR: f32 = 6.4;
const MONO_CHAR: f32 = 7.;
/// `gap_4` between the items.
const GAP: f32 = 16.;
const BAR_PADDING: f32 = 24.;
const ITEM_PADDING: f32 = 12.;
const ICON: f32 = 13.;
const INNER_GAP: f32 = 5.;
const ACCOUNT_GAP: f32 = 10.;
const SAVED_DOT: f32 = 12.;
const BRANCH_MAX: f32 = 180.;
const GITHUB_MAX: f32 = 200.;

fn text_width(text: &str) -> f32 {
    text.chars().count() as f32 * CHAR
}

/// The usage item with its refresh, with or without the words and the week meters.
fn usage_width(accounts: &[Account], words: bool, week: bool) -> f32 {
    if accounts.is_empty() {
        return 0.;
    }
    let meters: f32 = accounts
        .iter()
        .map(|a| {
            ICON + a
                .meters
                .iter()
                .take(if week { 2 } else { 1 })
                .map(|m| INNER_GAP + METER_WIDTH + INNER_GAP + text_width(&m.bar_text(!words)))
                .sum::<f32>()
        })
        .sum();
    ITEM_PADDING
        + meters
        + ACCOUNT_GAP * (accounts.len() - 1) as f32
        + GAP
        + ITEM_PADDING
        + ICON
        + GAP
}

/// The counts on the right.
fn counts_width(counts: &Summary) -> f32 {
    [
        counts.memory.clone(),
        counts.terminals_text(),
        counts.ports.to_string(),
    ]
    .iter()
    .map(|t| ITEM_PADDING + ICON + INNER_GAP + text_width(t))
    .sum::<f32>()
        + 2. * 2.
}

/// What the bar takes whatever the width (the 5-hour meters with their percent, refresh, the
/// branch, changes and the counts), and the width of each optional part in the order they give
/// way: the meters' words, the week meters, the CLI versions, the saved sessions, the GitHub
/// account, the agent with its account. Logical widths, as at 100 %.
pub fn widths(usage: &[Account], texts: &Texts, counts: Option<&Summary>) -> (f32, [f32; 6]) {
    let shown = bar_accounts(usage);
    let kept = BAR_PADDING
        + usage_width(&shown, false, false)
        + ((texts.branch.chars().count() + 1) as f32 * MONO_CHAR).min(BRANCH_MAX)
        + GAP
        + text_width(texts.changes)
        + GAP
        + counts.map_or(0., counts_width);
    let optional = [
        usage_width(&shown, true, true) - usage_width(&shown, false, true),
        usage_width(&shown, false, true) - usage_width(&shown, false, false),
        texts.cli.map_or(0., |t| text_width(t) + GAP),
        text_width(texts.saved) + SAVED_DOT + GAP,
        texts
            .github
            .map_or(0., |t| text_width(t).min(GITHUB_MAX) + GAP),
        texts.account.map_or(0., |t| text_width(t) + GAP),
    ];
    (kept, optional)
}

/// Which optional parts show in `room` when `kept` is taken: they give way in their order, the
/// first first, until the rest fits.
pub fn give_way(room: f32, kept: f32, optional: &[f32]) -> Vec<bool> {
    let mut total = kept + optional.iter().sum::<f32>();
    let mut shown = vec![true; optional.len()];
    for (i, width) in optional.iter().enumerate() {
        if total <= room {
            break;
        }
        shown[i] = false;
        total -= width;
    }
    shown
}

/// The parts of a bar `width` pixels wide on the screen at `scale` that show. Nothing the bar had
/// before goes while a part of the new meters can go first.
pub fn fit_at(
    width: f32,
    scale: f32,
    usage: &[Account],
    texts: &Texts,
    counts: Option<&Summary>,
) -> Fit {
    let (kept, optional) = widths(usage, texts, counts);
    let shown = give_way(width / scale, kept, &optional);
    Fit {
        words: shown[0],
        week: shown[1],
        cli: shown[2],
        saved: shown[3],
        github: shown[4],
        account: shown[5],
    }
}

/// The left edge of a popover `width` wide over its item (its left and right edge, when the bar
/// was drawn) in a window `window` wide: Usage starts at its item's left edge, the others end at
/// their item's right edge, as the items sit at the two ends of the bar. It stays `margin` inside
/// the window. Pixels on the screen, as the bounds of the item are.
pub fn place(
    window: f32,
    width: f32,
    item: Option<(f32, f32)>,
    from_left: bool,
    margin: f32,
) -> f32 {
    let wanted = match (item, from_left) {
        (Some((left, _)), true) => left,
        (Some((_, right)), false) => right - width,
        (None, true) => margin,
        (None, false) => window - margin - width,
    };
    wanted.min(window - margin - width).max(margin)
}

// ----- programs, terminals, orphans ------------------------------------------------------------

/// A session's terminal with a running program, and where it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Terminal {
    pub session: String,
    /// Its program (`TerminalView::child_pid`): the root of the session's programs.
    pub pid: u32,
    pub title: String,
    pub agent: AgentKind,
    pub project: String,
    pub branch: String,
}

/// A terminal's program as a read saw it while its terminal held it.
#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    pub terminal: Terminal,
    pub name: String,
    /// When a read first found that no terminal holds it.
    pub released: Option<Instant>,
}

/// Keep `seen` up to date with a read (`all`) and the terminals that hold a program now
/// (`held`), and give the orphan terminals, by number. A program a terminal holds is (again) one
/// of its own. One that no terminal holds stays while it is in the read with its name and as a
/// child of Glasscode (`me`); it is an orphan once `ORPHAN_AFTER` passed since a read first found
/// it let go. Anything else (it ended, or its number is another program's now) is forgotten.
pub fn orphans(
    seen: &mut HashMap<u32, Seen>,
    held: &[Terminal],
    all: &[ProcessUse],
    me: u32,
    now: Instant,
) -> Vec<Seen> {
    let by_pid: HashMap<u32, &ProcessUse> = all.iter().map(|p| (p.pid, p)).collect();
    let held_pids: HashSet<u32> = held.iter().map(|t| t.pid).collect();
    for terminal in held {
        if let Some(p) = by_pid.get(&terminal.pid) {
            seen.insert(
                terminal.pid,
                Seen {
                    terminal: terminal.clone(),
                    name: p.name.clone(),
                    released: None,
                },
            );
        }
    }
    seen.retain(|pid, s| {
        if held_pids.contains(pid) {
            return true;
        }
        match by_pid.get(pid) {
            Some(p) if p.name == s.name && p.parent == me => {
                s.released.get_or_insert(now);
                true
            }
            _ => false,
        }
    });
    let mut found: Vec<Seen> = seen
        .values()
        .filter(|s| {
            !held_pids.contains(&s.terminal.pid)
                && s.released
                    .is_some_and(|at| now.saturating_duration_since(at) >= ORPHAN_AFTER)
        })
        .cloned()
        .collect();
    found.sort_by_key(|s| s.terminal.pid);
    found
}

/// The programs of `processes` from `root` down: each after its parent (brothers by number), with
/// its depth and how many are under it. Empty when `root` is not among them.
pub fn tree(root: u32, processes: &[ProcessUse]) -> Vec<(usize, &ProcessUse, usize)> {
    let Some(top) = processes.iter().find(|p| p.pid == root) else {
        return Vec::new();
    };
    let mut children: HashMap<u32, Vec<&ProcessUse>> = HashMap::new();
    for p in processes.iter().filter(|p| p.pid != root) {
        children.entry(p.parent).or_default().push(p);
    }
    for list in children.values_mut() {
        list.sort_by_key(|p| p.pid);
    }
    fn walk<'a>(
        p: &'a ProcessUse,
        depth: usize,
        children: &HashMap<u32, Vec<&'a ProcessUse>>,
        seen: &mut HashSet<u32>,
        out: &mut Vec<(usize, &'a ProcessUse, usize)>,
    ) -> usize {
        let at = out.len();
        out.push((depth, p, 0));
        let mut under = 0;
        for &child in children.get(&p.pid).into_iter().flatten() {
            // A list read while programs start and end can have a loop.
            if seen.insert(child.pid) {
                under += 1 + walk(child, depth + 1, children, seen, out);
            }
        }
        out[at].2 = under;
        under
    }
    let mut out = Vec::new();
    walk(top, 0, &children, &mut HashSet::from([root]), &mut out);
    out
}

/// How many programs are under `pid` in `all`.
pub fn under(pid: u32, all: &[ProcessUse]) -> usize {
    tree(pid, all).first().map_or(0, |(_, _, under)| *under)
}

/// One program in the Resources popover.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub pid: u32,
    pub name: String,
    /// The session's title and agent, on the row of its terminal's program.
    pub title: Option<String>,
    pub agent: Option<AgentKind>,
    pub depth: usize,
    pub cpu: String,
    pub memory: String,
    pub under: usize,
    /// The program of an orphan terminal.
    pub orphan: bool,
    /// × shows: a program of a session's terminal or under it, or the program of an orphan
    /// terminal (not the programs under an orphan: "End orphan terminals" ends those).
    pub can_end: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorktreeUse {
    pub branch: String,
    pub rows: Vec<Row>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectUse {
    pub name: String,
    pub cpu: String,
    pub memory: String,
    pub worktrees: Vec<WorktreeUse>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resources {
    pub projects: Vec<ProjectUse>,
    pub cpu: String,
    pub memory: String,
    /// The orphan terminals' programs (number and name), and how many programs are under them.
    pub orphans: Vec<(u32, String)>,
    pub orphans_under: usize,
}

/// A port that a program listens on.
#[derive(Clone, Debug, PartialEq)]
pub struct Port {
    pub port: u16,
    pub pid: u32,
    pub process: String,
    /// "project · branch" of its session.
    pub place: String,
    pub under: usize,
    /// It belongs to an orphan terminal: Open, and no Kill ("End orphan terminals" ends it).
    pub orphan: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ports {
    /// The ports of the sessions' programs.
    pub sessions: Vec<Port>,
    /// The others: port and program.
    pub other: Vec<(u16, String)>,
}

/// The counts of the bar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    /// Of all the sessions' programs (and the orphans').
    pub memory: String,
    /// Terminals whose program runs, the orphans with them.
    pub terminals: usize,
    pub orphans: usize,
    /// Ports of the sessions' programs.
    pub ports: usize,
}

impl Summary {
    pub fn terminals_text(&self) -> String {
        if self.orphans > 0 {
            format!("{} ({})", self.terminals, self.orphans)
        } else {
            self.terminals.to_string()
        }
    }
}

/// What the bar and its two popovers show after a read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    pub summary: Summary,
    pub resources: Resources,
    pub ports: Ports,
}

/// What the confirm line ends.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// A program of a session's terminal or under it, or (`orphan`) an orphan terminal's program;
    /// `port` when Kill on that port asked.
    Program {
        pid: u32,
        name: String,
        orphan: bool,
        port: Option<u16>,
    },
    /// "End N orphan terminals": their programs, by number and name.
    Orphans(Vec<(u32, String)>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Confirm {
    pub target: Target,
    pub question: String,
}

/// The group of an orphan: never a session's id (a restarted session has the same id).
fn orphan_key(pid: u32) -> String {
    format!("orphan:{pid}")
}

fn roots_of(held: &[Terminal], orphans: &[Seen]) -> Vec<Root> {
    held.iter()
        .map(|t| Root {
            session: t.session.clone(),
            pid: t.pid,
        })
        .chain(orphans.iter().map(|o| Root {
            session: orphan_key(o.terminal.pid),
            pid: o.terminal.pid,
        }))
        .collect()
}

/// The bar's counts and the two popovers from a read: the programs (`all`), the ports, the
/// terminals that hold a program (in the order of their projects) and the orphans.
pub fn model(
    all: &[ProcessUse],
    listeners: &[Listener],
    held: &[Terminal],
    orphans: &[Seen],
) -> Model {
    let roots = roots_of(held, orphans);
    let groups = resources::group(all, &roots);
    let entries = held
        .iter()
        .map(|t| (t, false))
        .chain(orphans.iter().map(|o| (&o.terminal, true)));
    let mut projects: Vec<(ProjectUse, f32, u64)> = Vec::new();
    let (mut cpu_all, mut memory_all, mut running) = (0f32, 0u64, 0usize);
    let (mut orphan_list, mut orphans_under) = (Vec::new(), 0);
    for (terminal, orphan) in entries {
        let key = if orphan {
            orphan_key(terminal.pid)
        } else {
            terminal.session.clone()
        };
        let Some(group) = groups.iter().find(|g| g.session == key) else {
            continue;
        };
        let programs = tree(terminal.pid, &group.processes);
        // Its program ended since: nothing of it runs.
        let Some(&(_, top, top_under)) = programs.first() else {
            continue;
        };
        running += 1;
        if orphan {
            orphan_list.push((top.pid, top.name.clone()));
            orphans_under += top_under;
        }
        let rows: Vec<Row> = programs
            .iter()
            .map(|&(depth, p, under)| Row {
                pid: p.pid,
                name: p.name.clone(),
                title: (depth == 0 && !orphan).then(|| terminal.title.clone()),
                agent: (depth == 0 && !orphan).then_some(terminal.agent),
                depth,
                cpu: cpu(p.cpu_percent),
                memory: bytes(p.memory_bytes),
                under,
                orphan: orphan && depth == 0,
                can_end: !orphan || depth == 0,
            })
            .collect();
        let (cpu_used, memory) = programs.iter().fold((0f32, 0u64), |(c, m), (_, p, _)| {
            (c + p.cpu_percent, m.saturating_add(p.memory_bytes))
        });
        cpu_all += cpu_used;
        memory_all = memory_all.saturating_add(memory);
        let at = match projects
            .iter()
            .position(|(p, _, _)| p.name == terminal.project)
        {
            Some(at) => at,
            None => {
                projects.push((
                    ProjectUse {
                        name: terminal.project.clone(),
                        cpu: String::new(),
                        memory: String::new(),
                        worktrees: Vec::new(),
                    },
                    0.,
                    0,
                ));
                projects.len() - 1
            }
        };
        let (project, project_cpu, project_memory) = &mut projects[at];
        *project_cpu += cpu_used;
        *project_memory = project_memory.saturating_add(memory);
        match project
            .worktrees
            .iter_mut()
            .find(|w| w.branch == terminal.branch)
        {
            Some(worktree) => worktree.rows.extend(rows),
            None => project.worktrees.push(WorktreeUse {
                branch: terminal.branch.clone(),
                rows,
            }),
        }
    }
    let projects = projects
        .into_iter()
        .map(|(mut project, c, m)| {
            project.cpu = cpu(c);
            project.memory = bytes(m);
            project
        })
        .collect();

    let mut ports = Ports::default();
    for listener in listeners {
        let process = if listener.process.is_empty() {
            format!("process {}", listener.pid)
        } else {
            listener.process.clone()
        };
        let owner = resources::owner(listener.pid, all, &roots);
        let terminal = owner.as_deref().and_then(|key| {
            held.iter()
                .find(|t| t.session == key)
                .map(|t| (t, false))
                .or_else(|| {
                    orphans
                        .iter()
                        .find(|o| orphan_key(o.terminal.pid) == key)
                        .map(|o| (&o.terminal, true))
                })
        });
        match terminal {
            Some((terminal, orphan)) => ports.sessions.push(Port {
                port: listener.port,
                pid: listener.pid,
                process,
                place: format!("{} · {}", terminal.project, terminal.branch),
                under: under(listener.pid, all),
                orphan,
            }),
            None => ports.other.push((listener.port, process)),
        }
    }
    let session_ports: HashSet<u16> = ports.sessions.iter().map(|p| p.port).collect();
    Model {
        summary: Summary {
            memory: bytes_coarse(memory_all),
            terminals: running,
            orphans: orphan_list.len(),
            ports: session_ports.len(),
        },
        resources: Resources {
            projects,
            cpu: cpu(cpu_all),
            memory: bytes(memory_all),
            orphans: orphan_list,
            orphans_under,
        },
        ports,
    }
}

impl Model {
    /// Whether what a confirm line names is still in the popovers, as it was.
    pub fn has(&self, target: &Target) -> bool {
        match target {
            Target::Program {
                pid,
                name,
                port: Some(port),
                ..
            } => self
                .ports
                .sessions
                .iter()
                .any(|p| p.pid == *pid && p.port == *port && p.process == *name),
            Target::Program {
                pid,
                name,
                orphan,
                port: None,
            } => self
                .resources
                .projects
                .iter()
                .flat_map(|p| &p.worktrees)
                .flat_map(|w| &w.rows)
                .any(|r| r.pid == *pid && r.name == *name && r.orphan == *orphan && r.can_end),
            Target::Orphans(list) => self.resources.orphans == *list,
        }
    }
}

/// Whether a new read changes what the window shows: the counts of the bar, and the popover that
/// is open. The rest changes nothing on screen, so it draws nothing.
pub fn shows_change(old: Option<&Model>, new: &Model, open: Option<Popover>) -> bool {
    let Some(old) = old else { return true };
    old.summary != new.summary
        || match open {
            Some(Popover::Resources) => old.resources != new.resources,
            Some(Popover::Ports) => old.ports != new.ports,
            _ => false,
        }
}

/// Whether a read is due: the first one at once; while the window is in the back, none; while
/// Resources or Ports is open every `READ_OPEN`; when a terminal started or went, now (to see an
/// orphan, and to learn a new program's name); else every `READ_CLOSED`.
pub fn read_due(
    since: Option<Duration>,
    popover_open: bool,
    active: bool,
    roots_changed: bool,
) -> bool {
    let Some(age) = since else { return true };
    active && (roots_changed || age >= if popover_open { READ_OPEN } else { READ_CLOSED })
}

/// Whether the program `pid` (`name`) may end now, from a new read (`all`): it is the same
/// program, and it is still under the program of a session's terminal (`roots`); an orphan must
/// still be a child of Glasscode (`me`) that no terminal holds. Else why not.
pub fn may_end(
    pid: u32,
    name: &str,
    orphan: bool,
    all: &[ProcessUse],
    roots: &[Root],
    me: u32,
) -> Result<(), String> {
    let Some(p) = all.iter().find(|p| p.pid == pid) else {
        return Err(format!("{name} is not running any more"));
    };
    if p.name != name {
        return Err(format!("process {pid} is another program now ({})", p.name));
    }
    if orphan {
        if p.parent != me || roots.iter().any(|r| r.pid == pid) {
            return Err(format!(
                "{name} is not an orphan terminal of Glasscode any more"
            ));
        }
    } else if resources::owner(pid, all, roots).is_none() {
        return Err(format!("{name} does not belong to a session any more"));
    }
    Ok(())
}

/// End what the confirm line names, after a new read finds that it still may end. The words of
/// the toast. Blocks: run it in the background.
fn end(target: &Target, roots: &[Root], me: u32) -> Result<String, String> {
    let all = sys::process_use();
    let end_one = |pid: u32, name: &str, orphan: bool| {
        may_end(pid, name, orphan, &all, roots, me)
            .and_then(|()| sys::end_tree(pid).map_err(|e| e.to_string()))
    };
    match target {
        Target::Program {
            pid,
            name,
            orphan,
            port,
        } => {
            end_one(*pid, name, *orphan).map_err(|e| format!("Could not end {name}: {e}"))?;
            Ok(match port {
                Some(port) => format!("Port {port} is free: {name} ended."),
                None => format!("{name} ended."),
            })
        }
        Target::Orphans(list) => {
            let failed: Vec<String> = list
                .iter()
                .filter_map(|(pid, name)| end_one(*pid, name, true).err())
                .collect();
            match failed.len() {
                0 => Ok(if list.len() == 1 {
                    "The orphan terminal ended.".into()
                } else {
                    format!("{} orphan terminals ended.", list.len())
                }),
                n => Err(format!(
                    "Could not end {n} of the orphan terminals: {}",
                    failed.join("; ")
                )),
            }
        }
    }
}

/// The popover, the theme and the scale of a `GLASSCODE_DEBUG_OPEN=status…` value.
fn parse_hook(mode: &str) -> (Option<Popover>, Option<ThemeChoice>, Option<UiScale>) {
    let (mut popover, mut choice, mut scale) = (None, None, None);
    for word in mode.split(':').skip(1) {
        match word {
            "usage" => popover = Some(Popover::Usage),
            "resources" => popover = Some(Popover::Resources),
            "ports" => popover = Some(Popover::Ports),
            "light" => choice = Some(ThemeChoice::Light),
            "dark" => choice = Some(ThemeChoice::Dark),
            "classic" => choice = Some(ThemeChoice::Classic),
            "contrast" => choice = Some(ThemeChoice::Contrast),
            other => scale = theme::scale_named(other).or(scale),
        }
    }
    (popover, choice, scale)
}

// ----- the state -------------------------------------------------------------------------------

#[derive(Default)]
struct Bar {
    started: bool,
    hooked: bool,
    reading: bool,
    /// Goes up when an End finishes: a read that started before it is read again.
    generation: u64,
    read_at: Option<Instant>,
    /// The programs of the terminals when the last read started.
    roots: Vec<u32>,
    seen: HashMap<u32, Seen>,
    model: Option<Model>,
    confirm: Option<Confirm>,
    ending: bool,
    /// "Other ports" is unfolded.
    other_open: bool,
    focus: Option<FocusHandle>,
    /// The popover takes the keyboard at its next draw.
    focus_pending: bool,
    /// What had the keyboard before the popover took it.
    before: Option<FocusHandle>,
    /// Where the items were at the last draw (usage, memory, terminals, ports): screen pixels.
    anchors: [Option<Bounds<Pixels>>; 4],
}

thread_local! {
    static BAR: RefCell<Bar> = RefCell::new(Bar::default());
}

/// The bar's state. Take what is needed and let go: never call into `Workspace` inside.
fn with<R>(f: impl FnOnce(&mut Bar) -> R) -> R {
    BAR.with_borrow_mut(f)
}

/// Records where an item is, for the popover over it.
fn anchor(slot: usize) -> impl IntoElement {
    canvas(
        move |bounds, _, _| with(|b| b.anchors[slot] = Some(bounds)),
        |_, _, _, _| (),
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// An item of the bar that opens a popover.
fn item(id: &'static str, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .gap(sz(INNER_GAP))
        .h(sz(20.))
        .px(sz(ITEM_PADDING / 2.))
        .rounded(sz(5.))
        .whitespace_nowrap()
        .cursor_pointer()
        .when(on, |d| d.bg(theme::selected()).text_color(theme::text()))
        .hover(|s| s.bg(theme::hover()).text_color(theme::text()))
}

// Rows of the popovers. Text on glass is `text`, `text_soft` or `card_soft_text` only.

fn row() -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(sz(8.))
        .h(sz(ROW))
        .px(sz(14.))
        .min_w_0()
}

fn group_row() -> Div {
    div()
        .flex()
        .flex_none()
        .items_end()
        .gap(sz(8.))
        .h(sz(GROUP))
        .px(sz(14.))
        .pb(sz(5.))
        .min_w_0()
}

fn number(text: String, width: f32) -> Div {
    div()
        .flex_none()
        .w(sz(width))
        .text_right()
        .font_family(theme::MONO_FONT)
        .text_size(sz(11.5))
        .text_color(theme::text_soft())
        .child(text)
}

fn fill(text: impl Into<SharedString>) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .child(text.into())
}

fn small(text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .text_size(sz(11.))
        .text_color(theme::card_soft_text())
        .whitespace_nowrap()
        .child(text.into())
}

fn caps(text: impl Into<SharedString>) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .text_size(sz(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::card_soft_text())
        .child(text.into())
}

fn mini(id: impl Into<ElementId>, label: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .h(sz(22.))
        .px(sz(8.))
        .rounded(sz(5.))
        .border_1()
        .border_color(theme::border_strong())
        .text_size(sz(11.5))
        .text_color(theme::text())
        .cursor_pointer()
        .hover(|s| s.bg(theme::tint()))
        .child(label)
}

fn end_button(id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(sz(22.))
        .rounded(sz(5.))
        .cursor_pointer()
        .hover(|s| s.bg(theme::tint()))
        .child(icons::icon(
            icons::CLOSE,
            sz(10.),
            sz(10.),
            theme::card_soft_text(),
        ))
}

fn no_button() -> Div {
    div().flex_none().w(sz(22.))
}

fn footer() -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(sz(8.))
        .h(sz(FOOT))
        .px(sz(14.))
        .border_t_1()
        .border_color(theme::border())
}

/// A popover's content: its rows, their height (logical) and a footer.
struct Body {
    note: String,
    rows: Vec<AnyElement>,
    height: f32,
    footer: Option<AnyElement>,
}

impl Workspace {
    // ----- the clock and the reads ---------------------------------------------------------------

    /// Starts the bar's clock, once, at its first draw: each second it looks whether a read is due.
    /// A look that reads nothing draws nothing.
    fn status_start(&mut self, cx: &mut Context<Self>) {
        if with(|b| std::mem::replace(&mut b.started, true)) {
            return;
        }
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |this, cx| this.status_tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn status_tick(&mut self, cx: &mut Context<Self>) {
        if cfg!(debug_assertions) && !with(|b| std::mem::replace(&mut b.hooked, true)) {
            self.status_hook(cx);
        }
        let roots = self.status_root_pids(cx);
        let open = matches!(self.popover, Some(Popover::Resources | Popover::Ports));
        let active = self.window_active;
        if with(|b| {
            !b.reading
                && read_due(
                    b.read_at.map(|at| at.elapsed()),
                    open,
                    active,
                    b.roots != roots,
                )
        }) {
            self.status_read(cx);
        }
    }

    /// The programs that the sessions' terminals started, sorted.
    fn status_root_pids(&self, cx: &App) -> Vec<u32> {
        let mut pids: Vec<u32> = self
            .terminals
            .values()
            .filter_map(|t| t.read(cx).child_pid())
            .collect();
        pids.sort_unstable();
        pids
    }

    /// The sessions whose terminal has a running program, in the order of their projects. The
    /// terminals of server sessions are not here: their programs run on the server.
    fn status_terminals(&self, cx: &App) -> Vec<Terminal> {
        let mut sessions: Vec<&Session> = self.state.sessions.iter().collect();
        sessions.sort_by_key(|s| {
            self.state
                .projects
                .iter()
                .position(|p| p.id == s.project_id)
                .unwrap_or(usize::MAX)
        });
        sessions
            .into_iter()
            .filter_map(|s| {
                let pid = self.terminals.get(&s.id)?.read(cx).child_pid()?;
                Some(Terminal {
                    session: s.id.clone(),
                    pid,
                    title: self.session_title(s),
                    agent: s.agent,
                    project: self
                        .state
                        .project(&s.project_id)
                        .map(|p| p.name.clone())
                        .unwrap_or_default(),
                    branch: s.branch.clone(),
                })
            })
            .collect()
    }

    /// Read the programs and the ports in the background.
    fn status_read(&mut self, cx: &mut Context<Self>) {
        let roots = self.status_root_pids(cx);
        let generation = with(|b| {
            b.reading = true;
            b.read_at = Some(Instant::now());
            b.roots = roots;
            b.generation
        });
        let task = cx.background_spawn(async move { (sys::process_use(), sys::listeners()) });
        cx.spawn(async move |this, cx| {
            let (all, listeners) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.status_read_done(generation, all, listeners, cx)
            });
        })
        .detach();
    }

    fn status_read_done(
        &mut self,
        generation: u64,
        all: Vec<ProcessUse>,
        listeners: Vec<Listener>,
        cx: &mut Context<Self>,
    ) {
        // An End finished while this read ran, so the read may still show what ended.
        if with(|b| {
            b.reading = false;
            b.generation != generation
        }) {
            self.status_read(cx);
            return;
        }
        let held = self.status_terminals(cx);
        let (me, now, open) = (std::process::id(), Instant::now(), self.popover);
        let changed = with(|b| {
            let orphans = orphans(&mut b.seen, &held, &all, me, now);
            let model = model(&all, &listeners, &held, &orphans);
            let mut changed = shows_change(b.model.as_ref(), &model, open);
            // What a confirm line names is gone or changed: the line goes too.
            if !b.ending && b.confirm.as_ref().is_some_and(|c| !model.has(&c.target)) {
                b.confirm = None;
                changed = true;
            }
            b.model = Some(model);
            changed
        });
        if changed {
            cx.notify();
        }
    }

    // ----- opening, closing, ending --------------------------------------------------------------

    /// An item of the bar was clicked: its popover opens (a click outside an open popover closes
    /// it first, so it never gets here then).
    fn status_toggle(&mut self, popover: Popover, window: &mut Window, cx: &mut Context<Self>) {
        if self.popover.is_some() {
            self.status_close(window, cx);
            return;
        }
        self.toggle_popover(popover, cx);
        let reading = with(|b| {
            b.confirm = None;
            b.focus_pending = true;
            b.reading
        });
        if popover != Popover::Usage && !reading {
            self.status_read(cx);
        }
    }

    /// Close the popover; the keyboard goes back to what had it.
    fn status_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(popover) = self.popover else { return };
        self.toggle_popover(popover, cx);
        let (focus, before) = with(|b| {
            b.confirm = None;
            b.focus_pending = false;
            (b.focus.clone(), b.before.take())
        });
        if focus.is_some_and(|f| f.is_focused(window)) {
            match before {
                Some(before) => window.focus(&before, cx),
                None => window.focus(self.focus_handle_ref(), cx),
            }
        }
    }

    fn status_ask(&mut self, target: Target, question: String, cx: &mut Context<Self>) {
        with(|b| {
            if !b.ending {
                b.confirm = Some(Confirm { target, question });
            }
        });
        cx.notify();
    }

    fn status_cancel(&mut self, cx: &mut Context<Self>) {
        with(|b| b.confirm = None);
        cx.notify();
    }

    /// End what the confirm line names, in the background; then read again. An error shows as a
    /// toast with its words.
    fn status_end(&mut self, cx: &mut Context<Self>) {
        let Some(target) = with(|b| {
            if b.ending {
                return None;
            }
            let target = b.confirm.as_ref().map(|c| c.target.clone());
            b.ending = target.is_some();
            target
        }) else {
            return;
        };
        let roots: Vec<Root> = self
            .status_terminals(cx)
            .into_iter()
            .map(|t| Root {
                session: t.session,
                pid: t.pid,
            })
            .collect();
        let me = std::process::id();
        let task = cx.background_spawn(async move { end(&target, &roots, me) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let reading = with(|b| {
                    b.ending = false;
                    b.confirm = None;
                    b.generation += 1;
                    b.reading
                });
                this.show_toast(result.unwrap_or_else(|error| error), cx);
                if !reading {
                    this.status_read(cx);
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// `GLASSCODE_DEBUG_OPEN=status…` (debug builds; see the top of this file).
    fn status_hook(&mut self, cx: &mut Context<Self>) {
        let Some(mode) = std::env::var("GLASSCODE_DEBUG_OPEN")
            .ok()
            .filter(|m| m == "status" || m.starts_with("status:"))
        else {
            return;
        };
        let (popover, choice, scale) = parse_hook(&mode);
        if choice.is_some() || scale.is_some() {
            self.set_appearance(
                |a| {
                    a.theme = choice.unwrap_or(a.theme);
                    a.scale = scale.unwrap_or(a.scale);
                },
                cx,
            );
        }
        if let Some(popover) = popover.filter(|_| self.popover.is_none()) {
            self.toggle_popover(popover, cx);
            let reading = with(|b| {
                b.focus_pending = true;
                b.reading
            });
            if popover != Popover::Usage && !reading {
                self.status_read(cx);
            }
        }
    }

    // ----- the bar -------------------------------------------------------------------------------

    /// Each Claude and Codex account, with the meters of the limits the usage scan last read.
    pub(crate) fn status_accounts(&self) -> Vec<Account> {
        let now = glasscode_core::model::now() as i64;
        let report = self.usage.report.as_ref();
        self.state
            .accounts
            .iter()
            .filter(|a| a.agent != AgentKind::Shell)
            .map(|a| {
                let found = report.and_then(|r| r.accounts.iter().find(|x| x.id == a.id));
                Account {
                    agent: a.agent,
                    title: format!("{} · {}", a.agent.name(), a.label),
                    plan: a
                        .plan
                        .clone()
                        .or_else(|| found.and_then(|r| r.plan.as_deref()).map(capitalize)),
                    meters: found
                        .map(|r| r.limits.iter().map(|l| meter(l, now)).collect())
                        .unwrap_or_default(),
                }
            })
            .collect()
    }

    /// The counts of the bar after the last read, for `fit_at`.
    pub(crate) fn status_summary(&self) -> Option<Summary> {
        with(|b| b.model.as_ref().map(|m| m.summary.clone()))
    }

    /// The usage item (a meter for each account with limits) and its refresh; nothing when no
    /// limit is known.
    pub(crate) fn status_usage(
        &self,
        accounts: &[Account],
        fit: Fit,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let shown = bar_accounts(accounts);
        if shown.is_empty() {
            return Vec::new();
        }
        let scale = theme::scale();
        let meters = shown.iter().map(|account| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(sz(INNER_GAP))
                .child(icons::brand(
                    icons::agent_brand(account.agent),
                    ICON * scale,
                ))
                .children(
                    account
                        .meters
                        .iter()
                        .take(if fit.week { 2 } else { 1 })
                        .map(|m| {
                            div()
                                .flex()
                                .flex_none()
                                .items_center()
                                .gap(sz(INNER_GAP))
                                .child(
                                    div()
                                        .flex()
                                        .flex_none()
                                        .w(sz(METER_WIDTH))
                                        .child(ui::meter(m.fraction, m.hot)),
                                )
                                .child(m.bar_text(!fit.words))
                        }),
                )
        });
        let usage = item("status-usage", self.popover == Some(Popover::Usage))
            .gap(sz(ACCOUNT_GAP))
            .child(anchor(0))
            .children(meters)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.status_toggle(Popover::Usage, window, cx)
            }));
        let loading = self.usage.loading;
        let refresh = item("status-usage-refresh", false)
            .child(icons::icon(
                icons::REFRESH,
                sz(ICON),
                sz(ICON),
                if loading {
                    theme::faint()
                } else {
                    theme::muted()
                },
            ))
            .when(!loading, |d| {
                d.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.usage_refresh(cx)))
            });
        vec![usage.into_any_element(), refresh.into_any_element()]
    }

    /// The counts on the right: memory, terminals with the orphans, ports. Nothing before the
    /// first read.
    pub(crate) fn status_counts(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.status_start(cx);
        let Some(summary) = self.status_summary() else {
            return Vec::new();
        };
        let resources = self.popover == Some(Popover::Resources);
        let open = |popover: Popover| {
            cx.listener(move |this: &mut Workspace, _: &ClickEvent, window, cx| {
                this.status_toggle(popover, window, cx)
            })
        };
        let glyph = |data: &'static [u8]| icons::icon(data, sz(ICON), sz(ICON), theme::muted());
        vec![
            item("status-memory", resources)
                .child(anchor(1))
                .child(glyph(icons::CPU))
                .child(summary.memory.clone())
                .on_click(open(Popover::Resources))
                .into_any_element(),
            item("status-terminals", resources)
                .child(anchor(2))
                .child(glyph(icons::TERMINAL))
                .child(summary.terminals.to_string())
                .when(summary.orphans > 0, |d| {
                    d.child(
                        div()
                            .text_color(theme::attention())
                            .child(format!("({})", summary.orphans)),
                    )
                })
                .on_click(open(Popover::Resources))
                .into_any_element(),
            item("status-ports", self.popover == Some(Popover::Ports))
                .child(anchor(3))
                .child(glyph(icons::PLUG))
                .child(summary.ports.to_string())
                .on_click(open(Popover::Ports))
                .into_any_element(),
        ]
    }

    // ----- the popovers --------------------------------------------------------------------------

    /// A popover over its item of the bar, and under it a layer over the window that closes it at
    /// a press and keeps the press from what lies below.
    pub(crate) fn status_popover(
        &mut self,
        popover: Popover,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focus = match with(|b| b.focus.clone()) {
            Some(focus) => focus,
            None => {
                let focus = cx.focus_handle();
                with(|b| b.focus = Some(focus.clone()));
                focus
            }
        };
        // It takes the keyboard once, when it opens, for Esc.
        if with(|b| std::mem::take(&mut b.focus_pending)) {
            let before = window.focused(cx);
            with(|b| b.before = before);
            window.focus(&focus, cx);
        }
        let (model, confirm, ending, other_open, anchors) = with(|b| {
            (
                b.model.clone(),
                b.confirm.clone(),
                b.ending,
                b.other_open,
                b.anchors,
            )
        });
        let (glyph, title, body) = match popover {
            Popover::Usage => (icons::GAUGE, "Usage", self.usage_body(cx)),
            Popover::Resources => (
                icons::CPU,
                "Resources",
                self.resources_body(model.as_ref(), confirm.as_ref(), ending, cx),
            ),
            Popover::Ports => (
                icons::PLUG,
                "Ports",
                self.ports_body(model.as_ref(), confirm.as_ref(), ending, other_open, cx),
            ),
        };

        let viewport = window.viewport_size();
        let (width_screen, height_screen) = (f32::from(viewport.width), f32::from(viewport.height));
        let margin = f32::from(sz(MARGIN));
        let width = f32::from(sz(POPOVER_WIDTH)).min(width_screen - 2. * margin);
        let bounds = |slot: usize| {
            anchors[slot].map(|b| (f32::from(b.origin.x), f32::from(b.origin.x + b.size.width)))
        };
        let item = match popover {
            Popover::Usage => bounds(0),
            Popover::Resources => match (bounds(1), bounds(2)) {
                (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.max(b.1))),
                (a, b) => a.or(b),
            },
            Popover::Ports => bounds(3),
        };
        let left = place(width_screen, width, item, popover == Popover::Usage, margin);
        let bottom = f32::from(sz(BAR_HEIGHT)) + margin;
        // The body gives up height first, in a low window (900 x 560 at 130 %), and scrolls.
        let room = height_screen - f32::from(sz(crate::shell::TITLE_HEIGHT)) - bottom - margin;
        let chrome = f32::from(sz(HEAD))
            + if body.footer.is_some() {
                f32::from(sz(FOOT))
            } else {
                0.
            };
        let body_height = f32::from(sz(body.height))
            .min(room - chrome)
            .max(f32::from(sz(ROW)));
        let scroll = window
            .use_keyed_state(("status-popover-scroll", popover as usize), cx, |_, _| {
                ScrollHandle::new()
            })
            .read(cx)
            .clone();
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(sz(8.))
            .h(sz(HEAD))
            .px(sz(14.))
            .border_b_1()
            .border_color(theme::border())
            .child(icons::icon(glyph, sz(15.), sz(15.), theme::text_soft()))
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_right()
                    .text_size(sz(11.5))
                    .text_color(theme::card_soft_text())
                    .child(body.note),
            );
        let list = div().flex().flex_col().py(sz(BODY_PAD)).children(body.rows);
        deferred(
            div()
                .id("status-popover-layer")
                .occlude()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        this.status_close(window, cx)
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        this.status_close(window, cx)
                    }),
                )
                .child(
                    // Glass over whatever is on screen, with the dialog colour under it: with no
                    // backdrop painted, the tint alone would show the text below through (U15).
                    crate::backdrop::glass_strong(sz(12.))
                        .bg(theme::dialog())
                        .id("status-popover")
                        .occlude()
                        .track_focus(&focus)
                        .key_context("StatusPopover")
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key == "escape" {
                                cx.stop_propagation();
                                this.status_close(window, cx);
                            }
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                        .absolute()
                        .left(px(left))
                        .bottom(px(bottom))
                        .w(px(width))
                        .flex()
                        .flex_col()
                        .shadow_lg()
                        .text_size(sz(12.5))
                        .text_color(theme::text())
                        .child(header)
                        .child(
                            div()
                                .flex()
                                .flex_none()
                                .h(px(body_height))
                                .child(ui::scroll_area(
                                    ("status-popover-list", popover as usize),
                                    &scroll,
                                    list,
                                )),
                        )
                        .children(body.footer),
                ),
        )
        // Under the toast (3), which shows what an End did.
        .with_priority(2)
        .into_any_element()
    }

    fn usage_body(&self, cx: &mut Context<Self>) -> Body {
        let accounts = self.status_accounts();
        let scale = theme::scale();
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut height = 2. * BODY_PAD;
        for account in &accounts {
            rows.push(
                group_row()
                    .child(icons::brand(icons::agent_brand(account.agent), 14. * scale))
                    .child(fill(account.title.clone()).font_weight(FontWeight::MEDIUM))
                    .children(account.plan.clone().map(small))
                    .into_any_element(),
            );
            height += GROUP;
            for m in &account.meters {
                rows.push(
                    row()
                        .pl(sz(36.))
                        .child(
                            div()
                                .flex_none()
                                .w(sz(64.))
                                .text_color(theme::card_soft_text())
                                .child(m.window.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_1()
                                .min_w(sz(40.))
                                .child(ui::meter(m.fraction, m.hot)),
                        )
                        .child(number(m.text(), 112.))
                        .into_any_element(),
                );
                height += ROW;
            }
            if account.meters.is_empty() {
                rows.push(
                    row()
                        .pl(sz(36.))
                        .child(fill("No limits read yet.").text_color(theme::card_soft_text()))
                        .into_any_element(),
                );
                height += ROW;
            }
        }
        let note = match (&self.usage.report, self.usage.loading) {
            (_, true) => "Reading…".to_string(),
            (Some(report), false) => {
                let ago = (glasscode_core::model::now() as i64 - report.scanned_at).max(0);
                if ago < 60 {
                    "Updated just now".into()
                } else {
                    format!("Updated {}", crate::session_start::ago_secs(ago))
                }
            }
            (None, false) => String::new(),
        };
        let footer = footer()
            .child(small("Time to each reset").flex_1())
            .child(
                ui::button("status-open-usage", "Open usage").on_click(cx.listener(
                    |this, _: &ClickEvent, window, cx| {
                        this.status_close(window, cx);
                        this.go(View::Usage, window, cx);
                    },
                )),
            )
            .into_any_element();
        Body {
            note,
            rows,
            height,
            footer: Some(footer),
        }
    }

    fn resources_body(
        &self,
        model: Option<&Model>,
        confirm: Option<&Confirm>,
        ending: bool,
        cx: &mut Context<Self>,
    ) -> Body {
        let Some(model) = model else {
            return Body {
                note: "Reading…".into(),
                rows: Vec::new(),
                height: 2. * BODY_PAD,
                footer: None,
            };
        };
        let resources = &model.resources;
        let scale = theme::scale();
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut height = 2. * BODY_PAD;
        if resources.projects.is_empty() {
            rows.push(
                row()
                    .child(
                        fill("No session has a running terminal.")
                            .text_color(theme::card_soft_text()),
                    )
                    .into_any_element(),
            );
            height += ROW;
        }
        for project in &resources.projects {
            rows.push(
                group_row()
                    .child(caps(project.name.to_uppercase()))
                    .child(number(project.cpu.clone(), 48.))
                    .child(number(project.memory.clone(), 64.))
                    .child(no_button())
                    .into_any_element(),
            );
            height += GROUP;
            for worktree in &project.worktrees {
                rows.push(
                    row()
                        .child(icons::icon(
                            icons::BRANCH,
                            sz(13.),
                            sz(13.),
                            theme::card_soft_text(),
                        ))
                        .child(
                            fill(worktree.branch.clone())
                                .font_family(theme::MONO_FONT)
                                .text_size(sz(12.))
                                .text_color(theme::text_soft()),
                        )
                        .into_any_element(),
                );
                height += ROW;
                for r in &worktree.rows {
                    let mark: AnyElement = match (r.agent, r.orphan) {
                        (Some(agent), _) => icons::brand(icons::agent_brand(agent), 13. * scale),
                        (None, true) => ui::dot(theme::attention()).into_any_element(),
                        (None, false) => div()
                            .flex_none()
                            .size(sz(5.))
                            .rounded_full()
                            .bg(theme::card_soft_text())
                            .into_any_element(),
                    };
                    let label = match &r.title {
                        Some(title) => div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap(sz(6.))
                            .child(fill(title.clone()))
                            .child(small(r.name.clone())),
                        None => div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap(sz(6.))
                            .child(fill(r.name.clone()).text_color(theme::text_soft()))
                            .when(r.orphan, |d| d.child(small("orphan"))),
                    };
                    let question = end_question(&r.name, r.under);
                    let target = Target::Program {
                        pid: r.pid,
                        name: r.name.clone(),
                        orphan: r.orphan,
                        port: None,
                    };
                    rows.push(
                        row()
                            .pl(sz(30. + 14. * r.depth.min(4) as f32))
                            .pr(sz(8.))
                            .when(confirm.is_some_and(|c| c.target == target), |d| {
                                d.bg(theme::tint())
                            })
                            .child(mark)
                            .child(label)
                            .child(number(r.cpu.clone(), 48.))
                            .child(number(r.memory.clone(), 64.))
                            .child(if r.can_end {
                                end_button(("status-end-program", r.pid as usize))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.status_ask(target.clone(), question.clone(), cx)
                                    }))
                                    .into_any_element()
                            } else {
                                no_button().into_any_element()
                            })
                            .into_any_element(),
                    );
                    height += ROW;
                }
            }
        }
        let footer = match confirm {
            Some(confirm) => Some(self.confirm_line(confirm, ending, cx)),
            None if !resources.orphans.is_empty() => {
                let target = Target::Orphans(resources.orphans.clone());
                let question =
                    end_orphans_question(resources.orphans.len(), resources.orphans_under);
                let label = if resources.orphans.len() == 1 {
                    "End 1 orphan terminal".to_string()
                } else {
                    format!("End {} orphan terminals", resources.orphans.len())
                };
                Some(
                    footer()
                        .child(div().flex_1())
                        .child(ui::danger_button("status-end-orphans", label).on_click(
                            cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.status_ask(target.clone(), question.clone(), cx)
                            }),
                        ))
                        .into_any_element(),
                )
            }
            None => None,
        };
        Body {
            note: format!("{} CPU · {}", resources.cpu, resources.memory),
            rows,
            height,
            footer,
        }
    }

    fn ports_body(
        &self,
        model: Option<&Model>,
        confirm: Option<&Confirm>,
        ending: bool,
        other_open: bool,
        cx: &mut Context<Self>,
    ) -> Body {
        let Some(model) = model else {
            return Body {
                note: "Reading…".into(),
                rows: Vec::new(),
                height: 2. * BODY_PAD,
                footer: None,
            };
        };
        let ports = &model.ports;
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut height = 2. * BODY_PAD;
        if ports.sessions.is_empty() {
            rows.push(
                row()
                    .child(
                        fill("No session listens on a port.").text_color(theme::card_soft_text()),
                    )
                    .into_any_element(),
            );
            height += ROW;
        }
        for (i, port) in ports.sessions.iter().enumerate() {
            let number_of = port.port;
            let target = Target::Program {
                pid: port.pid,
                name: port.process.clone(),
                orphan: false,
                port: Some(port.port),
            };
            let question = end_question(
                &format!("{} (port {})", port.process, port.port),
                port.under,
            );
            rows.push(
                row()
                    .h(sz(ROW + 6.))
                    .when(confirm.is_some_and(|c| c.target == target), |d| {
                        d.bg(theme::tint())
                    })
                    .child(
                        div()
                            .flex_none()
                            .w(sz(48.))
                            .font_family(theme::MONO_FONT)
                            .font_weight(FontWeight::MEDIUM)
                            .child(port.port.to_string()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap(sz(6.))
                            .child(
                                div()
                                    .flex_none()
                                    .max_w(sz(140.))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(port.process.clone()),
                            )
                            .child(
                                fill(port.place.clone())
                                    .text_size(sz(11.))
                                    .text_color(theme::card_soft_text()),
                            )
                            .when(port.orphan, |d| d.child(small("orphan"))),
                    )
                    .child(mini(("status-port-open", i), "Open").on_click(
                        move |_: &ClickEvent, _, _| {
                            crate::center::open_url(&format!("http://localhost:{number_of}"))
                        },
                    ))
                    .when(!port.orphan, |d| {
                        d.child(mini(("status-port-kill", i), "Kill").on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| {
                                this.status_ask(target.clone(), question.clone(), cx)
                            },
                        )))
                    })
                    .into_any_element(),
            );
            height += ROW + 6.;
        }
        if !ports.other.is_empty() {
            rows.push(
                group_row()
                    .id("status-other-ports")
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::tint()))
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                        with(|b| b.other_open = !b.other_open);
                        cx.notify();
                    }))
                    .child(
                        icons::icon(
                            icons::CHEVRON_DOWN,
                            sz(12.),
                            sz(12.),
                            theme::card_soft_text(),
                        )
                        .when(!other_open, |s| {
                            s.with_transformation(Transformation::rotate(percentage(0.75)))
                        }),
                    )
                    .child(caps("OTHER PORTS"))
                    .child(
                        div()
                            .flex_none()
                            .text_size(sz(11.))
                            .text_color(theme::card_soft_text())
                            .child(ports.other.len().to_string()),
                    )
                    .into_any_element(),
            );
            height += GROUP;
            if other_open {
                for (port, process) in &ports.other {
                    rows.push(
                        row()
                            .child(
                                div()
                                    .flex_none()
                                    .w(sz(48.))
                                    .font_family(theme::MONO_FONT)
                                    .text_color(theme::text_soft())
                                    .child(port.to_string()),
                            )
                            .child(fill(process.clone()).text_color(theme::card_soft_text()))
                            .into_any_element(),
                    );
                    height += ROW;
                }
            }
        }
        let footer = confirm.map(|confirm| self.confirm_line(confirm, ending, cx));
        Body {
            note: format!(
                "{} in sessions · {} other",
                ports.sessions.len(),
                ports.other.len()
            ),
            rows,
            height,
            footer,
        }
    }

    /// "End <program> and N under it?" with End and Cancel.
    fn confirm_line(&self, confirm: &Confirm, ending: bool, cx: &mut Context<Self>) -> AnyElement {
        footer()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(confirm.question.clone()),
            )
            .child(if ending {
                ui::disabled_button("status-end", "Ending…").into_any_element()
            } else {
                ui::danger_button("status-end", "End")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.status_end(cx)))
                    .into_any_element()
            })
            .when(!ending, |d| {
                d.child(
                    ui::button("status-cancel", "Cancel").on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.status_cancel(cx)),
                    ),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: u32 = 50;

    fn process(
        pid: u32,
        parent: u32,
        name: &str,
        cpu_percent: f32,
        memory_bytes: u64,
    ) -> ProcessUse {
        ProcessUse {
            pid,
            parent,
            name: name.to_string(),
            cpu_percent,
            memory_bytes,
        }
    }

    fn terminal(session: &str, pid: u32, project: &str, branch: &str) -> Terminal {
        Terminal {
            session: session.into(),
            pid,
            title: format!("Title of {session}"),
            agent: AgentKind::Shell,
            project: project.into(),
            branch: branch.into(),
        }
    }

    fn listener(port: u16, pid: u32, process: &str) -> Listener {
        Listener {
            port,
            pid,
            process: process.into(),
        }
    }

    const MB: u64 = 1024 * 1024;

    /// Glasscode (50) with two shell sessions: "web" (a shell with a dev server and its helper)
    /// and "api" in another worktree; programs of the system around them.
    fn programs() -> Vec<ProcessUse> {
        vec![
            process(1, 0, "init", 0.5, 10 * MB),
            process(ME, 1, "glasscode", 12.0, 400 * MB),
            process(100, ME, "pwsh.exe", 0.25, 60 * MB),
            process(101, 100, "node.exe", 30.5, 200 * MB),
            process(102, 101, "esbuild.exe", 2.25, 50 * MB),
            process(200, ME, "pwsh.exe", 0.5, 70 * MB),
            process(300, 1, "postgres.exe", 9.0, 1000 * MB),
        ]
    }

    #[test]
    fn bytes_and_processor_read_short() {
        assert_eq!(bytes(0), "0.0 MB");
        assert_eq!(bytes(5 * MB + MB / 3), "5.3 MB");
        assert_eq!(bytes(412 * MB), "412 MB");
        assert_eq!(bytes(1000 * MB), "1.0 GB");
        assert_eq!(bytes(1310 * MB), "1.3 GB");
        assert_eq!(bytes_coarse(0), "0 MB");
        assert_eq!(bytes_coarse(86 * MB), "86 MB");
        assert_eq!(bytes_coarse(412 * MB), "410 MB");
        assert_eq!(bytes_coarse(416 * MB), "420 MB");
        assert_eq!(bytes_coarse(2048 * MB), "2.0 GB");
        // The coarse text stays while a program's memory moves a little.
        assert_eq!(bytes_coarse(412 * MB), bytes_coarse(413 * MB + MB / 2));
        assert_eq!(cpu(0.), "0.0%");
        assert_eq!(cpu(2.14), "2.1%");
        assert_eq!(cpu(14.4), "14%");
        assert_eq!(cpu(f32::NAN), "0.0%");
        assert_eq!(cpu(-1.), "0.0%");
    }

    #[test]
    fn the_time_to_a_reset() {
        assert_eq!(until(-5), "<1 m");
        assert_eq!(until(59), "<1 m");
        assert_eq!(until(45 * 60), "45 m");
        assert_eq!(until(2 * 3600), "2 h");
        assert_eq!(until(2 * 3600 + 10 * 60 + 59), "2 h 10 m");
        assert_eq!(until(86_400), "1 d");
        assert_eq!(until(86_400 + 4 * 3600), "1 d 4 h");
        assert_eq!(until(3 * 86_400 + 5 * 3600), "3 d");
    }

    #[test]
    fn the_confirm_line_names_what_ends() {
        assert_eq!(end_question("python.exe", 0), "End python.exe?");
        assert_eq!(end_question("pwsh.exe", 2), "End pwsh.exe and 2 under it?");
        assert_eq!(
            end_question("python.exe (port 8765)", 1),
            "End python.exe (port 8765) and 1 under it?"
        );
        assert_eq!(end_orphans_question(1, 0), "End 1 orphan terminal?");
        assert_eq!(
            end_orphans_question(1, 3),
            "End 1 orphan terminal and 3 under it?"
        );
        assert_eq!(
            end_orphans_question(2, 3),
            "End 2 orphan terminals and 3 under them?"
        );
    }

    fn limit(window_minutes: u64, used_percent: f64, resets_at: Option<i64>) -> Limit {
        Limit {
            window_minutes,
            used_percent,
            resets_at,
        }
    }

    #[test]
    fn meters_of_the_limits() {
        let now = 1_000_000;
        let five = meter(&limit(300, 84.4, Some(now + 2 * 3600 + 10 * 60)), now);
        assert_eq!(
            (five.window.as_str(), five.percent.as_str(), five.hot),
            ("5 hours", "84%", true)
        );
        assert_eq!(five.bar_text(false), "84% · 2 h 10 m");
        assert_eq!(five.bar_text(true), "84%");
        assert_eq!(five.text(), "84% · 2 h 10 m");
        let week = meter(&limit(10080, 59., Some(now + 3 * 86_400)), now);
        assert_eq!(
            (week.window.as_str(), week.hot, week.bar_text(false)),
            ("Week", false, "59% week".to_string())
        );
        assert_eq!(week.text(), "59% · 3 d");
        // The Usage page's threshold: 80 is near.
        assert!(meter(&limit(300, 80., None), now).hot);
        assert!(!meter(&limit(300, 79.9, None), now).hot);
        // Beyond the ends, and no reset time.
        let over = meter(&limit(300, 130., None), now);
        assert_eq!(
            (over.fraction, over.percent.as_str(), over.bar_text(false)),
            (1., "100%", "100%".to_string())
        );
        assert_eq!(meter(&limit(300, f64::NAN, None), now).fraction, 0.);
        assert_eq!(meter(&limit(1440, 10., None), now).window, "1-day window");
    }

    #[test]
    fn the_bar_shows_accounts_with_limits_only() {
        let now = 0;
        let with_limits = Account {
            agent: AgentKind::Claude,
            title: "Claude · Personal".into(),
            plan: None,
            meters: vec![
                meter(&limit(300, 10., None), now),
                meter(&limit(10080, 20., None), now),
                meter(&limit(1440, 5., None), now),
            ],
        };
        let without = Account {
            agent: AgentKind::Codex,
            title: "Codex · Work".into(),
            plan: None,
            meters: Vec::new(),
        };
        let shown = bar_accounts(&[without.clone(), with_limits.clone()]);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].meters.len(), 2, "5 hours and week");
        assert!(
            bar_accounts(&[without]).is_empty(),
            "no limit known: no meter and no empty box"
        );
    }

    fn two_accounts() -> Vec<Account> {
        let now = 0;
        let account = |agent| Account {
            agent,
            title: String::new(),
            plan: None,
            meters: vec![
                meter(&limit(300, 84., Some(now + 2 * 3600 + 600)), now),
                meter(&limit(10080, 59., Some(now + 3 * 86_400)), now),
            ],
        };
        vec![account(AgentKind::Claude), account(AgentKind::Codex)]
    }

    fn busy_texts() -> Texts<'static> {
        Texts {
            branch: "⎇ feat/board-drag",
            changes: "12 changed",
            github: Some("acme-bot (work)"),
            cli: Some("Claude 2.1.3 · Codex 0.50.0"),
            saved: "12 sessions saved · restored on relaunch",
            account: Some("Claude · Personal"),
        }
    }

    fn counts() -> Summary {
        Summary {
            memory: "1.2 GB".into(),
            terminals: 12,
            orphans: 1,
            ports: 3,
        }
    }

    #[test]
    fn give_way_drops_in_order_until_the_rest_fits() {
        assert_eq!(give_way(100., 50., &[10., 10., 10.]), [true, true, true]);
        assert_eq!(give_way(75., 50., &[10., 10., 10.]), [false, true, true]);
        assert_eq!(give_way(60., 50., &[10., 10., 10.]), [false, false, true]);
        assert_eq!(
            give_way(40., 50., &[10., 10., 10.]),
            [false, false, false],
            "what is kept stays, and the bar clips"
        );
        assert_eq!(give_way(50., 50., &[0., 10.]), [false, false]);
    }

    /// In a wide window everything shows; as it narrows, the new meters give way before any item
    /// the bar had, and the order holds at each width; in the narrowest window at 130 % what is
    /// kept still fits.
    #[test]
    fn the_bar_fits_at_each_width_and_scale() {
        let (usage, texts, counts) = (two_accounts(), busy_texts(), counts());
        let all = Fit {
            words: true,
            week: true,
            cli: true,
            saved: true,
            github: true,
            account: true,
        };
        assert_eq!(fit_at(2560., 1., &usage, &texts, Some(&counts)), all);
        let as_list = |f: Fit| [f.words, f.week, f.cli, f.saved, f.github, f.account];
        let mut last = as_list(all);
        for width in (500..2560).rev().step_by(10) {
            for scale in [0.9, 1., 1.15, 1.3] {
                let shown = as_list(fit_at(width as f32, scale, &usage, &texts, Some(&counts)));
                // Once a part gives way, each part before it in the order has given way too.
                let first_kept = shown.iter().position(|s| *s).unwrap_or(shown.len());
                assert!(
                    shown[first_kept..].iter().all(|s| *s),
                    "{shown:?} at {width} px and {scale}"
                );
                if scale == 1. {
                    assert!(
                        shown.iter().zip(last).all(|(now, before)| !now || before),
                        "nothing comes back as the window narrows"
                    );
                    last = shown;
                }
            }
        }
        let (kept, optional) = widths(&usage, &texts, Some(&counts));
        assert!(
            kept <= 900. / 1.3,
            "the essentials fit at 900 px and 130 %: {kept}"
        );
        assert!(optional.iter().all(|w| *w >= 0.));
        // 900 px at 130 %: the meters keep their percent; the rest gives way.
        assert_eq!(
            fit_at(900., 1.3, &usage, &texts, Some(&counts)),
            Fit {
                words: false,
                week: false,
                cli: false,
                saved: false,
                github: false,
                account: false
            }
        );
        // With no limit known, the old items get the room: at 1440 px all of them show.
        let old = fit_at(1440., 1., &[], &texts, Some(&counts));
        assert!(old.cli && old.saved && old.github && old.account, "{old:?}");
    }

    #[test]
    fn a_popover_stays_inside_the_window() {
        let margin = 8. * 1.3;
        for (window, scale) in [(900f32, 1.3f32), (900., 1.), (1440., 1.), (2560., 0.9)] {
            let width = (400. * scale).min(window - 2. * margin);
            for item in [
                None,
                Some((12., 200.)),
                Some((window - 220., window - 12.)),
                Some((-50., 10.)),
                Some((window - 5., window + 40.)),
            ] {
                for from_left in [true, false] {
                    let left = place(window, width, item, from_left, margin);
                    assert!(
                        left >= margin - 0.01 && left + width <= window - margin + 0.01,
                        "{left} + {width} in {window} for {item:?}"
                    );
                }
            }
        }
        // Over its item when there is room: Usage from the item's left edge, Ports to its right edge.
        assert_eq!(place(1440., 400., Some((12., 300.)), true, 8.), 12.);
        assert_eq!(place(1440., 400., Some((1300., 1420.)), false, 8.), 1020.);
        // At 900 px and 130 %: it ends at its item, or as far right as the window lets it.
        assert_eq!(place(900., 520., Some((700., 880.)), false, margin), 360.);
        assert_eq!(
            place(900., 520., Some((800., 896.)), false, margin),
            900. - margin - 520.
        );
    }

    #[test]
    fn the_tree_lists_each_program_after_its_parent() {
        let all = programs();
        let order: Vec<(usize, u32, usize)> = tree(100, &all)
            .iter()
            .map(|(d, p, u)| (*d, p.pid, *u))
            .collect();
        assert_eq!(order, [(0, 100, 2), (1, 101, 1), (2, 102, 0)]);
        assert_eq!(under(100, &all), 2);
        assert_eq!(under(102, &all), 0);
        assert_eq!(under(999, &all), 0);
        // A loop in a list read while programs start and end ends.
        let looped = vec![process(7, 8, "a", 0., 0), process(8, 7, "b", 0., 0)];
        assert_eq!(tree(7, &looped).len(), 2);
    }

    #[test]
    fn no_sessions() {
        let all = programs();
        let model = model(&all, &[listener(5432, 300, "postgres.exe")], &[], &[]);
        assert!(model.resources.projects.is_empty());
        assert_eq!(
            model.summary,
            Summary {
                memory: "0 MB".into(),
                terminals: 0,
                orphans: 0,
                ports: 0
            }
        );
        assert!(model.ports.sessions.is_empty());
        assert_eq!(model.ports.other, [(5432, "postgres.exe".to_string())]);
        assert!(model.resources.orphans.is_empty());
    }

    #[test]
    fn sessions_by_project_and_worktree_with_their_programs() {
        let all = programs();
        let held = [
            terminal("web", 100, "shop", "main"),
            terminal("api", 200, "shop", "feat/api"),
        ];
        let model = model(&all, &[], &held, &[]);
        assert_eq!(model.resources.projects.len(), 1);
        let shop = &model.resources.projects[0];
        assert_eq!(
            (shop.name.as_str(), shop.cpu.as_str(), shop.memory.as_str()),
            ("shop", "34%", "380 MB")
        );
        assert_eq!(
            shop.worktrees
                .iter()
                .map(|w| w.branch.as_str())
                .collect::<Vec<_>>(),
            ["main", "feat/api"]
        );
        let web = &shop.worktrees[0].rows;
        assert_eq!(
            web.iter()
                .map(|r| (r.pid, r.depth, r.under))
                .collect::<Vec<_>>(),
            [(100, 0, 2), (101, 1, 1), (102, 2, 0)]
        );
        assert_eq!(web[0].title.as_deref(), Some("Title of web"));
        assert_eq!(web[0].agent, Some(AgentKind::Shell));
        assert!(web[1].title.is_none() && web.iter().all(|r| r.can_end && !r.orphan));
        assert_eq!(
            model.summary,
            Summary {
                memory: "380 MB".into(),
                terminals: 2,
                orphans: 0,
                ports: 0
            }
        );
        // Glasscode itself and the programs of the system are in no row.
        let pids: Vec<u32> = model
            .resources
            .projects
            .iter()
            .flat_map(|p| &p.worktrees)
            .flat_map(|w| &w.rows)
            .map(|r| r.pid)
            .collect();
        assert!(!pids.contains(&ME) && !pids.contains(&1) && !pids.contains(&300));
    }

    #[test]
    fn a_port_of_a_session_and_a_port_with_no_session() {
        let all = programs();
        let held = [terminal("web", 100, "shop", "main")];
        let model = model(
            &all,
            &[
                listener(5173, 101, "node.exe"),
                listener(5432, 300, "postgres.exe"),
                listener(9000, 777, ""),
            ],
            &held,
            &[],
        );
        assert_eq!(
            model.ports.sessions,
            [Port {
                port: 5173,
                pid: 101,
                process: "node.exe".into(),
                place: "shop · main".into(),
                under: 1,
                orphan: false
            }]
        );
        assert_eq!(
            model.ports.other,
            [
                (5432, "postgres.exe".to_string()),
                (9000, "process 777".to_string())
            ]
        );
        assert_eq!(model.summary.ports, 1);
        // Two entries of one port (IPv4 and IPv6 by two programs) count once.
        let twice = super::model(
            &all,
            &[
                listener(5173, 101, "node.exe"),
                listener(5173, 102, "esbuild.exe"),
            ],
            &held,
            &[],
        );
        assert_eq!((twice.ports.sessions.len(), twice.summary.ports), (2, 1));
    }

    #[test]
    fn one_orphan() {
        let start = Instant::now();
        let mut seen = HashMap::new();
        let mut all = programs();
        let web = terminal("web", 100, "shop", "main");
        // A read while the terminal holds its shell: no orphan.
        assert!(orphans(&mut seen, std::slice::from_ref(&web), &all, ME, start).is_empty());
        // The session closed, the shell runs on as a child of Glasscode: not yet...
        assert!(orphans(&mut seen, &[], &all, ME, start + Duration::from_secs(2)).is_empty());
        // ...and 10 seconds after the read that found it let go, an orphan.
        let found = orphans(
            &mut seen,
            &[],
            &all,
            ME,
            start + ORPHAN_AFTER + Duration::from_secs(2),
        );
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].terminal.pid, found[0].name.as_str()),
            (100, "pwsh.exe")
        );

        let model = model(&all, &[listener(5173, 101, "node.exe")], &[], &found);
        assert_eq!(model.resources.orphans, [(100, "pwsh.exe".to_string())]);
        assert_eq!(model.resources.orphans_under, 2);
        assert_eq!(
            model.summary,
            Summary {
                memory: "310 MB".into(),
                terminals: 1,
                orphans: 1,
                ports: 1
            }
        );
        let rows = &model.resources.projects[0].worktrees[0].rows;
        assert!(
            rows[0].orphan && rows[0].can_end && rows[0].title.is_none(),
            "the orphan's program can end"
        );
        assert!(
            rows[1..].iter().all(|r| !r.orphan && !r.can_end),
            "the programs under it end with it"
        );
        assert!(
            model.ports.sessions[0].orphan,
            "its port has Open and no Kill"
        );
        assert_eq!(model.summary.terminals_text(), "1 (1)");

        // It ends: it is forgotten.
        all.retain(|p| p.pid != 100);
        assert!(orphans(&mut seen, &[], &all, ME, start + Duration::from_secs(30)).is_empty());
        assert!(seen.is_empty());
    }

    #[test]
    fn what_is_no_orphan() {
        let start = Instant::now();
        let later = start + ORPHAN_AFTER + Duration::from_secs(1);
        let web = terminal("web", 100, "shop", "main");
        // Its number is another program's now.
        let mut seen = HashMap::new();
        orphans(
            &mut seen,
            std::slice::from_ref(&web),
            &programs(),
            ME,
            start,
        );
        let mut reused = programs();
        reused[2].name = "git.exe".into();
        assert!(orphans(&mut seen, &[], &reused, ME, start).is_empty());
        assert!(orphans(&mut seen, &[], &reused, ME, later).is_empty());
        assert!(seen.is_empty(), "forgotten at once");
        // Not a child of this Glasscode: its parent ended, or another program started it.
        let mut seen = HashMap::new();
        orphans(
            &mut seen,
            std::slice::from_ref(&web),
            &programs(),
            ME,
            start,
        );
        let mut moved = programs();
        moved[2].parent = 0;
        assert!(orphans(&mut seen, &[], &moved, ME, later).is_empty());
        // A program no terminal ever held, even a shell under Glasscode.
        let mut seen = HashMap::new();
        assert!(orphans(&mut seen, &[], &programs(), ME, start).is_empty());
        assert!(orphans(&mut seen, &[], &programs(), ME, later).is_empty());
        // A terminal holds it again (a session that restarted with the same program): the wait starts over.
        let mut seen = HashMap::new();
        orphans(
            &mut seen,
            std::slice::from_ref(&web),
            &programs(),
            ME,
            start,
        );
        orphans(&mut seen, &[], &programs(), ME, start);
        orphans(
            &mut seen,
            std::slice::from_ref(&web),
            &programs(),
            ME,
            start + Duration::from_secs(5),
        );
        assert!(
            orphans(
                &mut seen,
                &[],
                &programs(),
                ME,
                start + Duration::from_secs(12)
            )
            .is_empty()
        );
    }

    #[test]
    fn only_a_session_or_an_orphan_may_end() {
        let all = programs();
        let roots = [Root {
            session: "web".into(),
            pid: 100,
        }];
        assert_eq!(may_end(101, "node.exe", false, &all, &roots, ME), Ok(()));
        assert_eq!(may_end(100, "pwsh.exe", false, &all, &roots, ME), Ok(()));
        assert!(
            may_end(300, "postgres.exe", false, &all, &roots, ME).is_err(),
            "no session"
        );
        assert!(may_end(ME, "glasscode", false, &all, &roots, ME).is_err());
        assert!(
            may_end(101, "python.exe", false, &all, &roots, ME).is_err(),
            "another program has the number now"
        );
        assert!(
            may_end(555, "node.exe", false, &all, &roots, ME).is_err(),
            "ended"
        );
        // An orphan: a child of Glasscode that no terminal holds.
        assert_eq!(may_end(200, "pwsh.exe", true, &all, &roots, ME), Ok(()));
        assert!(
            may_end(100, "pwsh.exe", true, &all, &roots, ME).is_err(),
            "a terminal holds it"
        );
        assert!(
            may_end(101, "node.exe", true, &all, &[], ME).is_err(),
            "not a child of Glasscode"
        );
    }

    #[test]
    fn a_confirm_line_goes_with_what_it_names() {
        let all = programs();
        let held = [terminal("web", 100, "shop", "main")];
        let model = model(&all, &[listener(5173, 101, "node.exe")], &held, &[]);
        assert!(model.has(&Target::Program {
            pid: 101,
            name: "node.exe".into(),
            orphan: false,
            port: None
        }));
        assert!(model.has(&Target::Program {
            pid: 101,
            name: "node.exe".into(),
            orphan: false,
            port: Some(5173)
        }));
        assert!(!model.has(&Target::Program {
            pid: 101,
            name: "node.exe".into(),
            orphan: false,
            port: Some(3000)
        }));
        assert!(
            !model.has(&Target::Program {
                pid: 101,
                name: "deno.exe".into(),
                orphan: false,
                port: None
            }),
            "its number is another program's"
        );
        assert!(
            !model.has(&Target::Program {
                pid: 300,
                name: "postgres.exe".into(),
                orphan: false,
                port: None
            }),
            "not a session's"
        );
        assert!(model.has(&Target::Orphans(Vec::new())));
        assert!(!model.has(&Target::Orphans(vec![(100, "pwsh.exe".into())])));
    }

    #[test]
    fn only_a_change_on_screen_draws() {
        let all = programs();
        let held = [terminal("web", 100, "shop", "main")];
        let before = model(&all, &[], &held, &[]);
        let mut busier = all.clone();
        busier[3].cpu_percent = 80.;
        let after = model(&busier, &[], &held, &[]);
        assert_eq!(before.summary, after.summary);
        assert!(shows_change(None, &before, None), "the first read");
        assert!(
            !shows_change(Some(&before), &after, None),
            "the processor moved: nothing on the bar changes"
        );
        assert!(!shows_change(Some(&before), &after, Some(Popover::Ports)));
        assert!(!shows_change(Some(&before), &after, Some(Popover::Usage)));
        assert!(shows_change(
            Some(&before),
            &after,
            Some(Popover::Resources)
        ));
        let mut bigger = all.clone();
        bigger[3].memory_bytes += 300 * MB;
        assert!(
            shows_change(Some(&before), &model(&bigger, &[], &held, &[]), None),
            "the bar's memory changed"
        );
    }

    #[test]
    fn reads_are_due() {
        let s = Duration::from_secs;
        assert!(
            read_due(None, false, false, false),
            "the first read, whatever the window"
        );
        assert!(!read_due(Some(s(1)), false, true, false));
        assert!(!read_due(Some(s(14)), false, true, false));
        assert!(read_due(Some(s(15)), false, true, false));
        assert!(read_due(Some(s(2)), true, true, false), "a popover is open");
        assert!(
            read_due(Some(s(1)), false, true, true),
            "a terminal started or went"
        );
        assert!(
            !read_due(Some(s(60)), true, false, true),
            "never while the window is in the back"
        );
    }

    #[test]
    fn the_hook_names_a_popover_a_theme_and_a_scale() {
        assert_eq!(parse_hook("status"), (None, None, None));
        assert_eq!(
            parse_hook("status:ports:dark:130"),
            (
                Some(Popover::Ports),
                Some(ThemeChoice::Dark),
                theme::scale_named("130")
            )
        );
        assert_eq!(
            parse_hook("status:light:resources"),
            (Some(Popover::Resources), Some(ThemeChoice::Light), None)
        );
    }
}
