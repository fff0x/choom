# SPDX-License-Identifier: GPL-3.0-or-later
"""Integration tests: real SSH viewers against server+host."""

import asyncio

import asyncssh
import pytest

from harness import capture, viewer_capture, _size_while_viewer


def test_ro_viewer_sees_content(shared):
    relay, host = shared
    host.run_cmd("echo MARKER_RO_123")
    out = capture(relay.ssh_port, host.tokens["ro"])
    assert "MARKER_RO_123" in out


def test_rw_viewer_can_type(shared):
    relay, host = shared
    out = capture(relay.ssh_port, host.tokens["rw"], inject="echo RW_TYPED_OK\r")
    assert "RW_TYPED_OK" in out


def test_ro_viewer_input_is_dropped(shared):
    relay, host = shared
    capture(relay.ssh_port, host.tokens["ro"], inject="echo RO_MUST_NOT_RUN\r")
    # Fresh look: the command must not have run anywhere.
    out = capture(relay.ssh_port, host.tokens["ro"], duration=1.0)
    assert "RO_MUST_NOT_RUN" not in out


def test_unknown_token_rejected(shared):
    relay, host = shared
    with pytest.raises(asyncssh.Error):
        capture(relay.ssh_port, "totally-invalid-token", duration=0.5)


def test_forwarding_denied_for_viewer(shared):
    relay, host = shared

    async def check():
        async with asyncssh.connect(
            "localhost", relay.ssh_port, username=host.tokens["ro"],
            known_hosts=None, client_keys=[], agent_path=None, password="",
        ) as conn:
            blocked = 0
            try:
                await conn.open_connection("example.com", 80)   # -L
            except Exception:
                blocked += 1
            try:
                lst = await conn.forward_remote_port("", 0, "localhost", 1)  # -R
                lst.close()
            except Exception:
                blocked += 1
            return blocked

    assert asyncio.run(check()) == 2


def test_multiple_viewers_all_see_output(shared):
    relay, host = shared
    host.run_cmd("echo SHARED_TO_ALL")

    async def two():
        a, b = await asyncio.gather(
            viewer_capture(relay.ssh_port, host.tokens["ro"]),
            viewer_capture(relay.ssh_port, host.tokens["rw"]),
        )
        return a, b

    a, b = asyncio.run(two())
    assert "SHARED_TO_ALL" in a
    assert "SHARED_TO_ALL" in b


# --- Resize-Aushandlung ----------------------------------------------------
def test_host_uses_native_size_without_viewers(shared):
    relay, host = shared
    host.run_cmd("stty size")
    out = host.drain()
    assert "24 80" in out                 # rows cols (from the 80x24 PTY)


def test_rw_viewer_shrinks_host(shared):
    relay, host = shared
    out = asyncio.run(_size_while_viewer(
        relay.ssh_port, host.tokens["rw"], (40, 10), host))
    assert "10 40" in out                 # host PTY follows the smallest RW viewer


def test_ro_viewer_does_not_shrink_host(shared):
    relay, host = shared
    out = asyncio.run(_size_while_viewer(
        relay.ssh_port, host.tokens["ro"], (40, 10), host))
    assert "24 80" in out                 # RO must NOT shrink the size
    assert "10 40" not in out
