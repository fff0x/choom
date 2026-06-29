// SPDX-License-Identifier: GPL-3.0-or-later
//! Milestone-3 integration test (Rust<->Rust): drives the real relay with a test host
//! and stock-ssh-style viewers (russh clients doing pty+shell), validating the core
//! product contract:
//!   * host authenticates as `new`, creates a session, gets share tokens (`Ready`),
//!   * a viewer joining receives the **snapshot** of prior output, then **live** output,
//!   * a **read-write** viewer's keystrokes reach the host,
//!   * a **read-only** viewer's keystrokes are dropped,
//!   * an **unknown token** fails authentication.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use choom_proto::codec::FrameCodec;
use choom_proto::{Control, Frame};
use choom_server::{serve_on, Config};
use futures::{SinkExt, StreamExt};
use russh::client;
use russh::keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use russh::{Channel, ChannelMsg};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::time::{timeout, Instant};
use tokio_util::codec::Framed;

struct Cli;

impl client::Handler for Cli {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _key: &PublicKey) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

async fn start_relay(authorized_keys: Vec<String>) -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cfg = Config {
        authorized_keys,
        ..Config::default()
    };
    tokio::spawn(async move {
        let _ = serve_on(cfg, listener).await;
    });
    addr
}

/// Connect and authenticate as a host with a specific (trusted) key.
async fn connect_host(addr: SocketAddr, key: PrivateKey) -> client::Handle<Cli> {
    let mut h = client::connect(Arc::new(client::Config::default()), addr, Cli)
        .await
        .expect("connect");
    let ok = h
        .authenticate_publickey("new", PrivateKeyWithHashAlg::new(Arc::new(key), None))
        .await
        .expect("auth call");
    assert!(ok.success(), "host auth should succeed");
    h
}

/// Connect and authenticate as `user`; returns the session handle and whether auth
/// succeeded.
async fn auth(addr: SocketAddr, user: &str) -> (client::Handle<Cli>, bool) {
    let mut h = client::connect(Arc::new(client::Config::default()), addr, Cli)
        .await
        .expect("connect");
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let res = h
        .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), None))
        .await
        .expect("auth call");
    (h, res.success())
}

async fn next_frame<S>(framed: &mut Framed<S, FrameCodec>) -> Option<Frame>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    timeout(Duration::from_secs(3), framed.next())
        .await
        .ok()
        .flatten()
        .and_then(|r| r.ok())
}

/// Read host-bound frames until an `Input` arrives (skipping control frames), or the
/// timeout elapses (returns `None` — used to assert *no* input from RO viewers).
async fn host_wait_input<S>(framed: &mut Framed<S, FrameCodec>, dur: Duration) -> Option<Vec<u8>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let deadline = Instant::now() + dur;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match timeout(remaining, framed.next()).await {
            Ok(Some(Ok(Frame::Input(b)))) => return Some(b),
            Ok(Some(Ok(_))) => continue,
            _ => return None,
        }
    }
}

/// Open a viewer channel, request a pty + shell, and read until `needle` is seen.
async fn viewer_open_and_read(
    h: &client::Handle<Cli>,
    needle: &str,
) -> (Channel<client::Msg>, String) {
    let mut channel = h.channel_open_session().await.expect("open viewer channel");
    channel
        .request_pty(true, "xterm", 80, 24, 0, 0, &[])
        .await
        .unwrap();
    channel.request_shell(true).await.unwrap();

    let mut acc = String::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match timeout(remaining, channel.wait()).await {
            Ok(Some(ChannelMsg::Data { data })) => {
                acc.push_str(&String::from_utf8_lossy(&data));
                if acc.contains(needle) {
                    break;
                }
            }
            Ok(Some(_)) => continue,
            _ => break,
        }
    }
    (channel, acc)
}

#[tokio::test]
async fn relay_core_flow() {
    // The host's key must be trusted by the relay (gating in auth_publickey).
    let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let host_pub = host_key.public_key().to_openssh().unwrap();
    let addr = start_relay(vec![host_pub]).await;

    // --- host creates a session ---
    let host = connect_host(addr, host_key).await;
    let host_chan = host.channel_open_session().await.unwrap();
    let mut hframed = Framed::new(host_chan.into_stream(), FrameCodec);
    hframed
        .send(Frame::Control(Control::Hello { cols: 80, rows: 24 }))
        .await
        .unwrap();

    let (ro, rw) = match next_frame(&mut hframed).await {
        Some(Frame::Control(Control::Ready(r))) => (r.ro, r.rw),
        other => panic!("expected Ready, got {other:?}"),
    };
    assert_ne!(ro, rw);

    // host emits some screen content before any viewer joins
    hframed
        .send(Frame::Output(b"hello-screen\r\n".to_vec()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // --- read-write viewer sees the snapshot, then types ---
    let (rw_view, screen) = {
        let (h, ok) = auth(addr, &rw).await;
        assert!(ok, "rw token auth should succeed");
        let (chan, screen) = viewer_open_and_read(&h, "hello-screen").await;
        // keep the handle alive for the duration of the test
        std::mem::forget(h);
        (chan, screen)
    };
    assert!(
        screen.contains("hello-screen"),
        "viewer must receive the snapshot; got {screen:?}"
    );

    rw_view.data(&b"TYPED"[..]).await.unwrap();
    let got = host_wait_input(&mut hframed, Duration::from_secs(2)).await;
    assert_eq!(
        got.as_deref(),
        Some(&b"TYPED"[..]),
        "rw viewer keystrokes must reach the host"
    );

    // --- read-only viewer's keystrokes are dropped ---
    let (ro_view, _) = {
        let (h, ok) = auth(addr, &ro).await;
        assert!(ok, "ro token auth should succeed");
        let (chan, screen) = viewer_open_and_read(&h, "hello-screen").await;
        std::mem::forget(h);
        (chan, screen)
    };
    ro_view.data(&b"NOPE"[..]).await.unwrap();
    let leaked = host_wait_input(&mut hframed, Duration::from_millis(600)).await;
    assert_eq!(leaked, None, "ro viewer input must never reach the host");

    // --- unknown token is rejected ---
    let (_, ok) = auth(addr, "definitely-not-a-real-token").await;
    assert!(!ok, "unknown token must fail authentication");
}

/// Stock `ssh <token>@relay` offers no key and no password — it authenticates with the
/// SSH `none` method, the username carrying the token. This is the exact path a real
/// viewer uses, so it must succeed for a valid token and fail for a bogus one (a
/// regression here makes `ssh` fall through to a misleading password prompt).
#[tokio::test]
async fn viewer_authenticates_with_none() {
    let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let host_pub = host_key.public_key().to_openssh().unwrap();
    let addr = start_relay(vec![host_pub]).await;

    // Host creates a session and we learn a real viewer token.
    let host = connect_host(addr, host_key).await;
    let host_chan = host.channel_open_session().await.unwrap();
    let mut hframed = Framed::new(host_chan.into_stream(), FrameCodec);
    hframed
        .send(Frame::Control(Control::Hello { cols: 80, rows: 24 }))
        .await
        .unwrap();
    let rw = match next_frame(&mut hframed).await {
        Some(Frame::Control(Control::Ready(r))) => r.rw,
        other => panic!("expected Ready, got {other:?}"),
    };

    // A valid token authenticates with `none` (no key offered) — the stock-ssh path.
    let mut h = client::connect(Arc::new(client::Config::default()), addr, Cli)
        .await
        .unwrap();
    let res = h.authenticate_none(rw).await.unwrap();
    assert!(res.success(), "valid token must authenticate via `none`");

    // A bogus token fails `none` too (must not fall through to another method).
    let mut bad = client::connect(Arc::new(client::Config::default()), addr, Cli)
        .await
        .unwrap();
    let res = bad.authenticate_none("not-a-real-token").await.unwrap();
    assert!(
        !res.success(),
        "unknown token must fail `none` authentication"
    );
}

/// When the host's shell exits it sends `Control::Bye`; the relay must then detach
/// viewers immediately (close their channels) rather than holding them for the whole
/// reconnect grace period — otherwise a viewer's `ssh` hangs after the host quits.
#[tokio::test]
async fn host_bye_detaches_viewers_immediately() {
    let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let addr = start_relay(vec![host_key.public_key().to_openssh().unwrap()]).await;

    let host = connect_host(addr, host_key).await;
    let host_chan = host.channel_open_session().await.unwrap();
    let mut hframed = Framed::new(host_chan.into_stream(), FrameCodec);
    hframed
        .send(Frame::Control(Control::Hello { cols: 80, rows: 24 }))
        .await
        .unwrap();
    let rw = match next_frame(&mut hframed).await {
        Some(Frame::Control(Control::Ready(r))) => r.rw,
        other => panic!("expected Ready, got {other:?}"),
    };
    hframed
        .send(Frame::Output(b"hello-screen\r\n".to_vec()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // A viewer attaches and reads the snapshot.
    let (vh, ok) = auth(addr, &rw).await;
    assert!(ok);
    let (mut chan, _) = viewer_open_and_read(&vh, "hello-screen").await;

    // The host's shell exits.
    hframed.send(Frame::Control(Control::Bye)).await.unwrap();

    // The viewer's channel must close promptly (well under the 60s grace period).
    let closed = timeout(Duration::from_secs(5), async {
        loop {
            match chan.wait().await {
                Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => break true,
                Some(_) => continue,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(closed, "viewer channel must close after host Bye");
}

/// A viewer is view-only: an `exec` request (a remote command, e.g. kitty's ssh kitten
/// bootstrapping the shell) must be refused and the channel closed — not left open to
/// fall back to injecting a payload.
#[tokio::test]
async fn viewer_exec_is_refused_and_disconnected() {
    let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let addr = start_relay(vec![host_key.public_key().to_openssh().unwrap()]).await;
    let host = connect_host(addr, host_key).await;
    let host_chan = host.channel_open_session().await.unwrap();
    let mut hframed = Framed::new(host_chan.into_stream(), FrameCodec);
    hframed
        .send(Frame::Control(Control::Hello { cols: 80, rows: 24 }))
        .await
        .unwrap();
    let rw = match next_frame(&mut hframed).await {
        Some(Frame::Control(Control::Ready(r))) => r.rw,
        other => panic!("expected Ready, got {other:?}"),
    };

    let (vh, ok) = auth(addr, &rw).await;
    assert!(ok);
    let mut chan = vh.channel_open_session().await.unwrap();
    let _ = chan.exec(true, &b"id"[..]).await;

    let closed = timeout(Duration::from_secs(5), async {
        loop {
            match chan.wait().await {
                Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => break true,
                Some(_) => continue,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(closed, "exec must be refused and the viewer disconnected");
}

/// A viewer that injects a blocklisted client-bootstrap marker (kitty's `KITTY_DATA_START`)
/// as input is disconnected, and the marker never reaches the host.
#[tokio::test]
async fn blocklisted_input_disconnects_viewer() {
    let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let addr = start_relay(vec![host_key.public_key().to_openssh().unwrap()]).await;
    let host = connect_host(addr, host_key).await;
    let host_chan = host.channel_open_session().await.unwrap();
    let mut hframed = Framed::new(host_chan.into_stream(), FrameCodec);
    hframed
        .send(Frame::Control(Control::Hello { cols: 80, rows: 24 }))
        .await
        .unwrap();
    let rw = match next_frame(&mut hframed).await {
        Some(Frame::Control(Control::Ready(r))) => r.rw,
        other => panic!("expected Ready, got {other:?}"),
    };

    let (vh, ok) = auth(addr, &rw).await;
    assert!(ok);
    let mut chan = vh.channel_open_session().await.unwrap();
    chan.request_pty(true, "xterm", 80, 24, 0, 0, &[])
        .await
        .unwrap();
    chan.request_shell(true).await.unwrap();
    // The kitty kitten's terminfo upload starts with this marker.
    chan.data(&b"\nKITTY_DATA_START\nOK\nsomepayload\n"[..])
        .await
        .unwrap();

    // The marker must NOT reach the host as input…
    let leaked = host_wait_input(&mut hframed, Duration::from_millis(800)).await;
    assert_eq!(leaked, None, "blocklisted input must never reach the host");

    // …and the viewer's channel must be closed.
    let closed = timeout(Duration::from_secs(5), async {
        loop {
            match chan.wait().await {
                Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => break true,
                Some(_) => continue,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(closed, "blocklisted viewer must be disconnected");
}

#[tokio::test]
async fn untrusted_host_key_is_rejected() {
    let trusted = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let addr = start_relay(vec![trusted.public_key().to_openssh().unwrap()]).await;

    // A different (untrusted) key must fail host authentication.
    let other = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let mut h = client::connect(Arc::new(client::Config::default()), addr, Cli)
        .await
        .unwrap();
    let res = h
        .authenticate_publickey("new", PrivateKeyWithHashAlg::new(Arc::new(other), None))
        .await
        .unwrap();
    assert!(!res.success(), "untrusted host key must be rejected");
}
