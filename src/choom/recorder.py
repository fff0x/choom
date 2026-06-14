# SPDX-License-Identifier: GPL-3.0-or-later
"""asciicast v2 recording of a session (capture + download).

Format: first line = JSON header, then one JSON line per event
  [t, "o", "<output>"]       terminal output at time t (seconds since start)
  [t, "r", "<cols>x<rows>"]  size change

Only terminal OUTPUT is recorded, never viewer input. Files are created with
0600 permissions -- recordings may contain secrets.
"""

from __future__ import annotations

import json
import os
import time


class Recorder:
    def __init__(self, directory: str, name: str, cols: int, rows: int):
        os.makedirs(directory, exist_ok=True)
        self.path = os.path.join(directory, f"{name}.cast")
        # Force 0600, even when the file is freshly created:
        fd = os.open(self.path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        self._f = os.fdopen(fd, "w", encoding="utf-8")
        self._start = time.monotonic()
        header = {"version": 2, "width": cols, "height": rows,
                  "timestamp": int(time.time())}
        self._f.write(json.dumps(header) + "\n")
        self._f.flush()

    def _t(self) -> float:
        return round(time.monotonic() - self._start, 6)

    def _event(self, code: str, payload: str):
        if self._f is None:
            return
        try:
            self._f.write(json.dumps([self._t(), code, payload]) + "\n")
            self._f.flush()
        except Exception:
            pass

    def write_output(self, data: bytes):
        self._event("o", data.decode("utf-8", "replace"))

    def write_resize(self, cols: int, rows: int):
        self._event("r", f"{cols}x{rows}")

    def close(self):
        if self._f is not None:
            try:
                self._f.close()
            finally:
                self._f = None
