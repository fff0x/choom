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
// The variants differ in size (server takes many flags), but this enum is parsed exactly
// once at startup — the size delta is irrelevant, and boxing would only obscure the args.
#[allow(clippy::large_enum_variant)]
enum Cmd {
    /// Run the central relay.
    Server(ServerArgs),
    /// Share your local shell through a relay.
    Host(HostArgs),
    /// Attach to a shared session as a viewer (a convenience wrapper over `ssh`).
    View(ViewArgs),
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
        Cmd::View(args) => choom_host::run_view(args.target, args.server, args.port, args.ssh_path),
        Cmd::Clients(args) => {
            choom_host::run_clients(args.identity, !args.no_default_key, args.watch)
        }
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
    /// Serve the Prometheus `/metrics` endpoint on this address (omit to disable). Keep it
    /// off the public web view — bind to loopback (e.g. 127.0.0.1:9090) and scrape there.
    #[arg(long, env = "CHOOM_METRICS_ADDR")]
    metrics_addr: Option<SocketAddr>,
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
    /// Persistent SSH host-key file (generated if missing) for a stable fingerprint
    /// across restarts; default is an ephemeral key.
    #[arg(long, env = "CHOOM_HOST_KEY")]
    host_key: Option<PathBuf>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ServerFile {
    ssh_addr: Option<SocketAddr>,
    host: Option<String>,
    port: Option<u16>,
    web: Option<SocketAddr>,
    web_url: Option<String>,
    metrics_addr: Option<SocketAddr>,
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
    host_key: Option<PathBuf>,
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
        metrics_addr: a.metrics_addr.or(f.metrics_addr),
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
        host_key_path: a.host_key.or(f.host_key),
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
    /// Human label for the session (shown on the host, in relay logs, and `choom clients`).
    #[arg(long, short = 'n')]
    name: Option<String>,
    /// Read-only session: only the read-only link is issued (no read-write link).
    #[arg(long)]
    read_only: bool,
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
    /// Print connection diagnostics (resolved relay address, connect attempts, the relay
    /// host-key fingerprint, the auth method).
    #[arg(long, short = 'v')]
    verbose: bool,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct HostFile {
    server: Option<String>,
    port: Option<u16>,
    shell: Option<String>,
    record: Option<bool>,
    name: Option<String>,
    read_only: Option<bool>,
    identity: Option<PathBuf>,
    use_default_key: Option<bool>,
    fingerprint: Option<String>,
    detach: Option<bool>,
    verbose: Option<bool>,
}

fn resolve_host(a: HostArgs) -> Result<HostConfig> {
    let f: HostFile = load_file(a.config.as_deref())?;
    let use_default_key = if a.no_default_key {
        false
    } else {
        f.use_default_key.unwrap_or(true)
    };
    let server = a
        .server
        .or(f.server)
        .unwrap_or_else(|| proto::DEFAULT_SERVER.to_string());
    let port = a.port.or(f.port).unwrap_or_else(|| proto::default_port_for(&server));
    Ok(HostConfig {
        server,
        port,
        shell: a.shell.or(f.shell),
        record: a.record || f.record.unwrap_or(false),
        name: a.name.or(f.name),
        read_only: a.read_only || f.read_only.unwrap_or(false),
        identity: a.identity.or(f.identity),
        use_default_key,
        fingerprint: a.fingerprint.or(f.fingerprint),
        detach: a.detach || f.detach.unwrap_or(false),
        verbose: a.verbose || f.verbose.unwrap_or(false),
    })
}

// --- view --------------------------------------------------------------------

#[derive(clap::Args)]
#[command(after_long_help = "EXAMPLES:\n  \
    Attach with a token (host from -s/default):\n      choom view <token>\n  \
    Attach with a full token@host:\n      choom view <token>@relay.example.com\n\n\
    `choom view` is a convenience over plain `ssh`: it adds the right -p and runs the\n\
    real ssh binary (sidestepping wrappers like kitty's ssh kitten). The token decides\n\
    read-only vs read-write — stock `ssh <token>@host` works just as well.")]
struct ViewArgs {
    /// The share token, or `<token>@<host>`.
    target: String,
    /// Relay hostname when the target is a bare token [default: choom.sh].
    #[arg(long, short = 's', env = "CHOOM_SERVER")]
    server: Option<String>,
    /// Relay SSH port [default: 22 for choom.sh, else 2200].
    #[arg(long, short = 'p', env = "CHOOM_PORT")]
    port: Option<u16>,
    /// Path to the `ssh` binary to exec [default: ssh on PATH].
    #[arg(long)]
    ssh_path: Option<String>,
}

// --- clients -----------------------------------------------------------------

#[derive(clap::Args)]
#[command(after_long_help = "EXAMPLES:\n  \
    List the viewers of your active session:\n      choom clients\n  \
    Keep a live, refreshing roster:\n      choom clients --watch")]
struct ClientsArgs {
    /// Identity key used to create the session (must match the host's key).
    #[arg(long, short = 'i')]
    identity: Option<PathBuf>,
    /// The host did not use the bundled default key.
    #[arg(long)]
    no_default_key: bool,
    /// Refresh the roster every 2s (live dashboard) until interrupted.
    #[arg(long, short = 'w')]
    watch: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn host(args: &[&str]) -> HostConfig {
        match Cli::try_parse_from(args).unwrap().cmd {
            Cmd::Host(a) => resolve_host(a).unwrap(),
            _ => panic!("expected host subcommand"),
        }
    }

    fn server(args: &[&str]) -> Config {
        match Cli::try_parse_from(args).unwrap().cmd {
            Cmd::Server(a) => resolve_server(a).unwrap(),
            _ => panic!("expected server subcommand"),
        }
    }

    /// A unique temp file holding `body`, cleaned up by the returned guard's path parent.
    fn tmp_yaml(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("choom-cli-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.yaml");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn the_cli_parses() {
        // Guards against clap configuration errors (duplicate short flags, etc.).
        Cli::command().debug_assert();
    }

    #[test]
    fn host_defaults() {
        // No server given -> the public choom.sh on the standard SSH port (22).
        let c = host(&["choom", "host"]);
        assert_eq!(c.server, proto::DEFAULT_SERVER);
        assert_eq!(c.port, proto::DEFAULT_SERVER_PORT);
        assert!(c.use_default_key);
        assert!(!c.detach);
        assert!(!c.verbose);
        assert!(!c.record);
    }

    #[test]
    fn host_default_port_depends_on_server() {
        // choom.sh (explicit or implicit) defaults to 22; any other relay to 2200.
        assert_eq!(host(&["choom", "host"]).port, 22);
        assert_eq!(host(&["choom", "host", "-s", "choom.sh"]).port, 22);
        assert_eq!(
            host(&["choom", "host", "-s", "your.relay"]).port,
            proto::DEFAULT_SSH_PORT
        );
        // An explicit -p always wins.
        assert_eq!(host(&["choom", "host", "-p", "2022"]).port, 2022);
        assert_eq!(
            host(&["choom", "host", "-s", "your.relay", "-p", "22"]).port,
            22
        );
    }

    #[test]
    fn host_cli_flags_win() {
        let c = host(&[
            "choom", "host", "-s", "relay.example.com", "-p", "2020", "--detach", "-v", "--record",
            "--no-default-key",
        ]);
        assert_eq!(c.server, "relay.example.com");
        assert_eq!(c.port, 2020);
        assert!(c.detach);
        assert!(c.verbose);
        assert!(c.record);
        assert!(!c.use_default_key);
    }

    #[test]
    fn server_defaults_and_overrides() {
        let d = server(&["choom", "server"]);
        assert_eq!(d.advertise_host, proto::DEFAULT_SERVER);
        assert_eq!(d.advertise_port, proto::DEFAULT_SSH_PORT);
        assert!(d.use_default_key);
        assert!(d.web_addr.is_none());
        assert!(!d.record_all);

        let c = server(&[
            "choom", "server", "-H", "r.example", "-w", "0.0.0.0:8080", "--no-default-key",
            "--record-all",
        ]);
        assert_eq!(c.advertise_host, "r.example");
        assert_eq!(c.web_addr.unwrap().to_string(), "0.0.0.0:8080");
        assert!(!c.use_default_key);
        assert!(c.record_all);
    }

    #[test]
    fn precedence_cli_over_file_over_default() {
        let f = tmp_yaml(
            "prec",
            "server: file-relay\nport: 2222\nverbose: true\n",
        );
        let fp = f.to_str().unwrap();

        // File supplies values where the CLI is silent.
        let from_file = host(&["choom", "host", "-c", fp]);
        assert_eq!(from_file.server, "file-relay");
        assert_eq!(from_file.port, 2222);
        assert!(from_file.verbose);

        // An explicit CLI flag overrides the file; unrelated file keys still apply.
        let overridden = host(&["choom", "host", "-c", fp, "-s", "cli-relay"]);
        assert_eq!(overridden.server, "cli-relay");
        assert_eq!(overridden.port, 2222);

        std::fs::remove_dir_all(f.parent().unwrap()).ok();
    }

    #[test]
    fn config_file_rejects_unknown_keys() {
        let f = tmp_yaml("bad", "bogus_key: 1\n");
        let res = Cli::try_parse_from(["choom", "host", "-c", f.to_str().unwrap()])
            .unwrap()
            .cmd;
        let err = match res {
            Cmd::Host(a) => resolve_host(a),
            _ => unreachable!(),
        };
        assert!(err.is_err(), "unknown YAML keys must be rejected");
        std::fs::remove_dir_all(f.parent().unwrap()).ok();
    }

    #[test]
    fn host_key_resolves_from_flag_or_env_field() {
        let c = server(&["choom", "server", "--host-key", "/var/lib/choom/hk"]);
        assert_eq!(
            c.host_key_path.as_deref(),
            Some(std::path::Path::new("/var/lib/choom/hk"))
        );
    }
}
