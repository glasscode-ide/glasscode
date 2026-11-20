//! The cards of Today (slice U11): what each one shows, made from plain data, and how it is
//! drawn on glass. Each card's model is a pure function of data that `TodayUi` keeps (nothing is
//! read from a file or a program while Today draws, as it draws ten times a second while the
//! backdrop moves); `today_ui.rs` gathers the data and lays the cards out in the grid.

use glasscode_core::activity::Activity;
use glasscode_core::board::Item;
use glasscode_core::day::{self, Day};
use glasscode_core::github::{CheckState, MyPr};
use glasscode_core::model::AgentKind;
use glasscode_core::usage::{self, Limit};
use gpui::{
    AnyElement, Bounds, ClickEvent, Context, Div, FontWeight, Hsla, ScrollHandle, SharedString,
    canvas, div, fill, linear_color_stop, linear_gradient, point, prelude::*, px, relative, size,
};

use crate::backdrop;
use crate::icons;
use crate::motion;
use crate::sidebar::RowState;
use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

// ----- the grid ---------------------------------------------------------------------------------

/// The cards, in the order they show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Card {
    Needs,
    ComingUp,
    Usage,
    Activity,
    Recent,
    PullRequests,
    Month,
}

/// The grid has this many tracks; a card spans some of them, as in the mock.
pub const TRACKS: u16 = 12;
/// The width of the centre, in logical pixels, from which the grid has three columns, and two:
/// the mock's container queries (70em and 38em at its 13 px).
const THREE_FROM: f32 = 910.;
const TWO_FROM: f32 = 494.;

/// How many columns the cards make in a centre `width` logical pixels wide.
pub fn columns(width: f32) -> usize {
    if width >= THREE_FROM {
        3
    } else if width >= TWO_FROM {
        2
    } else {
        1
    }
}

/// The tracks of the 12 that a card spans: in three columns the rows are 5 4 3, 7 5 and 8 4; in
/// two the wide cards take a row and the narrow ones half of one; in one each takes a row.
/// `quiet`: the heat map has nothing to show, so Activity is the narrow card of its row and
/// Recent sessions the wide one (a wide card for one sentence is wasted).
pub fn span(card: Card, columns: usize, quiet: bool) -> u16 {
    match columns {
        3 => match card {
            Card::Needs => 5,
            Card::ComingUp => 4,
            Card::Usage => 3,
            Card::Activity if quiet => 4,
            Card::Recent if quiet => 8,
            Card::Activity => 7,
            Card::Recent => 5,
            Card::PullRequests => 8,
            Card::Month => 4,
        },
        2 => match card {
            Card::ComingUp | Card::Usage | Card::PullRequests | Card::Month => TRACKS / 2,
            _ => TRACKS,
        },
        _ => TRACKS,
    }
}

/// The one gap of the page: between the cards, and between the start box, the grid and the list.
pub const GAP: f32 = 14.;
/// The height of the cards of a row (at 100 %): a list shows three rows and a part of the
/// fourth, which says that it scrolls; the heat map fits at the widest centre.
pub const CARD_HEIGHT: f32 = 236.;
/// The height of a row whose cards all have only their sentence.
pub const CARD_HEIGHT_EMPTY: f32 = 152.;

/// The height of each card, in the order of `cards`, which has each card and whether it has
/// nothing to show: the cards of a row are as high as each other. `None`: the card's own height
/// (the heat map alone in a row, whose height follows its width).
pub fn heights(cards: &[(Card, bool)], columns: usize, quiet: bool) -> Vec<Option<f32>> {
    fn close(row: &mut Vec<(Card, bool)>, out: &mut Vec<Option<f32>>) {
        let height = if row.iter().all(|(_, empty)| *empty) {
            Some(CARD_HEIGHT_EMPTY)
        } else if matches!(row.as_slice(), [(Card::Activity, _)]) {
            None
        } else {
            Some(CARD_HEIGHT)
        };
        out.extend(row.drain(..).map(|_| height));
    }
    let mut out = Vec::with_capacity(cards.len());
    let mut row: Vec<(Card, bool)> = Vec::new();
    let mut tracks = 0;
    for (card, empty) in cards {
        let wide = span(*card, columns, quiet);
        if tracks + wide > TRACKS {
            close(&mut row, &mut out);
            tracks = 0;
        }
        tracks += wide;
        row.push((*card, *empty));
    }
    close(&mut row, &mut out);
    out
}

// ----- a card's rows ------------------------------------------------------------------------------

/// What a click on a row does.
#[derive(Clone, Debug, PartialEq)]
pub enum Open {
    /// The row only tells.
    Nothing,
    Session(String),
    Server(String),
    Url(String),
    /// The Inbox tab of the right panel (the requests that wait for an OK).
    Inbox,
    /// The Usage page.
    Usage,
}

/// The colour of a row's icon (never of its text: text on glass is `text`, `text_soft` or
/// `card_soft_text`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Soft,
    Ok,
    Bad,
    Busy,
    Attention,
}

impl Tone {
    fn color(self) -> Hsla {
        match self {
            Tone::Soft => theme::card_soft_text(),
            Tone::Ok => theme::added(),
            Tone::Bad => theme::removed(),
            Tone::Busy | Tone::Attention => theme::attention(),
        }
    }
}

/// What a row starts with.
#[derive(Clone, Debug, PartialEq)]
pub enum Mark {
    None,
    Agent(AgentKind),
    Icon(&'static [u8], Tone),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// A short time or day before the row ("Tomorrow", "Fri").
    pub time: Option<String>,
    pub mark: Mark,
    pub title: String,
    pub detail: String,
    /// The pill of the one thing to do ("Answer"); it does what a click on the row does.
    pub action: Option<&'static str>,
    /// A session's state, as a dot at the end.
    pub state: Option<RowState>,
    pub open: Open,
}

impl Line {
    fn new(mark: Mark, title: impl Into<String>, detail: impl Into<String>, open: Open) -> Line {
        Line {
            time: None,
            mark,
            title: title.into(),
            detail: detail.into(),
            action: None,
            state: None,
            open,
        }
    }
}

/// A card that is a list: its rows (the card scrolls them), the number in its corner, and the
/// sentence it shows when it has no row.
#[derive(Clone, Debug, PartialEq)]
pub struct List {
    pub lines: Vec<Line>,
    pub count: Option<String>,
    pub empty: Option<&'static str>,
}

/// The most rows a list card has: each is laid out on each frame, and Today draws ten times a
/// second. The number in the card's corner is of all of them.
pub const LIST_MOST: usize = 40;

impl List {
    /// The first [`LIST_MOST`] of `lines`, with how many there are in the corner.
    fn of(mut lines: Vec<Line>, empty: &'static str) -> List {
        let count = (!lines.is_empty()).then(|| lines.len().to_string());
        lines.truncate(LIST_MOST);
        let empty = lines.is_empty().then_some(empty);
        List {
            lines,
            count,
            empty,
        }
    }
}

// ----- Needs you ------------------------------------------------------------------------------------

/// Something that waits for the person (`Workspace::today_needs`).
#[derive(Clone, Debug, PartialEq)]
pub struct Need {
    pub title: String,
    pub what: String,
    pub agent: Option<AgentKind>,
    /// A pull request whose checks fail (not a question).
    pub failing: bool,
    pub open: Open,
}

pub const NEEDS_EMPTY: &str = "Nothing waits for you right now.";

/// Needs you: the requests that wait for an OK (one row that leads to Inbox), then the sessions
/// that wait for an answer and the pull requests whose checks fail.
pub fn needs(needs: &[Need], approvals: usize) -> List {
    let mut lines = Vec::new();
    if approvals > 0 {
        let title = if approvals == 1 {
            "1 request waits for your OK".to_string()
        } else {
            format!("{approvals} requests wait for your OK")
        };
        lines.push(Line {
            action: Some("Answer"),
            ..Line::new(
                Mark::Icon(icons::SHIELD, Tone::Attention),
                title,
                "An agent asked to do something; answer it in Inbox",
                Open::Inbox,
            )
        });
    }
    for need in needs {
        let mark = match (need.failing, need.agent) {
            (true, _) => Mark::Icon(icons::PULL_REQUEST, Tone::Bad),
            (false, Some(agent)) => Mark::Agent(agent),
            (false, None) => Mark::Icon(icons::BELL, Tone::Attention),
        };
        lines.push(Line {
            action: Some(if need.failing { "Open" } else { "Answer" }),
            ..Line::new(
                mark,
                need.title.clone(),
                need.what.clone(),
                need.open.clone(),
            )
        });
    }
    List::of(lines, NEEDS_EMPTY)
}

// ----- Coming up ------------------------------------------------------------------------------------

/// An item of the board that is due in the next days, or a review someone asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct Upcoming {
    pub key: String,
    /// "Tomorrow", "Fri", "24 Nov", or "Review".
    pub when: String,
    pub title: String,
    pub detail: String,
    pub url: Option<String>,
}

/// How far ahead Coming up looks.
const AHEAD: Day = 7;
pub const COMING_EMPTY: &str = "Nothing is due in the next 7 days, and no review waits for you.";

fn is_review(item: &Item) -> bool {
    item.key.starts_with("review:") || item.key.starts_with("linear-review:")
}

/// What is due after today and at most 7 days ahead, by day, then the review requests that have
/// no day. Done, snoozed and recurring items stay out; so do items due today or late (they are
/// in Needs you and "Due and late").
pub fn coming_up(items: &[Item], today: Day) -> Vec<Upcoming> {
    let mut dated: Vec<(Day, &Item)> = Vec::new();
    let mut reviews: Vec<&Item> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for item in items {
        if item.done
            || item.snoozed.is_some()
            || item.key.starts_with("recurring:")
            || seen.contains(&item.key.as_str())
        {
            continue;
        }
        seen.push(&item.key);
        match item.due.as_deref().and_then(day::parse_iso) {
            Some(due) if due > today && due - today <= AHEAD => dated.push((due, item)),
            Some(_) => {}
            None if is_review(item) => reviews.push(item),
            None => {}
        }
    }
    dated.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.title.cmp(&b.1.title)));
    let upcoming = |when: String, item: &Item| {
        let mut detail = vec![item.source.clone().unwrap_or_else(|| "Your list".into())];
        if let Some(person) = &item.person {
            detail.push(match item.person_role.as_deref() {
                Some("asked") => format!("{person} asked"),
                Some("waiting") => format!("waiting on {person}"),
                Some("author") => format!("by {person}"),
                _ => person.clone(),
            });
        }
        let title = match item.reference.as_deref().filter(|_| item.outside) {
            Some(reference) => format!("{reference} {}", item.title),
            None => item.title.clone(),
        };
        Upcoming {
            key: item.key.clone(),
            when,
            title,
            detail: detail.join(" · "),
            url: item.url.clone(),
        }
    };
    dated
        .into_iter()
        .map(|(due, item)| upcoming(day::relative(due, today), item))
        .chain(
            reviews
                .into_iter()
                .map(|item| upcoming("Review".into(), item)),
        )
        .collect()
}

/// Coming up as a card; `open` says what a click on each does.
pub fn coming_up_list(upcoming: &[Upcoming], open: impl Fn(&Upcoming) -> Open) -> List {
    let lines = upcoming
        .iter()
        .map(|u| Line {
            time: Some(u.when.clone()),
            ..Line::new(Mark::None, u.title.clone(), u.detail.clone(), open(u))
        })
        .collect();
    List::of(lines, COMING_EMPTY)
}

// ----- Usage -----------------------------------------------------------------------------------------

/// An account and the limits the usage scan last read for it.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageAccount {
    pub agent: AgentKind,
    pub label: String,
    pub limits: Vec<Limit>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Meter {
    pub agent: AgentKind,
    pub fraction: f32,
    /// Near the limit.
    pub hot: bool,
    pub percent: String,
    pub label: String,
    pub resets: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageCard {
    pub meters: Vec<Meter>,
    pub empty: Option<&'static str>,
}

/// A meter past this is near its limit (as on the Usage page).
const NEAR: f64 = 80.;
pub const USAGE_NOT_READ: &str = "Usage is read a short time after Glasscode starts.";
pub const USAGE_EMPTY: &str = "Limits show here once a Claude or Codex session reports them.";

/// One meter for each account with a limit: its fullest window. `None`: the usage scan has not
/// run yet.
pub fn usage(accounts: Option<&[UsageAccount]>, now: i64) -> UsageCard {
    let Some(accounts) = accounts else {
        return UsageCard {
            meters: Vec::new(),
            empty: Some(USAGE_NOT_READ),
        };
    };
    let meters: Vec<Meter> = accounts
        .iter()
        .filter_map(|account| {
            let limit = account
                .limits
                .iter()
                .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))?;
            let used = limit.used_percent.clamp(0., 100.);
            let window = match limit.window_minutes {
                300 => "5 h window".to_string(),
                10080 => "week".to_string(),
                _ => limit.label(),
            };
            Some(Meter {
                agent: account.agent,
                fraction: (used / 100.) as f32,
                hot: used >= NEAR,
                percent: format!("{used:.0}%"),
                label: format!("{} · {window}", account.label),
                resets: limit
                    .resets_at
                    .map(|at| format!("resets {}", reset_text(at, now))),
            })
        })
        .collect();
    let empty = meters.is_empty().then_some(USAGE_EMPTY);
    UsageCard { meters, empty }
}

/// "14:20" today, "Mon 08:00" within the week, else "Nov 24" (the Usage page's words, without
/// its "at": the card is narrow).
fn reset_text(at: i64, now: i64) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (_, month, date, hour, minute, weekday) = usage::local_parts(at);
    if usage::local_day(at) == usage::local_day(now) {
        format!("{hour:02}:{minute:02}")
    } else if at - now < 6 * 86_400 {
        format!("{} {hour:02}:{minute:02}", DAYS[weekday as usize % 7])
    } else {
        format!("{} {date}", MONTHS[(month as usize + 11) % 12])
    }
}

// ----- Activity (the heat map) -----------------------------------------------------------------------

/// Weeks the heat map shows (half a year).
pub const WEEKS: usize = 26;

#[derive(Clone, Debug, PartialEq)]
pub struct Heat {
    /// For each week (oldest first) and day (Monday first): 0 for none, 1 to 4 for more.
    pub levels: Vec<[u8; 7]>,
    /// "Since 16 Nov": the history starts on the first day counted.
    pub since: String,
    pub totals: String,
    pub empty: Option<&'static str>,
}

pub const HEAT_EMPTY: &str = "Each session you start is counted here, one square a day.";

/// The heat map of the sessions started each day. `None`: the file is not read yet.
pub fn heat(activity: Option<&Activity>, today: &str) -> Heat {
    let Some(activity) = activity else {
        return Heat {
            levels: Vec::new(),
            since: String::new(),
            totals: String::new(),
            empty: Some(HEAT_EMPTY),
        };
    };
    let weeks = activity.weeks(today, WEEKS);
    let most = weeks.iter().flatten().copied().max().unwrap_or(0);
    let levels: Vec<[u8; 7]> = weeks
        .iter()
        .map(|week| week.map(|n| level(n, most)))
        .collect();
    // The totals of the days the map shows.
    let first = day::parse_iso(today)
        .map(|t| day::iso(t - i64::from(day::weekday(t)) - 7 * (WEEKS as i64 - 1)))
        .unwrap_or_default();
    let (mut sessions, mut merged, mut lines) = (0u32, 0u32, 0u32);
    for (_, counts) in activity
        .days
        .range(first..)
        .take_while(|(date, _)| date.as_str() <= today)
    {
        sessions = sessions.saturating_add(counts.sessions);
        merged = merged.saturating_add(counts.prs_merged);
        lines = lines.saturating_add(counts.lines);
    }
    let totals = format!(
        "{} · {} merged · {}",
        plural(sessions, "session", "sessions"),
        plural(merged, "pull request", "pull requests"),
        plural(lines, "line", "lines")
    );
    let empty = (most == 0).then_some(HEAT_EMPTY);
    Heat {
        levels,
        since: since_label(&activity.since, today),
        totals,
        empty,
    }
}

/// A day's square: 0 for no session, else 1 to 4 by its share of the busiest day.
fn level(sessions: u32, most: u32) -> u8 {
    if sessions == 0 || most == 0 {
        return 0;
    }
    (sessions * 4).div_ceil(most).clamp(1, 4) as u8
}

/// "Since 16 Nov", with the year when it is not this year's.
fn since_label(since: &str, today: &str) -> String {
    let Some(first) = day::parse_iso(since) else {
        return String::new();
    };
    if since.get(..4) == today.get(..4) {
        format!("Since {}", day::short(first))
    } else {
        format!("Since {} {}", day::short(first), &since[..4])
    }
}

fn plural(n: u32, one: &str, many: &str) -> String {
    format!("{} {}", thousands(n), if n == 1 { one } else { many })
}

/// 31904 as "31,904".
fn thousands(n: u32) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

// ----- Recent sessions -------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Recent {
    pub id: String,
    pub title: String,
    pub project: String,
    pub agent: AgentKind,
    pub state: RowState,
    pub created_at: u64,
}

pub const RECENT_EMPTY: &str = "The sessions you start show here.";

/// The newest sessions; those that wait for the person are in Needs you instead.
pub fn recent(mut sessions: Vec<Recent>, now: u64) -> List {
    sessions.retain(|s| s.state != RowState::NeedsYou);
    sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let lines = sessions
        .into_iter()
        .map(|s| {
            let detail = if s.project.is_empty() {
                ago(s.created_at, now)
            } else {
                format!("{} · {}", s.project, ago(s.created_at, now))
            };
            Line {
                state: Some(s.state),
                ..Line::new(Mark::Agent(s.agent), s.title, detail, Open::Session(s.id))
            }
        })
        .collect();
    List::of(lines, RECENT_EMPTY)
}

/// "started 5 min ago", "started 3 h ago", "started 2 days ago".
fn ago(at: u64, now: u64) -> String {
    let seconds = now.saturating_sub(at);
    match seconds {
        0..60 => "started just now".into(),
        60..3_600 => format!("started {} min ago", seconds / 60),
        3_600..86_400 => format!("started {} h ago", seconds / 3_600),
        86_400..172_800 => "started yesterday".into(),
        _ => format!("started {} days ago", seconds / 86_400),
    }
}

// ----- Pull requests -------------------------------------------------------------------------------

pub const PRS_LOADING: &str = "Looking for your open pull requests…";
pub const PRS_NO_GH: &str =
    "Your open pull requests show here when the GitHub command (gh) is installed and signed in.";
pub const PRS_EMPTY: &str = "You have no open pull requests.";

/// The person's open pull requests (`github::my_open_prs`), newest first. `None`: the first
/// search has not come back; an error: `gh` is missing, not signed in, or did not answer.
pub fn pull_requests(prs: Option<&Result<Vec<MyPr>, String>>) -> List {
    let list = match prs {
        None => {
            return List {
                lines: Vec::new(),
                count: None,
                empty: Some(PRS_LOADING),
            };
        }
        Some(Err(_)) => {
            return List {
                lines: Vec::new(),
                count: None,
                empty: Some(PRS_NO_GH),
            };
        }
        Some(Ok(list)) => list,
    };
    let lines = list
        .iter()
        .map(|pr| {
            let name = pr.repo.rsplit('/').next().unwrap_or(&pr.repo);
            let mut detail = vec![format!("#{}", pr.number), name.to_string()];
            let mut tone = Tone::Soft;
            if pr.draft {
                detail.push("draft".into());
            }
            if let Some(ci) = &pr.ci {
                let failed = ci.failed().len();
                if failed > 0 {
                    tone = Tone::Bad;
                    detail.push(if failed == 1 {
                        "1 check failing".into()
                    } else {
                        format!("{failed} checks failing")
                    });
                } else if ci.running() {
                    tone = Tone::Busy;
                    detail.push("checks running".into());
                } else if ci.checks.iter().any(|c| c.state() == CheckState::Passed) {
                    tone = Tone::Ok;
                    detail.push("checks passed".into());
                }
            }
            match pr.review.as_str() {
                "APPROVED" => detail.push("approved".into()),
                "CHANGES_REQUESTED" => detail.push("changes requested".into()),
                "REVIEW_REQUIRED" => detail.push("review required".into()),
                _ => {}
            }
            let open = if pr.url.is_empty() {
                Open::Nothing
            } else {
                Open::Url(pr.url.clone())
            };
            Line::new(
                Mark::Icon(icons::PULL_REQUEST, tone),
                pr.title.clone(),
                detail.join(" · "),
                open,
            )
        })
        .collect();
    let mut card = List::of(lines, PRS_EMPTY);
    card.count = (!list.is_empty()).then(|| format!("{} open", list.len()));
    card
}

// ----- This month ------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Month {
    pub sessions: u32,
    pub merged: u32,
    /// "9 more than in October", when the month before was counted.
    pub sessions_change: Option<String>,
    pub merged_change: Option<String>,
    pub empty: Option<&'static str>,
}

pub const MONTH_EMPTY: &str = "Nothing is counted this month yet.";

/// The sessions started and the pull requests merged this month, and the change from the month
/// before when the history goes back that far.
pub fn month(activity: Option<&Activity>, today: &str) -> Month {
    let Some(activity) = activity else {
        return Month {
            sessions: 0,
            merged: 0,
            sessions_change: None,
            merged_change: None,
            empty: Some(MONTH_EMPTY),
        };
    };
    let (now, before) = activity.month(today);
    // The month before is compared only when the history began before this month.
    let compared = !activity.since.is_empty() && activity.since.get(..7) < today.get(..7);
    let before_name = day::parse_iso(today).map(|t| {
        const NAMES: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        let (_, m, _) = day::to_civil(t);
        NAMES[(m as usize + 10) % 12]
    });
    let change = |now: u32, before: u32| {
        let name = before_name?;
        compared.then(|| match now.cmp(&before) {
            std::cmp::Ordering::Greater => format!("{} more than in {name}", now - before),
            std::cmp::Ordering::Less => format!("{} fewer than in {name}", before - now),
            std::cmp::Ordering::Equal => format!("as many as in {name}"),
        })
    };
    Month {
        sessions: now.sessions,
        merged: now.prs_merged,
        sessions_change: change(now.sessions, before.sessions),
        merged_change: change(now.prs_merged, before.prs_merged),
        empty: (now.sessions == 0 && now.prs_merged == 0).then_some(MONTH_EMPTY),
    }
}

// ----- drawing -------------------------------------------------------------------------------------

/// From a card's edge to its header and its text.
const PAD: f32 = 16.;
/// From a card's edge to a row: a row's hover reaches this far, and its text is at [`PAD`].
const ROW_PAD: f32 = 6.;

/// A card on glass: an icon and a title, a number in its corner, then its body, which fills the
/// rest of the card. `height` is the card's, at 100 %; none: as high as its body. In the light
/// theme glass on white has almost no edge (its border is white, as in the mock): the soft
/// shadow under it, the mock's, gives it one.
pub fn card(
    glyph: &'static [u8],
    glyph_tone: Tone,
    title: &'static str,
    count: Option<String>,
    height: Option<f32>,
    body: AnyElement,
) -> Div {
    backdrop::glass(theme::sz(14.))
        .flex()
        .flex_col()
        .min_w_0()
        .when_some(height, |d, height| d.h(theme::sz(height)).overflow_hidden())
        .when(theme::is_light(), |d| d.shadow_md())
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(theme::sz(8.))
                .h(theme::sz(20.))
                .mx(theme::sz(PAD))
                .mt(theme::sz(14.))
                .mb(theme::sz(8.))
                .text_size(theme::sz(13.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text_soft())
                .child(icons::icon(
                    glyph,
                    theme::sz(15.),
                    theme::sz(15.),
                    glyph_tone.color(),
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(title),
                )
                .when_some(count, |d, n| {
                    d.child(
                        div()
                            .flex_none()
                            .px(theme::sz(7.))
                            .py(theme::sz(1.))
                            .rounded_full()
                            .bg(theme::tint())
                            .text_size(theme::sz(11.5))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(theme::card_soft_text())
                            .child(n),
                    )
                }),
        )
        // A card of its own height is as high as its body; else the body has what is left.
        .child(
            div()
                .flex()
                .flex_col()
                .when(height.is_some(), |d| d.flex_1().min_h_0())
                .child(body),
        )
}

/// A card's body that does not scroll: it keeps the card's padding, and fills a card of a set
/// height (it grows from its own height, not from nothing: in a card of its own height, the
/// heat map alone in a row, that is the height of the card).
fn padded() -> Div {
    div()
        .flex()
        .flex_col()
        .flex_grow(1.)
        .min_h_0()
        .px(theme::sz(PAD))
        .pb(theme::sz(PAD))
}

/// The microphone of the start box, beside Send: it opens the voice screen (slice U19). Only where
/// `glasscode_ui::VOICE` is true, as the voice helper is a macOS program.
pub fn mic_button() -> gpui::Stateful<Div> {
    div()
        .id("today-voice")
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(theme::sz(38.))
        .rounded_full()
        .cursor_pointer()
        .hover(|s| s.bg(theme::tint()))
        .child(icons::icon(
            icons::MIC,
            theme::sz(17.),
            theme::sz(17.),
            theme::card_soft_text(),
        ))
}

/// A card with nothing to show: its sign and its one sentence, in the middle of the body.
pub fn empty(glyph: &'static [u8], sentence: &'static str) -> AnyElement {
    padded()
        .items_center()
        .justify_center()
        .gap(theme::sz(8.))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(theme::sz(32.))
                .rounded_full()
                .bg(theme::tint())
                .child(icons::icon(
                    glyph,
                    theme::sz(15.),
                    theme::sz(15.),
                    theme::card_soft_text(),
                )),
        )
        .child(
            div()
                .max_w(theme::sz(280.))
                .text_center()
                .text_size(theme::sz(12.5))
                .line_height(theme::sz(18.))
                .text_color(theme::card_soft_text())
                .child(sentence),
        )
        .into_any_element()
}

/// Rows that stand for a list that is on its way (a search for pull requests, a usage scan):
/// a mark and two lines, as wide as the card lets them be. `ring`: the mark is a ring's track,
/// as the Usage card's rows have.
fn waiting(id: &'static str, rows: usize, ring: bool) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .gap(theme::sz(if ring { 8. } else { 13. }))
        .px(theme::sz(PAD))
        .pt(theme::sz(if ring { 5. } else { 8. }))
        .children((0..rows).map(|row| {
            let line = |part: usize, width: f32, height: f32| {
                motion::skeleton_block(
                    (id, row * 3 + part),
                    div().w(relative(width)).h(theme::sz(height)),
                )
            };
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(theme::sz(12.))
                .child(if ring {
                    motion::ring(0., RING, RING_LINE, theme::hover()).into_any_element()
                } else {
                    motion::skeleton((id, row * 3), 16., 16.).into_any_element()
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(theme::sz(6.))
                        .child(line(1, 0.72 - 0.12 * (row % 3) as f32, 11.))
                        .child(line(2, 0.42 + 0.1 * (row % 2) as f32, 9.)),
                )
        }))
        .into_any_element()
}

/// Rows that scroll in their card. While there are rows under the card's lower edge, that edge
/// has a soft veil of the glass's own colour: the row there fades out instead of being cut,
/// which also says that the card scrolls. While rows are above the body, a thin line under the
/// card's header parts them from it. (The handle has what the frame before laid out.)
fn scrolling(id: &'static str, scroll: &ScrollHandle, rows: impl IntoElement) -> AnyElement {
    let (scrolled, most) = (-scroll.offset().y, scroll.max_offset().y);
    let (above, below) = (scrolled > px(1.), most - scrolled > px(1.));
    let mut veil = theme::glass();
    let clear = Hsla { a: 0., ..veil };
    veil.a = 0.88;
    let mut line = theme::text();
    line.a = 0.1;
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(ui::scroll_area(id, scroll, rows))
        .when(above, |d| {
            d.child(
                div()
                    .absolute()
                    .top_0()
                    .left(theme::sz(PAD))
                    .right(theme::sz(PAD))
                    .h(px(1.))
                    .bg(line),
            )
        })
        .when(below, |d| {
            d.child(
                div()
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .w_full()
                    .h(theme::sz(30.))
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(clear, 0.),
                        linear_color_stop(veil, 1.),
                    )),
            )
        })
        .into_any_element()
}

fn mark(mark: &Mark) -> Option<AnyElement> {
    match mark {
        Mark::None => None,
        Mark::Agent(agent) => Some(
            div()
                .flex_none()
                .mt(theme::sz(2.))
                .child(icons::brand(
                    icons::agent_brand(*agent),
                    15.,
                ))
                .into_any_element(),
        ),
        Mark::Icon(glyph, tone) => Some(
            div()
                .flex_none()
                .mt(theme::sz(2.))
                .child(icons::icon(
                    glyph,
                    theme::sz(15.),
                    theme::sz(15.),
                    tone.color(),
                ))
                .into_any_element(),
        ),
    }
}

/// A list card's body: its rows, which scroll in the card (`scroll` is kept by `TodayUi`), or
/// its sentence under `glyph`; `loading`: the list is on its way. `prefix` keeps the ids apart.
pub fn list(
    prefix: &'static str,
    glyph: &'static [u8],
    list: &List,
    loading: bool,
    scroll: &ScrollHandle,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    if let Some(sentence) = list.empty {
        return if loading {
            waiting(prefix, 3, false)
        } else {
            empty(glyph, sentence)
        };
    }
    let rows = div()
        .flex()
        .flex_col()
        .gap(theme::sz(2.))
        .px(theme::sz(PAD - ROW_PAD))
        .pb(theme::sz(PAD - ROW_PAD))
        .children(
            list.lines
                .iter()
                .enumerate()
                .map(|(i, line)| row(SharedString::from(format!("{prefix}-{i}")), line, cx)),
        );
    scrolling(prefix, scroll, rows)
}

fn row(id: SharedString, line: &Line, cx: &mut Context<Workspace>) -> AnyElement {
    let open = line.open.clone();
    let clickable = open != Open::Nothing;
    let pill = SharedString::from(format!("{id}-act"));
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_start()
        .gap(theme::sz(10.))
        .px(theme::sz(ROW_PAD))
        .py(theme::sz(6.))
        .rounded(theme::sz(8.))
        .text_size(theme::sz(13.))
        .when(clickable, |d| {
            d.cursor_pointer()
                .hover(|s| s.bg(theme::tint()))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.today_open(&open, window, cx)
                }))
        })
        .when_some(line.time.clone(), |d, time| {
            d.child(
                div()
                    .flex_none()
                    .w(theme::sz(62.))
                    .pt(theme::sz(1.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_size(theme::sz(12.))
                    .text_color(theme::card_soft_text())
                    .child(time),
            )
        })
        .children(mark(&line.mark))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(theme::sz(1.))
                .child(
                    div()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_color(theme::text())
                        .child(line.title.clone()),
                )
                .when(!line.detail.is_empty(), |d| {
                    d.child(
                        div()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(theme::sz(12.))
                            .text_color(theme::card_soft_text())
                            .child(line.detail.clone()),
                    )
                }),
        )
        // The pill does what the row does: the click reaches the row.
        .when_some(line.action, |d, action| {
            d.child(
                div()
                    .flex_none()
                    .self_center()
                    .child(ui::action_pill(pill, action)),
            )
        })
        .when_some(line.state, |d, state| {
            d.child(
                div()
                    .flex_none()
                    .self_center()
                    .pr(theme::sz(4.))
                    .child(ui::status_dot(state, false)),
            )
        })
        .into_any_element()
}

/// A usage ring is this wide, and its line this thick (at 100 %).
const RING: f32 = 46.;
const RING_LINE: f32 = 5.;

/// The colour of a ring's fill: the "needs you" orange near the limit, else the accent. An
/// accent that is itself an orange (the amber accent) would say "near the limit" for each
/// ring, so the ring is then the soft text colour, as the meters of the status bar are.
fn ring_color(hot: bool) -> Hsla {
    let (accent, attention) = (theme::accent(), theme::attention());
    let apart = (accent.h - attention.h).abs();
    if hot {
        attention
    } else if apart.min(1. - apart) < 0.1 || accent.s < 0.15 {
        theme::text_soft()
    } else {
        accent
    }
}

/// The Usage card's body: a ring for each account, which fills when it first shows, with the
/// percentage in it and the account, the window and the reset beside it. They scroll in the
/// card; a click opens the Usage page. `loading`: a usage scan is under way.
pub fn rings(
    card: &UsageCard,
    loading: bool,
    scroll: &ScrollHandle,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    if let Some(sentence) = card.empty {
        return if loading {
            waiting("today-usage-wait", 2, true)
        } else {
            empty(icons::GAUGE, sentence)
        };
    }
    let rows = div()
        .id("today-usage-meters")
        .flex()
        .flex_col()
        .gap(theme::sz(2.))
        .px(theme::sz(PAD - ROW_PAD))
        .pb(theme::sz(PAD - ROW_PAD))
        .children(card.meters.iter().enumerate().map(|(i, m)| {
            div()
                .id(("today-usage", i))
                .flex()
                .flex_none()
                .items_center()
                .gap(theme::sz(12.))
                .px(theme::sz(ROW_PAD))
                .py(theme::sz(5.))
                .rounded(theme::sz(8.))
                .cursor_pointer()
                .hover(|s| s.bg(theme::tint()))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.today_open(&Open::Usage, window, cx)
                }))
                .child(
                    div()
                        .relative()
                        .flex_none()
                        .size(theme::sz(RING))
                        .child(motion::ring_in(
                            ("today-usage-ring", i),
                            m.fraction,
                            RING,
                            RING_LINE,
                            ring_color(m.hot),
                        ))
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(theme::sz(11.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::text())
                                .child(m.percent.clone()),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(theme::sz(2.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(theme::sz(6.))
                                .text_size(theme::sz(12.5))
                                .text_color(theme::text())
                                .child(icons::brand(
                                    icons::agent_brand(m.agent),
                                    13.,
                                ))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(m.label.clone()),
                                ),
                        )
                        .when_some(m.resets.clone(), |d, resets| {
                            d.child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_size(theme::sz(12.))
                                    .text_color(theme::card_soft_text())
                                    .child(resets),
                            )
                        }),
                )
        }));
    scrolling("today-usage", scroll, rows)
}

/// The heat map: 26 weeks of 7 squares, painted at once (182 elements would each be laid out on
/// each frame), with its totals and where the history starts at the foot of the card.
/// `loading`: the activity file is being read for the first time.
pub fn heat_map(heat: &Heat, loading: bool) -> AnyElement {
    // A square, then a gap of a quarter of one.
    const GAP: f32 = 0.25;
    let weeks = if loading { WEEKS } else { heat.levels.len().max(1) } as f32;
    let (wide, tall) = (weeks + (weeks - 1.) * GAP, 7. + 6. * GAP);
    if loading {
        // The map's own shape, so that the card does not change when the map comes.
        return padded()
            .child(motion::skeleton_block(
                "today-heat-wait",
                div()
                    .w_full()
                    .max_w(theme::sz(640.))
                    .aspect_ratio(wide / tall),
            ))
            .into_any_element();
    }
    if let Some(sentence) = heat.empty {
        return empty(icons::GRID, sentence);
    }
    let footer = div()
        .flex()
        .flex_none()
        .flex_wrap()
        .justify_between()
        .gap_x(theme::sz(10.))
        .text_size(theme::sz(12.))
        .text_color(theme::card_soft_text())
        .child(heat.totals.clone())
        .child(heat.since.clone());
    let levels = heat.levels.clone();
    let empty_cell = theme::tint();
    let mut ink = theme::link();
    let shades: [Hsla; 4] = [0.22, 0.45, 0.7, 1.].map(|alpha| {
        ink.a = alpha;
        ink
    });
    let squares = canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let cell = bounds.size.width / wide;
            let step = cell * (1. + GAP);
            for (w, week) in levels.iter().enumerate() {
                for (d, level) in week.iter().enumerate() {
                    let color = if *level == 0 {
                        empty_cell
                    } else {
                        shades[usize::from(*level - 1).min(3)]
                    };
                    let origin = point(
                        bounds.origin.x + step * w as f32,
                        bounds.origin.y + step * d as f32,
                    );
                    window.paint_quad(
                        fill(Bounds::new(origin, size(cell, cell)), color)
                            .corner_radii(cell * 0.18),
                    );
                }
            }
        },
    )
    .flex_none()
    .w_full()
    // A card a row wide (two columns, one, 130 %) would make each square large.
    .max_w(theme::sz(640.))
    .aspect_ratio(wide / tall);
    // The map at the top and the totals at the foot, whatever the height of the row.
    padded()
        .justify_between()
        .gap(theme::sz(10.))
        .child(squares)
        .child(footer)
        .into_any_element()
}

/// This month: two numbers, each with its change from the month before.
pub fn month_stats(month: &Month) -> AnyElement {
    if let Some(sentence) = month.empty {
        return empty(icons::CHART, sentence);
    }
    let stat = |label: &'static str, value: u32, change: Option<String>| {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(theme::sz(150.))
            .gap(theme::sz(1.))
            .child(
                div()
                    .text_size(theme::sz(12.))
                    .text_color(theme::card_soft_text())
                    .child(label),
            )
            .child(
                div()
                    .text_size(theme::sz(26.))
                    .line_height(theme::sz(30.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text())
                    .child(thousands(value)),
            )
            .when_some(change, |d, c| {
                d.child(
                    div()
                        .text_size(theme::sz(12.))
                        .text_color(theme::card_soft_text())
                        .child(c),
                )
            })
    };
    // One under the other in a narrow card, side by side in a wide one.
    padded()
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_x(theme::sz(PAD))
                .gap_y(theme::sz(12.))
                .child(stat(
                    "Sessions started",
                    month.sessions,
                    month.sessions_change.clone(),
                ))
                .child(stat(
                    "Pull requests merged",
                    month.merged,
                    month.merged_change.clone(),
                )),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use glasscode_core::github::{CheckRun, CiStatus};

    const ALL: [Card; 7] = [
        Card::Needs,
        Card::ComingUp,
        Card::Usage,
        Card::Activity,
        Card::Recent,
        Card::PullRequests,
        Card::Month,
    ];

    #[test]
    fn the_grid_has_three_two_or_one_columns_by_the_width_of_the_centre() {
        assert_eq!(columns(300.), 1);
        assert_eq!(columns(700.), 2);
        assert_eq!(columns(1100.), 3);
        // The edges, and the narrowest centre the shell keeps (320).
        assert_eq!(
            (
                columns(TWO_FROM - 1.),
                columns(TWO_FROM),
                columns(THREE_FROM - 1.),
                columns(THREE_FROM)
            ),
            (1, 2, 2, 3)
        );
        assert_eq!(columns(320.), 1);
    }

    #[test]
    fn the_cards_fill_their_rows() {
        // Three columns: rows of 5 4 3, 7 5 and 8 4, as in the mock.
        let three: Vec<u16> = ALL.iter().map(|c| span(*c, 3, false)).collect();
        assert_eq!(three, vec![5, 4, 3, 7, 5, 8, 4]);
        for row in [&three[0..3], &three[3..5], &three[5..7]] {
            assert_eq!(row.iter().sum::<u16>(), TRACKS);
        }
        // A heat map with nothing to show: Activity is the narrow card, Recent the wide one.
        let quiet: Vec<u16> = ALL.iter().map(|c| span(*c, 3, true)).collect();
        assert_eq!(quiet, vec![5, 4, 3, 4, 8, 8, 4]);
        // Two: Coming up and Usage share a row, and so do Pull requests and This month.
        for quiet in [false, true] {
            assert_eq!(
                ALL.iter().map(|c| span(*c, 2, quiet)).collect::<Vec<_>>(),
                vec![12, 6, 6, 12, 12, 6, 6]
            );
            assert!(ALL.iter().all(|c| span(*c, 1, quiet) == TRACKS));
        }
    }

    #[test]
    fn the_cards_of_a_row_are_as_high_as_each_other() {
        let full = Some(CARD_HEIGHT);
        let short = Some(CARD_HEIGHT_EMPTY);
        let cards = |empty: [bool; 7]| -> Vec<(Card, bool)> { ALL.into_iter().zip(empty).collect() };
        // Three columns, each card with something to show.
        assert_eq!(heights(&cards([false; 7]), 3, false), vec![full; 7]);
        // One card of a row with something: the row is of the full height. A row of sentences
        // is short.
        assert_eq!(
            heights(&cards([true, true, false, true, true, true, true]), 3, true),
            vec![full, full, full, short, short, short, short]
        );
        // Two columns: the heat map alone in its row is as high as it is wide asks for; with
        // nothing to show it is a short card.
        assert_eq!(
            heights(&cards([false; 7]), 2, false),
            vec![full, full, full, None, full, full, full]
        );
        assert_eq!(heights(&cards([true; 7]), 2, true), vec![short; 7]);
        // One column: each card is its own row.
        assert_eq!(
            heights(&cards([true, false, false, false, false, false, true]), 1, false),
            vec![short, full, full, None, full, full, short]
        );
    }

    #[test]
    fn a_long_list_keeps_its_first_rows_and_counts_them_all() {
        let lines: Vec<Line> = (0..LIST_MOST + 5)
            .map(|n| Line::new(Mark::None, format!("Row {n}"), "", Open::Nothing))
            .collect();
        let list = List::of(lines, RECENT_EMPTY);
        assert_eq!(list.lines.len(), LIST_MOST);
        assert_eq!(list.lines[LIST_MOST - 1].title, format!("Row {}", LIST_MOST - 1));
        assert_eq!(list.count, Some((LIST_MOST + 5).to_string()));
        assert_eq!(list.empty, None);
    }

    fn item(key: &str, title: &str, due: Option<Day>) -> Item {
        Item {
            key: key.into(),
            title: title.into(),
            due: due.map(day::iso),
            ..Default::default()
        }
    }

    #[test]
    fn coming_up_picks_the_next_seven_days_and_the_reviews() {
        let today = day::parse_iso("2026-11-17").unwrap();
        let mut review = item("review:acme/app#12", "Speed up the build", None);
        review.reference = Some("#12".into());
        review.outside = true;
        review.source = Some("GitHub · acme/app".into());
        review.person = Some("octocat".into());
        review.person_role = Some("author".into());
        review.url = Some("https://github.com/acme/app/pull/12".into());
        let mut done = item("task:d", "Done already", Some(today + 2));
        done.done = true;
        let mut snoozed = item("task:s", "Snoozed", Some(today + 3));
        snoozed.snoozed = Some("Tomorrow".into());
        let mut ticket = item("linear:ENG-7", "The import", Some(today + 1));
        ticket.reference = Some("ENG-7".into());
        ticket.outside = true;
        ticket.source = Some("Linear · In Progress".into());
        let items = vec![
            item("task:late", "Late", Some(today - 2)),
            item("task:today", "Today", Some(today)),
            review.clone(),
            item("task:week", "In a week", Some(today + 7)),
            item("task:far", "Too far", Some(today + 8)),
            ticket,
            item("task:soon", "Soon", Some(today + 3)),
            item("task:none", "No day", None),
            item("recurring:r", "Every Monday", Some(today + 6)),
            done,
            snoozed,
            // A focused task shows in Focus and again in its own section: once here.
            item("task:soon", "Soon", Some(today + 3)),
        ];
        let upcoming = coming_up(&items, today);
        let keys: Vec<&str> = upcoming.iter().map(|u| u.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "linear:ENG-7",
                "task:soon",
                "task:week",
                "review:acme/app#12"
            ]
        );
        assert_eq!(upcoming[0].when, "Tomorrow");
        assert_eq!(upcoming[0].title, "ENG-7 The import");
        assert_eq!(upcoming[1].when, day::relative(today + 3, today));
        assert_eq!(upcoming[1].detail, "Your list");
        assert_eq!(upcoming[2].when, day::short(today + 7));
        assert_eq!(
            upcoming[3],
            Upcoming {
                key: review.key,
                when: "Review".into(),
                title: "#12 Speed up the build".into(),
                detail: "GitHub · acme/app · by octocat".into(),
                url: review.url
            }
        );
    }

    #[test]
    fn each_card_has_a_sentence_for_no_data() {
        assert_eq!(needs(&[], 0).empty, Some(NEEDS_EMPTY));
        assert_eq!(needs(&[], 0).count, None);
        let coming = coming_up_list(&coming_up(&[], 0), |_| Open::Nothing);
        assert_eq!((coming.lines.len(), coming.empty), (0, Some(COMING_EMPTY)));
        assert_eq!(usage(None, 0).empty, Some(USAGE_NOT_READ));
        assert_eq!(usage(Some(&[]), 0).empty, Some(USAGE_EMPTY));
        let no_limits = [UsageAccount {
            agent: AgentKind::Claude,
            label: "Personal".into(),
            limits: Vec::new(),
        }];
        assert_eq!(
            usage(Some(&no_limits), 0),
            UsageCard {
                meters: Vec::new(),
                empty: Some(USAGE_EMPTY)
            }
        );
        let unread = heat(None, "2026-11-17");
        assert_eq!((unread.levels.len(), unread.empty), (0, Some(HEAT_EMPTY)));
        let fresh = heat(
            Some(&Activity {
                since: "2026-11-17".into(),
                ..Activity::default()
            }),
            "2026-11-17",
        );
        assert_eq!(
            (fresh.levels.len(), fresh.empty, fresh.since.as_str()),
            (WEEKS, Some(HEAT_EMPTY), "Since 17 Nov")
        );
        assert_eq!(recent(Vec::new(), 0).empty, Some(RECENT_EMPTY));
        assert_eq!(pull_requests(None).empty, Some(PRS_LOADING));
        assert_eq!(
            pull_requests(Some(&Err("gh: not logged in".into()))).empty,
            Some(PRS_NO_GH)
        );
        assert_eq!(pull_requests(Some(&Ok(Vec::new()))).empty, Some(PRS_EMPTY));
        assert_eq!(month(None, "2026-11-17").empty, Some(MONTH_EMPTY));
        assert_eq!(
            month(Some(&Activity::default()), "2026-11-17").empty,
            Some(MONTH_EMPTY)
        );
    }

    #[test]
    fn needs_you_leads_with_the_requests_and_has_a_row_for_each_need() {
        let need = |n: usize, failing: bool| Need {
            title: format!("Session {n}"),
            what: "Claude is waiting for your answer".into(),
            agent: Some(AgentKind::Claude),
            failing,
            open: Open::Session(n.to_string()),
        };
        let list = needs(
            &[
                need(1, false),
                need(2, true),
                need(3, false),
                need(4, false),
            ],
            2,
        );
        // Each of them is a row: the card scrolls.
        assert_eq!(list.lines.len(), 5);
        assert_eq!(
            (
                list.lines[0].title.as_str(),
                &list.lines[0].open,
                list.lines[0].action
            ),
            ("2 requests wait for your OK", &Open::Inbox, Some("Answer"))
        );
        assert_eq!(
            (&list.lines[1].mark, list.lines[1].action),
            (&Mark::Agent(AgentKind::Claude), Some("Answer"))
        );
        assert_eq!(
            (&list.lines[2].mark, list.lines[2].action),
            (&Mark::Icon(icons::PULL_REQUEST, Tone::Bad), Some("Open"))
        );
        assert_eq!((list.count.as_deref(), list.empty), (Some("5"), None));
        assert_eq!(needs(&[], 1).lines[0].title, "1 request waits for your OK");
    }

    #[test]
    fn a_meter_shows_the_fullest_window_of_each_account() {
        let limit = |window_minutes, used_percent| Limit {
            window_minutes,
            used_percent,
            resets_at: None,
        };
        let accounts = [
            UsageAccount {
                agent: AgentKind::Claude,
                label: "Personal".into(),
                limits: vec![limit(300, 84.4), limit(10080, 40.)],
            },
            UsageAccount {
                agent: AgentKind::Codex,
                label: "Work".into(),
                limits: vec![limit(300, 12.), limit(10080, 47.)],
            },
            UsageAccount {
                agent: AgentKind::Claude,
                label: "Spare".into(),
                limits: Vec::new(),
            },
        ];
        let card = usage(Some(&accounts), 0);
        assert_eq!(card.empty, None);
        assert_eq!(
            card.meters.len(),
            2,
            "an account without limits has no meter"
        );
        assert_eq!(
            (
                card.meters[0].label.as_str(),
                card.meters[0].percent.as_str(),
                card.meters[0].hot
            ),
            ("Personal · 5 h window", "84%", true)
        );
        assert_eq!(
            (
                card.meters[1].label.as_str(),
                card.meters[1].percent.as_str(),
                card.meters[1].hot
            ),
            ("Work · week", "47%", false)
        );
        assert!((card.meters[1].fraction - 0.47).abs() < 1e-6);
        // Past 100 the meter is full, not wider.
        let over = usage(
            Some(&[UsageAccount {
                agent: AgentKind::Codex,
                label: "Work".into(),
                limits: vec![limit(300, 130.)],
            }]),
            0,
        );
        assert_eq!(
            (over.meters[0].fraction, over.meters[0].percent.as_str()),
            (1., "100%")
        );
    }

    #[test]
    fn the_heat_map_has_four_shades_from_the_busiest_day() {
        let mut activity = Activity::default();
        for (date, sessions) in [
            ("2026-11-16", 8),
            ("2026-11-17", 1),
            ("2026-11-09", 4),
            ("2025-01-01", 30),
        ] {
            for _ in 0..sessions {
                activity.add_session(date);
            }
        }
        activity.add_merge("2026-11-16", 31_904, "acme/app#1");
        let heat = heat(Some(&activity), "2026-11-17");
        assert_eq!(heat.levels.len(), WEEKS);
        // This week: Monday the busiest (4), Tuesday 1 of 8 (1), the rest none.
        assert_eq!(heat.levels[WEEKS - 1], [4, 1, 0, 0, 0, 0, 0]);
        assert_eq!(heat.levels[WEEKS - 2][0], 2);
        // A day older than the map counts neither in the shades nor in the totals.
        assert_eq!(
            heat.totals,
            "13 sessions · 1 pull request merged · 31,904 lines"
        );
        assert_eq!(heat.since, "Since 1 Jan 2025");
        assert_eq!(heat.empty, None);
        assert_eq!(
            (
                level(0, 8),
                level(1, 8),
                level(2, 8),
                level(3, 8),
                level(8, 8)
            ),
            (0, 1, 1, 2, 4)
        );
    }

    #[test]
    fn recent_sessions_are_newest_first_without_those_that_wait_for_you() {
        let session = |n: u64, state| Recent {
            id: format!("s{n}"),
            title: format!("Session {n}"),
            project: "app".into(),
            agent: AgentKind::Codex,
            state,
            created_at: 1_000_000 + n * 60,
        };
        let mut sessions: Vec<Recent> = (0..8).map(|n| session(n, RowState::Idle)).collect();
        sessions[7].state = RowState::NeedsYou;
        sessions[6].state = RowState::Working;
        let list = recent(sessions, 1_000_000 + 7 * 60);
        let ids: Vec<&Open> = list.lines.iter().map(|l| &l.open).collect();
        assert_eq!(
            ids,
            vec![
                &Open::Session("s6".into()),
                &Open::Session("s5".into()),
                &Open::Session("s4".into()),
                &Open::Session("s3".into()),
                &Open::Session("s2".into()),
                &Open::Session("s1".into()),
                &Open::Session("s0".into())
            ]
        );
        assert_eq!(
            (list.lines[0].state, list.lines[0].detail.as_str()),
            (Some(RowState::Working), "app · started 1 min ago")
        );
        assert_eq!(list.count.as_deref(), Some("7"));
        assert_eq!(ago(0, 59), "started just now");
        assert_eq!(ago(0, 3 * 3_600), "started 3 h ago");
        assert_eq!(ago(0, 30 * 3_600), "started yesterday");
        assert_eq!(ago(0, 5 * 86_400), "started 5 days ago");
    }

    #[test]
    fn pull_requests_show_their_checks_and_say_how_many_are_open() {
        let check = |conclusion: &str| CheckRun {
            name: "test".into(),
            status: "COMPLETED".into(),
            conclusion: conclusion.into(),
            ..Default::default()
        };
        let pr = |n: u64, ci: Option<CiStatus>| MyPr {
            repo: "acme/app".into(),
            number: n,
            title: format!("Change {n}"),
            url: format!("https://github.com/acme/app/pull/{n}"),
            ci,
            ..Default::default()
        };
        let failing = CiStatus {
            checks: vec![check("SUCCESS"), check("FAILURE")],
            ..Default::default()
        };
        let passed = CiStatus {
            checks: vec![check("SUCCESS")],
            ..Default::default()
        };
        let mut draft = pr(3, None);
        draft.draft = true;
        draft.review = "REVIEW_REQUIRED".into();
        let list = pull_requests(Some(&Ok(vec![
            pr(1, Some(failing)),
            pr(2, Some(passed)),
            draft,
            pr(4, None),
            pr(5, None),
        ])));
        assert_eq!(list.lines.len(), 5);
        assert_eq!(
            (list.count.as_deref(), list.empty),
            (Some("5 open"), None)
        );
        assert_eq!(
            (list.lines[0].detail.as_str(), &list.lines[0].mark),
            (
                "#1 · app · 1 check failing",
                &Mark::Icon(icons::PULL_REQUEST, Tone::Bad)
            )
        );
        assert_eq!(
            (list.lines[1].detail.as_str(), &list.lines[1].mark),
            (
                "#2 · app · checks passed",
                &Mark::Icon(icons::PULL_REQUEST, Tone::Ok)
            )
        );
        assert_eq!(list.lines[2].detail, "#3 · app · draft · review required");
        assert_eq!(
            list.lines[0].open,
            Open::Url("https://github.com/acme/app/pull/1".into())
        );
    }

    #[test]
    fn this_month_compares_with_the_month_before_only_when_it_was_counted() {
        let mut activity = Activity::default();
        for _ in 0..3 {
            activity.add_session("2026-10-20");
        }
        for _ in 0..5 {
            activity.add_session("2026-11-02");
        }
        activity.add_merge("2026-10-21", 10, "acme/app#1");
        let m = month(Some(&activity), "2026-11-17");
        assert_eq!((m.sessions, m.merged, m.empty), (5, 0, None));
        assert_eq!(m.sessions_change.as_deref(), Some("2 more than in October"));
        assert_eq!(m.merged_change.as_deref(), Some("1 fewer than in October"));
        // January compares with December.
        assert_eq!(
            month(Some(&activity), "2027-01-05")
                .sessions_change
                .as_deref(),
            Some("as many as in December")
        );
        // A history that began this month has nothing to compare with.
        let mut new = Activity::default();
        new.add_session("2026-11-16");
        let m = month(Some(&new), "2026-11-17");
        assert_eq!((m.sessions, m.sessions_change), (1, None));
    }

    #[test]
    fn numbers_get_separators() {
        assert_eq!(
            (
                thousands(0),
                thousands(999),
                thousands(1_000),
                thousands(31_904),
                thousands(1_234_567)
            ),
            (
                "0".into(),
                "999".into(),
                "1,000".into(),
                "31,904".into(),
                "1,234,567".into()
            )
        );
    }
}
