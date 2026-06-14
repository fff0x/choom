# SPDX-License-Identifier: GPL-3.0-or-later
"""Reconnect/resume at the protocol level -- deterministic via FakeHost.

Instead of forcing a real network drop, we speak the host protocol directly and
drop/resume the connection on purpose.
"""

import asyncio

import asyncssh

from harness import FakeHost


def test_resume_happy_path_keeps_viewer(relay):
    async def flow():
        fh = FakeHost(relay.ssh_port, relay.owner_id)
        assert await fh.connect() is True
        ro = fh.ready["ro"]
        rid = fh.ready["id"]

        # connect a viewer and keep it open
        vconn = await asyncssh.connect(
            "localhost", relay.ssh_port, username=ro, known_hosts=None,
            client_keys=[], agent_path=None, password="")
        vproc = await vconn.create_process(
            term_type="xterm", term_size=(80, 24),
            encoding="utf-8", errors="replace")
        seen = []

        async def rd():
            while True:
                d = await vproc.stdout.read(4096)
                if not d:
                    break
                seen.append(d)
        rt = asyncio.ensure_future(rd())

        await asyncio.sleep(0.3)
        fh.emit(b"BEFORE_DROP\r\n")
        await asyncio.sleep(0.6)
        assert "BEFORE_DROP" in "".join(seen)

        # hard-cut the host connection (the viewer stays connected)
        fh.drop()
        await asyncio.sleep(0.5)

        # resume with the same id + same owner key
        fh2 = FakeHost(relay.ssh_port, relay.owner_id)
        assert await fh2.connect(resume_id=rid) is True
        assert fh2.ready["ro"] == ro          # tokens stay valid

        fh2.emit(b"AFTER_RESUME\r\n")
        await asyncio.sleep(0.6)
        assert "AFTER_RESUME" in "".join(seen)   # same viewer keeps seeing it

        rt.cancel()
        vconn.close()
        await fh2.close()

    asyncio.run(flow())


def test_resume_rejected_with_wrong_owner_key(relay):
    async def flow():
        fh = FakeHost(relay.ssh_port, relay.owner_id)
        assert await fh.connect() is True
        rid = fh.ready["id"]
        fh.drop()
        await asyncio.sleep(0.3)
        # the intruder is authorized but NOT the owner -> resume rejected
        fh2 = FakeHost(relay.ssh_port, relay.intruder_id)
        assert await fh2.connect(resume_id=rid) is False
        await fh2.close()

    asyncio.run(flow())


def test_resume_rejected_with_bad_id(relay):
    async def flow():
        fh = FakeHost(relay.ssh_port, relay.owner_id)
        assert await fh.connect() is True
        fh.drop()
        await asyncio.sleep(0.3)
        fh2 = FakeHost(relay.ssh_port, relay.owner_id)
        assert await fh2.connect(resume_id="not-a-real-id") is False
        await fh2.close()

    asyncio.run(flow())
