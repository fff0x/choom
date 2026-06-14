# SPDX-License-Identifier: GPL-3.0-or-later
"""Unified `choom` CLI: dispatches to server / host / clients.

    choom server  [options]   -- start the central relay
    choom host    [options]   -- share your own shell
    choom clients [options]   -- list active viewers of your own session
"""

from __future__ import annotations

import sys

from . import __version__

_USAGE = """choom {ver} -- minimal SSH terminal sharing relay

usage: choom <command> [options]

commands:
  server    start the central SSH relay
  host      share your own shell (read-only / read-write links)
  clients   list active viewers of your own session

'choom <command> -h' shows the options for that command.
""".format(ver=__version__)


def main(argv=None):
    argv = list(sys.argv if argv is None else argv)
    if len(argv) < 2 or argv[1] in ("-h", "--help", "help"):
        print(_USAGE)
        return 0
    if argv[1] in ("-V", "--version"):
        print(f"choom {__version__}")
        return 0

    cmd, rest = argv[1], argv[2:]
    # Subcommands run their own argparse on sys.argv[1:].
    sys.argv = [f"choom {cmd}"] + rest

    if cmd == "server":
        from . import server
        return server.main()
    if cmd == "host":
        from . import host
        return host.main()
    if cmd == "clients":
        from . import clients
        return clients.main()

    print(f"choom: unknown command '{cmd}'\n", file=sys.stderr)
    print(_USAGE, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
