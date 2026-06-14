# Deploy choom/relay

Uses the shared chart [`../../charts/generic`](../../charts/generic) with the values in this directory.

## Prerequisites

- [Helm 3](https://helm.sh/docs/intro/install/)
- Cluster access (`kubectl`)
- Image published at `oci.this-is-fine.io/choom/relay` (or override `image.*` in `values.yaml`)

Chart install from OCI (versions, signing): [`../../charts/README.md`](../../charts/README.md).

## Install

From this directory:

```bash
helm upgrade --install choom-relay ../../charts/generic \
  -n choom --create-namespace \
  -f values.yaml
```

## Configure

Edit [`values.yaml`](values.yaml):

| Key | Purpose |
|-----|---------|
| `env.CHOOM_HOSTNAME` | Public FQDN clients dial — must match the routable address |
| `env.CHOOM_PORT_LISTEN` / `CHOOM_PORT_ADVERTISE` | Listen / advertised SSH port (default `2200`) |
| `env.CHOOM_WEB` | Web bind spec — keep `0.0.0.0:8080` so the port is reachable; empty disables the web view |
| `env.CHOOM_WEB_URL` | Public base URL printed for the web view (TLS terminated upstream), e.g. `https://choom.example.com` |
| `env.CHOOM_NO_DEFAULT_KEY` | `"1"` to stop trusting the built-in default key (private server) |
| `env.CHOOM_RECORD` | `"1"` to record sessions (asciicast) under `/choom/data/recordings` |
| `persistence.mountPath` | Must stay `/choom/data` (matches entrypoint) — host key persisted here |

Wire the SSH port through a TCP route / load balancer to Service port `2200`, and
the web view through an HTTP route to Service port `8080`.

> Runs **unprivileged** (uid 10001, all capabilities dropped). The shell runs on
> the host, never on the server, so no root / `SYS_ADMIN` is needed.
>
> By default the relay trusts the **built-in default key**, so anyone with the
> standard `choom` client can create a session (public-service model). For a
> private relay set `CHOOM_NO_DEFAULT_KEY=1` and provide your own
> `authorized_keys` in `/choom/data/authorized_keys`.

## Verify

```bash
kubectl -n choom get pods
kubectl -n choom logs deploy/choom-relay   # prints the host-key fingerprint
kubectl -n choom port-forward svc/choom-relay 2200:2200 8080:8080
```

## Uninstall

```bash
helm uninstall choom-relay -n choom
```

PVC is retained unless you delete it manually.

## Preview manifests

```bash
helm template choom-relay ../../charts/generic \
  -n choom -f values.yaml
```
