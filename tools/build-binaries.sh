#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build a self-contained, single-file `choom` binary into OUT (default:
# ./downloads). Copy OUT/* onto the relay's downloads volume (CHOOM_DOWNLOADS,
# default /choom/data/downloads) to offer them at /download/.
#
#   ./tools/build-binaries.sh [OUT_DIR]
#   MODE=docker ./tools/build-binaries.sh        # default: Nuitka in a container
#                                                #   -> compiled, small, glibc
#   MODE=nix    ./tools/build-binaries.sh        # self-extracting Nix bundle,
#                                                #   no extra tools, larger (~100MB)
#
# A binary is specific to its CPU arch AND libc (musl vs glibc) — build it for the
# target platform. The filename encodes the arch (choom-linux-x86_64 / -aarch64).
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$HERE/downloads}"
MODE="${MODE:-docker}"
PLATFORM="${PLATFORM:-}"           # e.g. linux/arm64 (needs qemu/binfmt)
mkdir -p "$OUT"

case "$MODE" in
  docker)
    # Nuitka build in python:3-slim -> portable glibc binary (see binary.Dockerfile).
    args=(build -f "$HERE/tools/binary.Dockerfile" --target export -o "$OUT")
    [ -n "$PLATFORM" ] && args+=(--platform "$PLATFORM")
    docker "${args[@]}" "$HERE"
    ;;
  nix)
    sys="$(uname -m)-linux"
    dest="$OUT/choom-linux-$(uname -m)"
    nix bundle "$HERE#packages.$sys.choom" -o "$dest.link"
    cp "$(readlink -f "$dest.link")" "$dest"; rm -f "$dest.link"; chmod +x "$dest"
    ;;
  *)
    echo "unknown MODE='$MODE' (use 'docker' or 'nix')" >&2; exit 2 ;;
esac

echo "done -> $OUT/"
ls -lh "$OUT"/choom-linux-* 2>/dev/null || true
