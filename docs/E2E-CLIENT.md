# Design: optional end-to-end-encrypted client

> **Status: proposed.** A design for an opt-in native client that gives true
> end-to-end encryption, so the relay forwards only ciphertext it can neither read
> nor inject. Not yet implemented (tracked as a follow-up).

## Goal & non-goal

choom 2.0's model is **trust-the-operator**: the relay handles sessions in cleartext,
so its operator can read everything *and inject keystrokes* (the RO/RW tokens restrict
other viewers, not the operator). This is unavoidable for **stock-`ssh` viewers** —
the relay *is* their SSH server, so it terminates their transport and sees plaintext.

This design adds an **opt-in** path for users who need confidentiality + integrity from
the operator: a native client (`choom view`) for **E2E sessions**, where the relay is a
blind forwarder.

**Non-goals.** It does not protect against a relay that *drops* data (censorship/DoS —
inherent to any forwarder). It does not change the stock-`ssh` path. No web E2E in
phase 1 (browser crypto is phase 2).

## The key insight: E2E is per-session and excludes stock ssh

If a session has *any* stock-`ssh` viewer, the relay must produce plaintext for it →
no E2E benefit. So E2E can't be mixed in: an **E2E session emits only ciphertext**, and
its viewers must all be native `choom view` clients holding the key. Therefore:

- `choom host --e2e` starts an **E2E-only** session.
- The host encrypts *everything* it would send; the relay sees only ciphertext.
- Stock-`ssh` viewers connecting to an E2E session are refused with a notice
  ("E2E session — use `choom view <link>`").
- The web view is disabled for E2E sessions (until phase 2).
- Server-side **recording is disabled** (the relay has no plaintext); a host that wants
  a recording keeps the key and records locally.

## Transport: reuse SSH, encrypt the payload

The native client still connects to the relay over **SSH** (token-as-username, exactly
like other viewers) — this reuses the relay's auth, rate-limiting, and framing. The
relay decrypts the *SSH transport* (as always) but the **payload is E2E ciphertext**, so
the SSH plaintext the relay sees is itself opaque. No new port or transport is needed;
the relay just routes opaque blobs between the host and the E2E viewers.

```
 host (choom host --e2e)        relay (blind forwarder)        viewer (choom view)
  PTY ─► avt + AEAD encrypt ──► opaque blobs (+keyframe cache) ──► AEAD decrypt ─► terminal
  PTY ◄── AEAD decrypt ◄─────── opaque blobs ◄───────────────────  local keys ─► AEAD encrypt
```

The relay holds **no key** → it cannot read the stream nor forge input (input is
AEAD-authenticated). It can only drop/reorder/replay, which sequence numbers + AEAD
detect (receiver rejects), bounding the relay to denial-of-service.

## Key distribution: symmetric key in the share-link fragment

The host generates a fresh **256-bit session key** (OS RNG) per E2E session. The share
link carries it in the URL **fragment**, e.g.

```
choom://<token>@relay.sh#k=<base64url-key>
```

Possession of the link = ability to decrypt — the same trust model as today's tokens,
and the relay never sees the fragment (the client parses it locally; it is never sent on
the wire). RO and RW links share the same decryption key; the RW link additionally
authorises sending encrypted input (enforced by the relay via the RW token, and by the
host only accepting AEAD-valid input).

## Crypto

- **AEAD**: ChaCha20-Poly1305 (or AES-256-GCM) from RustCrypto (pure Rust, small, no new
  C deps — keeps the binary lean, consistent with the `ring`-only choice).
- **Key**: 256-bit, per session, from `getrandom`. No long-term key; forward secrecy
  across sessions for free. (Optional later: a hash ratchet for intra-session FS.)
- **Nonce**: 96-bit; a per-direction monotonic counter (never reused under one key).
- **AAD**: a per-message **sequence number** + a direction/type tag, so the receiver
  rejects reordering/replay/cross-direction splicing.
- Distinct sub-keys for output vs input (HKDF from the session key) so the two
  directions can't be spliced.

## Snapshot/keyframe handling (the relay can't render avt)

Late joiners normally get a relay-rendered `avt` snapshot — impossible without
plaintext. Instead:

- The **host runs a local `avt`** and periodically emits an encrypted **keyframe** (a
  full-screen repaint) — on viewer-join signal and/or every N seconds.
- The **relay caches the latest keyframe ciphertext** (opaque) plus the trailing
  ciphertext deltas, and replays them to a joining E2E viewer.
- The viewer decrypts the keyframe (a full ANSI repaint) and writes it to its terminal,
  then applies live deltas — no relay-side rendering needed.

## Component changes

- **proto**: `Frame::E2eOutput(blob)`, `Frame::E2eKeyframe(blob)` (host→relay→viewer),
  `Frame::E2eInput(blob)` (viewer→relay→host); a `Control::Hello { e2e: true, .. }` flag
  marking the session E2E-only.
- **relay**: an E2E session keeps no `avt`/plaintext; it routes opaque blobs, caches the
  latest keyframe, and **refuses stock-`ssh` viewers** (and web) with a notice. Tokens,
  rate-limit, caps, roster, reconnect all still apply.
- **host (`--e2e`)**: generate the key; run a local `avt`; AEAD-encrypt output + emit
  keyframes; AEAD-decrypt RW input; print E2E links with the key fragment; recording off.
- **client (`choom view <link>`)**: parse token+key from the link; connect over SSH;
  decrypt output/keyframes → local terminal (raw mode); RW: encrypt local keystrokes.
  Reuses the host's terminal handling (raw mode, SIGWINCH, DA-strip).

## Phasing

1. **RO native E2E** — host `--e2e`, relay opaque-forward + keyframe cache, `choom view`
   read-only. Validates crypto + keyframes end to end.
2. **RW native E2E** — encrypted input path.
3. **Web E2E** — browser WebCrypto, key from the URL fragment, asciinema-player fed
   decrypted output.

## Open questions

- Keyframe cadence vs bandwidth (on-join only, or periodic?).
- Multi-viewer rekeying: if a viewer should lose access mid-session, the symmetric key
  can't be revoked without rekeying all viewers — accept (kill the session) or add a
  ratchet + rekey control message.
- Whether to also authenticate the *host* to viewers (bind the key to a host identity)
  to stop a relay from substituting a fake host — the key-in-link already implies a
  trusted out-of-band share, so probably out of scope.
