# SPDX-License-Identifier: GPL-3.0-or-later
"""pytest fixtures: relay (server) and shared (server + host)."""

import pytest

from harness import Relay, Host


@pytest.fixture
def relay(tmp_path):
    r = Relay(str(tmp_path)).start()
    try:
        yield r
    finally:
        r.stop()


@pytest.fixture
def shared(relay):
    """Relay + a host sharing a shell (recording opted in). Yields (relay, host)."""
    h = Host(relay, record=True).start()
    try:
        yield relay, h
    finally:
        h.stop()
