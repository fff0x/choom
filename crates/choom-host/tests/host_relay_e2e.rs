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
    start_relay_handle().await.0
}

/// Start a relay and also return its task handle, so a test can kill the relay mid-session.
async fn start_relay_handle() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = serve_on(Config::default(), listener).await;
    });
    (addr, handle)
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
        verbose: false,
        name: None,
        read_only: false,
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

/// If the shell exits while the relay is unreachable, the host must stop — not loop
/// "relay connection lost" forever. (Regression: a shell that exited concurrently with a
/// relay redeploy left the host reconnecting endlessly because the reconnect path never
/// checked whether the local shell was still alive.)
#[tokio::test]
async fn host_stops_when_shell_exits_during_reconnect() {
    let (relay, relay_task) = start_relay_handle().await;

    // The `shell` is exec'd directly (no arg parsing), so use a tiny script that just
    // sleeps and exits on its own shortly after the session is established.
    let dir = std::env::temp_dir().join(format!("choom-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("self-exit-shell.sh");
    std::fs::write(&script, "#!/bin/sh\nsleep 1\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let (ready_tx, mut ready_rx) = mpsc::unbounded_channel();
    let cfg = HostConfig {
        server: relay.ip().to_string(),
        port: relay.port(),
        shell: Some(script.to_str().unwrap().to_string()),
        record: false,
        identity: None,
        use_default_key: true,
        fingerprint: None,
        detach: false,
        verbose: false,
        name: None,
        read_only: false,
    };
    let host = tokio::spawn(async move { run_host_for_test(cfg, ready_tx).await });

    // Wait until the session exists (reconnect id set), then kill the relay so the host
    // drops into its reconnect loop while the shell is still running.
    timeout(Duration::from_secs(5), ready_rx.recv())
        .await
        .expect("host did not become ready")
        .expect("ready channel closed");
    relay_task.abort();

    // The shell exits ~1s later; the host must then return promptly instead of
    // reconnecting forever. Generous bound so slow CI still distinguishes "returns" from
    // "loops forever".
    let result = timeout(Duration::from_secs(10), host).await;
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        result.is_ok(),
        "host kept reconnecting after the shell exited (infinite-loop regression)"
    );
}
