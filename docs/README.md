# choom documentation

- [Architecture](ARCHITECTURE.md) — crates, transport, screen model, reconnect, web.
- [Deployment](DEPLOYMENT.md) — running the relay, env vars, observability.
- [Configuration](config.yaml) — example YAML config (`choom <cmd> -c file.yaml`).
- [E2E client (proposed)](E2E-CLIENT.md) — design for an opt-in end-to-end-encrypted
  native client (blind-forwarder relay).

The wire frames live in `crates/choom-proto`; the relay, host, and CLI in
`crates/choom-{server,host,cli}`. Man pages with examples are in
[`../man/`](../man/) (`choom.1`, `choom-server.1`, `choom-host.1`, `choom-clients.1`),
regenerated from the CLI with `task man`.

Security: the threat model and review are in the main
[README](../README.md#security) and on the relay's landing page (`/`).
