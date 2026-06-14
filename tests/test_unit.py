# SPDX-License-Identifier: GPL-3.0-or-later
"""Unit tests: protocol helpers, screen rendering, session/resume logic."""

import argparse

import pyte
import pytest

from choom import protocol as P
from choom.server import ChoomApp, Session, render_screen


# --- clamp_size ------------------------------------------------------------
def test_clamp_floor():
    assert P.clamp_size(0, 0) == (P.MIN_COLS, P.MIN_ROWS)


def test_clamp_ceiling():
    assert P.clamp_size(99999, 99999) == (P.MAX_COLS, P.MAX_ROWS)


def test_clamp_passthrough():
    assert P.clamp_size(100, 30) == (100, 30)


# --- FrameReader -----------------------------------------------------------
def test_frame_split_across_chunks():
    r = P.FrameReader()
    assert r.feed(b'{"t":"a"') == []
    out = r.feed(b',"x":1}\n')
    assert out == [{"t": "a", "x": 1}]


def test_frame_multiple_in_one_chunk():
    r = P.FrameReader()
    out = r.feed(b'{"t":"a"}\n{"t":"b"}\n')
    assert [f["t"] for f in out] == ["a", "b"]


def test_frame_malformed_without_type():
    r = P.FrameReader()
    with pytest.raises(ValueError):
        r.feed(b'{"x":1}\n')


def test_frame_oversize_rejected():
    r = P.FrameReader()
    with pytest.raises(ValueError):
        r.feed(b"a" * (P.MAX_FRAME_BYTES + 10))


# --- render_screen ---------------------------------------------------------
def test_render_contains_content_and_clear():
    screen = pyte.Screen(20, 3)
    stream = pyte.ByteStream(screen)
    stream.feed(b"hello")
    out = render_screen(screen)
    assert b"hello" in out
    assert b"\x1b[2J" in out      # clear screen
    assert b"\x1b[H" in out       # cursor home


def test_render_colors():
    screen = pyte.Screen(10, 1)
    stream = pyte.ByteStream(screen)
    stream.feed(b"\x1b[31mX")     # red X
    out = render_screen(screen)
    assert b"31" in out and b"X" in out


# --- ChoomApp: token lookup + resume binding -------------------------------
def _app(tmp_path=None):
    args = argparse.Namespace(host="localhost", advertise_port=0,
                              listen_port=2200, record="")
    return ChoomApp(args, host_key=None, authorized=set(), fingerprint="fp")


def test_lookup_token_ro_rw_and_unknown():
    app = _app()
    sess = app.create_session(b"owner", 80, 24)
    assert app.lookup_token(sess.ro_token) == (sess, "ro")
    assert app.lookup_token(sess.rw_token) == (sess, "rw")
    assert app.lookup_token("does-not-exist") is None


def test_resume_requires_matching_owner_key():
    app = _app()
    sess = app.create_session(b"owner-A", 80, 24)
    # correct owner -> ok
    assert app.resume_session(sess.reconnect_id, b"owner-A") is sess
    # different (possibly authorized) key -> rejected
    assert app.resume_session(sess.reconnect_id, b"owner-B") is None
    # wrong id -> rejected
    assert app.resume_session("garbage", b"owner-A") is None
    # empty id -> rejected
    assert app.resume_session("", b"owner-A") is None


def test_tokens_are_distinct_and_long():
    app = _app()
    s1 = app.create_session(b"o", 80, 24)
    s2 = app.create_session(b"o", 80, 24)
    toks = {s1.ro_token, s1.rw_token, s2.ro_token, s2.rw_token}
    assert len(toks) == 4              # all distinct
    assert all(len(t) >= 20 for t in toks)


def test_drop_session_removes_all_tokens():
    app = _app()
    sess = app.create_session(b"o", 80, 24)
    app.drop_session(sess)
    assert app.lookup_token(sess.ro_token) is None
    assert app.resume_session(sess.reconnect_id, b"o") is None


def test_strip_terminal_replies():
    from choom.host import strip_terminal_replies as s
    # DA replies are removed wherever they appear
    assert s(b"abc\x1b[?62;52;cdef") == b"abcdef"
    assert s(b"\x1b[>1;95;0c") == b""
    # ordinary input and real keys are preserved
    assert s(b"hello world") == b"hello world"
    assert s(b"\x1b") == b"\x1b"          # lone ESC (the Escape key)
    assert s(b"\x1b[A") == b"\x1b[A"      # arrow key
    assert s(b"\x1b[3~") == b"\x1b[3~"    # delete key


# --- bundled default key ---------------------------------------------------
def test_client_keys_default_is_bundled():
    import os
    keys = P.client_keys(None, use_default=True)
    assert keys and os.path.exists(keys[0])
    # explicit identity wins; opt-out yields nothing
    assert P.client_keys(["mine"], use_default=True) == ["mine"]
    assert P.client_keys(None, use_default=False) is None


def test_default_pubkey_matches_bundled_private():
    import asyncssh
    from choom.server import default_key_blob
    priv = asyncssh.read_private_key(P.default_identity_path())
    assert default_key_blob() == ChoomApp.key_blob(priv.convert_to_public())
