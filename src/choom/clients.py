#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""choom clients -- list the active viewers of your own session.

Connects to the server with a public key (the owner key) and queries the viewer
roster of your own session.
"""

from __future__ import annotations

import argparse
import asyncio
import sys

import asyncssh

from . import protocol as P


def fmt_secs(s: int) -> str:
    m, s = divmod(int(s), 60)
    h, m = divmod(m, 60)
    if h:
        return f"{h}h{m:02d}m"
    if m:
        return f"{m}m{s:02d}s"
    return f"{s}s"


async def amain(args) -> int:
    async with asyncssh.connect(
        args.server, args.port, username=P.USER_QUERY,
        client_keys=P.client_keys(args.identity, not args.no_default_key),
        known_hosts=None,
    ) as conn:
        proc = await conn.create_process(P.CONTROL_CMD, encoding=None)
        reader = P.FrameReader()
        clients = None
        while clients is None:
            data = await proc.stdout.read(65536)
            if not data:
                break
            for f in reader.feed(data):
                if f.get("t") == P.MSG_CLIENTS:
                    clients = f.get("list", [])

    clients = clients or []
    if not clients:
        print("No active viewers.")
        return 0
    print(f"{'ID':>3}  {'MODE':<4}  {'TRANSPORT':<9}  {'IP':<16}  {'SIZE':<9}  CONNECTED")
    for c in clients:
        size = f"{c.get('cols')}x{c.get('rows')}"
        print(f"{c.get('id'):>3}  {c.get('mode','?').upper():<4}  "
              f"{c.get('transport','?'):<9}  {c.get('ip','?'):<16}  "
              f"{size:<9}  {fmt_secs(c.get('secs', 0))}")
    return 0


def main():
    ap = argparse.ArgumentParser(description="choom clients -- list active viewers")
    ap.add_argument("-c", "--config", default="",
                    help="YAML config file (CLI flags override it)")
    ap.add_argument("-s", "--server", default=P.DEFAULT_SERVER)
    ap.add_argument("-p", "--port", type=int, default=P.DEFAULT_SSH_PORT)
    ap.add_argument("-i", "--identity", action="append",
                    help="private SSH key (may be given multiple times)")
    ap.add_argument("--no-default-key", action="store_true",
                    help="do NOT use the built-in default key")
    args = P.apply_config(ap, ap.parse_args())
    try:
        sys.exit(asyncio.run(amain(args)))
    except asyncssh.Error as e:
        print(f"choom clients: {e}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
