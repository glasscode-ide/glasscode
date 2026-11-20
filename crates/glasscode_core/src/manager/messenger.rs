//! Messages to and from teammates, through a messenger program the person sets
//! (`Settings::messenger`: a program on the PATH or a path to one; empty means no messenger).
//! Glasscode knows no messaging service itself: the program is the bridge to whatever the
//! person uses (a chat service, email, anything), and it is called in four ways:
//!
//! - `<program> whoami` prints `{"id": …, "name": …, "email": …}`: who the person is there.
//! - `<program> people <query>` prints `[{"id": …, "name": …, "email": …}]`.
//! - `<program> send --to <contact> [--urgent]` reads the text on stdin and sends it.
//! - `<program> inbox --since <unix seconds>` prints
//!   `[{"id", "chat", "from", "text", "at", "urgent", "handled"}]`: what others wrote since.
//!
//! JSON on stdout; exit 0 when it worked, 3 when the person is not signed in, anything else
//! with a line on stderr saying why. `docs/manager.md` has the whole protocol.
//!
//! Reading is free; `send` is only ever called for a message the person allowed
//! (`messages::ready`). The process call is a thin wrapper (`call`, `run`); what comes back is
//! read by pure functions (`outcome`, `parse_*`).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{InMessage, Settings};

/// Someone as the messenger knows them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Contact {
    pub id: String,
    pub name: String,
    pub email: String,
}

/// Why a call to the messenger did not work. Each reads as a sentence for the person.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MessengerError {
    /// `Settings::messenger` is empty.
    #[error("No messenger is set up. Choose one in Settings › Manager.")]
    NotSet,
    /// The program that is set is not on the PATH (or not at that path).
    #[error("The messenger program “{0}” was not found. Check it in Settings › Manager.")]
    NotFound(String),
    /// The program said no (exit code 3): the person is not signed in there.
    #[error("The messenger is {0}.")]
    Refused(String),
    #[error("The messenger did not do it: {0}")]
    Failed(String),
}

/// How long one call may take.
const LIMIT: Duration = Duration::from_secs(30);
/// How long `available` trusts its last answer.
const AVAILABLE_FOR: Duration = Duration::from_secs(60);
/// `incoming` never looks further back than this, whatever `since` says: the first round
/// must not bring in weeks of old messages as if they were new.
const LOOK_BACK_SECS: u64 = 3 * 24 * 3600;

/// A messenger is set, its program is there, and it says who the person is. Asked at most
/// once a minute.
pub fn available(settings: &Settings, env: &HashMap<String, String>) -> bool {
    static LAST: Mutex<Option<(PathBuf, Instant, bool)>> = Mutex::new(None);
    let Ok(program) = program(settings, env) else { return false };
    if let Ok(last) = LAST.lock()
        && let Some((path, at, ok)) = last.as_ref()
        && *path == program
        && at.elapsed() < AVAILABLE_FOR
    {
        return *ok;
    }
    let ok = me(settings, env).is_ok();
    if let Ok(mut last) = LAST.lock() {
        *last = Some((program, Instant::now(), ok));
    }
    ok
}

/// Who the person is in the messenger.
pub fn me(settings: &Settings, env: &HashMap<String, String>) -> anyhow::Result<Contact> {
    Ok(parse_contact(&answer(&call(settings, &["whoami"], None, env)?)?))
}

/// People the messenger knows by this name or address.
pub fn find_person(settings: &Settings, query: &str, env: &HashMap<String, String>) -> anyhow::Result<Vec<Contact>> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    Ok(parse_people(&answer(&call(settings, &["people", query], None, env)?)?))
}

/// Send a text to a contact (`Person::contact`, or the teammate's name). Only for a message
/// the person allowed. `urgent` is passed on as `--urgent`; what that means is the program's
/// business.
pub fn send(settings: &Settings, to: &str, body: &str, urgent: bool, env: &HashMap<String, String>) -> anyhow::Result<()> {
    let (to, body) = (to.trim(), body.trim());
    // Nothing set: say that first, whatever else is missing.
    program(settings, env)?;
    anyhow::ensure!(!to.is_empty(), "it does not say who the message is for");
    anyhow::ensure!(!body.is_empty(), "there is nothing to send");
    let mut args = vec!["send", "--to", to];
    if urgent {
        args.push("--urgent");
    }
    call(settings, &args, Some(body), env)?;
    Ok(())
}

/// Messages from others since `since` (unix seconds), oldest first.
///
/// What the person wrote themself is never in it (nor what the manager sent in their name):
/// the program is first asked who the person is, and without an answer nothing is read at
/// all, so the manager can never react to its own message.
pub fn incoming(settings: &Settings, since: u64, env: &HashMap<String, String>) -> anyhow::Result<Vec<InMessage>> {
    let me = me(settings, env)?;
    anyhow::ensure!(knows(&me), MessengerError::Failed("it did not say who you are, so your own messages cannot be told from others'".into()));
    let since = since.max(crate::model::now().saturating_sub(LOOK_BACK_SECS));
    let inbox = answer(&call(settings, &["inbox", "--since", &since.to_string()], None, env)?)?;
    Ok(parse_inbox(&inbox, &me, since))
}

// ----- the process ------------------------------------------------------------------------------

/// The program that is set: found on the PATH of `env`, or at the path it is given as
/// (`~/…` is the person's home folder).
fn program(settings: &Settings, env: &HashMap<String, String>) -> Result<PathBuf, MessengerError> {
    let set = settings.messenger.trim();
    if set.is_empty() {
        return Err(MessengerError::NotSet);
    }
    let found = if let Some(rest) = set.strip_prefix("~/") {
        let home = env.get("HOME").filter(|h| !h.is_empty()).map(PathBuf::from).unwrap_or_else(crate::paths::home);
        Some(home.join(rest)).filter(|p| p.is_file())
    } else if set.contains('/') {
        Some(PathBuf::from(set)).filter(|p| p.is_file())
    } else {
        crate::agents::resolve_program(set, env)
    };
    found.ok_or_else(|| MessengerError::NotFound(set.to_string()))
}

/// Call the program and give what it wrote to stdout.
fn call(settings: &Settings, args: &[&str], stdin: Option<&str>, env: &HashMap<String, String>) -> Result<String, MessengerError> {
    // `sys::command`: on Windows it also starts a `.cmd` or `.bat` program, with no console window.
    let mut cmd = crate::sys::command(program(settings, env)?);
    cmd.envs(env).args(args);
    run(cmd, stdin)
}

/// Run it, without a window and without waiting for ever, with `input` on its stdin.
fn run(mut cmd: Command, input: Option<&str>) -> Result<String, MessengerError> {
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|err| MessengerError::Failed(format!("the program could not be started ({err})")))?;
    // Its pipes are read and written on the side, so a long text cannot block either end.
    if let Some(mut stdin) = child.stdin.take() {
        let text = input.unwrap_or_default().to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });
    }
    let pipe = |mut pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(pipe) = pipe.as_mut() {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = pipe(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = pipe(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let deadline = Instant::now() + LIMIT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(MessengerError::Failed(format!("it did not answer within {}s; a message being sent may or may not have gone out", LIMIT.as_secs())));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(err) => return Err(MessengerError::Failed(format!("the program could not be waited for ({err})"))),
        }
    };
    outcome(status.code(), &out.join().unwrap_or_default(), &err.join().unwrap_or_default())
}

/// What a finished call means: what it printed, or why there is nothing. The program exits
/// with 0 when it worked and 3 when the person is not signed in; anything else is an error it
/// describes on stderr.
fn outcome(code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> Result<String, MessengerError> {
    let wrote = String::from_utf8_lossy(stderr).lines().map(str::trim).find(|l| !l.is_empty()).map(|l| l.chars().take(300).collect::<String>());
    match code {
        Some(0) => Ok(String::from_utf8_lossy(stdout).into_owned()),
        Some(3) => Err(MessengerError::Refused(match wrote {
            Some(why) => format!("not signed in ({why})"),
            None => "not signed in".into(),
        })),
        Some(code) => Err(MessengerError::Failed(wrote.unwrap_or_else(|| format!("the program stopped with code {code}")))),
        None => Err(MessengerError::Failed(wrote.unwrap_or_else(|| "the program was stopped".into()))),
    }
}

/// The JSON a call printed. `{"error": "…"}` is an error too.
fn answer(stdout: &str) -> Result<Value, MessengerError> {
    let value: Value = serde_json::from_str(stdout.trim()).map_err(|_| MessengerError::Failed("the program answered with something that is not JSON".into()))?;
    match value.get("error").filter(|e| !e.is_null()) {
        Some(error) => Err(MessengerError::Failed(error.as_str().map(str::to_string).or_else(|| error.get("message").and_then(Value::as_str).map(str::to_string)).unwrap_or_else(|| error.to_string()).chars().take(300).collect())),
        None => Ok(value),
    }
}

// ----- reading what came back -------------------------------------------------------------------

/// A field as text: a string (trimmed), or a number.
fn text(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// The list in an answer: the answer itself, or what it holds under this name.
fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value.as_array().or_else(|| value.get(key).and_then(Value::as_array)).map(Vec::as_slice).unwrap_or_default()
}

fn parse_contact(value: &Value) -> Contact {
    Contact { id: text(value, "id"), name: text(value, "name"), email: text(value, "email") }
}

/// The people of `people <query>`: those with something to address them by.
fn parse_people(answer: &Value) -> Vec<Contact> {
    list(answer, "people").iter().map(parse_contact).filter(|p| !p.id.is_empty() || !p.name.is_empty() || !p.email.is_empty()).collect()
}

/// Whether the program said anything about who the person is.
fn knows(me: &Contact) -> bool {
    !me.id.is_empty() || !me.name.is_empty() || !me.email.is_empty()
}

/// Who wrote a message: `from` as a name (or an id or address), or as `{id, name, email}`.
fn sender(message: &Value) -> Option<Contact> {
    let from = message.get("from")?;
    let who = match from {
        Value::String(name) => Contact { name: name.trim().to_string(), ..Default::default() },
        Value::Object(_) => parse_contact(from),
        _ => return None,
    };
    knows(&who).then_some(who)
}

/// Whether a message's writer is the person themself: any of what stands for the writer is
/// the person's id, name or address (letter case does not count).
fn is_me(from: &Contact, me: &Contact) -> bool {
    let mine = [&me.id, &me.name, &me.email];
    [&from.id, &from.name, &from.email].into_iter().filter(|f| !f.is_empty()).any(|f| mine.iter().any(|m| !m.is_empty() && m.to_lowercase() == f.to_lowercase()))
}

/// The messages of `inbox` that other people wrote after `since`, oldest first. Without
/// knowing who the person is, nothing counts as written by someone else.
fn parse_inbox(answer: &Value, me: &Contact, since: u64) -> Vec<InMessage> {
    let mut found = Vec::new();
    if !knows(me) {
        return found;
    }
    for message in list(answer, "messages") {
        let Some(at) = message.get("at").and_then(parse_time) else { continue };
        if at <= since {
            continue;
        }
        let Some(from) = sender(message) else { continue };
        if is_me(&from, me) {
            continue;
        }
        let body = plain(&text(message, "text"));
        if body.is_empty() {
            continue;
        }
        let from = [from.name, from.email, from.id].into_iter().find(|f| !f.is_empty()).unwrap_or_default();
        let id = text(message, "id");
        let flag = |key: &str| message.get(key).and_then(Value::as_bool).unwrap_or(false);
        found.push(InMessage { id: if id.is_empty() { format!("{from}|{at}") } else { id }, chat: text(message, "chat"), urgent: flag("urgent") || says_urgent(&body), handled: flag("handled"), from, text: body, at });
    }
    found.sort_by_key(|m| m.at);
    found
}

/// The text with every line's spaces squeezed and empty lines left out.
fn plain(text: &str) -> String {
    let lines: Vec<String> = text.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|l| !l.is_empty()).collect();
    lines.join("\n")
}

/// Whether the text itself says it is urgent ("urgent", "asap", "prod is down" …), in
/// English or German. "Not urgent" and "no rush" say the opposite.
fn says_urgent(text: &str) -> bool {
    let low = text.to_lowercase().replace('’', "'").replace("n't", " not");
    let mut words = String::from(" ");
    for c in low.chars() {
        if c.is_alphanumeric() {
            words.push(c);
        } else if !words.ends_with(' ') {
            words.push(' ');
        }
    }
    if !words.ends_with(' ') {
        words.push(' ');
    }
    for calm in ["not urgent", "not that urgent", "not very urgent", "not so urgent", "nothing urgent", "non urgent", "not an emergency", "no emergency", "no rush", "nicht dringend", "nicht so dringend", "kein notfall", "keine eile"] {
        words = words.replace(&format!(" {calm} "), "  ");
    }
    [
        "urgent", "urgently", "asap", "emergency", "prod is down", "production is down", "prod down", "production down", "prod is broken", "production is broken", "site is down", "outage", "sev1", "sev 1", "p0", "dringend", "notfall", "sofort",
    ]
    .iter()
    .any(|sign| words.contains(&format!(" {sign} ")))
}

/// Unix seconds from a number (seconds, or milliseconds when it is that large) or from an
/// ISO time (`2026-11-15T09:30:00.123Z`, `…+01:00`).
pub(crate) fn parse_time(value: &Value) -> Option<u64> {
    let seconds = |n: u64| if n > 100_000_000_000 { n / 1000 } else { n };
    if let Some(n) = value.as_u64() {
        return Some(seconds(n));
    }
    if let Some(n) = value.as_f64() {
        return (n >= 0.0).then(|| seconds(n as u64));
    }
    let text = value.as_str()?.trim();
    if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) {
        return text.parse().ok().map(seconds);
    }
    let day = crate::day::parse_iso(text)?;
    let rest = text.get(10..)?;
    let mut secs = day * 86_400;
    let Some(clock) = rest.strip_prefix(['T', 't', ' ']) else { return rest.is_empty().then_some(secs as u64) };
    let number = |range: std::ops::Range<usize>| clock.get(range).and_then(|s| s.parse::<i64>().ok());
    let (hours, minutes) = (number(0..2)?, number(3..5)?);
    let (seconds, mut zone) = match number(6..8) {
        Some(seconds) if clock.as_bytes().get(5) == Some(&b':') => (seconds, clock.get(8..)?),
        _ => (0, clock.get(5..)?),
    };
    if hours > 23 || minutes > 59 || seconds > 60 {
        return None;
    }
    secs += hours * 3600 + minutes * 60 + seconds;
    // Fractions of a second.
    if let Some(fraction) = zone.strip_prefix(['.', ',']) {
        zone = fraction.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    match zone.trim() {
        "" | "Z" | "z" => {}
        offset => {
            let sign = match offset.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let digits: String = offset[1..].chars().filter(char::is_ascii_digit).collect();
            if digits.len() != 4 && digits.len() != 2 {
                return None;
            }
            let hours: i64 = digits[..2].parse().ok()?;
            let minutes: i64 = if digits.len() == 4 { digits[2..].parse().ok()? } else { 0 };
            secs -= sign * (hours * 3600 + minutes * 60);
        }
    }
    u64::try_from(secs).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn alex() -> Contact {
        Contact { id: "u-alex".into(), name: "Alex Example".into(), email: "alex@example.com".into() }
    }

    fn message(id: &str, from: &str, at: u64, text: &str) -> Value {
        json!({ "id": id, "chat": format!("chat-{}", from.to_lowercase().replace(' ', "-")), "from": from, "text": text, "at": at, "urgent": false, "handled": false })
    }

    fn unix(iso: &str) -> u64 {
        parse_time(&json!(iso)).unwrap()
    }

    #[test]
    fn reads_times() {
        assert_eq!(parse_time(&json!(1_794_735_000u64)), Some(1_794_735_000));
        assert_eq!(parse_time(&json!(1_794_735_000_123u64)), Some(1_794_735_000));
        assert_eq!(parse_time(&json!(1_794_735_000.7)), Some(1_794_735_000));
        assert_eq!(parse_time(&json!(1_794_735_000_123.0)), Some(1_794_735_000));
        assert_eq!(parse_time(&json!("1794735000123")), Some(1_794_735_000));
        assert_eq!(unix("1970-01-01T00:00:00Z"), 0);
        assert_eq!(unix("2026-11-15T09:30:00.123Z"), 1_794_735_000);
        assert_eq!(unix("2026-11-15T10:30:00+01:00"), 1_794_735_000);
        assert_eq!(unix("2026-11-15T04:00:00.5-05:30"), 1_794_735_000);
        assert_eq!(unix("2026-11-15T09:30Z"), 1_794_735_000);
        assert_eq!(unix("2026-11-15 09:30:00"), 1_794_735_000);
        assert_eq!(unix("2026-11-15"), 1_794_700_800);
        assert_eq!(unix(&crate::linear::iso(1_794_735_000)), 1_794_735_000);
        for bad in [json!("yesterday"), json!("2026-13-40T00:00:00Z"), json!("2026-11-15T25:00:00Z"), json!("2026-11-15T09:30:00 CET"), json!(""), json!(-5), json!(null), json!({})] {
            assert_eq!(parse_time(&bad), None, "{bad}");
        }
    }

    #[test]
    fn exit_codes_say_what_went_wrong() {
        assert_eq!(outcome(Some(0), b"[]\n", b"a warning nobody reads"), Ok("[]\n".to_string()));
        assert_eq!(outcome(Some(3), b"", b""), Err(MessengerError::Refused("not signed in".into())));
        assert_eq!(outcome(Some(3), b"", b"\n  run `chat login` first\nmore\n"), Err(MessengerError::Refused("not signed in (run `chat login` first)".into())));
        assert_eq!(outcome(Some(1), b"", b"\n  nobody called Sam\nmore\n"), Err(MessengerError::Failed("nobody called Sam".into())));
        assert_eq!(outcome(Some(2), b"ignored", b""), Err(MessengerError::Failed("the program stopped with code 2".into())));
        assert_eq!(outcome(None, b"", b""), Err(MessengerError::Failed("the program was stopped".into())));
    }

    #[test]
    fn errors_read_as_sentences() {
        assert_eq!(MessengerError::NotSet.to_string(), "No messenger is set up. Choose one in Settings › Manager.");
        assert_eq!(MessengerError::NotFound("chat-bridge".into()).to_string(), "The messenger program “chat-bridge” was not found. Check it in Settings › Manager.");
        assert_eq!(MessengerError::Refused("not signed in".into()).to_string(), "The messenger is not signed in.");
        assert_eq!(MessengerError::Failed("nobody called Sam".into()).to_string(), "The messenger did not do it: nobody called Sam");
        // It is an error like any other.
        let error: Box<dyn std::error::Error> = Box::new(MessengerError::NotSet);
        assert!(error.source().is_none());
    }

    #[test]
    fn an_answer_is_json_and_may_be_an_error() {
        assert_eq!(answer(" {\"id\": \"u1\"}\n"), Ok(json!({ "id": "u1" })));
        assert_eq!(answer("[]"), Ok(json!([])));
        assert_eq!(answer("hello"), Err(MessengerError::Failed("the program answered with something that is not JSON".into())));
        assert_eq!(answer(""), Err(MessengerError::Failed("the program answered with something that is not JSON".into())));
        assert_eq!(answer(r#"{"error": "nope"}"#), Err(MessengerError::Failed("nope".into())));
        assert_eq!(answer(r#"{"error": {"message": "The chat was not found."}}"#), Err(MessengerError::Failed("The chat was not found.".into())));
        assert_eq!(answer(r#"{"error": null, "id": "u1"}"#), Ok(json!({ "error": null, "id": "u1" })));
    }

    #[test]
    fn reads_who_the_person_is_and_people() {
        assert_eq!(parse_contact(&json!({ "id": "u-alex", "name": " Alex Example ", "email": "alex@example.com", "more": true })), alex());
        assert_eq!(parse_contact(&json!({ "id": 42, "name": null })), Contact { id: "42".into(), ..Default::default() });
        assert_eq!(parse_contact(&json!({})), Contact::default());
        assert_eq!(parse_contact(&json!("Alex")), Contact::default());

        let people = parse_people(&json!([
            { "id": "u-sam", "name": "Sam Example", "email": "sam@example.com" },
            { "name": "Mira Example" },
            { "email": "" },
            "not a person"
        ]));
        assert_eq!(people, vec![Contact { id: "u-sam".into(), name: "Sam Example".into(), email: "sam@example.com".into() }, Contact { name: "Mira Example".into(), ..Default::default() }]);
        assert_eq!(parse_people(&json!({ "people": [{ "id": "u-sam" }] })).len(), 1, "a list under its name");
        assert!(parse_people(&json!({ "people": "none" })).is_empty());
        assert!(parse_people(&json!(null)).is_empty());
    }

    #[test]
    fn incoming_is_what_others_wrote_after_since() {
        let since = 1_000;
        let inbox = json!([
            message("m5", "Sam Example", 1_300, "Second line"),
            message("m4", "Alex Example", 1_200, "My own answer"),
            message("m3", "Sam Example", 1_100, "First line\n  with   spaces \n\n"),
            message("m2", "Sam Example", 1_000, "Exactly at since"),
            message("m1", "Sam Example", 900, "Old"),
            { "id": "m6", "from": "Sam Example", "at": 1_400, "text": "  " },
            { "id": "m7", "from": null, "at": 1_500, "text": "from nobody" },
            { "id": "m8", "from": "", "at": 1_500, "text": "from nobody either" },
            { "id": "m9", "from": "Sam Example", "at": "no time", "text": "no time" },
            { "id": "m10", "from": "Sam Example", "text": "no time at all" },
            { "from": "Mira Example", "at": "1970-01-01T00:26:40Z", "text": "No id, an ISO time", "handled": true, "urgent": true },
            "not a message"
        ]);
        let found = parse_inbox(&inbox, &alex(), since);
        assert_eq!(found.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(), vec!["First line\nwith spaces", "Second line", "No id, an ISO time"]);
        let first = &found[0];
        assert_eq!((first.id.as_str(), first.chat.as_str(), first.from.as_str(), first.at, first.urgent, first.handled), ("m3", "chat-sam-example", "Sam Example", 1_100, false, false));
        let last = &found[2];
        assert_eq!((last.id.as_str(), last.chat.as_str(), last.at, last.urgent, last.handled), ("Mira Example|1600", "", 1_600, true, true));
        // A list under its name reads the same; anything else is an empty inbox.
        assert_eq!(parse_inbox(&json!({ "messages": inbox }), &alex(), since), found);
        assert!(parse_inbox(&json!({ "messages": 3 }), &alex(), since).is_empty());
        assert!(parse_inbox(&json!(null), &alex(), since).is_empty());
    }

    #[test]
    fn the_persons_own_messages_are_never_incoming() {
        let one = |from: Value, me: &Contact| parse_inbox(&json!([{ "id": "1", "from": from, "at": 500, "text": "hello" }]), me, 0).len();
        // By name, id or address, in any case of letters.
        assert_eq!(one(json!("alex example"), &alex()), 0);
        assert_eq!(one(json!("U-ALEX"), &alex()), 0);
        assert_eq!(one(json!("Alex@Example.com"), &alex()), 0);
        assert_eq!(one(json!("Sam Example"), &alex()), 1);
        // The writer as an object: any of its fields that is the person's makes it their own.
        assert_eq!(one(json!({ "id": "u-alex", "name": "Someone Else" }), &alex()), 0);
        assert_eq!(one(json!({ "id": "u-sam", "name": "Alex Example" }), &alex()), 0);
        assert_eq!(one(json!({ "email": "ALEX@example.com" }), &alex()), 0);
        assert_eq!(one(json!({ "id": "u-sam", "name": "Sam Example" }), &alex()), 1);
        assert_eq!(one(json!({}), &alex()), 0, "a writer nothing is known about is not incoming");
        assert_eq!(one(json!(7), &alex()), 0);
        // The person known by name only.
        let named = Contact { name: "Alex Example".into(), ..Default::default() };
        assert_eq!(one(json!("Alex Example"), &named), 0);
        assert_eq!(one(json!({ "id": "u-sam" }), &named), 1);
        // Nothing known about the person: nothing is incoming.
        assert_eq!(one(json!("Sam Example"), &Contact::default()), 0);
        assert_eq!(one(json!({ "id": "u-sam", "name": "Sam Example" }), &Contact::default()), 0);
    }

    #[test]
    fn urgent_is_what_the_messenger_or_the_text_says() {
        let urgent = |text: &str, flag: bool| parse_inbox(&json!([{ "id": "1", "from": "Sam Example", "at": 500, "text": text, "urgent": flag }]), &alex(), 0)[0].urgent;
        assert!(urgent("Can you look at this?", true));
        assert!(!urgent("Can you look at this?", false));
        for text in ["URGENT: login is broken", "Need this ASAP!", "This is an emergency.", "prod is down", "Production is down since 9", "We have an outage", "Call me, it's urgent", "Das ist dringend", "sev1 on checkout", "Please answer urgently"] {
            assert!(urgent(text, false), "{text:?}");
        }
        for text in ["Not urgent, but have a look", "It isn't urgent", "No rush on #42", "Nothing urgent today", "Nicht dringend", "The download is slow", "How was the insurgent movie?", "Lunch?", ""] {
            assert!(!says_urgent(text), "{text:?}");
        }
        assert!(urgent("not urgent", true), "what the messenger says still counts");
        assert!(says_urgent("Not urgent for me, but prod is down for Sam"));
    }

    // ----- the process, against a stand-in for a messenger program -------------------------------

    /// A folder with a program `chat-bridge` that logs how it was called (and what it was
    /// given on stdin) and answers from files: `<call>.json` is printed, `<call>.code` is the
    /// exit code (0 without it), `<call>.err` goes to stderr.
    #[cfg(unix)]
    fn stand_in() -> (tempfile::TempDir, Settings, HashMap<String, String>) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/calls.log"
call=$1
[ "$call" = send ] && cat > "$dir/sent.txt"
[ -f "$dir/$call.json" ] && cat "$dir/$call.json"
[ -f "$dir/$call.err" ] && cat "$dir/$call.err" >&2
[ -f "$dir/$call.code" ] && exit "$(cat "$dir/$call.code")"
exit 0
"#;
        let path = dir.path().join("chat-bridge");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = HashMap::from([("PATH".to_string(), format!("{}:/usr/bin:/bin", dir.path().display()))]);
        (dir, Settings { messenger: "chat-bridge".into(), ..Settings::default() }, env)
    }

    #[cfg(unix)]
    fn answers(dir: &tempfile::TempDir, call: &str, value: Value) {
        std::fs::write(dir.path().join(format!("{call}.json")), value.to_string()).unwrap();
    }

    #[cfg(unix)]
    fn calls(dir: &tempfile::TempDir) -> Vec<String> {
        std::fs::read_to_string(dir.path().join("calls.log")).unwrap_or_default().lines().map(str::to_string).collect()
    }

    #[cfg(unix)]
    fn kind(error: &anyhow::Error) -> Option<&MessengerError> {
        error.downcast_ref::<MessengerError>()
    }

    #[cfg(unix)]
    #[test]
    fn finds_the_program_by_name_or_by_path() {
        let (dir, settings, env) = stand_in();
        let there = dir.path().join("chat-bridge");
        assert_eq!(program(&settings, &env), Ok(there.clone()));
        // A path, also with spaces around it, and one under the person's home folder.
        let by_path = Settings { messenger: format!("  {}  ", there.display()), ..Settings::default() };
        assert_eq!(program(&by_path, &HashMap::new()), Ok(there.clone()));
        let home = HashMap::from([("HOME".to_string(), dir.path().display().to_string())]);
        assert_eq!(program(&Settings { messenger: "~/chat-bridge".into(), ..Settings::default() }, &home), Ok(there));
        // Not set, not on the PATH, not at the path.
        assert_eq!(program(&Settings::default(), &env), Err(MessengerError::NotSet));
        assert_eq!(program(&Settings { messenger: "  ".into(), ..Settings::default() }, &env), Err(MessengerError::NotSet));
        assert_eq!(program(&settings, &HashMap::from([("PATH".to_string(), "/nowhere".to_string())])), Err(MessengerError::NotFound("chat-bridge".into())));
        assert_eq!(program(&settings, &HashMap::new()), Err(MessengerError::NotFound("chat-bridge".into())));
        assert_eq!(program(&Settings { messenger: "/nowhere/chat-bridge".into(), ..Settings::default() }, &env), Err(MessengerError::NotFound("/nowhere/chat-bridge".into())));
        assert_eq!(program(&Settings { messenger: "~/no-such-program".into(), ..Settings::default() }, &home), Err(MessengerError::NotFound("~/no-such-program".into())));
    }

    #[cfg(unix)]
    #[test]
    fn messenger_contract_unchanged() {
        // The four calls, word for word, as docs/manager.md has them: whoami; people <query>;
        // send --to <contact> [--urgent] with the text on stdin; inbox --since <unix seconds>.
        let (dir, settings, env) = stand_in();
        answers(&dir, "whoami", json!({ "id": "u-alex", "name": "Alex Example", "email": "alex@example.com" }));
        answers(&dir, "people", json!([]));
        answers(&dir, "inbox", json!([]));
        me(&settings, &env).unwrap();
        find_person(&settings, "Sam", &env).unwrap();
        send(&settings, "u-sam", "hello", false, &env).unwrap();
        send(&settings, "u-sam", "hello", true, &env).unwrap();
        incoming(&settings, 1_800_000_000, &env).unwrap();
        assert_eq!(calls(&dir), ["whoami", "people Sam", "send --to u-sam", "send --to u-sam --urgent", "whoami", "inbox --since 1800000000"]);
        assert_eq!(std::fs::read_to_string(dir.path().join("sent.txt")).unwrap(), "hello", "the text goes on stdin");
    }

    #[test]
    fn the_four_calls_are_made_as_the_protocol_says() {
        let (dir, settings, env) = stand_in();
        answers(&dir, "whoami", json!({ "id": "u-alex", "name": "Alex Example", "email": "alex@example.com" }));
        answers(&dir, "people", json!([{ "id": "u-sam", "name": "Sam Example", "email": "sam@example.com" }]));
        let now = crate::model::now();
        answers(&dir, "inbox", json!([message("m2", "Alex Example", now - 60, "On it"), message("m1", "Sam Example", now - 120, "prod is down, can you look?")]));

        assert_eq!(me(&settings, &env).unwrap(), alex());
        assert_eq!(find_person(&settings, " Sam ", &env).unwrap()[0].id, "u-sam");
        assert!(find_person(&settings, "  ", &env).unwrap().is_empty());
        assert_eq!(calls(&dir), vec!["whoami", "people Sam"], "an empty question asks nothing");

        send(&settings, " u-sam ", "The fix for #1111 is pushed.\nRobo, on behalf of Alex\n", false, &env).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("sent.txt")).unwrap(), "The fix for #1111 is pushed.\nRobo, on behalf of Alex");
        send(&settings, "Sam Example", "It is “urgent” & has $HOME `in` it", true, &env).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("sent.txt")).unwrap(), "It is “urgent” & has $HOME `in` it", "the text is never part of a command line");
        assert_eq!(calls(&dir)[2..], ["send --to u-sam", "send --to Sam Example --urgent"]);

        // Nothing to send, or nobody to send it to: the program is not even started.
        assert!(send(&settings, "", "text", false, &env).is_err());
        assert!(send(&settings, "u-sam", "  ", false, &env).is_err());
        assert_eq!(calls(&dir).len(), 4);

        let found = incoming(&settings, now - 3600, &env).unwrap();
        assert_eq!(found.len(), 1, "the person's own answer is not incoming");
        assert_eq!((found[0].id.as_str(), found[0].from.as_str(), found[0].text.as_str(), found[0].urgent, found[0].handled), ("m1", "Sam Example", "prod is down, can you look?", true, false));
        assert_eq!(calls(&dir)[4..], ["whoami".to_string(), format!("inbox --since {}", now - 3600)]);

        // The first round ever (since 0) looks back three days, not through all history.
        incoming(&settings, 0, &env).unwrap();
        let asked: u64 = calls(&dir).last().unwrap().strip_prefix("inbox --since ").unwrap().parse().unwrap();
        assert!(asked >= now - LOOK_BACK_SECS && asked <= crate::model::now() - LOOK_BACK_SECS + 5, "{asked}");
    }

    #[cfg(unix)]
    #[test]
    fn no_messenger_is_a_plain_reason_and_nothing_is_called() {
        let (dir, _, env) = stand_in();
        let none = Settings::default();
        assert!(!available(&none, &env));
        assert_eq!(kind(&me(&none, &env).unwrap_err()), Some(&MessengerError::NotSet));
        assert_eq!(kind(&find_person(&none, "Sam", &env).unwrap_err()), Some(&MessengerError::NotSet));
        assert_eq!(kind(&incoming(&none, 0, &env).unwrap_err()), Some(&MessengerError::NotSet));
        let error = send(&none, "Sam", "The fix for #1 is pushed", false, &env).unwrap_err();
        assert_eq!(error.to_string(), "No messenger is set up. Choose one in Settings › Manager.");
        assert_eq!(kind(&send(&none, "", "", false, &env).unwrap_err()), Some(&MessengerError::NotSet), "said first, whatever else is missing");
        assert!(calls(&dir).is_empty());

        let gone = Settings { messenger: "no-such-messenger".into(), ..Settings::default() };
        assert!(!available(&gone, &env));
        assert_eq!(kind(&send(&gone, "Sam", "x", false, &env).unwrap_err()), Some(&MessengerError::NotFound("no-such-messenger".into())));
        assert!(calls(&dir).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn not_signed_in_is_a_clear_error_and_not_available() {
        let (dir, settings, env) = stand_in();
        for call in ["whoami", "send"] {
            std::fs::write(dir.path().join(format!("{call}.code")), "3").unwrap();
        }
        assert!(!available(&settings, &env));
        assert_eq!(calls(&dir).len(), 1);
        assert!(!available(&settings, &env));
        assert_eq!(calls(&dir).len(), 1, "asked once, then remembered for a minute");
        assert_eq!(kind(&incoming(&settings, 0, &env).unwrap_err()), Some(&MessengerError::Refused("not signed in".into())));
        assert_eq!(calls(&dir), vec!["whoami", "whoami"], "the inbox is not read");
        let error = send(&settings, "Sam", "x", false, &env).unwrap_err();
        assert!(matches!(kind(&error), Some(MessengerError::Refused(why)) if why.starts_with("not signed in")));
        assert_eq!(error.to_string(), "The messenger is not signed in.");

        // Any other failure says what the program wrote.
        std::fs::write(dir.path().join("people.code"), "1").unwrap();
        std::fs::write(dir.path().join("people.err"), "the directory is closed today\n").unwrap();
        assert_eq!(kind(&find_person(&settings, "Sam", &env).unwrap_err()), Some(&MessengerError::Failed("the directory is closed today".into())));
    }

    #[cfg(unix)]
    #[test]
    fn available_when_the_program_says_who_the_person_is() {
        let (dir, settings, env) = stand_in();
        answers(&dir, "whoami", json!({ "id": "u-alex", "name": "Alex Example" }));
        assert!(available(&settings, &env));
    }

    /// `script/messenger-example.sh` is the protocol's worked example: it has to speak it.
    #[cfg(unix)]
    #[test]
    fn the_example_program_speaks_the_protocol() {
        let example = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../script/messenger-example.sh");
        assert!(example.is_file(), "{} is missing", example.display());
        // It writes under GLASSCODE_HOME: a folder of this test's own, never the real one.
        let home = tempfile::tempdir().unwrap();
        let env = HashMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string()), ("GLASSCODE_HOME".to_string(), home.path().display().to_string())]);
        let settings = Settings { messenger: example.display().to_string(), ..Settings::default() };

        let me = me(&settings, &env).unwrap();
        assert!(knows(&me) && !me.name.is_empty());
        let people = find_person(&settings, "Sam \"the fixer\" Example", &env).unwrap();
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].name, "Sam \"the fixer\" Example");
        send(&settings, "Sam Example", "The fix for #1111 is pushed.\nRobo, on behalf of Alex", true, &env).unwrap();
        let log = std::fs::read_to_string(home.path().join("manager/outbox-example.log")).unwrap();
        assert!(log.contains("to: Sam Example · urgent: yes\nThe fix for #1111 is pushed.\nRobo, on behalf of Alex\n"), "{log}");
        assert!(incoming(&settings, 0, &env).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn incoming_needs_to_know_who_the_person_is() {
        let (dir, settings, env) = stand_in();
        answers(&dir, "inbox", json!([message("m1", "Sam Example", crate::model::now() - 10, "hello")]));
        // `whoami` answers with nothing usable, with no JSON, or fails: the inbox is never read.
        answers(&dir, "whoami", json!({}));
        assert!(incoming(&settings, 0, &env).is_err());
        std::fs::write(dir.path().join("whoami.json"), "who knows").unwrap();
        assert!(incoming(&settings, 0, &env).is_err());
        std::fs::write(dir.path().join("whoami.code"), "1").unwrap();
        assert!(incoming(&settings, 0, &env).is_err());
        assert_eq!(calls(&dir), vec!["whoami", "whoami", "whoami"], "no inbox call");
    }
}
