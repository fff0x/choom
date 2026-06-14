# SPDX-License-Identifier: GPL-3.0-or-later
"""Web module: read-only stream, rw->404, asciicast download, security headers."""

import asyncio
import json
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


def test_web_sends_size_init(shared):
    relay, host = shared

    async def first_text():
        async with aiohttp.ClientSession() as s:
            async with s.ws_connect(
                    f"http://localhost:{relay.web_port}/s/{host.tokens['ro']}/stream") as ws:
                msg = await asyncio.wait_for(ws.receive(), timeout=3)
                return msg.data

    raw = asyncio.run(first_text())
    m = json.loads(raw)
    # browser needs the host grid size so xterm matches (no cut-off)
    assert m["t"] == "init"
    assert m["cols"] == 80 and m["rows"] == 24       # host's native PTY size
    assert m["rec"] is True                          # relay fixture enables -r


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


def test_web_downloads(shared):
    relay, host = shared

    async def get(path):
        async with aiohttp.ClientSession() as s:
            async with s.get(f"http://localhost:{relay.web_port}{path}") as r:
                return r.status, await r.text()

    # empty downloads dir -> 200 with an install hint, no file links
    status, body = asyncio.run(get("/download/"))
    assert status == 200 and "pip install choom" in body

    # drop a "binary" and it shows up + downloads
    import os
    with open(os.path.join(relay.downloads, "choom-linux-x86_64"), "wb") as f:
        f.write(b"BINARY-CONTENT")
    status, body = asyncio.run(get("/download/"))
    assert "choom-linux-x86_64" in body
    status, body = asyncio.run(get("/download/choom-linux-x86_64"))
    assert status == 200 and body == "BINARY-CONTENT"

    # no path traversal
    status, _ = asyncio.run(get("/download/..%2f..%2fhost_key"))
    assert status == 404


def test_web_landing_page(shared):
    relay, host = shared

    async def fetch():
        async with aiohttp.ClientSession() as s:
            async with s.get(f"http://localhost:{relay.web_port}/") as r:
                return r.status, await r.text()

    status, body = asyncio.run(fetch())
    assert status == 200
    # the landing page (not the viewer) greets + warns about the public relay
    assert "Welcome, Choomba." in body
    assert "Public relay" in body
    assert "READ-ONLY" not in body          # that string belongs to the viewer


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
