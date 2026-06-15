#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Generate the pyte golden screen fixture for the avt fidelity spike.

Feeds a representative full-screen redraw (the join-snapshot scenario: clear, CUP,
colors, autowrap, erase, final cursor) into pyte — the choom 1.x screen model — and
dumps the resulting visible grid + cursor as JSON. The Rust test feeds the *same*
input into `avt` and asserts the grids match, proving avt is a faithful replacement.

Run from anywhere:  python3 gen_pyte_golden.py  (writes screen_golden.json alongside)
"""
import json
import os

import pyte

COLS, ROWS = 20, 8

# Exercise: clear+home, plain text + CRLF, SGR colors, absolute CUP, autowrap across
# the right margin, and an explicit final cursor position.
INPUT = (
    "\x1b[2J\x1b[H"
    "Header line\r\n"
    "\x1b[31mred\x1b[0m-\x1b[32mgreen\x1b[0m\r\n"
    "\x1b[5;10HX"
    "\x1b[3;1Hwrapped " + ("Z" * 30) +
    "\x1b[8;1Hdone"
    "\x1b[8;5H"
)

screen = pyte.Screen(COLS, ROWS)
stream = pyte.Stream(screen)
stream.feed(INPUT)

out = {
    "cols": COLS,
    "rows": ROWS,
    "input": INPUT,
    "display": list(screen.display),
    "cursor": {"col": screen.cursor.x, "row": screen.cursor.y},
}

dst = os.path.join(os.path.dirname(os.path.abspath(__file__)), "screen_golden.json")
with open(dst, "w") as f:
    json.dump(out, f, indent=2, ensure_ascii=False)
    f.write("\n")
print(f"wrote {dst}")
