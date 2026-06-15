// SPDX-License-Identifier: GPL-3.0-or-later
//! Milestone-5 end-to-end test (real host + real relay + a russh viewer):
//!   * the host spawns a PTY shell (`cat`, which echoes its input),
//!   * connects to the relay and creates a session (we capture the tokens),
//!   * a read-write viewer attaches and types — the bytes reach the PTY shell,
//!     are echoed by the terminal, and stream back to the viewer.
//!
//! This exercises the full byte path host PTY <-> relay <-> viewer in both directions.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use choom_host::{run_host_for_test, HostConfig};
use choom_server::{serve_on, Config};
use russh::client;
use russh::keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use russh::ChannelMsg;
use tokio::sync::mpsc;
use tokio::time::{timeout, Instant};

struct Cli;
impl client::Handler for Cli {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _key: &PublicKey) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

async fn start_relay() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = serve_on(Config::default(), listener).await;
    });
    addr
}

#[tokio::test]
async fn host_relay_viewer_roundtrip() {
    let relay = start_relay().await;

    // --- start the host against the relay, shell = `cat` (echoes input) ---
    let (ready_tx, mut ready_rx) = mpsc::unbounded_channel();
    // Uses the bundled default key; the relay trusts it by default — exercises the
    // real default-key auth path end to end.
    let cfg = HostConfig {
        server: relay.ip().to_string(),
        port: relay.port(),
        shell: Some("cat".to_string()),
        record: false,
        identity: None,
        use_default_key: true,
        fingerprint: None,
        detach: false,
    };
    tokio::spawn(async move {
        let _ = run_host_for_test(cfg, ready_tx).await;
    });

    // Learn the share tokens from the host's Ready.
    let ready = timeout(Duration::from_secs(5), ready_rx.recv())
        .await
        .expect("host did not become ready")
        .expect("ready channel closed");

    // --- read-write viewer attaches ---
    let mut vh = client::connect(Arc::new(client::Config::default()), relay, Cli)
        .await
        .unwrap();
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let ok = vh
        .authenticate_publickey(&ready.rw, PrivateKeyWithHashAlg::new(Arc::new(key), None))
        .await
        .unwrap();
    assert!(ok.success(), "rw token auth should succeed");

    let mut chan = vh.channel_open_session().await.unwrap();
    chan.request_pty(true, "xterm", 80, 24, 0, 0, &[])
        .await
        .unwrap();
    chan.request_shell(true).await.unwrap();

    // Give the viewer's join a moment, then type — `cat` echoes it back through the
    // PTY, so it must round-trip to the viewer.
    tokio::time::sleep(Duration::from_millis(200)).await;
    chan.data(&b"roundtrip-marker\n"[..]).await.unwrap();

    let mut seen = String::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match timeout(remaining, chan.wait()).await {
            Ok(Some(ChannelMsg::Data { data })) => {
                seen.push_str(&String::from_utf8_lossy(&data));
                if seen.contains("roundtrip-marker") {
                    break;
                }
            }
            Ok(Some(_)) => continue,
            _ => break,
        }
    }
    assert!(
        seen.contains("roundtrip-marker"),
        "viewer must see its keystrokes echoed by the shell; got {seen:?}"
    );
}
