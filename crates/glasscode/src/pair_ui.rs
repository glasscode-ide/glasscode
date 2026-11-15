//! "Pair iPhone": a QR code that lets the Glasscode iPhone app reach this Mac's server.
//!
//! The code carries a one-time token and the addresses this Mac answers on (Tailscale first,
//! then the local network). The phone makes its own key (kept in its Secure Enclave), posts the
//! public half here with the token, and this Mac adds it on the server — restricted to
//! `glasscoded gate` — then answers with the server's address and host keys for the phone to
//! pin. The listener closes after one pairing, or after ten minutes.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use gpui::{AnyElement, ClickEvent, ClipboardItem, Context, SharedString, div, prelude::*, px};
use glasscode_core::remote::{PhoneKey, PhonePairing, RemoteClient, ServerConfig};
use serde_json::json;

use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

const LIFETIME: Duration = Duration::from_secs(600);

pub enum PairEvent {
    Paired(String),
    Failed(String),
}

pub struct Pairing {
    pub payload: String,
    pub modules: Vec<Vec<bool>>,
    pub addresses: Vec<String>,
    pub done: Option<Result<String, String>>,
    events: mpsc::Receiver<PairEvent>,
    stop: Arc<AtomicBool>,
    started: Instant,
}

impl Drop for Pairing {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn random_token() -> String {
    let mut bytes = [0u8; 18];
    glasscode_core::sys::random_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// This Mac's addresses a phone may reach: Tailscale's first, then the local network's.
fn local_addresses() -> Vec<String> {
    let mut out = Vec::new();
    for program in ["/usr/local/bin/tailscale", "/opt/homebrew/bin/tailscale", "/Applications/Tailscale.app/Contents/MacOS/Tailscale"] {
        if let Ok(o) = glasscode_core::sys::command(program).args(["ip", "-4"]).output() {
            let ip = String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or_default().trim().to_string();
            if o.status.success() && !ip.is_empty() {
                out.push(ip);
                break;
            }
        }
    }
    if let Ok(o) = glasscode_core::sys::command("/sbin/ifconfig").output() {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("inet ") else { continue };
            let ip = rest.split_whitespace().next().unwrap_or_default();
            if ip.starts_with("127.") || ip.starts_with("169.254.") || out.iter().any(|a| a == ip) {
                continue;
            }
            out.push(ip.to_string());
        }
    }
    out
}

/// How the phone reaches the server: `ssh -G` resolves the host alias the Mac uses.
fn ssh_endpoint(config: &ServerConfig) -> (String, u16, String) {
    let mut host = config.ssh_target.rsplit('@').next().unwrap_or_default().to_string();
    let mut port = 22;
    let mut user = config.ssh_target.split_once('@').map(|(u, _)| u.to_string()).unwrap_or_default();
    if let Ok(o) = glasscode_core::sys::command("ssh").arg("-G").args(&config.ssh_args).arg(&config.ssh_target).output() {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            match line.split_once(' ') {
                Some(("hostname", v)) => host = v.to_string(),
                Some(("port", v)) => port = v.parse().unwrap_or(22),
                Some(("user", v)) => user = v.to_string(),
                _ => {}
            }
        }
    }
    (host, port, user)
}

fn respond(stream: &mut TcpStream, status: &str, body: &serde_json::Value) {
    let body = body.to_string();
    let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
}

/// One HTTP request: `POST /pair {token, name, key}`.
fn handle(mut stream: TcpStream, token: &str, client: &RemoteClient, config: &ServerConfig) -> anyhow::Result<Option<String>> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        let header = header.trim();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    if request_line.starts_with("GET ") {
        respond(&mut stream, "200 OK", &json!({ "glasscode": true }));
        return Ok(None);
    }
    if !request_line.starts_with("POST /pair ") || length == 0 || length > 16 * 1024 {
        respond(&mut stream, "404 Not Found", &json!({ "error": "not found" }));
        return Ok(None);
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    let request: serde_json::Value = serde_json::from_slice(&body).context("bad request")?;
    let given = request["token"].as_str().unwrap_or_default();
    // Compare without stopping at the first difference.
    let same = given.len() == token.len() && given.bytes().zip(token.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0;
    if !same {
        respond(&mut stream, "403 Forbidden", &json!({ "error": "This code has expired. Show a new one on the Mac." }));
        return Ok(None);
    }
    let phone = PhoneKey { name: request["name"].as_str().unwrap_or("iPhone").to_string(), key: request["key"].as_str().unwrap_or_default().to_string() };
    match client.call::<PhonePairing>("phones.add", serde_json::to_value(&phone)?) {
        Ok(pairing) => {
            let (host, port, user) = ssh_endpoint(config);
            respond(
                &mut stream,
                "200 OK",
                &json!({
                    "server": {
                        "name": config.name(),
                        "host": host,
                        "port": port,
                        "user": if user.is_empty() { pairing.user.clone() } else { user },
                        "host_keys": pairing.host_keys,
                        "bin": config.remote_bin,
                        "hostname": pairing.host,
                    }
                }),
            );
            Ok(Some(phone.name))
        }
        Err(err) => {
            respond(&mut stream, "502 Bad Gateway", &json!({ "error": format!("{err:#}") }));
            bail!("{err:#}")
        }
    }
}

impl Workspace {
    pub fn start_pairing(&mut self, cx: &mut Context<Self>) {
        let (Some(client), Some(config)) = (self.server.client.clone(), self.server.config.clone()) else {
            self.show_toast("Connect to your server first.", cx);
            return;
        };
        let listener = match TcpListener::bind("0.0.0.0:0") {
            Ok(l) => l,
            Err(err) => {
                self.show_toast(format!("Could not listen for the phone: {err}"), cx);
                return;
            }
        };
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        let _ = listener.set_nonblocking(true);
        let token = random_token();
        let mut addresses: Vec<String> = local_addresses().into_iter().map(|ip| format!("{ip}:{port}")).collect();
        if cfg!(debug_assertions) {
            // The iOS simulator runs on this Mac and may not get past the firewall otherwise.
            addresses.push(format!("127.0.0.1:{port}"));
        }
        let payload = format!("glasscode://pair?v=1&t={token}&a={}&n={}", addresses.join(","), config.name());
        let modules = match qrcode::QrCode::with_error_correction_level(payload.as_bytes(), qrcode::EcLevel::M) {
            Ok(code) => {
                let width = code.width();
                let colors = code.to_colors();
                colors.chunks(width).map(|row| row.iter().map(|c| *c == qrcode::Color::Dark).collect()).collect()
            }
            Err(err) => {
                self.show_toast(format!("Could not make the code: {err}"), cx);
                return;
            }
        };
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_listener = stop.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            while !stop_listener.load(Ordering::Relaxed) && started.elapsed() < LIFETIME {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        match handle(stream, &token, &client, &config) {
                            Ok(Some(name)) => {
                                let _ = tx.send(PairEvent::Paired(name));
                                return;
                            }
                            Ok(None) => {}
                            Err(err) => {
                                let _ = tx.send(PairEvent::Failed(format!("{err:#}")));
                            }
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(120)),
                    Err(_) => std::thread::sleep(Duration::from_millis(300)),
                }
            }
        });
        if cfg!(debug_assertions) && std::env::var_os("GLASSCODE_DEBUG_OPEN").is_some() {
            eprintln!("PAIRING LINK {payload}");
        }
        self.pairing = Some(Pairing { payload, modules, addresses, done: None, events: rx, stop, started: Instant::now() });
        self.phones_refresh(cx);
        cx.notify();
    }

    /// Every tick while the code is shown: pick up the phone's answer.
    pub fn pairing_tick(&mut self, cx: &mut Context<Self>) {
        let Some(pairing) = self.pairing.as_mut() else { return };
        let mut changed = false;
        while let Ok(event) = pairing.events.try_recv() {
            pairing.done = Some(match event {
                PairEvent::Paired(name) => Ok(name),
                PairEvent::Failed(err) => Err(err),
            });
            changed = true;
        }
        if pairing.done.is_none() && pairing.started.elapsed() > LIFETIME {
            pairing.done = Some(Err("The code expired. Show a new one.".into()));
            changed = true;
        }
        if changed {
            if let Some(Ok(name)) = pairing.done.clone() {
                self.show_toast(format!("{name} is paired with {}.", self.server.name()), cx);
                self.phones_refresh(cx);
            }
            cx.notify();
        }
    }

    pub fn phones_refresh(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.server.client.clone() else { return };
        let task = cx.background_spawn(async move { client.call::<Vec<PhoneKey>>("phones.list", json!({})) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.phones = result.ok();
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn phone_remove(&mut self, key: String, cx: &mut Context<Self>) {
        let Some(client) = self.server.client.clone() else { return };
        let task = cx.background_spawn(async move { client.call_value("phones.remove", json!({ "key": key })) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(err) = result {
                    this.show_toast(format!("Could not remove the phone: {err:#}"), cx);
                }
                this.phones_refresh(cx);
            });
        })
        .detach();
    }

    /// The "iPhone" part of the Server dialog.
    pub fn render_pairing(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut section = div().flex().flex_col().gap_2().child(ui::caps_label("IPHONE"));
        section = section.child(div().text_size(px(12.)).text_color(theme::muted()).child(
            "The Glasscode iPhone app opens these sessions from anywhere, over Tailscale. Its key can only use Glasscode on the server: no shell, no forwarding.",
        ));
        match &self.pairing {
            None => {
                section = section.child(div().flex().child(ui::primary_button("pair-iphone", "Pair iPhone").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_pairing(cx)))));
            }
            Some(pairing) => {
                let cell = 5.0;
                let quiet = 4.0 * cell;
                let rows: Vec<AnyElement> = pairing
                    .modules
                    .iter()
                    // Black on white in every theme: that is what scanners read.
                    .map(|row| div().flex().children(row.iter().map(|&dark| div().size(px(cell)).bg(if dark { theme::c(0x000000) } else { theme::c(0xffffff) }).into_any_element())).into_any_element())
                    .collect();
                let status = match &pairing.done {
                    None => ("Open Glasscode on the iPhone and scan this code. If macOS asks whether Glasscode may accept incoming connections, choose Allow.".to_string(), theme::text()),
                    Some(Ok(name)) => (format!("{name} is paired. You can close this."), theme::added()),
                    Some(Err(err)) => (err.clone(), theme::removed()),
                };
                section = section.child(
                    div()
                        .flex()
                        .gap_4()
                        .items_center()
                        .child(div().flex_none().p(px(quiet)).bg(theme::c(0xffffff)).rounded_md().child(div().flex().flex_col().children(rows)))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_color(status.1).child(status.0))
                                .child(div().text_size(px(11.5)).text_color(theme::muted()).font_family(theme::MONO_FONT).child(format!("Listening on {}", pairing.addresses.join(", "))))
                                .child(div().flex().gap_2().child(ui::button("pair-again", "New code").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_pairing(cx)))).child({
                                    let link = pairing.payload.clone();
                                    // For a phone without a camera (or the simulator): paste this link in the app.
                                    ui::button("pair-copy", "Copy link").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(link.clone()));
                                        this.show_toast("Pairing link copied. Paste it in the iPhone app.", cx);
                                    }))
                                }).child(
                                    ui::button("pair-close", "Done").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.pairing = None;
                                        cx.notify();
                                    })),
                                )),
                        ),
                );
            }
        }
        if let Some(phones) = self.phones.as_ref().filter(|p| !p.is_empty()) {
            let rows: Vec<AnyElement> = phones
                .iter()
                .enumerate()
                .map(|(i, phone)| {
                    let key = phone.key.clone();
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().flex_1().child(phone.name.clone()))
                        .child(ui::button(SharedString::from(format!("phone-remove-{i}")), "Remove").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.phone_remove(key.clone(), cx))))
                        .into_any_element()
                })
                .collect();
            section = section.child(div().flex().flex_col().gap_1p5().children(rows));
        }
        section.into_any_element()
    }
}
