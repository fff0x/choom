// SPDX-License-Identifier: GPL-3.0-or-later
//! `choom` — minimal SSH terminal-sharing relay.
//!
//! Dispatches the three roles: the relay (`server`), the host that shares its shell
//! (`host`), and the viewer-roster query (`clients`). Viewers themselves need no
//! choom binary — they attach with stock `ssh <token>@relay`.
//!
//! Configuration precedence: explicit CLI flag > `-c file.yaml` > built-in default.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use choom_host::HostConfig;
use choom_proto as proto;
use choom_server::Config;
use clap::{CommandFactory, Parser, Subcommand};
use serde::Deserialize;

/// GNU-style `-V`/`--version` output: version + copyright + license.
const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "\nCopyright (C) 2026 Max 'ff0x' Reineke <ff0x@this-is-fine.io>",
    "\nLicense GPL-3.0-or-later: GNU GPL version 3 or later <https://gnu.org/licenses/gpl.html>",
    "\nThis is free software: you are free to change and redistribute it.",
);

#[derive(Parser)]
#[command(
    name = "choom",
    version = VERSION,
    propagate_version = true,
    author,
    about = "Minimal SSH terminal-sharing relay",
    after_long_help = "EXAMPLES:\n  \
        Share your shell on a relay:\n      choom host -s relay.example.com\n  \
        Run a relay with the web view:\n      choom server -H relay.example.com -w 0.0.0.0:8080\n  \
        List who is watching your session:\n      choom clients\n\n\
        Viewers need no choom binary — they attach with stock ssh:\n      \
        ssh <token>@relay.example.com"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the central relay.
    Server(ServerArgs),
    /// Share your local shell through a relay.
    Host(HostArgs),
    /// List the viewers of your session.
    Clients(ClientsArgs),
}

/// Build the top-level clap command (used by the man-page generator).
pub fn command() -> clap::Command {
    Cli::command()
}

/// Parse the arguments and run the selected role.
pub fn run() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Server(args) => choom_server::run_with(resolve_server(args)?),
        Cmd::Host(args) => choom_host::run_with(resolve_host(args)?),
        Cmd::Clients(args) => choom_host::run_clients(args.identity, !args.no_default_key),
    }
}

// --- server ------------------------------------------------------------------

#[derive(clap::Args)]
#[command(after_long_help = "EXAMPLES:\n  \
    Public relay (trusts the bundled default key) with web + recording:\n      \
    choom server -H relay.example.com -w 0.0.0.0:8080 -r /var/lib/choom/rec\n  \
    Private relay (only listed keys may host):\n      \
    choom server --no-default-key -a authorized_keys\n  \
    From a YAML file (CLI flags still win):\n      choom server -c /etc/choom.yaml")]
struct ServerArgs {
    /// YAML config file (CLI flags override its values).
    #[arg(long, short = 'c')]
    config: Option<PathBuf>,
    /// Address to listen for SSH on [default: 0.0.0.0:2200].
    #[arg(long, env = "CHOOM_SSH_ADDR")]
    ssh_addr: Option<SocketAddr>,
    /// Hostname advertised in the share links [default: choom.sh].
    #[arg(long, short = 'H', env = "CHOOM_HOST")]
    host: Option<String>,
    /// SSH port advertised in the share links [default: 2200].
    #[arg(long, short = 'p', env = "CHOOM_PORT")]
    port: Option<u16>,
    /// Serve the output-only web view on this address (omit to disable).
    #[arg(long, short = 'w', env = "CHOOM_WEB_ADDR")]
    web: Option<SocketAddr>,
    /// Public web URL advertised to hosts (e.g. behind a TLS gateway).
    #[arg(long, short = 'W', env = "CHOOM_WEB_URL")]
    web_url: Option<String>,
    /// Global cap on concurrent sessions [default: 200].
    #[arg(long)]
    max_sessions: Option<usize>,
    /// Per-session viewer cap [default: 50].
    #[arg(long)]
    max_viewers: Option<usize>,
    /// New connections per source IP per minute, 0 = unlimited [default: 60].
    #[arg(long)]
    rate_limit: Option<u32>,
    /// File of OpenSSH public keys allowed to host (one per line).
    #[arg(long, short = 'a')]
    authorized_keys: Option<PathBuf>,
    /// Do not trust the bundled default key (require authorized_keys).
    #[arg(long)]
    no_default_key: bool,
    /// Store recordings in this directory (enables recording storage).
    #[arg(long, short = 'r', env = "CHOOM_RECORD_DIR")]
    record_dir: Option<PathBuf>,
    /// Record every session, not only those whose host opted in.
    #[arg(long)]
    record_all: bool,
    /// Prune recordings older than this many days, 0 = no limit [default: 7].
    #[arg(long)]
    record_max_age_days: Option<u64>,
    /// Cap total recordings size in MB, oldest first, 0 = no limit [default: 500].
    #[arg(long)]
    record_max_mb: Option<u64>,
    /// Directory of prebuilt binaries to serve at `/download`.
    #[arg(long, env = "CHOOM_DOWNLOADS")]
    downloads: Option<PathBuf>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ServerFile {
    ssh_addr: Option<SocketAddr>,
    host: Option<String>,
    port: Option<u16>,
    web: Option<SocketAddr>,
    web_url: Option<String>,
    max_sessions: Option<usize>,
    max_viewers: Option<usize>,
    rate_limit: Option<u32>,
    record_dir: Option<PathBuf>,
    record_all: Option<bool>,
    record_max_age_days: Option<u64>,
    record_max_mb: Option<u64>,
    authorized_keys: Option<Vec<String>>,
    use_default_key: Option<bool>,
    downloads: Option<PathBuf>,
}

fn resolve_server(a: ServerArgs) -> Result<Config> {
    let f: ServerFile = load_file(a.config.as_deref())?;
    let authorized_keys = match &a.authorized_keys {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("read authorized_keys {}", path.display()))?
            .lines()
            .map(str::to_string)
            .collect(),
        None => f.authorized_keys.unwrap_or_default(),
    };
    let use_default_key = if a.no_default_key {
        false
    } else {
        f.use_default_key.unwrap_or(true)
    };
    Ok(Config {
        ssh_addr: a
            .ssh_addr
            .or(f.ssh_addr)
            .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], proto::DEFAULT_SSH_PORT))),
        advertise_host: a
            .host
            .or(f.host)
            .unwrap_or_else(|| proto::DEFAULT_SERVER.to_string()),
        advertise_port: a.port.or(f.port).unwrap_or(proto::DEFAULT_SSH_PORT),
        web_addr: a.web.or(f.web),
        web_url: a.web_url.or(f.web_url),
        max_sessions: a
            .max_sessions
            .or(f.max_sessions)
            .unwrap_or(proto::DEFAULT_MAX_SESSIONS),
        max_viewers: a
            .max_viewers
            .or(f.max_viewers)
            .unwrap_or(proto::DEFAULT_MAX_VIEWERS),
        rate_per_min: a
            .rate_limit
            .or(f.rate_limit)
            .unwrap_or(proto::DEFAULT_RATE_PER_MIN),
        record_dir: a.record_dir.or(f.record_dir),
        record_all: a.record_all || f.record_all.unwrap_or(false),
        record_max_age_days: a
            .record_max_age_days
            .or(f.record_max_age_days)
            .unwrap_or(proto::DEFAULT_RECORD_MAX_AGE_DAYS),
        record_max_mb: a
            .record_max_mb
            .or(f.record_max_mb)
            .unwrap_or(proto::DEFAULT_RECORD_MAX_MB),
        authorized_keys,
        use_default_key,
        downloads_dir: a.downloads.or(f.downloads),
    })
}

// --- host --------------------------------------------------------------------

#[derive(clap::Args)]
#[command(after_long_help = "EXAMPLES:\n  \
    Share your shell on the default relay:\n      choom host\n  \
    Point at your own relay and record the session:\n      \
    choom host -s relay.example.com --record\n  \
    Use a specific identity on a private relay:\n      \
    choom host -s relay.example.com --no-default-key -i ~/.ssh/id_ed25519")]
struct HostArgs {
    /// YAML config file (CLI flags override its values).
    #[arg(long, short = 'c')]
    config: Option<PathBuf>,
    /// Relay hostname [default: choom.sh].
    #[arg(long, short = 's', env = "CHOOM_SERVER")]
    server: Option<String>,
    /// Relay SSH port [default: 2200].
    #[arg(long, short = 'p', env = "CHOOM_PORT")]
    port: Option<u16>,
    /// Shell to run (defaults to $SHELL, then /bin/sh).
    #[arg(long)]
    shell: Option<String>,
    /// Ask the relay to record this session.
    #[arg(long)]
    record: bool,
    /// Identity key file to authenticate with (overrides the default key).
    #[arg(long, short = 'i')]
    identity: Option<PathBuf>,
    /// Do not use the bundled default key (use -i or a persisted per-user key).
    #[arg(long)]
    no_default_key: bool,
    /// Verify the relay's host-key fingerprint (`SHA256:…`) on connect.
    #[arg(long)]
    fingerprint: Option<String>,
    /// Detached "remote-support" mode: share the shell to viewers without attaching
    /// your local terminal (print the links, then relay; the shell is driven remotely).
    #[arg(long)]
    detach: bool,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct HostFile {
    server: Option<String>,
    port: Option<u16>,
    shell: Option<String>,
    record: Option<bool>,
    identity: Option<PathBuf>,
    use_default_key: Option<bool>,
    fingerprint: Option<String>,
    detach: Option<bool>,
}

fn resolve_host(a: HostArgs) -> Result<HostConfig> {
    let f: HostFile = load_file(a.config.as_deref())?;
    let use_default_key = if a.no_default_key {
        false
    } else {
        f.use_default_key.unwrap_or(true)
    };
    Ok(HostConfig {
        server: a
            .server
            .or(f.server)
            .unwrap_or_else(|| proto::DEFAULT_SERVER.to_string()),
        port: a.port.or(f.port).unwrap_or(proto::DEFAULT_SSH_PORT),
        shell: a.shell.or(f.shell),
        record: a.record || f.record.unwrap_or(false),
        identity: a.identity.or(f.identity),
        use_default_key,
        fingerprint: a.fingerprint.or(f.fingerprint),
        detach: a.detach || f.detach.unwrap_or(false),
    })
}

// --- clients -----------------------------------------------------------------

#[derive(clap::Args)]
#[command(after_long_help = "EXAMPLES:\n  \
    List the viewers of your active session:\n      choom clients")]
struct ClientsArgs {
    /// Identity key used to create the session (must match the host's key).
    #[arg(long, short = 'i')]
    identity: Option<PathBuf>,
    /// The host did not use the bundled default key.
    #[arg(long)]
    no_default_key: bool,
}

// --- shared ------------------------------------------------------------------

fn load_file<T: Default + for<'de> Deserialize<'de>>(path: Option<&std::path::Path>) -> Result<T> {
    match path {
        None => Ok(T::default()),
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .with_context(|| format!("read config {}", p.display()))?;
            serde_yaml::from_str(&text).with_context(|| format!("parse config {}", p.display()))
        }
    }
}
