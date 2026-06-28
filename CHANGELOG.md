# Changelog

Notable changes, newest first. [Keep a Changelog](https://keepachangelog.com/) /
[SemVer](https://semver.org/).

## [2.4.0] - 2026-06-28

### Security

- **Web live viewers now share the per-session viewer cap, and the live-stream WebSocket is
  rate limited per source IP.** The browser stream path (`/s/<token>/stream`) attached
  viewers without the `--max-viewers` check the SSH path enforces, and the web listener had
  no rate limit — so anyone holding a read-only token (which is meant to be shared, and is
  embedded in the web URL) could open unbounded live connections and exhaust the relay's
  memory and file descriptors. The cap is now enforced atomically for SSH and web alike
  (one lock, no check-then-act gap), and the stream upgrade is per-IP rate limited.
- **A robustness fuzz test guards the screen-model parse path.** The release binary is built
  `panic = "abort"`, so any panic in a session task aborts the whole multi-tenant relay. A
  new test feeds adversarial and seeded-random host output through `on_output` → `avt` →
  snapshot and asserts it never panics; the trade-off is documented in `docs/ARCHITECTURE.md`.

### Fixed

- **A multibyte UTF-8 character split across two output frames no longer corrupts the server
  screen model.** Output was UTF-8-decoded per frame, so a character split at a frame
  boundary became `U+FFFD` in the join snapshot, the recording, and the web view (the raw SSH
  live path was unaffected). Output is now reassembled across frames, so snapshot, live,
  recording, and web view stay byte-consistent.
- **The recordings registry no longer grows unbounded.** Entries are kept past session end so
  the web download keeps working, but they were never removed — a slow leak over the process
  lifetime. They are now pruned after the hourly retention GC (and when a file disappears).
- **A host resuming exactly at the end of the grace period can no longer be torn down by the
  expiry timer.** The connected-state transition is now serialized with the grace timer.
- **A viewer that falls behind the live broadcast now resyncs** by repainting from the current
  screen (SSH) / re-sending a reset (web), instead of staying permanently corrupted.

### Changed

- The host re-clamps a relay-sent resize to the allowed range (defense in depth).
- The `/download` directory listing is read asynchronously, so a flood of requests can't
  stall the web server's runtime threads.

## [2.3.4] - 2026-06-16

### Fixed

- **The web live view now follows the host's terminal size.** The live stream only sent the
  size once (in the player's reset message), so when the host resized — or a smaller
  read-write viewer joined/left and the relay renegotiated — the browser repainted the new
  screen into the *old* grid and the player never changed dimensions. The live WebSocket now
  emits an asciicast resize event (`[t,"r","<cols>x<rows>"]`) whenever the negotiated size
  changes, just before the repaint, so the player's terminal grows/shrinks with the session.
- **A recording replayed in the browser starts paused** on its first frame instead of
  auto-playing. When a session ends and the page switches to the recording (or the page is
  opened after the session ended), the viewer now presses play themselves. (The *live* view
  still auto-plays — that is required for it to open the stream at all.)

### Changed

- **`choom host` (attached) no longer prints an inline viewer join/leave notice.** Fired into
  the shared raw-mode terminal it raced the shell's own output and was unreliable (sometimes
  missing, sometimes staircased). The live roster is in **`choom clients`** instead; the
  in-place status block in **`choom host --detach`** is unchanged.
- **`choom clients` output is clearer**: a summary line (`N viewers connected (X ssh, Y web)`),
  a ruled header, and human-readable connection ages (`45s`, `5m12s`, `1h02m`).

## [2.3.3] - 2026-06-16

### Fixed

- **A recorded web session that ends while the page is open now switches to replay on its
  own** (previously it kept spinning until a manual reload). The live-vs-replay choice is
  made once at page load; if the session ends afterwards, the live WebSocket just closes and
  the player would spin. The viewer page now polls `/s/<token>/status` while watching live
  and, once the session is gone, disposes the live player and replays the recording in place
  (or shows a short "session has ended" notice when there is no recording). Reloading already
  worked — this makes the open page transition without a reload.

## [2.3.2] - 2026-06-16

### Fixed

- **Host logs no longer staircase in an attached session.** During `choom host` (attached)
  the terminal is in raw mode, where the kernel's LF→CRLF translation is off — so any log
  line written with a bare `\n` left the cursor mid-line and the next output (the shell, or
  the next log) "staircased". The host now installs a CRLF-translating log writer in
  attached mode, so warnings (and anything surfaced via `RUST_LOG`) render at column 0.
  This complements 2.3.1, which already demoted the per-viewer join/leave lines to `debug!`
  in favour of the `\r\n`-correct inline notice — note you must run the **2.3.2 host binary**
  to see the fix (an `INFO choom_host: viewer joined` line means an older binary is running).

## [2.3.1] - 2026-06-16

### Fixed

- **In-browser replay of an ended session now works.** Two causes: (1) recordings were
  written as asciicast **v3**, which the vendored asciinema-player (a v2 build) cannot
  parse from a URL — so the replay just spun forever. Recordings are now **asciicast v2**
  (absolute timestamps), still read by `asciinema play` **and** replayable in the browser.
  (2) The viewer page decided live-vs-replay by opening a throwaway *probe* WebSocket that
  connected and immediately disconnected — which churned the host's viewer roster (a
  join → leave → join flurry in the host log). The page now reads a single
  `GET /s/<token>/status` → `{ live, recording }` and picks live or replay from that.
- **Download-recording button** is anchored to the terminal box. The player letterboxes
  with `fit: 'both'`, so the button used to float in the black margin to the right of the
  terminal; it is now placed at the terminal's top-right (and re-placed on resize).
- **Host viewer logs no longer staircase.** `viewer joined` / `viewer left` were `info!`
  lines whose bare `\n` laddered across the host's raw-mode terminal. They are now
  `debug!`; the user-facing signal is the existing `\r\n`-correct inline notice (attached)
  / live status block (`--detach`).

## [2.3.0] - 2026-06-16

### Added

- **`choom view <token | token@host>`** — a convenience viewer that fills in the right
  `-p <port>` and execs the real `ssh` binary, which also sidesteps shell wrappers like
  kitty's `ssh` kitten. The token's own read-only/read-write grant decides the access
  mode; stock `ssh <token>@host` keeps working unchanged.
- **Read-only sessions** (`choom host --read-only`) — the relay issues only the read-only
  link; the read-write token is never advertised and is mapped to read-only access, so
  even a leaked rw token can only watch (a hard guarantee, not mere non-disclosure).
- **Named sessions** (`choom host --name/-n`) — a human label shown on the host banner, in
  the relay logs, and by `choom clients` (persisted in the session file).
- **Live viewer status** — a transient join/leave notice with the current viewer count
  when the host terminal is attached, an in-place status block in `--detach` mode, and
  `choom clients --watch` which refreshes the roster like `watch(1)`.
- **In-browser replay after a session ends** — the web viewer page probes the live stream
  and, when the session is over, replays the recording (if one exists) in place instead of
  going black.
- **`GET /healthz`** on the web listener — `200` while serving, `503` once the relay is
  draining (SIGTERM); usable as an HTTP readiness/liveness probe.

### Changed

- **`/metrics` moved off the public web port** onto its own operator-internal listener
  (`--metrics-addr` / `CHOOM_METRICS_ADDR`, off by default; bind to loopback/a private
  interface). It is no longer served on the `-w` web view. **Action:** scrapers must point
  at the new listener.

### Security

- **Web file downloads stream from disk** (`/s/<token>/cast`, `/download/{file}`) instead
  of being read whole into memory, and share a bounded concurrency pool — so the web view
  can't be turned into a memory/bandwidth amplifier (a recording has no per-file size cap).
- **Stored-XSS guard on the `/download` index**: filenames are HTML-escaped, and
  `/download/{file}` names are whitelisted to `[A-Za-z0-9._-]` (also blocking path
  traversal and `Content-Disposition` / CR-LF header injection).
- **CSP tightened** with `object-src 'none'`, `base-uri 'none'`, `frame-ancestors 'none'`.
- **Rate-limiter map bounded**: stale per-IP entries are swept so a churn of source IPs
  can't grow it without limit (slow memory DoS).
- Moving `/metrics` off the public web port also closes its unauthenticated exposure
  there (see Changed).

## [2.2.0] - 2026-06-15

### Added

- **Download button on the web viewer** — when the session was recorded, the viewer page
  shows a "Download recording" button (linking to `/s/<token>/cast`). It appears only when
  a recording exists (the page probes `/cast` with `HEAD`), so unrecorded sessions show no
  dead button. The recording stays downloadable **after the session ends** (resolved via a
  persistent token→recording index, not the live session) until it's GC'd off disk.

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
