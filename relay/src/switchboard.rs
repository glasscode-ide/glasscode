//! Listeners, pending connections and the byte pipe between two devices.
//!
//! A **host** (the Mac, a build server) keeps a `listen` control socket open. A **client**
//! (the phone, the Mac) calls `connect?to=<device>`; the relay tells that device's listener,
//! the host opens an `accept` socket, and the relay joins the two and passes binary frames
//! through until either side closes. What flows inside is the devices' own traffic — the relay
//! sees who talks to whom and how much, never what.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::auth::{Auth, Identity};

/// Frames larger than this are refused (control/SSH packets are far smaller).
const MAX_FRAME: usize = 1 << 20;
/// Pings keep NATs and load balancers from closing idle sockets.
const PING_EVERY: Duration = Duration::from_secs(20);
/// How long a client waits for the host to pick up.
const ACCEPT_WITHIN: Duration = Duration::from_secs(15);
/// Sockets one relay may have open at once (a single shared-secret group).
const MAX_SOCKETS: usize = 256;

struct Listener {
    identity: Identity,
    control: mpsc::Sender<String>,
    /// Distinguishes a reconnected listener from the one it replaced.
    epoch: u64,
}

struct Pending {
    target: String,
    host: oneshot::Sender<WebSocket>,
}

pub struct Switchboard {
    pub auth: Auth,
    listeners: Mutex<HashMap<String, Listener>>,
    pending: Mutex<HashMap<String, Pending>>,
    /// group → open sockets (one group per relay).
    open: Mutex<HashMap<String, usize>>,
    epoch: std::sync::atomic::AtomicU64,
}

impl Switchboard {
    pub fn new(secret: String) -> Arc<Switchboard> {
        Arc::new(Switchboard {
            auth: Auth::new(secret),
            listeners: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            open: Mutex::new(HashMap::new()),
            epoch: std::sync::atomic::AtomicU64::new(1),
        })
    }

    /// Count a socket against the group; false when the relay is at its cap.
    async fn admit(&self, user: &str) -> bool {
        let mut open = self.open.lock().await;
        let n = open.entry(user.to_string()).or_insert(0);
        if *n >= MAX_SOCKETS {
            return false;
        }
        *n += 1;
        true
    }

    async fn release(&self, user: &str) {
        let mut open = self.open.lock().await;
        if let Some(n) = open.get_mut(user) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                open.remove(user);
            }
        }
    }

    /// Tell the group's other listeners about a device coming or going.
    async fn announce(&self, user: &str, except: &str, device: &str, kind: &str, online: bool) {
        let message = json!({ "type": "presence", "device": device, "kind": kind, "online": online }).to_string();
        for listener in self.listeners.lock().await.values() {
            if listener.identity.user == user && listener.identity.device != except {
                let _ = listener.control.try_send(message.clone());
            }
        }
    }
}

fn refuse(status: u16, message: String) -> Response {
    (StatusCode::from_u16(status).unwrap_or(StatusCode::FORBIDDEN), message).into_response()
}

// ----- listener (hosts) ---------------------------------------------------------------

/// A host's control socket: the relay sends it `{"type":"incoming","conn":…,"from":…}`
/// when a client wants it, and presence changes of the group's other devices.
pub async fn listen(State(board): State<Arc<Switchboard>>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let identity = match board.auth.identify(&headers) {
        Ok(identity) => identity,
        Err((status, message)) => return refuse(status, message),
    };
    if !board.admit(&identity.user).await {
        return refuse(429, "too many connections".into());
    }
    ws.max_message_size(MAX_FRAME).max_frame_size(MAX_FRAME).on_upgrade(move |socket| run_listener(board, identity, socket))
}

async fn run_listener(board: Arc<Switchboard>, identity: Identity, socket: WebSocket) {
    let (mut tx, mut rx) = socket.split();
    let (control, mut controls) = mpsc::channel::<String>(64);
    let epoch = board.epoch.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let online: Vec<serde_json::Value> = {
        let mut listeners = board.listeners.lock().await;
        listeners.insert(identity.device.clone(), Listener { identity: identity.clone(), control, epoch });
        listeners
            .values()
            .filter(|l| l.identity.user == identity.user && l.identity.device != identity.device)
            .map(|l| json!({ "device": l.identity.device, "kind": l.identity.kind }))
            .collect()
    };
    tracing::info!(device = %identity.device, kind = %identity.kind, "listener up");
    let _ = tx.send(Message::Text(json!({ "type": "hello", "device": identity.device, "online": online }).to_string().into())).await;
    board.announce(&identity.user, &identity.device, &identity.device, &identity.kind, true).await;
    let mut ping = tokio::time::interval(PING_EVERY);
    loop {
        tokio::select! {
            message = controls.recv() => match message {
                Some(text) => if tx.send(Message::Text(text.into())).await.is_err() { break },
                None => break,
            },
            incoming = rx.next() => match incoming {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            _ = ping.tick() => if tx.send(Message::Ping(Vec::new().into())).await.is_err() { break },
        }
    }
    let replaced = {
        let mut listeners = board.listeners.lock().await;
        match listeners.get(&identity.device) {
            Some(l) if l.epoch == epoch => {
                listeners.remove(&identity.device);
                false
            }
            _ => true,
        }
    };
    if !replaced {
        board.announce(&identity.user, &identity.device, &identity.device, &identity.kind, false).await;
    }
    board.release(&identity.user).await;
    tracing::info!(device = %identity.device, "listener down");
}

// ----- connect (clients) and accept (hosts) -------------------------------------------

#[derive(Deserialize)]
pub struct ConnectQuery {
    to: String,
    #[serde(default)]
    channel: Option<String>,
}

pub async fn connect(State(board): State<Arc<Switchboard>>, headers: HeaderMap, Query(q): Query<ConnectQuery>, ws: WebSocketUpgrade) -> Response {
    let identity = match board.auth.identify(&headers) {
        Ok(identity) => identity,
        Err((status, message)) => return refuse(status, message),
    };
    let control = {
        let listeners = board.listeners.lock().await;
        match listeners.get(&q.to) {
            Some(l) if l.identity.user == identity.user => l.control.clone(),
            Some(_) => return refuse(403, "not your device".into()),
            None => return refuse(404, "that device is not online".into()),
        }
    };
    if !board.admit(&identity.user).await {
        return refuse(429, "too many connections".into());
    }
    let conn = uuid::Uuid::new_v4().to_string();
    let (host_tx, host_rx) = oneshot::channel();
    board.pending.lock().await.insert(conn.clone(), Pending { target: q.to.clone(), host: host_tx });
    let notice = json!({
        "type": "incoming",
        "conn": conn,
        "from": identity.device,
        "from_kind": identity.kind,
        "from_name": identity.name,
        "channel": q.channel.clone().unwrap_or_else(|| "control".into()),
    })
    .to_string();
    if control.send(notice).await.is_err() {
        board.pending.lock().await.remove(&conn);
        board.release(&identity.user).await;
        return refuse(404, "that device just went offline".into());
    }
    let target = q.to.clone();
    ws.max_message_size(MAX_FRAME).max_frame_size(MAX_FRAME).on_upgrade(move |client| async move {
        let host = tokio::time::timeout(ACCEPT_WITHIN, host_rx).await;
        board.pending.lock().await.remove(&conn);
        match host {
            Ok(Ok(host)) => {
                tracing::info!(from = %identity.device, to = %target, %conn, "joined");
                splice(client, host).await;
                tracing::info!(%conn, "closed");
            }
            _ => {
                let mut client = client;
                let _ = client.send(Message::Close(Some(CloseFrame { code: 4004, reason: "the device did not answer".into() }))).await;
            }
        }
        board.release(&identity.user).await;
    })
}

#[derive(Deserialize)]
pub struct AcceptQuery {
    conn: String,
}

pub async fn accept(State(board): State<Arc<Switchboard>>, headers: HeaderMap, Query(q): Query<AcceptQuery>, ws: WebSocketUpgrade) -> Response {
    let identity = match board.auth.identify(&headers) {
        Ok(identity) => identity,
        Err((status, message)) => return refuse(status, message),
    };
    let pending = {
        let mut pending = board.pending.lock().await;
        match pending.get(&q.conn) {
            Some(p) if p.target == identity.device => pending.remove(&q.conn),
            Some(_) => return refuse(403, "that connection is for another device".into()),
            None => return refuse(404, "no such connection (it timed out)".into()),
        }
    };
    let Some(pending) = pending else { return refuse(404, "no such connection".into()) };
    ws.max_message_size(MAX_FRAME).max_frame_size(MAX_FRAME).on_upgrade(move |host| async move {
        // The client side owns the pipe from here; if it gave up, this socket just closes.
        let _ = pending.host.send(host);
    })
}

/// Pass binary frames both ways until either side closes.
async fn splice(a: WebSocket, b: WebSocket) {
    let (mut a_tx, mut a_rx) = a.split();
    let (mut b_tx, mut b_rx) = b.split();
    let mut ping = tokio::time::interval(PING_EVERY);
    loop {
        tokio::select! {
            m = a_rx.next() => match m {
                Some(Ok(Message::Binary(data))) => if b_tx.send(Message::Binary(data)).await.is_err() { break },
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            m = b_rx.next() => match m {
                Some(Ok(Message::Binary(data))) => if a_tx.send(Message::Binary(data)).await.is_err() { break },
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            _ = ping.tick() => {
                if a_tx.send(Message::Ping(Vec::new().into())).await.is_err() || b_tx.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }
    let _ = a_tx.send(Message::Close(None)).await;
    let _ = b_tx.send(Message::Close(None)).await;
}

// ----- presence ---------------------------------------------------------------------------

/// Which of the group's devices are reachable now (hosts with a listener).
pub async fn online(State(board): State<Arc<Switchboard>>, headers: HeaderMap) -> Response {
    let identity = match board.auth.identify(&headers) {
        Ok(identity) => identity,
        Err((status, message)) => return refuse(status, message),
    };
    let devices: Vec<serde_json::Value> = board
        .listeners
        .lock()
        .await
        .values()
        .filter(|l| l.identity.user == identity.user)
        .map(|l| json!({ "device": l.identity.device, "kind": l.identity.kind, "name": l.identity.name }))
        .collect();
    Json(json!({ "online": devices })).into_response()
}
