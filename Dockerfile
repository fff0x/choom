# syntax=docker/dockerfile:1
# Dockerfile version: 0.0.1

##
## BUILD
##

FROM alpine:3.22.4 AS build

# Build a self-contained virtualenv with choom + its dependencies. asyncssh and
# pyte are pure Python; aiohttp and cryptography (pulled in via asyncssh) ship
# prebuilt musllinux wheels for x86_64/aarch64, so no compiler is needed here.
#
# FALLBACK: if a dependency has no musllinux wheel for the target arch, pip will
# try to build from source. In that case add the toolchain below, e.g.:
#   RUN apk add --no-cache gcc musl-dev libffi-dev openssl-dev python3-dev cargo
RUN apk add --no-cache --update python3 py3-pip

COPY pyproject.toml README.md LICENSE /build/
COPY src /build/src

RUN python3 -m venv /opt/venv \
  && /opt/venv/bin/pip install --no-cache-dir /build

##
## MAIN
##

FROM alpine:3.22.4

##
## VARIABLES
##

ARG CAT=choom
ARG APP=relay
ARG APP_VERSION=0.3.3
ARG APP_DESCRIPTION="choom — minimal SSH terminal sharing relay"
# ------------------

LABEL \
  org.opencontainers.image.vendor="this-is-fine" \
  org.opencontainers.image.url="https://this-is-fine.io" \
  org.opencontainers.image.authors="Max Reineke <ff0x@this-is-fine.io>" \
  org.opencontainers.image.title="${CAT}-${APP}" \
  org.opencontainers.image.version="${APP_VERSION}" \
  org.opencontainers.image.description="$APP_DESCRIPTION" \
  org.opencontainers.image.licenses="GPL-3.0-or-later" \
  org.opencontainers.image.source="https://radicle.network/nodes/seed.this-is-fine.io/rad:zc53mExb6ttoNQXMMZh9xxn5n2HH" 

##
## CONFIGURATION
##

ARG CAT_GID=10001
ARG APP_UID=10001
ARG TARGETARCH

##
## PREPARATION
##

USER root

# GENERIC

COPY --chmod=755 ep /ep

RUN \
  addgroup -S -g "$CAT_GID" "$CAT" \
  && adduser -S -u "$APP_UID" -G "$CAT" -h "/${CAT}" -s /sbin/nologin -D "$APP"

RUN \
  apk upgrade --update --no-cache \
  && apk add --no-cache tzdata \
  && rm -rf /tmp/* /var/cache/apk/* /etc/localtime \
  && ln -sf /usr/share/zoneinfo/UTC /etc/localtime

# APP

COPY --from=build /opt/venv /opt/venv

# python3 provides the interpreter the venv links against; openssh-keygen lets the
# entrypoint print host-key fingerprints; libgcc backs the cryptography wheel.
RUN \
  apk add --update --no-cache \
  python3 \
  libgcc \
  openssh-keygen \
  && /opt/venv/bin/python -c "import choom.server, choom.web, asyncssh, pyte, aiohttp" \
  && /opt/venv/bin/choom --version \
  && mkdir -p "/${CAT}/data" \
  && chown -R "$APP":"$CAT" "/${CAT}"

##
## ENVIRONMENT
##

# choom never runs a shell itself — it only relays bytes — so it runs unprivileged
# (uid 10001), unlike tmate-ssh-server which needs root + SYS_ADMIN for jailing.
USER "$APP"
WORKDIR "/${CAT}"
VOLUME [ "/${CAT}/data" ]
ENV PATH="/opt/venv/bin:$PATH" \
  CHOOM_DATA="/${CAT}/data" \
  PYTHONUNBUFFERED="1" \
  PYTHONDONTWRITEBYTECODE="1"

##
## PORTS
##

# 2200 = SSH relay (hosts + viewers) · 8080 = read-only web view
EXPOSE 2200 8080

##
## INIT
##

ENTRYPOINT [ "/ep" ]
