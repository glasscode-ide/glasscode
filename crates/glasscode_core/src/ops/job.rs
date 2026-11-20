//! A long op while it runs (M33.2, ops-10): how far it got, and whether its caller asked it to
//! stop. A caller that does not wait (`Request.detach`) has the op's id at once; `wait <id>` says
//! how far it got (`Progress`), and `cancel <id>` asks it to stop.
//!
//! Stopping is honest:
//! - **Where it stops.** An op stops between its steps (`Job::check`), or kills the one process
//!   it waits for (`output_unless`).
//! - **What its reply says.** It says what ran and what did not (`cancelled`).
//! - **What is recorded.** A step that ran stays done, and one stopped midway may have taken
//!   effect, so neither is recorded as a failure.
//!
//! Only the ops in [`CANCELLABLE`] look at it: a clone, a fetch and a worktree, a push and its
//! draft.

use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use serde_json::json;

use crate::control::{Reply, code};

/// The ops that stop when asked: each looks at its job between its steps.
pub const CANCELLABLE: &[&str] = &["project.import", "worktree.new", "pr.open_draft"];

/// How far an op got: step `step` of `of`, and what it does now ("pushing feat/login").
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Progress {
    pub step: u32,
    pub of: u32,
    pub text: String,
}

impl Progress {
    pub fn words(&self) -> String {
        format!("step {} of {}: {}", self.step, self.of, self.text)
    }
}

/// One running op's progress and its stop.
#[derive(Debug, Default)]
pub struct Job {
    stop: AtomicBool,
    progress: Mutex<Option<Progress>>,
    /// The steps that ran, in order: what a stop's reply says was done.
    done: Mutex<Vec<String>>,
}

/// Why a step did not run: its caller asked the op to stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cancelled {
    /// The steps that ran before it stopped.
    pub done: Vec<String>,
    /// A step stopped midway (a process killed): whether it took effect is not known.
    pub midway: Option<String>,
}

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cancelled")
    }
}

impl std::error::Error for Cancelled {}

impl Job {
    pub fn new() -> std::sync::Arc<Job> {
        std::sync::Arc::new(Job::default())
    }

    /// Ask it to stop. It stops at its next step, or kills the process it waits for.
    pub fn cancel(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn cancelled(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Step `step` of `of` begins.
    pub fn step(&self, step: u32, of: u32, text: impl Into<String>) {
        if let Ok(mut progress) = self.progress.lock() {
            *progress = Some(Progress { step, of, text: text.into() });
        }
    }

    pub fn progress(&self) -> Option<Progress> {
        self.progress.lock().ok().and_then(|p| p.clone())
    }

    /// A step ran (what a later stop says was done).
    pub fn ran(&self, what: impl Into<String>) {
        if let Ok(mut done) = self.done.lock() {
            done.push(what.into());
        }
    }

    /// Before a step: Err when the op was asked to stop, with what ran so far.
    pub fn check(&self) -> Result<(), Cancelled> {
        if !self.cancelled() {
            return Ok(());
        }
        Err(Cancelled { done: self.done.lock().map(|d| d.clone()).unwrap_or_default(), midway: None })
    }

    /// The stop of a step that was killed midway (`what`): whether it took effect is not known.
    pub fn stopped_midway(&self, what: impl Into<String>) -> Cancelled {
        Cancelled { done: self.done.lock().map(|d| d.clone()).unwrap_or_default(), midway: Some(what.into()) }
    }
}

/// A step's failure, or its stop: for a flow whose steps fail with `anyhow`.
#[derive(Debug)]
pub enum Failed {
    Cancelled(Cancelled),
    Error(anyhow::Error),
}

impl From<Cancelled> for Failed {
    fn from(stop: Cancelled) -> Failed {
        Failed::Cancelled(stop)
    }
}

impl From<anyhow::Error> for Failed {
    fn from(error: anyhow::Error) -> Failed {
        Failed::Error(error)
    }
}

/// Before a step, for an op that may have no job (a caller that waits for it): Err when asked to
/// stop.
pub fn check(job: Option<&Job>) -> Result<(), Cancelled> {
    job.map_or(Ok(()), Job::check)
}

/// `cmd`'s output, unless its job is asked to stop first: then the process is killed and the
/// answer is `None`. With no job it is `cmd.output()`. Its stdin is closed.
pub fn output_unless(mut cmd: Command, job: Option<&Job>) -> std::io::Result<Option<Output>> {
    let Some(job) = job else { return cmd.stdin(Stdio::null()).output().map(Some) };
    use std::io::Read;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if job.cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    Ok(Some(Output { status, stdout: out.join().unwrap_or_default(), stderr: err.join().unwrap_or_default() }))
}

/// The reply of an op that stopped when asked: what ran, and what may or may not have. Its
/// `data.status` is "cancelled"; the effect ledger keeps a step that ran as done and one stopped
/// midway as uncertain, never as failed.
pub fn cancelled(stop: &Cancelled) -> Reply {
    let mut text = String::from("Cancelled as asked");
    if stop.done.is_empty() && stop.midway.is_none() {
        text.push_str(": nothing was done.");
    } else {
        if !stop.done.is_empty() {
            text.push_str(&format!(" after {}", stop.done.join(", then ")));
        }
        match &stop.midway {
            Some(step) => text.push_str(&format!("; {step} was stopped midway, and whether it took effect is not known.")),
            None => text.push_str("; nothing after that was done."),
        }
    }
    Reply::Done { code: code::FAILED, text, data: json!({ "status": "cancelled", "done": stop.done, "midway": stop.midway, "error_code": "cancelled" }) }
}

// ----- op ids ----------------------------------------------------------------------------------

/// What became of a call with an op id (ops-10), as long as this run of the app keeps it.
#[derive(Clone, Debug)]
pub enum Called {
    /// Taken, not yet asked or started.
    Taken,
    /// It waits for the person, as approval `n`.
    Waiting(u64),
    /// It answered "started" as `n`, and runs.
    Running(u64),
    Done(Reply),
}

/// What a call with an op id finds.
#[derive(Debug)]
pub enum Seen<'a> {
    /// No call had this id: it runs.
    New,
    /// The id was given to a call of another op or other arguments: refused.
    OtherCall,
    Is(&'a Called),
}

/// The calls with an op id one run of the app keeps, by whose they are (`Caller::origin`) and the
/// id, at most `kept` (the oldest go first). After a restart the effect ledger answers for the
/// ops it records.
#[derive(Debug)]
pub struct Calls {
    map: std::collections::HashMap<(String, String), (String, Called)>,
    order: std::collections::VecDeque<(String, String)>,
    kept: usize,
}

/// How many calls with an op id the app keeps.
pub const CALLS_KEPT: usize = 1000;

impl Default for Calls {
    fn default() -> Calls {
        Calls { map: Default::default(), order: Default::default(), kept: CALLS_KEPT }
    }
}

/// What tells two calls apart under one op id: the op and its arguments.
pub fn call_hash(op: &super::Op) -> String {
    crate::effects::short_hash(&format!("{}{}", op.name(), op.args))
}

impl Calls {
    pub fn with_room(kept: usize) -> Calls {
        Calls { kept, ..Calls::default() }
    }

    pub fn seen(&self, key: &(String, String), hash: &str) -> Seen<'_> {
        match self.map.get(key) {
            None => Seen::New,
            Some((had, _)) if had != hash => Seen::OtherCall,
            Some((_, called)) => Seen::Is(called),
        }
    }

    pub fn keep(&mut self, key: (String, String), hash: String, called: Called) {
        if self.map.insert(key.clone(), (hash, called)).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > self.kept {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }

    /// What became of a call kept before; nothing when it was not kept.
    pub fn set(&mut self, key: &(String, String), called: Called) {
        if let Some((_, was)) = self.map.get_mut(key) {
            *was = called;
        }
    }

    /// The id is free again: the call was refused before anything ran.
    pub fn forget(&mut self, key: &(String, String)) {
        if self.map.remove(key).is_some() {
            self.order.retain(|k| k != key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ops-10: a call asked again with its op id finds what became of the first; other arguments
    /// under the same id are refused; a refused call frees its id; the oldest are let go.
    #[test]
    fn a_call_with_an_op_id_is_found_again() {
        let add = super::super::typed("board.add", json!({ "title": "x" })).unwrap();
        let other = super::super::typed("board.add", json!({ "title": "y" })).unwrap();
        let key = ("session:s1".to_string(), "call-1".to_string());
        let mut calls = Calls::with_room(2);
        assert!(matches!(calls.seen(&key, &call_hash(&add)), Seen::New));
        calls.keep(key.clone(), call_hash(&add), Called::Taken);
        assert!(matches!(calls.seen(&key, &call_hash(&add)), Seen::Is(Called::Taken)));
        assert!(matches!(calls.seen(&key, &call_hash(&other)), Seen::OtherCall));
        calls.set(&key, Called::Done(Reply::ok("Added", json!({}))));
        assert!(matches!(calls.seen(&key, &call_hash(&add)), Seen::Is(Called::Done(_))));
        // Another caller's same id is another call.
        assert!(matches!(calls.seen(&("manager:wake:".to_string(), "call-1".to_string()), &call_hash(&add)), Seen::New));
        calls.forget(&key);
        assert!(matches!(calls.seen(&key, &call_hash(&add)), Seen::New));
        for n in 0..3 {
            calls.keep(("o".into(), format!("c{n}")), "h".into(), Called::Taken);
        }
        assert!(matches!(calls.seen(&("o".into(), "c0".into()), "h"), Seen::New), "the oldest went");
        assert!(matches!(calls.seen(&("o".into(), "c2".into()), "h"), Seen::Is(_)));
    }

    #[test]
    fn a_job_stops_between_steps_and_says_what_ran() {
        let job = Job::new();
        assert!(job.check().is_ok());
        job.step(1, 2, "pushing feat/x");
        job.ran("pushing feat/x");
        assert_eq!(job.progress().unwrap().words(), "step 1 of 2: pushing feat/x");
        job.cancel();
        let stop = job.check().unwrap_err();
        assert_eq!(stop.done, ["pushing feat/x"]);
        let Reply::Done { code, text, data } = cancelled(&stop) else { panic!() };
        assert_eq!((code, data["status"].as_str()), (code::FAILED, Some("cancelled")));
        assert_eq!(text, "Cancelled as asked after pushing feat/x; nothing after that was done.");
        let Reply::Done { text, .. } = cancelled(&Cancelled { done: vec![], midway: None }) else { panic!() };
        assert_eq!(text, "Cancelled as asked: nothing was done.");
        let Reply::Done { text, data, .. } = cancelled(&job.stopped_midway("opening the draft")) else { panic!() };
        assert!(text.ends_with("opening the draft was stopped midway, and whether it took effect is not known."), "{text}");
        assert_eq!(data["midway"], json!("opening the draft"));
        assert!(check(None).is_ok());
    }

    /// A cancel kills the process the op waits for; with no job, it simply runs.
    #[cfg(unix)]
    #[test]
    fn a_cancel_kills_the_process_it_waits_for() {
        let job = Job::new();
        let stopper = job.clone();
        let started = std::time::Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            stopper.cancel();
        });
        let mut sleep = Command::new("sleep");
        sleep.arg("30");
        assert!(output_unless(sleep, Some(&job)).unwrap().is_none());
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        let mut echo = Command::new("echo");
        echo.arg("hi");
        let out = output_unless(echo, Some(&Job::new())).unwrap().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "hi\n");
        let mut echo = Command::new("echo");
        echo.arg("hi");
        assert!(output_unless(echo, None).unwrap().unwrap().status.success());
    }
}
