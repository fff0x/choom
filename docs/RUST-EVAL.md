# Evaluation: porting choom to Rust

*Assessment only — no migration is planned or started.*

## Why consider it

- A single **static binary** (musl, ~3–8 MB) that needs no Python runtime — vs. the
  current 78 MB Nuitka build or the Python/Nix install.
- Lower memory and startup cost; **memory safety** for a network-facing relay.

## Feasibility

choom is small (~1.5k LOC) and every dependency has a solid Rust equivalent:

| choom (Python) | Rust equivalent | Notes |
|----------------|-----------------|-------|
| asyncssh (server + client) | **russh** | pure-Rust async SSH; the biggest piece |
| pyte (screen model) | **termwiz** / `vt100` / `alacritty_terminal` | for the snapshot/repaint |
| aiohttp (HTTP + WS) | **axum** + `tokio-tungstenite` | web view + downloads |
| asyncio | **tokio** | runtime |
| pty (host side) | **portable-pty** (wezterm) / `nix` | local shell |
| PyYAML | `serde_yaml` | config |
| asciicast/json | `serde_json` | recording |

The wire protocol is tiny and already specified (`protocol.py`), so a faithful port
is mostly mechanical.

## Effort & risk

- **~2–4 weeks** for a solid port. Trickiest: matching pyte's snapshot rendering,
  the host PTY relay, and russh's server/client ergonomics.
- The Python version is **done, tested (49 tests), deployed**. A rewrite risks
  regressions and creates a two-implementation maintenance burden.

## Recommendation

**Not now.** The Python implementation is complete and its distribution story
(Nuitka single binary, Nix, container, NixOS module) already works; the main Rust
payoff (small static binary) is partly covered by Nuitka.

Revisit if binary size/startup or dropping the Python runtime becomes important. If
porting, do it **incrementally**: start with the relay (`russh` server) while keeping
wire-compatibility with the Python host, and validate it against the existing test
suite as a conformance harness before touching the client.
