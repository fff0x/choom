# Deployment

The relay is a single unprivileged binary. It needs no root, no capabilities, and no
state on disk beyond recordings (an ephemeral host key is generated at startup). Run
it as a bare binary, a container, or a NixOS service.

## Container (static musl on `scratch`, ~4 MB)

The [`Dockerfile`](../Dockerfile) compiles a static musl binary and ships it on
`scratch` — nothing but the binary, running as uid `10001`:

```sh
docker build -t choom/relay:2.0 .
docker run -p 2200:2200 -p 8080:8080 \
  -e CHOOM_HOST=choom.example.com -e CHOOM_RECORD_DIR=/data choom/relay:2.0
```

It logs JSON by default (`CHOOM_LOG_FORMAT=json`) and reads all `CHOOM_*` env vars
directly (no entrypoint wrapper). Mount a volume at your `CHOOM_RECORD_DIR` if
recording.

## Nix

The flake builds the binary with [crane](https://github.com/ipetkov/crane):

```sh
nix run  github:…/choom -- server -H choom.example.com -w 0.0.0.0:8080
nix build github:…/choom          # -> result/bin/choom
```

The NixOS module runs it as a hardened systemd service (`DynamicUser`,
`ProtectSystem=strict`, no capabilities, `MemoryDenyWriteExecute`):

```nix
imports = [ choom.nixosModules.default ];
services.choom-relay = {
  enable = true;
  openFirewall = true;
  settings = {           # snake_case config keys; see docs/config.yaml
    host = "choom.example.com";
    web = "0.0.0.0:8080";
  };
};
```

The flake also provides a dev shell (cargo, rustfmt, clippy, rust-analyzer) via
`nix develop`.

## Run the binary

```sh
cargo build --release
./target/release/choom server -H choom.example.com -w 0.0.0.0:8080
```

Every flag also reads an env var, so you can configure it the 12-factor way:

| Env | Flag | Default |
|-----|------|---------|
| `CHOOM_SSH_ADDR` | `--ssh-addr` | `0.0.0.0:2200` |
| `CHOOM_HOST` | `-H` (advertised hostname) | `choom.sh` |
| `CHOOM_PORT` | `-p` (advertised port) | `2200` |
| `CHOOM_WEB_ADDR` | `-w` (web bind; unset = no web) | — |
| `CHOOM_WEB_URL` | `-W` (public web URL) | — |
| `CHOOM_RECORD_DIR` | `-r` (enables recording) | — |
| `CHOOM_LOG_FORMAT` | `json` for structured logs | text |
| `RUST_LOG` | log level/filter | `info` |

Limits, `--record-all`, `--no-default-key`, `-a authorized_keys`, and the retention
caps are flags / YAML keys (see [config.yaml](config.yaml)). A YAML file is the
tidiest way to set many options: `choom server -c /etc/choom.yaml`.

## systemd

```ini
[Service]
DynamicUser=yes
ExecStart=/usr/local/bin/choom server -c /etc/choom.yaml
Environment=CHOOM_LOG_FORMAT=json
AmbientCapabilities=
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/lib/choom        # only if recording is enabled
Restart=on-failure
```

The relay drains on `SIGTERM` (stops accepting, ends sessions, exits cleanly), so
rolling restarts are graceful.

## Public vs. private relay

- **Public** (like choom.sh): trusts the bundled default key, so anyone
  with the standard client can host. Document the trust model — the operator can read
  and inject (see the README security section). Put TLS in front of the web port.
- **Private**: `--no-default-key` (`use_default_key: false`) plus your own
  `-a authorized_keys`; tighten the limits. Hosts then need `-i <key>` (a key listed
  in `authorized_keys`).

## Observe

- **Logs** — session create/close, host connect/disconnect/resume, viewer join/leave,
  rejected tokens, untrusted hosts, rate-limit drops. No full tokens are logged.
  `CHOOM_LOG_FORMAT=json` for aggregation.
- **Metrics** — `GET /metrics` (on the web listener) exposes Prometheus counters:
  `choom_sessions_created_total`, `choom_sessions_active`, `choom_viewers_active`,
  `choom_output_bytes_total`, `choom_tokens_rejected_total`,
  `choom_rate_limited_total`, `choom_host_keys_rejected_total`.

## Verify end to end

```sh
choom server --ssh-addr 127.0.0.1:2200 -H 127.0.0.1 -w 127.0.0.1:8080 &
choom host -s 127.0.0.1 -H 127.0.0.1        # prints share links
ssh <rw-token>@127.0.0.1 -p 2200            # a viewer attaches
choom clients                               # list the viewers
curl -s localhost:8080/metrics              # counters
```
