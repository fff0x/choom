// SPDX-License-Identifier: GPL-3.0-or-later
//! Milestone-6/#56 test: `choom clients` (query role). The host creates a session and
//! writes its state file; a viewer attaches; the query path reads the state file,
//! connects as the `query` role, and gets the viewer roster back from the relay.

use std::sync::Arc;
use std::time::Duration;

use choom_host::{fetch_roster_for_test, run_host_for_test, HostConfig};
use choom_proto::Mode;
use choom_server::{serve_on, Config};
use russh::client;
use russh::keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use tokio::sync::mpsc;
use tokio::time::timeout;

struct Cli;
impl client::Handler for Cli {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _key: &PublicKey) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

#[tokio::test]
async fn clients_lists_viewers() {
    // Isolate the config/session-file dir for this test process.
    let tmp = std::env::temp_dir().join(format!("choom-clients-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::env::set_var("CHOOM_CONFIG_DIR", &tmp);

    // Relay trusts the bundled default key (Config::default) — the host uses it.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let relay = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = serve_on(Config::default(), listener).await;
    });

    // Host creates a session (writes session.json) using the default key.
    let (ready_tx, mut ready_rx) = mpsc::unbounded_channel();
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
    let ready = timeout(Duration::from_secs(5), ready_rx.recv())
        .await
        .expect("host ready timeout")
        .expect("ready");

    // A read-write viewer attaches.
    let mut vh = client::connect(Arc::new(client::Config::default()), relay, Cli)
        .await
        .unwrap();
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    assert!(vh
        .authenticate_publickey(&ready.rw, PrivateKeyWithHashAlg::new(Arc::new(key), None))
        .await
        .unwrap()
        .success());
    let chan = vh.channel_open_session().await.unwrap();
    chan.request_pty(true, "xterm", 80, 24, 0, 0, &[])
        .await
        .unwrap();
    chan.request_shell(true).await.unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;

    // `choom clients` reads session.json, queries, and gets the roster.
    let roster = fetch_roster_for_test(None, true)
        .await
        .expect("fetch roster");
    assert_eq!(roster.len(), 1, "expected one viewer, got {roster:?}");
    assert_eq!(roster[0].mode, Mode::Rw);

    let _ = std::fs::remove_dir_all(&tmp);
}
