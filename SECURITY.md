# Security

A review of choom's threat model, the public-relay deployment, and the hardening
in place. Report issues on the [radicle seed](https://radicle.network/nodes/seed.this-is-fine.io/rad:zc53mExb6ttoNQXMMZh9xxn5n2HH).

## Architecture recap

The shell runs **on the host**; the relay only forwards bytes. The relay never
executes a shell, so it needs no chroot/namespace jailing. Three roles:

- **Host** — runs `choom host`, authenticated by SSH public key.
- **Read-write (RW) viewer** — connects with the rw token; keystrokes reach the
  host shell. **An RW link is equivalent to handing over a shell.**
- **Read-only (RO) viewer** — connects with the ro token (SSH or browser); only
  observes.

## Is the public relay (choom.this-is-fine.io) a security problem?

The public relay trusts a **built-in default key**, so anyone with the standard
client can create a session — this is the public-service model (like tmate.io),
intentional, not a flaw. The real considerations are:

1. **No end-to-end encryption — the operator has full access.** The relay handles
   every session in cleartext, so its operator can both **read** everything and
   **inject keystrokes** into the host shell (the relay forwards viewer input, so a
   malicious relay can originate input too). The RO/RW tokens restrict *other
   viewers*, **not** the operator. Hosting therefore means trusting the operator
   with read+write on your shell. *Inherent to a relay design*; for confidential
   work run a private relay you control.
2. **Recording.** When recording is enabled (it is on the public relay), every
   session is written to disk and may capture on-screen secrets. Mitigated by
   retention GC + `0600` perms; still a privacy consideration → documented.
3. **Resource abuse (DoS).** Anyone can open connections / create sessions →
   hardened with limits (below).

It is **not** a problem in these ways:

- **Session hijacking — no.** Tokens are 128-bit (`secrets.token_urlsafe(16)`),
  unguessable, looked up via dict + `secrets.compare_digest`. There is no
  enumeration or timing oracle. Reconnect requires the original `reconnect_id`
  **and** the same owner public key, so a stolen/guessed id alone cannot take over
  a session.
- **RO → RW escalation / breakout — no.** See below.

## Can you break out of a read-only session?

No. For every viewer connection (SSH and web), choom enforces output-only RO:

- **Input is dropped.** A viewer's stdin is only forwarded to the host when its
  token is the **rw** token; ro keystrokes are discarded (`data_received`).
- **No forwarding / pivot.** TCP, Unix-socket, X11 and remote forwarding are
  denied for *all* connections (`connection_requested`, `server_requested`,
  `unix_*`, `tun/tap` → `False`), so the relay cannot be used as a network pivot.
- **No exec / subsystem.** Viewers may only open the one shared PTY session;
  `exec` and subsystems (sftp, …) are refused.
- **No size griefing.** RO and web viewers are excluded from terminal-size
  negotiation, so a tiny RO client cannot shrink the host's shell. Sizes are
  clamped to `[20×5 .. 1000×1000]`.
- **Web is structurally output-only.** The WebSocket never reads inbound frames;
  the browser cannot send keystrokes at all. Auth is the token-in-URL (no
  cookies), so CSWSH/CSRF have no ambient authority to abuse.

## Hardening in place

- **Per-IP connection rate limiting** — token bucket, default 60 new connections
  per source IP per minute (`--rate-limit`, `0` disables).
- **Global session cap** — default 200 concurrent sessions (`--max-sessions`).
- **Per-session viewer cap** — default 50 viewers (`--max-viewers`).
- **Control-frame size bound** — host control lines are capped (`MAX_FRAME_BYTES`,
  64 KiB) so a malicious host cannot exhaust memory with one frame.
- **Recording retention** — GC by age (default 7 days) and total size (default
  500 MB, oldest first); files created `0600`.
- **SSH** — Ed25519 host key (generated `0600`); unknown tokens fail auth (no
  open pre-auth state); host role requires a public key in `authorized_keys`.
- **Web** — `Referrer-Policy: no-referrer`, `X-Content-Type-Options: nosniff`,
  local assets only, no path traversal on `/download/` or `/s/<token>/cast`.

All limits/retention are tunable via CLI flags or `CHOOM_*` env vars.

## Residual risks (accepted)

| Risk | Mitigation / note |
|------|-------------------|
| Operator can read **and inject** (no E2E) | Trust the operator; run a private relay for secrets |
| Recordings may contain secrets | opt-in, `0600`, GC, retention; disable with `CHOOM_RECORD=0` |
| Public default key = anyone can host | By design; `--no-default-key` + `authorized_keys` for a private relay |
| RW link = full shell | Share deliberately; RO is the default link |

## Running a private / hardened relay

```sh
choom server --no-default-key -a authorized_keys \
  --max-sessions 20 --max-viewers 10 --rate-limit 20
# recording off by default; enable with -r <dir> only if you need it
```

Put a TLS reverse proxy in front of the web port; never expose recordings or the
host key.
