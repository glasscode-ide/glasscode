//! From the `glasscode` command's words to an [`Op`], with no IO: the grammar the commands always
//! had (`control::Args` splits the words and options), now read from the table.
//!
//! What a command line can mean depends on the state of the app only through [`ParseCtx`]: the
//! day it is, who calls, and the sessions there are. Everything else a word names (a task, a
//! project, a ticket) stays a word here and is looked up by the op's handler (M12.2).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::spec::{ArgKind, ArgSpec, At, OpSpec, Options};
use super::{Candidate, ErrorCode, NOUNS, OPS, Op, OpError};
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
    /// The engine switch is v2 (`store::EngineSwitch`, read by the caller off the main thread):
    /// the ops that are `v2_only` are known. With v1 their words are unknown, as they always were.
    pub v2: bool,
}

/// The op the words name, with its arguments; or why not, in the words the commands always used.
pub fn parse(words: &[String], ctx: &ParseCtx) -> Result<Op, OpError> {
    let args = Args::parse(words);
    let (spec, own, verb) = command(&args, ctx.v2)?;
    // With the v2 engine, an op that changes something says what it would do (ops-22).
    let dry = ctx.v2 && spec.dry_runs() && args.has("dry-run");
    if spec.options == Options::Refuse {
        refuse_unknown(spec, &args, dry)?;
        // With the v2 engine a word no argument takes is an error, not dropped: `session new
        // "fix it"` would start a session without its message (its first message is `--message`).
        if ctx.v2 {
            refuse_stray(spec, &args, own)?;
        }
    }
    let mut out = Map::new();
    if dry {
        out.insert("dry_run".into(), Value::Bool(true));
    }
    for arg in spec.args {
        if let Some(value) = value(arg, &args, own, &verb, ctx)? {
            out.insert(arg.name.to_string(), value);
        }
    }
    // A v1 command whose v2 op has another name: with the v2 engine its words, read as they always
    // were, parse to that op (`RENAMED`, `table_journal::V2_WORDS`).
    if ctx.v2
        && let Some((v2, extra)) = super::v2_words(spec.name)
    {
        for (name, value) in extra {
            out.entry(name.to_string()).or_insert_with(|| Value::String(value.to_string()));
        }
        return Ok(Op { spec: v2, args: Value::Object(out) });
    }
    Ok(Op { spec, args: Value::Object(out) })
}

fn usage(text: impl Into<String>) -> OpError {
    OpError::usage(text)
}

/// The name of the op the words name, or None: only the words are read, never a session or a day
/// (glasscoded, which has no window's sessions, names a server session's call with it).
pub fn name_of(words: &[String], v2: bool) -> Option<&'static str> {
    let spec = command(&Args::parse(words), v2).ok()?.0;
    Some(v2.then(|| super::v2_words(spec.name)).flatten().map_or(spec.name, |(v2, _)| v2.name))
}

/// The ops this engine knows: every op with v2, the others with v1.
fn known(v2: bool) -> impl Iterator<Item = &'static OpSpec> {
    OPS.iter().filter(move |op| v2 || !op.v2_only)
}

/// The op the first words name, how many words are its own, and the word that named it (for
/// "Say which task: `glasscode board tick <id>`"). Nouns and verbs are read in any letter case.
fn command(args: &Args, v2: bool) -> Result<(&'static OpSpec, usize, String), OpError> {
    let words: Vec<String> = args.words.iter().map(|w| w.to_lowercase()).collect();
    let mut path: Vec<&'static str> = Vec::new();
    loop {
        let depth = path.len();
        let word = words.get(depth).map(String::as_str);
        let here = || known(v2).filter(|op| op.cli.len() == depth + 1 && op.cli[..depth] == path[..]);
        let Some(word) = word else {
            if let Some(op) = here().find(|op| op.aliases.contains(&"")) {
                return Ok((op, depth, op.cli[depth].to_string()));
            }
            return Err(unknown(&path, ""));
        };
        if let Some(op) = here().find(|op| op.cli[depth] == word || op.aliases.iter().any(|a| !a.is_empty() && *a == word)) {
            return Ok((op, depth + 1, word.to_string()));
        }
        if let Some(next) = noun(&path, word, v2) {
            path.push(next);
            continue;
        }
        if let Some(op) = here().find(|op| op.aliases.contains(&"*")) {
            return Ok((op, depth + 1, word.to_string()));
        }
        return Err(unknown(&path, word));
    }
}

/// The noun after `path` that `word` is, by its own word or another one.
fn noun(path: &[&'static str], word: &str, v2: bool) -> Option<&'static str> {
    let depth = path.len();
    known(v2).filter(|op| op.cli.len() > depth + 1 && op.cli[..depth] == path[..]).map(|op| op.cli[depth]).find(|next| {
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
fn refuse_unknown(spec: &OpSpec, args: &Args, dry: bool) -> Result<(), OpError> {
    let mut known = spec.flags();
    if dry {
        known.push("dry-run");
    }
    let unknown = args.unknown(&known);
    if unknown.is_empty() {
        return Ok(());
    }
    let takes = if known.is_empty() { "no options".to_string() } else { known.iter().map(|k| format!("--{k}")).collect::<Vec<_>>().join(", ") };
    Err(usage(format!("Unknown option {}. This command takes: {takes}.", unknown.join(", "))))
}

/// The words after the op's own that none of its arguments takes.
fn refuse_stray(spec: &OpSpec, args: &Args, own: usize) -> Result<(), OpError> {
    let mut takes = own;
    for arg in spec.args {
        match arg.at {
            // These take every word from their place on (`manager ask what is blocked?`, `ticket
            // move ENG-42 In Review`): no word is stray.
            At::Rest(_) | At::RestAfter(..) | At::FlagOrRest(..) => return Ok(()),
            At::Word(n) | At::FlagOrWord(_, n) => takes = takes.max(own + usize::from(n) + 1),
            _ => {}
        }
    }
    // "…" is the taught words' placeholder for what follows (`session new …`), not a word.
    let stray: Vec<&String> = args.words.get(takes..).unwrap_or_default().iter().filter(|w| !matches!(w.as_str(), "…" | "...")).collect();
    if stray.is_empty() {
        return Ok(());
    }
    let words = stray.iter().map(|w| format!("“{w}”")).collect::<Vec<_>>().join(" ");
    let how = spec.examples.first().copied().unwrap_or(spec.name);
    Err(usage(format!("{words}: no argument of {} takes it. {how}", spec.cli.join(" "))))
}

/// The words given for an argument, if any. A text of only white space is none.
fn given(arg: &ArgSpec, args: &Args, own: usize) -> Option<String> {
    let flag = |names: &[&str]| names.iter().find_map(|n| args.flag(n)).map(str::to_string);
    match arg.at {
        At::Word(n) => args.word(own + usize::from(n)).map(str::to_string),
        At::Rest(n) => Some(args.rest(own + usize::from(n))).filter(|t| !t.trim().is_empty()),
        At::Flag(names) => flag(names),
        At::FlagOrWord(names, n) => flag(names).or_else(|| args.word(own + usize::from(n)).map(str::to_string)),
        At::FlagOrRest(names, n) => flag(names).or_else(|| Some(args.rest(own + usize::from(n))).filter(|t| !t.trim().is_empty())),
        At::RestAfter(names, n) => {
            // An option that is on or off took the words after it as its value: they are the text.
            let taken = names.iter().find_map(|n| args.flag(n)).filter(|v| !matches!(v.to_lowercase().as_str(), "true" | "false" | "yes" | "no" | "1" | "0" | "on" | "off"));
            let text = [taken.unwrap_or_default(), args.rest(own + usize::from(n)).as_str()].join(" ");
            Some(text.trim().to_string()).filter(|t| !t.is_empty())
        }
    }
}

/// An argument's value in the op, or None when it is left out.
fn value(arg: &ArgSpec, args: &Args, own: usize, verb: &str, ctx: &ParseCtx) -> Result<Option<Value>, OpError> {
    match arg.kind {
        ArgKind::Switch => return Ok(arg.at.flags().iter().any(|n| args.has(n)).then_some(Value::Bool(true))),
        ArgKind::Many => {
            let mut all: Vec<String> = arg.at.flags().iter().flat_map(|n| args.all(n)).collect();
            // The rest of the line, a word each (`session keys <session> down enter`).
            if let At::Rest(n) = arg.at {
                all.extend(args.words.iter().skip(own + usize::from(n)).cloned());
            }
            if all.is_empty() && arg.required {
                return Err(usage(arg.missing.replace("{verb}", verb)));
            }
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
        ArgKind::Number => json!(text.trim_start_matches('#').parse::<u64>().map_err(|_| usage(arg.missing.replace("{verb}", verb)))?),
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
/// sessions the word could mean, so that it names one by its id. Handlers that take a session
/// among other words (`open`, `manager show`) look it up here too.
pub fn find_session(word: &str, ctx: &ParseCtx) -> Result<String, OpError> {
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
        let more = if could.len() > 6 { format!(" and {} more", could.len() - 6) } else { String::new() };
        return Err(OpError::new(
            ErrorCode::Ambiguous { candidates: candidates(&could) },
            format!(
                "“{w}” is not a session's id; it could mean {}: {list}{more}. Name a session by its whole id, or “this”: only the person, at a terminal, names one by the start of its id or words from its title.",
                if could.len() == 1 { "1 session".to_string() } else { format!("{} sessions", could.len()) }
            ),
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
            v2: false,
            today: day::from_civil(2026, 11, 19), // a Thursday
            sessions: vec![
                session("8765f51a-8ad5-4e73-9df0-27adaa817aa9", "API connectors", "feat/api-connectors"),
                session("973f0fe4-a51e-4b64-bfbb-779132b51c0a", "Retry queue", "fix/retry-queue"),
                SessionView { review: Some(958), ..session("d740a312-2980-49ad-96b0-17b1bc7bcb50", "Checkout page", "feat/checkout") },
                session("f58e3bf6-02d9-42ec-9a5e-dfdc83148786", "API docs", "docs/api"),
            ],
        }
    }

    fn shown(line: &[&str], ctx: &ParseCtx) -> Result<Value, OpError> {
        parse(&words(line), ctx).map(|op| serde_json::to_value(&op).unwrap())
    }

    /// With v2's stray-word check (#215), the arguments that take every word from their place on
    /// still take them: a question to the manager, a ticket's list.
    #[test]
    fn rest_arguments_take_every_word_with_v2() {
        let c = ParseCtx { v2: true, ..ParseCtx::default() };
        assert_eq!(shown(&["manager", "ask", "what", "is", "blocked?"], &c).unwrap()["args"]["question"], "what is blocked?");
        assert_eq!(shown(&["manager", "ask", "summarise the sessions"], &c).unwrap()["args"]["question"], "summarise the sessions");
        assert_eq!(shown(&["manager", "ask", "--voice", "what", "now?"], &c).unwrap()["args"]["question"], "what now?");
        assert!(shown(&["session", "new", "fix it"], &ParseCtx { caller_is_person: true, ..c.clone() }).is_err(), "a stray word is still an error");
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
            assert!(refused.text.contains(retry) && refused.text.contains("whole id") && refused.text.contains("could mean 1 session:"), "{}", refused.text);
        }
        assert_eq!(session_of(&["session", "show", "#958"], &person).as_deref(), Ok("d740a312-2980-49ad-96b0-17b1bc7bcb50"));
        assert!(session_of(&["session", "show", "#958"], &agent).is_err());
        // Several: the person is told which, as always; so is any other caller.
        let both = session_of(&["session", "show", "api"], &person).unwrap_err();
        assert_eq!(both.text, "“api” matches 2 sessions: 8765f51a (API connectors), f58e3bf6 (API docs). Use its id.");
        assert!(matches!(&both.code, ErrorCode::Ambiguous { candidates } if candidates.len() == 2));
        assert!(matches!(session_of(&["session", "show", "api"], &agent).unwrap_err().code, ErrorCode::Ambiguous { candidates } if candidates.len() == 2));
        // Words from the title are all in it, not any of them.
        assert_eq!(session_of(&["session", "show", "api docs"], &person).as_deref(), Ok("f58e3bf6-02d9-42ec-9a5e-dfdc83148786"));
        // More than six: six of them, and how many more.
        let mut many = ctx(false);
        many.sessions = (0..8).map(|i| session(&format!("aaaa{i}000-0000-0000-0000-000000000000"), &format!("Fix {i}"), "fix")).collect();
        let refused = session_of(&["session", "show", "fix"], &many).unwrap_err();
        assert!(refused.text.contains("could mean 8 sessions:") && refused.text.ends_with("title.") && refused.text.contains(" and 2 more."), "{}", refused.text);
        assert!(matches!(&refused.code, ErrorCode::Ambiguous { candidates } if candidates.len() == 6));
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
            Ok(json!({ "op": "board.add", "args": { "title": "Ask Jordan", "links": ["ENG-7", "#958"], "urgent": true, "list": "waiting", "every": "Monday and Friday", "as_written": true } }))
        );
        // An option v1 read as "given" even with no value: the title is taken as written; the
        // board lists every section.
        assert_eq!(shown(&["board", "add", "Call Sam by friday", "--due"], &c), Ok(json!({ "op": "board.add", "args": { "title": "Call Sam by friday", "as_written": true } })));
        assert_eq!(shown(&["board", "add", "Call Sam by friday", "--note", "x"], &c), Ok(json!({ "op": "board.add", "args": { "title": "Call Sam by friday", "note": "x" } })));
        assert_eq!(shown(&["board", "--section"], &c), Ok(json!({ "op": "board.list", "args": { "all": true } })));
        assert_eq!(shown(&["board", "--section", "due"], &c), Ok(json!({ "op": "board.list", "args": { "all": true, "section": "due" } })));
        assert_eq!(parse(&words(&["board", "--x"]), &c).unwrap_err().text, "Unknown option --x. This command takes: --all, --section.");
        // Switched off: not on.
        assert_eq!(shown(&["board", "edit", "abcd", "--urgent=false"], &c), Ok(json!({ "op": "board.edit", "args": { "task": "abcd" } })));
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

    /// With the v2 engine an op that changes something takes `--dry-run`; with v1 it is the
    /// unknown option it always was, and a read never takes it.
    #[test]
    fn dry_runs_are_the_v2_engines() {
        let mut c = ctx(true);
        c.v2 = true;
        let op = parse(&words(&["board", "done", "zysg", "--dry-run"]), &c).unwrap();
        assert!(op.dry_run() && op.text("task") == Some("zysg"));
        assert!(!parse(&words(&["board", "done", "zysg"]), &c).unwrap().dry_run());
        assert!(parse(&words(&["session", "list", "--dry-run"]), &c).unwrap_err().text.starts_with("Unknown option --dry-run"));
        // `worktree clean` has its own.
        let clean = parse(&words(&["worktree", "clean", "--dry-run"]), &c).unwrap();
        assert!(clean.on("dry_run") && !clean.dry_run());
        c.v2 = false;
        assert!(parse(&words(&["board", "done", "zysg", "--dry-run"]), &c).unwrap_err().text.starts_with("Unknown option --dry-run"));
    }

    #[test]
    fn ops_for_the_v2_engine_are_unknown_words_with_v1() {
        let mut c = ctx(true);
        let v2_only: Vec<&OpSpec> = OPS.iter().filter(|op| op.v2_only).collect();
        assert!(!v2_only.is_empty());
        for op in &v2_only {
            let line = words(op.cli);
            c.v2 = false;
            let error = parse(&line, &c).unwrap_err();
            assert_eq!(error.code, ErrorCode::Usage, "{}", op.name);
            assert!(error.text.starts_with("Unknown"), "{}: {}", op.name, error.text);
            assert_eq!(name_of(&line, false), None, "{}", op.name);
            c.v2 = true;
            assert_eq!(name_of(&line, true), Some(op.name));
        }
        c.v2 = false;
        assert_eq!(parse(&words(&["switch", "units"]), &c).unwrap_err().text, "Unknown command “switch”. `glasscode help` lists them.");
        assert_eq!(parse(&words(&["worktree", "move", "x"]), &c).unwrap_err().text, "Unknown worktree command “move”. `glasscode help worktree` lists them.");
        c.v2 = true;
        assert_eq!(shown(&["switch", "move", "--to", "server", "--now"], &c), Ok(json!({ "op": "switch.move", "args": { "to": "server", "now": true, "parallel": 4 } })));
        assert_eq!(parse(&words(&["switch", "frob"]), &c).unwrap_err().text, "Unknown switch command “frob”. `glasscode help` lists them.");
    }

    #[test]
    fn words_an_option_took_and_numbers_as_given() {
        let c = ctx(true);
        // `--voice` is not a switch of `Args`: it takes the words after it, which are the question.
        assert_eq!(shown(&["manager", "ask", "--voice", "what is blocked?"], &c), Ok(json!({ "op": "manager.ask", "args": { "question": "what is blocked?", "voice": true } })));
        assert_eq!(shown(&["manager", "ask", "what is blocked?", "--voice"], &c), Ok(json!({ "op": "manager.ask", "args": { "question": "what is blocked?", "voice": true } })));
        assert_eq!(shown(&["manager", "ask", "--voice=false", "what", "is", "blocked?"], &c), Ok(json!({ "op": "manager.ask", "args": { "question": "what is blocked?" } })));
        assert_eq!(parse(&words(&["manager", "ask", "--voice"]), &c).unwrap_err().text, "manager ask \"<question>\" [--voice]");
        // The option, else the rest of the line.
        assert_eq!(shown(&["ticket", "move", "eng-1", "In", "Review"], &c), Ok(json!({ "op": "ticket.move", "args": { "ticket": "ENG-1", "to": "In Review" } })));
        assert_eq!(shown(&["ticket", "move", "eng-1", "In", "--to", "Done"], &c), Ok(json!({ "op": "ticket.move", "args": { "ticket": "ENG-1", "to": "Done" } })));
        // A number as these commands read it: "#" or not, never a URL, and their own usage.
        assert_eq!(shown(&["wait", "#7"], &c), Ok(json!({ "op": "app.wait", "args": { "id": 7 } })));
        assert_eq!(parse(&words(&["wait", "soon"]), &c).unwrap_err().text, "wait <id>: the number the waiting command printed.");
        assert_eq!(parse(&words(&["pr", "rerun", "https://github.com/acme/app/pull/9"]), &c).unwrap_err().text, "pr rerun <number> …");
        assert_eq!(shown(&["pr", "comment", "#9", "Looks", "good"], &c), Ok(json!({ "op": "pr.comment", "args": { "number": 9, "text": "Looks good" } })));
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
