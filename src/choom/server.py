#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""choom server -- central SSH relay for shared terminals.

Roles are distinguished by the SSH username:
  * "new"/"resume"/"query"  -> host role, public-key required (authorized_keys).
  * anything else           -> the username IS a session token (viewer).

The server never runs a shell itself -- it only relays bytes. That removes any
need for the jailing (chroot/namespaces) that tmate needs on the server side.
"""

from __future__ import annotations

import argparse
import asyncio
import os
import secrets
import sys
import time

import asyncssh
import pyte

from . import __version__
from . import protocol as P
from .recorder import Recorder, gc as record_gc


# ---------------------------------------------------------------------------
# pyte screen -> ANSI (for repaint on join / resize)
# ---------------------------------------------------------------------------
_FG = {"black": 30, "red": 31, "green": 32, "brown": 33, "blue": 34,
       "magenta": 35, "cyan": 36, "white": 37, "default": 39}


def _is_hex(s: str) -> bool:
    return len(s) == 6 and all(c in "0123456789abcdefABCDEF" for c in s)


def _sgr(char) -> str:
    """SGR parameters for a pyte char (colors/attributes)."""
    p = ["0"]
    if char.bold:
        p.append("1")
    if char.italics:
        p.append("3")
    if char.underscore:
        p.append("4")
    if char.blink:
        p.append("5")
    if char.reverse:
        p.append("7")
    if char.strikethrough:
        p.append("9")
    fg = char.fg
    if fg in _FG and fg != "default":
        p.append(str(_FG[fg]))
    elif isinstance(fg, str) and _is_hex(fg):
        p += ["38", "2", str(int(fg[0:2], 16)), str(int(fg[2:4], 16)), str(int(fg[4:6], 16))]
    bg = char.bg
    if bg in _FG and bg != "default":
        p.append(str(_FG[bg] + 10))
    elif isinstance(bg, str) and _is_hex(bg):
        p += ["48", "2", str(int(bg[0:2], 16)), str(int(bg[2:4], 16)), str(int(bg[4:6], 16))]
    return "\x1b[" + ";".join(p) + "m"


def render_screen(screen: pyte.Screen) -> bytes:
    """Build an ANSI sequence that exactly redraws the current screen."""
    out = ["\x1b[?25l\x1b[2J\x1b[H"]  # cursor hide, clear, home
    buf = screen.buffer
    for y in range(screen.lines):
        out.append(f"\x1b[{y + 1};1H")
        row = buf[y]
        last = ""
        for x in range(screen.columns):
            ch = row[x]
            sgr = _sgr(ch)
            if sgr != last:
                out.append(sgr)
                last = sgr
            out.append(ch.data or " ")
        out.append("\x1b[0m")
    cx = min(screen.cursor.x, screen.columns - 1) + 1
    cy = min(screen.cursor.y, screen.lines - 1) + 1
    out.append(f"\x1b[{cy};{cx}H")
    if not screen.cursor.hidden:
        out.append("\x1b[?25h")
    return "".join(out).encode("utf-8", "replace")


# ---------------------------------------------------------------------------
# Participant (viewer) -- ssh or web
# ---------------------------------------------------------------------------
class Participant:
    __slots__ = ("id", "mode", "transport", "ip", "cols", "rows",
                 "connected_at", "send", "on_resize")

    def __init__(self, pid, mode, transport, ip, cols, rows, send,
                 on_resize=None):
        self.id = pid
        self.mode = mode            # "ro" | "rw"
        self.transport = transport  # "ssh" | "web"
        self.ip = ip
        self.cols = cols
        self.rows = rows
        self.connected_at = time.monotonic()
        self.send = send            # callable(bytes) -> None
        self.on_resize = on_resize  # optional callable(cols, rows) -> None

    def info(self) -> dict:
        return {
            "id": self.id, "mode": self.mode, "transport": self.transport,
            "ip": self.ip, "cols": self.cols, "rows": self.rows,
            "secs": round(time.monotonic() - self.connected_at),
        }


# ---------------------------------------------------------------------------
# Shared session
# ---------------------------------------------------------------------------
class Session:
    def __init__(self, app, owner_key: str, cols: int, rows: int,
                 record: bool = False):
        self.app = app
        self.owner_key = owner_key
        self.ro_token = secrets.token_urlsafe(P.TOKEN_NBYTES)
        self.rw_token = secrets.token_urlsafe(P.TOKEN_NBYTES)
        self.reconnect_id = secrets.token_urlsafe(P.RECONNECT_NBYTES)

        self.host_cols, self.host_rows = P.clamp_size(cols, rows)
        self.neg_cols, self.neg_rows = self.host_cols, self.host_rows
        self.screen = pyte.Screen(self.neg_cols, self.neg_rows)
        self.screen.set_mode(pyte.modes.LNM)
        self.stream = pyte.ByteStream(self.screen)

        self.participants: dict[int, Participant] = {}
        self._next_pid = 1
        self.host_control = None  # ChoomSession (control channel)
        self.host_data = None     # ChoomSession (data channel)
        self.state = "live"
        self._grace = None

        # Record only when storage is configured AND the host opted in
        # (choom host --record) or the operator forces it (--record-all).
        self.recorder = None
        if app.record_dir and (record or app.record_all):
            self.recorder = Recorder(app.record_dir, self.ro_token,
                                     self.neg_cols, self.neg_rows)

    # -- output from the host -----------------------------------------------
    def feed_output(self, data: bytes):
        try:
            self.stream.feed(data)
        except Exception:
            pass  # a single parser error must not kill the relay
        for p in list(self.participants.values()):
            try:
                p.send(data)
            except Exception:
                pass
        if self.recorder:
            self.recorder.write_output(data)

    # -- viewers ------------------------------------------------------------
    def can_add_viewer(self) -> bool:
        cap = self.app.max_viewers
        return not cap or len(self.participants) < cap

    def add_participant(self, mode, transport, ip, cols, rows, send,
                        on_resize=None) -> Participant:
        pid = self._next_pid
        self._next_pid += 1
        p = Participant(pid, mode, transport, ip, cols, rows, send, on_resize)
        self.participants[pid] = p
        # tell the viewer the current size first, then repaint, then live stream:
        if on_resize is not None:
            try:
                on_resize(self.neg_cols, self.neg_rows)
            except Exception:
                pass
        try:
            p.send(render_screen(self.screen))
        except Exception:
            pass
        self.recompute_size()
        self.send_control({"t": P.MSG_JOIN, "client": p.info()})
        return p

    def remove_participant(self, pid: int):
        if self.participants.pop(pid, None) is not None:
            self.recompute_size()
            self.send_control({"t": P.MSG_LEAVE, "id": pid})

    def update_size(self, p: Participant, cols: int, rows: int):
        p.cols, p.rows = cols, rows
        self.recompute_size()

    def recompute_size(self):
        # Only host + RW ssh viewers count (RO/web must not shrink the size).
        sizes = [(self.host_cols, self.host_rows)]
        for p in self.participants.values():
            if p.mode == "rw" and p.transport == "ssh" and p.cols > 0:
                sizes.append((p.cols, p.rows))
        cols = min(c for c, _ in sizes)
        rows = min(r for _, r in sizes)
        cols, rows = P.clamp_size(cols, rows)
        if (cols, rows) != (self.neg_cols, self.neg_rows):
            self.neg_cols, self.neg_rows = cols, rows
            self.screen.resize(rows, cols)
            if self.recorder:
                self.recorder.write_resize(cols, rows)
            self.send_control({"t": P.MSG_RESIZE, "cols": cols, "rows": rows})
            repaint = render_screen(self.screen)
            for p in list(self.participants.values()):
                if p.on_resize is not None:
                    try:
                        p.on_resize(cols, rows)
                    except Exception:
                        pass
                try:
                    p.send(repaint)
                except Exception:
                    pass

    def set_host_size(self, cols: int, rows: int):
        self.host_cols, self.host_rows = P.clamp_size(cols, rows)
        self.recompute_size()

    # -- keystrokes from RW viewers to the host -----------------------------
    def to_host(self, data: bytes):
        if self.state == "live" and self.host_data is not None:
            try:
                self.host_data.chan.write(data)
            except Exception:
                pass

    # -- control to the host ------------------------------------------------
    def send_control(self, obj: dict):
        if self.host_control is not None:
            try:
                self.host_control.chan.write(P.encode_frame(obj))
            except Exception:
                pass

    def roster(self) -> list[dict]:
        return [p.info() for p in self.participants.values()]

    # -- lifecycle / reconnect ----------------------------------------------
    def host_disconnected(self):
        if self.state != "live":
            return
        self.state = "disconnected"
        self.host_control = None
        self.host_data = None
        note = b"\r\n\x1b[33m[choom] host disconnected, waiting to reconnect...\x1b[0m\r\n"
        for p in list(self.participants.values()):
            try:
                p.send(note)
            except Exception:
                pass
        loop = asyncio.get_event_loop()
        self._grace = loop.call_later(P.GRACE_PERIOD, self.expire)

    def host_reconnected(self):
        if self._grace:
            self._grace.cancel()
            self._grace = None
        self.state = "live"

    def expire(self):
        self.app.drop_session(self)
        for p in list(self.participants.values()):
            try:
                p.send(b"\r\n\x1b[31m[choom] session closed.\x1b[0m\r\n")
            except Exception:
                pass
        self.participants.clear()
        if self.recorder:
            self.recorder.close()


# ---------------------------------------------------------------------------
# SSH server: one instance per connection (auth + per-connection state)
# ---------------------------------------------------------------------------
class ChoomSSHServer(asyncssh.SSHServer):
    def __init__(self, app):
        self.app = app
        self.username = None
        self.role = None        # "host" | "viewer"
        self.token_mode = None  # "ro" | "rw" (viewer)
        self.owner_key = None   # normalized pubkey blob (host)
        self.session = None     # session bound to this connection (host)
        self.peer_ip = "?"

    def connection_made(self, conn):
        self._conn = conn
        peer = conn.get_extra_info("peername")
        if peer:
            self.peer_ip = peer[0]
        # Per-IP connection rate limit (anti-DoS on a public relay).
        if not self.app.allow_connection(self.peer_ip):
            conn.close()

    # -- Auth ---------------------------------------------------------------
    def begin_auth(self, username):
        self.username = username
        if username in P.HOST_USERS:
            self.role = "host"
            return True  # public key required
        # Viewer: the username is a token -> validate it immediately.
        match = self.app.lookup_token(username)
        if match is None:
            self.role = None
            return True   # unknown token -> auth required, which then fails
        self.role = "viewer"
        self.session, self.token_mode = match
        return False      # valid token -> no further auth needed

    def password_auth_supported(self):
        return False

    def public_key_auth_supported(self):
        return self.role == "host"

    def validate_public_key(self, username, key):
        if username not in P.HOST_USERS:
            return False
        blob = self.app.key_blob(key)
        if blob in self.app.authorized:
            self.owner_key = blob
            return True
        return False

    # -- deny forwarding/tunnels for ALL connections (anti-pivot) -----------
    def connection_requested(self, *a):
        return False

    def unix_connection_requested(self, *a):
        return False

    def server_requested(self, *a):
        return False

    def unix_server_requested(self, *a):
        return False

    def tun_requested(self, *a):
        return False

    def tap_requested(self, *a):
        return False

    # -- Sessions -----------------------------------------------------------
    def session_requested(self):
        return ChoomSession(self)

    def connection_lost(self, exc):
        if self.role == "host" and self.session is not None:
            self.session.host_disconnected()


# ---------------------------------------------------------------------------
# SSH session: one channel (host control/data OR viewer shell)
# ---------------------------------------------------------------------------
class ChoomSession(asyncssh.SSHServerSession):
    def __init__(self, server: ChoomSSHServer):
        self.server = server
        self.app = server.app
        self.chan = None
        self.kind = None          # "control" | "data" | "viewer"
        self.participant = None
        self.session = None       # the session this channel belongs to
        self._reader = P.FrameReader()
        self._vw_cols = 0
        self._vw_rows = 0

    def connection_made(self, chan):
        self.chan = chan
        chan.set_encoding(None)   # raw bytes

    # The host uses exec with a fixed command; viewers use shell+pty.
    def exec_requested(self, command):
        if self.server.role != "host":
            return False
        if command == P.CONTROL_CMD:
            self.kind = "control"
            return True
        if command == P.DATA_CMD:
            self.kind = "data"
            return True
        return False

    def shell_requested(self):
        if self.server.role != "viewer":
            return False
        self.kind = "viewer"
        return True

    def pty_requested(self, term_type, term_size, term_modes):
        # Only viewers need a PTY.
        if self.server.role != "viewer":
            return False
        self._vw_cols, self._vw_rows = term_size[0], term_size[1]
        return True

    def subsystem_requested(self, subsystem):
        return False  # no sftp or similar

    def session_started(self):
        if self.kind == "control":
            self._start_control()
        elif self.kind == "data":
            self._start_data()
        elif self.kind == "viewer":
            self._start_viewer()

    # -- Host: control channel ---------------------------------------------
    def _start_control(self):
        if self.server.username == P.USER_QUERY:
            self._answer_query()
            return
        # Wait for MSG_HELLO / MSG_RESUME (arrives via data_received).

    def _answer_query(self):
        sess = self.app.session_for_owner(self.server.owner_key)
        lst = sess.roster() if sess else []
        self.chan.write(P.encode_frame({"t": P.MSG_CLIENTS, "list": lst}))
        self.chan.write_eof()
        self.chan.exit(0)

    def _start_data(self):
        sess = self.server.session
        if sess is None:
            self.chan.exit(1)
            return
        sess.host_data = self
        self.session = sess

    # -- Host: viewer -------------------------------------------------------
    def _start_viewer(self):
        sess = self.server.session
        if sess is None:
            self.chan.write(b"\x1b[31m[choom] unknown or expired session.\x1b[0m\r\n")
            self.chan.exit(1)
            return
        if not sess.can_add_viewer():
            self.chan.write(b"\x1b[31m[choom] session is full.\x1b[0m\r\n")
            self.chan.exit(1)
            return
        self.session = sess
        ip = self.server.peer_ip
        cols = self._vw_cols or sess.neg_cols
        rows = self._vw_rows or sess.neg_rows
        self.participant = sess.add_participant(
            self.server.token_mode, "ssh", ip, cols, rows, self.chan.write)

    # -- incoming bytes -----------------------------------------------------
    def data_received(self, data, datatype):
        if self.kind == "control":
            self._control_in(data)
        elif self.kind == "viewer":
            # RW may type, RO is dropped.
            if self.server.token_mode == "rw" and self.session is not None:
                self.session.to_host(data)
        # data channel host->server is the shell's PTY output
        elif self.kind == "data" and self.session is not None:
            self.session.feed_output(data)

    def _control_in(self, data):
        try:
            frames = self._reader.feed(data)
        except Exception:
            self.chan.exit(1)
            return
        for f in frames:
            self._handle_control(f)

    def _handle_control(self, f):
        t = f.get("t")
        srv = self.server
        if t == P.MSG_HELLO:
            sess = self.app.create_session(srv.owner_key,
                                           f.get("cols", 80), f.get("rows", 24),
                                           record=bool(f.get("record", False)))
            if sess is None:
                self.chan.write(P.encode_frame(
                    {"t": P.MSG_ERROR, "msg": "server at capacity, try later"}))
                self.chan.exit(1)
                return
            sess.host_control = self
            srv.session = self.session = sess
            self._send_ready(sess)
        elif t == P.MSG_RESUME:
            sess = self.app.resume_session(f.get("id"), srv.owner_key)
            if sess is None:
                self.chan.write(P.encode_frame({"t": P.MSG_ERROR, "msg": "resume rejected"}))
                self.chan.exit(1)
                return
            sess.host_control = self
            srv.session = self.session = sess
            sess.host_reconnected()
            self._send_ready(sess)
            # fresh repaint to all existing viewers:
            sess.recompute_size()
        elif t == P.MSG_HOST_SIZE and self.session is not None:
            self.session.set_host_size(f.get("cols", 80), f.get("rows", 24))
        elif t == P.MSG_LIST_CLIENTS and self.session is not None:
            self.chan.write(P.encode_frame({"t": P.MSG_CLIENTS, "list": self.session.roster()}))

    def _send_ready(self, sess):
        a = self.app
        web = None
        if a.web_host:
            base = a.web_url or f"http://{a.web_host}:{a.web_port}"
            web = f"{base.rstrip('/')}/s/{sess.ro_token}"
        self.chan.write(P.encode_frame({
            "t": P.MSG_READY,
            "ro": sess.ro_token, "rw": sess.rw_token,
            "id": sess.reconnect_id,
            "host": a.advertise_host, "port": a.advertise_port,
            "web": web, "fp": a.fingerprint,
        }))

    def terminal_size_changed(self, width, height, pixwidth, pixheight):
        if self.kind == "viewer" and self.participant and self.session:
            self.session.update_size(self.participant, width, height)

    def connection_lost(self, exc):
        if self.kind == "viewer" and self.participant and self.session:
            self.session.remove_participant(self.participant.id)
        elif self.kind == "data" and self.session is not None:
            if self.session.host_data is self:
                self.session.host_data = None


# ---------------------------------------------------------------------------
# Application / registry
# ---------------------------------------------------------------------------
class ChoomApp:
    def __init__(self, args, host_key, authorized, fingerprint):
        self.host_key = host_key
        self.authorized = authorized          # set[bytes] of normalized blobs
        self.fingerprint = fingerprint
        self.advertise_host = args.host
        self.advertise_port = args.advertise_port or args.listen_port
        self.record_dir = args.record
        self.record_all = getattr(args, "record_all", False)
        self.downloads_dir = getattr(args, "downloads", "") or ""
        self.web_host = None
        self.web_port = None
        self.web_url = getattr(args, "web_url", "") or ""  # advertised base URL
        self.sessions: dict[str, Session] = {}          # token -> Session
        self.by_reconnect: dict[str, Session] = {}

        # Resource limits (public-relay hardening).
        self.max_sessions = getattr(args, "max_sessions", P.DEFAULT_MAX_SESSIONS)
        self.max_viewers = getattr(args, "max_viewers", P.DEFAULT_MAX_VIEWERS)
        self._rate_per_min = getattr(args, "rate_limit", P.DEFAULT_RATE_PER_MIN)
        self._buckets: dict[str, tuple[float, float]] = {}  # ip -> (tokens, ts)

    def allow_connection(self, ip: str) -> bool:
        """Per-IP token bucket: throttles new connections from one source."""
        if self._rate_per_min <= 0:
            return True
        now = time.monotonic()
        cap = float(self._rate_per_min)
        tokens, ts = self._buckets.get(ip, (cap, now))
        tokens = min(cap, tokens + (now - ts) * (cap / 60.0))
        if tokens < 1.0:
            self._buckets[ip] = (tokens, now)
            return False
        self._buckets[ip] = (tokens - 1.0, now)
        if len(self._buckets) > 10000:        # bound the table
            self._buckets = {ip: self._buckets[ip]}
        return True

    def session_count(self) -> int:
        # sessions dict has two tokens per session; count unique objects.
        return len(set(self.sessions.values()))

    @staticmethod
    def key_blob(key) -> bytes:
        # Normalized comparison: only algorithm + base64 blob, no comment.
        parts = key.export_public_key().split()
        return b" ".join(parts[:2])

    def lookup_token(self, token: str):
        sess = self.sessions.get(token)
        if sess is None:
            return None
        if secrets.compare_digest(token, sess.ro_token):
            return sess, "ro"
        if secrets.compare_digest(token, sess.rw_token):
            return sess, "rw"
        return None

    def create_session(self, owner_key, cols, rows, record=False):
        if self.max_sessions and self.session_count() >= self.max_sessions:
            return None                      # global capacity reached
        sess = Session(self, owner_key, cols, rows, record=record)
        self.sessions[sess.ro_token] = sess
        self.sessions[sess.rw_token] = sess
        self.by_reconnect[sess.reconnect_id] = sess
        return sess

    def resume_session(self, reconnect_id, owner_key):
        if not reconnect_id:
            return None
        sess = self.by_reconnect.get(reconnect_id)
        if sess is None or sess.owner_key != owner_key:
            return None
        return sess

    def session_for_owner(self, owner_key):
        # Most recent live session of this owner (typically exactly one).
        found = [s for s in set(self.sessions.values())
                 if s.owner_key == owner_key and s.state == "live"]
        return found[-1] if found else None

    def drop_session(self, sess: Session):
        self.sessions.pop(sess.ro_token, None)
        self.sessions.pop(sess.rw_token, None)
        self.by_reconnect.pop(sess.reconnect_id, None)


# ---------------------------------------------------------------------------
# host key / authorized_keys / CLI
# ---------------------------------------------------------------------------
def load_or_create_host_key(path: str):
    if os.path.exists(path):
        return asyncssh.read_private_key(path)
    key = asyncssh.generate_private_key("ssh-ed25519")
    key.write_private_key(path)
    os.chmod(path, 0o600)
    with open(path + ".pub", "wb") as f:
        f.write(key.export_public_key())
    return key


def load_authorized(path: str) -> set[bytes]:
    keys = set()
    if not os.path.exists(path):
        return keys
    with open(path, "rb") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith(b"#"):
                continue
            try:
                k = asyncssh.import_public_key(line)
                keys.add(ChoomApp.key_blob(k))
            except Exception:
                pass
    return keys


def default_key_blob():
    """Blob of the built-in default pubkey, or None if it is a placeholder."""
    pub = P.DEFAULT_TRUSTED_PUBKEY
    if not pub or "__FILL" in pub:
        return None
    try:
        return ChoomApp.key_blob(asyncssh.import_public_key(pub))
    except Exception:
        return None


def parse_web(spec: str):
    if not spec:
        return None, None
    if ":" in spec:
        host, port = spec.rsplit(":", 1)
        return (host or "localhost"), int(port)
    return "localhost", int(spec)


async def amain(args):
    host_key = load_or_create_host_key(args.host_key)
    authorized = load_authorized(args.authorized_keys)
    trusts_default = False
    if not args.no_default_key:
        blob = default_key_blob()
        if blob is not None:
            authorized.add(blob)
            trusts_default = True
    fingerprint = host_key.get_fingerprint()
    app = ChoomApp(args, host_key, authorized, fingerprint)

    web_host, web_port = parse_web(args.web)
    web_runner = None
    if web_host:
        from . import web as webmod
        app.web_host = args.host if args.host not in ("localhost", "") else web_host
        app.web_port = web_port
        web_runner = await webmod.start_web(app, web_host, web_port)

    await asyncssh.create_server(
        lambda: ChoomSSHServer(app), args.bind, args.listen_port,
        server_host_keys=[host_key],
    )

    # --- startup banner -------------------------------------------------------
    if args.record:
        os.makedirs(args.record, exist_ok=True)

    def row(label, value):
        print(f"  \x1b[36m{label:<12}\x1b[0m {value}")

    listen = f"{args.bind or '0.0.0.0'}:{args.listen_port}"
    print(f"\n  \x1b[1;33mchoom relay {__version__}\x1b[0m")
    print("  " + "─" * 52)
    row("ssh", f"{listen}  (advertise {app.advertise_host}:{app.advertise_port})")
    if web_host:
        row("web (RO)", f"http://{web_host}:{web_port}/")
    row("host key", fingerprint)
    row("limits", f"sessions={app.max_sessions or '∞'} "
                  f"viewers/session={app.max_viewers or '∞'} "
                  f"rate={app._rate_per_min or 'off'}/min")
    if args.record:
        ret = []
        if args.record_max_age_days:
            ret.append(f"{args.record_max_age_days:g}d")
        if args.record_max_mb:
            ret.append(f"{args.record_max_mb:g}MB")
        row("recording", args.record + ("  (max " + " / ".join(ret) + ")" if ret else ""))
    if trusts_default:
        row("default key", "\x1b[33mTRUSTED — anyone can host "
                           "(use --no-default-key for a private relay)\x1b[0m")
    if not authorized:
        row("WARNING", "\x1b[31mno authorized keys and default key off — "
                       "no host can connect\x1b[0m")
    print()

    if args.record:
        if args.record_max_age_days or args.record_max_mb:
            async def _gc_loop():
                while True:
                    try:
                        record_gc(args.record, args.record_max_age_days,
                                  args.record_max_mb)
                    except Exception:
                        pass
                    await asyncio.sleep(3600)
            asyncio.ensure_future(_gc_loop())
    await asyncio.Event().wait()  # keep running


def main():
    ap = argparse.ArgumentParser(description="choom -- SSH terminal sharing relay")
    ap.add_argument("-c", "--config", default="",
                    help="YAML config file (CLI flags override it)")
    ap.add_argument("--bind", default="", help="bind address (default: all)")
    ap.add_argument("-p", "--listen-port", type=int, default=P.DEFAULT_SSH_PORT)
    ap.add_argument("-H", "--host", default=P.DEFAULT_SERVER,
                    help="hostname shown to viewers in the share links")
    ap.add_argument("-q", "--advertise-port", type=int, default=0,
                    help="SSH port in the links (default = listen-port)")
    ap.add_argument("-k", "--host-key", default="host_key")
    ap.add_argument("-a", "--authorized-keys", default="authorized_keys")
    ap.add_argument("--no-default-key", action="store_true",
                    help="do NOT trust the built-in default key (private servers)")
    ap.add_argument("-w", "--web", default="",
                    help="enable the web module, e.g. ':8080' or 'host:8080'")
    ap.add_argument("-W", "--web-url", default="",
                    help="public base URL advertised for the web view, e.g. "
                         "https://choom.example (overrides the http://host:port form)")
    ap.add_argument("-r", "--record", default="",
                    help="directory enabling asciicast recordings; hosts opt in "
                         "per session with `choom host --record`")
    ap.add_argument("--record-all", action="store_true",
                    help="record every session, even without the host opting in")
    ap.add_argument("--downloads", default="",
                    help="directory of prebuilt binaries to serve at /download/")
    # Resource limits (public-relay hardening).
    ap.add_argument("--max-sessions", type=int, default=P.DEFAULT_MAX_SESSIONS,
                    help="global cap on concurrent sessions (0 = unlimited)")
    ap.add_argument("--max-viewers", type=int, default=P.DEFAULT_MAX_VIEWERS,
                    help="cap on viewers per session (0 = unlimited)")
    ap.add_argument("--rate-limit", type=int, default=P.DEFAULT_RATE_PER_MIN,
                    help="new connections per source IP per minute (0 = off)")
    # Recording retention (only relevant with -r).
    ap.add_argument("--record-max-age-days", type=float,
                    default=P.DEFAULT_RECORD_MAX_AGE_DAYS,
                    help="delete recordings older than this (0 = keep)")
    ap.add_argument("--record-max-mb", type=float, default=P.DEFAULT_RECORD_MAX_MB,
                    help="cap total recordings size, oldest first (0 = no cap)")
    args = P.apply_config(ap, ap.parse_args())
    try:
        asyncio.run(amain(args))
    except (KeyboardInterrupt, asyncssh.Error) as e:
        print(f"[choom] stopped: {e}", file=sys.stderr)


if __name__ == "__main__":
    main()
