# choom

**Minimal SSH terminal sharing relay.** A host shares its local shell through a
central SSH relay; viewers connect with **stock `ssh`** over a read-only or
read-write link — no client to install. Optional: a read-only browser view and
asciicast recording. The relay only forwards bytes and never runs foreign code.

~1k LOC of Python · GPL-3.0-or-later · [docs](docs/)

> _choom_ (also _choomba_/_choombatta_): Neo-Afro-American slang for a friend or
> family member — Cyberpunk 2077.

- [Quick start](#quick-start)
- [choom vs. tmate](#choom-vs-tmate)
- [Features](#features)
- [How it works](#how-it-works)
- [Recordings · Config · Binary](#recordings--config--binary)
- [Security](#security)
- [Docs · Tests · Dev](#docs--tests--dev)

## Quick start

Install the client (Nix or pip) and share your shell on the public relay
`choom.this-is-fine.io`:

```sh
nix run git+https://seed.this-is-fine.io/zc53mExb6ttoNQXMMZh9xxn5n2HH.git -- host
# or:  pip install choom && choom host
```

The host prints links; viewers run the `ssh` command (or open the web link):

```
Welcome, Choomba, your shell is now shared!
Share a link below; viewers connect with their ssh client.

  Read-only:  ssh <ro-token>@choom.this-is-fine.io
  Read-write: ssh <rw-token>@choom.this-is-fine.io  (full shell access!)
  Web (RO):   https://choom.this-is-fine.io/s/<ro-token>
```

Run your own private relay (don't trust the built-in key; gate hosts by your keys):

```sh
cat ~/.ssh/id_ed25519.pub >> authorized_keys
choom server -H your.host --no-default-key -a authorized_keys -w 0.0.0.0:8080
choom host   -s your.host -i ~/.ssh/id_ed25519 --no-default-key
```

See [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md) for container / Kubernetes / NixOS.

## choom vs. tmate

choom is a lean alternative to [**tmate**](https://github.com/tmate-io/tmate). tmate
is a full tmux fork; choom keeps only the sharing core.

| | **choom** | **tmate** |
|---|---|---|
| Core | sharing relay only | tmux fork (full multiplexer) |
| Size | ~1k LOC Python | ~50k LOC C |
| Server deps | none special (pure byte relay) | libssh, libevent, msgpack |
| Server privilege | **unprivileged** | root + `SYS_ADMIN` (session jailing) |
| Viewer | stock `ssh` / browser | stock `ssh` / browser |
| Panes / windows / copy-mode | no — run tmux *inside* | yes |
| Web view | read-only | read-only + read-write |
| Single static binary | yes (Nuitka / musl) | distro packages |

## Features

- Read-only **and** read-write links (RW = full shell access), many simultaneous
  viewers with a synchronized view.
- **Correct repaint** on join/resize via a server-side screen model, and
  **resize negotiation** (RO/web never shrink the host — no resize DoS).
- **Reconnect**: brief host network drops are survived (grace period, owner-bound).
- **Web (output-only)** browser view that scales to the window; landing page at `/`,
  prebuilt binaries at `/download/`.
- **asciicast recording** (opt-in per session), with age/size retention.
- **Public-relay hardening**: per-IP rate limiting, session + viewer caps; operator
  logging of connections.

## How it works

The shell runs on the **host**; the relay forwards bytes (no jailing, runs
unprivileged). The host opens one SSH connection with a JSON **control** channel and
a raw **data** channel; viewers connect with the **token as the SSH username**. A
per-session [pyte](https://pypi.org/project/pyte/) screen model lets new viewers get
a correct snapshot of the current screen (even mid-vim) before the live stream.

Full design: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Recordings · Config · Binary

- **Recordings** — enable storage on the relay (`-r` / `CHOOM_RECORD=1`), then
  `choom host --record`. Download at `/s/<ro-token>/cast`; play with
  `asciinema play session.cast`. Pruned by age/size; `--record-all` records all.
- **Config** — any options can come from YAML: `choom server -c choom.yaml`
  (CLI overrides). See [docs/config.yaml](docs/config.yaml).
- **Binary** — `./tools/build-binaries.sh` builds a portable single-file `choom`
  (Nuitka) into `downloads/`, served at `/download/`. `MODE=nix` uses a Nix bundle.

## Security

- **No end-to-end encryption — trust the operator.** The relay handles sessions in
  cleartext, so its operator can both read everything **and inject keystrokes** into
  the host shell. The RO/RW tokens restrict *other viewers*, not the operator. Only
  host on a relay you trust; run a private relay for anything sensitive.
- **Host auth via public key**; viewer auth solely via the token (128-bit,
  unguessable — no session hijacking). RO viewers cannot break out: input dropped,
  forwarding and `exec`/subsystem denied, web is structurally output-only.
- **Hardening**: per-IP rate limit, session + viewer caps; recordings `0600`, opt-in,
  GC'd. The **public default key** means anyone can host — use `--no-default-key` +
  `authorized_keys` for a private relay, and TLS in front of the web port.

## Docs · Tests · Dev

- **Docs**: [architecture](docs/ARCHITECTURE.md) ·
  [deployment](docs/DEPLOYMENT.md) · [config](docs/config.yaml) ·
  [Rust rewrite plan](docs/RUST-REWRITE-PLAN.md) · man page [`man/choom.1`](man/choom.1).
- **Tests**: `pip install -e '.[test]' && pytest` (50 tests: unit, integration,
  web, reconnect, recording, hardening, config).
- **Dev**: [direnv](https://direnv.net/) loads the flake devShell (`.envrc`); a
  [Taskfile](https://taskfile.dev) wraps `setup`/`test`/`build`/`run`/`image`/`binary`.

## License

GNU General Public License v3.0 or later — see [LICENSE](LICENSE).
