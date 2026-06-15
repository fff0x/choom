// SPDX-License-Identifier: GPL-3.0-or-later
//! Milestone-2 spike, kept as a regression test: prove the load-bearing pattern for
//! choom 2.0's host<->relay link end to end.
//!
//! It exercises, over a real russh connection on localhost:
//!   * a russh **server** that authenticates by public key and reads the SSH
//!     **username** in `auth_publickey` (this is how host-role vs. viewer-token is
//!     distinguished at the relay),
//!   * a russh **client** (the host's side) that opens a single session channel,
//!   * the [`choom_proto::codec::FrameCodec`] running over `channel.into_stream()` in
//!     **both directions** — one typed, framed channel carrying `Frame`s, replacing
//!     1.x's two channels + newline-JSON.
//!
//! If this holds, M3 (server) and M5 (host) are de-risked: they grow from exactly
//! this skeleton.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use choom_proto::codec::FrameCodec;
use choom_proto::{Control, Frame, Ready, DEFAULT_SERVER, DEFAULT_SSH_PORT};
use futures::{SinkExt, StreamExt};
use russh::keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use russh::server::{Auth, Server as _, Session};
use russh::{client, server, Channel};
use tokio::net::TcpListener;
use tokio_util::codec::Framed;

// --- relay (server) side ------------------------------------------------------

#[derive(Clone)]
struct TestServer;

impl server::Server for TestServer {
    type Handler = TestHandler;
    fn new_client(&mut self, _peer: Option<SocketAddr>) -> TestHandler {
        TestHandler { user: None }
    }
}

struct TestHandler {
    /// The SSH username the peer authenticated as (host role or viewer token).
    user: Option<String>,
}

impl server::Handler for TestHandler {
    type Error = russh::Error;

    async fn auth_publickey(&mut self, user: &str, _key: &PublicKey) -> Result<Auth, Self::Error> {
        // Real relay will branch host-role vs. token here; the spike just records it.
        self.user = Some(user.to_string());
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<server::Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        let user = self.user.clone().unwrap_or_default();
        // Each channel is owned by its own task running the frame codec — the shape
        // the real session relay will take.
        tokio::spawn(async move {
            let mut framed = Framed::new(channel.into_stream(), FrameCodec);
            while let Some(Ok(frame)) = framed.next().await {
                if let Frame::Control(Control::Hello { cols, rows }) = frame {
                    let ready = Ready {
                        ro: format!("ro-{user}"),
                        rw: format!("rw-{user}"),
                        web: None,
                        id: "s1".into(),
                        host: DEFAULT_SERVER.into(),
                        port: DEFAULT_SSH_PORT,
                        fp: "SHA256:test".into(),
                    };
                    let _ = framed.send(Frame::Control(Control::Ready(ready))).await;
                    let _ = framed
                        .send(Frame::Control(Control::Resize { cols, rows }))
                        .await;
                }
            }
        });
        Ok(true)
    }
}

// --- host (client) side -------------------------------------------------------

struct TestClient;

impl client::Handler for TestClient {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _key: &PublicKey) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

async fn start_server() -> SocketAddr {
    let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let config = Arc::new(server::Config {
        keys: vec![host_key],
        ..Default::default()
    });
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut sh = TestServer;
        let _ = sh.run_on_socket(config, &listener).await;
    });
    addr
}

async fn recv(framed: &mut Framed<russh::ChannelStream<client::Msg>, FrameCodec>) -> Frame {
    tokio::time::timeout(Duration::from_secs(5), framed.next())
        .await
        .expect("frame did not arrive within 5s")
        .expect("channel closed unexpectedly")
        .expect("frame decode failed")
}

#[tokio::test]
async fn host_frames_roundtrip_over_single_channel() {
    let addr = start_server().await;

    let client_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let mut session = client::connect(Arc::new(client::Config::default()), addr, TestClient)
        .await
        .expect("connect");

    let auth = session
        .authenticate_publickey(
            "new",
            PrivateKeyWithHashAlg::new(Arc::new(client_key), None),
        )
        .await
        .expect("auth call");
    assert!(auth.success(), "publickey auth should succeed");

    let channel = session.channel_open_session().await.expect("open channel");
    let mut framed = Framed::new(channel.into_stream(), FrameCodec);

    // Host -> relay: announce the session.
    framed
        .send(Frame::Control(Control::Hello {
            cols: 120,
            rows: 40,
        }))
        .await
        .expect("send hello");

    // Relay -> host: Ready (carrying the username we authenticated as) then Resize,
    // both decoded intact from the one framed channel.
    match recv(&mut framed).await {
        Frame::Control(Control::Ready(r)) => {
            assert_eq!(r.ro, "ro-new", "username must reach the channel handler");
            assert_eq!(r.rw, "rw-new");
        }
        other => panic!("expected Ready, got {other:?}"),
    }
    assert_eq!(
        recv(&mut framed).await,
        Frame::Control(Control::Resize {
            cols: 120,
            rows: 40
        }),
    );
}
