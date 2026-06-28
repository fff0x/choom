# Architecture

choom has three roles and one binary (`choom server|host|clients`); viewers use
stock `ssh`:

```
   host machine                  relay (choom server)         viewer
 ┌──────────────┐  SSH (1 ch.)  ┌────────────────┐   ssh      ┌─────────────┐
 │ choom host   │── Frame ──────│  sessions +    │◀──token@srv─│  stock ssh  │
 │  $SHELL in   │  (postcard)   │  avt screen    │             │  (ro / rw)  │
 │  a local PTY │◀── Frame ─────│  + recorder    │──pty out───▶│             │
 └──────────────┘               │  + web         │   WS        ┌─────────────┐
                                └────────────────┘────────────▶│ browser RO  │
```

**The shell runs on the host.** The relay only forwards bytes — it never executes a
shell, so it needs no chroot/namespace jailing and runs unprivileged.

## Crates

```
crates/choom-proto    wire frames + postcard codec, tokens, size clamp, constants
crates/choom-server   russh server, sessions, avt screen, axum web, recorder, metrics
crates/choom-host     russh client, pty-process shell relay, reconnect loop
crates/choom-cli      the `choom` binary (clap dispatch + config layering)
```

## Transport

The host opens **one** SSH connection to the relay and a single session channel,
over which it speaks a typed, length-delimited, [`postcard`](https://docs.rs/postcard)-
encoded frame protocol (`crates/choom-proto`):

- `Frame::Output(bytes)` — host → relay: the shell's PTY output.
- `Frame::Input(bytes)` — relay → host: keystrokes from read-write viewers.
- `Frame::Control(..)` — both ways: `Hello`/`Resume`/`Query`, `Ready`, `Resize`,
  `HostSize`, `Join`/`Leave`/`Clients`, `Record`, `Error`.

(This replaces 1.x's two channels + newline-JSON with one schema'd stream.)

Viewers use an ordinary interactive SSH session; the **token is the SSH username**.
Roles are decided by the username: `new`/`resume`/`query` are host operations
(public-key auth, gated by the trusted key set); anything else is a viewer token,
validated in `auth_publickey` (unknown token → auth fails).

## Server-side screen model

A relay that only forwarded raw bytes would render incorrectly for a viewer who
joins mid-session (alternate-screen apps like vim/htop, partial escape sequences).
So each session feeds the host output into an [`avt`](https://docs.rs/avt) screen
model — asciinema's terminal emulator. On join or resize the relay renders a clean
ANSI **snapshot** and sends it first, then streams live output. The same `avt` model
backs the recording and the live web view, so all three agree by construction.

The live path to stock-`ssh` viewers stays a direct byte-forward (no added latency):
output bytes are broadcast immediately and `avt` is fed in parallel for snapshots.

## Resize negotiation

The host PTY is sized to the smallest *active read-write viewer* plus the host's own
terminal (RO and web viewers never shrink it — no resize DoS). With no RW viewers,
the host keeps its native size. Sizes are clamped to `[20×5 .. 1000×1000]`.

## Reconnect

If the host's connection drops, the session survives for a grace period
(`GRACE_PERIOD_SECS`, 60 s): tokens stay valid and viewers stay attached. The host
runs a reconnect loop — the PTY/shell keep running — and resumes with its
`reconnect_id` (bound to the original owner public key). On resume the relay swaps the
session's host channel to the new connection and repaints viewers. After the grace
period without a resume, the session is dropped.

## Web view

`choom server -w ADDR` serves a **read-only** browser view (axum) rendered by a
**self-hosted** [asciinema-player](https://docs.asciinema.org/) (vendored at `/assets`,
no CDN). The WebSocket at `/s/<ro-token>/stream` speaks the player's live format — a
reset message carrying `{cols, rows, time, init}` then absolute-time `[t,"o",data]`
events (a bespoke live format, separate from the recording's file format). It is
structurally output-only (the WS ignores all inbound frames). Auth is the token in the
URL — no cookies; responses carry hardening headers (`Referrer-Policy: no-referrer`,
`X-Content-Type-Options`, `X-Frame-Options`, CSP — `object-src/base-uri/frame-ancestors
'none'`). The viewer page reads `/s/<ro-token>/status` (`{live, recording}`) once to pick
between watching live and replaying the recording in place after the session ends. Other
routes: landing `/`, recording `/s/<ro-token>/cast`, prebuilt binaries `/download`,
health `/healthz`. File routes (`/cast`, `/download/{file}`) stream from disk (never read
whole into memory) and share a bounded download-concurrency slot pool, so the web view
can't be made a memory/bandwidth amplifier; `/download/{file}` names are whitelisted to
`[A-Za-z0-9._-]`. Metrics are not on this listener (see below).

## Recording

Recording is opt-in per session (`choom host --record`, sent as a `Record` control
frame) and requires the relay to have storage enabled (`-r DIR`); `--record-all`
forces it. Output is written as [asciicast v2](https://docs.asciinema.org/) (`0600`),
pruned hourly by age and total size. v2 (absolute timestamps) is what both `asciinema
play` and the vendored browser player can replay — so the same `.cast` plays on the CLI
and in the web viewer.

## Observability

`tracing` structured logs (`CHOOM_LOG_FORMAT=json` for pod-log aggregation) — no full
tokens logged. A Prometheus recorder exposes `/metrics` on its own operator-internal
listener (`--metrics-addr`, off by default; keep it off the public web port and bind it
to loopback/a private interface): sessions created/active, viewers joined/active, bytes
relayed, tokens rejected, rate-limit and untrusted-host drops. Graceful drain on `SIGTERM` (stop accepting, end sessions, exit) for clean
rolling restarts.

## Failure model

The release profile is `panic = "abort"` (a single static musl binary is the distribution
goal, and `panic` can only be set per-profile, not per-crate). On a multi-tenant relay that
means **any** panic in a session/connection task aborts the whole process and drops every
session — there is no per-task isolation (and `catch_unwind` can't help under `abort`). This
is an accepted trade-off for binary size. The one attacker-reachable parsing path — host
output fed into the `avt` screen model and rendered to a snapshot — is guarded by a
robustness fuzz test (`crates/choom-server/tests/avt_fuzz.rs`) that feeds adversarial and
seeded-random bytes through `on_output` → `avt` → `render::snapshot` and asserts it never
panics. If task-level isolation is ever required, switch the server build to `panic =
"unwind"` and wrap the per-session tasks in `catch_unwind`.

## Why no tmux

choom keeps only the *sharing* core. Panes, windows, copy-mode, config, and HTML5
rendering — the bulk of a tmux fork like tmate — are out of scope. You get
panes/windows by running tmux *inside* the shared shell.
