# SPDX-License-Identifier: GPL-3.0-or-later
"""Web module: read-only stream, rw->404, asciicast download, security headers."""

import asyncio
import time

import aiohttp


async def _ws_capture(web_port, token, secs=2.0):
    out = []
    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(
                f"http://localhost:{web_port}/s/{token}/stream") as ws:
            end = time.time() + secs
            while time.time() < end:
                try:
                    msg = await asyncio.wait_for(ws.receive(), timeout=0.5)
                except asyncio.TimeoutError:
                    continue
                if msg.type == aiohttp.WSMsgType.BINARY:
                    out.append(msg.data.decode("utf-8", "replace"))
                elif msg.type in (aiohttp.WSMsgType.CLOSED,
                                  aiohttp.WSMsgType.CLOSING,
                                  aiohttp.WSMsgType.ERROR):
                    break
    return "".join(out)


def test_web_ro_stream_shows_content(shared):
    relay, host = shared
    host.run_cmd("echo WEB_CONTENT_MARK")
    out = asyncio.run(_ws_capture(relay.web_port, host.tokens["ro"]))
    assert "WEB_CONTENT_MARK" in out


def test_web_rw_token_is_404(shared):
    relay, host = shared

    async def get_status(path):
        async with aiohttp.ClientSession() as s:
            async with s.get(f"http://localhost:{relay.web_port}{path}") as r:
                return r.status

    # the rw token does NOT open the web view
    assert asyncio.run(get_status(f"/s/{host.tokens['rw']}/stream")) == 404
    assert asyncio.run(get_status(f"/s/{host.tokens['rw']}/cast")) == 404


def test_web_cast_download(shared):
    relay, host = shared
    host.run_cmd("echo RECORD_ME")

    async def fetch():
        async with aiohttp.ClientSession() as s:
            async with s.get(
                    f"http://localhost:{relay.web_port}/s/{host.tokens['ro']}/cast") as r:
                return r.status, r.headers.get("Content-Type"), await r.text()

    status, ctype, body = asyncio.run(fetch())
    assert status == 200
    assert "asciicast" in (ctype or "")
    first = body.splitlines()[0]
    assert '"version": 2' in first or '"version":2' in first


def test_web_security_headers(shared):
    relay, host = shared

    async def headers():
        async with aiohttp.ClientSession() as s:
            async with s.get(
                    f"http://localhost:{relay.web_port}/s/{host.tokens['ro']}") as r:
                return dict(r.headers)

    h = asyncio.run(headers())
    assert h.get("Referrer-Policy") == "no-referrer"
    assert h.get("X-Content-Type-Options") == "nosniff"
