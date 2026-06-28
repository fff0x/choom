// SPDX-License-Identifier: GPL-3.0-or-later
//! The russh server: authentication routing (host role vs. viewer token), the host's
//! single framed channel, and viewer streaming (snapshot + live output, RW input).

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use choom_proto as proto;
use choom_proto::codec::FrameCodec;
use choom_proto::{Control, Frame, Mode, Ready, Transport};
use futures::{SinkExt, StreamExt};
use russh::keys::PublicKey;
use russh::server::{Auth, Handle, Msg, Session};
use russh::{Channel, ChannelId, ChannelStream};
use tokio::sync::mpsc;
use tokio_util::codec::Framed;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::session::{App, CreateError, SessionShared};

/// Build the `Ready` payload for a session (same tokens on create and resume).
fn make_ready(app: &App, s: &SessionShared) -> Ready {
    Ready {
        ro: s.ro_token.clone(),
        // A read-only session advertises no RW link (the token grants only read anyway).
        rw: if s.read_only {
            String::new()
        } else {
            s.rw_token.clone()
        },
        web: app
            .cfg
            .web_url
            .as_ref()
            .map(|u| format!("{u}/s/{}", s.ro_token)),
        id: s.reconnect_id.clone(),
        host: app.cfg.advertise_host.clone(),
        port: app.cfg.advertise_port,
        fp: app.host_fp.clone(),
    }
}

/// The russh `Server`: one shared `App`, a fresh [`Conn`] per connection.
#[derive(Clone)]
pub struct Relay {
    pub app: Arc<App>,
}

impl russh::server::Server for Relay {
    type Handler = Conn;

    fn new_client(&mut self, peer: Option<SocketAddr>) -> Conn {
        let peer_ip = peer.map(|a| a.ip().to_string()).unwrap_or_default();
        // Rate-limit at connection time (counts once per connection, not per auth try).
        let rate_ok = self.app.allow_connection(&peer_ip);
        if !rate_ok {
            warn!(ip = %peer_ip, "connection rate limit exceeded");
            metrics::counter!("choom_rate_limited_total").increment(1);
        }
        Conn {
            app: self.app.clone(),
            peer_ip,
            role: Role::Unknown,
            viewer: None,
            rate_ok,
            blocked: false,
        }
    }

    fn handle_session_error(&mut self, error: <Conn as russh::server::Handler>::Error) {
        debug!(%error, "session error");
    }
}

#[derive(Clone, Copy)]
enum HostKind {
    New,
    Resume,
    Query,
}

/// What a connection is, decided at authentication time.
enum Role {
    Unknown,
    Host {
        kind: HostKind,
        key: String,
    },
    Viewer {
        session: Arc<SessionShared>,
        mode: Mode,
    },
}

/// Per-viewer-connection state (a viewer has exactly one interactive channel).
struct ViewerConn {
    cols: u16,
    rows: u16,
    id: Option<u64>,
}

/// A single connection's handler.
pub struct Conn {
    app: Arc<App>,
    peer_ip: String,
    role: Role,
    viewer: Option<ViewerConn>,
    rate_ok: bool,
    /// Set once this viewer hit the input blocklist; all further input is dropped while
    /// its channel is being torn down (so the rest of the payload can't leak to the host).
    blocked: bool,
}

fn reject() -> Auth {
    Auth::Reject {
        proceed_with_methods: None,
        partial_success: false,
    }
}

/// Markers in *viewer input* that identify a client trying to bootstrap/own the remote
/// shell — unsupported through a relay, and disruptive (it injects a large payload into
/// the host shell). A match disconnects that one viewer (host + others unaffected) with
/// the paired hint. Extend this list as new offenders show up.
const VIEWER_INPUT_BLOCKLIST: &[(&[u8], &str)] = &[
    // kitty's `ssh` kitten uploads its terminfo + shell-integration as input, framed by
    // `KITTY_DATA_START` … `KITTY_DATA_END`; it assumes it drives the remote shell's
    // launch, which a relay cannot provide.
    (
        b"KITTY_DATA_START",
        "the kitty ssh kitten isn't supported here — connect with plain `ssh`",
    ),
];

/// Return the hint for the first blocklist marker present in `data`, if any.
fn blocked_viewer_input(data: &[u8]) -> Option<&'static str> {
    VIEWER_INPUT_BLOCKLIST.iter().find_map(|(marker, reason)| {
        data.windows(marker.len())
            .any(|w| w == *marker)
            .then_some(*reason)
    })
}

/// Show `reason` to a viewer and close its channel (the host + other viewers are
/// untouched). Spawned because the close is async; the caller sets `blocked` first so no
/// further input from this viewer is forwarded while the channel tears down.
fn disconnect_viewer(session: &mut Session, channel: ChannelId, reason: &str) {
    let handle = session.handle();
    let msg = format!("\r\n\x1b[33m[choom] {reason}.\x1b[0m\r\n").into_bytes();
    tokio::spawn(async move {
        let _ = handle.data(channel, Bytes::from(msg)).await;
        let _ = handle.close(channel).await;
    });
}

impl Conn {
    fn session(&self) -> Option<&Arc<SessionShared>> {
        match &self.role {
            Role::Viewer { session, .. } => Some(session),
            _ => None,
        }
    }

    /// Resolve a viewer token (the SSH username) to a session, becoming a viewer on
    /// success. Shared by `auth_none` (keyless viewers — the common path) and
    /// `auth_publickey` (a viewer whose client happened to offer a key first).
    fn accept_token(&mut self, user: &str) -> Auth {
        match self.app.lookup_token(user) {
            Some((session, mode)) => {
                self.role = Role::Viewer { session, mode };
                Auth::Accept
            }
            None => {
                warn!(ip = %self.peer_ip, "rejected unknown or expired token");
                metrics::counter!("choom_tokens_rejected_total").increment(1);
                reject()
            }
        }
    }
}

impl russh::server::Handler for Conn {
    type Error = russh::Error;

    /// Viewers carry no credential — the username *is* the unguessable token — so they
    /// authenticate with the SSH `none` method: `ssh <token>@relay`, no key, no password.
    /// (Hosts must present a key, so `none` is refused for them and the client falls
    /// through to publickey.)
    async fn auth_none(&mut self, user: &str) -> Result<Auth, Self::Error> {
        if !self.rate_ok {
            return Ok(reject());
        }
        if proto::is_host_user(user) {
            return Ok(reject());
        }
        Ok(self.accept_token(user))
    }

    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        if !self.rate_ok {
            return Ok(reject());
        }
        if proto::is_host_user(user) {
            // Host role: gate by the trusted key set (bundled default + authorized_keys).
            if !self.app.host_key_allowed(key) {
                warn!(ip = %self.peer_ip, "rejected host: key not trusted");
                metrics::counter!("choom_host_keys_rejected_total").increment(1);
                return Ok(reject());
            }
            let kind = match user {
                proto::USER_RESUME => HostKind::Resume,
                proto::USER_QUERY => HostKind::Query,
                _ => HostKind::New,
            };
            self.role = Role::Host {
                kind,
                key: key.to_openssh().unwrap_or_default(),
            };
            Ok(Auth::Accept)
        } else {
            // A viewer whose client offered a key before trying `none`; accept on a
            // valid token all the same (the key is irrelevant — the token is the secret).
            Ok(self.accept_token(user))
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        match &self.role {
            Role::Host { kind, key } => {
                let (kind, key, app) = (*kind, key.clone(), self.app.clone());
                let ip = self.peer_ip.clone();
                tokio::spawn(host_loop(app, kind, key, ip, channel.into_stream()));
                Ok(true)
            }
            Role::Viewer { .. } => {
                // Keep russh's callback model for viewers (pty/shell/data); just record
                // the channel so we can push to it from the streaming task.
                // Drop the Channel object; russh keeps delivering pty/shell/data
                // callbacks for this channel, and we push output via the session handle.
                drop(channel);
                self.viewer = Some(ViewerConn {
                    cols: 80,
                    rows: 24,
                    id: None,
                });
                Ok(true)
            }
            Role::Unknown => Ok(false),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(v) = self.viewer.as_mut() {
            v.cols = col_width.clamp(1, u16::MAX as u32) as u16;
            v.rows = row_height.clamp(1, u16::MAX as u32) as u16;
        }
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let (Some(sess), Some(v)) = (self.session().cloned(), self.viewer.as_mut()) else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        let mode = match &self.role {
            Role::Viewer { mode, .. } => *mode,
            _ => unreachable!(),
        };
        // Per-session viewer cap (public-relay hardening). The cap is enforced atomically
        // inside `add_viewer` (one lock, no TOCTOU) and shared with the web path; `None`
        // means the session is already full.
        let Some((id, snapshot, mut rx)) =
            sess.add_viewer(mode, Transport::Ssh, self.peer_ip.clone(), v.cols, v.rows)
        else {
            session.channel_success(channel)?;
            let handle = session.handle();
            tokio::spawn(async move {
                let _ = handle
                    .data(channel, Bytes::from_static(b"[choom] session is full\r\n"))
                    .await;
                let _ = handle.close(channel).await;
            });
            warn!(sid = sess.sid, ip = %self.peer_ip, "viewer rejected: session full");
            return Ok(());
        };
        v.id = Some(id);
        session.channel_success(channel)?;
        info!(sid = sess.sid, viewer = id, ip = %self.peer_ip, ?mode, "viewer joined");

        // Stream: snapshot first, then live output, until the channel or session ends.
        let handle: Handle = session.handle();
        let cancel = sess.cancel.clone();
        let sess2 = sess.clone();
        tokio::spawn(async move {
            if handle.data(channel, Bytes::from(snapshot)).await.is_err() {
                sess2.remove_viewer(id);
                return;
            }
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    msg = rx.recv() => match msg {
                        Ok(bytes) => {
                            if handle
                                .data(channel, Bytes::copy_from_slice(&bytes))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            // This viewer fell behind and the broadcast dropped output for
                            // it. Drain the remaining backlog and repaint from the current
                            // screen, so it resyncs instead of staying corrupted.
                            while let Ok(_) | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) =
                                rx.try_recv()
                            {}
                            let snap = sess2.current_snapshot();
                            if handle.data(channel, Bytes::from(snap)).await.is_err() {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
            sess2.remove_viewer(id);
            let _ = handle.close(channel).await;
        });
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let (sess, mode) = match &self.role {
            Role::Viewer { session, mode } => (session.clone(), *mode),
            _ => return Ok(()),
        };
        // Already kicked (blocklist/exec) — drop everything until the channel closes, so
        // the rest of the offending payload can't leak to the host.
        if self.blocked {
            return Ok(());
        }
        // Some "smart" ssh clients try to take over the remote shell's startup by
        // injecting a setup payload as input (it would flood/corrupt the host). A relay
        // can't host that, so disconnect such a viewer with a hint instead. Applies to RO
        // and RW alike (RO otherwise just hangs waiting for a reply that never comes).
        if let Some(reason) = blocked_viewer_input(data) {
            self.blocked = true;
            warn!(ip = %self.peer_ip, reason, "disconnecting viewer: blocked input");
            metrics::counter!("choom_viewers_blocked_total").increment(1);
            disconnect_viewer(session, channel, reason);
            return Ok(());
        }
        // Read-write viewer keystrokes -> host. Read-only viewers: dropped here (the
        // hard RO/RW boundary).
        if mode == Mode::Rw {
            sess.send_to_host(Frame::Input(data.to_vec()));
        }
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let (Some(sess), Some(v)) = (self.session().cloned(), self.viewer.as_ref()) {
            if let Some(id) = v.id {
                let cols = col_width.clamp(1, u16::MAX as u32) as u16;
                let rows = row_height.clamp(1, u16::MAX as u32) as u16;
                sess.resize_viewer(id, cols, rows);
            }
        }
        Ok(())
    }

    // RO viewers cannot break out: deny exec and subsystems for every viewer (only the
    // interactive shell view is allowed). A viewer never has a legitimate reason to run a
    // remote command, and clients like kitty's `ssh` kitten use `exec` to bootstrap the
    // remote shell — so refuse *and* disconnect with a hint, before they fall back to
    // injecting a setup payload over a shell channel.
    async fn exec_request(
        &mut self,
        channel: ChannelId,
        _data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)?;
        self.blocked = true;
        info!(ip = %self.peer_ip, "viewer attempted exec; refused and disconnected (view-only)");
        metrics::counter!("choom_viewers_blocked_total").increment(1);
        disconnect_viewer(
            session,
            channel,
            "choom is view-only — connect with plain `ssh` (no remote command)",
        );
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        _name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)?;
        Ok(())
    }

    // Deny all port/agent/X11 forwarding so the relay never becomes a pivot.
    async fn tcpip_forward(
        &mut self,
        _address: &str,
        _port: &mut u32,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        Ok(false)
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        // Viewer left (any cause): keep the roster honest.
        if let (Role::Viewer { session, .. }, Some(v)) = (&self.role, &self.viewer) {
            if let Some(id) = v.id {
                session.remove_viewer(id);
            }
        }
    }
}

/// Drive one host connection over its single framed channel.
async fn host_loop(
    app: Arc<App>,
    kind: HostKind,
    owner_key: String,
    ip: String,
    stream: ChannelStream<Msg>,
) {
    let framed = Framed::new(stream, FrameCodec);
    let (mut sink, mut source) = framed.split();
    let (to_host_tx, mut to_host_rx) = mpsc::channel::<Frame>(256);

    // Writer task: drain outbound frames (Ready/Resize/Join/Leave/Clients/Input) -> host.
    let writer = tokio::spawn(async move {
        while let Some(frame) = to_host_rx.recv().await {
            if sink.send(frame).await.is_err() {
                break;
            }
        }
    });

    // `choom clients`: one-shot roster for the session with a given reconnect id.
    if matches!(kind, HostKind::Query) {
        let id = loop {
            match source.next().await {
                Some(Ok(Frame::Control(Control::Query { id }))) => break Some(id),
                Some(Ok(_)) => continue,
                _ => break None,
            }
        };
        if let Some(id) = id {
            let reply = match app.find_by_reconnect(&id) {
                Some(s) if s.owner_key == owner_key => {
                    Frame::Control(Control::Clients(s.state.lock().unwrap().roster()))
                }
                Some(_) => Frame::Control(Control::Error {
                    msg: "not your session".into(),
                }),
                None => Frame::Control(Control::Error {
                    msg: "no such session".into(),
                }),
            };
            let _ = to_host_tx.send(reply).await;
            tokio::time::sleep(Duration::from_millis(50)).await; // let the writer flush
        }
        writer.abort();
        return;
    }

    // New / Resume: obtain the session, then run the shared relay loop. A `SessionOpts`
    // frame (name / read-only) may arrive *before* `Hello`; capture it and apply it when
    // the session is created.
    let mut pending_opts = crate::session::SessionOpts::default();
    let session = loop {
        match source.next().await {
            Some(Ok(Frame::Control(Control::SessionOpts { name, read_only })))
                if matches!(kind, HostKind::New) =>
            {
                pending_opts = crate::session::SessionOpts { name, read_only };
                continue;
            }
            Some(Ok(Frame::Control(Control::Hello { cols, rows })))
                if matches!(kind, HostKind::New) =>
            {
                match app.create_session(
                    owner_key.clone(),
                    cols,
                    rows,
                    to_host_tx.clone(),
                    pending_opts.clone(),
                ) {
                    Ok(s) => {
                        info!(sid = s.sid, ip = %ip, name = ?s.name, read_only = s.read_only, sessions = app.session_count(), "session created");
                        let _ = to_host_tx
                            .send(Frame::Control(Control::Ready(make_ready(&app, &s))))
                            .await;
                        if let Some(dir) = &app.cfg.record_dir {
                            if app.cfg.record_all {
                                match crate::recorder::start(&s, dir) {
                                    Ok(path) => {
                                        app.register_recording(&s.ro_token, &s.rw_token, path)
                                    }
                                    Err(e) => {
                                        warn!(sid = s.sid, error = %e, "failed to start recording")
                                    }
                                }
                            }
                        }
                        break Some(s);
                    }
                    Err(CreateError::TooManySessions) => {
                        let _ = to_host_tx
                            .send(Frame::Control(Control::Error {
                                msg: "relay at capacity".into(),
                            }))
                            .await;
                        break None;
                    }
                }
            }
            Some(Ok(Frame::Control(Control::Resume { id, cols, rows })))
                if matches!(kind, HostKind::Resume) =>
            {
                match app.find_by_reconnect(&id) {
                    Some(s) if s.owner_key == owner_key => {
                        // Cancel the grace timer and mark connected **under the grace lock**,
                        // so a grace timer firing at this instant (it re-checks `connected`
                        // under the same lock) can't close a session that is resuming.
                        {
                            let mut grace = s.grace.lock().unwrap();
                            if let Some(t) = grace.take() {
                                t.cancel();
                            }
                            s.connected.store(true, Ordering::Relaxed);
                        }
                        s.set_to_host(to_host_tx.clone());
                        s.set_host_size(cols, rows);
                        let _ = to_host_tx
                            .send(Frame::Control(Control::Ready(make_ready(&app, &s))))
                            .await;
                        s.repaint_viewers();
                        info!(sid = s.sid, ip = %ip, "host resumed");
                        break Some(s);
                    }
                    _ => {
                        let _ = to_host_tx
                            .send(Frame::Control(Control::Error {
                                msg: "resume rejected (unknown or expired session)".into(),
                            }))
                            .await;
                        break None;
                    }
                }
            }
            Some(Ok(_)) => continue,
            _ => break None,
        }
    };
    let Some(session) = session else {
        writer.abort();
        return;
    };

    // Shared relay loop: host output -> viewers; host control.
    let mut session_over = false;
    while let Some(item) = source.next().await {
        let frame = match item {
            Ok(f) => f,
            Err(e) => {
                debug!(%e, "host frame decode error");
                break;
            }
        };
        match frame {
            Frame::Output(bytes) => session.on_output(&bytes),
            Frame::Control(Control::HostSize { cols, rows }) => session.set_host_size(cols, rows),
            Frame::Control(Control::Record) => {
                if let Some(dir) = &app.cfg.record_dir {
                    match crate::recorder::start(&session, dir) {
                        Ok(path) => {
                            app.register_recording(&session.ro_token, &session.rw_token, path);
                            info!(sid = session.sid, "recording started (host opt-in)");
                        }
                        Err(e) => warn!(sid = session.sid, error = %e, "failed to start recording"),
                    }
                }
            }
            Frame::Control(Control::ListClients) => {
                let roster = session.state.lock().unwrap().roster();
                let _ = to_host_tx
                    .send(Frame::Control(Control::Clients(roster)))
                    .await;
            }
            // The host's shell exited: end the session now so viewers detach immediately
            // instead of hanging for the whole grace period.
            Frame::Control(Control::Bye) => {
                session_over = true;
                break;
            }
            _ => {}
        }
    }

    if session_over {
        info!(sid = session.sid, ip = %ip, "host ended the session (shell exited)");
        session.close();
        app.remove_session(&session);
        writer.abort();
        return;
    }

    // Host disconnected unexpectedly: tell viewers a reconnect is in progress, then keep
    // the session alive for a grace period (reconnect window).
    let _ = session.output_tx.send(Arc::from(
        &b"\r\n\x1b[33m[choom] host disconnected \xe2\x80\x94 waiting for reconnect\xe2\x80\xa6\x1b[0m\r\n"[..],
    ));
    session.connected.store(false, Ordering::Relaxed);
    let grace_token = CancellationToken::new();
    *session.grace.lock().unwrap() = Some(grace_token.clone());
    let (app2, s2) = (app.clone(), session.clone());
    tokio::spawn(async move {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(proto::GRACE_PERIOD_SECS)) => {
                // Re-check `connected` under the grace lock: resume sets it true while
                // holding the same lock, so this check-and-close is atomic w.r.t. a resume
                // landing at the grace boundary (no session torn down mid-resume).
                let _grace = s2.grace.lock().unwrap();
                if !s2.connected.load(Ordering::Relaxed) {
                    info!(sid = s2.sid, "grace period expired; session closed");
                    s2.close();
                    app2.remove_session(&s2);
                }
            }
            _ = grace_token.cancelled() => {} // resumed
        }
    });
    info!(sid = session.sid, ip = %ip, secs = proto::GRACE_PERIOD_SECS, "host disconnected; awaiting resume");
    writer.abort();
}
