//! Typing into a session's terminal, one delivery at a time (M31; review ops-19, ops-m2, land-m3,
//! alt-4).
//!
//! In a v2 home every message Glasscode types into a session goes through that session's queue:
//! `session.send` from any caller, the CI fix, the manager's messages. The queue is the one writer
//! of the session's terminal (ops-m2). It is pure: the app asks it what to do next and tells it
//! what happened.
//!
//! - **Preconditions** ([`ready`]): the session runs, its turn is over (idle), no prompt or
//!   question is on its screen, nothing is typed in its message box, and nobody typed in it for
//!   [`QUIET_TYPING_MS`]. Until then a delivery waits.
//! - **Expiry and supersede** ([`Queues::push`], [`Queues::expire`]): a delivery has an expiry,
//!   and a group with a version (the CI fix of `acme/app#42` at a head SHA): a new version cancels
//!   the older ones of its group, and the same version again is the same delivery (two playbooks
//!   reacting to one event type one message).
//! - **One at a time**: while one is in flight, the session's next waits.
//! - **Never on a permission prompt** (land-m3): right before the Enter the app checks again that
//!   no prompt is showing and that the box holds exactly the typed text ([`may_press_enter`]).
//! - **A receipt** (ops-19): each delivery carries a short marker in its last line. The session's
//!   `UserPromptSubmit` hook event with that marker is the receipt ([`probe`]); a claim cut off by a
//!   crash is settled by the same check (`effects::settle`).

use std::collections::{BTreeMap, VecDeque};

use crate::effects::{EffectKey, ProbeResult};
use crate::words::InjectKind;

/// How long after the person last typed in a session nothing is typed into it.
pub const QUIET_TYPING_MS: u64 = 10_000;

/// How long a delivery waits for its session at most, unless it says otherwise.
pub const EXPIRY_MS: u64 = 24 * 3_600_000;

/// How long the app waits for the receipt after the Enter.
pub const RECEIPT_WAIT_MS: u64 = 15_000;

/// How many times a delivery that was not sent is put back in its queue.
pub const MAX_TRIES: u32 = 3;

/// One message to type into a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    /// 12 hex digits; its first 8 are the marker.
    pub id: String,
    pub session: String,
    /// The message itself.
    pub text: String,
    /// "(Sent by Glasscode because … asked it to.)": the note under the message, when it has one.
    pub note: Option<String>,
    /// What it is in the words ledger.
    pub kind: InjectKind,
    /// Its claim in the effect ledger, which the delivery's outcome finishes.
    pub effect: Option<EffectKey>,
    /// What supersedes it: a later delivery of the same group with another version.
    pub group: Option<String>,
    pub version: Option<String>,
    pub queued_ms: u64,
    pub expires_ms: u64,
    /// Goes before the others (an interrupt).
    pub urgent: bool,
    /// How many times it was not sent (a question came up before the Enter); it is tried again
    /// up to [`MAX_TRIES`] times.
    pub tries: u32,
    /// v2: what the session's events descend from once it is in (events-13): the depth of the
    /// action that sent it (0: the person's, nothing to stamp) and its causes (lineage keys, hex;
    /// its own effect key joins them).
    pub depth: u8,
    pub causes: Vec<String>,
}

impl Delivery {
    /// A delivery of `text` into `session`, queued at `now_ms`, with a fresh id and the default
    /// expiry.
    pub fn new(session: &str, text: &str, kind: InjectKind, now_ms: u64) -> Delivery {
        let mut bytes = [0u8; 6];
        crate::sys::random_bytes(&mut bytes);
        Delivery {
            id: bytes.iter().map(|b| format!("{b:02x}")).collect(),
            session: session.to_string(),
            text: text.to_string(),
            note: None,
            kind,
            effect: None,
            group: None,
            version: None,
            queued_ms: now_ms,
            expires_ms: now_ms.saturating_add(EXPIRY_MS),
            urgent: false,
            tries: 0,
            depth: 0,
            causes: Vec::new(),
        }
    }

    /// What the session's events descend from once it is in: its causes and its own effect.
    pub fn drive_causes(&self) -> Vec<String> {
        let mut causes = self.causes.clone();
        if let Some(key) = self.effect.map(|key| key.hex())
            && !causes.contains(&key)
        {
            causes.push(key);
        }
        causes
    }

    /// What a message typed outside a queue (the server's manager, M57) ends with: its marker, and
    /// the text as typed (events-13: the session's next prompt with it makes its events descend
    /// from the message).
    pub fn marked(session: &str, text: &str) -> (String, String) {
        let marker = Delivery::new(session, text, InjectKind::SessionSend, 0).marker();
        let typed = format!("{} {marker}", text.trim_end());
        (marker, typed)
    }

    /// The marker typed with it: `[gc:1a2b3c4d]`.
    pub fn marker(&self) -> String {
        format!("[gc:{}]", &self.id[..self.id.len().min(8)])
    }

    /// Exactly what is typed: the message, its note, and the marker at the end of the last line.
    /// The words ledger records this text, so the hook's prompt matches it.
    pub fn typed(&self) -> String {
        match &self.note {
            Some(note) => format!("{}\n\n{note} {}", self.text.trim_end(), self.marker()),
            None => format!("{} {}", self.text.trim_end(), self.marker()),
        }
    }
}

/// What the app sees of a session right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Inputs {
    /// Its terminal is open and its program runs.
    pub running: bool,
    /// Its agent is working, or waits for an answer (hooks, or a screen that is not quiet yet).
    pub busy: bool,
    /// A prompt or question is on its screen.
    pub asking: bool,
    /// Text is typed in its message box.
    pub draft: bool,
    /// How long ago the person last typed in it.
    pub typed_ms_ago: Option<u64>,
}

/// Why a delivery waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    NotRunning,
    Working,
    Asking,
    Draft,
    Typing,
    /// Another delivery of the session is in flight.
    InFlight,
}

impl Wait {
    /// For the session's header: "waits until … ".
    pub fn words(self) -> &'static str {
        match self {
            Wait::NotRunning => "the session runs",
            Wait::Working => "the agent's turn is over",
            Wait::Asking => "its question is answered",
            Wait::Draft => "the text in its box is sent or cleared",
            Wait::Typing => "you stop typing in it",
            Wait::InFlight => "the message before it is in",
        }
    }
}

/// Whether a session can take a delivery now.
pub fn ready(inputs: &Inputs) -> Result<(), Wait> {
    if !inputs.running {
        Err(Wait::NotRunning)
    } else if inputs.asking {
        Err(Wait::Asking)
    } else if inputs.busy {
        Err(Wait::Working)
    } else if inputs.draft {
        Err(Wait::Draft)
    } else if inputs.typed_ms_ago.is_some_and(|ago| ago < QUIET_TYPING_MS) {
        Err(Wait::Typing)
    } else {
        Ok(())
    }
}

/// Whether the Enter may be pressed after the paste (land-m3): no prompt came up meanwhile, and
/// the box holds exactly what was typed. White space does not count, also inside a word: a box
/// read from the screen joins its wrapped lines with spaces, wherever they wrapped. An empty box
/// (`Some("")`) does not hold it. `box_text` None: the agent shows no box Glasscode can read
/// (Codex), which counts as holding it when nothing is asking.
pub fn may_press_enter(box_text: Option<&str>, asking: bool, typed: &str) -> bool {
    if asking {
        return false;
    }
    match box_text {
        Some(text) => squeezed(text) == squeezed(typed),
        None => true,
    }
}

/// The text without any white space.
pub fn squeezed(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

fn normal(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What became of a delivery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The receipt came (or the session takes no receipt and the text was entered): what to keep.
    Delivered(serde_json::Value),
    /// It was not typed, or the Enter was not pressed: why. It may be tried again.
    NotSent(String),
    /// Typed, and the Enter held back (a prompt came up, or the box held something else): the text
    /// was left in the box, so it is not tried again.
    NotEntered(String),
    /// Typed and entered, and no receipt came.
    Uncertain(String),
    /// A newer version of its group took its place, or it expired.
    Dropped(String),
}

/// What pushing a delivery did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pushed {
    Queued,
    /// The same group and version is queued or in flight already: this one is not.
    Same(String),
    /// Queued, and these older versions of its group are dropped.
    Superseded(Vec<Delivery>),
}

/// What the app does next for a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Nothing,
    Wait(Wait),
    /// Type this one (it is in flight until [`Queues::finish`]).
    Send(Delivery),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Lane {
    waiting: VecDeque<Delivery>,
    in_flight: Option<Delivery>,
    /// Why the first one waited at the last look (`next`), for the session's header.
    wait: Option<Wait>,
}

/// Every session's queue.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Queues {
    lanes: BTreeMap<String, Lane>,
}

impl Queues {
    /// Queue a delivery: an urgent one first. A delivery of a group drops the waiting ones of the
    /// group with another version; one of the same group and version as one queued or in flight is
    /// not queued.
    pub fn push(&mut self, delivery: Delivery) -> Pushed {
        let lane = self.lanes.entry(delivery.session.clone()).or_default();
        if let Some(group) = &delivery.group {
            let same = |other: &Delivery| other.group.as_ref() == Some(group) && other.version == delivery.version;
            if let Some(have) = lane.in_flight.iter().chain(lane.waiting.iter()).find(|other| same(other)) {
                return Pushed::Same(have.id.clone());
            }
        }
        let mut dropped = Vec::new();
        if let Some(group) = &delivery.group {
            let (older, keep): (VecDeque<Delivery>, VecDeque<Delivery>) = lane.waiting.drain(..).partition(|other| other.group.as_ref() == Some(group));
            lane.waiting = keep;
            dropped.extend(older);
        }
        if delivery.urgent {
            lane.waiting.push_front(delivery);
        } else {
            lane.waiting.push_back(delivery);
        }
        if dropped.is_empty() { Pushed::Queued } else { Pushed::Superseded(dropped) }
    }

    /// Take out the deliveries that waited past their expiry.
    pub fn expire(&mut self, now_ms: u64) -> Vec<Delivery> {
        let mut gone = Vec::new();
        for lane in self.lanes.values_mut() {
            let (old, keep): (VecDeque<Delivery>, VecDeque<Delivery>) = lane.waiting.drain(..).partition(|d| d.expires_ms <= now_ms);
            lane.waiting = keep;
            gone.extend(old);
        }
        self.lanes.retain(|_, lane| !lane.waiting.is_empty() || lane.in_flight.is_some());
        gone
    }

    /// What to do next for `session`, seeing `inputs`: the first delivery, when the session can
    /// take it and none is in flight (it is in flight from now on).
    pub fn next(&mut self, session: &str, inputs: &Inputs) -> Step {
        let Some(lane) = self.lanes.get_mut(session) else { return Step::Nothing };
        if lane.in_flight.is_some() {
            lane.wait = Some(Wait::InFlight);
            return Step::Wait(Wait::InFlight);
        }
        if lane.waiting.is_empty() {
            lane.wait = None;
            return Step::Nothing;
        }
        if let Err(wait) = ready(inputs) {
            lane.wait = Some(wait);
            return Step::Wait(wait);
        }
        lane.wait = None;
        let delivery = lane.waiting.pop_front().expect("not empty");
        lane.in_flight = Some(delivery.clone());
        Step::Send(delivery)
    }

    /// The delivery in flight was not sent: it goes back to the front of its queue, unless it was
    /// tried [`MAX_TRIES`] times (then it is given back, over).
    pub fn not_sent(&mut self, session: &str, id: &str) -> Option<Delivery> {
        let lane = self.lanes.get_mut(session)?;
        let mut delivery = lane.in_flight.take_if(|d| d.id == id)?;
        delivery.tries += 1;
        if delivery.tries >= MAX_TRIES {
            if lane.waiting.is_empty() {
                self.lanes.remove(session);
            }
            return Some(delivery);
        }
        lane.waiting.push_front(delivery);
        None
    }

    /// The delivery in flight is over; the next may go. Gives it back.
    pub fn finish(&mut self, session: &str, id: &str) -> Option<Delivery> {
        let lane = self.lanes.get_mut(session)?;
        let done = lane.in_flight.take_if(|d| d.id == id)?;
        if lane.waiting.is_empty() {
            self.lanes.remove(session);
        }
        Some(done)
    }

    /// The sessions with deliveries waiting or in flight.
    pub fn sessions(&self) -> Vec<String> {
        self.lanes.keys().cloned().collect()
    }

    /// How many wait for `session`, the one in flight not counted.
    pub fn waiting(&self, session: &str) -> usize {
        self.lanes.get(session).map_or(0, |lane| lane.waiting.len())
    }

    /// What the session's header says of its queue: how many messages wait, and until when
    /// ("1 message waits until the agent's turn is over"). None when nothing waits.
    pub fn header(&self, session: &str) -> Option<String> {
        let lane = self.lanes.get(session)?;
        let n = lane.waiting.len();
        if n == 0 {
            return None;
        }
        let what = if n == 1 { "1 message waits".to_string() } else { format!("{n} messages wait") };
        Some(match lane.wait {
            Some(wait) => format!("{what} until {}", wait.words()),
            None => what,
        })
    }

    pub fn in_flight(&self, session: &str) -> Option<&Delivery> {
        self.lanes.get(session)?.in_flight.as_ref()
    }
}

/// Whether the delivery whose marker is `marker` reached its session, from the session's hook
/// events written after the claim (the lines of its events file past the length kept in the
/// claim): a `UserPromptSubmit` whose prompt holds the marker (white space aside) is the receipt.
/// None, but a `SessionStart` after the claim: the session started again, and what was typed is
/// gone. Neither: nobody can tell. (When the journal lands, M14, its events are the same source.)
pub fn probe(marker: &str, events: &str) -> ProbeResult {
    let marker = normal(marker);
    let mut restarted = false;
    for line in events.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        match event.get("hook_event_name").and_then(serde_json::Value::as_str) {
            Some("UserPromptSubmit") => {
                let prompt = event.get("prompt").and_then(serde_json::Value::as_str).unwrap_or_default();
                if normal(prompt).contains(&marker) {
                    return ProbeResult::Happened(serde_json::json!({ "receipt": "hook" }));
                }
            }
            Some("SessionStart") => restarted = true,
            _ => {}
        }
    }
    if restarted { ProbeResult::DidNotHappen } else { ProbeResult::Unknown }
}

/// What a probe of a `session.send` claim needs: the session's events file length when it was
/// claimed, and the marker.
pub fn evidence(events_len: u64, marker: &str) -> serde_json::Value {
    serde_json::json!({ "events_len": events_len, "marker": marker })
}

/// The probe of a `session.send` claim from its evidence, reading the session's events file from
/// the length kept. It reads the disk. None: the claim kept no evidence (nothing to look at).
pub fn probe_claim(session: &str, evidence: &serde_json::Value) -> Option<ProbeResult> {
    let marker = evidence.get("marker")?.as_str()?;
    let from = evidence.get("events_len")?.as_u64()?;
    let bytes = std::fs::read(crate::agents::events_file(session)).unwrap_or_default();
    let from = usize::try_from(from).unwrap_or(usize::MAX).min(bytes.len());
    Some(probe(marker, &String::from_utf8_lossy(&bytes[from..])))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message the server's manager types: its marker at the end, and the session's events after
    /// its prompt descend from it.
    #[test]
    fn a_marked_message_drives_its_session() {
        crate::journal::reset_driven();
        let (marker, typed) = Delivery::marked("s7", "Go on with the tests. ");
        assert!(marker.starts_with("[gc:") && typed == format!("Go on with the tests. {marker}").replace(".  ", ". "), "{typed}");
        crate::journal::expect_drive("s7", &marker, 1, &["cd".repeat(32)]);
        crate::journal::hook_events("s7", None, &serde_json::json!({ "hook_event_name": "UserPromptSubmit", "prompt": typed }));
        assert_eq!(crate::journal::driven("s7").map(|d| d.depth), Some(1));
        crate::journal::reset_driven();
    }

    fn delivery(session: &str, text: &str) -> Delivery {
        Delivery::new(session, text, InjectKind::SessionSend, 1_000)
    }

    fn ci(session: &str, head: &str) -> Delivery {
        Delivery { group: Some("ci-fix:acme/app#42".into()), version: Some(head.into()), ..Delivery::new(session, &format!("CI failed at {head}"), InjectKind::Playbook, 1_000) }
    }

    fn idle() -> Inputs {
        Inputs { running: true, ..Inputs::default() }
    }

    #[test]
    fn send_waits_for_idle() {
        let mut queues = Queues::default();
        queues.push(delivery("s1", "look at the failing test"));
        assert_eq!(queues.next("s1", &Inputs::default()), Step::Wait(Wait::NotRunning));
        assert_eq!(queues.next("s1", &Inputs { busy: true, ..idle() }), Step::Wait(Wait::Working));
        assert_eq!(queues.next("s1", &Inputs { asking: true, busy: true, ..idle() }), Step::Wait(Wait::Asking));
        assert_eq!(queues.next("s1", &Inputs { draft: true, ..idle() }), Step::Wait(Wait::Draft));
        assert_eq!(queues.next("s1", &Inputs { typed_ms_ago: Some(QUIET_TYPING_MS - 1), ..idle() }), Step::Wait(Wait::Typing));
        let Step::Send(sent) = queues.next("s1", &Inputs { typed_ms_ago: Some(QUIET_TYPING_MS), ..idle() }) else { panic!("not sent when idle") };
        assert_eq!(sent.text, "look at the failing test");
        // One at a time: the next waits for the first to be in.
        queues.push(delivery("s1", "and then the docs"));
        assert_eq!(queues.next("s1", &idle()), Step::Wait(Wait::InFlight));
        assert_eq!(queues.finish("s1", &sent.id).map(|d| d.id), Some(sent.id.clone()));
        assert!(matches!(queues.next("s1", &idle()), Step::Send(d) if d.text == "and then the docs"));
        // Another session's queue is its own.
        queues.push(delivery("s2", "hello"));
        assert!(matches!(queues.next("s2", &idle()), Step::Send(_)));
        assert_eq!(queues.sessions(), ["s1", "s2"]);
    }

    #[test]
    fn send_superseded_by_new_head() {
        let mut queues = Queues::default();
        let old = ci("s1", "aaa");
        assert_eq!(queues.push(old.clone()), Pushed::Queued);
        queues.push(delivery("s1", "an unrelated message"));
        let Pushed::Superseded(dropped) = queues.push(ci("s1", "bbb")) else { panic!("not superseded") };
        assert_eq!(dropped, [old]);
        assert_eq!(queues.waiting("s1"), 2);
        assert!(matches!(queues.next("s1", &idle()), Step::Send(d) if d.text == "an unrelated message"));
        queues.finish("s1", &queues.in_flight("s1").unwrap().id.clone());
        assert!(matches!(queues.next("s1", &idle()), Step::Send(d) if d.text == "CI failed at bbb"));
    }

    #[test]
    fn two_playbooks_one_event_one_effect() {
        // Two rules react to the same red CI on the same head: one message is typed.
        let mut queues = Queues::default();
        let first = ci("s1", "aaa");
        assert_eq!(queues.push(first.clone()), Pushed::Queued);
        assert_eq!(queues.push(ci("s1", "aaa")), Pushed::Same(first.id.clone()));
        let Step::Send(sent) = queues.next("s1", &idle()) else { panic!() };
        // Also while the first is in flight.
        assert_eq!(queues.push(ci("s1", "aaa")), Pushed::Same(sent.id.clone()));
        queues.finish("s1", &sent.id);
        assert_eq!(queues.next("s1", &idle()), Step::Nothing);
    }

    #[test]
    fn deliveries_expire_and_urgent_ones_go_first() {
        let mut queues = Queues::default();
        let stale = Delivery { expires_ms: 5_000, ..delivery("s1", "stale") };
        queues.push(stale.clone());
        queues.push(delivery("s1", "fresh"));
        queues.push(Delivery { urgent: true, ..delivery("s1", "stop") });
        assert_eq!(queues.expire(5_000), [stale]);
        assert!(matches!(queues.next("s1", &idle()), Step::Send(d) if d.text == "stop"));
        assert_eq!(queues.finish("s1", "not it"), None);
        // One not sent goes back to the front, until its tries are over.
        let in_flight = queues.in_flight("s1").unwrap().id.clone();
        assert_eq!(queues.not_sent("s1", &in_flight), None);
        for _ in 1..MAX_TRIES {
            let Step::Send(again) = queues.next("s1", &idle()) else { panic!() };
            assert_eq!(again.text, "stop");
            if let Some(over) = queues.not_sent("s1", &again.id) {
                assert_eq!(over.tries, MAX_TRIES);
            }
        }
        assert!(matches!(queues.next("s1", &idle()), Step::Send(d) if d.text == "fresh"));
    }

    #[test]
    fn the_enter_waits_for_a_prompt_that_came_up() {
        let d = Delivery { note: Some("(Sent by Glasscode because the manager asked it to.)".into()), ..delivery("s1", "fix the lint") };
        let typed = d.typed();
        assert!(typed.ends_with(&format!("asked it to.) {}", d.marker())), "{typed}");
        assert!(d.marker().starts_with("[gc:") && d.marker().len() == 13);
        assert!(may_press_enter(Some(&typed.replace('\n', " ")), false, &typed));
        // A permission prompt came up after the paste: no Enter.
        assert!(!may_press_enter(Some(&typed), true, &typed));
        // The box holds something else (the person typed, or the paste went elsewhere).
        assert!(!may_press_enter(Some("fix the"), false, &typed));
        assert!(!may_press_enter(Some(""), false, &typed));
        // The screen wrapped it in the middle of a word, or of the marker.
        let marker = d.marker();
        let wrapped = typed.replace(&marker, &format!("{} {}", &marker[..5], &marker[5..])).replace("lint", "li nt");
        assert!(may_press_enter(Some(&wrapped), false, &typed));
        assert!(may_press_enter(None, false, &typed));
        assert_eq!(Delivery { note: None, ..d.clone() }.typed(), format!("fix the lint {}", d.marker()));
    }

    #[test]
    fn the_hook_event_with_the_marker_is_the_receipt() {
        let d = delivery("s1", "fix the lint");
        let typed = d.typed();
        let prompt = serde_json::json!({ "hook_event_name": "UserPromptSubmit", "prompt": typed.replace(' ', "  ") }).to_string();
        let other = serde_json::json!({ "hook_event_name": "UserPromptSubmit", "prompt": "something else" }).to_string();
        let stop = serde_json::json!({ "hook_event_name": "Stop" }).to_string();
        let start = serde_json::json!({ "hook_event_name": "SessionStart" }).to_string();
        assert!(matches!(probe(&d.marker(), &format!("{other}\n{prompt}\n")), ProbeResult::Happened(_)));
        assert_eq!(probe(&d.marker(), &format!("{other}\n{stop}\n")), ProbeResult::Unknown);
        assert_eq!(probe(&d.marker(), &format!("{stop}\n{start}\nnot json\n")), ProbeResult::DidNotHappen);
        assert_eq!(probe(&d.marker(), ""), ProbeResult::Unknown);
        assert_eq!(evidence(10, "[gc:1]")["events_len"], 10);
    }

    /// M31: the session's header says how many messages wait, and until when.
    #[test]
    fn the_header_says_what_waits_and_until_when() {
        let mut queues = Queues::default();
        assert_eq!(queues.header("s1"), None);
        queues.push(delivery("s1", "one"));
        queues.push(delivery("s1", "two"));
        assert_eq!(queues.header("s1").as_deref(), Some("2 messages wait"));
        let busy = Inputs { busy: true, ..idle() };
        assert!(matches!(queues.next("s1", &busy), Step::Wait(Wait::Working)));
        assert_eq!(queues.header("s1").as_deref(), Some("2 messages wait until the agent's turn is over"));
        assert!(matches!(queues.next("s1", &idle()), Step::Send(_)));
        assert_eq!(queues.header("s1").as_deref(), Some("1 message waits"));
        assert!(queues.in_flight("s1").is_some());
        assert!(matches!(queues.next("s1", &idle()), Step::Wait(Wait::InFlight)));
        assert_eq!(queues.header("s1").as_deref(), Some("1 message waits until the message before it is in"));
        assert!(queues.in_flight("s2").is_none());
    }

}
