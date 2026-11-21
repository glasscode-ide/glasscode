//! The manager for the glasses on the server (G08, `docs/mobile-glasses-plan.md`): the same phone
//! methods the Mac's device door answers, for a phone that reaches this server over SSH through
//! `glasscoded gate --device` (so its caller is `Caller::Phone`).
//!
//! - `manager.home`: where the manager acts from now (this server, or the Mac), its name.
//! - `manager.ask`: the person's question from their glasses, posted in the conversation as the
//!   device's tier-2 message (D1), with the lens's focus for the brain. While this server is home
//!   its chat lane answers it (woken at once, not at its next 2-second look), and the request
//!   returns the answer. While the Mac is home the phone is told to ask the Mac.
//! - `manager.conversation`: the conversation, and the answer being written now.
//! - `manager.stop`: stop the answer this phone asked for.
//! - `device.presence`: what the glasses do, passed to the Mac with the next sync.
//! - `manager.voice`: a pass to the manager's voice room on this server (LiveKit and the voice
//!   worker of `voice/manager`), when `~/.glasscode/voice/server.env` says where it is.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use glasscode_core::caller::Caller;
use glasscode_core::conversation::{Author, Conversation, Draft, Item, ItemId, Kind};
use serde_json::{Value, json};

/// How long `manager.ask` waits for the answer.
const ANSWER_WITHIN: Duration = Duration::from_secs(150);

/// A question a phone asked, as the brain gets it (the glasses block, then the words), by item.
static COMPOSED: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
/// The answer being written: the question it answers and the text so far.
static WRITING: Mutex<Option<(String, String)>> = Mutex::new(None);
/// The brain's stop for the answer being written, and the device whose question it is.
static STOP: Mutex<Option<(String, Arc<AtomicBool>)>> = Mutex::new(None);
/// The chat lane waits on this between its looks; a phone's question wakes it.
static WAKE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
/// What each device said its glasses do, for the Mac.
static PRESENCE: Mutex<Option<HashMap<String, Value>>> = Mutex::new(None);
/// A place the manager showed for the device whose question it answers: taken once, by that
/// phone's next `manager.conversation`.
static OPEN: Mutex<Option<(String, Value)>> = Mutex::new(None);

/// `device.open` on the server (G08): only in answer to a phone's question being answered now.
pub fn show(place: &str) -> Result<String, String> {
    let device = lock(&STOP).as_ref().map(|(device, _)| device.clone()).filter(|d| !d.is_empty()).ok_or("It shows a place on the glasses only in answer to a question from them.")?;
    let lower = place.trim().to_lowercase();
    let data = if lower == "today" || lower == "glance" {
        json!({ "kind": "today" })
    } else if lower == "conversation" {
        json!({ "kind": "conversation" })
    } else if let Some(n) = lower.strip_prefix("pr:").or_else(|| lower.strip_prefix('#')).and_then(|n| n.trim().parse::<u64>().ok()) {
        json!({ "kind": "pr", "number": n })
    } else if let Some(project) = place.trim().strip_prefix("project:") {
        json!({ "kind": "project", "title": project.trim() })
    } else if !lower.is_empty() {
        json!({ "kind": "session", "id": place.trim() })
    } else {
        return Err("device open <place>".into());
    };
    *lock(&OPEN) = Some((device, data));
    Ok(format!("Shown on the glasses: {place}."))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The chat lane's wait between looks: at most `most`, less when a phone asked something.
pub fn wait_for_question(most: Duration) {
    let (flag, wake) = &WAKE;
    let mut asked = lock(flag);
    if !*asked {
        asked = wake.wait_timeout(asked, most).map(|(g, _)| g).unwrap_or_else(|p| p.into_inner().0);
    }
    *asked = false;
}

fn wake_the_chat() {
    let (flag, wake) = &WAKE;
    *lock(flag) = true;
    wake.notify_all();
}

/// What the brain gets for a question: a phone's with its glasses block, else the words.
pub fn composed(question: &Item) -> String {
    lock(&COMPOSED).as_ref().and_then(|m| m.get(question.id.as_str()).cloned()).unwrap_or_else(|| question.text.clone())
}

/// The chat lane starts answering `question`: what it writes is kept for `manager.conversation`,
/// and a phone may stop it.
pub fn answering(question: &Item, stop: Arc<AtomicBool>) {
    *lock(&WRITING) = Some((question.id.as_str().to_string(), String::new()));
    let device = match question.words().and_then(glasscode_core::words::find).map(|r| r.origin) {
        Some(glasscode_core::words::Origin::Device(device)) => device,
        _ => String::new(),
    };
    *lock(&STOP) = Some((device, stop));
}

/// More of the answer (a part from Claude, the whole from Codex).
pub fn wrote(part: &str) {
    if let Some((_, text)) = lock(&WRITING).as_mut() {
        text.push_str(part);
    }
}

/// The answer is posted (or failed): nothing is being written.
pub fn answered(question: &Item) {
    *lock(&WRITING) = None;
    *lock(&STOP) = None;
    if let Some(map) = lock(&COMPOSED).as_mut() {
        map.remove(question.id.as_str());
    }
}

/// What the devices said their glasses do (for the Mac's title bar, with the sync).
pub fn presence() -> HashMap<String, Value> {
    lock(&PRESENCE).clone().unwrap_or_default()
}

fn device_of(caller: &Caller) -> anyhow::Result<String> {
    match caller {
        Caller::Phone { device } => Ok(device.clone()),
        _ => Err(anyhow!("only a paired phone calls this")),
    }
}

/// The phone methods of the manager here; none for any other method or caller.
pub fn dispatch(caller: &Caller, method: &str, params: &Value) -> Option<anyhow::Result<Value>> {
    if !matches!(caller, Caller::Phone { .. }) {
        return None;
    }
    let answer = match method {
        "manager.home" => Ok(home()),
        "manager.ask" => ask(caller, params),
        "manager.conversation" => Ok({
            let mut seen = conversation(params);
            // A place the manager showed for this phone, once.
            let mut open = lock(&OPEN);
            if open.as_ref().is_some_and(|(device, _)| matches!(caller, Caller::Phone { device: d } if d == device)) {
                seen["open"] = open.take().map(|(_, data)| data).unwrap_or(Value::Null);
            }
            seen
        }),
        "manager.stop" => stop(caller),
        "manager.voice" => voice_pass(caller),
        "device.presence" => device_of(caller).map(|device| {
            let mut said = params.clone();
            said["at_ms"] = json!(glasscode_core::clock::now_ms());
            lock(&PRESENCE).get_or_insert_with(HashMap::new).insert(device, said);
            json!({ "ok": true })
        }),
        _ => return None,
    };
    Some(answer)
}

/// What `~/.glasscode/voice/server.env` says (`NAME=value` lines, as `voice/manager/.env`): the
/// room's address as a phone reaches it (`VOICE_URL`, `wss://…` or `ws://<tailnet address>:7880`)
/// and LiveKit's key and secret (`LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET`).
fn voice_setup(text: &str) -> Option<(String, String, String)> {
    let mut found: HashMap<&str, String> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((name, value)) = line.split_once('=') {
            found.insert(name.trim(), value.trim().trim_matches('"').trim_matches('\'').to_string());
        }
    }
    let take = |name: &str| found.get(name).filter(|v| !v.is_empty()).cloned();
    Some((take("VOICE_URL")?, take("LIVEKIT_API_KEY")?, take("LIVEKIT_API_SECRET")?))
}

/// `manager.voice`: a pass to the voice room on this server, for an hour.
fn voice_pass(caller: &Caller) -> anyhow::Result<Value> {
    let device = device_of(caller)?;
    let file = glasscode_core::paths::root().join("voice").join("server.env");
    let text = std::fs::read_to_string(&file).map_err(|_| anyhow!("The manager's voice is not set up on the server (no ~/.glasscode/voice/server.env)."))?;
    let (url, key, secret) = voice_setup(&text).ok_or_else(|| anyhow!("~/.glasscode/voice/server.env needs VOICE_URL, LIVEKIT_API_KEY and LIVEKIT_API_SECRET."))?;
    let short: String = device.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect();
    let identity = format!("phone-{short}");
    let room = "manager".to_string();
    let token = glasscode_core::manager::voice::token(&key, &secret, &identity, &room, 60 * 60)?;
    let name = crate::brain::from_mac().unwrap_or_default().name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Manager".to_string());
    Ok(json!({ "url": url, "token": token, "room": room, "identity": identity, "name": name, "talker": true }))
}

/// `manager.home`.
pub fn home() -> Value {
    let generation = crate::home::server_generation();
    let said = crate::brain::from_mac().unwrap_or_default();
    let name = said.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Manager".to_string());
    let initials: String = name.split_whitespace().take(2).filter_map(|w| w.chars().next()).flat_map(char::to_uppercase).collect();
    json!({
        "home": if generation.is_some() { "server" } else { "mac" },
        "here": generation.is_some(),
        "generation": generation.unwrap_or(0),
        "server": crate::daemon::hostname(),
        "manager": {
            "enabled": said.enabled,
            "name": name,
            "initials": initials,
            "thinking": lock(&WRITING).is_some(),
            "answers": crate::brain::chat_status().err(),
        },
    })
}

/// `manager.ask`: post the question, wake the chat lane, and wait for its answer.
fn ask(caller: &Caller, params: &Value) -> anyhow::Result<Value> {
    let device = device_of(caller)?;
    let text = params.get("text").and_then(Value::as_str).unwrap_or_default().trim().to_string();
    if text.is_empty() {
        return Err(anyhow!("There is nothing to ask."));
    }
    if crate::home::server_generation().is_none() {
        return Ok(json!({ "elsewhere": "mac", "home": home() }));
    }
    let said = crate::brain::from_mac().unwrap_or_default();
    if !said.enabled {
        return Err(anyhow!("The manager is off. Switch it on in Settings › Manager on your Mac."));
    }
    if let Err(why) = crate::brain::chat_status() {
        return Err(anyhow!("The manager cannot answer on this server now: {why}"));
    }
    let focus: glasscode_core::runtime::chat::DeviceFocus = params.get("focus").cloned().and_then(|f| serde_json::from_value(f).ok()).unwrap_or_default();
    // The Mac's window is not here: only the lens shows (O8).
    let block = glasscode_core::runtime::chat::glasses_block(&focus, None, false);
    let words = glasscode_core::words::record(glasscode_core::words::Origin::Device(device), &text, focus.session.as_deref(), None);
    let question = Conversation::load().post(Draft { author: Author::Person, kind: Kind::You { words, reply_to: None }, text: text.clone(), refs: Vec::new(), topic: None }).map_err(|e| anyhow!("The question could not be kept: {e}"))?;
    lock(&COMPOSED).get_or_insert_with(HashMap::new).insert(question.id.as_str().to_string(), format!("{block}\n\n{text}"));
    wake_the_chat();
    let started = Instant::now();
    while started.elapsed() < ANSWER_WITHIN {
        std::thread::sleep(Duration::from_millis(300));
        let conversation = Conversation::load();
        if let Some(reply) = reply_to(&conversation.items(), &question.id) {
            return match reply.kind {
                Kind::Error => Err(anyhow!("{}", reply.text)),
                _ => Ok(json!({ "say": reply.text, "item": reply.id })),
            };
        }
    }
    Err(anyhow!("The manager took too long to answer; the answer comes in the conversation."))
}

/// The answer posted after `question`: the chat's, or the app's line that says why not.
fn reply_to(items: &[&Item], question: &ItemId) -> Option<Item> {
    let asked = items.iter().find(|i| &i.id == question)?;
    items.iter().filter(|i| i.posted > asked.posted && matches!(i.author, Author::Chat | Author::App)).min_by(|a, b| a.posted.cmp(&b.posted)).map(|i| (*i).clone())
}

/// `manager.conversation`: the items after `after` (an item id), at most `limit`, and the answer
/// being written.
fn conversation(params: &Value) -> Value {
    let conversation = Conversation::load();
    let items = conversation.items();
    let after = params.get("after").and_then(Value::as_str);
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(40).clamp(1, 200) as usize;
    let start = after.and_then(|id| items.iter().position(|i| i.id.as_str() == id).map(|at| at + 1)).unwrap_or(0);
    let newer = &items[start.min(items.len())..];
    let take: Vec<Value> = newer[newer.len().saturating_sub(limit)..].iter().map(|i| serde_json::to_value(i).unwrap_or_default()).collect();
    let writing = lock(&WRITING).clone().map(|(question, text)| json!({ "question": question, "text": text }));
    json!({ "items": take, "writing": writing })
}

/// `manager.stop`: only the answer to this phone's own question.
fn stop(caller: &Caller) -> anyhow::Result<Value> {
    let device = device_of(caller)?;
    match lock(&STOP).as_ref() {
        Some((asker, stop)) if *asker == device => {
            stop.store(true, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        }
        _ => Err(anyhow!("nothing this phone asked is being answered")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_phone_reaches_these() {
        for caller in [Caller::Ui, Caller::Terminal { pid: None }] {
            assert!(dispatch(&caller, "manager.home", &json!({})).is_none());
        }
        assert!(dispatch(&Caller::Phone { device: "phone-1".into() }, "sessions.create", &json!({})).is_none());
    }

    /// G08: a phone asks the server only while it is home; else it is told to ask the Mac.
    #[test]
    fn phone_asks_the_server_home() {
        let (_home, _scratch) = crate::daemon::tests::scratch_home();
        crate::home::mac_is_home_for_test();
        let phone = Caller::Phone { device: "phone-1a2b".into() };
        let answer = dispatch(&phone, "manager.ask", &json!({ "text": "what needs me?" })).unwrap().unwrap();
        // No voice set up here: a sentence that says where it is set up.
        let voice = dispatch(&phone, "manager.voice", &json!({})).unwrap();
        assert!(voice.is_err_and(|e| e.to_string().contains("server.env")));
        assert_eq!(answer["elsewhere"], "mac");
        let home = dispatch(&phone, "manager.home", &json!({})).unwrap().unwrap();
        assert_eq!((home["home"].as_str(), home["here"].as_bool()), (Some("mac"), Some(false)));
    }

    #[test]
    fn a_place_shown_reaches_the_asking_phone_once() {
        let (_home, _scratch) = crate::daemon::tests::scratch_home();
        assert!(show("pr:42").is_err(), "no question from the glasses is being answered");
        let question = Conversation::load().post(Draft { author: Author::Person, kind: Kind::You { words: glasscode_core::words::record(glasscode_core::words::Origin::Device("phone-1a2b".into()), "open the checkout PR", None, None), reply_to: None }, text: "open the checkout PR".into(), refs: Vec::new(), topic: None }).unwrap();
        answering(&question, Arc::new(AtomicBool::new(false)));
        assert!(show("pr:42").is_ok());
        let other = dispatch(&Caller::Phone { device: "phone-ffff".into() }, "manager.conversation", &json!({})).unwrap().unwrap();
        assert!(other.get("open").is_none(), "not another phone's");
        let seen = dispatch(&Caller::Phone { device: "phone-1a2b".into() }, "manager.conversation", &json!({})).unwrap().unwrap();
        assert_eq!(seen["open"]["number"], 42);
        let again = dispatch(&Caller::Phone { device: "phone-1a2b".into() }, "manager.conversation", &json!({})).unwrap().unwrap();
        assert!(again.get("open").is_none(), "once");
        answered(&question);
    }

    #[test]
    fn chat_wakes_on_post() {
        let started = Instant::now();
        let waiter = std::thread::spawn(|| wait_for_question(Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(50));
        wake_the_chat();
        waiter.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(2), "woken, not timed out");
    }

    #[test]
    fn the_answer_being_written_is_read() {
        let (_home, _scratch) = crate::daemon::tests::scratch_home();
        let question = Conversation::load().post(Draft { author: Author::Person, kind: Kind::You { words: glasscode_core::words::record(glasscode_core::words::Origin::Device("phone-1a2b".into()), "hi", None, None), reply_to: None }, text: "hi".into(), refs: Vec::new(), topic: None }).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        answering(&question, stop.clone());
        wrote("Two ");
        wrote("things.");
        let seen = conversation(&json!({}));
        assert_eq!(seen["writing"]["text"], "Two things.");
        // Another phone cannot stop it; the one that asked can.
        assert!(super::stop(&Caller::Phone { device: "phone-ffff".into() }).is_err());
        assert!(super::stop(&Caller::Phone { device: "phone-1a2b".into() }).is_ok());
        assert!(stop.load(Ordering::SeqCst));
        answered(&question);
        assert!(conversation(&json!({}))["writing"].is_null());
    }

    #[test]
    fn the_voice_setup_is_read_from_its_lines() {
        let text = "# the room\nVOICE_URL=wss://voice.example.test\nLIVEKIT_API_KEY=APIabc\nLIVEKIT_API_SECRET=\"s3cret\"\n";
        assert_eq!(voice_setup(text), Some(("wss://voice.example.test".into(), "APIabc".into(), "s3cret".into())));
        assert_eq!(voice_setup("VOICE_URL=wss://x\nLIVEKIT_API_KEY=k\n"), None);
    }
}
