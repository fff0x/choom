# syntax=docker/dockerfile:1
# Build a portable, single-file `choom` binary with Nuitka (glibc).
#
#   docker build -f tools/binary.Dockerfile --target export -o downloads .
#   docker build -f tools/binary.Dockerfile --platform linux/arm64 \
#           --target export -o downloads .            # needs qemu/binfmt
#
# The result is downloads/choom-linux-<arch> (compiled, self-contained). It is
# glibc-linked, so it runs on mainstream Linux distros (not Alpine/musl).

FROM python:3.12-slim AS build
ARG TARGETARCH
RUN apt-get update \
  && apt-get install -y --no-install-recommends build-essential patchelf \
  && rm -rf /var/lib/apt/lists/*

COPY pyproject.toml README.md LICENSE /src/
COPY src /src/src

RUN pip install --no-cache-dir /src nuitka

# Map docker's TARGETARCH (amd64/arm64) to the uname-style arch in the filename.
RUN set -eu; \
  case "${TARGETARCH}" in amd64) A=x86_64 ;; arm64) A=aarch64 ;; *) A="$(uname -m)" ;; esac; \
  printf 'from choom.cli import main\nraise SystemExit(main())\n' > /src/_entry.py; \
  cd /src; \
  python -m nuitka --onefile --standalone --assume-yes-for-downloads \
    --include-package=choom --include-package-data=choom \
    --output-dir=/out --output-filename="choom-linux-$A" /src/_entry.py; \
  ls -lh "/out/choom-linux-$A"

FROM scratch AS export
COPY --from=build /out/choom-linux-* /
