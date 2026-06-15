# choom

**Minimal SSH terminal sharing relay.** A host shares its local shell through a
central SSH relay; viewers connect with **stock `ssh`** over a read-only or
read-write link — no client to install. Optional: a read-only browser view and
asciicast recording. The relay only forwards bytes and never runs foreign code.

Rust · GPL-3.0-or-later · [docs](docs/)

> _choom_ (also _choomba_/_choombatta_): Neo-Afro-American slang for a friend or
> family member — Cyberpunk 2077.

> **2.0** is a clean break from the Python 1.x line — a 2.0 host needs a 2.0 relay
> (the wire protocol changed). See the [CHANGELOG](CHANGELOG.md) for what changed.

- [Quick start](#quick-start)
- [choom vs. tmate](#choom-vs-tmate)
- [Features](#features)
- [How it works](#how-it-works)
- [CLI](#cli)
- [Recordings · Config](#recordings--config)
- [Security](#security)
- [Build · Test · Dev](#build--test--dev)

## Quick start

Build the binary (a single `choom` for all three roles):

```sh
cargo build --release        # -> target/release/choom
```

Share your shell on a relay (host prints the share links):

```sh
choom host -s your.relay
```

```
Welcome, Choomba, your shell is now shared!
Share a link below; viewers connect with their ssh client.

  Read-only:  ssh -p 2200 <ro-token>@your.relay
  Read-write: ssh -p 2200 <rw-token>@your.relay  (full shell access!)
  Web (RO):   https://your.relay/s/<ro-token>
```

The banner prints the exact `ssh` command. `choom host` dials `choom.sh` on the standard
SSH port 22; any other relay on choom's default **2200** (override with `-p`).

Run your own relay:

```sh
# public-style: trusts the bundled default key, so anyone with the client can host
choom server -H your.relay -w 0.0.0.0:8080

# private: gate hosts by your own keys
cat ~/.ssh/id_ed25519.pub >> authorized_keys
choom server -H your.relay --no-default-key -a authorized_keys
choom host   -s your.relay --no-default-key -i ~/.ssh/id_ed25519
```

See who's watching, from another terminal on the host:

```sh
choom clients
```

See [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md) for running the relay as a service.

## choom vs. tmate

choom is a lean alternative to [**tmate**](https://github.com/tmate-io/tmate). tmate
is a full tmux fork; choom keeps only the sharing core.

| | **choom** | **tmate** |
|---|---|---|
| Core | sharing relay only | tmux fork (full multiplexer) |
| Implementation | Rust (single static binary) | ~50k LOC C |
| Server deps | none special (pure byte relay) | libssh, libevent, msgpack |
| Server privilege | **unprivileged** | root + `SYS_ADMIN` (session jailing) |
| Viewer | stock `ssh` / browser | stock `ssh` / browser |
| Panes / windows / copy-mode | no — run tmux *inside* | yes |
| Web view | read-only | read-only + read-write |

## Features

- Read-only **and** read-write links (RW = full shell access), many simultaneous
  viewers with a synchronized view.
- **Correct repaint** on join/resize via a server-side screen model, and
  **resize negotiation** (RO/web never shrink the host — no resize DoS).
- **Reconnect**: brief host network drops are survived (grace period, owner-bound
  resume); the shell keeps running and viewers stay attached.
- **Web (output-only)** browser view served as a live [asciicast](https://docs.asciinema.org/)
  stream (asciinema-player); landing page at `/`.
- **asciicast v3 recording** (opt-in per session), with age/size retention.
- **`choom clients`** — list the viewers of your session (mode, transport, IP, size).
- **Public-relay hardening**: per-IP rate limiting, session + viewer caps;
  structured (JSON) logging and a Prometheus `/metrics` endpoint.

## How it works

The shell runs on the **host**; the relay forwards bytes (no jailing, runs
unprivileged). The host opens **one** SSH connection carrying a single typed, framed
channel ([`postcard`](https://docs.rs/postcard)-encoded frames: output, input,
resize, control). Viewers connect with the **token as the SSH username**. A
per-session [`avt`](https://docs.rs/avt) screen model (asciinema's terminal emulator)
lets new viewers get a correct snapshot of the current screen (even mid-vim) before
the live stream — and backs the recording and live web view from the same source.

Full design: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Viewing a session

- **Use plain `ssh`** (`ssh <token>@host`). Terminal *ssh wrappers that bootstrap the
  remote shell* — notably **kitty's `ssh` kitten** — can't work as viewers: they upload
  terminfo / shell-integration data through the terminal at startup, assuming they
  control the remote shell's launch, which a relay (forwarding an already-running shell)
  can't provide. choom detects such clients (they request `exec`, or inject a known
  setup payload) and **disconnects that viewer with a hint** — so they can't flood the
  host — but you still need plain `ssh` to view. If your `ssh` is aliased to the kitten,
  call the real binary (e.g. `\ssh` or `/usr/bin/ssh`).
- **Port**: connect on the relay's SSH port. The share banner prints the exact `ssh`
  command — it includes **`-p <port>`** whenever the relay isn't on the standard SSH
  port (22). The public **`choom.sh` is reachable on port 22**, so `ssh <token>@choom.sh`
  needs no `-p`; a self-hosted relay on `2200` is `ssh -p 2200 <token>@host`. (Without the
  right port, `ssh <token>@host` hits whatever sshd is on port 22 and asks for a password.)
- **Shared size**: the terminal is sized to the **smallest read-write viewer** so
  everyone sees the whole screen, so the host's shell repaints (and its cursor moves)
  when a smaller read-write viewer joins. Read-only/web viewers never resize the host.

## CLI

`choom <server|host|clients>`. Every flag also reads an env var and can come from a
YAML file (`-c file.yaml`); precedence is **CLI flag > file > default**.

```
choom server [-H host] [-p port] [-w addr] [-W url]
             [-a authorized_keys] [--no-default-key]
             [-r dir] [--record-all] [--max-sessions N] [--max-viewers N] [--rate-limit N]
choom host   [-s server] [-p port] [--shell SH] [--record] [--detach] [-v]
             [-i identity] [--no-default-key] [--fingerprint SHA256:…]
choom clients [-i identity] [--no-default-key]
```

See `choom <cmd> --help` for the full list and env vars, and
[docs/config.yaml](docs/config.yaml) for the file keys.

## Recordings · Config

- **Recordings** — enable storage on the relay (`-r DIR` / `CHOOM_RECORD_DIR`), then
  `choom host --record` (or force all with `--record-all`). asciicast v3, `0600`,
  pruned by age/size. Download at `/s/<ro-token>/cast`; play with `asciinema play`.
- **Config** — any options can come from YAML: `choom server -c choom.yaml` (CLI
  overrides). See [docs/config.yaml](docs/config.yaml).
- **Logging / metrics** — `CHOOM_LOG_FORMAT=json` for structured logs;
  `GET /metrics` exposes Prometheus counters (sessions, viewers, bytes, rejects).

## Security

- **No end-to-end encryption — trust the operator.** The relay handles sessions in
  cleartext, so its operator can both read everything **and inject keystrokes** into
  the host shell. The RO/RW tokens restrict *other viewers*, not the operator. Only
  host on a relay you trust; run a private relay for anything sensitive. (An optional
  native client with true E2E — a blind-forwarder relay — is designed in
  [docs/E2E-CLIENT.md](docs/E2E-CLIENT.md).)
- **Host auth via public key**: the relay trusts a bundled default key (public-relay
  convenience; opt out with `--no-default-key`) plus any `-a authorized_keys`. Viewer
  auth is solely via the token (128-bit, unguessable — no session hijacking).
- **RO viewers cannot break out**: input dropped, TCP/Unix/X11 forwarding and
  `exec`/subsystem denied, the web view is structurally output-only.
- **Hardening**: per-IP rate limit, session + viewer caps; recordings `0600`, opt-in.
  The host can verify the relay's host-key fingerprint (`--fingerprint`) against MITM.

## Build · Test · Dev

- **Build**: `cargo build --release` → `target/release/choom`. A single static musl
  binary is the release target (`docker build .` → ~4 MB `scratch` image).
- **Test**: `cargo test --workspace` (unit + integration: relay core, host↔relay
  e2e, web stream, recorder, reconnect, clients, metrics).
- **Man pages**: `man/choom.1` (+ `choom-server`/`host`/`clients`), generated from the
  CLI with `task man` (`cargo run -p xtask`); each carries usage examples, also shown
  in `choom <cmd> --help`.
- **Dev**: [direnv](https://direnv.net/) loads the flake devShell (cargo, rustfmt,
  clippy, rust-analyzer, go-task); a [Taskfile](https://taskfile.dev) wraps
  `build`/`test`/`check`/`fmt`/`run`/`man`/`image`.

## License

GNU General Public License v3.0 or later — see [LICENSE](LICENSE).
