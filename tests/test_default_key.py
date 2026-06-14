# SPDX-License-Identifier: GPL-3.0-or-later
"""Built-in default key: trusted by default, rejected with --no-default-key."""

import pytest

from harness import Host, Relay


def test_default_key_host_connects(relay):
    # Server trusts the built-in default key by default; a host without -i
    # (i.e. using the bundled key) can create a session.
    h = Host(relay, identity=None)
    try:
        h.start()
        assert "ro" in h.tokens and "rw" in h.tokens
    finally:
        h.stop()


def test_no_default_key_rejects_default_host(tmp_path):
    # With --no-default-key the relay must NOT accept the bundled default key.
    r = Relay(str(tmp_path), no_default_key=True).start()
    h = Host(r, identity=None)
    try:
        with pytest.raises(RuntimeError):
            h.start(timeout=6)
    finally:
        h.stop()
        r.stop()
