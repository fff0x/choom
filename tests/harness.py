# SPDX-License-Identifier: GPL-3.0-or-later
"""Helpers for the choom test suite: start relay/host, simulate viewers.

Deliberately built on subprocesses + real SSH connections (instead of mocks) so
the tests exercise the same code paths as real operation.
"""

from __future__ import annotations

import asyncio
import fcntl
import os
import pty
import re
import select
import socket
import struct
import subprocess
import termios
import time

import asyncssh

from choom import protocol as P

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CHOOM = os.path.join(ROOT, ".venv", "bin", "choom")
PYBIN = os.path.join(ROOT, ".venv", "bin", "python")


def free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def write_keypair(path: str):
    key = asyncssh.generate_private_key("ssh-ed25519")
    key.write_private_key(path)
    return key


def wait_port(port: int, timeout: float = 8.0):
    end = time.time() + timeout
    while time.time() < end:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.3):
                return True
        except OSError:
            time.sleep(0.1)
    raise TimeoutError(f"port {port} did not come up")


class Relay:
    """Starts `choom server` as a subprocess."""

    def __init__(self, workdir: str, no_default_key: bool = False):
        self.work = workdir
        self.no_default_key = no_default_key
        self.ssh_port = free_port()
        self.web_port = free_port()
        self.record_dir = os.path.join(workdir, "rec")
        self.downloads = os.path.join(workdir, "downloads")
        self.host_key = os.path.join(workdir, "host_key")
        self.authorized = os.path.join(workdir, "authorized_keys")
        self.owner_id = os.path.join(workdir, "owner")
        self.intruder_id = os.path.join(workdir, "intruder")
        self.proc = None

    def start(self):
        owner = write_keypair(self.owner_id)
        intruder = write_keypair(self.intruder_id)
        with open(self.authorized, "wb") as f:
            f.write(owner.export_public_key() + b"\n")
            f.write(intruder.export_public_key() + b"\n")
        os.makedirs(self.downloads, exist_ok=True)
        cmd = [CHOOM, "server", "-p", str(self.ssh_port), "-H", "localhost",
               "-k", self.host_key, "-a", self.authorized,
               "-w", f":{self.web_port}", "-r", self.record_dir,
               "--downloads", self.downloads]
        if self.no_default_key:
            cmd.append("--no-default-key")
        self.proc = subprocess.Popen(
            cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        wait_port(self.ssh_port)
        wait_port(self.web_port)
        return self

    def stop(self):
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(2)
            try:
                self.proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.proc.kill()


class Host:
    """Starts `choom host` in a PTY and parses the share tokens.

    identity=<path>  -> connect with that key (`-i`).
    identity=None    -> no `-i`, i.e. use the bundled default key.
    """

    def __init__(self, relay: Relay, identity: str | None = "owner",
                 record: bool = False):
        self.relay = relay
        self.identity = relay.owner_id if identity == "owner" else identity
        self.record = record
        self.master = -1
        self.proc = None
        self.tokens = {}
        self._buf = b""

    def start(self, timeout: float = 10.0):
        self.master, slave = pty.openpty()
        fcntl.ioctl(self.master, termios.TIOCSWINSZ,
                    struct.pack("HHHH", 24, 80, 0, 0))
        import shutil
        shell = shutil.which("bash") or shutil.which("sh") or "/bin/sh"
        env = dict(os.environ, SHELL=shell, PS1="$ ", TERM="xterm-256color")
        cmd = [CHOOM, "host", "-s", "localhost", "-p", str(self.relay.ssh_port)]
        if self.identity:
            cmd += ["-i", self.identity]
        if self.record:
            cmd += ["--record"]
        self.proc = subprocess.Popen(
            cmd, stdin=slave, stdout=slave, stderr=slave, env=env,
            start_new_session=True)
        os.close(slave)
        end = time.time() + timeout
        while time.time() < end and not ("ro" in self.tokens and "rw" in self.tokens):
            r, _, _ = select.select([self.master], [], [], 0.3)
            if self.master in r:
                try:
                    self._buf += os.read(self.master, 65536)
                except OSError:
                    break
            text = self._buf.decode("utf-8", "replace")
            m_ro = re.search(r"Read-only:\s+ssh.*?\s([A-Za-z0-9_-]{18,})@", text)
            m_rw = re.search(r"Read-write:\s+ssh.*?\s([A-Za-z0-9_-]{18,})@", text)
            if m_ro:
                self.tokens["ro"] = m_ro.group(1)
            if m_rw:
                self.tokens["rw"] = m_rw.group(1)
        if "ro" not in self.tokens or "rw" not in self.tokens:
            raise RuntimeError("tokens not parsed:\n" +
                               self._buf.decode("utf-8", "replace"))
        return self

    def send(self, data: bytes):
        os.write(self.master, data)

    def run_cmd(self, cmd: str, settle: float = 0.8):
        self.send(cmd.encode() + b"\r")
        time.sleep(settle)

    def drain(self, window: float = 0.3) -> str:
        """Read currently pending PTY output (non-blocking)."""
        out = b""
        end = time.time() + window
        while time.time() < end:
            r, _, _ = select.select([self.master], [], [], 0.05)
            if self.master in r:
                try:
                    out += os.read(self.master, 65536)
                except OSError:
                    break
            else:
                break
        return out.decode("utf-8", "replace")

    def stop(self):
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(2)
            try:
                self.proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        if self.master >= 0:
            try:
                os.close(self.master)
            except OSError:
                pass


# --- async viewer helpers --------------------------------------------------
async def viewer_capture(port, token, inject=None, inject_delay=0.6,
                         duration=1.8, term_size=(80, 24)):
    async with asyncssh.connect(
        "localhost", port, username=token, known_hosts=None,
        client_keys=[], agent_path=None, password="",
    ) as conn:
        proc = await conn.create_process(
            term_type="xterm-256color", term_size=term_size,
            encoding="utf-8", errors="replace")
        buf = []

        async def rd():
            while True:
                d = await proc.stdout.read(4096)
                if not d:
                    break
                buf.append(d)

        t = asyncio.ensure_future(rd())
        if inject:
            await asyncio.sleep(inject_delay)
            proc.stdin.write(inject)
        await asyncio.sleep(duration)
        t.cancel()
        return "".join(buf)


def capture(port, token, **kw):
    return asyncio.run(viewer_capture(port, token, **kw))


async def _size_while_viewer(port, token, term_size, host):
    """Connect a viewer and run `stty size` on the host (resize probe)."""
    async with asyncssh.connect(
        "localhost", port, username=token, known_hosts=None,
        client_keys=[], agent_path=None, password="",
    ) as conn:
        proc = await conn.create_process(
            term_type="xterm-256color", term_size=term_size,
            encoding="utf-8", errors="replace")
        buf = []

        async def rd():
            while True:
                d = await proc.stdout.read(4096)
                if not d:
                    break
                buf.append(d)

        t = asyncio.ensure_future(rd())
        await asyncio.sleep(0.6)          # attach + resize negotiation
        host.send(b"stty size\r")
        await asyncio.sleep(1.0)
        t.cancel()
        return "".join(buf)


# --- protocol level: "fake host" for deterministic reconnect tests ---------
class FakeHost:
    """Speaks the host protocol directly to control drops/resumes precisely."""

    def __init__(self, port, identity):
        self.port = port
        self.identity = identity
        self.conn = None
        self.ctl = None
        self.data = None
        self.ready = None

    async def connect(self, resume_id=None, cols=80, rows=24):
        user = P.USER_NEW if resume_id is None else P.USER_RESUME
        self.conn = await asyncssh.connect(
            "localhost", self.port, username=user,
            client_keys=[self.identity], known_hosts=None)
        self.ctl = await self.conn.create_process(P.CONTROL_CMD, encoding=None)
        if resume_id is None:
            self.ctl.stdin.write(P.encode_frame(
                {"t": P.MSG_HELLO, "cols": cols, "rows": rows}))
        else:
            self.ctl.stdin.write(P.encode_frame(
                {"t": P.MSG_RESUME, "id": resume_id, "cols": cols, "rows": rows}))
        reader = P.FrameReader()
        ok = False
        for _ in range(50):
            try:
                chunk = await asyncio.wait_for(self.ctl.stdout.read(65536), 2.0)
            except asyncio.TimeoutError:
                break
            if not chunk:
                break
            done = False
            for f in reader.feed(chunk):
                if f.get("t") == P.MSG_READY:
                    self.ready = f
                    ok = True
                    done = True
                elif f.get("t") == P.MSG_ERROR:
                    done = True
            if done:
                break
        if ok:
            self.data = await self.conn.create_process(P.DATA_CMD, encoding=None)
        return ok

    def emit(self, data: bytes):
        self.data.stdin.write(data)

    def drop(self):
        if self.conn:
            self.conn.abort()
            self.conn = None
            self.data = None
            self.ctl = None

    async def close(self):
        if self.conn:
            self.conn.close()
            try:
                await self.conn.wait_closed()
            except Exception:
                pass
