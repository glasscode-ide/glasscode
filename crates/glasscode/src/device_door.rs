//! The device door (G04, `docs/mobile-glasses-plan.md` section 4): how the Glasscode app on the
//! person's iPhone, with their glasses, reaches this Mac directly over the tailnet.
//!
//! - **Where it listens.** Only on this Mac's tailnet address (`device_link::tailscale_ipv4`),
//!   only while a phone is paired, and only while the app runs. A debug build may add loopback
//!   (`GLASSCODE_DOOR_LOOPBACK=1`) for the simulator and the tests.
//! - **Who gets in.** A connection from outside the tailnet is closed at once. Then the
//!   handshake of `glasscode_core::device_link`: the door signs first with its own key (which the
//!   phone pinned at pairing), the phone answers with the key this Mac pinned. From then on the
//!   connection is `Caller::Phone { device }`.
//! - **What it may do.** `caller::PHONE_ALLOWED`, the list glasscoded keeps too: refused here on
//!   the connection's own thread, before the window hears of it. Input to a session is the phone's
//!   signed input (`relayed_apply`, #288); an approval is its signed `today.op`; a question to the
//!   manager is a tier-2 message from the device (`manager.ask`).
//! - **What it tells the phone.** Answers as they are written, the conversation, where the
//!   manager is home, Today when it changes, and places the manager shows (`device.open`).

use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, Window};
use glasscode_core::device_link::{self, DoorKey, PhoneHello, Welcome, events};
use glasscode_core::remote::{InputSig, MirrorInput, Request, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::workspace::Workspace;

/// How long a connection may be quiet before it is closed (the app asks `hello` every 30 s).
const IDLE: Duration = Duration::from_secs(120);
/// How often the tailnet address is looked up again while the door is wanted.
const LOOK_EVERY: Duration = Duration::from_secs(30);
/// How long a device's use counts as the person being present (O9).
pub const PRESENT_FOR: Duration = Duration::from_secs(90);
/// How long a device's question may wait for its answer.
const ASK_WITHIN: Duration = Duration::from_secs(200);
/// How long a device's question may wait for the manager to be free, and how many of a device's
/// may wait at once.
const WAIT_WITHIN: Duration = Duration::from_secs(300);
const WAITING_MAX: usize = 5;
/// How long after a device's question the manager may show it a place (`device.open`).
const OPEN_WITHIN_MS: u64 = 30_000;

/// What a connection's thread tells the window.
pub enum DoorMsg {
    Connected { conn: u64, device: String, name: String, app: String, peer: SocketAddr, out: mpsc::Sender<String> },
    Request { conn: u64, request: Request },
    Gone { conn: u64 },
}

/// One phone connected through the door.
pub struct Conn {
    pub device: String,
    pub name: String,
    pub app: String,
    pub peer: SocketAddr,
    out: mpsc::Sender<String>,
    pub since: Instant,
}

/// What a device says its glasses are doing (`device.presence`): the title bar's icon.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Presence {
    /// The glasses are connected to the phone.
    pub glasses: bool,
    /// They are worn (the toolkit says so).
    pub worn: bool,
    /// The lens is on.
    pub lens: bool,
    /// What the voice is doing: idle, listening, thinking, speaking.
    pub voice: String,
    /// Which end the phone talks to now: mac or server.
    pub via: String,
    #[serde(skip)]
    pub at: Option<Instant>,
}

/// A question a device asked that the chat answers now.
struct DeviceAsk {
    conn: u64,
    device: String,
    request: u64,
    client_id: String,
    since: Instant,
    /// How much of the answer was sent, and the last activity line.
    sent: usize,
    activity: Option<String>,
    /// The Mac's window may move for it (O8).
    pub window_moves: bool,
}

/// A question a device asked while the manager answered another: it is asked once the manager is
/// free, in order, as the Mac's own chat and voice queue theirs (nothing is refused).
#[derive(Clone, Debug, PartialEq)]
struct WaitingAsk {
    conn: u64,
    device: String,
    request: u64,
    params: Value,
    since: Instant,
}

/// The data a connection's handshake needs, made once (the key comes from the Keychain).
pub struct DoorInfo {
    pub key: DoorKey,
    pub mac: String,
    pub mac_id: String,
    pub launch: String,
}

#[derive(Default)]
pub struct DoorUi {
    /// Where it listens now.
    pub listening: Vec<SocketAddr>,
    pub error: Option<String>,
    stop: Option<Arc<AtomicBool>>,
    info: Option<Arc<DoorInfo>>,
    loading: bool,
    paired: Arc<Mutex<BTreeSet<String>>>,
    tx: Option<async_channel::Sender<DoorMsg>>,
    /// The tailnet address, as last looked up, and when.
    pub(crate) tailnet: Option<std::net::Ipv4Addr>,
    looked: Option<Instant>,
    looking: bool,
    pub conns: HashMap<u64, Conn>,
    pub presence: HashMap<String, Presence>,
    ask: Option<DeviceAsk>,
    /// Questions that wait for the manager to be free, oldest first.
    waiting: std::collections::VecDeque<WaitingAsk>,
    /// The last place a device was asked (device, connection, when): `device.open`'s target.
    last_ask: Option<(String, u64, u64)>,
    /// When a device last did anything here (O9's presence).
    pub used: Option<Instant>,
    /// What each connection was last told, so it is told again only when that changes.
    told_today: HashMap<u64, String>,
    told_home: HashMap<u64, String>,
    /// The lens page of the window's place each connection was last sent (`lens_pages`).
    told_place: HashMap<u64, String>,
    place_at: Option<Instant>,
    told_items: HashMap<u64, String>,
    today_at: Option<Instant>,
    /// What phones that reach the server said their glasses do (the server's device ids).
    pub via_server: HashMap<String, Presence>,
}

impl DoorUi {
    /// A device used the door within [`PRESENT_FOR`].
    pub fn used_recently(&self) -> bool {
        self.used.is_some_and(|at| at.elapsed() < PRESENT_FOR)
    }

    /// The device asked something now being answered (`manager.ask`), and whether the Mac's window
    /// may move for it.
    pub fn asking(&self) -> Option<(&str, bool)> {
        self.ask.as_ref().map(|ask| (ask.device.as_str(), ask.window_moves))
    }

    /// Keep a device's question for when the manager is free. Err: the device has
    /// [`WAITING_MAX`] waiting already.
    fn wait(&mut self, conn: u64, device: &str, request: u64, params: &Value) -> Result<(), ()> {
        if self.waiting.iter().filter(|w| w.device == device).count() >= WAITING_MAX {
            return Err(());
        }
        self.waiting.push_back(WaitingAsk { conn, device: device.to_string(), request, params: params.clone(), since: Instant::now() });
        Ok(())
    }

    /// The waiting questions that waited past [`WAIT_WITHIN`], taken out.
    fn waited_too_long(&mut self) -> Vec<WaitingAsk> {
        let (old, keep): (std::collections::VecDeque<WaitingAsk>, _) = self.waiting.drain(..).partition(|w| w.since.elapsed() > WAIT_WITHIN);
        self.waiting = keep;
        old.into()
    }

    /// A device's (or a connection's) waiting questions, taken out.
    fn drop_waiting(&mut self, gone: impl Fn(&WaitingAsk) -> bool) -> Vec<WaitingAsk> {
        let (out, keep): (std::collections::VecDeque<WaitingAsk>, _) = self.waiting.drain(..).partition(|w| gone(w));
        self.waiting = keep;
        out.into()
    }
}

// ----- the listener and its connections (no window) --------------------------------------------

/// Listen on `addrs` until `stop`, one thread per connection. `loopback` lets a connection from
/// this Mac in (tests, the simulator). Returns where it listens.
pub fn listen(addrs: &[SocketAddr], loopback: bool, info: Arc<DoorInfo>, paired: Arc<Mutex<BTreeSet<String>>>, tx: async_channel::Sender<DoorMsg>, stop: Arc<AtomicBool>) -> std::io::Result<Vec<SocketAddr>> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let mut bound = Vec::new();
    for addr in addrs {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        bound.push(listener.local_addr()?);
        let (info, paired, tx, stop) = (info.clone(), paired.clone(), tx.clone(), stop.clone());
        std::thread::Builder::new().name("glasscode-door".into()).spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, peer)) => {
                        let conn = NEXT.fetch_add(1, Ordering::Relaxed);
                        let (info, paired, tx, stop) = (info.clone(), paired.clone(), tx.clone(), stop.clone());
                        let _ = std::thread::Builder::new().name("glasscode-door-conn".into()).spawn(move || serve(stream, peer, conn, loopback, &info, &paired, &tx, &stop));
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(150)),
                    Err(err) => {
                        log::warn!("device door: {err}");
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        })?;
    }
    Ok(bound)
}

/// One line, at most `max` bytes (a longer one is an error, and the connection ends).
fn read_line(reader: &mut BufReader<TcpStream>, max: usize) -> std::io::Result<Option<String>> {
    let mut line = String::new();
    let n = reader.by_ref().take(max as u64 + 1).read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    if line.len() > max {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "that line is too long"));
    }
    Ok(Some(line))
}

fn write_line(stream: &mut TcpStream, line: &str) -> std::io::Result<()> {
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

/// One connection: who it is, then its requests. What a phone may not call is refused here.
#[allow(clippy::too_many_arguments)]
fn serve(stream: TcpStream, peer: SocketAddr, conn: u64, loopback: bool, info: &DoorInfo, paired: &Mutex<BTreeSet<String>>, tx: &async_channel::Sender<DoorMsg>, stop: &AtomicBool) {
    if !device_link::peer_allowed(peer.ip(), loopback) {
        log::info!("device door: {peer} is not on the tailnet; closed");
        return;
    }
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(device_link::HANDSHAKE_SECS)));
    let Ok(read) = stream.try_clone() else { return };
    let mut reader = BufReader::new(read);
    let mut out = stream;
    let hello = device_link::hello(&info.key, &info.mac, &info.mac_id, &info.launch, &["manager", "today", "device"]);
    if write_line(&mut out, &serde_json::to_string(&hello).unwrap_or_default()).is_err() {
        return;
    }
    let phone: PhoneHello = match read_line(&mut reader, 16 * 1024).ok().flatten().map(|line| serde_json::from_str(line.trim())) {
        Some(Ok(phone)) => phone,
        _ => {
            let _ = write_line(&mut out, &serde_json::to_string(&Welcome { ok: false, device: String::new(), error: Some("that is not a phone's answer".into()) }).unwrap_or_default());
            return;
        }
    };
    let checked = {
        let paired = paired.lock().unwrap_or_else(|p| p.into_inner());
        device_link::check_phone(&hello, &phone, &|id| paired.contains(id))
    };
    let device = match checked {
        Ok(device) => device,
        Err(why) => {
            log::info!("device door: {peer} refused: {why}");
            let _ = write_line(&mut out, &serde_json::to_string(&Welcome { ok: false, device: String::new(), error: Some(why) }).unwrap_or_default());
            return;
        }
    };
    if write_line(&mut out, &serde_json::to_string(&Welcome { ok: true, device: device.clone(), error: None }).unwrap_or_default()).is_err() {
        return;
    }
    let _ = reader.get_ref().set_read_timeout(Some(IDLE));
    // The window and this thread write lines; one writer keeps them whole.
    let (lines, queued) = mpsc::channel::<String>();
    let Ok(mut writer) = out.try_clone() else { return };
    std::thread::spawn(move || {
        while let Ok(line) = queued.recv() {
            if write_line(&mut writer, &line).is_err() {
                let _ = writer.shutdown(std::net::Shutdown::Both);
                break;
            }
        }
    });
    let name: String = phone.name.chars().filter(|c| !c.is_control()).take(60).collect();
    if tx.send_blocking(DoorMsg::Connected { conn, device: device.clone(), name, app: phone.app.chars().take(40).collect(), peer, out: lines.clone() }).is_err() {
        return;
    }
    while !stop.load(Ordering::Relaxed) {
        let line = match read_line(&mut reader, device_link::MAX_LINE) {
            Ok(Some(line)) => line,
            _ => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let request = match device_link::parse_request(&line) {
            Ok(request) => request,
            Err(why) => {
                let _ = lines.send(serde_json::to_string(&Response { id: 0, result: None, error: Some(why) }).unwrap_or_default());
                continue;
            }
        };
        if !device_link::allowed(&request.method) {
            let _ = lines.send(serde_json::to_string(&Response { id: request.id, result: None, error: Some(format!("a phone cannot call {}", request.method)) }).unwrap_or_default());
            continue;
        }
        if tx.send_blocking(DoorMsg::Request { conn, request }).is_err() {
            break;
        }
    }
    let _ = out.shutdown(std::net::Shutdown::Both);
    let _ = tx.send_blocking(DoorMsg::Gone { conn });
}

// ----- the window's side ------------------------------------------------------------------------

/// A request's params, as the phone's methods take them.
fn text_param(params: &Value, name: &str) -> String {
    params.get(name).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn signed_param(params: &Value) -> Option<InputSig> {
    params.get("signed").cloned().and_then(|v| serde_json::from_value(v).ok())
}

/// A place `device.open` names, as the phone shows it.
pub enum DevicePlace {
    Session(String),
    Pr(u64),
    Project(String),
    Today,
    Conversation,
}

impl DevicePlace {
    pub fn parse(word: &str) -> Option<DevicePlace> {
        let word = word.trim();
        let lower = word.to_lowercase();
        if lower == "today" || lower == "glance" {
            return Some(DevicePlace::Today);
        }
        if lower == "conversation" || lower == "chat" {
            return Some(DevicePlace::Conversation);
        }
        if let Some(number) = lower.strip_prefix("pr:").or_else(|| lower.strip_prefix('#')).and_then(|n| n.trim().parse().ok()) {
            return Some(DevicePlace::Pr(number));
        }
        if let Some(project) = word.strip_prefix("project:") {
            return Some(DevicePlace::Project(project.trim().to_string()));
        }
        (!word.is_empty()).then(|| DevicePlace::Session(word.to_string()))
    }
}

impl Workspace {
    /// The door's channel to the window; its listener starts in [`Self::door_tick`] once a phone
    /// is paired and the tailnet has an address. The door key is read from the Keychain off the
    /// main thread.
    pub fn door_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.door.tx.is_some() || crate::workspace::no_agents() && std::env::var_os("GLASSCODE_DOOR_LOOPBACK").is_none() {
            return;
        }
        let (tx, rx) = async_channel::unbounded::<DoorMsg>();
        self.door.tx = Some(tx);
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(msg) = rx.recv().await {
                if this.update_in(cx, |ws, window, cx| ws.door_handle(msg, window, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Every tick: the listener runs while it is wanted (a paired phone, an address), and
    /// connections hear what changed.
    pub fn door_tick(&mut self, cx: &mut Context<Self>) {
        let Some(tx) = self.door.tx.clone() else { return };
        let devices = self.control.authority.devices.clone();
        *self.door.paired.lock().unwrap_or_else(|p| p.into_inner()) = devices.clone();
        let loopback = std::env::var_os("GLASSCODE_DOOR_LOOPBACK").is_some();
        let wanted = !devices.is_empty();
        if !wanted {
            self.door_stop();
            return;
        }
        // The tailnet address, looked up now and then (Tailscale may start later, or change).
        if !self.door.looking && self.door.looked.is_none_or(|at| at.elapsed() >= LOOK_EVERY) {
            self.door.looking = true;
            let task = cx.background_spawn(async { device_link::tailscale_ipv4() });
            cx.spawn(async move |this, cx| {
                let ip = task.await;
                let _ = this.update(cx, |ws, cx| {
                    ws.door.looking = false;
                    ws.door.looked = Some(Instant::now());
                    if ws.door.tailnet != ip {
                        ws.door.tailnet = ip;
                        ws.door_stop();
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        let Some(info) = self.door.info.clone() else {
            if !self.door.loading {
                self.door.loading = true;
                let (launch, mac) = (self.control.launch.clone(), crate::platform::this_computer().to_string());
                let task = cx.background_spawn(async move {
                    // A debug build's journeys use a key of their own, never the Keychain of the
                    // person running them (script/journeys.sh, journey 9).
                    let debug_key = std::env::var("GLASSCODE_DEBUG_DOOR_KEY").ok().filter(|_| cfg!(debug_assertions)).and_then(|hex| DoorKey::from_hex(&hex));
                    let key = match debug_key {
                        Some(key) => key,
                        None => DoorKey::this_mac().map_err(|e| format!("{e:#}"))?,
                    };
                    let mac_id = glasscode_core::paths::MachineId::this().map(|id| id.as_str().to_string()).unwrap_or_default();
                    let name = glasscode_core::mirror::identity().1;
                    Ok::<DoorInfo, String>(DoorInfo { key, mac: if name.is_empty() { mac } else { name }, mac_id, launch })
                });
                cx.spawn(async move |this, cx| {
                    let info = task.await;
                    let _ = this.update(cx, |ws, cx| {
                        ws.door.loading = false;
                        match info {
                            Ok(info) => ws.door.info = Some(Arc::new(info)),
                            Err(why) => ws.door.error = Some(format!("The door key could not be kept in the Keychain: {why}")),
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            return;
        };
        if self.door.stop.is_none() {
            let mut addrs: Vec<SocketAddr> = self.door.tailnet.map(|ip| SocketAddr::from((ip, device_link::port()))).into_iter().collect();
            if loopback {
                addrs.push(SocketAddr::from(([127, 0, 0, 1], device_link::port())));
            }
            if addrs.is_empty() {
                self.door.error = Some("Tailscale is not running on this Mac, so your glasses cannot reach it.".into());
            } else {
                let stop = Arc::new(AtomicBool::new(false));
                match listen(&addrs, loopback, info, self.door.paired.clone(), tx, stop.clone()) {
                    Ok(bound) => {
                        log::info!("device door: listening on {bound:?}");
                        self.door.listening = bound;
                        self.door.error = None;
                        self.door.stop = Some(stop);
                    }
                    Err(err) => {
                        self.door.error = Some(format!("The door for your glasses could not open: {err}"));
                        // Try again at the next look.
                        self.door.looked = None;
                    }
                }
            }
            cx.notify();
        }
        if !self.door.conns.is_empty() {
            self.door_tell_changes(cx);
        }
    }

    /// Close the listener and every connection.
    fn door_stop(&mut self) {
        if let Some(stop) = self.door.stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        self.door.listening.clear();
        self.door.conns.clear();
    }

    /// Four times a second while a device asks something: the answer as it is written, and the
    /// next waiting question once the manager is free.
    pub fn door_fast_tick(&mut self, cx: &mut Context<Self>) {
        if !self.door.waiting.is_empty() {
            for late in self.door.waited_too_long() {
                self.door_reply(late.conn, late.request, Err(format!("{} was busy for too long; ask again.", self.state.manager.name)));
            }
            if self.door.ask.is_none() && self.mchat.turn.is_none() && self.mchat.queue.is_empty()
                && let Some(next) = self.door.waiting.pop_front()
            {
                self.door_ask_now(next.conn, next.device, next.request, &next.params, cx);
            }
        }
        // A question whose answer never came (the turn ended some other way): let it go, so the
        // next one is taken and the person's own chat moves the window again.
        if let Some(ask) = self.door.ask.as_ref().filter(|ask| ask.since.elapsed() > ASK_WITHIN) {
            let (conn, request) = (ask.conn, ask.request);
            self.door_answered(conn, request, Err("The manager took too long to answer; the answer comes in the conversation.".into()));
        }
        let Some(ask) = self.door.ask.as_mut() else { return };
        let Some(turn) = self.mchat.turn.as_ref() else { return };
        let mut lines = Vec::new();
        if turn.activity != ask.activity {
            ask.activity = turn.activity.clone();
            if let Some(line) = &ask.activity {
                lines.push(device_link::event_line(events::WORKING, json!({ "ask": ask.client_id, "state": "thinking", "line": line })));
            }
        }
        if turn.text.len() > ask.sent {
            ask.sent = turn.text.len();
            lines.push(device_link::event_line(events::ANSWER, json!({ "ask": ask.client_id, "text": turn.text, "done": false })));
        }
        let conn = ask.conn;
        for line in lines {
            self.door_send(conn, line);
        }
    }

    /// Tell each connection what changed since it was last told: where the manager is home,
    /// Today, the conversation.
    fn door_tell_changes(&mut self, cx: &mut Context<Self>) {
        let conns: Vec<u64> = self.door.conns.keys().copied().collect();
        let home = self.door_home();
        let home_text = home.to_string();
        let items = self.door_items(None, 20);
        let last_item = items.as_array().and_then(|a| a.last()).and_then(|i| i["id"].as_str()).unwrap_or_default().to_string();
        let today_due = self.door.today_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(2));
        let today = today_due.then(|| {
            self.door.today_at = Some(Instant::now());
            let (mac_id, mac) = self.door.info.as_ref().map(|i| (i.mac_id.clone(), i.mac.clone())).unwrap_or_default();
            let mut snapshot = serde_json::to_value(self.today_snapshot(&mac_id, &mac)).unwrap_or_default();
            let at = snapshot["at"].take();
            let text = snapshot.to_string();
            snapshot["at"] = at;
            (snapshot, text)
        });
        // The window's place as the lens shows it: sent when it, or what it shows, changes.
        let place = (!conns.is_empty() && self.door.place_at.is_none_or(|at| at.elapsed() >= Duration::from_millis(500))).then(|| {
            self.door.place_at = Some(Instant::now());
            self.lens_refresh_reply(cx);
            let page = serde_json::to_value(self.lens_page(&self.lens_place(), cx)).unwrap_or_default();
            let text = page.to_string();
            (page, text)
        });
        for conn in conns {
            if let Some((page, text)) = &place
                && self.door.told_place.get(&conn) != Some(text)
            {
                self.door.told_place.insert(conn, text.clone());
                self.door_send(conn, device_link::event_line(events::PLACE, page.clone()));
            }
            if self.door.told_home.get(&conn) != Some(&home_text) {
                self.door.told_home.insert(conn, home_text.clone());
                self.door_send(conn, device_link::event_line(events::HOME, home.clone()));
            }
            if self.door.told_items.get(&conn) != Some(&last_item) {
                let told = self.door.told_items.insert(conn, last_item.clone());
                // New items only (the first time, none: the phone asks for the conversation).
                if let Some(told) = told {
                    for item in self.door_items(Some(&told), 20).as_array().cloned().unwrap_or_default() {
                        self.door_send(conn, device_link::event_line(events::CONVERSATION, item));
                    }
                }
            }
            if let Some((snapshot, text)) = &today
                && self.door.told_today.get(&conn) != Some(text)
            {
                self.door.told_today.insert(conn, text.clone());
                self.door_send(conn, device_link::event_line(events::TODAY, snapshot.clone()));
            }
        }
        let _ = cx;
    }

    fn door_send(&mut self, conn: u64, line: String) {
        let gone = self.door.conns.get(&conn).is_some_and(|c| c.out.send(line).is_err());
        if gone {
            self.door.conns.remove(&conn);
        }
    }

    fn door_reply(&mut self, conn: u64, id: u64, result: Result<Value, String>) {
        let response = match result {
            Ok(value) => Response { id, result: Some(value), error: None },
            Err(why) => Response { id, result: None, error: Some(why) },
        };
        self.door_send(conn, serde_json::to_string(&response).unwrap_or_default());
    }

    fn door_handle(&mut self, msg: DoorMsg, window: &mut Window, cx: &mut Context<Self>) {
        match msg {
            DoorMsg::Connected { conn, device, name, app, peer, out } => {
                log::info!("device door: {name} ({device}) connected from {peer}");
                self.door.conns.insert(conn, Conn { device, name, app, peer, out, since: Instant::now() });
                self.door.used = Some(Instant::now());
                cx.notify();
            }
            DoorMsg::Gone { conn } => {
                if let Some(gone) = self.door.conns.remove(&conn) {
                    log::info!("device door: {} disconnected", gone.name);
                    if !self.door.conns.values().any(|c| c.device == gone.device) {
                        self.door.presence.remove(&gone.device);
                    }
                }
                // Its question goes with it, and the ones it had waiting.
                if self.door.ask.as_ref().is_some_and(|ask| ask.conn == conn) {
                    self.door.ask = None;
                    self.mchat.device_focus = None;
                }
                self.door.drop_waiting(|w| w.conn == conn);
                self.door.told_home.remove(&conn);
                self.door.told_place.remove(&conn);
                self.door.told_items.remove(&conn);
                self.door.told_today.remove(&conn);
                cx.notify();
            }
            DoorMsg::Request { conn, request } => {
                let Some(device) = self.door.conns.get(&conn).map(|c| c.device.clone()) else { return };
                // A phone unpaired since it connected is refused from now on.
                if !self.control.authority.devices.contains(&device) {
                    self.door_reply(conn, request.id, Err("this phone is not paired with this Mac any more".into()));
                    self.door.conns.remove(&conn);
                    cx.notify();
                    return;
                }
                self.door.used = Some(Instant::now());
                self.door_request(conn, device, request, window, cx);
            }
        }
    }

    /// Answer one request of a paired phone.
    fn door_request(&mut self, conn: u64, device: String, request: Request, window: &mut Window, cx: &mut Context<Self>) {
        let Request { id, method, params } = request;
        let result: Result<Value, String> = match method.as_str() {
            "hello" => Ok(self.door_hello(&device)),
            "state" => return self.door_state(conn, id, cx),
            "sessions.peek" => {
                let session = text_param(&params, "session_id");
                let lines = params.get("lines").and_then(Value::as_u64).unwrap_or(40).clamp(5, 200) as usize;
                match self.terminals.get(&session) {
                    Some(terminal) => Ok(json!({ "text": terminal.read(cx).text_tail(lines) })),
                    None => Err(format!("no session {session} runs on this Mac")),
                }
            }
            "sessions.transcript" => return self.door_transcript(conn, id, &params, cx),
            "sessions.attach" => Err("on this Mac, the phone watches a terminal with sessions.peek (view only)".into()),
            "sessions.send" | "sessions.keys" | "sessions.answer" => {
                let session = text_param(&params, "session_id");
                let signed = signed_param(&params);
                let input = match method.as_str() {
                    "sessions.send" => MirrorInput::Text { text: text_param(&params, "text"), enter: params.get("enter").and_then(Value::as_bool).unwrap_or(true), device: Some(device.clone()), signed },
                    "sessions.keys" => MirrorInput::Keys { keys: params.get("keys").cloned().and_then(|k| serde_json::from_value(k).ok()).unwrap_or_default(), signed },
                    _ => MirrorInput::Answer {
                        option: params.get("option").and_then(Value::as_u64).unwrap_or(0) as u32,
                        text: params.get("text").and_then(Value::as_str).map(str::to_string),
                        device: Some(device.clone()),
                        signed,
                    },
                };
                match self.relayed_apply(&session, vec![input], cx).pop() {
                    Some(Ok(())) => Ok(json!({ "ok": true })),
                    Some(Err(why)) => Err(why),
                    None => Err("nothing was done".into()),
                }
            }
            "today.get" => {
                let (mac_id, mac) = self.door.info.as_ref().map(|i| (i.mac_id.clone(), i.mac.clone())).unwrap_or_default();
                serde_json::to_value(self.today_snapshot(&mac_id, &mac)).map_err(|e| e.to_string())
            }
            "today.op" => match serde_json::from_value::<glasscode_core::remote::TodayOp>(params.clone()) {
                Ok(op) => self.today_apply_checked(op, window, cx).map(|()| json!({ "ok": true })),
                Err(err) => Err(format!("not a Today op: {err}")),
            },
            // Push notifications go through the server; a phone that reaches only this Mac gets
            // what needs it in the door's notes.
            "phones.push_token" => Ok(json!({ "ok": true, "push": false })),
            "manager.home" => Ok(self.door_home()),
            // One place everywhere (`lens_pages`): the phone opens a place on the Mac, reads a
            // place's page, and its voice's tools answer here as the Mac's do.
            "place.open" => self.lens_open(&text_param(&params, "place"), window, cx).and_then(|page| serde_json::to_value(page).map_err(|e| e.to_string())),
            "place.get" => {
                let place = text_param(&params, "place");
                let place = if place.trim().is_empty() { self.lens_place() } else { place };
                serde_json::to_value(self.lens_page(&place, cx)).map_err(|e| e.to_string())
            }
            "voice.tool" => {
                let name = text_param(&params, "name");
                let arguments = params.get("arguments").cloned().unwrap_or_default();
                match name.as_str() {
                    "list_sessions" | "open_place" => Ok(json!({ "text": self.manager_voice_tool(&name, &arguments, cx) })),
                    other => Err(format!("this Mac runs only list_sessions and open_place for a phone's voice, not {other}")),
                }
            }
            "manager.conversation" => {
                let after = params.get("after").and_then(Value::as_str);
                let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(40).clamp(1, 200) as usize;
                Ok(json!({ "items": self.door_items(after, limit) }))
            }
            "manager.ask" => return self.door_ask(conn, device, id, &params, cx),
            "manager.voice" => return self.door_voice(conn, device, id, cx),
            "manager.stop" => {
                // What it asked is stopped, and what it had waiting is dropped.
                let dropped = self.door.drop_waiting(|w| w.device == device);
                for w in &dropped {
                    self.door_reply(w.conn, w.request, Err("stopped".into()));
                }
                if self.door.ask.as_ref().is_some_and(|ask| ask.device == device) {
                    self.chat_stop(cx);
                    Ok(json!({ "ok": true }))
                } else if !dropped.is_empty() {
                    Ok(json!({ "ok": true, "dropped": dropped.len() }))
                } else {
                    Err("nothing this phone asked is being answered".into())
                }
            }
            "device.presence" => match serde_json::from_value::<Presence>(params.clone()) {
                Ok(mut presence) => {
                    presence.at = Some(Instant::now());
                    if self.door.presence.get(&device).map(|p| Presence { at: None, ..p.clone() }) != Some(Presence { at: None, ..presence.clone() }) {
                        cx.notify();
                    }
                    self.door.presence.insert(device.clone(), presence);
                    Ok(json!({ "ok": true }))
                }
                Err(err) => Err(format!("not a presence: {err}")),
            },
            other => Err(format!("this Mac does not answer {other}")),
        };
        self.door_reply(conn, id, result);
    }

    /// This Mac's id and name, as the door says them.
    pub(crate) fn door_names(&self) -> (String, String) {
        self.door.info.as_ref().map(|i| (i.mac_id.clone(), i.mac.clone())).unwrap_or_else(|| (String::new(), crate::platform::this_computer().to_string()))
    }

    fn door_hello(&self, device: &str) -> Value {
        let info = self.door.info.as_ref();
        json!({
            "door": device_link::DOOR_VERSION,
            "mac": info.map(|i| i.mac.clone()).unwrap_or_default(),
            "mac_id": info.map(|i| i.mac_id.clone()).unwrap_or_default(),
            "launch": self.control.launch,
            "version": glasscode_core::update::VERSION,
            "build": glasscode_core::update::BUILD,
            "device": device,
            "features": ["manager", "today", "device", "place"],
        })
    }

    /// `manager.home`: where the manager acts from now, its name and what it is doing.
    pub fn door_home(&self) -> Value {
        let here = self.home_here();
        let server = self.server.config.as_ref().map(|_| self.server.name());
        let manager = &self.state.manager;
        json!({
            "home": if here { "mac" } else { "server" },
            "here": here,
            "generation": self.home_generation(),
            "mac": self.door.info.as_ref().map(|i| i.mac.clone()).unwrap_or_else(|| crate::platform::this_computer().to_string()),
            "server": server,
            "manager": {
                "enabled": manager.enabled,
                "name": manager.name,
                "initials": manager.initials(),
                "thinking": self.mchat.turn.is_some(),
            },
        })
    }

    /// The conversation's items after `after` (an item id), at most `limit`, oldest first.
    fn door_items(&self, after: Option<&str>, limit: usize) -> Value {
        let items = &self.mchat.items;
        let start = after.and_then(|id| items.iter().position(|item| item.id.as_str() == id).map(|at| at + 1)).unwrap_or(0);
        let newer = &items[start.min(items.len())..];
        let take = &newer[newer.len().saturating_sub(limit)..];
        Value::Array(take.iter().map(|item| serde_json::to_value(item).unwrap_or_default()).collect())
    }

    /// `state`: this Mac's projects and sessions, in the server's shape (`RemoteSession`), with
    /// each session's last reply read off the main thread.
    fn door_state(&mut self, conn: u64, id: u64, cx: &mut Context<Self>) {
        let live = self.mirror_live(cx);
        let projects: Vec<Value> = self.state.visible_projects().map(|p| json!({ "id": p.id, "name": p.name })).collect();
        let sessions: Vec<(glasscode_core::model::Session, glasscode_core::mirror::Live, Option<Value>)> = self
            .state
            .sessions
            .iter()
            .map(|s| {
                let pr = self.prs.map.get(&s.id).and_then(|status| {
                    let pr = status.pr.as_ref()?;
                    let ci = status.ci.as_ref();
                    let checks: Vec<Value> = ci.map(|ci| ci.checks.iter().map(|c| json!({ "name": c.name, "status": c.status, "conclusion": c.conclusion })).collect()).unwrap_or_default();
                    let failed = ci.map_or(0, |ci| ci.failed().len());
                    let running = ci.is_some_and(|ci| ci.running());
                    Some(json!({
                        "number": pr.number,
                        "title": pr.title,
                        "url": pr.url,
                        "state": pr.state,
                        "draft": pr.is_draft,
                        "ci": if ci.is_none() { "none" } else if failed > 0 { "failed" } else if running { "running" } else { "passed" },
                        "checks_failed": failed,
                        "checks": checks,
                        "merge_state": ci.map(|ci| ci.mergeable.clone()).unwrap_or_default(),
                    }))
                });
                (s.clone(), live.get(&s.id).cloned().unwrap_or_default(), pr)
            })
            .collect();
        let mac = self.door.info.as_ref().map(|i| i.mac.clone()).unwrap_or_default();
        let task = cx.background_spawn(async move {
            let sessions: Vec<Value> = sessions
                .into_iter()
                .map(|(session, live, pr)| {
                    let last_reply = session.transcript_path.as_ref().and_then(|path| glasscode_core::transcript::last_reply(session.agent, path));
                    let updated_at = session.transcript_path.as_ref().and_then(|path| std::fs::metadata(path).ok()).and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs());
                    let mut value = serde_json::to_value(glasscode_core::remote::RemoteSession {
                        session,
                        running: live.running,
                        activity: live.activity,
                        tmux_name: String::new(),
                        prompt: live.prompt,
                        last_reply,
                        updated_at,
                        pr: None,
                        host: None,
                        synced_at: None,
                        signed_out: false,
                    })
                    .unwrap_or_default();
                    if let Some(pr) = pr {
                        value["pr"] = pr;
                    }
                    value
                })
                .collect();
            json!({ "mac": mac, "projects": projects, "sessions": sessions })
        });
        cx.spawn(async move |this, cx| {
            let state = task.await;
            let _ = this.update(cx, |ws, _| ws.door_reply(conn, id, Ok(state)));
        })
        .detach();
    }

    /// `sessions.transcript`: a page of a session's conversation, read off the main thread.
    fn door_transcript(&mut self, conn: u64, id: u64, params: &Value, cx: &mut Context<Self>) {
        let session = text_param(params, "session_id");
        let after = params.get("after").and_then(Value::as_u64);
        let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(200).clamp(1, 2000) as usize;
        let Some((agent, path)) = self.state.session(&session).map(|s| (s.agent, s.transcript_path.clone())) else {
            self.door_reply(conn, id, Err(format!("no session {session} on this Mac")));
            return;
        };
        let task = cx.background_spawn(async move {
            match path {
                Some(path) => glasscode_core::transcript::read(agent, &path, after, limit).map_err(|e| format!("{e:#}")).and_then(|page| serde_json::to_value(page).map_err(|e| e.to_string())),
                None => serde_json::to_value(glasscode_core::transcript::TranscriptPage { items: Vec::new(), cursor: 0, reset: after.is_none() }).map_err(|e| e.to_string()),
            }
        });
        cx.spawn(async move |this, cx| {
            let page = task.await;
            let _ = this.update(cx, |ws, _| ws.door_reply(conn, id, page));
        })
        .detach();
    }

    /// `manager.voice`: a pass to the manager's voice room for this phone (its address over
    /// Tailscale, a token for an hour, the room). Only while a voice runs: on this Mac (Settings ›
    /// Manager › Voice › On this Mac) or on a server whose address is set.
    fn door_voice(&mut self, conn: u64, device: String, id: u64, cx: &mut Context<Self>) {
        let settings = self.state.manager.clone();
        if !settings.enabled {
            return self.door_reply(conn, id, Err("The manager is off. Switch it on in Settings › Manager on your Mac.".into()));
        }
        let url = if settings.voice.local {
            match (&self.vserver.status, self.vserver.phone_url()) {
                (crate::voice_server::Status::Running, Some(url)) => url,
                (crate::voice_server::Status::Running, None) => return self.door_reply(conn, id, Err("The voice runs on your Mac, but Tailscale does not, so your phone cannot reach it.".into())),
                (crate::voice_server::Status::Failed(why), _) => return self.door_reply(conn, id, Err(why.clone())),
                _ => return self.door_reply(conn, id, Err("The voice on your Mac is not running yet.".into())),
            }
        } else {
            let url = settings.voice.livekit_url.trim().to_string();
            if url.is_empty() {
                return self.door_reply(conn, id, Err("Voice is not set up: switch on Settings › Manager › Voice › On this Mac.".into()));
            }
            url
        };
        let room = glasscode_core::manager::voice::room(&settings);
        let short: String = device.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect();
        let identity = format!("phone-{short}");
        let (name, talker) = (settings.name.clone(), !settings.voice.talker.trim().is_empty() && settings.voice.local);
        // The Keychain may take a moment: not on the main thread.
        let task = cx.background_spawn(async move {
            let (key, secret) = glasscode_core::manager::voice::keys().ok_or_else(|| "The voice's key is missing on your Mac.".to_string())?;
            glasscode_core::manager::voice::token(&key, &secret, &identity, &room, 60 * 60).map(|token| json!({ "url": url, "token": token, "room": room, "identity": identity, "name": name, "talker": talker })).map_err(|e| format!("Could not make a voice pass: {e:#}"))
        });
        cx.spawn(async move |this, cx| {
            let pass = task.await;
            let _ = this.update(cx, |ws, _| ws.door_reply(conn, id, pass));
        })
        .detach();
    }

    /// `manager.ask`: the person's question from their glasses, a tier-2 message of the device's
    /// (D1), in the one conversation. The answer streams to that connection as it is written, and
    /// is the request's reply when it is done. While the server is home, the phone is told to ask
    /// there.
    fn door_ask(&mut self, conn: u64, device: String, id: u64, params: &Value, cx: &mut Context<Self>) {
        // Behind the ones that wait already, in order.
        if !self.door.waiting.is_empty() && !text_param(params, "text").trim().is_empty() {
            return self.door_wait(conn, device, id, params);
        }
        self.door_ask_now(conn, device, id, params, cx);
    }

    /// Hold a device's question until the manager is free (`door_fast_tick` asks it then), and
    /// tell the device it waits.
    fn door_wait(&mut self, conn: u64, device: String, id: u64, params: &Value) {
        if self.door.wait(conn, &device, id, params).is_err() {
            return self.door_reply(conn, id, Err(format!("{} has {WAITING_MAX} of your questions waiting already; ask again once it answers.", self.state.manager.name)));
        }
        let client_id: String = text_param(params, "client_id").chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(64).collect();
        self.door_send(conn, device_link::event_line(events::WORKING, json!({ "ask": client_id, "state": "waiting", "line": "It waits for the question before it, then it is answered." })));
    }

    fn door_ask_now(&mut self, conn: u64, device: String, id: u64, params: &Value, cx: &mut Context<Self>) {
        let text = text_param(params, "text").trim().to_string();
        let client_id: String = text_param(params, "client_id").chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(64).collect();
        if text.is_empty() {
            return self.door_reply(conn, id, Err("There is nothing to ask.".into()));
        }
        if !self.state.manager.enabled {
            return self.door_reply(conn, id, Err("The manager is off. Switch it on in Settings › Manager on your Mac.".into()));
        }
        if self.server.config.is_some() && !self.home_here() {
            return self.door_reply(conn, id, Ok(json!({ "elsewhere": "server", "home": self.door_home() })));
        }
        // Answering another question (this phone's, the Mac's chat or its voice, a command's):
        // it waits its turn, as the Mac's own chat queues the person's messages.
        if self.door.ask.is_some() || self.mchat.turn.is_some() || !self.mchat.queue.is_empty() {
            return self.door_wait(conn, device, id, params);
        }
        let focus: glasscode_core::runtime::chat::DeviceFocus = params.get("focus").cloned().and_then(|f| serde_json::from_value(f).ok()).unwrap_or_default();
        // Only ids this Mac knows: "this" never names a session the brain cannot find.
        let session_title = focus.session.as_deref().and_then(|s| self.state.session(s)).map(|s| self.session_title(s));
        let focus = glasscode_core::runtime::chat::DeviceFocus { session: focus.session.filter(|_| session_title.is_some()), ..focus };
        // O8: the Mac's window moves only while the person was at it a moment ago, or when they
        // ask to see it there.
        let window_moves = self.window_active || glasscode_core::runtime::chat::asks_for_the_mac(&text);
        let now = glasscode_core::clock::now_ms();
        self.door.ask = Some(DeviceAsk { conn, device: device.clone(), request: id, client_id: client_id.clone(), since: Instant::now(), sent: 0, activity: None, window_moves });
        self.door.last_ask = Some((device.clone(), conn, now));
        let block = glasscode_core::runtime::chat::glasses_block(&focus, session_title.as_deref(), window_moves);
        self.mchat.device_focus = Some(format!("{block}\n[{}]", self.projects_line()));
        let words = glasscode_core::words::record(glasscode_core::words::Origin::Device(device), &text, focus.session.as_deref(), None);
        let on_answer: crate::manager_chat::OnAnswer = Box::new(move |ws, said, _cx| ws.door_answered(conn, id, said));
        self.manager_ask_as(text, false, Some(words), None, Some(on_answer), cx);
        // An answer from code came back at once; one for the brain streams from the fast tick.
        cx.notify();
    }

    /// The question a device asked is answered (or could not be).
    fn door_answered(&mut self, conn: u64, request: u64, said: Result<String, String>) {
        let ask = self.door.ask.take().filter(|ask| ask.conn == conn && ask.request == request);
        self.mchat.device_focus = None;
        let client_id = ask.map(|a| a.client_id).unwrap_or_default();
        match said {
            Ok(text) => {
                self.door_send(conn, device_link::event_line(events::ANSWER, json!({ "ask": client_id, "text": text, "done": true })));
                self.door_reply(conn, request, Ok(json!({ "say": text })));
            }
            Err(why) => {
                self.door_send(conn, device_link::event_line(events::ANSWER, json!({ "ask": client_id, "text": "", "done": true, "error": why })));
                self.door_reply(conn, request, Err(why));
            }
        }
    }

    /// What phones that reach the server said, with the sync (G08): a phone the server heard from
    /// in the last 90 s counts as connected there, for the title bar.
    pub fn door_presence_from_server(&mut self, said: &HashMap<String, Value>) {
        let now = glasscode_core::clock::now_ms();
        let fresh: HashMap<String, Presence> = said
            .iter()
            .filter(|(_, v)| now.saturating_sub(v["at_ms"].as_u64().unwrap_or(0)) < PRESENT_FOR.as_millis() as u64)
            .filter_map(|(device, v)| serde_json::from_value::<Presence>(v.clone()).ok().map(|p| (device.clone(), Presence { via: "server".into(), ..p })))
            .collect();
        self.door.via_server = fresh;
    }

    /// The person's paired devices and which are connected (`device.list`).
    pub fn door_devices(&self) -> Value {
        let connected: BTreeSet<&str> = self.door.conns.values().map(|c| c.device.as_str()).collect();
        let connections: Vec<Value> = self.door.conns.values().map(|c| json!({ "device": c.device, "name": c.name, "app": c.app, "from": c.peer.ip().to_string(), "for_secs": c.since.elapsed().as_secs() })).collect();
        let devices: Vec<Value> = self
            .control
            .authority
            .devices
            .iter()
            .map(|id| {
                let presence = self.door.presence.get(id);
                json!({
                    "id": id,
                    "connected": connected.contains(id.as_str()),
                    "glasses": presence.is_some_and(|p| p.glasses),
                    "worn": presence.is_some_and(|p| p.worn),
                    "voice": presence.map(|p| p.voice.clone()).unwrap_or_default(),
                })
            })
            .collect();
        json!({ "devices": devices, "connections": connections, "listening": self.door.listening.iter().map(|a| a.to_string()).collect::<Vec<_>>() })
    }

    /// `device.open`: put a place on the lens of the device that asked the chat's question of the
    /// last 30 s. Its title and what the lens shows come with it.
    pub fn door_open(&mut self, place: DevicePlace) -> Result<String, String> {
        let now = glasscode_core::clock::now_ms();
        let Some((device, conn, _)) = self.door.last_ask.clone().filter(|(_, _, at)| now.saturating_sub(*at) <= OPEN_WITHIN_MS) else {
            return Err("It shows a place on the glasses only in answer to a question from them of the last 30 seconds.".into());
        };
        let conn = if self.door.conns.contains_key(&conn) { conn } else { self.door.conns.iter().find(|(_, c)| c.device == device).map(|(k, _)| *k).ok_or("The glasses are not connected now.")? };
        let (data, said) = match place {
            DevicePlace::Today => (json!({ "kind": "today" }), "Today".to_string()),
            DevicePlace::Conversation => (json!({ "kind": "conversation" }), "the conversation".to_string()),
            DevicePlace::Project(name) => {
                let project = self.state.visible_projects().find(|p| p.name.eq_ignore_ascii_case(&name) || p.id == name).ok_or_else(|| format!("No project is called {name}."))?;
                (json!({ "kind": "project", "id": project.id, "title": project.name }), format!("the project {}", project.name))
            }
            DevicePlace::Pr(number) => {
                let found = self.state.sessions.iter().find_map(|s| self.prs.map.get(&s.id).and_then(|st| st.pr.as_ref().filter(|pr| pr.number == number).map(|pr| (s.id.clone(), pr.title.clone(), st.ci.clone()))));
                let (session, title, ci) = found.map(|(s, t, ci)| (Some(s), t, ci)).unwrap_or((None, String::new(), None));
                let checks: Vec<Value> = ci.as_ref().map(|ci| ci.checks.iter().map(|c| json!({ "name": c.name, "status": c.status, "conclusion": c.conclusion })).collect()).unwrap_or_default();
                (json!({ "kind": "pr", "number": number, "title": title, "session": session, "checks": checks, "merge_state": ci.map(|ci| ci.mergeable).unwrap_or_default() }), format!("pull request #{number}"))
            }
            DevicePlace::Session(word) => {
                let lower = word.to_lowercase();
                let session = self.state.session(&word).or_else(|| self.state.sessions.iter().find(|s| s.id.starts_with(&word))).or_else(|| self.state.sessions.iter().find(|s| self.session_title(s).to_lowercase().contains(&lower))).ok_or_else(|| format!("No session matches “{word}”."))?;
                let title = self.session_title(session);
                (json!({ "kind": "session", "id": session.id, "title": title, "branch": session.branch, "agent": session.agent.name() }), format!("the session “{title}”"))
            }
        };
        self.door_send(conn, device_link::event_line(events::OPEN, data));
        Ok(format!("Shown on the glasses: {said}."))
    }
}

/// What the title bar's glasses icon shows (G10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlassesLook {
    /// No phone is paired: no icon.
    Hidden,
    /// Paired, not connected.
    Grey,
    /// Connected, and the glasses are on.
    Green,
    /// The person is talking from the glasses: listening, thinking or speaking.
    Talking,
    /// Connected, but the lens is off or the glasses are not there, or the door cannot open.
    Amber,
}

/// The icon's look from what is known: paired devices, connections and what they said.
pub fn glasses_look(paired: usize, connected: usize, presence: &[&Presence], asking: bool, door_error: bool) -> GlassesLook {
    if paired == 0 {
        return GlassesLook::Hidden;
    }
    if connected == 0 {
        return if door_error { GlassesLook::Amber } else { GlassesLook::Grey };
    }
    if asking || presence.iter().any(|p| matches!(p.voice.as_str(), "listening" | "thinking" | "speaking")) {
        return GlassesLook::Talking;
    }
    if presence.iter().any(|p| p.glasses && p.lens) {
        GlassesLook::Green
    } else {
        GlassesLook::Amber
    }
}

impl Workspace {
    /// The title bar's glasses icon: none with no phone paired; a click opens Settings › Glasses.
    pub fn render_glasses_icon(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        use gpui::{ClickEvent, div, prelude::*};
        // Through this Mac's door, or through the server (its sync says what the phone told it).
        let presence: Vec<&Presence> = self.door.presence.values().chain(self.door.via_server.values()).collect();
        let connected = self.door.conns.len() + self.door.via_server.len();
        let look = glasses_look(self.control.authority.devices.len(), connected, &presence, self.door.ask.is_some(), self.door.error.is_some());
        let (dot, color) = match look {
            GlassesLook::Hidden => return None,
            GlassesLook::Grey => (None, crate::theme::faint()),
            GlassesLook::Green => (Some(crate::theme::added()), crate::theme::text_soft()),
            GlassesLook::Talking => (Some(crate::theme::added()), crate::theme::text_strong()),
            GlassesLook::Amber => (Some(crate::theme::attention()), crate::theme::text_soft()),
        };
        let size = crate::theme::sz(if look == GlassesLook::Talking { 9. } else { 7. });
        Some(
            div()
                .id("title-glasses")
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(crate::theme::sz(30.))
                .rounded(crate::theme::sz(6.))
                .cursor_pointer()
                .hover(|s| s.bg(crate::theme::hover()))
                .when(look == GlassesLook::Talking, |d| d.bg(crate::theme::selected()))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_settings(crate::settings_ui::Section::Server, window, cx)))
                .child(crate::icons::icon(crate::icons::GLASSES, crate::theme::sz(20.), crate::theme::sz(20.), color))
                .when_some(dot, |d, dot| d.child(div().absolute().top(crate::theme::sz(4.)).right(crate::theme::sz(3.)).size(size).rounded_full().border_1().border_color(crate::theme::chrome()).bg(dot)))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> Arc<DoorInfo> {
        Arc::new(DoorInfo { key: DoorKey::generate(), mac: "Test Mac".into(), mac_id: "0123456789abcdef".into(), launch: "launch-1".into() })
    }

    /// A device's questions asked while the manager answers wait, in order, a few per device;
    /// they go when their connection goes or the device stops, and after WAIT_WITHIN.
    #[test]
    fn questions_wait_their_turn() {
        let mut door = DoorUi::default();
        let ask = |text: &str| json!({ "text": text, "client_id": text });
        for (i, text) in ["one", "two"].iter().enumerate() {
            assert_eq!(door.wait(1, "phone-a", i as u64, &ask(text)), Ok(()));
        }
        assert_eq!(door.wait(2, "phone-b", 9, &ask("other")), Ok(()));
        let order: Vec<&str> = door.waiting.iter().map(|w| w.params["text"].as_str().unwrap()).collect();
        assert_eq!(order, ["one", "two", "other"]);
        for i in 2..WAITING_MAX as u64 {
            assert_eq!(door.wait(1, "phone-a", i, &ask("more")), Ok(()));
        }
        assert_eq!(door.wait(1, "phone-a", 99, &ask("too many")), Err(()), "a device has at most WAITING_MAX waiting");
        assert!(door.waited_too_long().is_empty());
        // The other phone's stays when this one stops.
        let dropped = door.drop_waiting(|w| w.device == "phone-a");
        assert_eq!(dropped.len(), WAITING_MAX);
        assert_eq!(door.waiting.len(), 1);
        assert_eq!(door.drop_waiting(|w| w.conn == 2).len(), 1);
        // One that waited too long is taken out.
        door.waiting.push_back(WaitingAsk { conn: 3, device: "phone-c".into(), request: 1, params: ask("late"), since: Instant::now() - WAIT_WITHIN - Duration::from_secs(1) });
        assert_eq!(door.waited_too_long().len(), 1);
        assert!(door.waiting.is_empty());
    }

    /// Connect as a phone with `phone`'s key and do the handshake; the welcome line.
    fn handshake(addr: SocketAddr, phone: &DoorKey, pinned: &str) -> (Welcome, BufReader<TcpStream>, TcpStream) {
        let stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let hello: device_link::DoorHello = serde_json::from_str(line.trim()).unwrap();
        device_link::check_door(&hello, pinned).unwrap();
        let mut out = stream.try_clone().unwrap();
        write_line(&mut out, &serde_json::to_string(&device_link::answer(&hello, phone, "iPhone")).unwrap()).unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        (serde_json::from_str(line.trim()).unwrap(), reader, out)
    }

    fn door(paired: &[&DoorKey]) -> (Vec<SocketAddr>, Arc<DoorInfo>, async_channel::Receiver<DoorMsg>, Arc<AtomicBool>) {
        let info = info();
        let set: BTreeSet<String> = paired.iter().map(|k| glasscode_core::gate::device_id(&k.public_openssh())).collect();
        let (tx, rx) = async_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let bound = listen(&[SocketAddr::from(([127, 0, 0, 1], 0))], true, info.clone(), Arc::new(Mutex::new(set)), tx, stop.clone()).unwrap();
        (bound, info, rx, stop)
    }

    /// G04: the connection is the paired phone's, named by the key it proved.
    #[test]
    fn door_caller_is_the_phone() {
        let phone = DoorKey::generate();
        let (addrs, info, rx, stop) = door(&[&phone]);
        let (welcome, _reader, _out) = handshake(addrs[0], &phone, &info.key.public_openssh());
        assert!(welcome.ok, "{welcome:?}");
        assert_eq!(welcome.device, glasscode_core::gate::device_id(&phone.public_openssh()));
        match rx.recv_blocking().unwrap() {
            DoorMsg::Connected { device, .. } => assert_eq!(device, welcome.device),
            _ => panic!("not connected"),
        }
        stop.store(true, Ordering::Relaxed);
    }

    #[test]
    fn door_refuses_a_key_it_did_not_pair() {
        let (paired, stranger) = (DoorKey::generate(), DoorKey::generate());
        let (addrs, info, rx, stop) = door(&[&paired]);
        let (welcome, _reader, _out) = handshake(addrs[0], &stranger, &info.key.public_openssh());
        assert!(!welcome.ok);
        assert!(welcome.error.unwrap().contains("not paired"));
        assert!(rx.try_recv().is_err(), "the window never hears of it");
        stop.store(true, Ordering::Relaxed);
    }

    /// A method a phone may not call is refused on the connection's thread; an allowed one goes
    /// to the window.
    #[test]
    fn door_runs_only_the_list() {
        let phone = DoorKey::generate();
        let (addrs, info, rx, stop) = door(&[&phone]);
        let (welcome, mut reader, mut out) = handshake(addrs[0], &phone, &info.key.public_openssh());
        assert!(welcome.ok);
        let _ = rx.recv_blocking().unwrap();
        write_line(&mut out, r#"{"id":7,"method":"sessions.create","params":{}}"#).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let response: Response = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(response.id, 7);
        assert!(response.error.unwrap().contains("cannot call sessions.create"));
        write_line(&mut out, r#"{"id":8,"method":"manager.home"}"#).unwrap();
        match rx.recv_blocking().unwrap() {
            DoorMsg::Request { request, .. } => assert_eq!((request.id, request.method.as_str()), (8, "manager.home")),
            _ => panic!("no request"),
        }
        stop.store(true, Ordering::Relaxed);
    }

    /// A connection that is not from the tailnet (here: loopback, not allowed) is closed before
    /// the door says anything.
    #[test]
    fn door_refuses_a_lan_address() {
        let info = info();
        let (tx, rx) = async_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let addrs = listen(&[SocketAddr::from(([127, 0, 0, 1], 0))], false, info, Arc::new(Mutex::new(BTreeSet::new())), tx, stop.clone()).unwrap();
        let stream = TcpStream::connect(addrs[0]).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut line = String::new();
        let n = BufReader::new(stream).read_line(&mut line).unwrap_or(0);
        assert_eq!(n, 0, "closed without a hello: {line}");
        assert!(rx.try_recv().is_err());
        stop.store(true, Ordering::Relaxed);
    }

    /// G10: the icon is hidden with nothing paired, grey when not connected, green with the
    /// glasses on, and says when the person talks from them.
    #[test]
    fn indicator_states_from_presence() {
        let on = Presence { glasses: true, worn: true, lens: true, voice: "idle".into(), ..Default::default() };
        let talking = Presence { voice: "listening".into(), ..on.clone() };
        let off = Presence { glasses: false, ..Default::default() };
        assert_eq!(glasses_look(0, 0, &[], false, false), GlassesLook::Hidden);
        assert_eq!(glasses_look(1, 0, &[], false, false), GlassesLook::Grey);
        assert_eq!(glasses_look(1, 0, &[], false, true), GlassesLook::Amber);
        assert_eq!(glasses_look(1, 1, &[&on], false, false), GlassesLook::Green);
        assert_eq!(glasses_look(1, 1, &[&talking], false, false), GlassesLook::Talking);
        assert_eq!(glasses_look(1, 1, &[&on], true, false), GlassesLook::Talking);
        assert_eq!(glasses_look(1, 1, &[&off], false, false), GlassesLook::Amber);
    }

    /// The iPhone app's own link code against this door (script/glasses-e2e): the handshake both
    /// ways, a call and its answer, a refused method, a signed input the Mac's #288 check takes,
    /// and an event. Skipped unless GLASSCODE_DOOR_E2E names the built stand-in.
    #[test]
    fn door_end_to_end_with_the_swift_client() {
        let Some(client) = std::env::var_os("GLASSCODE_DOOR_E2E") else {
            eprintln!("skipped: build the stand-in with script/glasses-e2e/build.sh and set GLASSCODE_DOOR_E2E");
            return;
        };
        let phone = DoorKey::from_hex(&"09".repeat(32)).unwrap();
        let device = glasscode_core::gate::device_id(&phone.public_openssh());
        let (addrs, info, rx, stop) = door(&[&phone]);
        let child = std::process::Command::new(client)
            .env("DOOR_ADDR", addrs[0].to_string())
            .env("DOOR_KEY", info.key.public_openssh())
            .env("PHONE_RAW", "09".repeat(32))
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(40);
        let mut out: Option<mpsc::Sender<String>> = None;
        let mut used = std::collections::HashSet::new();
        let mut answered = 0;
        while answered < 2 && Instant::now() < deadline {
            let Ok(msg) = rx.try_recv() else {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            };
            match msg {
                DoorMsg::Connected { device: d, out: o, .. } => {
                    assert_eq!(d, device);
                    out = Some(o);
                }
                DoorMsg::Request { request, .. } => {
                    let reply = match request.method.as_str() {
                        "manager.home" => json!({ "home": "mac", "manager": { "name": "Bigboss", "enabled": true } }),
                        "sessions.send" => {
                            let p = &request.params;
                            let input = MirrorInput::Text { text: p["text"].as_str().unwrap().into(), enter: p["enter"].as_bool().unwrap(), device: None, signed: serde_json::from_value(p["signed"].clone()).ok() };
                            let checked = glasscode_core::remote::check_relayed(&input, "s1", glasscode_core::model::AgentKind::Claude, &info.launch, &|d| d == device, &mut used);
                            assert_eq!(checked, Ok(()), "the Mac takes the phone's signed input");
                            json!({ "ok": true })
                        }
                        other => panic!("{other} reached the window"),
                    };
                    let o = out.as_ref().unwrap();
                    o.send(serde_json::to_string(&Response { id: request.id, result: Some(reply), error: None }).unwrap()).unwrap();
                    answered += 1;
                    if answered == 2 {
                        o.send(device_link::event_line(events::OPEN, json!({ "kind": "session", "id": "s1" }))).unwrap();
                    }
                }
                DoorMsg::Gone { .. } => break,
            }
        }
        let output = child.wait_with_output().unwrap();
        stop.store(true, Ordering::Relaxed);
        let lines: Vec<Value> = String::from_utf8_lossy(&output.stdout).lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        let has = |key: &str| lines.iter().find(|l| l.get(key).is_some()).cloned().unwrap_or_else(|| panic!("no {key} in {lines:?}"));
        assert_eq!(has("connected")["launch"], json!("launch-1"));
        assert_eq!(has("home")["home"]["manager"]["name"], json!("Bigboss"));
        assert_eq!(has("refused")["refused"], json!(true), "sessions.create is refused at the door");
        assert_eq!(has("sent")["sent"]["ok"], json!(true));
        assert_eq!(has("event")["event"], json!("device.open"));
    }

    #[test]
    fn device_places_parse() {
        assert!(matches!(DevicePlace::parse("pr:42"), Some(DevicePlace::Pr(42))));
        assert!(matches!(DevicePlace::parse("#7"), Some(DevicePlace::Pr(7))));
        assert!(matches!(DevicePlace::parse("today"), Some(DevicePlace::Today)));
        assert!(matches!(DevicePlace::parse("project:shop"), Some(DevicePlace::Project(p)) if p == "shop"));
        assert!(matches!(DevicePlace::parse("checkout"), Some(DevicePlace::Session(s)) if s == "checkout"));
        assert!(DevicePlace::parse("  ").is_none());
    }
}
