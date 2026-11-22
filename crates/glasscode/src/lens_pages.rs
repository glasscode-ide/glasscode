//! One place everywhere (`docs/lens-everywhere.md`): the place the Mac's window shows, the lens
//! page of each place (`glasscode_core::lens::Page`), and opening a place on the Mac for the
//! phone, the lens or the voice. The door sends the page of the window's place to each phone
//! whenever it changes, so the lens always shows what the Mac shows.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::{AppContext, Context, Window};
use glasscode_core::github::CheckState;
use glasscode_core::lens::{Page, Tone, clip};
use glasscode_core::model::Session;

use crate::center::Center;
use crate::shell::View;
use crate::tickets_ui::Key;
use crate::workspace::Workspace;

/// How long a session's last reply, read from its transcript, is taken as current.
const REPLY_FRESH: Duration = Duration::from_secs(4);

/// What the pages need that is read off the main thread: sessions' last replies.
#[derive(Default)]
pub struct LensState {
    replies: HashMap<String, (Instant, String)>,
    reading: HashSet<String>,
    /// A page shown on the lens that the window has no view of its own for (the list of
    /// sessions: the window shows one session), with the window's place when it was opened. It
    /// is the place until the window goes somewhere else.
    over: Option<(String, String)>,
}

/// A percentage used of a limit, as its tone.
fn used_tone(percent: f64) -> Tone {
    match percent {
        p if p >= 90.0 => Tone::Bad,
        p if p >= 75.0 => Tone::Warn,
        _ => Tone::Ok,
    }
}

/// A limit's window in a few words.
fn window_words(minutes: u64) -> String {
    match minutes {
        300 => "5 hours".into(),
        10080 => "the week".into(),
        m if m >= 1440 && m % 1440 == 0 => format!("{} days", m / 1440),
        m if m >= 60 => format!("{} hours", m / 60),
        m => format!("{m} minutes"),
    }
}

/// A state word with a capital, as a status shows it.
fn label(state: &str) -> String {
    match state {
        "not running" => "Not started".into(),
        "needs you" => "Needs you".into(),
        other => {
            let mut c = other.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }
    }
}

/// How a pull request's checks stand: words and tone.
fn checks_of(ci: Option<&glasscode_core::github::CiStatus>) -> (String, Tone) {
    let Some(ci) = ci else { return ("no checks yet".into(), Tone::Plain) };
    let failed = ci.failed().len();
    if failed > 0 {
        (format!("{failed} check{} failing", if failed == 1 { "" } else { "s" }), Tone::Bad)
    } else if ci.running() {
        ("checks running".into(), Tone::Busy)
    } else if ci.conflicting() {
        ("conflicts with its base".into(), Tone::Warn)
    } else {
        ("checks pass".into(), Tone::Ok)
    }
}

impl Workspace {
    /// The place every screen shows, as `place.open` takes it: the window's, or the page shown
    /// over it (`LensState::over`) while the window stays where it was.
    pub(crate) fn lens_place(&self) -> String {
        let window = self.window_place();
        match &self.lens.over {
            Some((page, at)) if *at == window => page.clone(),
            _ => window,
        }
    }

    /// The place the window itself shows.
    fn window_place(&self) -> String {
        match self.view {
            View::Sessions => match self.state.active_session.as_deref().filter(|_| self.center == Center::Session).and_then(|id| self.state.session(id)) {
                Some(s) if !self.state.session_hidden(s) => s.id.clone(),
                _ => "sessions".into(),
            },
            View::Board => match &self.tickets.selected {
                Some(Key::Ticket(id)) => format!("ticket:{id}"),
                Some(Key::Pr(_, n)) => format!("pr:{n}"),
                None => "tickets".into(),
            },
            View::PullRequests => "pull-requests".into(),
            View::Usage => "usage".into(),
            View::Settings => "settings".into(),
            View::Today | View::FirstRun => "today".into(),
        }
    }

    /// The place's page, from what the app holds now.
    pub(crate) fn lens_page(&self, place: &str, cx: &gpui::App) -> Page {
        let place = place.trim();
        if let Some(id) = place.strip_prefix("ticket:") {
            return self.lens_ticket(id);
        }
        if let Some(n) = place.strip_prefix("pr:").and_then(|n| n.trim_start_matches('#').parse::<u64>().ok()) {
            return self.lens_pr(n);
        }
        match place {
            "today" => self.lens_today(),
            "tickets" | "board" => self.lens_tickets(),
            "pull-requests" => self.lens_prs(),
            "usage" => self.lens_usage(),
            "settings" => Page::new("settings", "settings", "Settings").eyebrow("Glasscode").body("Settings are changed on the Mac."),
            "sessions" | "" => self.lens_sessions(),
            id => match self.state.session(id) {
                Some(s) => self.lens_session(s, cx),
                None => Page::new("sessions", id, "Not found").eyebrow("Glasscode").body("That place is not on this Mac now."),
            },
        }
    }

    /// Whether a ticket or a pull request a place names is one this Mac knows (the voice opens
    /// only those; a page is always known).
    pub(crate) fn lens_knows(&self, place: &str) -> bool {
        if let Some(id) = place.strip_prefix("ticket:") {
            return self.tickets.snapshot.as_ref().is_some_and(|s| s.ticket(id).is_some());
        }
        if let Some(n) = place.strip_prefix("pr:").and_then(|n| n.parse::<u64>().ok()) {
            return self.tickets.reviews.iter().any(|r| r.hit.number == n) || self.state.visible_sessions().any(|s| self.prs.map.get(&s.id).and_then(|st| st.pr.as_ref()).is_some_and(|pr| pr.number == n));
        }
        true
    }

    /// The window's place in a sentence, for the voice: where the person is.
    pub(crate) fn lens_now(&self, cx: &gpui::App) -> String {
        let page = self.lens_page(&self.lens_place(), cx);
        let status = page.status.as_ref().map(|s| format!(" ({})", s.label.to_lowercase())).unwrap_or_default();
        let what = if page.eyebrow.is_empty() { page.title.clone() } else { format!("{}: \"{}\"", page.eyebrow, page.title) };
        // No id: the voice names a session by its title (an id read aloud is noise).
        format!("On the Mac's screen now: {what}{status}.")
    }

    fn lens_project(&self, s: &Session) -> String {
        self.state.project(&s.project_id).map(|p| p.name.clone()).unwrap_or_default()
    }

    fn lens_session(&self, s: &Session, cx: &gpui::App) -> Page {
        let state = self.session_state(s);
        let some = |t: &&str| !t.trim().is_empty();
        let reply = self.lens.replies.get(&s.id).map(|(_, text)| text.as_str()).filter(some).or(self.round.replies.get(&s.id).map(String::as_str).filter(some)).or(s.initial_prompt.as_deref()).unwrap_or_default();
        let mut page = Page::new("session", &s.id, &self.session_title(s)).eyebrow(format!("Session · {}", self.lens_project(s))).status(label(state), Tone::of_state(state)).body(reply);
        for a in self.control.pending_for(&s.id) {
            page.item("Needs your OK", &a.what, Tone::Warn, Some(format!("approval:{}", a.id)));
        }
        if let Some(prompt) = self.terminals.get(&s.id).and_then(|t| glasscode_core::prompt::parse(&t.read(cx).text_tail(60))) {
            page.item(&format!("Asks: {}", prompt.question), prompt.title.as_deref().unwrap_or_default(), Tone::Warn, Some(format!("question:{}", s.id)));
        }
        if let Some(status) = self.prs.map.get(&s.id)
            && let Some(pr) = status.pr.as_ref()
        {
            let (checks, tone) = checks_of(status.ci.as_ref());
            page.item(&format!("Pull request #{}", pr.number), &format!("{checks} · {}", pr.title), tone, Some(format!("pr:{}", pr.number)));
        }
        page
    }

    fn lens_sessions(&self) -> Page {
        let (sessions, leftovers) = self.sidebar_places();
        let mut page = Page::new("sessions", "sessions", "Sessions").eyebrow("Glasscode").status(format!("{} open", sessions.len()), Tone::Plain);
        for s in &sessions {
            let state = self.session_state(s);
            page.item(&self.session_title(s), &format!("{} · {}", self.lens_project(s), label(state)), Tone::of_state(state), Some(s.id.clone()));
        }
        if !leftovers.is_empty() {
            page.body = format!("{} worktrees have no session: closed, not sessions.", leftovers.len());
        }
        page
    }

    fn lens_today(&self) -> Page {
        let (mac_id, mac) = self.door_names();
        let snapshot = self.today_snapshot(&mac_id, &mac);
        let need = snapshot.needs.len() + snapshot.approvals.len();
        let mut page = Page::new("today", "today", "Today").eyebrow(format!("Today · {mac}")).status(if need == 0 { "Nothing needs you".to_string() } else { format!("{need} need you") }, if need == 0 { Tone::Ok } else { Tone::Warn }).body(&snapshot.summary);
        for a in &snapshot.approvals {
            page.item("Needs your OK", &a.what, Tone::Warn, Some(format!("approval:{}", a.id)));
        }
        for n in &snapshot.needs {
            page.item(&n.title, &n.what, Tone::Warn, n.session.clone().filter(|s| !s.starts_with("server:")));
        }
        page
    }

    /// The open tickets in the board's order, then the pull requests that ask for a review.
    fn lens_ticket_places(&self) -> Vec<String> {
        let mut places: Vec<String> = self.tickets.snapshot.as_ref().map(|s| s.mine.iter().filter(|t| !t.is_done()).map(|t| format!("ticket:{}", t.identifier)).collect()).unwrap_or_default();
        places.extend(self.tickets.reviews.iter().map(|r| format!("pr:{}", r.hit.number)));
        places
    }

    fn lens_tickets(&self) -> Page {
        let Some(snapshot) = self.tickets.snapshot.as_ref() else {
            let why = if self.tickets.connected { "Linear has not answered yet." } else { "Linear is not connected: Settings › Integrations › Linear on the Mac." };
            return Page::new("tickets", "tickets", "Tickets").eyebrow("Board").body(why);
        };
        let open: Vec<&glasscode_core::linear::Ticket> = snapshot.mine.iter().filter(|t| !t.is_done()).collect();
        let reviews = self.tickets.reviews.len();
        let mut page = Page::new("tickets", "tickets", "Your tickets").eyebrow("Board").status(format!("{} open · {reviews} to review", open.len()), Tone::Plain);
        for t in &open {
            let due = t.due.as_deref().map(|d| format!(" · due {}", glasscode_core::linear::short_date(d))).unwrap_or_default();
            let tone = match (t.priority, t.status.kind.as_str()) {
                (1, _) => Tone::Warn,
                (_, "started") => Tone::Busy,
                _ => Tone::Plain,
            };
            page.item(&t.title, &format!("{} · {}{due}", t.identifier, t.status.name), tone, Some(format!("ticket:{}", t.identifier)));
        }
        for r in &self.tickets.reviews {
            page.item(&r.hit.title, &format!("Review #{} · {} · by {}", r.hit.number, r.repo, r.hit.author), Tone::Warn, Some(format!("pr:{}", r.hit.number)));
        }
        page
    }

    fn lens_ticket(&self, id: &str) -> Page {
        let Some(t) = self.tickets.snapshot.as_ref().and_then(|s| s.ticket(id)) else {
            return Page::new("ticket", &format!("ticket:{id}"), id).eyebrow("Ticket").body("This ticket is not on the board now.");
        };
        let tone = if t.is_done() { Tone::Ok } else if t.status.kind == "started" { Tone::Busy } else { Tone::Plain };
        let description = glasscode_core::linear::markdown(&t.description);
        let mut page = Page::new("ticket", &format!("ticket:{}", t.identifier), &t.title)
            .eyebrow(format!("Ticket {}{}", t.identifier, if t.team.is_empty() { String::new() } else { format!(" · {}", t.team) }))
            .status(t.status.name.clone(), tone)
            .body(if description.trim().is_empty() { "No description." } else { description.as_str() });
        page.item(&format!("Priority {}", t.priority_name()), &t.due.as_deref().map(|d| format!("due {}", glasscode_core::linear::short_date(d))).unwrap_or_default(), if t.priority == 1 { Tone::Warn } else { Tone::Plain }, None);
        for s in self.state.visible_sessions().filter(|s| self.state.ticket_of.get(&s.id).is_some_and(|tk| tk.eq_ignore_ascii_case(&t.identifier))) {
            let state = self.session_state(s);
            page.item(&self.session_title(s), &format!("Its session · {}", label(state)), Tone::of_state(state), Some(s.id.clone()));
        }
        page.beside(&self.lens_ticket_places())
    }

    /// The sessions' pull requests (a session's own), then the ones asking for a review.
    fn lens_prs(&self) -> Page {
        let mut page = Page::new("prs", "pull-requests", "Pull requests").eyebrow("GitHub");
        for s in self.state.visible_sessions() {
            let Some(status) = self.prs.map.get(&s.id) else { continue };
            let Some(pr) = status.pr.as_ref() else { continue };
            let (checks, tone) = checks_of(status.ci.as_ref());
            page.item(&pr.title, &format!("#{} · {checks}", pr.number), tone, Some(format!("pr:{}", pr.number)));
        }
        let yours = page.total;
        for r in &self.tickets.reviews {
            page.item(&r.hit.title, &format!("Review #{} · by {}", r.hit.number, r.hit.author), Tone::Warn, Some(format!("pr:{}", r.hit.number)));
        }
        let reviews = page.total - yours;
        page.status(format!("{yours} yours · {reviews} to review"), if reviews > 0 { Tone::Warn } else { Tone::Plain })
    }

    fn lens_pr(&self, number: u64) -> Page {
        let place = format!("pr:{number}");
        let own = self.state.visible_sessions().find_map(|s| self.prs.map.get(&s.id).filter(|st| st.pr.as_ref().is_some_and(|pr| pr.number == number)).map(|st| (s, st)));
        if let Some((s, status)) = own {
            let pr = status.pr.as_ref().expect("found by its number");
            let (checks, tone) = checks_of(status.ci.as_ref());
            let mut page = Page::new("pr", &place, &pr.title).eyebrow(format!("Pull request #{number}{}", status.repo.as_deref().map(|r| format!(" · {r}")).unwrap_or_default())).status(checks, tone);
            for c in status.ci.as_ref().map(|ci| ci.checks.as_slice()).unwrap_or_default() {
                let tone = match c.state() {
                    CheckState::Failed => Tone::Bad,
                    CheckState::Running => Tone::Busy,
                    CheckState::Passed => Tone::Ok,
                    CheckState::Skipped => Tone::Plain,
                };
                page.item(&c.name, &c.workflow, tone, None);
            }
            let state = self.session_state(s);
            page.item(&self.session_title(s), &format!("Its session · {}", label(state)), Tone::of_state(state), Some(s.id.clone()));
            return page.beside(&self.lens_ticket_places());
        }
        if let Some(r) = self.tickets.reviews.iter().find(|r| r.hit.number == number) {
            let merge = match r.merge_state.as_str() {
                "DIRTY" => ("conflicts", Tone::Bad),
                "BEHIND" => ("behind its base", Tone::Warn),
                "CLEAN" => ("ready to merge", Tone::Ok),
                _ => ("asks for your review", Tone::Warn),
            };
            return Page::new("pr", &place, &r.hit.title).eyebrow(format!("Review #{number} · {}", r.repo)).status(merge.0, merge.1).body(&format!("{} asks you to review it.", r.hit.author)).beside(&self.lens_ticket_places());
        }
        Page::new("pr", &place, &format!("Pull request #{number}")).eyebrow("GitHub").body("Not on this Mac's list now.")
    }

    fn lens_usage(&self) -> Page {
        let report = self.usage.report.as_ref();
        let mut page = Page::new("usage", "usage", "Usage").eyebrow("Claude and Codex");
        let mut most: Option<(f64, String)> = None;
        for a in self.state.accounts.iter().filter(|a| a.agent != glasscode_core::model::AgentKind::Shell) {
            let found = report.and_then(|r| r.accounts.iter().find(|x| x.id == a.id));
            let limits = found.map(|f| f.limits.as_slice()).unwrap_or_default();
            let words: Vec<String> = limits.iter().map(|l| format!("{:.0}% of {}", l.used_percent, window_words(l.window_minutes))).collect();
            let top = limits.iter().map(|l| l.used_percent).fold(0.0f64, f64::max);
            let who = format!("{} · {}", a.agent.name(), a.label);
            if most.as_ref().is_none_or(|(p, _)| top > *p) && !limits.is_empty() {
                let window = limits.iter().max_by(|x, y| x.used_percent.total_cmp(&y.used_percent)).map(|l| window_words(l.window_minutes)).unwrap_or_default();
                most = Some((top, format!("{who}: {top:.0}% of {window}")));
            }
            page.item(&who, &if words.is_empty() { "not read yet".to_string() } else { words.join(" · ") }, if limits.is_empty() { Tone::Plain } else { used_tone(top) }, None);
        }
        match most {
            Some((percent, words)) => page.meter((percent / 100.0) as f32, words).status(format!("{percent:.0}% at most"), used_tone(percent)),
            None => page.body(if report.is_some() { "No limits read yet." } else { "Usage is not read yet." }),
        }
    }

    /// Keep the last reply of the session on screen fresh for its page, read off the main thread.
    pub(crate) fn lens_refresh_reply(&mut self, cx: &mut Context<Self>) {
        // The window went somewhere else: the page shown over it is over.
        if self.lens.over.as_ref().is_some_and(|(_, at)| *at != self.window_place()) {
            self.lens.over = None;
        }
        let place = self.lens_place();
        let Some(s) = self.state.session(&place) else { return };
        if self.lens.reading.contains(&s.id) || self.lens.replies.get(&s.id).is_some_and(|(at, _)| at.elapsed() < REPLY_FRESH) {
            return;
        }
        let (id, agent, Some(path)) = (s.id.clone(), s.agent, s.transcript_path.clone()) else { return };
        self.lens.reading.insert(id.clone());
        let task = cx.background_spawn(async move { glasscode_core::transcript::last_reply(agent, &path) });
        cx.spawn(async move |this, cx| {
            let reply = task.await;
            let _ = this.update(cx, |ws, _| {
                ws.lens.reading.remove(&id);
                ws.lens.replies.insert(id, (Instant::now(), reply.map(|r| clip(&r, 600)).unwrap_or_default()));
            });
        })
        .detach();
    }

    /// Open a place on the Mac, for the phone, the lens or the voice: the window goes there, and
    /// the door sends its page to every phone. The page is returned too.
    pub(crate) fn lens_open(&mut self, place: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<Page, String> {
        let place = place.trim();
        if let Some(id) = place.strip_prefix("ticket:") {
            self.tickets.selected = Some(Key::Ticket(id.to_string()));
            self.show_tickets(cx);
        } else if let Some(n) = place.strip_prefix("pr:").and_then(|n| n.trim_start_matches('#').parse::<u64>().ok()) {
            let own = self.state.visible_sessions().find(|s| self.prs.map.get(&s.id).and_then(|st| st.pr.as_ref()).is_some_and(|pr| pr.number == n)).map(|s| s.id.clone());
            match own {
                Some(id) => self.run_op_named("ui.open", serde_json::json!({ "place": id }), window, cx),
                None => match self.tickets.reviews.iter().find(|r| r.hit.number == n) {
                    Some(r) => {
                        self.tickets.selected = Some(Key::Pr(r.repo.clone(), n));
                        self.show_tickets(cx);
                    }
                    None => self.run_op_named("ui.open", serde_json::json!({ "place": "pull-requests" }), window, cx),
                },
            }
        } else {
            let word = match place {
                "board" => "tickets".to_string(),
                "today" | "tickets" | "pull-requests" | "usage" | "sessions" | "settings" => place.to_string(),
                said => self.voice_session_for(said)?,
            };
            if word == "tickets" {
                self.tickets.selected = None;
            }
            self.run_op_named("ui.open", serde_json::json!({ "place": word }), window, cx);
            // The list of sessions has no view of its own in the window (it shows one session):
            // it is the lens's page until the window goes somewhere else.
            self.lens.over = (word == "sessions").then(|| ("sessions".to_string(), self.window_place()));
        }
        if place != "sessions" {
            self.lens.over = None;
        }
        cx.notify();
        Ok(self.lens_page(&self.lens_place(), cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_for_states_limits_and_checks() {
        assert_eq!(label("not running"), "Not started");
        assert_eq!(label("needs you"), "Needs you");
        assert_eq!(label("working"), "Working");
        assert_eq!(window_words(300), "5 hours");
        assert_eq!(window_words(10080), "the week");
        assert_eq!(window_words(2880), "2 days");
        assert_eq!(used_tone(95.0), Tone::Bad);
        assert_eq!(used_tone(80.0), Tone::Warn);
        assert_eq!(used_tone(10.0), Tone::Ok);
        assert_eq!(checks_of(None), ("no checks yet".to_string(), Tone::Plain));
        let check = |conclusion: &str, status: &str| glasscode_core::github::CheckRun { name: "test".into(), status: status.into(), conclusion: conclusion.into(), ..Default::default() };
        let ci = |checks| glasscode_core::github::CiStatus { checks, mergeable: "MERGEABLE".into(), ..Default::default() };
        assert_eq!(checks_of(Some(&ci(vec![check("SUCCESS", "COMPLETED"), check("FAILURE", "COMPLETED")]))).1, Tone::Bad);
        assert_eq!(checks_of(Some(&ci(vec![check("SUCCESS", "COMPLETED")]))), ("checks pass".to_string(), Tone::Ok));
    }
}
