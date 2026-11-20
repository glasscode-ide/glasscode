//! Intake in the app (M60): every few minutes, on the home with the v2 engine and the manager on,
//! each project whose approved policy has an intake rule gets what `glasscode_core::intake` picks
//! for it: its labelled Linear tickets (from Tickets' last read) and GitHub issues (read here), how
//! loaded it is (its automation sessions, today's tokens, its pull requests waiting for review or
//! red), and then a session for each pick, started through the gate as the manager's, once.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use glasscode_core::effects::{self, Claim, EffectState};
use glasscode_core::gate::{ProjectPolicy, SessionOrigin};
use glasscode_core::intake::{self, Found, Load, Pick, Source};
use glasscode_core::manager::SessionState;
use glasscode_core::paths::ProjectKey;
use glasscode_core::store::Store as _;
use gpui::{AppContext as _, Context};

use crate::workspace::Workspace;

/// How often intake looks.
const EVERY: Duration = Duration::from_secs(10 * 60);

/// Today's tokens count only when the usage scan is this recent (it scans every 15 minutes).
const USAGE_FRESH_SECS: i64 = 30 * 60;

/// A start that did not happen: the work item intake wrote for it goes. Off the main thread.
fn forget_work(key: glasscode_core::identity::WorkKey) {
    std::thread::spawn(move || {
        if let Err(err) = glasscode_core::store::update::<glasscode_core::work::WorkLedger, _>(|ledger| ledger.items.retain(|item| !(item.key == key && item.sessions.is_empty()))) {
            log::warn!("intake: the work item of {} could not be removed ({err})", key.id);
        }
    });
}

#[derive(Default)]
pub struct IntakeUi {
    busy: bool,
    at: Option<Instant>,
    /// What the journal heard last of each project, not to say it every round.
    said: BTreeMap<String, String>,
    /// Work intake found but did not take (a project not on Autopilot, no room, red CI…), by
    /// project id: Today shows it, for the person to start.
    pub suggestions: BTreeMap<String, Vec<Suggestion>>,
}

/// A piece of work intake found and left for the person.
#[derive(Clone, Debug)]
pub struct Suggestion {
    pub found: Found,
    /// Why intake did not take it itself.
    pub why: String,
}

/// One project intake looks at.
struct Wanted {
    id: String,
    key: ProjectKey,
    policy: ProjectPolicy,
    /// The repository and label of its issues, when it takes issues.
    issues: Option<(String, String)>,
}

impl Workspace {
    pub(crate) fn intake_tick(&mut self, cx: &mut Context<Self>) {
        if !self.control.engine_v2 || !self.state.manager.enabled || !self.acts_as_home() || self.intake.busy || self.intake.at.is_some_and(|at| at.elapsed() < EVERY) {
            return;
        }
        self.intake.at = Some(Instant::now());
        let authority = &self.control.authority;
        if authority.pause.all {
            return;
        }
        let policy = &authority.policy;
        let wanted: Vec<Wanted> = self
            .state
            .projects
            .iter()
            // The manager leaves it alone, or the person paused it: no work is taken.
            .filter(|project| !self.state.manager.off_projects.contains(&project.id))
            .filter_map(|project| {
                let key = self.control.project_keys.get(&project.id)?.clone();
                if authority.pause.projects.contains(&key) {
                    return None;
                }
                let policy = policy.projects.get(&key).cloned().unwrap_or_else(|| policy.default.clone());
                let rule = policy.intake.as_ref()?;
                let issues = rule.github_label.clone().and_then(|label| self.tickets_repo_of(&project.root).map(|repo| (repo, label)));
                Some(Wanted { id: project.id.clone(), key, policy, issues })
            })
            .collect();
        if wanted.is_empty() {
            return;
        }
        // Today's tokens must be known, and lately, before any work is taken.
        let fresh = self.usage.report.as_ref().is_some_and(|report| (glasscode_core::model::now() as i64).saturating_sub(report.scanned_at) < USAGE_FRESH_SECS);
        if !fresh {
            self.intake.at = None;
            return;
        }
        // The person's own tickets (others' in review are not work to take).
        let linear: Vec<Found> = self.tickets.snapshot.iter().flat_map(|s| s.mine.iter()).filter_map(intake::from_ticket).collect();
        let env = (*self.env).clone();
        let asks: Vec<(ProjectKey, Option<(String, String)>)> = wanted.iter().map(|w| (w.key.clone(), w.issues.clone())).collect();
        self.intake.busy = true;
        let task = cx.background_spawn(async move {
            let ledger = glasscode_core::work::WorkLedger::load().unwrap_or_default();
            let Ok(effects) = effects::load() else {
                log::warn!("intake: the effect ledger could not be read; no work is taken");
                return Vec::new();
            };
            let now = glasscode_core::clock::now_ms();
            let mut out: Vec<(Vec<Found>, BTreeSet<String>)> = Vec::new();
            for (key, issues) in asks {
                let mut found = linear.clone();
                if let Some((repo, label)) = issues {
                    match intake::labeled_issues(&repo, &label, &env) {
                        Ok(list) => found.extend(list),
                        Err(err) => log::info!("intake: {repo}'s issues labelled “{label}” could not be read ({err})"),
                    }
                }
                // Known already: work the ledger has, or an item picked before.
                let taken: BTreeSet<String> = found
                    .iter()
                    .filter(|f| {
                        let work = intake::work_key(&key, f);
                        ledger.items.iter().any(|item| item.key == work || item.aliases.contains(&work.id) && item.key.project == key) || intake::picked_before(effects.records.get(&intake::effect(&key, f)), now)
                    })
                    .map(Found::id)
                    .collect();
                out.push((found, taken));
            }
            out
        });
        cx.spawn(async move |this, cx| {
            let read = task.await;
            let _ = this.update(cx, |ws, cx| {
                ws.intake.busy = false;
                // The ledger could not be read: nothing this round.
                if read.len() != wanted.len() {
                    return;
                }
                for (w, (found, taken)) in wanted.into_iter().zip(read) {
                    ws.intake_project(w, &found, &taken, cx);
                }
            });
        })
        .detach();
    }

    /// One project's round: its load, the plan, what the journal hears, and the picks.
    fn intake_project(&mut self, w: Wanted, found: &[Found], taken: &BTreeSet<String>, cx: &mut Context<Self>) {
        let load = self.intake_load(&w.id);
        let me: Vec<String> = self.manager.me.iter().cloned().collect();
        let confirmed: Vec<String> = self.state.manager.people.iter().map(|p| p.github.clone()).filter(|login| !login.is_empty()).collect();
        let plan = intake::plan(&w.key, &w.policy, found, &load, &|key| found.iter().any(|f| intake::work_key(&w.key, f) == *key && taken.contains(&f.id())), &me, &confirmed);
        let said = match (&plan.blocked, plan.picks.len()) {
            (Some(why), _) => format!("nothing taken: {}", why.words()),
            (None, 0) => String::new(),
            (None, n) => format!("{n} taken"),
        };
        if !said.is_empty() && self.intake.said.get(&w.id) != Some(&said) {
            let held: Vec<serde_json::Value> = plan.held.iter().map(|(id, why)| serde_json::json!({ "item": id, "why": why.words() })).collect();
            glasscode_core::journal::append(glasscode_core::journal::decision("intake", if plan.blocked.is_some() { "held" } else { "picked" }, &format!("{}: {said}", w.key), serde_json::json!({ "project": w.key.as_str(), "held": held, "load": { "sessions": load.automation_sessions, "tokens_today": load.tokens_today, "waiting_review": load.waiting_review, "red": load.red } }), None));
            self.intake.said.insert(w.id.clone(), said);
        }
        // What it left is the person's to start, from Today (never an item whose author is not
        // trusted: its text goes to no session).
        let left: Vec<Suggestion> = plan
            .held
            .iter()
            .filter(|(_, why)| !matches!(why, intake::Held::Untrusted { .. }))
            .filter_map(|(id, why)| found.iter().find(|f| f.id() == *id).map(|f| Suggestion { found: f.clone(), why: why.words() }))
            .collect();
        if left.is_empty() {
            self.intake.suggestions.remove(&w.id);
        } else {
            self.intake.suggestions.insert(w.id.clone(), left);
        }
        for pick in plan.picks {
            self.intake_start(&w.id, pick, cx);
        }
        cx.notify();
    }

    /// The person starts a suggestion from Today: `ticket.start` or `session.new`, as they would
    /// by hand, with intake's first message and the work item's typed done conditions.
    pub(crate) fn intake_start_suggestion(&mut self, project_id: &str, id: &str, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let Some(suggestion) = self.intake.suggestions.get(project_id).and_then(|list| list.iter().find(|s| s.found.id() == id)).cloned() else { return };
        let found = suggestion.found;
        let (name, args) = match &found.source {
            Some(Source::Linear { identifier, .. }) => ("ticket.start", serde_json::json!({ "ticket": identifier, "project": project_id, "message": intake::message(&found), "open": true })),
            _ => ("session.new", serde_json::json!({ "project": project_id, "branch": intake::branch(&found), "title": glasscode_core::linear::short_title(&found.title, 60), "message": intake::message(&found), "open": true })),
        };
        if let Some(list) = self.intake.suggestions.get_mut(project_id) {
            list.retain(|s| s.found.id() != id);
        }
        self.run_op_named(name, args, window, cx);
    }

    /// How loaded a project is now.
    fn intake_load(&self, project_id: &str) -> Load {
        let snap = self.manager_snapshot();
        let ours: Vec<&glasscode_core::manager::SessionInfo> = snap.sessions.iter().filter(|s| s.project_id == project_id).collect();
        // An automation session holds a place while its work is not done (a session whose pull
        // request is ready and green, or merged, gives its place back).
        let automation_sessions = ours.iter().filter(|s| s.state != SessionState::NotRunning && self.control.authority.origin(&s.id) != SessionOrigin::Person && !intake::work_done(s.pr.as_ref())).count() as u32;
        let mut waiting = BTreeSet::new();
        let mut red = BTreeSet::new();
        for pr in ours.iter().filter_map(|s| s.pr.as_ref()) {
            let id = format!("{}#{}", pr.repo, pr.number);
            if intake::waits_for_review(pr) {
                waiting.insert(id.clone());
            }
            if pr.state == "open" && pr.ci == "failing" {
                red.insert(id);
            }
        }
        let mut roots: Vec<PathBuf> = self.state.projects.iter().filter(|p| p.id == project_id).map(|p| p.root.clone()).collect();
        roots.extend(self.worktrees.get(project_id).into_iter().flatten().map(|w| w.path.clone()));
        let today = glasscode_core::usage::local_day(glasscode_core::model::now() as i64);
        let tokens_today = self.usage.report.as_ref().map(|report| intake::tokens_on(report, &today, &roots)).unwrap_or(0);
        Load { automation_sessions, tokens_today, waiting_review: waiting.len() as u32, red: red.into_iter().collect() }
    }

    /// Start a pick's session, once: its effect claimed first, its work item written with its done
    /// conditions, then `ticket.start` (a Linear ticket) or `session.new` (an issue, on its own
    /// branch), through the gate as the manager's.
    fn intake_start(&mut self, project_id: &str, pick: Pick, cx: &mut Context<Self>) {
        let (name, args) = match &pick.found.source {
            Some(Source::Linear { identifier, .. }) => ("ticket.start", serde_json::json!({ "ticket": identifier, "project": project_id, "message": pick.message })),
            _ => ("session.new", serde_json::json!({ "project": project_id, "branch": intake::branch(&pick.found), "title": glasscode_core::linear::short_title(&pick.found.title, 60), "message": pick.message })),
        };
        let op = match glasscode_core::ops::typed(name, args) {
            Ok(op) => op,
            Err(err) => {
                log::warn!("intake: {} could not be started ({})", pick.found.id(), err.text);
                return;
            }
        };
        let intent = intake::intent(&pick, name, &self.control.launch);
        let generation = self.home_generation();
        let (key, done, ticket) = (pick.key.clone(), pick.done.clone(), matches!(pick.found.source, Some(Source::Linear { .. })).then(|| pick.found.id()));
        let work = key.clone();
        let task = cx.background_spawn(async move {
            let home = glasscode_core::paths::MachineId::this().map_err(|err| std::io::Error::other(err.to_string()))?;
            let claim = effects::claim_at(&intent, &home, generation)?;
            let mut added = false;
            if let Claim::New(_) = claim {
                // The work item first, with its typed done conditions: the session's start finds it.
                let stamp = glasscode_core::identity::next_stamps(&home, 1).ok().and_then(|mut s| s.pop()).unwrap_or_else(|| glasscode_core::identity::Hlc::zero(home.clone()));
                added = glasscode_core::store::update::<glasscode_core::work::WorkLedger, _>(|ledger| {
                    if ledger.items.iter().any(|item| item.key == key) {
                        return false;
                    }
                    let mut item = glasscode_core::work::WorkItem::new(key.clone(), stamp);
                    item.done = done;
                    item.ticket = ticket;
                    ledger.items.push(item);
                    true
                })
                .map_err(|err| std::io::Error::other(err.to_string()))?;
            }
            Ok::<_, std::io::Error>((claim, added))
        });
        let id = pick.found.id();
        let key_of_pick = pick.key.clone();
        cx.spawn(async move |this, cx| {
            let claimed = task.await;
            let _ = this.update(cx, |ws, cx| match claimed {
                Ok((Claim::New(effect), added)) if ws.main_window.is_some() => {
                    glasscode_core::journal::append(glasscode_core::journal::decision("intake", "started", &format!("Took {id}"), serde_json::json!({ "item": id }), None));
                    let caller = crate::control_ui::Caller {
                        who: "Glasscode's intake".into(),
                        trust: crate::control_ui::Trust::Session,
                        channel: glasscode_core::caller::Caller::Manager { lane: glasscode_core::caller::Lane::Wake, wake: Some("intake".into()), project: None },
                        session: None,
                        session_title: None,
                        agent: None,
                        cwd: None,
                        project: None,
                        drive: Some((1, Vec::new())),
                    };
                    ws.with_main_window(cx, move |ws, window, cx| {
                        ws.run_op(
                            &op,
                            caller,
                            move |_, reply| match reply {
                                glasscode_core::control::Reply::Done { code: 0, .. } => effects::finish_later(effect, EffectState::Done(serde_json::Value::Null)),
                                glasscode_core::control::Reply::Done { text, .. } => {
                                    effects::finish_later(effect, EffectState::Failed(text.lines().next().unwrap_or_default().to_string()));
                                    // No session: the work item written for it goes again (it is
                                    // tried again later, a few times).
                                    if added {
                                        forget_work(work.clone());
                                    }
                                }
                                // A card waits for the person; a restart settles it.
                                glasscode_core::control::Reply::Waiting { .. } => {}
                            },
                            window,
                            cx,
                        );
                    });
                }
                Ok((Claim::New(effect), added)) => {
                    effects::finish_later(effect, EffectState::Failed("no window to start it in".into()));
                    if added {
                        forget_work(key_of_pick);
                    }
                }
                Ok((Claim::Already(_), _)) => {}
                Err(err) => log::error!("intake: {id} was not taken: the effect ledger or the work ledger could not be written ({err})"),
            });
        })
        .detach();
    }
}
