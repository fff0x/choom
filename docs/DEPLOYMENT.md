# Deployment

choom ships a container image, a NixOS module, and Kubernetes manifests. Pick one.

## Container image

The image (`oci.this-is-fine.io/choom/relay`) runs the relay unprivileged (uid
10001). The entrypoint maps `CHOOM_*` env vars to `choom server` flags:

| Env | Flag | Default |
|-----|------|---------|
| `CHOOM_HOSTNAME` | `-H` | — |
| `CHOOM_PORT_LISTEN` / `CHOOM_PORT_ADVERTISE` | `-p` / `-q` | 2200 |
| `CHOOM_WEB` | `-w` | `0.0.0.0:8080` |
| `CHOOM_WEB_URL` | `-W` | — |
| `CHOOM_RECORD` | `-r ${CHOOM_DATA}/recordings` | off |
| `CHOOM_RECORD_ALL` | `--record-all` | off |
| `CHOOM_NO_DEFAULT_KEY` | `--no-default-key` | off |
| `CHOOM_MAX_SESSIONS` / `CHOOM_MAX_VIEWERS` / `CHOOM_RATE_LIMIT` | limits | 200 / 50 / 60 |
| `CHOOM_DOWNLOADS` | `--downloads` | `${CHOOM_DATA}/downloads` |

Persist `CHOOM_DATA` (default `/choom/data`) so the host key (and any recordings)
survive restarts.

```sh
docker run -p 2200:2200 -p 8080:8080 \
  -e CHOOM_HOSTNAME=choom.example.com -e CHOOM_RECORD=1 \
  -v choom-data:/choom/data oci.this-is-fine.io/choom/relay
```

## Kubernetes

The Flux application lives under
`lab/k8s/clusters/hydra/applications/choom/relay/`. It mirrors the radicle seed-node
pattern: a `Deployment` (non-root), a `Service` (ssh 2200 + http 8080), a TCP
`Gateway`+`TCPRoute` for SSH, an `HTTPRoute` for the web view, the registry pull
secret, and a VolSync-backed PVC for `/choom/data`. SSH is advertised at
`choom.${EXTERNAL_DOMAIN}:2200`; the web view at `https://choom.${EXTERNAL_DOMAIN}`.

Or with Helm (`deploy/values.yaml` + the shared `charts/generic`):

```sh
helm upgrade --install choom-relay ../../charts/generic -n choom -f values.yaml
```

## NixOS

The flake exposes `nixosModules.default`:

```nix
imports = [ choom.nixosModules.default ];
services.choom-relay = {
  enable = true;
  openFirewall = true;
  settings = { host = "choom.example.com"; web = "0.0.0.0:8080"; };
};
```

It runs the relay as a hardened systemd service (DynamicUser, ProtectSystem, no
capabilities), writing the settings to a YAML config. See
[`config.yaml`](config.yaml) for the keys.

## Public vs. private relay

- **Public** (like choom.this-is-fine.io): trusts the built-in default key, so
  anyone with the standard client can host. Document the trust model (the operator
  can read and inject — see the README security section). Put TLS in front of the
  web port.
- **Private**: `--no-default-key` (`CHOOM_NO_DEFAULT_KEY=1`) + your own
  `authorized_keys`; tighten the limits.

## Verify

```sh
kubectl -n choom logs deploy/relay      # banner + connection activity
kubectl -n choom port-forward svc/relay 2200:2200 8080:8080
```

The relay logs session create/close, host connect/disconnect, viewer join/leave,
rejected tokens, and rate-limit drops.
