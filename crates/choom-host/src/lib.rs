// SPDX-License-Identifier: GPL-3.0-or-later
//! choom host: spawn `$SHELL` in a local PTY and relay it through the server.
//!
//! The host opens one russh connection to the relay (username `new`), a single
//! [`choom_proto`]-framed channel, and bridges:
//!   * PTY output  -> `Frame::Output` to the relay (and the host's own screen),
//!   * `Frame::Input` (RW viewer keystrokes) -> the PTY (device-attribute replies
//!     stripped so they don't land at the prompt),
//!   * `Frame::Resize` (negotiated size) -> `TIOCSWINSZ` on the PTY,
//!   * the host's own stdin -> the PTY, and `SIGWINCH` -> `Frame::HostSize`.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use choom_proto as proto;
use choom_proto::codec::FrameCodec;
use choom_proto::{Client, Control, Frame, Mode, Ready, Transport};
use futures::{SinkExt, StreamExt};
use pty_process::Size;
use russh::client;
use russh::keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::codec::Framed;
use tracing::{info, warn};

mod term;

/// The bundled default private key (its public half is trusted by the public relay).
const DEFAULT_ID: &str = include_str!("default_id");

/// Per-attempt cap on the relay connect+auth handshake. Without it an unreachable relay
/// (a dropped SYN) blocks on the OS connect timeout (~127s on Linux) with a blank,
/// un-interruptible screen.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);
/// Delay between connect attempts (initial dial and reconnects alike).
const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
/// How many times the initial dial is retried before giving up. Reconnects (after a
/// session was established) retry indefinitely, since the local shell is still alive.
const INITIAL_ATTEMPTS: u32 = 3;

/// Host configuration.
#[derive(Clone, Debug)]
pub struct HostConfig {
    /// Relay hostname.
    pub server: String,
    /// Relay SSH port.
    pub port: u16,
    /// Shell to run; `None` -> `$SHELL` -> `/bin/sh`.
    pub shell: Option<String>,
    /// Ask the relay to record this session.
    pub record: bool,
    /// Explicit identity key file (`-i`); overrides the default/persisted key.
    pub identity: Option<std::path::PathBuf>,
    /// Use the bundled default key (public-relay convenience; opt out for private).
    pub use_default_key: bool,
    /// Expected relay host-key fingerprint (`SHA256:…`); verified on connect if set.
    pub fingerprint: Option<String>,
    /// Detached "remote-support" mode: print the share links and relay the shell to
    /// viewers, but do NOT attach the host's local terminal (no raw mode, no local
    /// echo/keystrokes). The shell is driven only by read-write viewers.
    pub detach: bool,
    /// Print connection diagnostics (resolved relay address, each connect attempt, the
    /// relay host-key fingerprint, the auth method) on the host's own screen.
    pub verbose: bool,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            server: proto::DEFAULT_SERVER.to_string(),
            port: proto::DEFAULT_SSH_PORT,
            shell: None,
            record: false,
            identity: None,
            use_default_key: true,
            fingerprint: None,
            detach: false,
            verbose: false,
        }
    }
}

/// Persisted state of the active session, so `choom clients` (a separate process) can
/// find and query it.
#[derive(Serialize, Deserialize)]
struct SessionFile {
    reconnect_id: String,
    server: String,
    port: u16,
}

fn session_file_path() -> std::path::PathBuf {
    config_dir().join("session.json")
}

fn write_session_file(sf: &SessionFile) -> Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir).ok();
    let json = serde_json::to_string_pretty(sf)?;
    std::fs::write(session_file_path(), json).context("write session file")?;
    Ok(())
}

/// `~/.config/choom` (or `$XDG_CONFIG_HOME/choom`), where keys + session state live.
/// `CHOOM_CONFIG_DIR` overrides it outright (used by tests).
fn config_dir() -> std::path::PathBuf {
    if let Some(d) = std::env::var_os("CHOOM_CONFIG_DIR") {
        return std::path::PathBuf::from(d);
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("choom")
}

/// Resolve the host's private key: explicit `-i` identity, else the bundled default
/// key (when enabled), else a per-user key persisted at `~/.config/choom/id_ed25519`.
fn resolve_key(identity: Option<&std::path::Path>, use_default_key: bool) -> Result<PrivateKey> {
    if let Some(path) = identity {
        return russh::keys::load_secret_key(path, None)
            .with_context(|| format!("load identity {}", path.display()));
    }
    if use_default_key {
        return PrivateKey::from_openssh(DEFAULT_ID).context("parse bundled default key");
    }
    persisted_key()
}

fn persisted_key() -> Result<PrivateKey> {
    use russh::keys::ssh_key::LineEnding;
    let dir = config_dir();
    let path = dir.join("id_ed25519");
    if path.exists() {
        return russh::keys::load_secret_key(&path, None)
            .with_context(|| format!("load {}", path.display()));
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
    let pem = key.to_openssh(LineEnding::LF)?;
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        f.write_all(pem.as_bytes())?;
    }
    Ok(key)
}

/// Runtime hooks controlling how the host drives its local terminal.
#[derive(Default)]
struct Hooks {
    /// Attach the host's own terminal: raw mode, stdin -> PTY, PTY -> stdout, SIGWINCH.
    /// Off in `--detach` mode and in tests.
    attach_terminal: bool,
    /// Print the share-link banner when the session is ready.
    banner: bool,
    /// If set, each `Ready` is forwarded here (tests use it to learn the tokens).
    ready_tx: Option<mpsc::UnboundedSender<Ready>>,
}

/// Commands to the single task that owns the PTY write half.
enum PtyCmd {
    Write(Vec<u8>),
    Resize(u16, u16),
}

struct Cli {
    /// If set, the relay's host key must match this `SHA256:…` fingerprint.
    expected_fp: Option<String>,
    /// Records the relay's actual host-key fingerprint, so `--verbose` can report it.
    seen_fp: Arc<std::sync::Mutex<Option<String>>>,
}

impl client::Handler for Cli {
    type Error = russh::Error;
    async fn check_server_key(&mut self, key: &PublicKey) -> Result<bool, Self::Error> {
        // No E2E (the relay sees cleartext by design); this only guards against MITM of
        // the host<->relay link when an expected fingerprint is configured.
        let got = key.fingerprint(Default::default()).to_string();
        *self.seen_fp.lock().unwrap() = Some(got.clone());
        match &self.expected_fp {
            Some(want) => {
                if &got != want {
                    warn!(expected = %want, got = %got, "relay host-key fingerprint mismatch");
                }
                Ok(&got == want)
            }
            None => Ok(true),
        }
    }
}

/// Run the host with a default config on its own Tokio runtime (CLI entrypoint).
pub fn run() -> Result<()> {
    run_with(HostConfig::default())
}

/// Run the host with an explicit config on its own Tokio runtime.
pub fn run_with(cfg: HostConfig) -> Result<()> {
    init_tracing();
    let rt = tokio::runtime::Runtime::new().context("build tokio runtime")?;
    let result = rt.block_on(host(cfg));
    // `tokio::io::stdin()` reads on a blocking thread whose read cannot be cancelled. The
    // default runtime drop blocks until every blocking task finishes — so when the shell
    // exits, the process would hang on that parked stdin read until the next keypress (the
    // "exit doesn't quit until you press Return again" bug). Abandon blocking threads on
    // shutdown so the process exits the moment the session ends.
    rt.shutdown_background();
    result
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let json = std::env::var("CHOOM_LOG_FORMAT").is_ok_and(|v| v.eq_ignore_ascii_case("json"));
    if json {
        let _ = fmt().with_env_filter(filter).json().try_init();
    } else {
        let _ = fmt().with_env_filter(filter).try_init();
    }
}

/// Share the local shell (the CLI behaviour). Attaches the local terminal unless the
/// config requests `--detach`.
pub async fn host(cfg: HostConfig) -> Result<()> {
    let attach_terminal = !cfg.detach;
    run_host(
        cfg,
        Hooks {
            attach_terminal,
            banner: true,
            ready_tx: None,
        },
    )
    .await
}

/// Test/embedding entrypoint: run the host non-interactively (no raw mode / stdin /
/// banner) and forward each `Ready` to `ready_tx` so the caller learns the tokens.
#[doc(hidden)]
pub async fn run_host_for_test(
    cfg: HostConfig,
    ready_tx: mpsc::UnboundedSender<Ready>,
) -> Result<()> {
    run_host(
        cfg,
        Hooks {
            attach_terminal: false,
            banner: false,
            ready_tx: Some(ready_tx),
        },
    )
    .await
}

async fn run_host(cfg: HostConfig, hooks: Hooks) -> Result<()> {
    let shell = cfg
        .shell
        .clone()
        .or_else(|| std::env::var("SHELL").ok())
        .unwrap_or_else(|| "/bin/sh".to_string());

    // --- local PTY + shell ---
    let (init_cols, init_rows) = term::initial_size(hooks.attach_terminal);
    let (pty, pts) = pty_process::open().context("open pty")?;
    pty.resize(Size::new(init_rows, init_cols)).ok();
    let mut child = pty_process::Command::new(&shell)
        .spawn(pts)
        .with_context(|| format!("spawn shell {shell:?}"))?;
    let (mut pty_read, pty_write) = pty.into_split();

    // PTY writer task: the single owner of the write half (input + resizes).
    let (pty_cmd_tx, mut pty_cmd_rx) = mpsc::unbounded_channel::<PtyCmd>();
    tokio::spawn(async move {
        let mut pty_write = pty_write;
        while let Some(cmd) = pty_cmd_rx.recv().await {
            match cmd {
                PtyCmd::Write(bytes) => {
                    if pty_write.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                PtyCmd::Resize(cols, rows) => {
                    let _ = pty_write.resize(Size::new(rows, cols));
                }
            }
        }
    });

    // --- host terminal + key (set up once, persist across reconnects) ---
    let key = Arc::new(resolve_key(cfg.identity.as_deref(), cfg.use_default_key)?);
    // Raw mode and stdin are claimed lazily, only after the first relay connect
    // succeeds (below). While we are still dialing — possibly an unreachable relay —
    // the terminal stays in cooked mode, so the shell prompt isn't hidden behind a
    // blank screen and Ctrl-C still aborts instead of being swallowed as a keystroke.
    let mut _raw_guard = None;
    let mut stdin: Option<tokio::io::Stdin> = None;
    let mut stdout = tokio::io::stdout();
    let mut winch = term::winch_stream(hooks.attach_terminal);
    let mut pty_buf = [0u8; 8192];
    let mut stdin_buf = [0u8; 4096];
    let mut established = false;
    let mut attempt = 0u32;
    // Captures the relay host-key fingerprint each connect, for `--verbose` reporting.
    let seen_fp = Arc::new(std::sync::Mutex::new(None));

    // The PTY + shell outlive the relay connection: brief relay drops are survived by
    // reconnecting with the reconnect id while the shell keeps running.
    let mut reconnect_id: Option<String> = None;
    let mut size = (init_cols, init_rows);

    'reconnect: loop {
        let user = if reconnect_id.is_some() {
            "resume"
        } else {
            "new"
        };
        if !established && attempt == 0 {
            status(
                &mut stdout,
                hooks.banner,
                format!("connecting to {}:{}…", cfg.server, cfg.port),
            )
            .await;
        }
        if cfg.verbose {
            // Resolve up front so `-v` shows exactly which address(es) we dial — this is
            // where an IPv6-only / stale-DNS relay becomes visible.
            let resolved = match tokio::net::lookup_host((cfg.server.as_str(), cfg.port)).await {
                Ok(addrs) => {
                    let list = addrs.map(|a| a.to_string()).collect::<Vec<_>>().join(", ");
                    if list.is_empty() {
                        "no addresses".to_string()
                    } else {
                        list
                    }
                }
                Err(e) => format!("lookup failed: {e}"),
            };
            status(
                &mut stdout,
                hooks.banner,
                format!("resolved {} -> {resolved}", cfg.server),
            )
            .await;
        }
        let connect = async {
            let handler = Cli {
                expected_fp: cfg.fingerprint.clone(),
                seen_fp: seen_fp.clone(),
            };
            let mut session = client::connect(
                Arc::new(client::Config::default()),
                (cfg.server.as_str(), cfg.port),
                handler,
            )
            .await
            .with_context(|| format!("connect to {}:{}", cfg.server, cfg.port))?;
            let auth = session
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(key.clone(), None))
                .await?;
            anyhow::ensure!(auth.success(), "relay rejected host key");
            let channel = session
                .channel_open_session()
                .await
                .context("open channel")?;
            Ok::<_, anyhow::Error>((session, Framed::new(channel.into_stream(), FrameCodec)))
        };
        // Cap the whole connect+auth+channel handshake: an unreachable relay (dropped
        // SYN) would otherwise block on the OS connect timeout (~127s) with no output
        // and — once attached — no way to Ctrl-C out.
        let connect = match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
            Ok(r) => r,
            Err(_) => Err(anyhow::anyhow!(
                "connect to {}:{} timed out after {}s",
                cfg.server,
                cfg.port,
                CONNECT_TIMEOUT.as_secs()
            )),
        };

        let (_session, framed) = match connect {
            Ok(x) => x,
            // A session already exists: keep retrying — the shell is still alive and we
            // want to resume it — but surface each attempt instead of failing silently.
            Err(e) if reconnect_id.is_some() => {
                attempt += 1;
                status(
                    &mut stdout,
                    hooks.banner,
                    format!("relay unreachable, reconnecting (attempt {attempt})… ({e:#})"),
                )
                .await;
                tokio::time::sleep(RETRY_DELAY).await;
                continue 'reconnect;
            }
            // No session yet: retry a bounded number of times, then give up cleanly
            // instead of hanging. The shell was already spawned, so kill it on the way out.
            Err(e) => {
                attempt += 1;
                if attempt >= INITIAL_ATTEMPTS {
                    let _ = child.start_kill();
                    return Err(e.context(format!(
                        "relay {}:{} unreachable after {attempt} attempts",
                        cfg.server, cfg.port
                    )));
                }
                status(
                    &mut stdout,
                    hooks.banner,
                    format!("{e:#}; retrying ({attempt}/{INITIAL_ATTEMPTS})…"),
                )
                .await;
                tokio::time::sleep(RETRY_DELAY).await;
                continue 'reconnect;
            }
        };

        if cfg.verbose {
            let fp = seen_fp
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            status(
                &mut stdout,
                hooks.banner,
                format!("connected; relay host key {fp}; authenticated as `{user}` (publickey)"),
            )
            .await;
        }

        // First successful connect: only now claim the terminal. Raw mode stayed off
        // while dialing, so an unreachable relay could still be aborted with Ctrl-C.
        if !established {
            established = true;
            if hooks.attach_terminal {
                _raw_guard = term::enter_raw();
                stdin = Some(tokio::io::stdin());
            }
        }
        let (mut sink, mut source) = framed.split();
        let (to_relay_tx, mut to_relay_rx) = mpsc::unbounded_channel::<Frame>();
        let writer = tokio::spawn(async move {
            while let Some(frame) = to_relay_rx.recv().await {
                if sink.send(frame).await.is_err() {
                    break;
                }
            }
        });

        // Announce (create) or resume the session.
        match &reconnect_id {
            Some(id) => {
                to_relay_tx
                    .send(Frame::Control(Control::Resume {
                        id: id.clone(),
                        cols: size.0,
                        rows: size.1,
                    }))
                    .ok();
            }
            None => {
                to_relay_tx
                    .send(Frame::Control(Control::Hello {
                        cols: size.0,
                        rows: size.1,
                    }))
                    .ok();
                if cfg.record {
                    to_relay_tx.send(Frame::Control(Control::Record)).ok();
                }
            }
        }

        // Inner loop: relay <-> PTY. Shell exit / fatal error -> return; the only `break`
        // is a relay drop, after which we reconnect (if a session was established).
        loop {
            tokio::select! {
                n = pty_read.read(&mut pty_buf) => match n {
                    Ok(0) | Err(_) => { writer.abort(); let _ = child.start_kill(); return Ok(()); }
                    Ok(n) => {
                        let bytes = pty_buf[..n].to_vec();
                        if hooks.attach_terminal {
                            let _ = stdout.write_all(&bytes).await;
                            let _ = stdout.flush().await;
                        }
                        to_relay_tx.send(Frame::Output(bytes)).ok();
                    }
                },

                frame = source.next() => {
                    let Some(Ok(frame)) = frame else { break };
                    match frame {
                        Frame::Input(bytes) => {
                            pty_cmd_tx.send(PtyCmd::Write(term::strip_da_replies(&bytes))).ok();
                        }
                        Frame::Control(Control::Resize { cols, rows }) => {
                            pty_cmd_tx.send(PtyCmd::Resize(cols, rows)).ok();
                        }
                        Frame::Control(Control::Ready(ready)) => {
                            reconnect_id = Some(ready.id.clone());
                            let _ = write_session_file(&SessionFile {
                                reconnect_id: ready.id.clone(),
                                server: cfg.server.clone(),
                                port: cfg.port,
                            });
                            if hooks.banner {
                                print_banner(&mut stdout, &ready, &cfg).await;
                            }
                            if let Some(tx) = &hooks.ready_tx {
                                let _ = tx.send(ready);
                            }
                        }
                        Frame::Control(Control::Join(c)) => {
                            info!(viewer = c.id, ip = %c.ip, mode = ?c.mode, "viewer joined");
                        }
                        Frame::Control(Control::Leave { id }) => info!(viewer = id, "viewer left"),
                        Frame::Control(Control::Error { msg }) => {
                            warn!(%msg, "relay error");
                            writer.abort();
                            let _ = child.start_kill();
                            return Ok(());
                        }
                        _ => {}
                    }
                },

                n = read_opt(&mut stdin, &mut stdin_buf) => {
                    if let Some(Ok(n)) = n {
                        if n > 0 {
                            pty_cmd_tx.send(PtyCmd::Write(stdin_buf[..n].to_vec())).ok();
                        }
                    }
                },

                _ = winch_recv(&mut winch) => {
                    size = term::initial_size(true);
                    pty_cmd_tx.send(PtyCmd::Resize(size.0, size.1)).ok();
                    to_relay_tx.send(Frame::Control(Control::HostSize { cols: size.0, rows: size.1 })).ok();
                },

                _ = child.wait() => { writer.abort(); let _ = child.start_kill(); return Ok(()); }
            }
        }

        writer.abort();
        if reconnect_id.is_some() {
            status(
                &mut stdout,
                hooks.banner,
                "relay connection lost; reconnecting…".into(),
            )
            .await;
            attempt = 0;
            tokio::time::sleep(RETRY_DELAY).await;
            continue 'reconnect;
        }
        // Lost the relay before a session was ever established: give up.
        let _ = child.start_kill();
        return Ok(());
    }
}

/// Print a one-line status/progress message to the host's screen, CRLF-terminated so it
/// renders cleanly whether or not the terminal is in raw mode. Silent when `verbose` is
/// false (tests / embedding) so it never pollutes captured output.
async fn status(out: &mut tokio::io::Stdout, verbose: bool, msg: String) {
    if !verbose {
        return;
    }
    let _ = out.write_all(format!("choom: {msg}\r\n").as_bytes()).await;
    let _ = out.flush().await;
}

/// Read into `buf` from an optional reader; `None` if there is no reader.
async fn read_opt(
    reader: &mut Option<tokio::io::Stdin>,
    buf: &mut [u8],
) -> Option<std::io::Result<usize>> {
    match reader {
        Some(r) => Some(r.read(buf).await),
        None => {
            // Never resolves, so this select branch is inert when not interactive.
            std::future::pending().await
        }
    }
}

async fn winch_recv(winch: &mut Option<tokio::signal::unix::Signal>) {
    match winch {
        Some(s) => {
            s.recv().await;
        }
        None => std::future::pending().await,
    }
}

/// The `ssh` command a viewer runs to attach. Includes `-p <port>` whenever the relay
/// isn't on the default SSH port (22): without it `ssh <token>@host` hits port 22 —
/// usually a *different* sshd — which then prompts for a password instead of attaching.
fn viewer_ssh_cmd(token: &str, host: &str, port: u16) -> String {
    if port == 22 {
        format!("ssh {token}@{host}")
    } else {
        format!("ssh -p {port} {token}@{host}")
    }
}

async fn print_banner(stdout: &mut tokio::io::Stdout, ready: &Ready, cfg: &HostConfig) {
    let web = ready
        .web
        .as_ref()
        .map(|u| format!("\r\n  Web (RO):   {u}"))
        .unwrap_or_default();
    let detach = if cfg.detach {
        "\r\nDetached: your local terminal is not attached — the shell is driven by \
         read-write viewers. Ctrl-C here ends the session.\r\n"
    } else {
        ""
    };
    let ro_cmd = viewer_ssh_cmd(&ready.ro, &ready.host, ready.port);
    let rw_cmd = viewer_ssh_cmd(&ready.rw, &ready.host, ready.port);
    let banner = format!(
        "\r\nWelcome, Choomba, your shell is now shared!\r\n\
         Share a link below; viewers connect with their ssh client.\r\n\r\n  \
         Read-only:  {ro_cmd}\r\n  \
         Read-write: {rw_cmd}  (full shell access!){web}\r\n{detach}\r\n",
    );
    let _ = stdout.write_all(banner.as_bytes()).await;
    let _ = stdout.flush().await;
}

/// List the viewers of your session (`choom clients`).
pub fn run_clients(identity: Option<std::path::PathBuf>, use_default_key: bool) -> Result<()> {
    init_tracing();
    let rt = tokio::runtime::Runtime::new().context("build tokio runtime")?;
    rt.block_on(clients_async(identity, use_default_key))
}

async fn clients_async(identity: Option<std::path::PathBuf>, use_default_key: bool) -> Result<()> {
    let roster = fetch_roster(identity, use_default_key).await?;
    print_roster(&roster);
    Ok(())
}

/// Connect as the query role and fetch the active session's viewer roster.
async fn fetch_roster(
    identity: Option<std::path::PathBuf>,
    use_default_key: bool,
) -> Result<Vec<Client>> {
    let text = std::fs::read_to_string(session_file_path())
        .context("no active session (start `choom host` first)")?;
    let sf: SessionFile = serde_json::from_str(&text).context("parse session file")?;
    let key = resolve_key(identity.as_deref(), use_default_key)?;

    let mut session = client::connect(
        Arc::new(client::Config::default()),
        (sf.server.as_str(), sf.port),
        Cli {
            expected_fp: None,
            seen_fp: Arc::new(std::sync::Mutex::new(None)),
        },
    )
    .await
    .map_err(|e| {
        anyhow::anyhow!(
            "relay {}:{} is unreachable ({e}); the session may have ended — \
             start `choom host` first",
            sf.server,
            sf.port
        )
    })?;
    let auth = session
        .authenticate_publickey("query", PrivateKeyWithHashAlg::new(Arc::new(key), None))
        .await?;
    anyhow::ensure!(auth.success(), "relay rejected query key");

    let channel = session
        .channel_open_session()
        .await
        .context("open channel")?;
    let framed = Framed::new(channel.into_stream(), FrameCodec);
    let (mut sink, mut source) = framed.split();
    sink.send(Frame::Control(Control::Query {
        id: sf.reconnect_id,
    }))
    .await?;

    match tokio::time::timeout(std::time::Duration::from_secs(5), source.next()).await {
        Ok(Some(Ok(Frame::Control(Control::Clients(list))))) => Ok(list),
        Ok(Some(Ok(Frame::Control(Control::Error { msg })))) => anyhow::bail!("relay: {msg}"),
        _ => anyhow::bail!("no response from relay"),
    }
}

/// Test entrypoint for [`fetch_roster`].
#[doc(hidden)]
pub async fn fetch_roster_for_test(
    identity: Option<std::path::PathBuf>,
    use_default_key: bool,
) -> Result<Vec<Client>> {
    fetch_roster(identity, use_default_key).await
}

fn print_roster(list: &[Client]) {
    if list.is_empty() {
        println!("No viewers connected.");
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!(
        "{:>3}  {:<4} {:<4} {:<16} {:<9} FOR",
        "ID", "MODE", "VIA", "IP", "SIZE"
    );
    for c in list {
        let mode = match c.mode {
            Mode::Ro => "RO",
            Mode::Rw => "RW",
        };
        let via = match c.transport {
            Transport::Ssh => "ssh",
            Transport::Web => "web",
        };
        let secs = now.saturating_sub(c.connected_at);
        println!(
            "{:>3}  {:<4} {:<4} {:<16} {:<9} {}s",
            c.id,
            mode,
            via,
            c.ip,
            format!("{}x{}", c.cols, c.rows),
            secs
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_frame_builds() {
        let f = Frame::Control(Control::Hello { cols: 80, rows: 24 });
        assert!(f.encode().is_ok());
    }

    #[test]
    fn viewer_ssh_cmd_includes_nondefault_port() {
        // The relay runs on 2200, so the share link must carry `-p 2200` — otherwise
        // `ssh <token>@host` hits port 22 (a different sshd) and prompts for a password.
        assert_eq!(
            viewer_ssh_cmd("TOK", "choom.sh", 2200),
            "ssh -p 2200 TOK@choom.sh"
        );
        // On the default SSH port the `-p` is omitted for a clean link.
        assert_eq!(viewer_ssh_cmd("TOK", "choom.sh", 22), "ssh TOK@choom.sh");
    }
}
