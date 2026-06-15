# syntax=docker/dockerfile:1
# choom 2.0 relay — a single static musl binary on `scratch`.

##
## BUILD — compile a static musl binary (alpine's rust targets musl natively)
##
FROM rust:alpine AS build

RUN apk add --no-cache musl-dev
WORKDIR /src

# Cargo manifests + lockfile first, so the dependency build layer caches across
# source-only changes.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# The release profile (workspace Cargo.toml) already strips, LTOs, and opt-z's.
RUN cargo build --release --locked -p choom-cli \
  && /src/target/release/choom --version

##
## MAIN — nothing but the binary
##
FROM scratch

ARG CAT=choom
ARG APP=relay
ARG APP_VERSION=2.0.1
ARG APP_DESCRIPTION="choom — minimal SSH terminal sharing relay"

LABEL \
  org.opencontainers.image.vendor="this-is-fine" \
  org.opencontainers.image.url="https://this-is-fine.io" \
  org.opencontainers.image.authors="Max 'ff0x' Reineke <ff0x@this-is-fine.io>" \
  org.opencontainers.image.title="${CAT}-${APP}" \
  org.opencontainers.image.version="${APP_VERSION}" \
  org.opencontainers.image.description="$APP_DESCRIPTION" \
  org.opencontainers.image.licenses="GPL-3.0-or-later" \
  org.opencontainers.image.source="https://radicle.network/nodes/seed.this-is-fine.io/rad:zc53mExb6ttoNQXMMZh9xxn5n2HH"

COPY --from=build /src/target/release/choom /choom

# choom never runs a shell itself — it only relays bytes — so it runs unprivileged,
# unlike tmate-ssh-server which needs root + SYS_ADMIN for jailing. `scratch` has no
# users, but a numeric uid is enough.
USER 10001:10001

# Configure entirely via CHOOM_* env / flags (clap reads the env vars directly):
#   CHOOM_SSH_ADDR, CHOOM_HOST, CHOOM_PORT, CHOOM_WEB_ADDR, CHOOM_WEB_URL,
#   CHOOM_RECORD_DIR, CHOOM_LOG_FORMAT=json, RUST_LOG
ENV CHOOM_SSH_ADDR="0.0.0.0:2200" \
    CHOOM_WEB_ADDR="0.0.0.0:8080" \
    CHOOM_LOG_FORMAT="json"

# 2200 = SSH relay (hosts + viewers) · 8080 = read-only web view + /metrics
EXPOSE 2200 8080

ENTRYPOINT ["/choom"]
CMD ["server"]
