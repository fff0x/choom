# Deploying choom on Kubernetes

A self-contained [kustomize](https://kustomize.io/) example that runs the choom
relay (SSH session-sharing server with a web view) on any Kubernetes cluster.

## Quickstart

```sh
kubectl apply -k deploy
```

This creates a `Deployment`, a `relay-ssh` `LoadBalancer` Service (SSH ingress), a
`relay` `ClusterIP` Service (web + metrics) and a 1Gi `PersistentVolumeClaim`.

## Verify the image signature

Images are signed with [cosign](https://docs.sigstore.dev/) using a fixed key —
[`cosign.pub`](cosign.pub). Verify before pulling/deploying:

```sh
cosign verify --key cosign.pub oci.this-is-fine.io/choom/relay:latest
```

If a copy was not published to a transparency log, add `--insecure-ignore-tlog`.

## Configure

choom is configured entirely through `CHOOM_*` environment variables in
`deployment.yaml`:

| Env var             | Purpose                                          | Default                     |
| ------------------- | ------------------------------------------------ | --------------------------- |
| `CHOOM_HOST`        | Hostname advertised in share links               | `choom.<domain.tld>`         |
| `CHOOM_SSH_ADDR`    | SSH listen address inside the pod                | `0.0.0.0:2200`              |
| `CHOOM_PORT`        | SSH port advertised in links (display only)      | `22`                        |
| `CHOOM_WEB_ADDR`    | Web view bind address                            | `0.0.0.0:8080`              |
| `CHOOM_WEB_URL`     | Public web URL                                   | `https://choom.<domain.tld>` |
| `CHOOM_METRICS_ADDR`| Prometheus endpoint (in-cluster scrape only)     | `0.0.0.0:9090`              |
| `CHOOM_LOG_FORMAT`  | `json` or `text`                                 | `json`                      |
| `CHOOM_HOST_KEY`    | Persistent SSH host key path on the PVC          | `/data/host_key`            |
| `CHOOM_RECORD_DIR`  | Session recording dir on the PVC                 | `/data/recordings`          |

What to change per file:

| File              | Change                                                             |
| ----------------- | ----------------------------------------------------------------- |
| `deployment.yaml` | `CHOOM_HOST` + `CHOOM_WEB_URL` to your hostname; pin the image tag |
| `service.yaml`    | Service type if you don't want a `LoadBalancer` for SSH           |
| `pvc.yaml`        | `storage` size and `storageClassName` if needed                   |

## Expose

- **SSH** — the `relay-ssh` LoadBalancer Service exposes port `2200`. Point your
  DNS (`CHOOM_HOST`) at its external IP. Hosts share sessions with
  `ssh <token>@<host>`.
- **Web view** — put an Ingress or Gateway HTTPRoute in front of the `relay`
  Service port `8080` and terminate TLS for `CHOOM_WEB_URL`.
- **Metrics** — scrape the `relay` Service port `9090` from Prometheus. It is
  deliberately *not* exposed publicly.

## Use as a GitOps base

Reference this folder as a remote kustomize base and patch it for your environment:

```yaml
resources:
  - https://your-git-host/choom//deploy?ref=main
```

## Uninstall

```sh
kubectl delete -k deploy
```

> The PVC (host key + recordings) is removed with it. Back up `/data` first if you
> want to keep a stable host-key fingerprint.
