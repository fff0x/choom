// SPDX-License-Identifier: GPL-3.0-or-later
//! Session registry and per-session shared state: the `avt` screen model, the viewer
//! roster, live-output fan-out, the channel back to the host, and resize negotiation.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use choom_proto as proto;
use choom_proto::{Client, Frame, Mode, Transport};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

/// Relay configuration (the subset M3 needs; CLI/YAML wiring lands in M6).
#[derive(Clone, Debug)]
pub struct Config {
    /// Address the SSH relay listens on.
    pub ssh_addr: SocketAddr,
    /// Hostname advertised in the share links.
    pub advertise_host: String,
    /// SSH port advertised in the share links.
    pub advertise_port: u16,
    /// Public web URL, if the web view is exposed (advertised to the host).
    pub web_url: Option<String>,
    /// Local address to serve the web view on; `None` disables it.
    pub web_addr: Option<SocketAddr>,
    /// Global cap on concurrent sessions.
    pub max_sessions: usize,
    /// Per-session viewer cap.
    pub max_viewers: usize,
    /// New connections allowed per source IP per minute (0 = unlimited).
    pub rate_per_min: u32,
    /// Directory to store recordings in; `None` disables recording storage.
    pub record_dir: Option<std::path::PathBuf>,
    /// Record every session, even those whose host did not opt in.
    pub record_all: bool,
    /// Prune recordings older than this many days (0 = no age limit).
    pub record_max_age_days: u64,
    /// Cap total recordings size in MB, oldest first (0 = no size limit).
    pub record_max_mb: u64,
    /// OpenSSH public-key lines allowed to host (in addition to the default key).
    pub authorized_keys: Vec<String>,
    /// Trust the bundled default key (public-relay convenience; opt out for private).
    pub use_default_key: bool,
    /// Directory of prebuilt binaries served at `/download`; `None` disables it.
    pub downloads_dir: Option<std::path::PathBuf>,
    /// Path to a persistent SSH host key (loaded if present, else generated + saved at
    /// `0600`). `None` generates an ephemeral key each start (the fingerprint changes).
    pub host_key_path: Option<std::path::PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ssh_addr: SocketAddr::from(([0, 0, 0, 0], proto::DEFAULT_SSH_PORT)),
            advertise_host: proto::DEFAULT_SERVER.to_string(),
            advertise_port: proto::DEFAULT_SSH_PORT,
            web_url: None,
            web_addr: None,
            max_sessions: proto::DEFAULT_MAX_SESSIONS,
            max_viewers: proto::DEFAULT_MAX_VIEWERS,
            rate_per_min: proto::DEFAULT_RATE_PER_MIN,
            record_dir: None,
            record_all: false,
            record_max_age_days: proto::DEFAULT_RECORD_MAX_AGE_DAYS,
            record_max_mb: proto::DEFAULT_RECORD_MAX_MB,
            authorized_keys: Vec::new(),
            use_default_key: true,
            downloads_dir: None,
            host_key_path: None,
        }
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One viewer in the roster.
#[derive(Clone, Debug)]
pub struct Viewer {
    pub id: u64,
    pub mode: Mode,
    pub transport: Transport,
    pub ip: String,
    pub cols: u16,
    pub rows: u16,
    pub connected_at: u64,
}

impl Viewer {
    fn to_client(&self) -> Client {
        Client {
            id: self.id,
            mode: self.mode,
            transport: self.transport,
            ip: self.ip.clone(),
            cols: self.cols,
            rows: self.rows,
            connected_at: self.connected_at,
        }
    }
}

/// Mutable per-session state behind a mutex.
pub struct SessionState {
    /// Server-side screen model (snapshot + repaint source).
    pub vt: avt::Vt,
    /// Host's own terminal size.
    pub host_cols: u16,
    pub host_rows: u16,
    /// Negotiated size currently applied to the host PTY.
    pub neg_cols: u16,
    pub neg_rows: u16,
    viewers: HashMap<u64, Viewer>,
    next_viewer_id: u64,
}

impl SessionState {
    /// Recompute the negotiated size: the smallest of the host and every RW viewer
    /// (RO/web viewers never shrink the host — no resize DoS), clamped to the floor.
    /// With no RW viewers the host keeps its own native size.
    fn negotiate(&self) -> (u16, u16) {
        let mut cols = self.host_cols;
        let mut rows = self.host_rows;
        for v in self.viewers.values() {
            if v.mode == Mode::Rw {
                cols = cols.min(v.cols);
                rows = rows.min(v.rows);
            }
        }
        proto::clamp_size(cols, rows)
    }

    /// Current roster as protocol `Client`s.
    pub fn roster(&self) -> Vec<Client> {
        let mut list: Vec<Client> = self.viewers.values().map(Viewer::to_client).collect();
        list.sort_by_key(|c| c.id);
        list
    }
}

/// Shared, cloneable handle to a session.
pub struct SessionShared {
    /// Short numeric id for logs (not a secret).
    pub sid: u64,
    pub ro_token: String,
    pub rw_token: String,
    pub reconnect_id: String,
    /// Owner public key (OpenSSH form) — binds reconnect/query to the host.
    pub owner_key: String,
    /// Outbound frames to the host (viewer input + control). Swapped on reconnect so
    /// input flows to the *current* host connection.
    to_host: Mutex<mpsc::Sender<Frame>>,
    /// Live raw output fan-out to viewers.
    pub output_tx: broadcast::Sender<Arc<[u8]>>,
    /// Cancelled when the session ends (host gone) so viewer tasks detach.
    pub cancel: CancellationToken,
    /// Whether a host is currently attached (false during a reconnect grace window).
    pub connected: AtomicBool,
    /// The pending grace-period timer, if the host is currently disconnected.
    pub grace: Mutex<Option<CancellationToken>>,
    /// Path of this session's recording, once recording has started.
    pub record_path: Mutex<Option<std::path::PathBuf>>,
    pub state: Mutex<SessionState>,
}

impl SessionShared {
    /// End the session: signal viewer tasks to detach.
    pub fn close(&self) {
        self.cancel.cancel();
    }

    /// Send a frame to the currently-attached host (no-op if the channel is full/gone).
    pub fn send_to_host(&self, frame: Frame) {
        let _ = self.to_host.lock().unwrap().try_send(frame);
    }

    /// Point host-bound frames at a new connection (on reconnect/resume).
    pub fn set_to_host(&self, sender: mpsc::Sender<Frame>) {
        *self.to_host.lock().unwrap() = sender;
    }

    /// Re-broadcast the current screen to viewers (used after a host resumes).
    pub fn repaint_viewers(&self) {
        let snapshot = {
            let st = self.state.lock().unwrap();
            crate::render::snapshot(&st.vt)
        };
        let _ = self.output_tx.send(Arc::from(snapshot.as_slice()));
    }

    /// Current number of attached viewers (for the per-session cap).
    pub fn viewer_count(&self) -> usize {
        self.state.lock().unwrap().viewers.len()
    }

    /// Feed host output into the screen model and fan it out to viewers (raw bytes).
    /// The avt update and the broadcast happen under one lock so a viewer joining
    /// concurrently sees each output chunk exactly once (snapshot xor live).
    pub fn on_output(&self, bytes: &[u8]) {
        let mut st = self.state.lock().unwrap();
        // avt models UTF-8 text; the live path stays byte-exact.
        let chunk = String::from_utf8_lossy(bytes);
        st.vt.feed_str(&chunk);
        let _ = self.output_tx.send(Arc::from(bytes));
        metrics::counter!("choom_output_bytes_total").increment(bytes.len() as u64);
    }

    /// Add a viewer to the roster. Returns its id, a snapshot of the current screen,
    /// and a live-output receiver — subscribed under the same lock as the snapshot so
    /// no output is missed or duplicated. Renegotiates the size afterwards.
    pub fn add_viewer(
        &self,
        mode: Mode,
        transport: Transport,
        ip: String,
        cols: u16,
        rows: u16,
    ) -> (u64, Vec<u8>, broadcast::Receiver<Arc<[u8]>>) {
        let (id, snapshot, rx, client, resize) = {
            let mut st = self.state.lock().unwrap();
            let id = st.next_viewer_id;
            st.next_viewer_id += 1;
            let viewer = Viewer {
                id,
                mode,
                transport,
                ip,
                cols,
                rows,
                connected_at: now_unix(),
            };
            let client = viewer.to_client();
            st.viewers.insert(id, viewer);
            let snapshot = crate::render::snapshot(&st.vt);
            let rx = self.output_tx.subscribe();
            let resize = self.apply_negotiation(&mut st);
            (id, snapshot, rx, client, resize)
        };
        self.send_to_host(Frame::Control(proto::Control::Join(client)));
        if let Some((c, r)) = resize {
            self.push_resize(c, r);
        }
        metrics::counter!("choom_viewers_joined_total").increment(1);
        metrics::gauge!("choom_viewers_active").increment(1.0);
        (id, snapshot, rx)
    }

    /// Remove a viewer; renegotiates (a departing RW viewer may grow the host again).
    pub fn remove_viewer(&self, id: u64) {
        let resize = {
            let mut st = self.state.lock().unwrap();
            if st.viewers.remove(&id).is_none() {
                return;
            }
            self.apply_negotiation(&mut st)
        };
        metrics::gauge!("choom_viewers_active").decrement(1.0);
        self.send_to_host(Frame::Control(proto::Control::Leave { id }));
        if let Some((c, r)) = resize {
            self.push_resize(c, r);
        }
    }

    /// Update a viewer's terminal size and renegotiate.
    pub fn resize_viewer(&self, id: u64, cols: u16, rows: u16) {
        let resize = {
            let mut st = self.state.lock().unwrap();
            if let Some(v) = st.viewers.get_mut(&id) {
                v.cols = cols;
                v.rows = rows;
            }
            self.apply_negotiation(&mut st)
        };
        if let Some((c, r)) = resize {
            self.push_resize(c, r);
        }
    }

    /// Update the host's own terminal size and renegotiate.
    pub fn set_host_size(&self, cols: u16, rows: u16) {
        let resize = {
            let mut st = self.state.lock().unwrap();
            let (c, r) = proto::clamp_size(cols, rows);
            st.host_cols = c;
            st.host_rows = r;
            self.apply_negotiation(&mut st)
        };
        if let Some((c, r)) = resize {
            self.push_resize(c, r);
        }
    }

    /// If the negotiated size changed, resize the screen model and return the new size.
    fn apply_negotiation(&self, st: &mut SessionState) -> Option<(u16, u16)> {
        let (c, r) = st.negotiate();
        if (c, r) != (st.neg_cols, st.neg_rows) {
            st.neg_cols = c;
            st.neg_rows = r;
            st.vt.resize(c as usize, r as usize);
            Some((c, r))
        } else {
            None
        }
    }

    /// Tell the host to apply a new size, then repaint viewers from the resized screen.
    fn push_resize(&self, cols: u16, rows: u16) {
        self.send_to_host(Frame::Control(proto::Control::Resize { cols, rows }));
        let snapshot = {
            let st = self.state.lock().unwrap();
            crate::render::snapshot(&st.vt)
        };
        let _ = self.output_tx.send(Arc::from(snapshot.as_slice()));
    }
}

/// Token -> session, with the access mode that token grants.
struct TokenEntry {
    session: Arc<SessionShared>,
    mode: Mode,
}

/// The relay: configuration + the live session registry.
pub struct App {
    pub cfg: Config,
    /// Relay host-key fingerprint, advertised to hosts (viewer MITM check).
    pub host_fp: String,
    /// Public keys allowed to host (default key + authorized_keys), parsed once.
    host_keys: Vec<russh::keys::PublicKey>,
    sessions: Mutex<HashMap<String, TokenEntry>>,
    next_sid: AtomicU64,
    /// Per-IP connection timestamps for rate limiting.
    rate: Mutex<HashMap<String, VecDeque<Instant>>>,
}

/// Why a session could not be created.
#[derive(Debug)]
pub enum CreateError {
    TooManySessions,
}

impl App {
    pub fn new(cfg: Config, host_fp: String) -> Arc<Self> {
        use russh::keys::PublicKey;
        let mut host_keys = Vec::new();
        if cfg.use_default_key {
            if let Ok(k) = PublicKey::from_openssh(proto::DEFAULT_TRUSTED_PUBKEY) {
                host_keys.push(k);
            }
        }
        for line in &cfg.authorized_keys {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match PublicKey::from_openssh(line) {
                Ok(k) => host_keys.push(k),
                Err(e) => tracing::warn!(error = %e, "ignoring invalid authorized_keys line"),
            }
        }
        if host_keys.is_empty() {
            tracing::warn!(
                "no host keys trusted: no one can host (set -a or enable the default key)"
            );
        }
        Arc::new(Self {
            cfg,
            host_fp,
            host_keys,
            sessions: Mutex::new(HashMap::new()),
            next_sid: AtomicU64::new(1),
            rate: Mutex::new(HashMap::new()),
        })
    }

    /// Is `key` allowed to host (default key or an authorized_keys entry)? Compares the
    /// key material only — wire keys carry no comment, unlike `authorized_keys` lines.
    pub fn host_key_allowed(&self, key: &russh::keys::PublicKey) -> bool {
        self.host_keys
            .iter()
            .any(|k| k.key_data() == key.key_data())
    }

    /// Record a connection from `ip` and report whether it is within the per-IP rate
    /// limit (sliding 60s window). `rate_per_min == 0` disables the limit.
    pub fn allow_connection(&self, ip: &str) -> bool {
        let limit = self.cfg.rate_per_min;
        if limit == 0 {
            return true;
        }
        let now = Instant::now();
        let window = Duration::from_secs(60);
        let mut rate = self.rate.lock().unwrap();
        let hits = rate.entry(ip.to_string()).or_default();
        while hits
            .front()
            .is_some_and(|t| now.duration_since(*t) > window)
        {
            hits.pop_front();
        }
        if hits.len() as u32 >= limit {
            return false;
        }
        hits.push_back(now);
        true
    }

    /// Create a session for a host, registering both tokens. The caller supplies the
    /// `to_host` sender (it owns the matching receiver and drains it to the host).
    pub fn create_session(
        &self,
        owner_key: String,
        cols: u16,
        rows: u16,
        to_host: mpsc::Sender<Frame>,
    ) -> Result<Arc<SessionShared>, CreateError> {
        let mut sessions = self.sessions.lock().unwrap();
        if sessions.len() / 2 >= self.cfg.max_sessions {
            return Err(CreateError::TooManySessions);
        }
        let (c, r) = proto::clamp_size(cols, rows);
        let (output_tx, _) = broadcast::channel(256);
        let sid = self.next_sid.fetch_add(1, Ordering::Relaxed);
        let session = Arc::new(SessionShared {
            sid,
            ro_token: proto::new_token(),
            rw_token: proto::new_token(),
            reconnect_id: proto::new_reconnect_id(),
            owner_key,
            to_host: Mutex::new(to_host),
            output_tx,
            cancel: CancellationToken::new(),
            connected: AtomicBool::new(true),
            grace: Mutex::new(None),
            record_path: Mutex::new(None),
            state: Mutex::new(SessionState {
                vt: avt::Vt::new(c as usize, r as usize),
                host_cols: c,
                host_rows: r,
                neg_cols: c,
                neg_rows: r,
                viewers: HashMap::new(),
                next_viewer_id: 1,
            }),
        });
        sessions.insert(
            session.ro_token.clone(),
            TokenEntry {
                session: session.clone(),
                mode: Mode::Ro,
            },
        );
        sessions.insert(
            session.rw_token.clone(),
            TokenEntry {
                session: session.clone(),
                mode: Mode::Rw,
            },
        );
        metrics::counter!("choom_sessions_created_total").increment(1);
        metrics::gauge!("choom_sessions_active").increment(1.0);
        Ok(session)
    }

    /// Resolve a viewer token to its session and access mode.
    pub fn lookup_token(&self, token: &str) -> Option<(Arc<SessionShared>, Mode)> {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(token).map(|e| (e.session.clone(), e.mode))
    }

    /// Find a live session by its reconnect id (for resume / `choom clients`).
    pub fn find_by_reconnect(&self, id: &str) -> Option<Arc<SessionShared>> {
        let sessions = self.sessions.lock().unwrap();
        sessions
            .values()
            .find(|e| proto::ct_eq(&e.session.reconnect_id, id))
            .map(|e| e.session.clone())
    }

    /// Remove a session and both its tokens from the registry.
    pub fn remove_session(&self, session: &SessionShared) {
        let mut sessions = self.sessions.lock().unwrap();
        if sessions.remove(&session.ro_token).is_some() {
            metrics::gauge!("choom_sessions_active").decrement(1.0);
        }
        sessions.remove(&session.rw_token);
    }

    /// Number of live sessions (for metrics/logging).
    pub fn session_count(&self) -> usize {
        self.sessions.lock().unwrap().len() / 2
    }

    /// Drain: end every session (viewer tasks detach) and clear the registry. Used on
    /// SIGTERM for clean rolling restarts.
    pub fn shutdown_all(&self) {
        let mut sessions = self.sessions.lock().unwrap();
        for entry in sessions.values() {
            entry.session.close();
        }
        sessions.clear();
    }
}
