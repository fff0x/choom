# SPDX-License-Identifier: GPL-3.0-or-later
"""asciicast recording: file permissions, header, output and resize events."""

import asyncio
import json
import os
import stat

from harness import _size_while_viewer  # noqa: F401  (Resize ausloesen)


def _cast_path(relay, host):
    return os.path.join(relay.record_dir, host.tokens["ro"] + ".cast")


def test_cast_file_perms_and_header(shared):
    relay, host = shared
    host.run_cmd("echo RECORDED_OUTPUT")
    path = _cast_path(relay, host)
    assert os.path.exists(path)

    # 0600 -- recordings may contain secrets
    mode = stat.S_IMODE(os.stat(path).st_mode)
    assert mode == 0o600, oct(mode)

    lines = open(path, encoding="utf-8").read().splitlines()
    header = json.loads(lines[0])
    assert header["version"] == 2
    assert header["width"] > 0 and header["height"] > 0

    # at least one output event that contains the content
    events = [json.loads(l) for l in lines[1:] if l.strip()]
    assert any(e[1] == "o" for e in events)
    assert any("RECORDED_OUTPUT" in e[2] for e in events if e[1] == "o")


def test_cast_records_resize_event(shared):
    relay, host = shared
    # connect a small RW viewer -> triggers a size change
    asyncio.run(_size_while_viewer(
        relay.ssh_port, host.tokens["rw"], (40, 10), host))
    path = _cast_path(relay, host)
    events = [json.loads(l) for l in open(path, encoding="utf-8").read().splitlines()[1:]
              if l.strip()]
    assert any(e[1] == "r" for e in events), "no resize event recorded"
