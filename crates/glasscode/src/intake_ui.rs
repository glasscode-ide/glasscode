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

#[derive(Default)]
pub struct IntakeUi {
    busy: bool,
    at: Option<Instant>,
    /// What the journal heard last of each project, not to say it every round.
    said: BTreeMap<String, String>,
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
        let policy = &self.control.authority.policy;
        let wanted: Vec<Wanted> = self
            .state
            .projects
            .iter()
            .filter_map(|project| {
                let key = self.control.project_keys.get(&project.id)?.clone();
                let policy = policy.projects.get(&key).cloned().unwrap_or_else(|| policy.default.clone());
                let rule = policy.intake.as_ref()?;
                let issues = rule.github_label.clone().and_then(|label| self.tickets_repo_of(&project.root).map(|repo| (repo, label)));
                Some(Wanted { id: project.id.clone(), key, policy, issues })
            })
            .collect();
        if wanted.is_empty() {
            return;
        }
        // Today's tokens must be known before any work is taken.
        if self.usage.report.is_none() {
            self.intake.at = None;
            return;
        }
        let linear: Vec<Found> = self.tickets.snapshot.iter().flat_map(|s| s.mine.iter().chain(s.reviewing.iter())).filter_map(intake::from_ticket).collect();
        let env = (*self.env).clone();
        let asks: Vec<(ProjectKey, Option<(String, String)>)> = wanted.iter().map(|w| (w.key.clone(), w.issues.clone())).collect();
        self.intake.busy = true;
        let task = cx.background_spawn(async move {
            let ledger = glasscode_core::work::WorkLedger::load().unwrap_or_default();
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
                        ledger.items.iter().any(|item| item.key == work || item.aliases.contains(&work.id) && item.key.project == key) || effects::find(&intake::effect(&key, f)).ok().flatten().is_some()
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
        for pick in plan.picks {
            self.intake_start(&w.id, pick, cx);
        }
    }

    /// How loaded a project is now.
    fn intake_load(&self, project_id: &str) -> Load {
        let snap = self.manager_snapshot();
        let ours: Vec<&glasscode_core::manager::SessionInfo> = snap.sessions.iter().filter(|s| s.project_id == project_id).collect();
        let automation_sessions = ours.iter().filter(|s| s.state != SessionState::NotRunning && self.control.authority.origin(&s.id) != SessionOrigin::Person).count() as u32;
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
        let task = cx.background_spawn(async move {
            let home = glasscode_core::paths::MachineId::this().map_err(|err| std::io::Error::other(err.to_string()))?;
            let claim = effects::claim_at(&intent, &home, generation)?;
            if let Claim::New(_) = claim {
                // The work item first, with its typed done conditions: the session's start finds it.
                let stamp = glasscode_core::identity::next_stamps(&home, 1).ok().and_then(|mut s| s.pop()).unwrap_or_else(|| glasscode_core::identity::Hlc::zero(home.clone()));
                glasscode_core::store::update::<glasscode_core::work::WorkLedger, _>(|ledger| {
                    if !ledger.items.iter().any(|item| item.key == key) {
                        let mut item = glasscode_core::work::WorkItem::new(key.clone(), stamp);
                        item.done = done;
                        item.ticket = ticket;
                        ledger.items.push(item);
                    }
                })
                .map_err(|err| std::io::Error::other(err.to_string()))?;
            }
            Ok::<_, std::io::Error>(claim)
        });
        let id = pick.found.id();
        cx.spawn(async move |this, cx| {
            let claimed = task.await;
            let _ = this.update(cx, |ws, cx| match claimed {
                Ok(Claim::New(effect)) if ws.main_window.is_some() => {
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
                                glasscode_core::control::Reply::Done { text, .. } => effects::finish_later(effect, EffectState::Failed(text.lines().next().unwrap_or_default().to_string())),
                                // A card waits for the person; a restart settles it.
                                glasscode_core::control::Reply::Waiting { .. } => {}
                            },
                            window,
                            cx,
                        );
                    });
                }
                Ok(Claim::New(effect)) => effects::finish_later(effect, EffectState::Failed("no window to start it in".into())),
                Ok(Claim::Already(_)) => {}
                Err(err) => log::error!("intake: {id} was not taken: the effect ledger or the work ledger could not be written ({err})"),
            });
        })
        .detach();
    }
}
