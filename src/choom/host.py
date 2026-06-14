#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""choom host -- share the local shell through a choom relay.

Starts $SHELL in a local PTY and relays it over two SSH channels (control = JSON,
data = raw bytes) to the server. Viewers connect with a plain `ssh <token>@server`.
The shell runs exclusively here (on the host) -- the server never executes code.
"""

from __future__ import annotations

import argparse
import asyncio
import fcntl
import os
import re
import signal
import struct
import sys
import termios
import tty

import asyncssh

from . import protocol as P

# Terminal Device-Attributes replies (e.g. ESC[?62;52;c, ESC[>1;95;0c). A terminal
# auto-sends these in answer to an app's DA query; in a byte relay they can arrive
# after the app already exited and then show up at the shell prompt as garbage.
# They are never something a human types, so strip them from input to the shell.
_DA_REPLY = re.compile(rb"\x1b\[[?>][0-9;]*c")


def strip_terminal_replies(data: bytes) -> bytes:
    return _DA_REPLY.sub(b"", data)


def set_winsize(fd: int, cols: int, rows: int):
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


def term_size() -> tuple[int, int]:
    try:
        sz = os.get_terminal_size(0)
        if sz.columns and sz.lines:   # 0x0 (unconfigured PTY) is invalid
            return sz.columns, sz.lines
    except OSError:
        pass
    return 80, 24


class Host:
    def __init__(self, args):
        self.args = args
        self.master = -1
        self.child = -1
        self.dproc = None            # data channel (SSHClientProcess)
        self.cproc = None            # control channel
        self.reconnect_id = None
        self.ready = asyncio.Event()
        self.child_dead = asyncio.Event()
        self._sendbuf = bytearray()  # buffers PTY output during reconnect
        self._old_termios = None
        self.loop = None

    # -- local PTY + shell --------------------------------------------------
    def spawn_shell(self):
        cols, rows = term_size()
        pid, master = os.forkpty()
        if pid == 0:
            shell = self.args.command or os.environ.get("SHELL") or "/bin/sh"
            os.environ["CHOOM"] = "1"
            try:
                os.execvp(shell, [shell])
            except OSError:
                try:
                    os.execvp("/bin/sh", ["/bin/sh"])
                except OSError:
                    pass
            os._exit(127)
        self.child = pid
        self.master = master
        set_winsize(master, cols, rows)
        os.set_blocking(master, False)
        self.loop.add_reader(master, self._on_master)

    def _on_master(self):
        try:
            data = os.read(self.master, 65536)
        except OSError:
            data = b""
        if not data:
            self.loop.remove_reader(self.master)
            self.child_dead.set()
            return
        os.write(1, data)                  # host sees its own shell
        if self.dproc is not None:
            try:
                self.dproc.stdin.write(data)
            except Exception:
                self._sendbuf.extend(data)
        else:
            self._sendbuf.extend(data)     # buffer while reconnecting

    def _on_stdin(self):
        try:
            data = os.read(0, 65536)
        except OSError:
            return
        if data:
            os.write(self.master, strip_terminal_replies(data))  # host types itself

    def _on_winch(self):
        cols, rows = term_size()
        if self.cproc is not None:
            try:
                self.cproc.stdin.write(P.encode_frame(
                    {"t": P.MSG_HOST_SIZE, "cols": cols, "rows": rows}))
            except Exception:
                pass

    # -- switch the local terminal to raw mode / restore it -----------------
    def raw_mode(self):
        try:
            self._old_termios = termios.tcgetattr(0)
            tty.setraw(0)
            self.loop.add_reader(0, self._on_stdin)
            self.loop.add_signal_handler(signal.SIGWINCH, self._on_winch)
        except Exception:
            pass

    def restore(self):
        try:
            self.loop.remove_reader(0)
        except Exception:
            pass
        if self._old_termios is not None:
            try:
                termios.tcsetattr(0, termios.TCSADRAIN, self._old_termios)
            except Exception:
                pass

    # -- control frames from the server -------------------------------------
    def _handle_control(self, f):
        t = f.get("t")
        if t == P.MSG_READY:
            self.reconnect_id = f.get("id")
            self._print_links(f)
            self.ready.set()
        elif t == P.MSG_RESIZE:
            cols, rows = f.get("cols", 80), f.get("rows", 24)
            try:
                set_winsize(self.master, cols, rows)
            except Exception:
                pass
        elif t == P.MSG_JOIN:
            c = f.get("client", {})
            self._notice(f"viewer #{c.get('id')} joined "
                         f"({c.get('mode')}/{c.get('transport')}, {c.get('ip')})")
        elif t == P.MSG_LEAVE:
            self._notice(f"viewer #{f.get('id')} left")
        elif t == P.MSG_ERROR:
            self._notice("server error: " + str(f.get("msg")))

    def _notice(self, msg: str):
        os.write(1, f"\r\n\x1b[36m[choom] {msg}\x1b[0m\r\n".encode())

    def _print_links(self, f):
        host, port = f.get("host", "localhost"), f.get("port", P.DEFAULT_SSH_PORT)
        pa = "" if port == 22 else f" -p {port}"
        lines = [
            "",
            "\x1b[1;33mWelcome, Choomba, your shell is now shared!\x1b[0m",
            "Share a link below; viewers connect with their ssh client.",
            "",
            f"  Read-only:  ssh{pa} {f.get('ro')}@{host}",
            f"  \x1b[33mRead-write: ssh{pa} {f.get('rw')}@{host}\x1b[0m  (full shell access!)",
        ]
        if f.get("web"):
            lines.append(f"  Web (RO):   {f.get('web')}")
        if f.get("fp"):
            lines.append(f"  server key: {f.get('fp')}")
        lines.append("")
        # Leading "\r" forces column 0 even if the shell prompt left the cursor
        # mid-line (the terminal is in raw mode here).
        os.write(1, ("\r" + "\r\n".join(lines) + "\r\n").encode())

    # -- run a single connection --------------------------------------------
    async def _serve(self, conn):
        self.cproc = await conn.create_process(P.CONTROL_CMD, encoding=None)
        # hello or resume
        cols, rows = term_size()
        if self.reconnect_id is None:
            self.cproc.stdin.write(P.encode_frame(
                {"t": P.MSG_HELLO, "cols": cols, "rows": rows,
                 "record": bool(getattr(self.args, "record", False))}))
        else:
            self.cproc.stdin.write(P.encode_frame(
                {"t": P.MSG_RESUME, "id": self.reconnect_id,
                 "cols": cols, "rows": rows}))

        async def pump_control():
            reader = P.FrameReader()
            while True:
                data = await self.cproc.stdout.read(65536)
                if not data:
                    break
                for fr in reader.feed(data):
                    self._handle_control(fr)

        ctl_task = asyncio.ensure_future(pump_control())

        # wait for READY, then open the data channel
        await asyncio.wait_for(self.ready.wait(), timeout=10)
        self.dproc = await conn.create_process(P.DATA_CMD, encoding=None)
        # flush output buffered during the reconnect gap
        if self._sendbuf:
            self.dproc.stdin.write(bytes(self._sendbuf))
            self._sendbuf.clear()

        async def pump_data_in():
            while True:
                data = await self.dproc.stdout.read(65536)
                if not data:
                    break
                os.write(self.master, strip_terminal_replies(data))  # RW viewer keys

        data_task = asyncio.ensure_future(pump_data_in())
        closed = asyncio.ensure_future(conn.wait_closed())
        done, _ = await asyncio.wait(
            [closed, asyncio.ensure_future(self.child_dead.wait())],
            return_when=asyncio.FIRST_COMPLETED)
        for tsk in (ctl_task, data_task, closed):
            tsk.cancel()
        self.dproc = None
        self.cproc = None

    async def run(self):
        self.loop = asyncio.get_running_loop()
        self.spawn_shell()
        self.raw_mode()
        a = self.args
        try:
            while not self.child_dead.is_set():
                self.ready.clear()
                try:
                    async with asyncssh.connect(
                        a.server, a.port,
                        username=(P.USER_NEW if self.reconnect_id is None
                                  else P.USER_RESUME),
                        client_keys=P.client_keys(a.identity, not a.no_default_key),
                        known_hosts=None,
                    ) as conn:
                        await self._serve(conn)
                except (OSError, asyncssh.Error, asyncio.TimeoutError) as e:
                    if self.child_dead.is_set():
                        break
                    self._notice(f"disconnected ({e}); reconnecting...")
                    await asyncio.sleep(1.0)
                    if self.reconnect_id is None:
                        # never became ready -> give up
                        break
        finally:
            self.restore()
            try:
                os.close(self.master)
            except OSError:
                pass
            os.write(1, b"\r\n[choom] session ended.\r\n")


def main():
    ap = argparse.ArgumentParser(description="choom host -- share your shell")
    ap.add_argument("-c", "--config", default="",
                    help="YAML config file (CLI flags override it)")
    ap.add_argument("-s", "--server", default=P.DEFAULT_SERVER)
    ap.add_argument("-p", "--port", type=int, default=P.DEFAULT_SSH_PORT)
    ap.add_argument("-i", "--identity", action="append",
                    help="private SSH key (may be given multiple times)")
    ap.add_argument("--no-default-key", action="store_true",
                    help="do NOT use the built-in default key")
    ap.add_argument("--record", action="store_true",
                    help="ask the relay to record this session (asciicast)")
    ap.add_argument("command", nargs="?", help="run this instead of $SHELL")
    args = P.apply_config(ap, ap.parse_args())
    if not sys.stdin.isatty():
        print("choom host: stdin is not a TTY", file=sys.stderr)
        sys.exit(1)
    try:
        asyncio.run(Host(args).run())
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
