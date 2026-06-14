# Architecture

choom has three roles and two programs (viewers use stock `ssh`):

```
   host machine                  relay (choom server)         viewer
 ┌──────────────┐  SSH (2 ch.)  ┌───────────────┐   ssh       ┌─────────────┐
 │ choom host   │──control JSON─│  sessions +   │◀──token@srv─│  stock ssh  │
 │  $SHELL in   │──data  (raw)─▶│  pyte screen  │             │  (ro / rw)  │
 │  a local PTY │◀─data (keys)──│  + recorder   │──pty out───▶│             │
 └──────────────┘               │  + web        │   HTTP/WS   ┌─────────────┐
                                └───────────────┘────────────▶│ browser RO  │
```

**The shell runs on the host.** The relay only forwards bytes — it never executes a
shell, so it needs no chroot/namespace jailing and runs unprivileged.

## Transport

The host opens one SSH connection to the relay with **two channels** (distinguished
by the SSH "command"):

- **control** (`choom-control`) — newline-delimited JSON frames, bidirectional.
- **data** (`choom-data`) — raw bytes: host→relay is the shell's PTY output,
  relay→host is keystrokes from read-write viewers.

Viewers use an ordinary interactive SSH session; the **token is the SSH username**.
Roles are decided by the username: `new`/`resume`/`query` are host operations
(public-key auth); anything else is a viewer token.

See [`../src/choom/protocol.py`](../src/choom/protocol.py) for the full frame schema.

## Server-side screen model

A relay that only forwarded raw bytes would render incorrectly for a viewer who
joins mid-session (alternate-screen apps like vim/htop, partial escape sequences).
So each session feeds the host output into a **pyte** screen model. On join or
resize the relay renders a clean ANSI **snapshot** of the current screen and sends
it first, then streams live output. This is the one place choom needs a terminal
emulator — but only the screen grid, not tmux's multiplexing.

## Resize negotiation

The host PTY is sized to the smallest *active read-write ssh viewer* plus the host's
own terminal (RO and web viewers never shrink it — no resize DoS). With no viewers,
the host uses its native size. Sizes are clamped to `[20×5 .. 1000×1000]`.

## Reconnect

If the host's connection drops, the session survives for a grace period (60 s):
tokens stay valid, viewers see "host disconnected", and the host can resume with its
`reconnect_id` (bound to the original owner public key). After the grace period the
session is dropped.

## Web view

`choom server -w` serves a **read-only** browser view (xterm.js): the WebSocket
pushes the snapshot + live output and a size message; the page scales the terminal
grid to fit the window. It is structurally output-only (the WS never reads inbound
frames). Auth is the `ro_token` in the URL — no cookies. A landing page lives at
`/`, prebuilt binaries at `/download/`, and the asciicast at `/s/<ro-token>/cast`.

## Recording

Recording is opt-in per session (`choom host --record`) and requires the relay to
have storage enabled (`-r`). Output is written as [asciicast v2](https://docs.asciinema.org/manual/asciicast/v2/)
(`0600`), pruned by age and total size.

## Why no tmux

choom keeps only the *sharing* core. Panes, windows, copy-mode, config, and HTML5
rendering — the bulk of a tmux fork like tmate — are out of scope. You get
panes/windows by running tmux *inside* the shared shell.
