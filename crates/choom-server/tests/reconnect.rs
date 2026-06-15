// SPDX-License-Identifier: GPL-3.0-or-later
//! #56 reconnect test (server side): when a host drops, the relay keeps the session
//! (and its viewers) alive for a grace period; the host can `resume` with the same key
//! + reconnect id, and viewers keep streaming across the gap.

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

/// Open a host channel authenticated with `key` as `user`.
async fn host_channel(
    addr: SocketAddr,
    user: &str,
    key: Arc<PrivateKey>,
) -> (
    client::Handle<Cli>,
    Framed<russh::ChannelStream<client::Msg>, FrameCodec>,
) {
    let mut h = client::connect(Arc::new(client::Config::default()), addr, Cli)
        .await
        .unwrap();
    assert!(h
        .authenticate_publickey(user, PrivateKeyWithHashAlg::new(key, None))
        .await
        .unwrap()
        .success());
    let chan = h.channel_open_session().await.unwrap();
    let framed = Framed::new(chan.into_stream(), FrameCodec);
    (h, framed)
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

async fn viewer_read_until(chan: &mut Channel<client::Msg>, needle: &str, dur: Duration) -> bool {
    let deadline = Instant::now() + dur;
    let mut acc = String::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match timeout(remaining, chan.wait()).await {
            Ok(Some(ChannelMsg::Data { data })) => {
                acc.push_str(&String::from_utf8_lossy(&data));
                if acc.contains(needle) {
                    return true;
                }
            }
            Ok(Some(_)) => continue,
            _ => return false,
        }
    }
}

#[tokio::test]
async fn host_resumes_and_viewer_survives() {
    let key = Arc::new(PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap());
    let pubkey = key.public_key().to_openssh().unwrap();
    let addr = start_relay(vec![pubkey]).await;

    // --- host A creates the session ---
    let (host_a, mut fa) = host_channel(addr, "new", key.clone()).await;
    fa.send(Frame::Control(Control::Hello { cols: 80, rows: 24 }))
        .await
        .unwrap();
    let (ro, reconnect_id) = match next_frame(&mut fa).await {
        Some(Frame::Control(Control::Ready(r))) => (r.rw, r.id),
        other => panic!("expected Ready, got {other:?}"),
    };

    // --- a viewer attaches and sees pre-drop output ---
    let mut vh = client::connect(Arc::new(client::Config::default()), addr, Cli)
        .await
        .unwrap();
    let vkey = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    assert!(vh
        .authenticate_publickey(&ro, PrivateKeyWithHashAlg::new(Arc::new(vkey), None))
        .await
        .unwrap()
        .success());
    let mut vchan = vh.channel_open_session().await.unwrap();
    vchan
        .request_pty(true, "xterm", 80, 24, 0, 0, &[])
        .await
        .unwrap();
    vchan.request_shell(true).await.unwrap();
    fa.send(Frame::Output(b"before-drop\r\n".to_vec()))
        .await
        .unwrap();
    assert!(
        viewer_read_until(&mut vchan, "before-drop", Duration::from_secs(3)).await,
        "viewer should see pre-drop output"
    );

    // --- host A drops; the relay holds the session in its grace window ---
    drop(fa);
    drop(host_a);
    tokio::time::sleep(Duration::from_millis(300)).await;

    // --- host B resumes with the same key + reconnect id ---
    let (_host_b, mut fb) = host_channel(addr, "resume", key.clone()).await;
    fb.send(Frame::Control(Control::Resume {
        id: reconnect_id,
        cols: 80,
        rows: 24,
    }))
    .await
    .unwrap();
    match next_frame(&mut fb).await {
        Some(Frame::Control(Control::Ready(_))) => {}
        other => panic!("expected Ready on resume, got {other:?}"),
    }

    // --- the same viewer keeps streaming: it sees post-resume output ---
    fb.send(Frame::Output(b"after-resume\r\n".to_vec()))
        .await
        .unwrap();
    assert!(
        viewer_read_until(&mut vchan, "after-resume", Duration::from_secs(3)).await,
        "viewer should keep streaming across the host reconnect"
    );
}
