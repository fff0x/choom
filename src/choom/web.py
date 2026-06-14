# SPDX-License-Identifier: GPL-3.0-or-later
"""choom web -- read-only browser view + asciicast download.

Strictly output-only: the WebSocket only pushes server->browser (snapshot + live
output); incoming frames are ignored. There is no input path -- the browser is
structurally unable to send keystrokes.

Auth is solely via the ro_token in the URL (no cookies -> CSWSH/CSRF defused by
construction). The rw_token does NOT open the web view.
"""

from __future__ import annotations

import asyncio
import os

from aiohttp import web

from . import protocol as P

STATIC = os.path.join(os.path.dirname(__file__), "static")


@web.middleware
async def _security_headers(request, handler):
    resp = await handler(request)
    resp.headers["Referrer-Policy"] = "no-referrer"
    resp.headers["X-Content-Type-Options"] = "nosniff"
    return resp


def _ro_session(app, token):
    """Return the session if token is exactly the ro_token -- otherwise None."""
    match = app.lookup_token(token)
    if match is None or match[1] != "ro":
        return None
    return match[0]


async def _index(request):
    return web.FileResponse(os.path.join(STATIC, "index.html"))


async def _cast(request):
    app = request.app["choom"]
    sess = _ro_session(app, request.match_info["token"])
    if sess is None or sess.recorder is None or not os.path.exists(sess.recorder.path):
        raise web.HTTPNotFound()
    return web.FileResponse(
        sess.recorder.path,
        headers={
            "Content-Type": "application/x-asciicast",
            "Content-Disposition": 'attachment; filename="choom-session.cast"',
        },
    )


async def _stream(request):
    app = request.app["choom"]
    sess = _ro_session(app, request.match_info["token"])
    if sess is None:
        raise web.HTTPNotFound()

    ws = web.WebSocketResponse()
    await ws.prepare(request)

    queue: asyncio.Queue = asyncio.Queue(maxsize=2048)

    def send(data: bytes):
        try:
            queue.put_nowait(data)
        except asyncio.QueueFull:
            pass  # slow client -> drop frames instead of blocking

    ip = request.remote or "?"
    p = sess.add_participant("ro", "web", ip, 0, 0, send)

    async def pump():
        while True:
            data = await queue.get()
            await ws.send_bytes(data)

    sender = asyncio.ensure_future(pump())
    try:
        # Incoming frames are deliberately ignored (output-only); this loop only
        # serves to detect when the connection closes.
        async for _msg in ws:
            pass
    finally:
        sender.cancel()
        sess.remove_participant(p.id)
        await ws.close()
    return ws


async def start_web(choom_app, host: str, port: int):
    application = web.Application(middlewares=[_security_headers])
    application["choom"] = choom_app
    application.add_routes([
        web.get("/", _index),
        web.get("/s/{token}", _index),
        web.get("/s/{token}/stream", _stream),
        web.get("/s/{token}/cast", _cast),
        web.static("/static", STATIC),
    ])
    runner = web.AppRunner(application, access_log=None)
    await runner.setup()
    site = web.TCPSite(runner, host, port)
    await site.start()
    return runner
