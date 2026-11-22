//! A place as the lens and the phone show it (`docs/lens-everywhere.md`): the Mac builds it for
//! the place its window shows, and every device draws the same page. Short on purpose: the lens
//! is 600 by 600 and glanced at.

use serde::{Deserialize, Serialize};

/// How many rows a page carries; the rest are counted (`total`).
pub const ITEMS_MAX: usize = 30;
const TITLE_MAX: usize = 80;
const DETAIL_MAX: usize = 90;
const BODY_MAX: usize = 420;

/// The colour a status or a row is drawn in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Ok,
    /// At work: a session working, checks running.
    Busy,
    /// Needs the person.
    Warn,
    /// Failing.
    Bad,
    #[default]
    Plain,
}

impl Tone {
    /// A session's state word (`session_state`) as a tone.
    pub fn of_state(state: &str) -> Tone {
        match state {
            "needs you" => Tone::Warn,
            "working" | "goes on by itself" => Tone::Busy,
            "idle" | "finished" => Tone::Ok,
            "not running" | "ended" => Tone::Plain,
            _ => Tone::Bad,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub label: String,
    pub tone: Tone,
}

/// How much of something is used (0 to 1), and what that is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Meter {
    pub value: f32,
    pub label: String,
}

/// A row of a page.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    #[serde(default)]
    pub tone: Tone,
    /// The place it opens (`Page::place` of that page), when it opens one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Page {
    /// session, today, tickets, ticket, prs, pr, usage, sessions, settings.
    pub kind: String,
    /// The word that opens it again: a session's id, "today", "board", "ticket:ENG-42",
    /// "pull-requests", "pr:958", "usage".
    pub place: String,
    /// What it is, over the title ("Session · glasscode").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub eyebrow: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meter: Option<Meter>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<Item>,
    /// How many rows there are in all (`items` holds the first `ITEMS_MAX`).
    #[serde(default)]
    pub total: usize,
    /// The places before and after it in its list (a ticket's neighbours on the board).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// A text on one line and not longer than `max` characters.
pub fn clip(text: &str, max: usize) -> String {
    let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(max.saturating_sub(1)) {
        Some((end, _)) if line.chars().count() > max => format!("{}…", line[..end].trim_end()),
        _ => line,
    }
}

impl Page {
    pub fn new(kind: &str, place: &str, title: &str) -> Page {
        Page { kind: kind.into(), place: place.into(), title: clip(title, TITLE_MAX), ..Page::default() }
    }

    pub fn eyebrow(mut self, eyebrow: impl Into<String>) -> Page {
        self.eyebrow = clip(&eyebrow.into(), 48);
        self
    }

    pub fn status(mut self, label: impl Into<String>, tone: Tone) -> Page {
        self.status = Some(Status { label: clip(&label.into(), 40), tone });
        self
    }

    pub fn body(mut self, body: &str) -> Page {
        self.body = clip(body, BODY_MAX);
        self
    }

    pub fn meter(mut self, value: f32, label: impl Into<String>) -> Page {
        self.meter = Some(Meter { value: value.clamp(0.0, 1.0), label: clip(&label.into(), 48) });
        self
    }

    /// Add a row (only the first `ITEMS_MAX` are kept; all are counted).
    pub fn item(&mut self, title: &str, detail: &str, tone: Tone, open: Option<String>) {
        self.total += 1;
        if self.items.len() < ITEMS_MAX {
            self.items.push(Item { title: clip(title, TITLE_MAX), detail: clip(detail, DETAIL_MAX), tone, open });
        }
    }

    /// Its place among `places` (a list's order): the ones before and after it.
    pub fn beside(mut self, places: &[String]) -> Page {
        if let Some(at) = places.iter().position(|p| *p == self.place) {
            self.prev = at.checked_sub(1).map(|i| places[i].clone());
            self.next = places.get(at + 1).cloned();
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_keeps_what_fits_and_counts_the_rest() {
        let mut page = Page::new("tickets", "board", "Your tickets").eyebrow("Board").status("12 open", Tone::Plain);
        for i in 0..ITEMS_MAX + 5 {
            page.item(&format!("ENG-{i} a ticket"), "In progress", Tone::Busy, Some(format!("ticket:ENG-{i}")));
        }
        assert_eq!(page.items.len(), ITEMS_MAX);
        assert_eq!(page.total, ITEMS_MAX + 5);
        let json = serde_json::to_value(&page).unwrap();
        assert_eq!(json["items"][0]["open"], "ticket:ENG-0");
        assert_eq!(json["status"]["tone"], "plain");
        assert!(json.get("meter").is_none() && json.get("prev").is_none());
        let back: Page = serde_json::from_value(json).unwrap();
        assert_eq!(back, page);
    }

    #[test]
    fn long_text_is_cut_on_one_line() {
        assert_eq!(clip("one\ntwo   three", 40), "one two three");
        assert_eq!(clip("abcdefghij", 5), "abcd…");
        assert_eq!(clip("abcde", 5), "abcde");
        let page = Page::new("session", "s1", &"x".repeat(200));
        assert_eq!(page.title.chars().count(), TITLE_MAX);
    }

    #[test]
    fn neighbours_in_a_list() {
        let places: Vec<String> = ["ticket:A-1", "ticket:A-2", "ticket:A-3"].map(String::from).to_vec();
        let middle = Page::new("ticket", "ticket:A-2", "Two").beside(&places);
        assert_eq!((middle.prev.as_deref(), middle.next.as_deref()), (Some("ticket:A-1"), Some("ticket:A-3")));
        let first = Page::new("ticket", "ticket:A-1", "One").beside(&places);
        assert_eq!((first.prev, first.next.as_deref()), (None, Some("ticket:A-2")));
        let alone = Page::new("ticket", "ticket:B-9", "Nine").beside(&places);
        assert_eq!((alone.prev, alone.next), (None, None));
    }

    #[test]
    fn states_have_tones() {
        assert_eq!(Tone::of_state("needs you"), Tone::Warn);
        assert_eq!(Tone::of_state("working"), Tone::Busy);
        assert_eq!(Tone::of_state("idle"), Tone::Ok);
        assert_eq!(Tone::of_state("not running"), Tone::Plain);
        assert_eq!(Tone::of_state("finished"), Tone::Ok);
        assert_eq!(Tone::of_state("goes on by itself"), Tone::Busy);
        assert_eq!(Tone::of_state("ended"), Tone::Plain);
        assert_eq!(Tone::of_state("crashed"), Tone::Bad);
        assert!((Page::new("usage", "usage", "Usage").meter(1.7, "x").meter.unwrap().value - 1.0).abs() < f32::EPSILON);
    }
}
