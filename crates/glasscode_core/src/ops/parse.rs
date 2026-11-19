//! From the `glasscode` command's words to an [`Op`], with no IO: the grammar the commands always
//! had (`control::Args` splits the words and options), now read from the table.
//!
//! What a command line can mean depends on the state of the app only through [`ParseCtx`]: the
//! day it is, who calls, and the sessions there are. Everything else a word names (a task, a
//! project, a ticket) stays a word here and is looked up by the op's handler (M12.2).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::spec::{ArgKind, ArgSpec, At, OpSpec, Options};
use super::{Candidate, ErrorCode, NOT_YET, NOUNS, OPS, Op, OpError};
use crate::board::{Every, List, Section};
use crate::control::Args;
use crate::day::{self, Day};

/// A session as the window shows it, for naming one on the command line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionView {
    pub id: String,
    /// Its title as shown.
    pub title: String,
    #[serde(default)]
    pub branch: String,
    /// The pull request a review session reviews: "#958" names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// What a command line's meaning depends on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParseCtx {
    /// The person at a terminal (or in the app). Only they may name a session by the start of
    /// its id or words from its title (ops-17).
    pub caller_is_person: bool,
    /// The session the command runs in: "this".
    pub caller_session: Option<String>,
    /// Today, for "fri" and "in 3 days" (`day::local` of the clock's time).
    pub today: Day,
    /// The sessions it may name, in the order the window keeps them; hidden projects' are not here.
    pub sessions: Vec<SessionView>,
}

/// The op the words name, with its arguments; or why not, in the words the commands always used.
pub fn parse(words: &[String], ctx: &ParseCtx) -> Result<Op, OpError> {
    let args = Args::parse(words);
    let (spec, own, verb) = command(&args)?;
    if spec.options == Options::Refuse {
        refuse_unknown(spec, &args)?;
    }
    let mut out = Map::new();
    for arg in spec.args {
        if let Some(value) = value(arg, &args, own, &verb, ctx)? {
            out.insert(arg.name.to_string(), value);
        }
    }
    Ok(Op { spec, args: Value::Object(out) })
}

fn usage(text: impl Into<String>) -> OpError {
    OpError::usage(text)
}

/// The op the first words name, how many words are its own, and the word that named it (for
/// "Say which task: `glasscode board tick <id>`"). Nouns and verbs are read in any letter case.
fn command(args: &Args) -> Result<(&'static OpSpec, usize, String), OpError> {
    let words: Vec<String> = args.words.iter().map(|w| w.to_lowercase()).collect();
    let mut path: Vec<&'static str> = Vec::new();
    loop {
        let depth = path.len();
        let word = words.get(depth).map(String::as_str);
        let here = || OPS.iter().filter(|op| op.cli.len() == depth + 1 && op.cli[..depth] == path[..]);
        let Some(word) = word else {
            if let Some(op) = here().find(|op| op.aliases.contains(&"")) {
                return Ok((op, depth, op.cli[depth].to_string()));
            }
            return Err(unknown(&path, ""));
        };
        if let Some(op) = here().find(|op| op.cli[depth] == word || op.aliases.iter().any(|a| !a.is_empty() && *a == word)) {
            return Ok((op, depth + 1, word.to_string()));
        }
        if let Some(next) = noun(&path, word) {
            path.push(next);
            continue;
        }
        if NOT_YET.iter().any(|(p, verbs)| p[..] == path[..] && verbs.contains(&word)) {
            let line = words[..=depth].join(" ");
            return Err(OpError::new(ErrorCode::Failed, format!("“{line}” is not in the registry yet (M12.2).")));
        }
        if let Some(op) = here().find(|op| op.aliases.contains(&"*")) {
            return Ok((op, depth + 1, word.to_string()));
        }
        return Err(unknown(&path, word));
    }
}

/// The noun after `path` that `word` is, by its own word or another one.
fn noun(path: &[&'static str], word: &str) -> Option<&'static str> {
    let depth = path.len();
    OPS.iter().filter(|op| op.cli.len() > depth + 1 && op.cli[..depth] == path[..]).map(|op| op.cli[depth]).find(|next| {
        *next == word || NOUNS.iter().any(|n| n.path.len() == depth + 1 && n.path[..depth] == path[..] && n.path[depth] == *next && n.also.contains(&word))
    })
}

fn unknown(path: &[&str], word: &str) -> OpError {
    if path.is_empty() {
        return usage(if word.is_empty() { "Say what to do; `glasscode help` lists the commands.".to_string() } else { format!("Unknown command “{word}”. `glasscode help` lists them.") });
    }
    let text = NOUNS.iter().find(|n| n.path == path).map(|n| n.unknown).unwrap_or("Unknown command “{word}”. `glasscode help` lists them.");
    usage(text.replace("{word}", word))
}

/// "Unknown option --x. This command takes: --a, --b." for an option the op does not know.
fn refuse_unknown(spec: &OpSpec, args: &Args) -> Result<(), OpError> {
    let known = spec.flags();
    let unknown = args.unknown(&known);
    if unknown.is_empty() {
        return Ok(());
    }
    let takes = if known.is_empty() { "no options".to_string() } else { known.iter().map(|k| format!("--{k}")).collect::<Vec<_>>().join(", ") };
    Err(usage(format!("Unknown option {}. This command takes: {takes}.", unknown.join(", "))))
}

/// The words given for an argument, if any. A text of only white space is none.
fn given(arg: &ArgSpec, args: &Args, own: usize) -> Option<String> {
    let flag = |names: &[&str]| names.iter().find_map(|n| args.flag(n)).map(str::to_string);
    match arg.at {
        At::Word(n) => args.word(own + usize::from(n)).map(str::to_string),
        At::Rest(n) => Some(args.rest(own + usize::from(n))).filter(|t| !t.trim().is_empty()),
        At::Flag(names) => flag(names),
        At::FlagOrWord(names, n) => flag(names).or_else(|| args.word(own + usize::from(n)).map(str::to_string)),
    }
}

/// An argument's value in the op, or None when it is left out.
fn value(arg: &ArgSpec, args: &Args, own: usize, verb: &str, ctx: &ParseCtx) -> Result<Option<Value>, OpError> {
    match arg.kind {
        ArgKind::Switch => return Ok(arg.at.flags().iter().any(|n| args.has(n)).then_some(Value::Bool(true))),
        ArgKind::Many => {
            let all: Vec<String> = arg.at.flags().iter().flat_map(|n| args.all(n)).collect();
            return Ok((!all.is_empty()).then(|| json!(all)));
        }
        _ => {}
    }
    let Some(text) = given(arg, args, own).or_else(|| arg.default.map(str::to_string)) else {
        if arg.required {
            return Err(usage(arg.missing.replace("{verb}", verb)));
        }
        return Ok(None);
    };
    let value = match arg.kind {
        ArgKind::Word | ArgKind::Text | ArgKind::Project | ArgKind::Task => Value::String(text),
        ArgKind::Count => match text.parse::<u64>().ok().or_else(|| arg.default.and_then(|d| d.parse().ok())) {
            Some(n) => json!(n),
            None => return Ok(None),
        },
        ArgKind::Day => json!(when(&text, ctx.today)?.map(day::iso)),
        ArgKind::Until => {
            let until = when(&text, ctx.today)?.unwrap_or(ctx.today + 1);
            if until <= ctx.today {
                return Err(usage("Snooze until a day after today."));
            }
            json!(day::iso(until))
        }
        ArgKind::Session => json!(find_session(&text, ctx)?),
        ArgKind::Pr => json!(pr_number(&text)?),
        ArgKind::Ticket => json!(text.to_uppercase()),
        ArgKind::List => {
            let list = List::parse(&text).ok_or_else(|| usage(format!("--list is todo, asked, waiting or later (not {text}).")))?;
            serde_json::to_value(list).unwrap_or_default()
        }
        ArgKind::Section => json!(Section::parse(&text).ok_or_else(|| usage(format!("No section “{text}”.")))?.key()),
        ArgKind::Movable => json!(Section::parse(&text).filter(|s| s.movable()).ok_or_else(|| usage(format!("Tasks move to asked, todo, waiting or later (not {text}).")))?.key()),
        ArgKind::Every => {
            Every::parse(&text).ok_or_else(|| usage(format!("--every is day, weekday, monday…sunday, month-start or month-end (not {text}).")))?;
            Value::String(text)
        }
        ArgKind::Switch | ArgKind::Many => unreachable!("read above"),
    };
    Ok(Some(value))
}

/// A day from the command line ("fri", "tomorrow", "2026-11-03"), or None for "none".
fn when(word: &str, today: Day) -> Result<Option<Day>, OpError> {
    if matches!(word.to_lowercase().as_str(), "none" | "no" | "never" | "-") {
        return Ok(None);
    }
    day::parse_when(word, today).map(Some).ok_or_else(|| usage(format!("“{word}” is not a day: today, tomorrow, fri, next monday, in 3 days, 2026-11-03 or 30.10.")))
}

fn pr_number(word: &str) -> Result<u64, OpError> {
    let w = word.trim().trim_start_matches('#');
    let w = w.rsplit('/').next().unwrap_or(w);
    w.parse().map_err(|_| usage(format!("“{word}” is not a pull request number (958, #958 or its URL).")))
}

fn short(id: &str) -> &str {
    &id[..id.len().min(8)]
}

fn candidates(sessions: &[&SessionView]) -> Vec<Candidate> {
    sessions.iter().take(6).map(|s| Candidate { id: s.id.clone(), title: s.title.clone(), project: s.project.clone() }).collect()
}

/// A session's whole id from a word. "this" is the caller's own session, and its whole id names
/// one for anyone; the start of an id (four characters at least) and words from its title or
/// branch (or "#958" for a review) only for the person (ops-17): any other caller is told which
/// sessions the word could mean, so that it names one by its id.
fn find_session(word: &str, ctx: &ParseCtx) -> Result<String, OpError> {
    let w = word.trim();
    if matches!(w, "this" | "self" | "." | "me") {
        return ctx.caller_session.clone().ok_or_else(|| usage("“this” is the session the command runs in, and this command does not run in a Glasscode session."));
    }
    if let Some(s) = ctx.sessions.iter().find(|s| s.id == w) {
        return Ok(s.id.clone());
    }
    let starts: Vec<&SessionView> = ctx.sessions.iter().filter(|s| w.len() >= 4 && s.id.starts_with(w)).collect();
    if let ([one], true) = (starts.as_slice(), ctx.caller_is_person) {
        return Ok(one.id.clone());
    }
    let words: Vec<String> = w.to_lowercase().split_whitespace().map(str::to_string).collect();
    let hits: Vec<&SessionView> = ctx
        .sessions
        .iter()
        .filter(|s| {
            let (title, branch) = (s.title.to_lowercase(), s.branch.to_lowercase());
            let pr = s.review.map(|n| format!("#{n}")).unwrap_or_default();
            !words.is_empty() && words.iter().all(|x| title.contains(x) || branch.contains(x) || pr == *x)
        })
        .collect();
    let not_found = || OpError::new(ErrorCode::NotFound, format!("No session “{w}”. `glasscode session list` shows them."));
    if !ctx.caller_is_person {
        let more: Vec<&SessionView> = hits.into_iter().filter(|h| !starts.iter().any(|s| s.id == h.id)).collect();
        let could: Vec<&SessionView> = starts.into_iter().chain(more).collect();
        if could.is_empty() {
            return Err(not_found());
        }
        let list = could.iter().take(6).map(|s| format!("{} ({})", s.id, s.title)).collect::<Vec<_>>().join(", ");
        return Err(OpError::new(
            ErrorCode::Ambiguous { candidates: candidates(&could) },
            format!("Name the session by its whole id, or “this”: “{w}” could be {list}. Only the person, at a terminal, names a session by the start of its id or words from its title."),
        ));
    }
    match hits.as_slice() {
        [one] => Ok(one.id.clone()),
        [] => Err(not_found()),
        many => Err(OpError::new(
            ErrorCode::Ambiguous { candidates: candidates(many) },
            format!(
                "“{w}” matches {} sessions: {}. Use its id.",
                many.len(),
                many.iter().take(6).map(|s| format!("{} ({})", short(&s.id), s.title)).collect::<Vec<_>>().join(", ")
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &[&str]) -> Vec<String> {
        line.iter().map(|w| w.to_string()).collect()
    }

    fn session(id: &str, title: &str, branch: &str) -> SessionView {
        SessionView { id: id.into(), title: title.into(), branch: branch.into(), review: None, project: Some("app".into()) }
    }

    fn ctx(person: bool) -> ParseCtx {
        ParseCtx {
            caller_is_person: person,
            caller_session: None,
            today: day::from_civil(2026, 11, 19), // a Thursday
            sessions: vec![
                session("8765f51a-8ad5-4e73-9df0-27adaa817aa9", "API connectors", "feat/api-connectors"),
                session("973f0fe4-a51e-4b64-bfbb-779132b51c0a", "Retry queue", "fix/retry-queue"),
                SessionView { review: Some(958), ..session("d740a312-2980-49ad-96b0-17b1bc7bcb50", "#958 Checkout page", "feat/checkout") },
                session("f58e3bf6-02d9-42ec-9a5e-dfdc83148786", "API docs", "docs/api"),
            ],
        }
    }

    fn shown(line: &[&str], ctx: &ParseCtx) -> Result<Value, OpError> {
        parse(&words(line), ctx).map(|op| serde_json::to_value(&op).unwrap())
    }

    fn session_of(line: &[&str], ctx: &ParseCtx) -> Result<String, OpError> {
        parse(&words(line), ctx).map(|op| op.args["session"].as_str().unwrap_or_default().to_string())
    }

    #[test]
    fn fuzzy_match_only_for_person() {
        let (person, agent) = (ctx(true), ctx(false));
        let retry = "973f0fe4-a51e-4b64-bfbb-779132b51c0a";
        // The whole id names it for anyone.
        assert_eq!(session_of(&["session", "show", retry], &person).as_deref(), Ok(retry));
        assert_eq!(session_of(&["session", "show", retry], &agent).as_deref(), Ok(retry));
        // The start of its id, words of its title or branch, a review's number: the person only.
        for word in ["973f", "973f0fe4", "retry", "RETRY", "fix/retry", "queue"] {
            assert_eq!(session_of(&["session", "show", word], &person).as_deref(), Ok(retry), "{word}");
            let refused = session_of(&["session", "show", word], &agent).unwrap_err();
            assert_eq!(refused.exit_code(), 2, "{word}");
            assert!(matches!(&refused.code, ErrorCode::Ambiguous { candidates } if candidates.len() == 1 && candidates[0].id == retry), "{word}: {refused:?}");
            assert!(refused.text.contains(retry) && refused.text.contains("whole id"), "{}", refused.text);
        }
        assert_eq!(session_of(&["session", "show", "#958"], &person).as_deref(), Ok("d740a312-2980-49ad-96b0-17b1bc7bcb50"));
        assert!(session_of(&["session", "show", "#958"], &agent).is_err());
        // Several: the person is told which, as always; so is any other caller.
        let both = session_of(&["session", "show", "api"], &person).unwrap_err();
        assert_eq!(both.text, "“api” matches 2 sessions: 8765f51a (API connectors), f58e3bf6 (API docs). Use its id.");
        assert!(matches!(&both.code, ErrorCode::Ambiguous { candidates } if candidates.len() == 2));
        assert!(matches!(session_of(&["session", "show", "api"], &agent).unwrap_err().code, ErrorCode::Ambiguous { candidates } if candidates.len() == 2));
        // Nothing like it: not found for both (exit 1, as before).
        for c in [&person, &agent] {
            let none = session_of(&["session", "show", "nosuch"], c).unwrap_err();
            assert_eq!((none.exit_code(), none.code, none.text.as_str()), (1, ErrorCode::NotFound, "No session “nosuch”. `glasscode session list` shows them."));
        }
        // A start of three characters is too short to name one, even for the person.
        assert!(session_of(&["session", "show", "973"], &person).is_err());
    }

    #[test]
    fn this_is_the_callers_own_session() {
        let mut agent = ctx(false);
        assert_eq!(session_of(&["session", "show"], &agent).unwrap_err().text, "“this” is the session the command runs in, and this command does not run in a Glasscode session.");
        agent.caller_session = Some("f58e3bf6-02d9-42ec-9a5e-dfdc83148786".into());
        for line in [&["session", "show"][..], &["session", "read", "this"], &["s", "info", "me"], &["session", "show", "."]] {
            assert_eq!(session_of(line, &agent).as_deref(), Ok("f58e3bf6-02d9-42ec-9a5e-dfdc83148786"), "{line:?}");
        }
    }

    #[test]
    fn days_are_read_from_today() {
        let c = ctx(false);
        let due = |word: &str| shown(&["board", "add", "x", "--due", word], &c).map(|v| v["args"]["due"].clone());
        assert_eq!(due("fri"), Ok(json!("2026-11-20")));
        assert_eq!(due("next monday"), Ok(json!("2026-11-23")));
        assert_eq!(due("in 3 days"), Ok(json!("2026-11-22")));
        assert_eq!(due("30.11."), Ok(json!("2026-11-30")));
        assert_eq!(due("none"), Ok(Value::Null));
        assert_eq!(due("someday").unwrap_err().text, "“someday” is not a day: today, tomorrow, fri, next monday, in 3 days, 2026-11-03 or 30.10.");
        let until = |line: &[&str]| shown(line, &c).map(|v| v["args"]["until"].clone());
        assert_eq!(until(&["board", "snooze", "x"]), Ok(json!("2026-11-20")));
        assert_eq!(until(&["board", "snooze", "x", "--until", "none"]), Ok(json!("2026-11-20")));
        assert_eq!(until(&["board", "later", "x", "2026-12-01"]), Ok(json!("2026-12-01")));
        // The option wins over the word.
        assert_eq!(until(&["board", "snooze", "x", "mon", "--until", "fri"]), Ok(json!("2026-11-20")));
        assert_eq!(until(&["board", "snooze", "x", "--until", "today"]).unwrap_err().text, "Snooze until a day after today.");
    }

    #[test]
    fn words_and_options_bind_to_their_fields() {
        let c = ctx(false);
        assert_eq!(
            shown(&["board", "+", "Ask Jordan", "--link", "ENG-7", "--urgent", "--link=#958", "--section", "Waiting-On", "--every", "Monday and Friday"], &c),
            Ok(json!({ "op": "board.add", "args": { "title": "Ask Jordan", "links": ["ENG-7", "#958"], "urgent": true, "list": "waiting", "every": "Monday and Friday" } }))
        );
        // Everything after `--` is words.
        assert_eq!(shown(&["board", "add", "--", "--urgent"], &c), Ok(json!({ "op": "board.add", "args": { "title": "--urgent" } })));
        assert_eq!(shown(&["board", "move", "abcd", "--list", "asked", "later"], &c), Ok(json!({ "op": "board.move", "args": { "task": "abcd", "to": "asked" } })));
        assert_eq!(shown(&["log", "--limit", "lots"], &c), Ok(json!({ "op": "app.log", "args": { "limit": 20 } })));
        assert_eq!(shown(&["pr", "view", "https://github.com/acme/app/pull/958"], &c), Ok(json!({ "op": "pr.status", "args": { "number": 958 } })));
        assert_eq!(shown(&["ticket", "read", "eng-42"], &c), Ok(json!({ "op": "ticket.show", "args": { "ticket": "ENG-42" } })));
        assert_eq!(shown(&["manager", "memory", "deploy", "friday"], &c), Ok(json!({ "op": "manager.memory", "args": { "query": "deploy friday", "limit": 20 } })));
    }

    #[test]
    fn nouns_verbs_and_their_other_words() {
        let c = ctx(false);
        let name = |line: &[&str]| parse(&words(line), &c).map(|op| op.name());
        assert_eq!(name(&["board"]), Ok("board.list"));
        assert_eq!(name(&["Today"]), Ok("board.list"));
        assert_eq!(name(&["tasks", "today"]), Ok("board.list"));
        assert_eq!(name(&["MGR"]), Ok("manager.status"));
        assert_eq!(name(&["manager", "followups"]), Ok("manager.follow_up.list"));
        assert_eq!(name(&["accounts", "of", "mine"]), Ok("account.list"));
        assert_eq!(name(&["pending", "--anything"]), Ok("app.approvals"));
        assert_eq!(name(&[]).unwrap_err().text, "Say what to do; `glasscode help` lists the commands.");
        assert_eq!(name(&["frob"]).unwrap_err().text, "Unknown command “frob”. `glasscode help` lists them.");
        assert_eq!(name(&["board", "Frob"]).unwrap_err().text, "Unknown board command “frob”. `glasscode help board` lists them.");
        assert_eq!(name(&["manager", "check", "frob"]).unwrap_err().text, "manager check list | add \"<what>\" --every <minutes> (or --in <minutes>, once) | remove <id>");
    }

    #[test]
    fn commands_still_to_come_say_so() {
        let c = ctx(true);
        for (path, verbs) in NOT_YET {
            for verb in *verbs {
                let line: Vec<&str> = path.iter().copied().chain([*verb, "x"]).collect();
                let error = parse(&words(&line), &c).unwrap_err();
                assert_eq!(error.code, ErrorCode::Failed, "{line:?}");
                assert!(error.text.ends_with("is not in the registry yet (M12.2)."), "{line:?}: {}", error.text);
            }
        }
        assert_eq!(parse(&words(&["Session", "NEW"]), &c).unwrap_err().text, "“session new” is not in the registry yet (M12.2).");
    }

    #[test]
    fn options_are_checked_unless_the_command_never_did() {
        let c = ctx(false);
        assert_eq!(parse(&words(&["board", "show", "x", "--all"]), &c).unwrap_err().text, "Unknown option --all. This command takes: no options.");
        assert_eq!(parse(&words(&["project", "list", "--x"]), &c).unwrap_err().text, "Unknown option --x. This command takes: no options.");
        assert_eq!(
            parse(&words(&["board", "add", "x", "--z", "1", "--colour", "red"]), &c).unwrap_err().text,
            "Unknown option --colour, --z. This command takes: --due, --asked-by, --waiting-on, --with, --via, --list, --section, --link, --note, --urgent, --every."
        );
        for line in [&["status", "--x"][..], &["whoami", "--x"], &["approvals", "--x", "y"]] {
            assert!(parse(&words(line), &c).is_ok(), "{line:?}");
        }
    }
}
