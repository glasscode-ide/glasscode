//! The relay end to end: a host listening, a client connecting, bytes both ways, and refusals.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite::{self, Message, client::IntoClientRequest};

const SECRET: &str = "a-long-enough-test-secret";

async fn relay() -> String {
    let board = glasscode_relay::switchboard::Switchboard::new(SECRET.to_string());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = glasscode_relay::router(board);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("127.0.0.1:{}", addr.port())
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Open a WebSocket, presenting a secret and a device identity.
async fn open(relay: &str, path: &str, secret: &str, device: &str, kind: &str) -> Result<Socket, tungstenite::Error> {
    let mut request = format!("ws://{relay}{path}").into_client_request().unwrap();
    let h = request.headers_mut();
    h.insert("authorization", format!("Bearer {secret}").parse().unwrap());
    h.insert("x-device-id", device.parse().unwrap());
    h.insert("x-device-kind", kind.parse().unwrap());
    h.insert("x-device-name", device.parse().unwrap());
    tokio_tungstenite::connect_async(request).await.map(|(socket, _)| socket)
}

fn status_of(error: tungstenite::Error) -> u16 {
    match error {
        tungstenite::Error::Http(response) => response.status().as_u16(),
        other => panic!("expected an HTTP refusal, got {other:?}"),
    }
}

async fn next_text(socket: &mut Socket) -> Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), socket.next()).await.expect("a message").expect("open").unwrap() {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            _ => continue,
        }
    }
}

async fn next_binary(socket: &mut Socket) -> Vec<u8> {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), socket.next()).await.expect("a message").expect("open").unwrap() {
            Message::Binary(data) => return data.to_vec(),
            Message::Close(_) => panic!("closed"),
            _ => continue,
        }
    }
}

async fn reqwest_get(url: &str, secret: &str, device: &str) -> Value {
    reqwest::Client::new()
        .get(url)
        .header("authorization", format!("Bearer {secret}"))
        .header("x-device-id", device)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn joins_devices_over_a_shared_secret() {
    let relay = relay().await;

    // Refusals before any socket opens.
    assert_eq!(status_of(open(&relay, "/v1/listen", "wrong-secret", "mac-1", "mac").await.unwrap_err()), 401, "a wrong secret");
    // A missing device id: the secret is right, but there is nothing to name the socket.
    let mut no_device = format!("ws://{relay}/v1/listen").into_client_request().unwrap();
    no_device.headers_mut().insert("authorization", format!("Bearer {SECRET}").parse().unwrap());
    assert_eq!(status_of(tokio_tungstenite::connect_async(no_device).await.unwrap_err()), 400, "no device id");

    // The Mac listens.
    let mut host = open(&relay, "/v1/listen", SECRET, "mac-1", "mac").await.unwrap();
    assert_eq!(next_text(&mut host).await["type"], "hello");

    // The phone may not reach a device that is offline or unknown.
    assert_eq!(status_of(open(&relay, "/v1/connect?to=nowhere", SECRET, "phone-1", "phone").await.unwrap_err()), 404);

    // The phone connects; the Mac is told and picks up; bytes go both ways.
    let client_task = {
        let relay = relay.clone();
        tokio::spawn(async move { open(&relay, "/v1/connect?to=mac-1&channel=control", SECRET, "phone-1", "phone").await.unwrap() })
    };
    let incoming = next_text(&mut host).await;
    assert_eq!(incoming["type"], "incoming");
    assert_eq!(incoming["from"], "phone-1");
    assert_eq!(incoming["from_kind"], "phone");
    assert_eq!(incoming["channel"], "control");
    let conn = incoming["conn"].as_str().unwrap().to_string();

    // Only the target device may pick up the connection.
    assert_eq!(status_of(open(&relay, &format!("/v1/accept?conn={conn}"), SECRET, "phone-1", "phone").await.unwrap_err()), 403, "another device may not accept");

    let mut accepted = open(&relay, &format!("/v1/accept?conn={conn}"), SECRET, "mac-1", "mac").await.unwrap();
    let mut client = client_task.await.unwrap();
    client.send(Message::Binary(b"hello-from-phone".to_vec().into())).await.unwrap();
    assert_eq!(next_binary(&mut accepted).await, b"hello-from-phone");
    accepted.send(Message::Binary(b"hello-from-mac".to_vec().into())).await.unwrap();
    assert_eq!(next_binary(&mut client).await, b"hello-from-mac");
    let big = vec![7u8; 256 * 1024];
    client.send(Message::Binary(big.clone().into())).await.unwrap();
    assert_eq!(next_binary(&mut accepted).await, big, "large frames pass whole");

    // Who is online, for the phone's device list.
    let online: Value = reqwest_get(&format!("http://{relay}/v1/online"), SECRET, "phone-1").await;
    assert_eq!(online["online"].as_array().unwrap().len(), 1, "the Mac is online");
    assert_eq!(online["online"][0]["device"], "mac-1");

    // A wrong secret is refused on the HTTP endpoints too.
    let refused = reqwest::Client::new().get(format!("http://{relay}/v1/online")).header("authorization", "Bearer nope").header("x-device-id", "phone-1").send().await.unwrap();
    assert_eq!(refused.status(), 401);
}
