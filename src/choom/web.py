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
import json
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


async def _landing(request):
    # `/` with no token: info + usage + public-relay warning, not the viewer.
    return web.FileResponse(os.path.join(STATIC, "landing.html"))


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

    if not sess.can_add_viewer():
        raise web.HTTPServiceUnavailable(text="session is full")

    ws = web.WebSocketResponse()
    await ws.prepare(request)

    queue: asyncio.Queue = asyncio.Queue(maxsize=2048)

    def send(data: bytes):
        try:
            queue.put_nowait(data)
        except asyncio.QueueFull:
            pass  # slow client -> drop frames instead of blocking

    # Terminal-size updates (queued as dicts; raw output is queued as bytes) let the
    # browser size xterm.js to the host's grid so nothing is cut off.
    def on_resize(cols: int, rows: int):
        try:
            queue.put_nowait({"t": "size", "cols": cols, "rows": rows})
        except asyncio.QueueFull:
            pass

    # One-time init: current size + whether a downloadable recording exists.
    await ws.send_str(json.dumps({
        "t": "init",
        "cols": sess.neg_cols,
        "rows": sess.neg_rows,
        "rec": sess.recorder is not None,
    }))

    ip = request.remote or "?"
    p = sess.add_participant("ro", "web", ip, 0, 0, send, on_resize=on_resize)

    async def pump():
        while True:
            item = await queue.get()
            if isinstance(item, (bytes, bytearray)):
                await ws.send_bytes(item)
            else:
                await ws.send_str(json.dumps(item))

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


_INSTALL_HINT = (
    "<p>No prebuilt binaries are published here. Install via Nix or pip:</p>"
    "<pre>nix run git+https://seed.this-is-fine.io/"
    "zc53mExb6ttoNQXMMZh9xxn5n2HH.git -- host\n"
    "pip install choom &amp;&amp; choom host</pre>"
)


def _dl_page(body):
    return web.Response(
        text=("<!DOCTYPE html><meta charset=utf-8><title>choom downloads</title>"
              "<meta name=referrer content=no-referrer>"
              "<body style='background:#0b0b0b;color:#ddd;font-family:monospace;"
              "max-width:760px;margin:2rem auto;padding:0 1rem'>"
              "<h1 style='color:#fa0'>choom &mdash; downloads</h1>" + body +
              "<p><a style=color:#6cf href='/'>&larr; back</a></p></body>"),
        content_type="text/html")


def _downloads_list(app):
    d = getattr(app, "downloads_dir", "")
    if not d or not os.path.isdir(d):
        return []
    out = []
    for n in sorted(os.listdir(d)):
        p = os.path.join(d, n)
        if n.startswith(".") or not os.path.isfile(p):
            continue
        out.append((n, os.path.getsize(p)))
    return out


async def _download_index(request):
    files = _downloads_list(request.app["choom"])
    if not files:
        return _dl_page(_INSTALL_HINT)
    rows = "".join(
        f"<li><a style=color:#6cf href='/download/{n}'>{n}</a> "
        f"<span style=color:#666>({sz // 1024} KiB)</span></li>"
        for n, sz in files)
    return _dl_page(f"<ul>{rows}</ul>" + _INSTALL_HINT)


async def _download_file(request):
    app = request.app["choom"]
    d = getattr(app, "downloads_dir", "")
    name = request.match_info["name"]
    # No path traversal: only plain filenames from the downloads dir.
    if not d or "/" in name or "\\" in name or name.startswith(".") or ".." in name:
        raise web.HTTPNotFound()
    path = os.path.join(d, name)
    if not os.path.isfile(path):
        raise web.HTTPNotFound()
    return web.FileResponse(
        path,
        headers={"Content-Disposition": f'attachment; filename="{name}"'})


async def start_web(choom_app, host: str, port: int):
    application = web.Application(middlewares=[_security_headers])
    application["choom"] = choom_app
    application.add_routes([
        web.get("/", _landing),
        web.get("/download/", _download_index),
        web.get("/download/{name}", _download_file),
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
