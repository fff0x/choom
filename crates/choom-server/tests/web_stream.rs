// SPDX-License-Identifier: GPL-3.0-or-later
//! Milestone-4 web test (headless): a host creates a session and emits output; a
//! WebSocket client connects to `/s/{token}/stream` and must receive the asciicast
//! header followed by an output event carrying the screen content. Validates the
//! live-asciicast wire (the part testable without a browser) and that the web path is
//! output-only.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use choom_proto::codec::FrameCodec;
use choom_proto::{Control, Frame};
use choom_server::{serve_on, Config};
use futures::{SinkExt, StreamExt};
use russh::client;
use russh::keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use tokio::net::TcpListener;
use tokio::time::{timeout, Instant};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_util::codec::Framed;

struct Cli;
impl client::Handler for Cli {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _key: &PublicKey) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// Start the relay with the web view enabled; returns (ssh_addr, web_addr).
async fn start_relay_with_web(authorized_keys: Vec<String>) -> (SocketAddr, SocketAddr) {
    let ssh = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let ssh_addr = ssh.local_addr().unwrap();
    // Reserve an ephemeral port for the web view, then free it for serve_web to rebind.
    let web = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let web_addr = web.local_addr().unwrap();
    drop(web);

    let cfg = Config {
        web_addr: Some(web_addr),
        authorized_keys,
        ..Config::default()
    };
    tokio::spawn(async move {
        let _ = serve_on(cfg, ssh).await;
    });
    // Give the web listener a moment to bind.
    tokio::time::sleep(Duration::from_millis(150)).await;
    (ssh_addr, web_addr)
}

#[tokio::test]
async fn web_stream_emits_asciicast() {
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let host_pub = key.public_key().to_openssh().unwrap();
    let (ssh_addr, web_addr) = start_relay_with_web(vec![host_pub]).await;

    // --- host creates a session and emits output ---
    let mut host = client::connect(Arc::new(client::Config::default()), ssh_addr, Cli)
        .await
        .unwrap();
    assert!(host
        .authenticate_publickey("new", PrivateKeyWithHashAlg::new(Arc::new(key), None))
        .await
        .unwrap()
        .success());
    let chan = host.channel_open_session().await.unwrap();
    let mut hframed = Framed::new(chan.into_stream(), FrameCodec);
    hframed
        .send(Frame::Control(Control::Hello { cols: 80, rows: 24 }))
        .await
        .unwrap();
    let ro = match timeout(Duration::from_secs(3), hframed.next()).await {
        Ok(Some(Ok(Frame::Control(Control::Ready(r))))) => r.ro,
        other => panic!("expected Ready, got {other:?}"),
    };
    hframed
        .send(Frame::Output(b"web-marker-123\r\n".to_vec()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    // --- web client connects to the live asciicast stream ---
    let url = format!("ws://{web_addr}/s/{ro}/stream");
    let (mut ws, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("ws connect");

    // Output produced *after* the viewer joined must arrive as a live event.
    tokio::time::sleep(Duration::from_millis(100)).await;
    hframed
        .send(Frame::Output(b"more-live-output\r\n".to_vec()))
        .await
        .unwrap();

    // First message is the asciinema-player live "reset": top-level cols/rows, a numeric
    // `time` (so the player sets its clock rather than guessing the epoch), and the
    // current screen as `init` — so a joiner sees it immediately and the stream stays
    // live (not a finite recording that ends + shows a replay button).
    let reset = next_text(&mut ws).await.expect("reset");
    let rv: serde_json::Value = serde_json::from_str(&reset).expect("reset json");
    assert_eq!(rv["cols"], 80, "expected live {{cols,rows,...}} reset, got {reset}");
    assert_eq!(rv["rows"], 24);
    assert!(rv["time"].is_number(), "reset must carry a numeric stream clock");
    assert!(
        rv["init"].as_str().unwrap_or("").contains("web-marker-123"),
        "reset `init` must carry the current screen snapshot"
    );

    // Live output then arrives as [t, "o", data] events.
    let mut found = false;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let Some(text) = next_text(&mut ws).await else {
            break;
        };
        let ev: serde_json::Value = serde_json::from_str(&text).expect("event json");
        if ev.is_array() {
            assert_eq!(ev[1], "o", "events must be output events");
        }
        if ev[2].as_str().unwrap_or("").contains("more-live-output") {
            found = true;
            break;
        }
    }
    assert!(found, "live output must stream as [t,\"o\",data] events after the reset");
}

async fn next_text<S>(ws: &mut S) -> Option<String>
where
    S: StreamExt<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        match timeout(Duration::from_secs(3), ws.next()).await {
            Ok(Some(Ok(WsMessage::Text(t)))) => return Some(t.to_string()),
            Ok(Some(Ok(_))) => continue,
            _ => return None,
        }
    }
}
