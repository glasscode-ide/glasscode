//! Byte pipes through the Glasscode relay. The devices' own traffic runs inside them, end to
//! end; the relay only joins the sockets.
//!
//! - [`listen`]: the host side. One control socket stays open; each client the relay announces
//!   gets its own socket, joined to a local address chosen by channel (`control` → the voice
//!   server, `ssh` → a build server's sshd).
//! - [`connect_stdio`]: the client side as an SSH `ProxyCommand` (stdin/stdout ↔ relay).
//!
//! Every socket presents the relay's shared secret as a bearer token and names its device with
//! `x-device-*` headers. See `relay/` for the server.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as _, bail};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Pings keep NATs and load balancers from closing quiet sockets.
const PING_EVERY: Duration = Duration::from_secs(20);

/// How a device names itself to the relay (the secret authorises; the id is a label).
#[derive(Clone, Debug)]
pub struct Device {
    pub id: String,
    pub kind: String,
    pub name: String,
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    install_crypto();
    Ok(tokio::runtime::Builder::new_current_thread().enable_all().build()?)
}

/// rustls needs one crypto provider for the process; ring is the one compiled in.
pub fn install_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

async fn open(url: &str, bearer: &str, device: &Device) -> anyhow::Result<Ws> {
    let mut request = url.into_client_request()?;
    let headers = request.headers_mut();
    headers.insert("authorization", format!("Bearer {bearer}").parse()?);
    headers.insert("x-device-id", device.id.parse()?);
    headers.insert("x-device-kind", device.kind.parse()?);
    headers.insert("x-device-name", device.name.parse()?);
    match tokio::time::timeout(Duration::from_secs(20), tokio_tungstenite::connect_async(request)).await {
        Err(_) => bail!("the relay did not answer"),
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response))) => {
            let body = response.body().as_ref().map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
            bail!("relay: {} {}", response.status(), body.trim())
        }
        Ok(Err(error)) => Err(error).context("could not reach the relay"),
        Ok(Ok((socket, _))) => Ok(socket),
    }
}

/// Copy between a WebSocket (binary frames) and a byte stream until either side ends.
async fn splice<R, W>(ws: Ws, mut reader: R, mut writer: W) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let (mut tx, mut rx) = ws.split();
    let mut buf = vec![0u8; 64 * 1024];
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    loop {
        tokio::select! {
            message = rx.next() => match message {
                Some(Ok(Message::Binary(data))) => {
                    writer.write_all(&data).await?;
                    writer.flush().await?;
                }
                Some(Ok(Message::Close(frame))) => {
                    if let Some(frame) = frame.filter(|f| u16::from(f.code) >= 4000) {
                        bail!("{}", frame.reason);
                    }
                    break;
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(error.into()),
                None => break,
            },
            n = reader.read(&mut buf) => {
                let n = n?;
                if n == 0 {
                    break;
                }
                tx.send(Message::Binary(buf[..n].to_vec().into())).await?;
            }
            _ = ping.tick() => tx.send(Message::Ping(Vec::new().into())).await?,
        }
    }
    let _ = tx.send(Message::Close(None)).await;
    Ok(())
}

/// SSH `ProxyCommand`: join this process's stdin/stdout to `device` through the relay.
pub fn connect_stdio(relay: &str, bearer: &str, me: &Device, target: &str) -> anyhow::Result<()> {
    let runtime = runtime()?;
    let result = runtime.block_on(async {
        let ws = open(&format!("{relay}/v1/connect?to={target}&channel=ssh"), bearer, me).await?;
        splice(ws, tokio::io::stdin(), tokio::io::stdout()).await
    });
    // tokio reads stdin on a thread that cannot be cancelled; waiting for it would keep the
    // pipe open after the relay closed it (ssh would not notice the close).
    runtime.shutdown_background();
    result
}

/// What the relay tells a listening host.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Event {
    Hello {
        device: String,
        #[serde(default)]
        online: Vec<serde_json::Value>,
    },
    Presence {
        device: String,
        #[serde(default)]
        kind: String,
        online: bool,
    },
    Incoming {
        conn: String,
        from: String,
        #[serde(default)]
        from_kind: String,
        #[serde(default)]
        from_name: String,
        #[serde(default)]
        channel: String,
    },
}

/// Listen for clients until the control socket closes (the caller reconnects).
/// `token` is called for every socket (it may refresh); `route` maps an incoming connection's
/// channel to the local address it is joined to (None drops it); `on_event` sees every event.
/// It is called off the socket loop for `Incoming`, before the client is joined, so it may
/// block to get ready. A build server routes the `ssh` (or empty) channel to its sshd; the Mac
/// routes a `control` channel to the voice server.
pub fn listen(
    relay: &str,
    me: Device,
    token: Arc<dyn Fn() -> anyhow::Result<String> + Send + Sync>,
    route: Arc<dyn Fn(&str) -> Option<SocketAddr> + Send + Sync>,
    on_event: Arc<dyn Fn(&Event) + Send + Sync>,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    runtime()?.block_on(async move {
        let bearer = token()?;
        let ws = open(&format!("{relay}/v1/listen"), &bearer, &me).await?;
        let (mut tx, mut rx) = ws.split();
        let mut ping = tokio::time::interval(PING_EVERY);
        let mut check = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                message = rx.next() => match message {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(event) = serde_json::from_str::<Event>(&text) else { continue };
                        let Event::Incoming { conn, channel, .. } = &event else {
                            on_event(&event);
                            continue;
                        };
                        let Some(local) = route(channel) else {
                            log::warn!("relay connection {conn}: no route for channel “{channel}”");
                            continue;
                        };
                        let (relay, conn, me, token, on_event) = (relay.to_string(), conn.clone(), me.clone(), token.clone(), on_event.clone());
                        tokio::spawn(async move {
                            let _ = tokio::task::spawn_blocking(move || on_event(&event)).await;
                            if let Err(error) = accept(&relay, &me, &conn, token, local).await {
                                log::warn!("relay connection {conn}: {error:#}");
                            }
                        });
                    }
                    Some(Ok(Message::Close(frame))) => {
                        if let Some(frame) = frame.filter(|f| u16::from(f.code) >= 4000) {
                            bail!("{}", frame.reason);
                        }
                        return Ok(());
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(error.into()),
                    None => return Ok(()),
                },
                _ = ping.tick() => tx.send(Message::Ping(Vec::new().into())).await?,
                _ = check.tick() => if stop.load(Ordering::Relaxed) {
                    let _ = tx.send(Message::Close(None)).await;
                    return Ok(());
                },
            }
        }
    })
}

async fn accept(relay: &str, me: &Device, conn: &str, token: Arc<dyn Fn() -> anyhow::Result<String> + Send + Sync>, local: SocketAddr) -> anyhow::Result<()> {
    // The token may need a refresh, which blocks: keep it off the socket loop.
    let bearer = tokio::task::spawn_blocking(move || token()).await??;
    let ws = open(&format!("{relay}/v1/accept?conn={conn}"), &bearer, me).await?;
    let tcp = tokio::net::TcpStream::connect(local).await.with_context(|| format!("the local service at {local}"))?;
    tcp.set_nodelay(true)?;
    let (reader, writer) = tcp.into_split();
    splice(ws, reader, writer).await
}
