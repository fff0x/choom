# SPDX-License-Identifier: GPL-3.0-or-later
"""Shared constants and the control-frame schema for choom.

choom = "tmate without tmux": a host shares its local shell through a central
SSH relay; viewers connect with a plain `ssh <token>@server`.

Two transport paths between the host client and the server (both over a single
asyncssh connection, distinguished by the SSH "command"):

  * CONTROL_CMD : newline-delimited JSON frames (see below), bidirectional.
  * DATA_CMD    : raw bytes. host->server = the shell's PTY output,
                  server->host = keystrokes from read-write viewers.

Viewers use an ordinary interactive SSH session (no command).
"""

from __future__ import annotations

import json
import os

# --- Reserved SSH usernames (host role, public-key required) ------------------
USER_NEW = "new"        # create a new session
USER_RESUME = "resume"  # take over an existing session after a disconnect
USER_QUERY = "query"    # only query viewers (choom clients); creates nothing
HOST_USERS = frozenset({USER_NEW, USER_RESUME, USER_QUERY})

# --- SSH "commands" the host client uses to tag its channels -------------------
CONTROL_CMD = "choom-control"
DATA_CMD = "choom-data"

# --- Tokens / secrets ---------------------------------------------------------
TOKEN_NBYTES = 16      # secrets.token_urlsafe(16) -> 128 bit
RECONNECT_NBYTES = 32  # reconnect_id, high entropy

# --- Terminal sizes (clamped against resize / memory DoS) ---------------------
MIN_COLS, MIN_ROWS = 20, 5
MAX_COLS, MAX_ROWS = 1000, 1000

# --- Defaults -----------------------------------------------------------------
DEFAULT_SERVER = "choom.this-is-fine.io"  # public default relay
DEFAULT_SSH_PORT = 2200
DEFAULT_WEB_PORT = 8080
GRACE_PERIOD = 60.0      # seconds a session lives on after the host drops
MAX_FRAME_BYTES = 64 * 1024  # upper bound for a single control line

# --- Resource limits (public-relay hardening) ---------------------------------
DEFAULT_MAX_SESSIONS = 200      # global cap on concurrent sessions
DEFAULT_MAX_VIEWERS = 50        # cap on viewers per session
DEFAULT_RATE_PER_MIN = 60       # new connections per source IP per minute

# --- Recording retention ------------------------------------------------------
DEFAULT_RECORD_MAX_AGE_DAYS = 7    # delete .cast files older than this
DEFAULT_RECORD_MAX_MB = 500        # cap total recordings size (oldest first)

# Built-in default keypair: the standard client ships with the private key
# (file `default_id` inside the package), and the server trusts the matching
# public key by default (opt-out via --no-default-key). Convenience for the
# public service; disable it for private servers and use your own authorized_keys.
DEFAULT_TRUSTED_PUBKEY = (
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOTfZTtt2qfPS8AIbSU/vNajIGfRjFAaJTzzEWrHqdd1"
    " choom-default"
)

# --- Control message types (value of the "t" field) ---------------------------
# host -> server
MSG_HELLO = "hello"            # {cols, rows}            create a new session
MSG_RESUME = "resume"          # {id, cols, rows}        take over a session
MSG_HOST_SIZE = "size"         # {cols, rows}            host terminal resized
MSG_LIST_CLIENTS = "clients?"  # {}                      request the roster
# server -> host
MSG_READY = "ready"            # {ro, rw, web, id, host, port, fp}
MSG_RESIZE = "resize"          # {cols, rows}            negotiated size
MSG_JOIN = "join"              # {client}                viewer joined
MSG_LEAVE = "leave"            # {id}                    viewer left
MSG_CLIENTS = "clients"        # {list:[client,...]}     roster reply
MSG_ERROR = "error"            # {msg}


def default_identity_path() -> str:
    """Path to the bundled private default key ('' if it is not present)."""
    p = os.path.join(os.path.dirname(__file__), "default_id")
    return p if os.path.exists(p) else ""


def client_keys(identity, use_default: bool = True):
    """Resolve which client keys to use.

    Explicit `-i` keys win; otherwise (opt-out via use_default=False) the bundled
    default key for the public default server.
    """
    if identity:
        return list(identity)
    if use_default:
        d = default_identity_path()
        if d:
            return [d]
    return None


def apply_config(parser, args):
    """If ``args.config`` is set, load that YAML file and re-parse with its values
    as defaults so explicit CLI flags still win (CLI > config > built-in defaults).
    YAML keys use the long option name (dashes or underscores), e.g. ``max-viewers``.
    """
    path = getattr(args, "config", "")
    if not path:
        return args
    import yaml
    with open(path) as f:
        cfg = yaml.safe_load(f) or {}
    if not isinstance(cfg, dict):
        raise SystemExit(f"choom: config {path} is not a mapping")
    parser.set_defaults(**{k.replace("-", "_"): v for k, v in cfg.items()})
    return parser.parse_args()


def clamp_size(cols: int, rows: int) -> tuple[int, int]:
    """Clamp a terminal size to the allowed range."""
    cols = max(MIN_COLS, min(MAX_COLS, int(cols)))
    rows = max(MIN_ROWS, min(MAX_ROWS, int(rows)))
    return cols, rows


def encode_frame(obj: dict) -> bytes:
    """Serialize a control frame as one newline-terminated JSON line."""
    return (json.dumps(obj, separators=(",", ":")) + "\n").encode("utf-8")


class FrameReader:
    """Accumulates incoming bytes and yields complete JSON frames.

    Guards against over-long lines (MAX_FRAME_BYTES) -> no memory DoS.
    """

    def __init__(self) -> None:
        self._buf = bytearray()

    def feed(self, data: bytes) -> list[dict]:
        out: list[dict] = []
        self._buf.extend(data)
        if len(self._buf) > MAX_FRAME_BYTES and b"\n" not in self._buf:
            raise ValueError("control frame too large")
        while True:
            nl = self._buf.find(b"\n")
            if nl < 0:
                break
            line = bytes(self._buf[:nl])
            del self._buf[: nl + 1]
            line = line.strip()
            if not line:
                continue
            obj = json.loads(line)
            if not isinstance(obj, dict) or "t" not in obj:
                raise ValueError("malformed control frame")
            out.append(obj)
        return out
