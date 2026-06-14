# Changelog

All notable changes to choom are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/), and the project uses
[semantic versioning](https://semver.org/).

## [0.3.3] - 2026-06-14

### Fixed

- **Terminal artifacts**: device-attributes replies (e.g. `ESC[?62;52;c`) from a
  viewer's or the host's terminal are stripped from shell input, so they no longer
  land at the prompt as `62;52;c` garbage (seen around starting/exiting vim).
- **Host banner**: corrected wording ("Welcome, Choomba, your shell is now shared!"
  / "viewers connect with their ssh client.") and a leading `\r` so it always starts
  at column 0.

### Changed

- Server log lines now include the host's source IP on session create.
- Removed `SECURITY.md` (its content lives in the README and on the landing page);
  moved `choom.example.yaml` to `docs/config.yaml`.

## [0.3.2] - 2026-06-14

### Added

- **Operational logging**: the relay now logs session create/close, host
  connect/disconnect/reconnect, viewer join/leave, rejected (unknown/expired)
  tokens, and rate-limit drops — so operators can see activity in the pod logs.
  No secrets are logged (sessions get a short numeric id).

### Fixed

- **Unknown/expired token UX**: connecting with a stale token now shows
  "unknown or expired session — ask for a fresh link" instead of a bare
  "Shared connection closed". (The relay itself was fine; the silent close just
  looked like a bug when a viewer's `choom host` had already ended.)

## [0.3.1] - 2026-06-14

### Changed

- Dropped the "tmate without tmux" phrasing for "minimal SSH terminal sharing relay";
  README now references [tmate](https://github.com/tmate-io/tmate) as prior art.
- Security wording corrected: a relay operator can **read and inject** into sessions
  (no E2E); the landing page and docs now say so plainly.
- `justfile` replaced by a `Taskfile.yml` (go-task); removed the unused
  `requirements.txt`.

### Added

- `man/choom.1` man page; `docs/RUST-EVAL.md` (assessment of a Rust port).

## [0.3.0] - 2026-06-14

### Added

- **Per-session recording opt-in**: `choom host --record` asks the relay to record
  that session; the server's `-r` only enables storage. `--record-all` forces all.
- **YAML config**: `choom server|host|clients -c file.yaml` (CLI flags still win);
  see `choom.example.yaml`.
- **NixOS module** `nixosModules.default` (`services.choom-relay`) — runs the relay
  as a hardened systemd service, configured via Nix.
- **Dev tooling**: `.envrc` (direnv `use flake`) and a `justfile`
  (`setup`/`test`/`build`/`run`/`image`/`binary`/`fmt`).
- Flavor: the host greets with "Welcome, Choomba." and the landing page leads with it.
- README sections on recordings (asciicast) and the config file.

### Changed

- The public relay now records only sessions whose host opted in (better privacy).

## [0.2.0] - 2026-06-14

### Added

- **Public-relay hardening**: per-IP connection rate limiting (`--rate-limit`),
  global session cap (`--max-sessions`), and a per-session viewer cap
  (`--max-viewers`).
- **Recording retention / GC**: prune `.cast` files by age (`--record-max-age-days`,
  default 7) and total size (`--record-max-mb`, default 500), oldest first.
- **Landing page** at `/` (info, usage, and a public-relay warning) — the terminal
  viewer now lives only at `/s/<token>`.
- **Downloads** served at `/download/` from a directory (`--downloads`,
  `CHOOM_DOWNLOADS`); empty state shows install instructions. With
  `tools/build-binaries.sh` / `tools/binary.Dockerfile` you can build a portable
  single-file binary (Nuitka, default) and drop it there.
- `-W/--web-url` to advertise a public web URL (e.g. behind a TLS gateway).
- A threat-model / security review (in the README and on the landing page) and
  this changelog.

### Fixed

- **Web view sizing**: the browser now receives the host's terminal size and scales
  xterm.js to fit the window, so wide sessions are no longer cut off.
- **Cast download**: the download link is hidden unless a recording exists; the
  deployment enables recording so `/s/<token>/cast` works.

### Notes

- A literal `%` in output is transmitted verbatim (verified by test). A trailing
  `%` some shells show is zsh's `PROMPT_EOL_MARK`, not a choom bug.

## [0.1.0] - 2026-06-14

- Initial release: SSH relay, host client, read-only web view (xterm.js),
  asciicast recording, reconnect, resize negotiation, bundled default key,
  Nix flake, container image, and Kubernetes deployment.
