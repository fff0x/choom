// SPDX-License-Identifier: GPL-3.0-or-later
//! choom relay: the central SSH broker, session registry, `avt` screen model,
//! axum web view and recorder.
//!
//! Milestone 3 implements the core relay: a russh server that authenticates hosts by
//! public key and viewers by token (the SSH username), carries the host link over one
//! [`choom_proto`]-framed channel, models each session with `avt`, and streams a
//! snapshot + live output to stock-`ssh` viewers (read-write keystrokes flow back to
//! the host; read-only input is dropped). Web (M4), recorder/limits/drain (later).

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Context as _;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use russh::keys::{Algorithm, PrivateKey};
use tokio::net::TcpListener;
use tracing::info;

pub mod asciicast;
pub mod recorder;
pub mod relay;
pub mod render;
pub mod session;
pub mod web;

pub use session::{App, Config};

/// Install a tracing subscriber (idempotent; honours `RUST_LOG`, defaults to `info`).
/// `CHOOM_LOG_FORMAT=json` selects structured JSON output (for pod-log aggregation).
pub fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let json = std::env::var("CHOOM_LOG_FORMAT").is_ok_and(|v| v.eq_ignore_ascii_case("json"));
    if json {
        let _ = fmt().with_env_filter(filter).json().try_init();
    } else {
        let _ = fmt().with_env_filter(filter).try_init();
    }
}

/// Process-wide Prometheus recorder (installed once; `None` if install failed or it
/// was already installed by something else, e.g. across tests).
static METRICS: OnceLock<Option<PrometheusHandle>> = OnceLock::new();

fn metrics_handle() -> Option<&'static PrometheusHandle> {
    METRICS
        .get_or_init(|| PrometheusBuilder::new().install_recorder().ok())
        .as_ref()
}

/// Render the Prometheus exposition text, if the recorder is installed.
pub fn render_metrics() -> Option<String> {
    metrics_handle().map(|h| h.render())
}

/// Run the relay with a default config on its own Tokio runtime (CLI entrypoint).
pub fn run() -> anyhow::Result<()> {
    run_with(Config::default())
}

/// Run the relay with an explicit config on its own Tokio runtime.
pub fn run_with(cfg: Config) -> anyhow::Result<()> {
    init_tracing();
    let rt = tokio::runtime::Runtime::new().context("build tokio runtime")?;
    rt.block_on(serve(cfg))
}

/// Bind the configured SSH address and serve until the process exits.
pub async fn serve(cfg: Config) -> anyhow::Result<()> {
    let listener = TcpListener::bind(cfg.ssh_addr)
        .await
        .with_context(|| format!("bind {}", cfg.ssh_addr))?;
    serve_on(cfg, listener).await
}

/// Load a persistent SSH host key from `path`, generating + saving (`0600`) one if it
/// does not exist — so the relay's fingerprint is stable across restarts.
fn load_or_create_host_key(path: &std::path::Path) -> anyhow::Result<PrivateKey> {
    if path.exists() {
        return russh::keys::load_secret_key(path, None)
            .with_context(|| format!("load host key {}", path.display()));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
    let pem = key.to_openssh(russh::keys::ssh_key::LineEnding::LF)?;
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("write host key {}", path.display()))?;
        f.write_all(pem.as_bytes())?;
    }
    info!(path = %path.display(), "generated relay host key");
    Ok(key)
}

/// Serve on an already-bound listener (used by tests on an ephemeral port).
pub async fn serve_on(cfg: Config, listener: TcpListener) -> anyhow::Result<()> {
    info!(version = env!("CARGO_PKG_VERSION"), "choom relay starting");
    // Persistent host key if configured (stable fingerprint), else ephemeral.
    let host_key = match &cfg.host_key_path {
        Some(path) => load_or_create_host_key(path)?,
        None => PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
            .context("generate relay host key")?,
    };
    let fp = host_key
        .public_key()
        .fingerprint(Default::default())
        .to_string();

    // Install the Prometheus recorder (idempotent across the process).
    let _ = metrics_handle();

    let app = App::new(cfg, fp);

    // Recording retention GC (hourly) when storage is enabled.
    if let Some(dir) = app.cfg.record_dir.clone() {
        let (age, mb) = (app.cfg.record_max_age_days, app.cfg.record_max_mb);
        tokio::spawn(async move {
            loop {
                recorder::gc(&dir, age, mb);
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
    }

    // Optional output-only web view on its own listener.
    if let Some(web_addr) = app.cfg.web_addr {
        let web_app = app.clone();
        tokio::spawn(async move {
            if let Err(e) = web::serve_web(web_app, web_addr).await {
                tracing::error!(error = %e, "web view stopped");
            }
        });
    }

    // Optional operator-internal Prometheus endpoint on its own listener (keep it off the
    // public web port; bind it to loopback or a private interface and scrape it there).
    if let Some(metrics_addr) = app.cfg.metrics_addr {
        let m_app = app.clone();
        tokio::spawn(async move {
            if let Err(e) = web::serve_metrics(m_app, metrics_addr).await {
                tracing::error!(error = %e, "metrics endpoint stopped");
            }
        });
    }

    let ssh_config = Arc::new(russh::server::Config {
        inactivity_timeout: Some(Duration::from_secs(3600)),
        auth_rejection_time: Duration::from_secs(2),
        // Advertise only `none` (viewers: the username is the token) and `publickey`
        // (hosts). russh's default set also offers password + keyboard-interactive, which
        // makes a stock `ssh <bad-token>@relay` fall through to a misleading password
        // prompt instead of failing cleanly with "Permission denied".
        methods: russh::MethodSet::from(
            &[russh::MethodKind::None, russh::MethodKind::PublicKey][..],
        ),
        keys: vec![host_key],
        ..Default::default()
    });

    if let Ok(addr) = listener.local_addr() {
        info!(%addr, "choom relay listening");
    }
    let mut server = relay::Relay { app: app.clone() };
    let running = russh::server::Server::run_on_socket(&mut server, ssh_config, &listener);
    let shutdown = running.handle();

    // Graceful drain on SIGTERM/Ctrl-C: stop accepting, end sessions, exit cleanly.
    tokio::pin!(running);
    tokio::select! {
        r = &mut running => r.context("relay server loop")?,
        _ = shutdown_signal() => {
            info!(sessions = app.session_count(), "draining: shutting down");
            app.set_draining(); // flip /healthz to unready before tearing sessions down
            app.shutdown_all();
            shutdown.shutdown("choom relay shutting down".into());
            let _ = (&mut running).await;
        }
    }
    Ok(())
}

/// Resolve when the process is asked to terminate (SIGTERM on Unix, or Ctrl-C).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
