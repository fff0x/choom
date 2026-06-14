# SPDX-License-Identifier: GPL-3.0-or-later
"""YAML config loading: config provides defaults, CLI flags override them."""

import argparse
import sys

from choom import protocol as P


def _parser():
    ap = argparse.ArgumentParser()
    ap.add_argument("-c", "--config", default="")
    ap.add_argument("--max-viewers", type=int, default=50)
    ap.add_argument("--no-default-key", action="store_true")
    return ap


def test_config_sets_defaults(tmp_path, monkeypatch):
    cfg = tmp_path / "c.yaml"
    cfg.write_text("max-viewers: 7\nno-default-key: true\n")
    ap = _parser()
    monkeypatch.setattr(sys, "argv", ["choom", "-c", str(cfg)])
    args = P.apply_config(ap, ap.parse_args())
    assert args.max_viewers == 7
    assert args.no_default_key is True


def test_cli_overrides_config(tmp_path, monkeypatch):
    cfg = tmp_path / "c.yaml"
    cfg.write_text("max-viewers: 7\n")
    ap = _parser()
    monkeypatch.setattr(sys, "argv", ["choom", "-c", str(cfg), "--max-viewers", "3"])
    args = P.apply_config(ap, ap.parse_args())
    assert args.max_viewers == 3


def test_no_config_is_noop(monkeypatch):
    ap = _parser()
    monkeypatch.setattr(sys, "argv", ["choom"])
    args = P.apply_config(ap, ap.parse_args())
    assert args.max_viewers == 50
