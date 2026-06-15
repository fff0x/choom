# Changelog

Notable changes, newest first. [Keep a Changelog](https://keepachangelog.com/) /
[SemVer](https://semver.org/).

## [2.1.0] - 2026-06-15

### Fixed

- **Web view**: live sessions render again instead of a black screen. The
  asciinema-player was created without `autoPlay`, so it showed a black poster and
  never even opened the WebSocket; it now auto-plays. The player container is also given
  a height so `fit: 'both'` fills the window rather than collapsing to nothing.
- **Host**: exiting the shell (`exit` / `Ctrl-D`) now quits immediately. The Tokio
  runtime drop blocked on the uncancellable `tokio::io::stdin()` blocking read, so the
  process hung after the shell exited until the next keypress. Blocking threads are now
  abandoned on shutdown.

### Notes

- **Post-quantum key exchange**: no change was needed — the relay already negotiates
  `mlkem768x25519-sha256` with modern OpenSSH clients (verified end to end). The earlier
  "not using a post-quantum key exchange" warning came from a stale deployed image.

### Internal

- Expanded test coverage: the previously untested `choom-cli` crate gains config
  precedence / parsing tests, a PTY-driven end-to-end regression test guards the host
  exit-hang, and the web auto-play and viewer `none`-auth paths get regression tests.

## [2.0.1] - 2026-06-15

### Fixed

- **Viewer authentication**: `ssh <token>@relay` now authenticates with the SSH `none`
  method (the username carries the token), so stock `ssh` connects with no key and no
  password. The relay advertises only `none` + `publickey` — a bad token fails cleanly
  with "Permission denied" instead of falling through to a misleading password prompt.
- **Host connect**: each relay connect attempt is capped at 8s (was the ~127s OS
  default), so an unreachable relay no longer hangs on a blank, un-interruptible screen.
  The initial dial retries up to 3× with visible status; raw mode is entered only after
  the first successful connect, so `Ctrl-C` still aborts while dialing.

### Added

- `choom host -v`/`--verbose` — connection diagnostics on the host's screen: the
  resolved relay address(es), each connect attempt, the relay host-key fingerprint, and
  the auth method.

## [2.0.0] - 2026-06-15

Ground-up Rust rewrite. Clean-break wire protocol — a 2.0 host needs a 2.0 relay.
Shipped as one static `choom` binary (`server`/`host`/`clients`), ~3.6 MB.

### Changed (architecture)

- **Transport**: one typed, length-delimited [`postcard`](https://docs.rs/postcard)
  frame channel host↔relay, replacing 1.x's two SSH channels + newline-JSON.
- **Screen model**: [`avt`](https://docs.rs/avt) (asciinema's VT) replaces pyte; one
  model backs the join snapshot, the recording, and the live web view.
- **Web**: live stream to a self-hosted asciinema-player (vendored, gzipped, no CDN),
  output-only. Live wire is the player's format — a top-level `{cols,rows}` header +
  absolute-time `[t,"o",data]` events (not v3-over-WS).
- **Recording**: asciicast v3 files (opt-in, `0600`, age/size GC).
- **Crypto**: russh `ring` backend, EC-only (ed25519/ecdsa, no RSA), no zlib — drops
  the ~720 KB `aws-lc-sys` C library.

### Added

- `choom clients` — viewer roster via a relay query role + a per-host session file.
- **Reconnect/resume** — host reconnect loop (PTY/shell survive); the relay holds the
  session for a grace period and viewers stay attached across the gap.
- CLI flags + env vars + YAML config (`CLI > file > default`).
- Structured JSON logging (`CHOOM_LOG_FORMAT=json`), Prometheus `/metrics`, SIGTERM
  drain.
- Host-key fingerprint verification (`choom host --fingerprint`).
- Detached remote-support mode (`choom host --detach`).
- `authorized_keys` host gating (`-a`) alongside the bundled default key.
- Man pages generated from the CLI (`clap_mangen` via `xtask`); examples also in
  `--help`.

### Security

- Unchanged trust-the-operator model: the relay handles cleartext and can read **and
  inject**; RO/RW tokens restrict other viewers, not the operator. An optional native
  end-to-end-encrypted client is designed in [docs/E2E-CLIENT.md](docs/E2E-CLIENT.md).

## 1.x

1.x was a Python implementation (asyncssh + pyte + aiohttp), preserved on `main`
(tag `1.0.0`) as the functional reference. 2.0 does not interoperate with it.
