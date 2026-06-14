# SPDX-License-Identifier: GPL-3.0-or-later
"""Unit tests for the public-relay hardening: limits and recording GC."""

import argparse
import os
import time

from choom.server import ChoomApp
from choom.recorder import gc


def _app(**kw):
    args = argparse.Namespace(host="localhost", advertise_port=0,
                              listen_port=2200, record="", **kw)
    return ChoomApp(args, host_key=None, authorized=set(), fingerprint="fp")


def test_max_sessions_enforced():
    app = _app(max_sessions=2, max_viewers=50, rate_limit=0)
    assert app.create_session(b"o", 80, 24) is not None
    assert app.create_session(b"o", 80, 24) is not None
    assert app.create_session(b"o", 80, 24) is None         # at capacity


def test_rate_limiter_per_ip():
    app = _app(max_sessions=0, max_viewers=0, rate_limit=3)
    assert app.allow_connection("1.2.3.4")
    assert app.allow_connection("1.2.3.4")
    assert app.allow_connection("1.2.3.4")
    assert not app.allow_connection("1.2.3.4")              # 4th too soon
    assert app.allow_connection("5.6.7.8")                  # other IP unaffected


def test_rate_limit_disabled():
    app = _app(rate_limit=0)
    assert all(app.allow_connection("x") for _ in range(100))


def test_can_add_viewer_cap():
    app = _app(max_viewers=1)
    sess = app.create_session(b"o", 80, 24)
    assert sess.can_add_viewer()
    sess.add_participant("ro", "ssh", "ip", 80, 24, lambda d: None)
    assert not sess.can_add_viewer()                        # cap reached


def test_gc_by_age(tmp_path):
    d = str(tmp_path)
    old = os.path.join(d, "old.cast")
    new = os.path.join(d, "new.cast")
    open(old, "w").write("x")
    open(new, "w").write("y")
    past = time.time() - 10 * 86400
    os.utime(old, (past, past))
    removed = gc(d, max_age_days=7, max_mb=0)
    assert removed == 1
    assert not os.path.exists(old) and os.path.exists(new)


def test_gc_by_size(tmp_path):
    d = str(tmp_path)
    # three 1 MiB files; cap at 2 MiB -> oldest one pruned
    for i in range(3):
        p = os.path.join(d, f"{i}.cast")
        with open(p, "wb") as f:
            f.write(b"\0" * (1024 * 1024))
        os.utime(p, (1000 + i, 1000 + i))                  # ascending mtime
    removed = gc(d, max_age_days=0, max_mb=2)
    assert removed == 1
    assert not os.path.exists(os.path.join(d, "0.cast"))   # oldest gone
    assert os.path.exists(os.path.join(d, "2.cast"))       # newest kept
