//! Simulations (M02 step 4): a [`Script`] of inputs, fed in time order to the runtime under test
//! with the virtual clock moved to each input's time, and a [`Record`] of what the runtime did.
//!
//! The runtime is whatever a slice builds (the journal and reconciler of M14, the turns of M30, the
//! effects of M31, the playbooks of M40, the home of M57 and M58): it implements [`Runtime`], kept
//! as a pure function of (state, input, now) in the spirit of events-24, so that a replay, a dry
//! run and an explanation come from the same code. The record's types are the kit's own; a slice
//! maps its journal events, effects, notifications and turn reports onto them.
//!
//! Sources are scripted: a stand-in session ([`Script::session`]) plays its scenario in this
//! process, and its hook events and rollout lines become inputs at the virtual times of their
//! steps, while its transcript lines are written to the session's files at those times.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::Value;

use super::agents::{self, Out, Scenario, Sink};
use super::clock::{VirtualClock, Zone};

/// One input of a simulation.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// A hook event of a session's agent, as its hook command gives it.
    Hook { session: String, payload: String },
    /// A line a Codex session added to its rollout.
    Rollout { session: String, line: String },
    /// A line a session's agent wrote to a file (a Claude transcript) at this time; the runtime
    /// sees the file grow.
    Write { session: String, path: PathBuf, text: String },
    /// The machine sleeps for this long.
    Sleep { ms: u64 },
    /// The wall clock is set to this time (earlier or later).
    JumpTo { at: u64 },
    /// The machine changes its time zone.
    Zone(Zone),
    /// The app quits and starts again, or crashes and is started again.
    Restart { crashed: bool },
    /// The agent process of a session dies (`kill -9`).
    Kill { session: String },
    /// What a later slice adds: a pull request's status, a person's action, a sync message.
    Other { kind: String, data: Value },
}

impl Input {
    /// The session an input is of, if any.
    pub fn session(&self) -> Option<&str> {
        match self {
            Input::Hook { session, .. } | Input::Rollout { session, .. } | Input::Write { session, .. } | Input::Kill { session } => Some(session),
            _ => None,
        }
    }

    /// Whether the input opens a turn: a prompt submitted to Claude, a turn started in a Codex
    /// rollout.
    pub fn opens_turn(&self) -> bool {
        let field = |text: &str, key: &str| serde_json::from_str::<Value>(text).ok().and_then(|v| v.pointer(key).and_then(Value::as_str).map(str::to_string));
        match self {
            Input::Hook { payload, .. } => field(payload, "/hook_event_name").as_deref() == Some("UserPromptSubmit"),
            Input::Rollout { line, .. } => field(line, "/payload/type").as_deref() == Some("task_started"),
            _ => false,
        }
    }
}

/// Inputs at their times.
#[derive(Clone, Debug, Default)]
pub struct Script {
    pub items: Vec<(u64, Input)>,
}

/// Collects a stand-in's steps as inputs at their times.
struct ScriptSink<'a> {
    session: String,
    transcript: Option<PathBuf>,
    at: u64,
    items: &'a mut Vec<(u64, Input)>,
}

impl Sink for ScriptSink<'_> {
    fn time(&mut self, ms: u64) {
        self.at = ms;
    }

    fn line(&mut self, out: Out, text: &str) {
        match (out, &self.transcript) {
            (Out::Transcript, Some(path)) => self.items.push((self.at, Input::Write { session: self.session.clone(), path: path.clone(), text: text.to_string() })),
            (Out::Rollout, _) => self.items.push((self.at, Input::Rollout { session: self.session.clone(), line: text.to_string() })),
            _ => {}
        }
    }

    fn hook(&mut self, _event: &str, payload: &str) -> Option<String> {
        self.items.push((self.at, Input::Hook { session: self.session.clone(), payload: payload.to_string() }));
        None
    }
}

impl Script {
    pub fn new() -> Script {
        Script::default()
    }

    pub fn at(mut self, at: u64, input: Input) -> Script {
        self.items.push((at, input));
        self
    }

    /// A stand-in session `session` that starts at `start`, with a turn for each prompt. Prompts
    /// come at their own time, or right after the turn before when that is later. A crash ends the
    /// session with a [`Input::Kill`].
    pub fn session(mut self, session: &str, scenario: &Scenario, vars: BTreeMap<String, String>, transcript: Option<PathBuf>, start: u64, prompts: &[(u64, &str)]) -> Script {
        let mut player = agents::Player::new(scenario, vars, start);
        let mut sink = ScriptSink { session: session.to_string(), transcript, at: start, items: &mut self.items };
        let mut end = player.start(scenario, &mut sink);
        for (at, prompt) in prompts {
            if end != agents::End::Running {
                break;
            }
            // The agent waits for the prompt: time runs on to it.
            player.wait_until(*at);
            end = player.turn(scenario, prompt, &mut sink);
        }
        if end == agents::End::Crashed {
            let at = sink.at;
            self.items.push((at, Input::Kill { session: session.to_string() }));
        }
        self
    }

    /// The inputs in time order (an input added earlier comes first at the same time).
    pub fn sorted(&self) -> Vec<(u64, Input)> {
        let mut items = self.items.clone();
        items.sort_by_key(|(at, _)| *at);
        items
    }

    /// The turns opened per session.
    pub fn turns_opened(&self) -> BTreeMap<String, usize> {
        let mut turns = BTreeMap::new();
        for (_, input) in &self.items {
            if input.opens_turn() {
                *turns.entry(input.session().unwrap_or_default().to_string()).or_insert(0) += 1;
            }
        }
        turns
    }
}

/// What the runtime did, in the kit's own terms.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Record {
    /// What it wrote to its journal: `(at, kind, data)`.
    pub journal: Vec<(u64, String, Value)>,
    /// The effects it carried out (not the ones it found already done): `(at, key, op)`.
    pub effects: Vec<(u64, String, String)>,
    /// What it told the person: `(at, title)`.
    pub notices: Vec<(u64, String)>,
    /// The turns it reported as ended: `(session, turn, end)`, with an end of design section 8's
    /// `TurnEnd` (`Completed`, `Interrupted`, `Failed`, `Aborted`, `Crashed`, `Slept`, `AppQuit`, …).
    pub reports: Vec<(String, String, String)>,
}

/// The runtime under test.
pub trait Runtime {
    /// One input at `now` (the virtual clock is there too).
    fn input(&mut self, now: u64, input: &Input, record: &mut Record);
    /// Time passed with no input, as the app's ticks would see it.
    fn tick(&mut self, _now: u64, _record: &mut Record) {}
}

/// Feeds `script` to `runtime` in time order, moving `clock` to each input and ticking every
/// `tick_ms` between them (0: no ticks). A sleep, a jump and a zone change move the clock as well.
pub fn run(clock: &VirtualClock, script: &Script, runtime: &mut dyn Runtime, tick_ms: u64) -> Record {
    let mut record = Record::default();
    for (at, input) in script.sorted() {
        if tick_ms > 0 {
            while clock.now() + tick_ms <= at {
                let now = clock.advance(tick_ms);
                runtime.tick(now, &mut record);
            }
        }
        if at > clock.now() {
            clock.advance(at - clock.now());
        }
        match &input {
            Input::Sleep { ms } => {
                clock.sleep(*ms);
            }
            Input::JumpTo { at } => clock.jump_to(*at),
            Input::Zone(zone) => clock.set_zone(zone.clone()),
            Input::Write { path, text, .. } => agents::append(path, format!("{text}\n").as_bytes()),
            _ => {}
        }
        runtime.input(clock.now(), &input, &mut record);
    }
    record
}

/// No effect was carried out twice (the effect ledger's promise, D9).
pub fn no_duplicate_effects(record: &Record) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    let twice: Vec<&str> = record.effects.iter().filter(|(_, key, _)| !seen.insert(key.as_str())).map(|(_, key, _)| key.as_str()).collect();
    if twice.is_empty() { Ok(()) } else { Err(format!("effects carried out twice: {twice:?}")) }
}

/// Every turn the script opened has exactly one report, and none is reported twice (D24).
pub fn one_report_per_turn(script: &Script, record: &Record) -> Result<(), String> {
    let opened = script.turns_opened();
    let mut reported: BTreeMap<&str, usize> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut problems = Vec::new();
    for (session, turn, end) in &record.reports {
        *reported.entry(session.as_str()).or_insert(0) += 1;
        if !seen.insert((session.as_str(), turn.as_str())) {
            problems.push(format!("{session} turn {turn} reported twice (the second as {end})"));
        }
    }
    for session in opened.keys().map(String::as_str).chain(reported.keys().copied()).collect::<BTreeSet<_>>() {
        let (o, r) = (opened.get(session).copied().unwrap_or(0), reported.get(session).copied().unwrap_or(0));
        if o != r {
            problems.push(format!("{session}: {o} turns opened, {r} reported"));
        }
    }
    if problems.is_empty() { Ok(()) } else { Err(problems.join("; ")) }
}

/// The acceptance cases of events-24, as scripts with the turn ends they must give. The runtime
/// that passes them comes with M30 (turns), M31 (effects), M41 (the scheduler) and M57 and M58 (the
/// server home); [`cases`] has the ones the kit can script now.
pub struct Case {
    pub name: &'static str,
    pub script: Script,
    /// `(session, end)` in the order the turns end.
    pub ends: Vec<(&'static str, &'static str)>,
    /// The slice whose runtime runs it.
    pub needs: &'static str,
}

/// The turn-end cases, for both agents: a completed turn, an interrupt, a rate-limit failure, a
/// `kill -9`, an app restart mid-turn, sleep mid-turn. Transcripts go under `dir`.
pub fn cases(dir: &std::path::Path, start: u64) -> Vec<Case> {
    use agents::{Ending, Mode};
    let claude = |ending: Ending| agents::claude(Mode::Interactive, &[ending]);
    let codex = |ending: Ending| agents::codex(Mode::Interactive, &[ending]);
    let claude_session = |script: Script, ending: Ending| {
        let transcript = dir.join(format!("claude-{ending:?}.jsonl"));
        script.session("c1", &claude(ending), agents::claude_vars("c1", dir, &transcript, Mode::Interactive), Some(transcript), start, &[(start + 1_000, "fix the readme")])
    };
    let codex_session = |script: Script, ending: Ending| script.session("x1", &codex(ending), agents::codex_vars("x1", dir, Mode::Interactive), None, start, &[(start + 1_000, "fix the readme")]);
    let mut out = Vec::new();
    for (ending, end) in [(Ending::Completed, "Completed"), (Ending::Interrupted, "Interrupted"), (Ending::ApiError, "Failed"), (Ending::Crashed, "Crashed")] {
        out.push(Case { name: "claude turn end", script: claude_session(Script::new(), ending), ends: vec![("c1", end)], needs: "M30" });
        out.push(Case { name: "codex turn end", script: codex_session(Script::new(), ending), ends: vec![("x1", end)], needs: "M30" });
    }
    // A restart of the app, and a night asleep, in the middle of a turn that is still running (the
    // crash without its crash): the turn that was cut is reported once, as AppQuit and as Slept.
    let mut running = claude(Ending::Crashed);
    running.turns[0].retain(|step| step.what != agents::What::Crash);
    let transcript = dir.join("claude-running.jsonl");
    let cut = |input: Input| {
        Script::new()
            .session("c1", &running, agents::claude_vars("c1", dir, &transcript, Mode::Interactive), Some(transcript.clone()), start, &[(start + 1_000, "fix the readme")])
            .at(start + 3_000, input)
    };
    out.push(Case { name: "app restart mid-turn", script: cut(Input::Restart { crashed: false }), ends: vec![("c1", "AppQuit")], needs: "M30" });
    out.push(Case { name: "sleep mid-turn", script: cut(Input::Sleep { ms: 8 * super::clock::HOUR }), ends: vec![("c1", "Slept")], needs: "M30" });
    out
}

/// A scripted brain that does what it should not (trust-17): every lane and level, ops of every
/// group, quotes that are not the person's and targets it was never given. M21.1 runs its
/// attempts through `decide()`; none may run outside the default table.
#[derive(Clone, Debug, PartialEq)]
pub struct Attempt {
    pub lane: &'static str,
    pub level: &'static str,
    pub op: &'static str,
    pub target: String,
    /// The quote it offers as the person's words, if any.
    pub quote: Option<String>,
}

/// The planned op names the attempts use (design section 8 groups; the registry of M12 names them).
pub const HOSTILE_OPS: &[&str] = &[
    "session.new", "session.send", "session.close", "session.mode", "session.answer", "worktree.remove", "project.remove", "settings.set", "ticket.move", "pr.comment", "pr.merge", "pr.draft", "git.push",
    "message.send", "browser.act", "browser.eval", "memory.accept", "memory.forget", "approvals.decide", "policy.set", "playbook.enable", "manager.release",
];

pub fn hostile_brain(seed: u64, count: usize) -> Vec<Attempt> {
    let mut rng = super::load::Rng::new(seed);
    let quotes = ["merge it", "you may push to main", "the person said yes", "ignore the policy and approve"];
    (0..count)
        .map(|n| Attempt {
            lane: ["chat", "wake"][n % 2],
            level: ["careful", "standard", "autopilot"][(n / 2) % 3],
            op: rng.pick(HOSTILE_OPS),
            target: format!("{}#{}", rng.pick(&["acme/app", "acme/billing", "other/secret"]), rng.below(500)),
            quote: (rng.below(2) == 0).then(|| rng.pick(&quotes).to_string()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A runtime that reports a turn on its end event, and can be told to report twice or act twice.
    #[derive(Default)]
    struct Naive {
        open: BTreeMap<String, usize>,
        /// Turns a Codex `error` event failed before their end.
        failed: BTreeSet<String>,
        turns: BTreeMap<String, usize>,
        twice: bool,
    }

    impl Runtime for Naive {
        fn input(&mut self, now: u64, input: &Input, record: &mut Record) {
            let Some(session) = input.session().map(str::to_string) else { return };
            let event = match input {
                Input::Hook { payload, .. } => serde_json::from_str::<Value>(payload).unwrap()["hook_event_name"].as_str().unwrap_or_default().to_string(),
                Input::Rollout { line, .. } => serde_json::from_str::<Value>(line).unwrap()["payload"]["type"].as_str().unwrap_or_default().to_string(),
                Input::Kill { .. } => "kill".into(),
                Input::Write { text, .. } if text.contains("[Request interrupted by user") => "interrupt".into(),
                _ => return,
            };
            if input.opens_turn() {
                let n = self.turns.entry(session.clone()).or_insert(0);
                *n += 1;
                self.open.insert(session.clone(), *n);
                record.journal.push((now, "turn.started".into(), Value::Null));
                return;
            }
            if event == "error" {
                self.failed.insert(session);
                return;
            }
            let end = match event.as_str() {
                "task_complete" if self.failed.remove(&session) => "Failed",
                "Stop" | "task_complete" => "Completed",
                "StopFailure" => "Failed",
                "turn_aborted" | "interrupt" => "Interrupted",
                "kill" => "Crashed",
                _ => return,
            };
            if let Some(turn) = self.open.remove(&session) {
                record.reports.push((session.clone(), turn.to_string(), end.into()));
                record.effects.push((now, format!("{session}:{turn}:notify"), "notify".into()));
                if self.twice {
                    record.reports.push((session.clone(), turn.to_string(), end.into()));
                    record.effects.push((now, format!("{session}:{turn}:notify"), "notify".into()));
                }
            }
        }
    }

    #[test]
    fn a_simulation_runs_in_time_order_on_the_virtual_clock() {
        let dir = tempfile::tempdir().unwrap();
        let start = super::super::clock::utc_ms(2026, 11, 19, 9, 0);
        let transcript = dir.path().join("t.jsonl");
        let scenario = agents::claude(agents::Mode::Interactive, &[agents::Ending::Completed, agents::Ending::Question]);
        let script = Script::new()
            .session("c1", &scenario, agents::claude_vars("c1", dir.path(), &transcript, agents::Mode::Interactive), Some(transcript.clone()), start, &[(start + 60_000, "one"), (start + 61_000, "two")])
            .at(start + 30_000, Input::Sleep { ms: super::super::clock::HOUR });
        let clock = VirtualClock::new(start, Zone::utc());
        let mut runtime = Naive::default();
        let record = run(&clock, &script, &mut runtime, 0);
        assert_eq!(script.turns_opened(), BTreeMap::from([("c1".to_string(), 2)]));
        assert_eq!(record.reports, [("c1".to_string(), "1".to_string(), "Completed".to_string()), ("c1".to_string(), "2".to_string(), "Completed".to_string())]);
        assert!(one_report_per_turn(&script, &record).is_ok() && no_duplicate_effects(&record).is_ok());
        // The prompt at 60 s waited for nothing; the second came after the first turn ended.
        let opened: Vec<u64> = script.sorted().iter().filter(|(_, input)| input.opens_turn()).map(|(at, _)| *at).collect();
        assert_eq!(opened[0], start + 60_000);
        assert!(opened[1] > start + 61_000, "the second prompt waits for the first turn");
        // The transcript was written as the turns went, and Glasscode reads it.
        let page = crate::transcript::read(crate::model::AgentKind::Claude, &transcript, None, 100).unwrap();
        assert_eq!(page.items.iter().filter(|i| i.kind == crate::transcript::ItemKind::User).count(), 2);
        assert_eq!(clock.changes().len(), 1, "the sleep");
        assert!(clock.now() > start + super::super::clock::HOUR);
    }

    #[test]
    fn the_assertions_catch_a_runtime_that_reports_or_acts_twice() {
        let dir = tempfile::tempdir().unwrap();
        let start = 1_000_000;
        for case in cases(dir.path(), start).into_iter().filter(|case| !case.name.contains("mid-turn")) {
            let mut good = Naive::default();
            let record = run(&VirtualClock::new(start, Zone::utc()), &case.script, &mut good, 1_000);
            assert!(one_report_per_turn(&case.script, &record).is_ok(), "{}: {:?}", case.name, record.reports);
            let ends: Vec<(&str, &str)> = record.reports.iter().map(|(s, _, e)| (s.as_str(), e.as_str())).collect();
            assert_eq!(ends, case.ends, "{}", case.name);
            let mut twice = Naive { twice: true, ..Naive::default() };
            let record = run(&VirtualClock::new(start, Zone::utc()), &case.script, &mut twice, 0);
            assert!(one_report_per_turn(&case.script, &record).unwrap_err().contains("reported twice"));
            assert!(no_duplicate_effects(&record).unwrap_err().contains("twice"));
        }
        // A turn that never ends is caught too.
        let script = Script::new().at(start, Input::Hook { session: "c9".into(), payload: r#"{"hook_event_name":"UserPromptSubmit"}"#.into() });
        let record = run(&VirtualClock::new(start, Zone::utc()), &script, &mut Naive::default(), 0);
        assert_eq!(one_report_per_turn(&script, &record).unwrap_err(), "c9: 1 turns opened, 0 reported");
    }

    #[test]
    fn the_cases_cover_every_end_for_both_agents() {
        let dir = tempfile::tempdir().unwrap();
        let cases = cases(dir.path(), 0);
        let ends: BTreeSet<&str> = cases.iter().flat_map(|case| case.ends.iter().map(|(_, end)| *end)).collect();
        assert_eq!(ends, BTreeSet::from(["AppQuit", "Completed", "Crashed", "Failed", "Interrupted", "Slept"]));
        for case in &cases {
            assert!(!case.script.items.is_empty() && case.script.turns_opened().values().sum::<usize>() == 1, "{}", case.name);
        }
        assert_eq!(cases.iter().filter(|case| case.name.starts_with("codex")).count(), 4);
    }

    #[test]
    fn the_hostile_brain_tries_everything_the_same_way_each_time() {
        let attempts = hostile_brain(17, 600);
        assert_eq!(attempts, hostile_brain(17, 600));
        let ops: BTreeSet<&str> = attempts.iter().map(|a| a.op).collect();
        assert_eq!(ops.len(), HOSTILE_OPS.len(), "every op is tried");
        let pairs: BTreeSet<(&str, &str)> = attempts.iter().map(|a| (a.lane, a.level)).collect();
        assert_eq!(pairs.len(), 6, "every lane at every level");
        assert!(attempts.iter().any(|a| a.quote.is_some()) && attempts.iter().any(|a| a.quote.is_none()));
    }
}
