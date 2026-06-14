# choom

`choom` = Cyberpunk-2077-Slang for **friend**: A lean reimplementation of the tmate
sharing core in Python (~1k LOC). A host shares its local shell through a central
SSH relay; viewers connect with **stock `ssh`** over a read-only or read-write
link. Optionally: a read-only browser view and asciicast recording.

No tmux fork, no `libssh`/`msgpack`, no jailing — the server only relays bytes and
never runs foreign code.

> _choom_ (also _choomba_/_choombatta_): Neo-Afro-American slang for a friend or
> family member — Cyberpunk 2077.

## Architecture

```
   host machine                  central server               viewer
 ┌──────────────┐  SSH (2 ch.)  ┌───────────────┐   ssh       ┌─────────────┐
 │ host.py      │──control JSON─│  server.py    │◀──token@srv─│  stock ssh  │
 │  $SHELL in   │──data  (raw)─▶│  sessions +   │             │  (ro / rw)  │
 │  a local PTY │◀─data (keys)──│  pyte screen  │──pty out───▶│             │
 └──────────────┘               │  + recorder   │   HTTP/WS   ┌─────────────┐
                                │  + web        │────────────▶│ browser RO  │
                                └───────────────┘             └─────────────┘
```

- **The shell runs on the host** (the real tmate model). The server is a pure
  relay → no code execution, no chroot/namespaces needed.
- **pyte** keeps a server-side screen model per session. New viewers get a correct
  repaint of the current screen (even for full-screen apps like vim/htop) and
  resizes are redrawn cleanly.
- **Viewers need no client of their own** — `ssh <token>@server` is enough.

## Project layout

```
choom/
  pyproject.toml          # package + console script `choom`  (GPL-3.0-or-later)
  LICENSE                 # GNU GPL v3
  src/choom/
    cli.py                # `choom server|host|clients` dispatcher
    server.py             # SSH relay, sessions, pyte screen, resize, reconnect
    host.py               # host client: local PTY <-> server, reconnect loop
    web.py + static/      # read-only browser view (xterm.js) + .cast download
    recorder.py           # asciicast v2 capture per session
    clients.py            # `choom clients` -- list active viewers
    protocol.py           # shared constants + control-frame schema
    default_id            # bundled default private key (see Security)
  tests/                  # pytest suite (unit + integration + web + reconnect)
```

## Setup

```sh
python3 -m venv .venv
.venv/bin/pip install -e .          # installs the `choom` command + deps
# for the tests as well:  .venv/bin/pip install -e '.[test]'
```

## Usage

A single command with subcommands: `choom server | host | clients`. By default
everything targets the public relay **`choom.this-is-fine.io`** and the host
authenticates with the bundled default key, so the simplest case is just:

```sh
choom host          # share your shell on choom.this-is-fine.io
```

The host prints the links:

```
[choom] session ready
  Read-only:  ssh <ro-token>@choom.this-is-fine.io
  Read-write: ssh <rw-token>@choom.this-is-fine.io  (full shell access!)
  Web (RO):   http://choom.this-is-fine.io:8080/s/<ro-token>
  server key: SHA256:....
```

**Viewers** just run the relevant command (stock `ssh`).

**List active viewers** (from the host):

```sh
choom clients
```

### Running your own (private) server

```sh
cat ~/.ssh/id_ed25519.pub >> authorized_keys     # who may create sessions

choom server -p 2200 -H your.server.example \
    --no-default-key \   # do not trust the built-in key
    -a authorized_keys \
    -w :8080 \           # optional: web view
    -r recordings        # optional: recording (opt-in)

choom host -s your.server.example -p 2200 -i ~/.ssh/id_ed25519 --no-default-key
```

(The server auto-generates `host_key` on first start.)

## Features

- Read-only **and** read-write links (RW = full shell access).
- Multiple simultaneous viewers, synchronized view.
- **Resize negotiation**: the smallest _active RW viewer_ + the host wins (RO/web
  never shrink → no resize DoS). With no viewers the host uses its native size.
- **Correct repaint** on join/resize via the pyte screen model.
- **Reconnect**: brief host network drops are survived (grace period, tokens stay
  valid, bound to the owner key).
- **Web (output only)**: live browser view, structurally without an input path.
- **asciicast recording**: one `.cast` file per session, playable with
  `asciinema play` or asciinema-player, downloadable at `/s/<ro-token>/cast`.

## Security

- **Host auth via public key**; viewer auth solely via the token (unknown token →
  auth failure).
- **An RW link = full shell access as the host user.** Share it deliberately; RO
  is the default case.
- For **all** viewers: TCP/Unix/X11 forwarding and `exec`/subsystem are denied →
  the server cannot be abused as a pivot.
- Web: auth only via token-in-URL (no cookies → CSWSH/CSRF defused),
  `Referrer-Policy: no-referrer`, local assets only, WS strictly output-only.
- Recordings are **opt-in**, created with `0600`, and may contain secrets
  (passwords/keys shown on screen) — use sparingly.
- **No end-to-end:** the relay sees cleartext. Intended for self-hosting with a
  trusted operator; put a TLS reverse proxy in front of the web port.
- **Built-in default key:** the client ships with a private key (`default_id`)
  that the public server trusts by default, so anyone can host a session (like
  the public tmate service). Opt out with `--no-default-key` on both sides and use
  your own `authorized_keys` for a private/controlled server. The session tokens,
  not the host key, are what protect viewing access.

## Tests

```sh
.venv/bin/pip install -e '.[test]'
.venv/bin/python -m pytest          # 35 tests
```

The suite (`tests/`) covers:

- **Unit** (`test_unit.py`): `clamp_size`, `FrameReader` (partial / multiple /
  oversize / malformed frames), `render_screen`, token lookup, resume owner
  binding, bundled default key.
- **Integration** (`test_integration.py`): RO visibility, RW input, RO dropping,
  unknown token, forwarding denial, multiple viewers, resize negotiation (RW
  shrinks, RO does **not**, host uses native size without viewers).
- **Default key** (`test_default_key.py`): trusted by default, rejected with
  `--no-default-key`.
- **Web** (`test_web.py`): RO stream content, rw token → 404, asciicast download,
  security headers.
- **Reconnect** (`test_reconnect.py`): happy path (viewer survives host drop +
  resume) plus rejection on wrong owner key / wrong id.
- **Recording** (`test_recording.py`): `.cast` perms `0600`, header, output and
  resize events.

## Deliberately out of scope (vs. tmate)

Viewer-side copy-mode/scrollback, an RW web client, `~/.tmate.conf`, named/stable
URLs/API, end-to-end encryption. You get panes/windows by simply running tmux
_inside_ the shared shell.

## License

GNU General Public License v3.0 or later — see [LICENSE](LICENSE).
