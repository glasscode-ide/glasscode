//! The follow-ups on the server (M56.2, the `follow-ups` built-in): `glasscode_core::manager::
//! followups` while the server is the manager's home, so a review the person gave goes on being
//! followed while the Mac is away; the Mac runs them while the Mac is home (`followups_ui.rs`),
//! never both. They run with the engine at v2, the manager on and the built-in on (what the Mac
//! said, `ManagerFromMac::follow_ups`), in a thread of their own.
//!
//! - **Where they are.** The server's own `manager/work.json`: the sync keeps its follow-ups in
//!   step with the Mac's both ways, each by the stamp of its last change
//!   (`WorkLedger::follow_merge`), and every change here is stamped (`WorkLedger::follow_edit`).
//! - **Every 30 s,** `followups::away`: the teammates' names, and a quiet one told when nobody
//!   known has the ball. A note to a teammate is left for the Mac: the outbox and its sending are
//!   the Mac's, so it drafts the note when it is home again. Reviews received on the person's own
//!   pull requests are the Mac's too (the server does not read its sessions).
//! - **Every 5 min,** the reviews the person gave (`gh::given_reviews` for the login the Mac said,
//!   with `gh` signed in here), then `followups::given`: "ready for you to look again", once on
//!   either home (the "fixed" mark travels with the follow-up).
//! - **What they say** goes in the manager's conversation (which syncs to the Mac) and as a
//!   notice with the Mac's next sync.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use glasscode_core::manager::{Action, FollowKind, Person, Settings, followups, gh};
use glasscode_core::paths::MachineId;
use glasscode_core::remote::{FollowUpsFromMac, ServerNotice};
use glasscode_core::store::Store as _;
use glasscode_core::work::WorkLedger;

use crate::daemon::log;

/// How often the daemon looks, and how often the reviews the person gave are read.
const EVERY: Duration = Duration::from_secs(30);
const REVIEWS_EVERY: Duration = Duration::from_secs(5 * 60);

static LOOKED: Mutex<Option<Instant>> = Mutex::new(None);
static REVIEWS_AT: Mutex<Option<Instant>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);
static WHY_NOT: Mutex<Option<String>> = Mutex::new(None);
/// The last round's failure, said once until it changes (`gh` not signed in here, say).
static FAILED: Mutex<Option<String>> = Mutex::new(None);
/// What they told since the last sync: the sync takes them (`take_notices`).
static NOTICES: Mutex<Vec<ServerNotice>> = Mutex::new(Vec::new());

/// Why the follow-ups do not run here now (none: they may), and the home's generation and what
/// the Mac said for them when they may.
fn not_here() -> Result<(u64, FollowUpsFromMac), String> {
    use glasscode_core::store::{Engine, EngineSwitch};
    if EngineSwitch::current().engine != Engine::V2 {
        return Err("the engine here is v1".into());
    }
    let generation = crate::home::server_generation().ok_or("the manager's home is the Mac, which follows them")?;
    let said = crate::brain::from_mac().ok_or("the Mac has not said how the manager runs yet")?;
    if !said.enabled {
        return Err("the manager is off".into());
    }
    let follow = said.follow_ups.ok_or("the follow-ups are off on the Mac (Settings › Manager › Built-ins)")?;
    Ok((generation, follow))
}

/// The notices since the last sync, for its reply.
pub fn take_notices() -> Vec<ServerNotice> {
    std::mem::take(&mut *NOTICES.lock().unwrap_or_else(|p| p.into_inner()))
}

/// From the daemon's tick: every 30 s, a round in its own thread when they may run here and
/// none runs.
pub fn tick(shared: &crate::daemon::Shared) {
    {
        let mut looked = LOOKED.lock().unwrap_or_else(|p| p.into_inner());
        if looked.is_some_and(|at| at.elapsed() < EVERY) {
            return;
        }
        *looked = Some(Instant::now());
    }
    let (generation, said) = match not_here() {
        Ok(here) => here,
        Err(why) => {
            let mut told = WHY_NOT.lock().unwrap_or_else(|p| p.into_inner());
            if told.as_deref() != Some(why.as_str()) {
                log(format!("follow-ups: they do not run here: {why}"));
                *told = Some(why);
            }
            return;
        }
    };
    *WHY_NOT.lock().unwrap_or_else(|p| p.into_inner()) = None;
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let reviews = {
        let mut at = REVIEWS_AT.lock().unwrap_or_else(|p| p.into_inner());
        let due = said.me.is_some() && at.is_none_or(|at| at.elapsed() >= REVIEWS_EVERY);
        if due {
            *at = Some(Instant::now());
        }
        due
    };
    let env = crate::daemon::lock(shared).env.clone();
    let _ = std::thread::Builder::new().name("follow-ups".into()).spawn(move || {
        let failed = round(generation, &said, reviews, &|me, known| gh::given_reviews(me, known, &env).map_err(|e| format!("{e:#}"))).err();
        let mut last = FAILED.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(why) = &failed
            && last.as_deref() != Some(why.as_str())
        {
            log(format!("follow-ups: {why}"));
        }
        // A round that read the reviews, or found nothing to read, clears it.
        if failed.is_some() || reviews {
            *last = failed;
        }
        drop(last);
        RUNNING.store(false, Ordering::SeqCst);
    });
}

/// The teammates the Mac said, as the follow-ups' rules read them.
fn settings_of(said: &FollowUpsFromMac) -> Settings {
    let people = said.people.iter().map(|t| Person { name: t.name.clone(), github: t.github.clone(), ..Person::default() }).collect();
    Settings { enabled: true, people, ..Settings::default() }
}

type GivenReviews<'a> = dyn Fn(&str, &[(String, u64)]) -> Result<Vec<gh::GivenReview>, String> + 'a;

/// One round: the quiet ones, then (when due) the reviews the person gave, each change stamped
/// and kept, and what they say told.
fn round(generation: u64, said: &FollowUpsFromMac, reviews: bool, given_reviews: &GivenReviews) -> Result<(), String> {
    let machine = crate::owners::machine().ok_or("this server has no machine id")?;
    let settings = settings_of(said);
    let now = glasscode_core::model::now();
    let quiet = change(&machine, |ledger| followups::away(&mut ledger.book(), &settings, now))?;
    tell(quiet);
    let Some(me) = said.me.as_deref().filter(|_| reviews) else { return Ok(()) };
    let known: Vec<(String, u64)> = WorkLedger::load().map_err(|err| format!("the work ledger cannot be read ({err})"))?.follow_ups.iter().filter(|f| f.kind == FollowKind::BlockersGiven && f.is_open()).map(|f| (f.repo.clone(), f.number)).collect();
    let found = given_reviews(me, &known).map_err(|why| format!("the reviews you gave could not be read ({why})"))?;
    // The Mac may have become home while they were read: then they are its.
    if not_here().map(|(at, _)| at) != Ok(generation) {
        return Ok(());
    }
    let now = glasscode_core::model::now();
    let fixed = change(&machine, |ledger| followups::given(&mut ledger.book(), &settings, &found, None, now))?;
    tell(fixed);
    Ok(())
}

/// A change to the follow-ups, kept (and stamped) only when it changes them: a round that finds
/// nothing writes nothing.
fn change(machine: &MachineId, rule: impl Fn(&mut WorkLedger) -> Vec<Action>) -> Result<Vec<Action>, String> {
    let mut trial = WorkLedger::load().map_err(|err| format!("the work ledger cannot be read ({err})"))?;
    let before = (trial.follow_ups.clone(), trial.follow_seen.clone());
    let actions = rule(&mut trial);
    if actions.is_empty() && (trial.follow_ups.clone(), trial.follow_seen.clone()) == before {
        return Ok(Vec::new());
    }
    glasscode_core::store::update::<WorkLedger, Vec<Action>>(|ledger| ledger.follow_edit(machine, |ledger| rule(ledger))).map_err(|err| format!("the follow-ups could not be kept ({err})"))
}

/// What a round says: a line in the manager's conversation for each (the reviews' "fixed" says it
/// in its own words, else the notification's), and a notice for the Mac's next sync. A note to a
/// teammate is the Mac's (`followups::away` asks for none).
fn tell(actions: Vec<Action>) {
    if actions.is_empty() {
        return;
    }
    use glasscode_core::conversation::{Author, Conversation, Draft, Kind};
    let says = actions.iter().any(|a| matches!(a, Action::Say { .. }));
    let mut lines = Vec::new();
    for action in actions {
        match action {
            Action::Notify { title, body, .. } => {
                if !says {
                    lines.push(format!("{title}. {body}"));
                }
                let matter = format!("follow-ups:{}", glasscode_core::effects::short_hash(&format!("{title}|{body}")));
                log(format!("follow-ups: {title}: {body}"));
                let mut notices = NOTICES.lock().unwrap_or_else(|p| p.into_inner());
                notices.retain(|n| n.matter != matter);
                notices.push(ServerNotice { title, body, matter });
            }
            Action::Say { text, .. } => lines.push(text),
            other => log(format!("follow-ups: the server has no use for {other:?}")),
        }
    }
    let mut chat = Conversation::load();
    for text in lines {
        let matter = format!("follow-ups:{}", glasscode_core::effects::short_hash(&text));
        if let Err(err) = chat.post(Draft { author: Author::App, kind: Kind::Nudge { matter }, text, refs: Vec::new(), topic: None }) {
            log(format!("follow-ups: a line could not be posted: {err}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glasscode_core::manager::Blocker;
    use glasscode_core::remote::{ManagerFromMac, Teammate};
    use glasscode_core::store::{Engine, EngineSwitch};

    fn said() -> FollowUpsFromMac {
        FollowUpsFromMac { me: Some("robin".into()), people: vec![Teammate { name: "Sam".into(), github: "sam-dev".into() }] }
    }

    fn review(number: u64, author: &str, head: &str, resolved: bool) -> gh::GivenReview {
        gh::GivenReview {
            repo: "acme/app".into(),
            number,
            title: format!("Change {number}"),
            url: format!("https://github.com/acme/app/pull/{number}"),
            author: author.into(),
            head: head.into(),
            reviewed_head: "aaa".into(),
            my_state: "CHANGES_REQUESTED".into(),
            threads: vec![Blocker { thread: "t1".into(), text: "fix it".into(), path: "src/a.rs".into(), resolved }],
            state: "open".into(),
            updated: 0,
        }
    }

    #[test]
    fn they_run_here_only_on_a_v2_server_that_is_home_with_them_on() {
        let (_home, _scratch) = crate::daemon::tests::scratch_home();
        glasscode_core::store::update::<EngineSwitch, ()>(|s| s.engine = Engine::V1).unwrap();
        assert!(not_here().unwrap_err().contains("v1"));
        glasscode_core::store::update::<EngineSwitch, ()>(|s| s.engine = Engine::V2).unwrap();
        assert!(not_here().unwrap_err().contains("not said"));
        crate::brain::keep_from_mac(ManagerFromMac { enabled: true, ..ManagerFromMac::default() });
        assert!(not_here().unwrap_err().contains("off on the Mac"));
        crate::brain::keep_from_mac(ManagerFromMac { enabled: false, follow_ups: Some(said()), ..ManagerFromMac::default() });
        assert!(not_here().unwrap_err().contains("manager is off"));
        crate::brain::keep_from_mac(ManagerFromMac { enabled: true, follow_ups: Some(said()), ..ManagerFromMac::default() });
        // With no home record, the server is home (generation 0).
        assert_eq!(not_here(), Ok((0, said())));
        glasscode_core::store::update::<EngineSwitch, ()>(|s| s.engine = Engine::V1).unwrap();
    }

    /// A round here: the reviews the person gave become follow-ups (stamped, so the sync takes
    /// them to the Mac); "fixed" is told once, in the conversation and as a notice; a quiet one
    /// with nobody known is told, and Sam's note is left for the Mac; a round with nothing new
    /// writes nothing.
    #[test]
    fn a_round_here_follows_the_reviews_and_tells_once() {
        let (_home, _scratch) = crate::daemon::tests::scratch_home();
        glasscode_core::store::update::<EngineSwitch, ()>(|s| s.engine = Engine::V2).unwrap();
        crate::brain::keep_from_mac(ManagerFromMac { enabled: true, follow_ups: Some(said()), ..ManagerFromMac::default() });
        let _ = take_notices();
        let machine = crate::owners::machine().unwrap();
        let open = |_: &str, _: &[(String, u64)]| Ok(vec![review(1111, "sam-dev", "aaa", false), review(1200, "stranger", "aaa", false)]);
        round(0, &said(), true, &open).unwrap();
        let ledger = WorkLedger::load().unwrap();
        assert_eq!(ledger.follow_ups.iter().map(|f| (f.number, f.person.as_str())).collect::<Vec<_>>(), [(1111, "Sam"), (1200, "")]);
        assert_eq!(ledger.follow_changed(&machine, None).len(), 2, "stamped here, for the Mac");
        assert!(take_notices().is_empty());

        // Two days on, nothing happened: the stranger's is told; Sam's note waits for the Mac.
        glasscode_core::store::update::<WorkLedger, ()>(|l| {
            for f in &mut l.follow_ups {
                f.created -= followups::STALE_AFTER;
                f.updated -= followups::STALE_AFTER;
            }
        })
        .unwrap();
        round(0, &said(), false, &open).unwrap();
        let told = take_notices();
        assert_eq!(told.iter().map(|n| n.body.as_str()).collect::<Vec<_>>(), ["Stranger has 1 open blocker on #1200."]);
        assert!(WorkLedger::load().unwrap().follow_ups.iter().find(|f| f.number == 1111).unwrap().nudged.is_none());
        let written = std::fs::metadata(WorkLedger::path()).unwrap().modified().unwrap();
        round(0, &said(), false, &open).unwrap();
        assert!(take_notices().is_empty());
        assert_eq!(std::fs::metadata(WorkLedger::path()).unwrap().modified().unwrap(), written, "nothing new, nothing written");

        // Sam fixed it: told once.
        let fixed = |_: &str, _: &[(String, u64)]| Ok(vec![review(1111, "sam-dev", "bbb", true)]);
        round(0, &said(), true, &fixed).unwrap();
        assert_eq!(take_notices().iter().map(|n| n.title.as_str()).collect::<Vec<_>>(), ["#1111 is ready for you to look again"]);
        round(0, &said(), true, &fixed).unwrap();
        assert!(take_notices().is_empty());
        let chat = glasscode_core::conversation::Conversation::load();
        let lines: Vec<String> = chat.items().iter().map(|i| i.text.clone()).collect();
        assert!(lines.iter().any(|l| l == "Quiet for 2 days. Stranger has 1 open blocker on #1200."), "{lines:?}");
        assert_eq!(lines.iter().filter(|l| l.contains("ready for you to look again")).count(), 1, "{lines:?}");
        let fixed_mark = WorkLedger::load().unwrap().follow_changed(&machine, None).into_iter().find(|f| f.follow.number == 1111).and_then(|f| f.fixed);
        assert!(fixed_mark.is_some(), "the mark goes with the follow-up");
        glasscode_core::store::update::<EngineSwitch, ()>(|s| s.engine = Engine::V1).unwrap();
    }
}
