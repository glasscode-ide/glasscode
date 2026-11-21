//! The gate's tests (M21.1). The ops of other slices are made here as specs of their own, with
//! the group, effect and v1 kind the registry gives them.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::caller::Lane;
use crate::clock::Clock;
use crate::control::Kind;
use crate::ops::{EffectClass, OpGroup, OpHost, OpSpec, Retry, Undo};
use crate::store::Store;
use crate::paths::MachineId;
use crate::testkit::home::Home;

const fn spec(name: &'static str, group: OpGroup, effect: EffectClass, kind: Kind) -> OpSpec {
    OpSpec { name, group, effect, kind_v1: kind, retry: Retry::AtMostOnce, undo: Undo::None, host: OpHost::SessionHost, summary: "do it", ..OpSpec::READ }
}

static SESSION_NEW: OpSpec = spec("session.new", OpGroup::SessionStart, EffectClass::Create, Kind::Start);
static SESSION_SEND: OpSpec = spec("session.send", OpGroup::SessionInput, EffectClass::SessionInput, Kind::Send);
static SESSION_CLOSE: OpSpec = spec("session.close", OpGroup::SessionControl, EffectClass::Destructive, Kind::Close);
static SESSION_MODE: OpSpec = spec("session.mode", OpGroup::SessionControl, EffectClass::LocalWrite, Kind::Close);
static SESSION_MOVE: OpSpec = spec("session.move", OpGroup::Worktree, EffectClass::Create, Kind::Move);
static PR_COMMENT: OpSpec = spec("pr.comment", OpGroup::ForgeWrite, EffectClass::Outward, Kind::Github);
static SETTINGS_SET: OpSpec = spec("project.set_account", OpGroup::Settings, EffectClass::LocalWrite, Kind::Settings);
static APPROVALS_DECIDE: OpSpec = OpSpec { human_only: true, ..spec("approvals.decide", OpGroup::Settings, EffectClass::LocalWrite, Kind::Settings) };
static RELEASE: OpSpec = OpSpec { human_only: true, ..spec("manager.release", OpGroup::Manager, EffectClass::LocalWrite, Kind::Board) };
static PAUSE_SPEC: OpSpec = spec("manager.pause", OpGroup::Manager, EffectClass::LocalWrite, Kind::Board);
static BOARD_ADD: OpSpec = spec("board.add", OpGroup::Board, EffectClass::LocalWrite, Kind::Board);
static SESSION_LIST: OpSpec = spec("session.list", OpGroup::Read, EffectClass::Read, Kind::Read);

fn op(spec: &'static OpSpec, args: serde_json::Value) -> Op {
    Op { spec, args }
}

fn key(text: &str) -> ProjectKey {
    ProjectKey::parse(text).unwrap()
}

fn careful() -> ProjectKey {
    key("work-3f9a1c")
}

fn autopilot() -> ProjectKey {
    key("side-5c7e21")
}

fn manager(lane: Lane) -> Caller {
    Caller::Manager { lane, wake: None, project: None }
}

fn session(id: &str) -> Caller {
    Caller::Session { id: id.into(), project: None }
}

fn facts(project: &ProjectKey, origin: SessionOrigin, started_by: Option<&str>) -> SessionFacts {
    SessionFacts { project: Some(project.clone()), origin, started_by: started_by.map(str::to_string), branch: Some("feat/x".into()), mode: Some("default".into()), account: None }
}

/// Two projects, one Careful and one Autopilot; a person's session in each, and a session the
/// manager started in the Autopilot one.
fn set() -> PolicySet {
    let mut projects = BTreeMap::new();
    projects.insert(careful(), ProjectPolicy { level: Level::Careful, ..ProjectPolicy::default() });
    projects.insert(autopilot(), ProjectPolicy { level: Level::Autopilot, ..ProjectPolicy::default() });
    let mut sessions = BTreeMap::new();
    sessions.insert("s-work".to_string(), facts(&careful(), SessionOrigin::Person, None));
    sessions.insert("s-side".to_string(), facts(&autopilot(), SessionOrigin::Person, None));
    sessions.insert("s-auto".to_string(), facts(&autopilot(), SessionOrigin::Automation, Some("manager")));
    sessions.insert("s-work-auto".to_string(), facts(&careful(), SessionOrigin::Automation, Some("manager")));
    PolicySet { policy: Policy { default: ProjectPolicy::default(), projects }, sessions, home: "/home/dev".into(), ..PolicySet::default() }
}

fn on(projects: &[ProjectKey]) -> Targets {
    Targets { projects: projects.to_vec(), ..Targets::default() }
}

fn rule(caller: &Caller, op: &Op, targets: &Targets, set: &PolicySet) -> Rule {
    decide(caller, op, targets, set, None).rule()
}

#[test]
fn careful_manager_session_new_asks() {
    let set = set();
    let new = op(&SESSION_NEW, json!({}));
    for lane in [Lane::Chat, Lane::Wake] {
        let decision = decide(&manager(lane), &new, &on(&[careful()]), &set, None);
        assert_eq!(decision.rule(), Rule::Ask, "{decision:?}");
        assert!(decision.why().contains("Careful") && decision.why().contains("starts a session"), "{}", decision.why());
    }
    // An automation session in a Careful project does not start sessions at all ("—").
    assert_eq!(rule(&session("s-work-auto"), &new, &on(&[careful()]), &set), Rule::Never);
}

#[test]
fn autopilot_manager_session_new_allows() {
    let set = set();
    let new = op(&SESSION_NEW, json!({}));
    for lane in [Lane::Chat, Lane::Wake] {
        assert_eq!(decide(&manager(lane), &new, &on(&[autopilot()]), &set, None), Decision::Allow);
    }
    // Standard allows it too; a project the policy does not name is the default, Careful.
    let mut standard = set.clone();
    standard.policy.default.level = Level::Standard;
    assert_eq!(rule(&manager(Lane::Wake), &new, &on(&[key("other-0a0a0a")]), &standard), Rule::Allow);
    assert_eq!(rule(&manager(Lane::Wake), &new, &on(&[key("other-0a0a0a")]), &set), Rule::Ask);
    // An op that touches both: the strictest wins.
    assert_eq!(rule(&manager(Lane::Wake), &new, &on(&[autopilot(), careful()]), &set), Rule::Ask);
}

#[test]
fn human_only_refused_to_manager() {
    let set = set();
    let ops = [op(&APPROVALS_DECIDE, json!({ "id": "p1", "allow": true })), op(&RELEASE, json!({}))];
    // The real registry's person-only op too.
    let recent = op(crate::ops::find("words.recent").unwrap(), json!({}));
    assert!(recent.spec.human_only);
    for op in ops.iter().chain([&recent]) {
        for caller in [manager(Lane::Chat), manager(Lane::Wake), session("s-side"), session("s-auto"), Caller::Terminal { pid: Some(7) }] {
            let decision = decide(&caller, op, &on(&[autopilot()]), &set, None);
            assert_eq!(decision.rule(), Rule::Never, "{caller:?} {}", op.name());
        }
        // A grant cannot lift it either.
        let grant = Grant { message: serde_json::from_value(json!("00")).unwrap(), quote: (0, 2), op_hash: op_hash(op), targets: on(&[autopilot()]), turn: "t".into() };
        assert_eq!(decide(&manager(Lane::Chat), op, &on(&[autopilot()]), &set, Some(&grant)).rule(), Rule::Never);
        // The person in the window may; in a shell tab it is confirmed in the app.
        assert_eq!(decide(&Caller::Ui, op, &Targets::default(), &set, None), Decision::Allow);
        assert_eq!(rule(&Caller::ShellTab { session: "s".into(), project: None }, op, &Targets::default(), &set), Rule::Ask);
    }
}

#[test]
fn condition_branch_blocks_default() {
    let mut set = set();
    let send = op(&SESSION_SEND, json!({ "session": "s-auto", "message": "go on" }));
    let targets = Targets { sessions: vec!["s-auto".into()], ..Targets::default() };
    // Autopilot allows the manager to type into a session...
    assert_eq!(rule(&manager(Lane::Wake), &send, &targets, &set), Rule::Allow);
    // ...but not on a branch the policy names.
    set.policy.projects.get_mut(&autopilot()).unwrap().conditions.push(Condition::Branch { not: vec!["feat/*".into()] });
    let decision = decide(&manager(Lane::Wake), &send, &targets, &set, None);
    assert_eq!(decision.rule(), Rule::Ask);
    assert!(decision.why().contains("feat/x"), "{}", decision.why());
    // Reads pass, and the person is never held by a condition.
    let list = op(&SESSION_LIST, json!({}));
    assert_eq!(rule(&manager(Lane::Wake), &list, &targets, &set), Rule::Allow);
    assert_eq!(rule(&Caller::Ui, &send, &targets, &set), Rule::Allow);
    // A path the policy keeps out of reach is refused; hours outside the allowed ones ask.
    set.policy.projects.get_mut(&autopilot()).unwrap().conditions = vec![Condition::Paths { deny: vec!["vault/**".into()] }];
    let vault = Targets { paths: vec!["/home/dev/side/vault/keys".into()], projects: vec![autopilot()], ..Targets::default() };
    assert_eq!(rule(&manager(Lane::Wake), &op(&SESSION_NEW, json!({})), &vault, &set), Rule::Never);
    set.policy.projects.get_mut(&autopilot()).unwrap().conditions = vec![Condition::Hours { local: (9 * 60, 17 * 60), days: 0 }];
    let clock = Arc::new(Fixed(AtomicU64::new(20 * 3_600_000)));
    let _clock = crate::clock::use_test_clock(clock.clone());
    assert_eq!(rule(&manager(Lane::Wake), &op(&SESSION_NEW, json!({})), &on(&[autopilot()]), &set), Rule::Ask);
    clock.0.store(10 * 3_600_000, Ordering::Relaxed);
    assert_eq!(rule(&manager(Lane::Wake), &op(&SESSION_NEW, json!({})), &on(&[autopilot()]), &set), Rule::Allow);
}

use std::sync::atomic::{AtomicU64, Ordering};

struct Fixed(AtomicU64);

impl Clock for Fixed {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    fn local_minutes(&self, at_ms: u64) -> Option<i64> {
        Some((at_ms / 60_000) as i64)
    }
}

#[test]
fn unpaired_phone_key_refused() {
    let mut set = set();
    let add = op(&BOARD_ADD, json!({ "title": "x" }));
    let phone = Caller::Phone { device: "phone-1".into() };
    let decision = decide(&phone, &add, &Targets::default(), &set, None);
    assert_eq!(decision.rule(), Rule::Never);
    assert!(decision.why().contains("not paired"));
    // A phone this Mac paired reads and confirms an approval itself; anything else only the person
    // may do asks in the app, and the rest it asks the manager for (G06).
    set.devices.insert("phone-1".into());
    assert_eq!(decide(&phone, &op(&SESSION_LIST, json!({})), &Targets::default(), &set, None), Decision::Allow);
    assert_eq!(decide(&phone, &op(&APPROVALS_DECIDE, json!({})), &Targets::default(), &set, None), Decision::Allow);
    assert_eq!(rule(&phone, &op(&RELEASE, json!({})), &Targets::default(), &set), Rule::Ask);
    assert_eq!(rule(&phone, &add, &Targets::default(), &set), Rule::Never);
    // Another phone is still not paired.
    assert_eq!(rule(&Caller::Phone { device: "phone-2".into() }, &add, &Targets::default(), &set), Rule::Never);
}

/// G06: a paired phone is narrower than the person. It does not start, drive, move or close a
/// session, write settings or act on the forge by itself; the manager does, within the level.
#[test]
fn a_paired_phone_never_acts_as_the_person() {
    let mut set = set();
    set.devices.insert("phone-1".into());
    let phone = Caller::Phone { device: "phone-1".into() };
    for (spec, args) in [
        (&SESSION_NEW, json!({ "mode": "bypassPermissions" })),
        (&SESSION_NEW, json!({})),
        (&SESSION_SEND, json!({ "text": "rebase on main" })),
        (&SESSION_CLOSE, json!({})),
        (&SESSION_MODE, json!({ "mode": "acceptEdits" })),
        (&SESSION_MOVE, json!({})),
        (&PR_COMMENT, json!({ "body": "LGTM" })),
        (&SETTINGS_SET, json!({})),
        (&BOARD_ADD, json!({ "title": "x" })),
    ] {
        let decision = decide(&phone, &op(spec, args), &Targets::default(), &set, None);
        assert_eq!(decision.rule(), Rule::Never, "{}", spec.name);
        assert!(decision.why().contains("ask the manager"), "{}: {}", spec.name, decision.why());
    }
}

#[test]
fn phone_cannot_start_a_bypass_session() {
    let mut set = set();
    set.devices.insert("phone-1".into());
    let decision = decide(&Caller::Phone { device: "phone-1".into() }, &op(&SESSION_NEW, json!({ "mode": "bypassPermissions" })), &Targets::default(), &set, None);
    assert_eq!(decision.rule(), Rule::Never);
}

/// The rules `glasscode whoami` lists agree with the gate: a phone reads, and nothing more.
#[test]
fn the_rules_list_agrees_for_a_phone() {
    let settings = crate::control::AgentSettings::default();
    let phone = Caller::Phone { device: "phone-1".into() };
    assert_eq!(crate::caller::rule(&phone, Kind::Read, &settings), settings.rule(Kind::Read));
    for kind in [Kind::Send, Kind::Start, Kind::Close, Kind::Board, Kind::Github, Kind::Settings] {
        assert_eq!(crate::caller::rule(&phone, kind, &settings), Rule::Never, "{kind:?}");
    }
}

#[test]
fn mode_never_raised() {
    let set = set();
    let caller = manager(Lane::Wake);
    let to = on(&[autopilot()]);
    for (mode, allowed) in [("default", true), ("plan", true), ("read-only", true), ("auto", false), ("full-auto", false), ("accept-edits", false), ("bypass", false), ("yolo-2", false)] {
        let new = op(&SESSION_NEW, json!({ "mode": mode }));
        assert_eq!(rule(&caller, &new, &to, &set) == Rule::Allow, allowed, "{mode}");
        if !allowed {
            assert_eq!(rule(&session("s-side"), &new, &to, &set), Rule::Never, "a session may not either: {mode}");
        }
    }
    // A project whose own default mode is auto lets the manager start auto sessions; bypass never.
    let mut auto = set.clone();
    auto.project_modes.insert(autopilot(), "auto".into());
    assert_eq!(rule(&caller, &op(&SESSION_NEW, json!({ "mode": "auto" })), &to, &auto), Rule::Allow);
    auto.project_modes.insert(autopilot(), "bypass".into());
    assert_eq!(rule(&caller, &op(&SESSION_NEW, json!({ "mode": "bypass" })), &to, &auto), Rule::Never);
    // A running session's mode: only down, never up.
    let at = Targets { sessions: vec!["s-auto".into()], ..Targets::default() };
    assert_eq!(rule(&caller, &op(&SESSION_MODE, json!({ "session": "s-auto", "mode": "plan" })), &at, &set), Rule::Allow);
    assert_eq!(rule(&caller, &op(&SESSION_MODE, json!({ "session": "s-auto", "mode": "auto" })), &at, &set), Rule::Never);
    // The person may.
    assert_eq!(rule(&Caller::Ui, &op(&SESSION_NEW, json!({ "mode": "bypass" })), &to, &set), Rule::Allow);
}

/// trust-3: the rule is the strictest over the caller's own project and every target's.
#[test]
fn another_projects_target_takes_its_rule() {
    let set = set();
    let new = op(&SESSION_NEW, json!({}));
    // A session the manager drives in the Autopilot project names the Careful one: it asks (its
    // own project's rule would allow, the target's does not).
    assert_eq!(rule(&session("s-auto"), &new, &on(&[careful()]), &set), Rule::Never, "Careful: an automation session does not start sessions");
    let send = op(&SESSION_SEND, json!({ "session": "s-work" }));
    let to_work = Targets { sessions: vec!["s-work".into()], ..Targets::default() };
    assert_eq!(rule(&manager(Lane::Wake), &send, &to_work, &set), Rule::Ask, "the target session's project is Careful");
    // A session the person started, in the Autopilot project, names the Careful one: it asks
    // there, though its own rules would allow it (trust-3).
    assert_eq!(rule(&session("s-side"), &new, &on(&[careful()]), &set), Rule::Ask);
    assert_eq!(rule(&session("s-side"), &new, &on(&[autopilot()]), &set), Rule::Allow);
    // A Caller's project is the app's record of it, never what the caller says.
    let lying = Caller::Session { id: "s-work-auto".into(), project: Some(autopilot()) };
    assert_eq!(rule(&lying, &new, &Targets::default(), &set), Rule::Never);
    // A server session the app has no record of is in the strictest of the server's projects.
    let mut server = set.clone();
    server.server_projects = vec![autopilot(), careful()];
    assert_eq!(rule(&session("server:abc"), &new, &Targets::default(), &server), Rule::Never);
    server.server_projects = vec![autopilot()];
    assert_eq!(rule(&session("server:abc"), &new, &Targets::default(), &server), Rule::Allow);
    // A session the app knows nothing of counts as driven by the manager, in every project.
    assert_eq!(rule(&session("nobody"), &new, &Targets::default(), &set), Rule::Never);
}

/// `session.child` (M56 row 7): the gate's decision on it weighs the caller's Start rule (a
/// person's session keeps the person's own rule), so its child's start runs on that decision and
/// is not asked again.
#[test]
fn a_child_session_is_decided_by_the_start_rule() {
    let mut set = set();
    let child = op(crate::ops::find("session.child").unwrap(), json!({}));
    assert_eq!(rule(&session("s-work"), &child, &Targets::default(), &set), Rule::Allow);
    set.person_rules.set(Kind::Start, Rule::Ask);
    assert_eq!(rule(&session("s-work"), &child, &Targets::default(), &set), Rule::Ask);
    set.person_rules.set(Kind::Start, Rule::Never);
    assert_eq!(rule(&session("s-work"), &child, &Targets::default(), &set), Rule::Never);
    // For every caller and every Start rule it decides as `session.new` would: the
    // person, a person's session, a session the manager drives, the manager, a terminal, in a
    // Careful and in an Autopilot project.
    let new = op(&SESSION_NEW, json!({}));
    let callers = [Caller::Ui, session("s-work"), session("s-side"), session("s-auto"), session("s-work-auto"), manager(Lane::Wake), manager(Lane::Chat), Caller::Terminal { pid: None }];
    for start in [Rule::Allow, Rule::Ask, Rule::Never] {
        set.person_rules.set(Kind::Start, start);
        for caller in &callers {
            for targets in [Targets::default(), on(&[careful()]), on(&[autopilot()])] {
                // (The same rule; an ask's words name the op.)
                assert_eq!(rule(caller, &child, &targets, &set), rule(caller, &new, &targets, &set), "{caller:?} at Start {start:?} on {targets:?}");
            }
        }
    }
    // The person's own ops in the app, before the approved authority is read: allowed.
    assert_eq!(rule(&Caller::Ui, &op(&BOARD_ADD, json!({})), &Targets::default(), &PolicySet::default()), Rule::Allow);
}

#[test]
fn person_sessions_keep_their_settings() {
    let mut set = set();
    let new = op(&SESSION_NEW, json!({}));
    // Settings › Permissions: start is Allow by default, close asks, settings never.
    assert_eq!(rule(&session("s-work"), &new, &Targets::default(), &set), Rule::Allow);
    assert_eq!(rule(&session("s-work"), &op(&SESSION_CLOSE, json!({})), &Targets::default(), &set), Rule::Ask);
    assert_eq!(rule(&session("s-work"), &op(&SETTINGS_SET, json!({})), &Targets::default(), &set), Rule::Never);
    set.person_rules.set(Kind::Start, Rule::Ask);
    assert_eq!(rule(&session("s-work"), &new, &Targets::default(), &set), Rule::Ask);
    // The project's own never list holds for them too.
    set.policy.projects.get_mut(&careful()).unwrap().never.push("board".into());
    assert_eq!(rule(&session("s-work"), &op(&BOARD_ADD, json!({})), &Targets::default(), &set), Rule::Never);
    // A program outside Glasscode reads, and the rest asks.
    let terminal = Caller::Terminal { pid: None };
    assert_eq!(rule(&terminal, &op(&SESSION_LIST, json!({})), &Targets::default(), &set), Rule::Allow);
    assert_eq!(rule(&terminal, &op(&BOARD_ADD, json!({})), &Targets::default(), &set), Rule::Ask);
}

#[test]
fn the_table_for_the_manager() {
    let mut set = set();
    let wake = manager(Lane::Wake);
    let ap = on(&[autopilot()]);
    // Settings never, at every level; messages and the browser ask at every level.
    assert_eq!(rule(&wake, &op(&SETTINGS_SET, json!({})), &ap, &set), Rule::Never);
    // Forge writes on Autopilot only where the forge carries the wall.
    let comment = op(&PR_COMMENT, json!({ "pr": 3, "text": "hi" }));
    assert_eq!(rule(&wake, &comment, &ap, &set), Rule::Ask);
    set.forge.insert(autopilot(), ForgeFacts { review_required: true, no_force_push: true, no_bypass: false, read_ms: 1 });
    assert_eq!(rule(&wake, &comment, &ap, &set), Rule::Ask);
    set.forge.insert(autopilot(), ForgeFacts { review_required: true, no_force_push: true, no_bypass: true, read_ms: 1 });
    assert_eq!(rule(&wake, &comment, &ap, &set), Rule::Allow);
    // Standard: typing into a session it started, yes; into another, it asks.
    let mut standard = set.clone();
    standard.policy.projects.get_mut(&autopilot()).unwrap().level = Level::Standard;
    let send = op(&SESSION_SEND, json!({}));
    assert_eq!(rule(&wake, &send, &Targets { sessions: vec!["s-auto".into()], ..Targets::default() }, &standard), Rule::Allow);
    assert_eq!(rule(&wake, &send, &Targets { sessions: vec!["s-side".into()], ..Targets::default() }, &standard), Rule::Ask);
    // A move to the server asks at every level.
    assert_eq!(rule(&wake, &op(&SESSION_MOVE, json!({})), &ap, &set), Rule::Ask);
    // An explicit ask in the policy wins over Autopilot's allow.
    set.policy.projects.get_mut(&autopilot()).unwrap().ask.push("session_start".into());
    assert_eq!(rule(&wake, &op(&SESSION_NEW, json!({})), &ap, &set), Rule::Ask);
}

#[test]
fn a_grant_lifts_an_ask_up_to_the_ceiling() {
    let set = set();
    let new = op(&SESSION_NEW, json!({ "title": "fix login" }));
    let to = on(&[careful()]);
    let grant = Grant { message: serde_json::from_value(json!("ab")).unwrap(), quote: (0, 5), op_hash: op_hash(&new), targets: to.clone(), turn: "t1".into() };
    assert_eq!(decide(&manager(Lane::Chat), &new, &to, &set, Some(&grant)), Decision::Allow);
    // Not from the wake lane; not for other arguments; not for a target it does not name.
    assert_eq!(rule_with(&manager(Lane::Wake), &new, &to, &set, &grant), Rule::Ask);
    assert_eq!(rule_with(&manager(Lane::Chat), &op(&SESSION_NEW, json!({ "title": "other" })), &to, &set, &grant), Rule::Ask);
    assert_eq!(rule_with(&manager(Lane::Chat), &new, &on(&[careful(), autopilot()]), &set, &grant), Rule::Ask);
    // Not past a ceiling: a comment always shows a card.
    let comment = op(&PR_COMMENT, json!({ "pr": 3 }));
    let grant = Grant { op_hash: op_hash(&comment), ..grant };
    assert_eq!(rule_with(&manager(Lane::Chat), &comment, &to, &set, &grant), Rule::Ask);
    // The hash does not depend on the order of the arguments.
    assert_eq!(op_hash(&op(&SESSION_NEW, json!({ "a": 1, "b": { "x": 1, "y": 2 } }))), op_hash(&op(&SESSION_NEW, json!({ "b": { "y": 2, "x": 1 }, "a": 1 }))));
}

fn rule_with(caller: &Caller, op: &Op, targets: &Targets, set: &PolicySet, grant: &Grant) -> Rule {
    decide(caller, op, targets, set, Some(grant)).rule()
}

#[test]
fn a_pause_stops_the_manager_and_only_the_person_releases_it() {
    let mut set = set();
    set.pause = Pause { projects: [autopilot()].into(), ..Pause::default() };
    let new = op(&SESSION_NEW, json!({}));
    assert_eq!(rule(&manager(Lane::Wake), &new, &on(&[autopilot()]), &set), Rule::Never);
    assert_eq!(rule(&session("s-auto"), &new, &on(&[autopilot()]), &set), Rule::Never, "the sessions it drives stop too");
    // Reads go on; pausing again is fine; the person's own sessions go on.
    assert_eq!(rule(&manager(Lane::Wake), &op(&SESSION_LIST, json!({})), &on(&[autopilot()]), &set), Rule::Allow);
    assert_eq!(rule(&manager(Lane::Wake), &op(&PAUSE_SPEC, json!({})), &on(&[autopilot()]), &set), Rule::Allow);
    assert_eq!(rule(&session("s-side"), &new, &on(&[autopilot()]), &set), Rule::Allow);
    // Releasing is the person's.
    assert_eq!(rule(&manager(Lane::Chat), &op(&RELEASE, json!({})), &Targets::default(), &set), Rule::Never);
    assert_eq!(rule(&Caller::Ui, &op(&RELEASE, json!({})), &Targets::default(), &set), Rule::Allow);
    assert!(Pause::everything().is_paused(None) && !Pause::default().is_paused(Some(&careful())));
    assert!(PolicySet::fail_closed().pause.all);
}

#[test]
fn a_project_that_stays_on_this_mac() {
    let mut set = set();
    set.policy.projects.get_mut(&careful()).unwrap().local_only = true;
    let to_server = op(&SESSION_MOVE, json!({ "session": "s-work" }));
    let at = Targets { sessions: vec!["s-work".into()], ..Targets::default() };
    // A hard limit: the person too.
    for caller in [Caller::Ui, manager(Lane::Chat), Caller::Machine { id: crate::paths::MachineId::parse("0123456789abcdef").unwrap() }] {
        assert_eq!(rule(&caller, &to_server, &at, &set), Rule::Never, "{caller:?}");
    }
    let back = op(&SESSION_MOVE, json!({ "session": "s-work", "to_this_computer": true }));
    assert_eq!(rule(&Caller::Ui, &back, &at, &set), Rule::Allow);
    assert_eq!(rule(&Caller::Ui, &op(&SESSION_NEW, json!({ "on_server": true })), &on(&[careful()]), &set), Rule::Never);
    // Another project's session cannot read it; its own may, and the manager may.
    let list = op(&SESSION_LIST, json!({}));
    assert_eq!(rule(&session("s-side"), &list, &on(&[careful()]), &set), Rule::Never);
    assert_eq!(rule(&session("s-work"), &list, &on(&[careful()]), &set), Rule::Allow);
    assert_eq!(rule(&manager(Lane::Wake), &list, &on(&[careful()]), &set), Rule::Allow);
}

/// D17: which account's model may read a project.
#[test]
fn only_the_projects_account_reads_it() {
    let mut set = set();
    set.policy.projects.get_mut(&careful()).unwrap().model_account = Some("work".into());
    let list = op(&SESSION_LIST, json!({}));
    set.manager_account = Some("personal".into());
    assert_eq!(rule(&manager(Lane::Wake), &list, &on(&[careful()]), &set), Rule::Never);
    set.manager_account = Some("work".into());
    assert_eq!(rule(&manager(Lane::Wake), &list, &on(&[careful()]), &set), Rule::Allow);
    // A session on another account may not read it, in another project or in its own; one on the
    // project's account may.
    set.sessions.get_mut("s-side").unwrap().account = Some("personal".into());
    assert_eq!(rule(&session("s-side"), &list, &on(&[careful()]), &set), Rule::Never);
    assert_eq!(rule(&session("s-work"), &list, &on(&[careful()]), &set), Rule::Never);
    set.sessions.get_mut("s-work").unwrap().account = Some("work".into());
    assert_eq!(rule(&session("s-work"), &list, &on(&[careful()]), &set), Rule::Allow);
}

/// D17 holds for a program outside Glasscode too. Any session can be one: it connects to the
/// control socket without its signature (it is "anyone who says so"). So a terminal does not
/// pass for the project's account: it asks, and the person answers in the app.
#[test]
fn a_terminal_is_not_the_projects_account() {
    let mut set = set();
    set.policy.projects.get_mut(&careful()).unwrap().model_account = Some("work".into());
    let list = op(&SESSION_LIST, json!({}));
    let terminal = Caller::Terminal { pid: Some(4242) };
    assert_eq!(rule(&terminal, &list, &on(&[careful()]), &set), Rule::Ask);
    // A project kept to no account reads as before.
    set.policy.projects.get_mut(&careful()).unwrap().model_account = None;
    assert_eq!(rule(&terminal, &list, &on(&[careful()]), &set), Rule::Allow);
}

/// D17 for the manager's reads that name no project (`memory.search`, `memory.show`,
/// `journal.events`, its brief): a project bound to another account is left out of them, as the
/// gate refuses it when named.
#[test]
fn the_manager_reads_no_project_of_another_account() {
    let mut set = set();
    set.policy.projects.get_mut(&careful()).unwrap().model_account = Some("work".into());
    let keys = [careful(), autopilot()];
    set.manager_account = Some("personal".into());
    assert!(!set.manager_may_read(&careful()) && set.manager_may_read(&autopilot()));
    assert_eq!(set.hidden_from_manager(&keys), [careful()]);
    set.manager_account = None;
    assert_eq!(set.hidden_from_manager(&keys), [careful()], "no account of its own: not one bound to an account");
    set.manager_account = Some("work".into());
    assert!(set.hidden_from_manager(&keys).is_empty());
}

#[test]
fn unknown_policy_names_are_found() {
    let policy = ProjectPolicy { never: vec!["board.add".into(), "session_start".into(), "push_default_branch".into()], ask: vec!["forge_write".into(), "pr.merge".into()], ..ProjectPolicy::default() };
    assert_eq!(policy.unknown_names(), ["pr.merge", "push_default_branch"]);
    for group in all_groups() {
        assert!(!group_key(group).contains('.'), "a group's key is never an op's name");
        assert_eq!(serde_json::to_value(group).unwrap(), json!(group_key(group)));
    }
}

// ----- the authority store -----------------------------------------------------------------------

fn approved_policy() -> Policy {
    let mut projects = BTreeMap::new();
    projects.insert(careful(), ProjectPolicy { level: Level::Careful, never: vec!["session_start".into()], local_only: true, ..ProjectPolicy::default() });
    projects.insert(autopilot(), ProjectPolicy { level: Level::Autopilot, allow_commands: vec!["cargo test".into()], ..ProjectPolicy::default() });
    Policy { default: ProjectPolicy { level: Level::Standard, ..ProjectPolicy::default() }, projects }
}

#[test]
fn changed_policy_counts_as_careful() {
    let _home = Home::new();
    let policy = approved_policy();
    approve(&policy).unwrap();
    let read = load();
    assert!(!read.live_differs && read.problems.is_empty(), "{:?}", read.problems);
    assert_eq!(read.policy, policy);
    // A program outside the app raises the Careful project to Autopilot and drops its never.
    let mut edited = policy.clone();
    let work = edited.projects.get_mut(&careful()).unwrap();
    work.level = Level::Autopilot;
    work.never.clear();
    work.local_only = false;
    PolicyFile(edited.clone()).save().unwrap();
    let read = load();
    assert!(read.live_differs);
    // The snapshot, at most Careful: the approved never and "stays on this Mac" survive, the
    // levels and allow-commands do not.
    let effective = &read.policy;
    assert_eq!(effective.of(&careful()).never, ["session_start"]);
    assert!(effective.of(&careful()).local_only);
    assert!(Level::ALL.iter().all(|_| effective.projects.values().all(|p| p.level == Level::Careful)));
    assert_eq!(effective.default.level, Level::Careful);
    assert!(effective.of(&autopilot()).allow_commands.is_empty());
    let set = read.policy_set(Default::default());
    assert_eq!(rule(&manager(Lane::Wake), &op(&SESSION_NEW, json!({})), &on(&[autopilot()]), &set), Rule::Ask);
    // The import screen says what changed, in plain words; importing what was seen approves it.
    let lines = diff(read.approved.as_ref().unwrap(), read.live.as_ref().unwrap());
    assert!(lines.contains(&"work-3f9a1c: Trust level: Careful to Autopilot".to_string()), "{lines:?}");
    assert!(lines.contains(&"work-3f9a1c: never: no longer session_start".to_string()), "{lines:?}");
    assert!(lines.contains(&"work-3f9a1c: no longer stays on this Mac".to_string()), "{lines:?}");
    let mut other = edited.clone();
    other.default.level = Level::Autopilot;
    assert!(import_live(&other).is_err(), "only what the person saw");
    import_live(&edited).unwrap();
    let read = load();
    assert!(!read.live_differs);
    assert_eq!(read.policy.of(&careful()).level, Level::Autopilot);
    // A deleted live file counts as a change too; a snapshot edited by hand no longer matches its
    // hash and counts as nothing approved.
    std::fs::remove_file(PolicyFile::path()).unwrap();
    assert!(load().live_differs);
    approve(&policy).unwrap();
    let mut snapshot = ApprovedPolicy::load().unwrap();
    snapshot.policy.default.level = Level::Autopilot;
    snapshot.save().unwrap();
    let mut live = policy.clone();
    live.default.level = Level::Autopilot;
    PolicyFile(live).save().unwrap();
    let read = load();
    assert_eq!(read.policy.default.level, Level::Careful);
    assert!(read.policy.projects.is_empty() && !read.problems.is_empty());
    // A policy that names nothing real is not approved.
    let mut bad = policy.clone();
    bad.default.never.push("push_default_branch".into());
    assert!(approve(&bad).is_err());
}

#[test]
fn policy_never_reads_prev() {
    let _home = Home::new();
    // An older, looser generation, then the stricter one.
    let mut loose = approved_policy();
    loose.default.level = Level::Autopilot;
    approve(&loose).unwrap();
    approve(&approved_policy()).unwrap();
    for path in [ApprovedPolicy::path(), PolicyFile::path()] {
        let mut prev = path.clone().into_os_string();
        prev.push(".prev");
        assert!(std::path::Path::new(&prev).is_file(), "the older generation is kept beside it");
    }
    // The snapshot is damaged: nothing comes from its .prev, every project is Careful.
    std::fs::write(ApprovedPolicy::path(), b"{ not json").unwrap();
    let read = load();
    assert_eq!(read.policy.default.level, Level::Careful);
    assert!(read.policy.projects.is_empty());
    assert!(!read.problems.is_empty());
    assert!(ApprovedPolicy::load().is_err());
    // The live file damaged beside a whole snapshot: the snapshot at most Careful.
    approve(&approved_policy()).unwrap();
    std::fs::write(PolicyFile::path(), b"[]").unwrap();
    let read = load();
    assert!(read.live_differs);
    assert_eq!(read.policy.of(&careful()).never, ["session_start"]);
    assert_eq!(read.policy.default.level, Level::Careful);
    // The pause and the devices fail closed too.
    std::fs::create_dir_all(Pause::path().parent().unwrap()).unwrap();
    std::fs::write(Pause::path(), b"x").unwrap();
    std::fs::write(Devices::path(), b"x").unwrap();
    let read = load();
    assert!(read.pause.all && read.devices.is_empty());
}

#[test]
fn devices_and_pause_are_kept() {
    let _home = Home::new();
    pin_device("phone-1", "ssh-ed25519 AAAA test", "Test phone").unwrap();
    pin_device("phone-2", "ssh-ed25519 BBBB test", "Other").unwrap();
    unpin_device("phone-2").unwrap();
    assert_eq!(load().devices, ["phone-1".to_string()].into());
    set_pause(|pause| {
        pause.projects.insert(careful());
        pause.by = Some("a session".into());
    })
    .unwrap();
    let read = load();
    assert!(read.pause.is_paused(Some(&careful())) && !read.pause.is_paused(Some(&autopilot())));
    set_pause(|pause| *pause = Pause::default()).unwrap();
    assert!(!load().pause.is_paused(Some(&careful())));
}

/// crit-5: what the server gets, and what it does with an old copy.
#[test]
fn the_servers_copy() {
    let _home = Home::new();
    approve(&approved_policy()).unwrap();
    let read = load();
    let copy = FromMac::of(&read, crate::paths::MachineId::parse("0123456789abcdef"));
    // "Stays on this Mac" projects are not in it.
    assert!(!copy.policy.projects.contains_key(&careful()));
    assert!(copy.policy.projects.contains_key(&autopilot()));
    copy.clone().receive().unwrap();
    let kept = FromMac::load().unwrap();
    let now = crate::clock::now_ms();
    let (policy, stale) = kept.effective(now);
    assert!(!stale);
    assert_eq!(policy.of(&autopilot()).level, Level::Autopilot);
    // Past a day it is at most Careful; one never received is too.
    let (policy, stale) = kept.effective(now + MAX_COPY_AGE_MS + 1);
    assert!(stale && policy.of(&autopilot()).level == Level::Careful);
    assert!(FromMac::default().effective(now).1);
    // A copy edited on the server (raised to Autopilot) no longer matches its hash, and is not
    // kept: the old one stays.
    let mut forged = copy.clone();
    forged.policy.default.level = Level::Autopilot;
    assert!(forged.receive().is_err());
    assert_eq!(FromMac::load().unwrap().policy.default.level, Level::Standard);
    // The server's own pause and the Mac's: the stricter.
    let set = kept.policy_set(&Pause::everything(), now);
    assert!(set.pause.all);
}

#[test]
fn the_policy_md_header() {
    let mut policy = approved_policy().projects[&careful()].clone();
    policy.conditions.push(Condition::Branch { not: vec!["main".into()] });
    policy.model_account = Some("work".into());
    let header = policy_header(&careful(), &policy);
    assert!(header.starts_with("<!-- Generated by Glasscode"));
    for words in ["Trust level: Careful.", "Never, for anyone but you: session_start.", "Asks before anything on the branches main.", "Only the account work may read this project.", "Stays on this Mac", "told to them, not enforced"] {
        assert!(header.contains(words), "{words}: {header}");
    }
}

/// M21.1 step 6: the header is written again in its place; the prose under it stays as it was.
#[test]
fn the_policy_md_header_is_replaced_and_the_prose_kept() {
    let policy = approved_policy().projects[&careful()].clone();
    let header = policy_header(&careful(), &policy);
    let first = with_policy_header(None, &header);
    assert_eq!(first, header);
    let written = format!("{first}\nRun the tests with make test.\n");
    let mut looser = policy.clone();
    looser.level = Level::Standard;
    let again = with_policy_header(Some(&written), &policy_header(&careful(), &looser));
    assert!(again.contains("Trust level: Standard.") && !again.contains("Trust level: Careful."), "{again}");
    assert!(again.ends_with("\nRun the tests with make test.\n"), "{again}");
    assert_eq!(again.matches("<!-- Generated by Glasscode").count(), 1);
    // A file the person wrote before there was a header keeps all of it, after the header.
    let old = with_policy_header(Some("Be careful with the migrations.\n"), &header);
    assert!(old.starts_with("<!-- Generated by Glasscode") && old.ends_with("\n\nBe careful with the migrations.\n"), "{old}");
    assert_eq!(with_policy_header(Some(&old), &header), old, "the same header changes nothing");
}

/// exec-7: a `state.json` of main (0cf333f) keeps its 13 kinds and every field when this build
/// reads and writes it, and v2 reads those kinds as the person-session rules.
#[test]
fn older_state_json_round_trip() {
    let _home = Home::new();
    let old = json!({
        "projects": [],
        "sessions": [],
        "agents": { "rules": { "send": "allow", "remove": "never", "settings": "ask", "start": "ask" }, "outside_allowed": true, "log_off": true },
        "manager": { "name": "Robin" },
        "a_field_of_a_newer_build": { "kept": true }
    });
    let path = crate::paths::root().join("state.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&old).unwrap()).unwrap();
    let state = crate::store::load();
    let rules = &state.agents.rules;
    assert_eq!((rules.get(&Kind::Send), rules.get(&Kind::Remove), rules.get(&Kind::Settings), rules.get(&Kind::Start)), (Some(&Rule::Allow), Some(&Rule::Never), Some(&Rule::Ask), Some(&Rule::Ask)));
    assert!(state.agents.outside_allowed && state.agents.log_off);
    crate::store::save(&state).unwrap();
    let written: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(written["agents"]["rules"], old["agents"]["rules"]);
    assert_eq!(written["agents"]["outside_allowed"], json!(true));
    // v2's person-started sessions read those very rules.
    let mut set = set();
    set.person_rules = state.agents.clone();
    assert_eq!(rule(&session("s-work"), &op(&SESSION_NEW, json!({})), &Targets::default(), &set), Rule::Ask);
    assert_eq!(rule(&session("s-work"), &op(&SESSION_SEND, json!({})), &Targets::default(), &set), Rule::Allow);
}

/// The budget of design section 9: `decide()` ≤ 50 µs.
#[test]
fn decide_is_fast() {
    let set = set();
    let new = op(&SESSION_NEW, json!({ "mode": "default" }));
    let targets = Targets { projects: vec![careful(), autopilot()], sessions: vec!["s-auto".into(), "s-work".into()], ..Targets::default() };
    let started = std::time::Instant::now();
    let n = 2_000;
    for _ in 0..n {
        std::hint::black_box(decide(&manager(Lane::Wake), &new, &targets, &set, None));
    }
    let each = started.elapsed() / n;
    assert!(each < std::time::Duration::from_micros(500), "{each:?} for one decision (debug build)");
}

#[test]
fn a_sessions_origin_is_never_lowered() {
    let _home = Home::new();
    let mut origins = SessionOrigins::default();
    assert_eq!(origins.origin("s1"), SessionOrigin::Person);
    assert!(origins.mark("s1", SessionOrigin::Automation, "manager", 1));
    assert!(!origins.mark("s1", SessionOrigin::Person, "you", 2), "never back to the person's");
    assert!(!origins.mark("s1", SessionOrigin::Automation, "manager", 3));
    assert!(origins.mark("s1", SessionOrigin::Quarantine, "playbook:review", 4));
    assert!(!origins.mark("s1", SessionOrigin::Automation, "manager", 5));
    assert_eq!(origins.origin("s1"), SessionOrigin::Quarantine);
    // Kept on disk; an unreadable store makes every session the manager's.
    assert!(mark_session("s2", SessionOrigin::Automation, "s1").unwrap());
    let read = load();
    assert_eq!((read.origin("s2"), read.origin("s3")), (SessionOrigin::Automation, SessionOrigin::Person));
    std::fs::write(SessionOrigins::path(), b"oops").unwrap();
    let read = load();
    assert_eq!(read.origin("s3"), SessionOrigin::Automation);
    assert!(read.origins.is_none() && !read.problems.is_empty());
}

#[test]
fn device_ids_ignore_the_comment() {
    assert_eq!(device_id("ecdsa-sha2-nistp256 AAAAE2 phone"), device_id("ecdsa-sha2-nistp256 AAAAE2"));
    assert_ne!(device_id("ecdsa-sha2-nistp256 AAAAE2"), device_id("ecdsa-sha2-nistp256 AAAAE3"));
    assert_eq!(device_id("x y").len(), 16);
}

// ----- pending actions and grants (M21.2) ---------------------------------------------------------

fn pending_new(set: &PolicySet, now: u64) -> PendingAction {
    let _ = set;
    let op = op(&BOARD_ADD, json!({ "title": "Ask Jordan about the token" }));
    let mut action = PendingAction::new(&op, on(&[careful()]), Some(manager(Lane::Wake)), now);
    action.who = "The manager".into();
    action.what = "add “Ask Jordan about the token” to Today".into();
    action.reason = "work-3f9a1c is Careful".into();
    action
}

#[test]
fn three_pending_survive_restart() {
    let _home = Home::new();
    let set = set();
    let now = crate::clock::now_ms();
    let ids: Vec<String> = (0..3).map(|_| add_pending(pending_new(&set, now)).unwrap().id).collect();
    // A new process: the store holds them, numbered, each with its op and hash.
    let pending = Pending::load().unwrap();
    let waiting = pending.waiting(now);
    assert_eq!(waiting.iter().map(|a| a.id.clone()).collect::<Vec<_>>(), ids);
    assert_eq!(waiting.iter().map(|a| a.number).collect::<Vec<_>>(), [1, 2, 3]);
    assert!(waiting.iter().all(|a| a.op().is_some_and(|op| op.name() == "board.add") && a.id.len() == 32));
    // An action approved before a quit is uncertain after it, never run again by itself.
    settle_pending(&ids[0], PendingState::Approved, None).unwrap().unwrap();
    let mut after = Pending::load().unwrap();
    assert_eq!(after.after_restart(now), [ids[0].clone()]);
    assert_eq!(after.get(&ids[0]).unwrap().state, PendingState::Uncertain);
    assert!(after.settle(&ids[0], PendingState::Approved, None, now).is_err());
}

#[test]
fn expired_cannot_run() {
    let set = set();
    let now = 1_000_000;
    let mut pending = Pending::default();
    let action = pending.add(pending_new(&set, now), now);
    let later = now + WAIT_MS;
    assert!(action.expired(later, 0));
    assert!(action.recheck(Ok(&action.targets), &Decision::Allow, later, 0).is_err());
    assert!(pending.settle(&action.id, PendingState::Approved, None, later).is_err());
    assert_eq!(pending.get(&action.id).unwrap().state, PendingState::Expired);
    // A session's prompt answer goes stale sooner.
    static ANSWER: OpSpec = spec("session.answer", OpGroup::SessionInput, EffectClass::SessionInput, Kind::Send);
    let answer = PendingAction::new(&op(&ANSWER, json!({})), Targets::default(), None, now);
    assert_eq!(answer.expires_ms - now, ANSWER_WAIT_MS);
    // Settled ones go after a week; waiting ones stay.
    let mut pending = Pending::default();
    let kept = pending.add(pending_new(&set, now), now);
    let gone = pending.add(pending_new(&set, now), now);
    pending.settle(&gone.id, PendingState::Denied, None, now).unwrap();
    pending.prune(now + KEPT_MS + 1);
    assert!(pending.get(&gone.id).is_none());
    assert_eq!(pending.get(&kept.id).unwrap().state, PendingState::Waiting, "prune leaves a waiting one for expire to mark");
}

#[test]
fn approve_runs_stored_args_only() {
    let set = set();
    let now = 1_000_000;
    let mut action = pending_new(&set, now);
    let stored = action.recheck(Ok(&action.targets.clone()), &Decision::Ask { why: "Careful".into() }, now, 0).unwrap();
    assert_eq!(stored.args, json!({ "title": "Ask Jordan about the token" }));
    assert_eq!(op_hash(&stored), action.arg_hash);
    // Arguments changed in the store after the person saw them: the hash no longer matches, and
    // nothing runs.
    action.op["args"]["title"] = json!("Delete everything");
    assert!(action.op().is_none());
    assert!(action.recheck(Ok(&action.targets.clone()), &Decision::Allow, now, 0).is_err());
    // The full payload is shown, every argument, not clipped.
    let long = "x".repeat(5_000);
    let full = PendingAction::new(&op(&BOARD_ADD, json!({ "title": long })), Targets::default(), None, now);
    assert!(full.payload().contains(&"x".repeat(5_000)));
}

#[test]
fn stale_target_reports_and_does_nothing() {
    let set = set();
    let now = 1_000_000;
    let action = pending_new(&set, now);
    let said = action.recheck(Err("The pull request is closed now."), &Decision::Allow, now, 0).unwrap_err();
    assert_eq!(said, "The pull request is closed now. Nothing was done.");
    let moved = on(&[autopilot()]);
    assert!(action.recheck(Ok(&moved), &Decision::Allow, now, 0).unwrap_err().contains("changed"));
    let never = Decision::Never { why: "the manager is paused".into() };
    assert!(action.recheck(Ok(&action.targets), &never, now, 0).unwrap_err().contains("not allowed any more"));
}

#[test]
fn clock_set_back_never_extends_a_pending_expiry() {
    let set = set();
    let now = 10 * WAIT_MS;
    let mut pending = Pending::default();
    let action = pending.add(pending_new(&set, now), now);
    // The clock reached past the expiry once, then went back a day.
    pending.expire(now + WAIT_MS + 1);
    assert_eq!(pending.get(&action.id).unwrap().state, PendingState::Expired);
    let mut pending = Pending::default();
    let action = pending.add(pending_new(&set, now), now);
    pending.prune(now + WAIT_MS + 5);
    let back = now + 1;
    assert!(pending.waiting(back).is_empty(), "the store saw a later time: it stays expired");
    assert!(pending.settle(&action.id, PendingState::Approved, None, back).is_err());
}

/// A ledger record: its spans as given, else the whole text with its own origin (as the ledger
/// writes a typed message).
fn record(origin: crate::words::Origin, text: &str, spans: Vec<(usize, usize, crate::words::Origin)>) -> crate::words::WordsRecord {
    let machine = crate::paths::MachineId::parse("0123456789abcdef").unwrap();
    let spans = if spans.is_empty() {
        vec![(0, text.len(), origin.clone())]
    } else {
        let mut all = Vec::new();
        let mut at = 0;
        for (start, end, o) in spans {
            if start > at {
                all.push((at, start, origin.clone()));
            }
            all.push((start, end, o));
            at = end;
        }
        if at < text.len() {
            all.push((at, text.len(), origin.clone()));
        }
        all
    };
    crate::words::WordsRecord { id: serde_json::from_value(json!("w1")).unwrap(), at: crate::identity::Hlc::zero(machine), session: None, project: None, origin, text: text.into(), hash: [0; 32], spans, measured: false }
}

#[test]
fn grant_quote_must_be_tier_one() {
    use crate::words::{ComposerKind, Origin};
    let new = op(&SESSION_NEW, json!({ "title": "fix login" }));
    let to = on(&[careful()]);
    let mut grants = ChatGrants::default();
    grants.start_turn("t1");
    for origin in [Origin::Voice, Origin::Peer("s1".into()), Origin::TerminalKeys, Origin::Device("phone".into()), Origin::Pasted] {
        let message = record(origin.clone(), "start a session to fix login", Vec::new());
        let proposal = Proposal { op: &new, targets: &to, message: &message, quote: "start a session", turn: "t1" };
        assert_eq!(grants.grant(&proposal, &to), Err(Refused::NotTheirWords), "{origin:?}");
    }
    // Typed in the chat: a grant; a quote that is not in it, or only in a pasted span, is not.
    let typed = record(Origin::Composer(ComposerKind::ManagerChat), "start a session to fix login", Vec::new());
    assert_eq!(grants.grant(&Proposal { op: &new, targets: &to, message: &typed, quote: "merge it", turn: "t1" }, &to), Err(Refused::QuoteNotFound));
    let pasted = record(Origin::Composer(ComposerKind::ManagerChat), "look: > yes, merge it", vec![(6, 21, Origin::Pasted)]);
    assert_eq!(grants.grant(&Proposal { op: &new, targets: &to, message: &pasted, quote: "yes, merge it", turn: "t1" }, &to), Err(Refused::QuoteNotFound));
    // A yes asks for nothing itself.
    let yes = record(Origin::Composer(ComposerKind::ManagerChat), "yes", Vec::new());
    assert_eq!(grants.grant(&Proposal { op: &new, targets: &to, message: &yes, quote: "yes", turn: "t1" }, &to), Err(Refused::OnlyYes));
    let grant = grants.grant(&Proposal { op: &new, targets: &to, message: &typed, quote: "start a session", turn: "t1" }, &to).unwrap();
    assert_eq!(decide(&manager(Lane::Chat), &new, &to, &set(), Some(&grant)), Decision::Allow);
}

#[test]
fn grant_single_use() {
    use crate::words::{ComposerKind, Origin};
    let new = op(&SESSION_NEW, json!({}));
    let to = on(&[careful()]);
    let typed = record(Origin::Composer(ComposerKind::ManagerChat), "start a session", Vec::new());
    let mut grants = ChatGrants::default();
    grants.start_turn("t1");
    let proposal = Proposal { op: &new, targets: &to, message: &typed, quote: "start a session", turn: "t1" };
    assert!(grants.grant(&proposal, &to).is_ok());
    assert_eq!(grants.grant(&proposal, &to), Err(Refused::Used));
    // It ends with its turn.
    grants.end_turn();
    let other = op(&SESSION_NEW, json!({ "title": "b" }));
    assert_eq!(grants.grant(&Proposal { op: &other, ..proposal.clone() }, &to), Err(Refused::TurnOver));
    grants.start_turn("t2");
    assert_eq!(grants.grant(&Proposal { op: &other, ..proposal }, &to), Err(Refused::TurnOver), "a grant of t1 in t2");
}

#[test]
fn grant_targets_must_be_resolved_from_message() {
    use crate::words::{ComposerKind, Origin};
    let send = op(&SESSION_SEND, json!({ "session": "s-auto", "message": "rebase" }));
    let typed = record(Origin::Composer(ComposerKind::ManagerChat), "tell the s-auto session to rebase", Vec::new());
    let mut grants = ChatGrants::default();
    grants.start_turn("t1");
    let named = Targets { sessions: vec!["s-auto".into()], ..Targets::default() };
    let other = Targets { sessions: vec!["s-work".into()], ..Targets::default() };
    assert_eq!(grants.grant(&Proposal { op: &send, targets: &other, message: &typed, quote: "tell the s-auto session to rebase", turn: "t1" }, &named), Err(Refused::TargetNotNamed));
    assert!(grants.grant(&Proposal { op: &send, targets: &named, message: &typed, quote: "tell the s-auto session to rebase", turn: "t1" }, &named).is_ok());
}

#[test]
fn yes_with_two_open_runs_nothing() {
    use crate::words::{ComposerKind, Origin};
    let typed = Origin::Composer(ComposerKind::ManagerChat);
    assert!(yes_binds(&typed, "yes", 1));
    assert!(yes_binds(&typed, " Go ahead! ", 1));
    assert!(!yes_binds(&typed, "yes", 2));
    assert!(!yes_binds(&typed, "yes", 0));
    assert!(!yes_binds(&typed, "yes, and also delete the branch", 1));
}

#[test]
fn spoken_yes_never_binds() {
    use crate::words::Origin;
    for origin in [Origin::Voice, Origin::Device("phone".into()), Origin::TerminalKeys, Origin::Pasted, Origin::Peer("s".into())] {
        assert!(!yes_binds(&origin, "yes", 1), "{origin:?}");
    }
}

#[test]
fn short_expiry_dropped_on_restart_and_wake() {
    let send = op(&SESSION_SEND, json!({ "session": "s-auto" }));
    let to = Targets { sessions: vec!["s-auto".into()], ..Targets::default() };
    let other = Targets { sessions: vec!["s-work".into()], ..Targets::default() };
    let mut grants = SessionGrants::default();
    grants.allow("s-auto", &send, &to, 60_000, 1_000_000, 5_000);
    assert!(grants.covers("s-auto", &send, &to, 1_010_000, 15_000));
    // This op on this target only, for this session only, never the kind.
    assert!(!grants.covers("s-auto", &send, &other, 1_010_000, 15_000));
    assert!(!grants.covers("s-work", &send, &to, 1_010_000, 15_000));
    assert!(!grants.covers("s-auto", &op(&SESSION_NEW, json!({})), &to, 1_010_000, 15_000));
    // Past its monotonic time it is gone.
    assert!(!grants.covers("s-auto", &send, &to, 1_070_000, 75_000));
    // A wake: the wall clock ran an hour while the monotonic one stood still; every grant goes.
    grants.allow("s-auto", &send, &to, 3_600_000, 1_070_000, 75_000);
    assert!(grants.covers("s-auto", &send, &to, 1_071_000, 76_000));
    assert!(!grants.covers("s-auto", &send, &to, 1_071_000 + 3_600_000, 77_000));
    assert!(grants.is_empty());
    // A clock set back is the same gap.
    grants.allow("s-auto", &send, &to, 3_600_000, 5_000_000, 100_000);
    assert!(!grants.covers("s-auto", &send, &to, 4_000_000, 101_000));
    // A restart: a new value, nothing in it (they are never on disk).
    assert!(SessionGrants::default().is_empty());
}

#[test]
fn always_allow_in_a_project() {
    let mut set = set();
    let new = op(&SESSION_NEW, json!({}));
    let wake = manager(Lane::Wake);
    assert_eq!(rule(&wake, &new, &on(&[careful()]), &set), Rule::Ask);
    assert!(can_always_allow(&wake, &new, &set, &careful()));
    set.policy = with_always_allowed(&set.policy, &careful(), "session.new");
    assert_eq!(set.policy.of(&careful()).allow, ["session.new"]);
    assert_eq!(rule(&wake, &new, &on(&[careful()]), &set), Rule::Allow);
    // Not a card that always shows; not what an automation session may never do there.
    let comment = op(&PR_COMMENT, json!({}));
    assert!(!can_always_allow(&wake, &comment, &set, &careful()));
    set.policy = with_always_allowed(&set.policy, &careful(), "pr.comment");
    assert_eq!(rule(&wake, &comment, &on(&[careful()]), &set), Rule::Ask);
    assert_eq!(rule(&session("s-work-auto"), &new, &on(&[careful()]), &set), Rule::Never);
    // A changed file drops it, as it drops allow-commands.
    assert!(set.policy.at_most_careful().of(&careful()).allow.is_empty());
    assert!(diff(&Policy::default(), &set.policy).iter().any(|l| l.contains("always allowed: now session.new")));
}

/// D28 (M23): script or actions in a page of a site the browser is signed in to ask, even for a
/// session the person started; a page with no sign-in follows their rules.
#[test]
fn browser_eval_asks_on_signed_in_site() {
    let set = set();
    let eval = |signed_in: bool| op(crate::ops::find("browser.eval").unwrap(), json!({ "url": "https://github.com/acme/app", "signed_in": signed_in }));
    let act = op(crate::ops::find("browser.act").unwrap(), json!({ "url": "https://github.com/acme/app", "signed_in": true }));
    let read = op(crate::ops::find("browser.read").unwrap(), json!({ "url": "https://github.com/acme/app", "signed_in": true }));
    let person_session = session("s-side");
    assert_eq!(rule(&person_session, &eval(true), &Targets::default(), &set), Rule::Ask);
    assert_eq!(rule(&person_session, &act, &Targets::default(), &set), Rule::Ask);
    assert_eq!(rule(&person_session, &eval(false), &Targets::default(), &set), Rule::Allow);
    assert_eq!(rule(&person_session, &read, &Targets::default(), &set), Rule::Allow, "reading a page asks nothing");
    // The person in the window acts as they like.
    assert_eq!(rule(&Caller::Ui, &eval(true), &Targets::default(), &set), Rule::Allow);
}

/// M23: a session the person does not drive browses only its project's sites, there without a
/// card (its browser has no sign-ins), elsewhere never; a site it is signed in on still asks.
#[test]
fn automation_browses_only_the_projects_sites() {
    let mut set = set();
    set.policy.projects.get_mut(&autopilot()).unwrap().browser_hosts = vec!["docs.rs".into()];
    let act = |url: &str, signed_in: bool| op(crate::ops::find("browser.act").unwrap(), json!({ "url": url, "signed_in": signed_in }));
    let auto = session("s-auto");
    assert_eq!(rule(&auto, &act("https://docs.rs/serde", false), &Targets::default(), &set), Rule::Allow);
    assert_eq!(rule(&auto, &act("https://evil.example/", false), &Targets::default(), &set), Rule::Never);
    assert_eq!(rule(&auto, &act("https://docs.rs/serde", true), &Targets::default(), &set), Rule::Ask);
    // A project that names no site: nothing opens for such a session.
    assert_eq!(rule(&session("s-work-auto"), &act("https://docs.rs/", false), &Targets::default(), &set), Rule::Never);
    // The person's own session is not kept to the list.
    assert_eq!(rule(&session("s-side"), &act("https://evil.example/", false), &Targets::default(), &set), Rule::Allow);
    // Careful takes the list away with the other permissions.
    assert!(set.policy.projects[&autopilot()].at_most_careful().browser_hosts.is_empty());
    assert!(policy_header(&autopilot(), &set.policy.projects[&autopilot()]).contains("may open only these sites, in a browser with no sign-ins: docs.rs"));
}

/// M57: pending actions sync both ways and carry no authority. One asked on the server shows on the
/// Mac and goes only forward; the server takes the person's answer from the Mac; the Mac never
/// takes an answer to its own from the server, nor an action the server says the Mac asked.
#[test]
fn pending_actions_sync_without_authority() {
    let set = PolicySet::default();
    let (mac, server) = (MachineId::parse("00000000000000aa").unwrap(), MachineId::parse("00000000000000bb").unwrap());
    let now = 1_000_000;
    let mut on_server = Pending::default();
    let asked_there = on_server.add(pending_new(&set, now), now);
    let mut on_mac = Pending::default();
    let mine = on_mac.add(pending_new(&set, now), now);
    assert_eq!(asked_there.number, mine.number, "both machines number from one");

    // The server's ask comes to the Mac as a card it may answer but never runs.
    assert_eq!(on_mac.merge_theirs(&on_server.changed_since(0), &server, &mac, false, now), 1);
    let card = on_mac.get(&asked_there.id).unwrap().clone();
    assert!(card.asked_elsewhere(&mac), "it runs on the server");
    assert_ne!(card.number, mine.number, "numbered past the Mac's own");
    assert_eq!(on_mac.merge_theirs(&on_server.changed_since(0), &server, &mac, false, now), 0, "taken once");

    // The person answers it on the Mac: the server takes the answer.
    on_mac.settle(&asked_there.id, PendingState::Approved, Some("You allowed it.".into()), now + 1).unwrap();
    // A restart of the Mac leaves it approved: it runs on the server, which says how it ended.
    let mut restarted = on_mac.clone();
    restarted.after_restart(now + 1);
    assert_eq!(restarted.get(&asked_there.id).unwrap().state, PendingState::Approved);
    assert_eq!(on_server.merge_theirs(&on_mac.changed_since(now), &mac, &server, true, now + 2), 1);
    assert_eq!(on_server.get(&asked_there.id).unwrap().state, PendingState::Approved);
    // Handed back to run on the Mac (an op only the Mac runs): the server takes how it ended there.
    let mut handed = on_server.clone();
    let mut ran_on_mac = on_mac.get(&asked_there.id).unwrap().clone();
    ran_on_mac.state = PendingState::Ran;
    ran_on_mac.outcome = Some("Done on the Mac.".into());
    assert_eq!(handed.merge_theirs(std::slice::from_ref(&ran_on_mac), &mac, &server, true, now + 2), 1);
    assert_eq!(handed.get(&asked_there.id).unwrap().state, PendingState::Ran);
    assert_eq!(on_server.clone().merge_theirs(&[ran_on_mac], &mac, &server, false, now + 2), 0, "only where answers from there count");
    // It ran there; the Mac's card goes forward, never back.
    on_server.settle(&asked_there.id, PendingState::Ran, Some("Done.".into()), now + 3).unwrap();
    assert_eq!(on_mac.merge_theirs(&on_server.changed_since(now), &server, &mac, false, now + 4), 1);
    assert_eq!(on_mac.get(&asked_there.id).unwrap().state, PendingState::Ran);
    let mut stale = on_server.get(&asked_there.id).unwrap().clone();
    stale.state = PendingState::Waiting;
    assert_eq!(on_mac.merge_theirs(&[stale], &server, &mac, false, now + 5), 0, "never back");

    // The Mac's own ask: the server sees it, and nothing the server says answers or adds one.
    assert_eq!(on_server.merge_theirs(&on_mac.changed_since(0), &mac, &server, true, now + 6), 1);
    let mut answered = on_server.get(&mine.id).unwrap().clone();
    answered.state = PendingState::Approved;
    assert_eq!(on_mac.merge_theirs(&[answered], &server, &mac, false, now + 7), 0, "no answer from the server");
    assert_eq!(on_mac.get(&mine.id).unwrap().state, PendingState::Waiting);
    let mut forged = pending_new(&set, now);
    forged.machine = Some(mac.clone());
    assert_eq!(on_mac.merge_theirs(&[forged.clone()], &server, &mac, false, now + 8), 0, "the server cannot add an ask of the Mac's");
    forged.machine = Some(MachineId::parse("00000000000000cc").unwrap());
    assert_eq!(on_mac.merge_theirs(&[forged], &server, &mac, false, now + 8), 0, "nor a third machine's");
}

/// The server has no person to answer a card, so nothing it says is the person's answer on the
/// Mac: a card of its own that it calls approved (new, or one the Mac shows waiting) waits on the
/// Mac until the person answers it there, which is what lets the Mac run a card the server hands
/// back. Its caller is never the person's, whatever the server says; and a record that reuses the
/// id of one of the Mac's own asks changes nothing.
#[test]
fn the_server_cannot_approve_a_card_on_the_mac() {
    let set = PolicySet::default();
    let (mac, server) = (MachineId::parse("00000000000000aa").unwrap(), MachineId::parse("00000000000000bb").unwrap());
    let now = 1_000_000;
    let mut on_mac = Pending::default();
    let mine = on_mac.add(pending_new(&set, now), now);

    // A card the server says is approved, as the person: it arrives waiting, as the manager's.
    let mut forged = pending_new(&set, now);
    forged.state = PendingState::Approved;
    forged.caller = Some(crate::caller::Caller::Ui);
    assert_eq!(on_mac.merge_theirs(std::slice::from_ref(&forged), &server, &mac, false, now + 1), 1);
    let card = on_mac.get(&forged.id).unwrap().clone();
    assert!(card.asked_elsewhere(&mac));
    assert_eq!(card.state, PendingState::Waiting, "nobody answered it on the Mac");
    assert!(card.caller.as_ref().is_some_and(|c| !c.is_person()), "{:?}", card.caller);

    // One the Mac shows waiting does not become approved either; how it ended there still comes.
    assert_eq!(on_mac.merge_theirs(std::slice::from_ref(&forged), &server, &mac, false, now + 2), 0);
    assert_eq!(on_mac.get(&forged.id).unwrap().state, PendingState::Waiting);
    let mut ran = forged.clone();
    ran.state = PendingState::Ran;
    assert_eq!(on_mac.merge_theirs(&[ran], &server, &mac, false, now + 3), 1);
    assert_eq!(on_mac.get(&forged.id).unwrap().state, PendingState::Ran);

    // The Mac's own ask, under the server's name: not touched.
    let mut taken_over = on_mac.get(&mine.id).unwrap().clone();
    taken_over.machine = None;
    taken_over.state = PendingState::Approved;
    assert_eq!(on_mac.merge_theirs(&[taken_over], &server, &mac, false, now + 4), 0, "the id of the Mac's own ask");
    assert_eq!(on_mac.get(&mine.id).unwrap().state, PendingState::Waiting);

    // The server takes the person's answer from the Mac only for its own card (asked there and
    // kept there), never through a record that reuses the id of one the Mac asked.
    let mut on_server = Pending::default();
    let asked_there = on_server.add(pending_new(&set, now), now);
    assert_eq!(on_server.merge_theirs(&[on_mac.get(&mine.id).unwrap().clone()], &mac, &server, true, now + 5), 1);
    let mut answer = on_server.get(&mine.id).unwrap().clone();
    answer.machine = Some(server.clone());
    answer.state = PendingState::Approved;
    assert_eq!(on_server.merge_theirs(&[answer], &mac, &server, true, now + 6), 0, "the Mac's ask, said to be the server's");
    let mut collided = asked_there.clone();
    collided.machine = None;
    collided.state = PendingState::Approved;
    assert_eq!(on_server.merge_theirs(&[collided], &mac, &server, true, now + 7), 0, "the server's card, said to be the Mac's");
    assert_eq!(on_server.get(&asked_there.id).unwrap().state, PendingState::Waiting);
}

/// D27: a session started for a pull request by a stranger is quarantined, whoever starts it; one
/// by the person, a confirmed teammate or the repository's own people keeps its origin.
#[test]
fn a_session_for_a_strangers_pull_request_is_quarantined() {
    let (me, team) = (vec!["alex-dev".to_string()], vec!["jordan".to_string()]);
    for association in [None, Some("NONE"), Some("CONTRIBUTOR"), Some("FIRST_TIME_CONTRIBUTOR")] {
        for started in [SessionOrigin::Person, SessionOrigin::Automation] {
            assert_eq!(origin_for_input(started, "drive-by", association, &me, &team), SessionOrigin::Quarantine, "{association:?} {started:?}");
        }
    }
    assert_eq!(origin_for_input(SessionOrigin::Person, "Alex-Dev", None, &me, &team), SessionOrigin::Person);
    assert_eq!(origin_for_input(SessionOrigin::Automation, "jordan", Some("NONE"), &me, &team), SessionOrigin::Automation);
    assert_eq!(origin_for_input(SessionOrigin::Person, "maintainer", Some("MEMBER"), &me, &team), SessionOrigin::Person);
    // Already quarantined: never lowered.
    assert_eq!(origin_for_input(SessionOrigin::Quarantine, "alex-dev", None, &me, &team), SessionOrigin::Quarantine);
}
