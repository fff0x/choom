# Changelog

Notable changes, newest first. [Keep a Changelog](https://keepachangelog.com/) /
[SemVer](https://semver.org/).

## [2.2.0] - 2026-06-15

### Added

- **Download button on the web viewer** — when the session was recorded, the viewer page
  shows a "Download recording" button (linking to `/s/<token>/cast`). It appears only when
  a recording exists (the page probes `/cast` with `HEAD`), so unrecorded sessions show no
  dead button.

### Fixed

- **Web view renders at all** — the actual root cause of the black/garbled web view:
  asciinema-player runs its VT engine as **WebAssembly**, which the relay's
  `Content-Security-Policy` blocked (`script-src` lacked `'wasm-unsafe-eval'`). The
  browser refused `WebAssembly.instantiate()`, so the terminal never drew. Added the
  narrow `'wasm-unsafe-eval'` directive (allows WASM only, not general `eval`).
- **Web live view stays live.** The stream now opens with a single asciinema-player
  *reset* message (`{cols, rows, time, init}`) carrying the current screen as `init` and a
  numeric clock, instead of a bare `{cols,rows}` header plus the snapshot as a `t=0` event.
  The old shape made the player treat the stream as a finite recording — it played to the
  end and showed a paused/replay state (black screen + play button, garbled on replay)
  rather than a continuous live terminal.

### Changed

- **Default relay port depends on the server**: `choom host` with no `-p` dials the public
  `choom.sh` on the standard SSH port 22, and any other relay on choom's own default 2200.
  `-p` (or `CHOOM_PORT`) overrides. So `choom host` "just works" against `choom.sh` and the
  share links it advertises are portless, while self-hosted relays stay on 2200.

### Added

- The relay logs its **version** on startup (`choom relay starting version=…`).

## [2.1.0] - 2026-06-15

### Fixed

- **Share links**: the printed `ssh` command now includes `-p <port>` whenever the relay
  isn't on port 22.
- **Tokens** never start with `-` anymore: such a token made `ssh <token>@host` parse the
  token as an option flag, breaking the share link.

- **Web view**: live sessions render again instead of a black screen. The
  asciinema-player was created without `autoPlay`, so it showed a black poster and
  never even opened the WebSocket; it now auto-plays. The player container is also given
  a height so `fit: 'both'` fills the window rather than collapsing to nothing.
- **Host**: exiting the shell (`exit` / `Ctrl-D`) now quits immediately. The Tokio
  runtime drop blocked on the uncancellable `tokio::io::stdin()` blocking read, so the
  process hung after the shell exited until the next keypress. Blocking threads are now
  abandoned on shutdown.
- **Host**: a shell that exits while the relay connection is down or flapping no longer
  leaves the host looping `relay connection lost; reconnecting…` forever. Shell exit is
  now tracked by a dedicated watcher, and the reconnect path stops once the local shell
  is gone (there is nothing left to share).
- **Viewers detach immediately when the host's shell exits.** The host now sends a `Bye`
  control frame on shell exit, so the relay ends the session at once instead of holding
  viewers for the full 60s reconnect grace period — a viewer's `ssh` no longer hangs
  after the host quits.
- **Viewers are told when the host disconnects**: a `[choom] host disconnected — waiting
  for reconnect…` notice is shown while the relay waits out the grace window, instead of
  a silently frozen screen.
- **Incompatible viewer clients are disconnected cleanly** instead of corrupting the host.
  A viewer that requests `exec` (a remote command) or injects a client-bootstrap payload
  — e.g. **kitty's `ssh` kitten**, which uploads terminfo/shell-integration as input — is
  refused with a hint to use plain `ssh`, via an extensible input blocklist. The host and
  other viewers are unaffected. (Verified end to end against kitty 0.44.)

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
