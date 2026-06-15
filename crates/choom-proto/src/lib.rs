// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared constants and the wire-frame schema for choom 2.0.
//!
//! A host shares its local shell through a central SSH relay; viewers connect with a
//! plain `ssh <token>@server`.
//!
//! Unlike choom 1.x (two SSH channels carrying newline-JSON + raw bytes), 2.0 uses a
//! **single typed, framed channel** between host and relay: a stream of [`Frame`]s,
//! `postcard`-encoded, with raw terminal bytes carried as variant payloads. The frame
//! is the only thing both ends must agree on; everything else is local policy.

use serde::{Deserialize, Serialize};

// --- Reserved SSH usernames (host role, public-key required) ------------------
/// Create a new session.
pub const USER_NEW: &str = "new";
/// Take over an existing session after a disconnect.
pub const USER_RESUME: &str = "resume";
/// Only query viewers (`choom clients`); creates nothing.
pub const USER_QUERY: &str = "query";

/// Is `user` one of the reserved host-role usernames (vs. a viewer token)?
pub fn is_host_user(user: &str) -> bool {
    matches!(user, USER_NEW | USER_RESUME | USER_QUERY)
}

// --- Tokens / secrets ---------------------------------------------------------
/// Bytes of entropy for a share token (128-bit, base64url -> the SSH username).
pub const TOKEN_NBYTES: usize = 16;
/// Bytes of entropy for a reconnect id (high entropy, owner-bound).
pub const RECONNECT_NBYTES: usize = 32;

// --- Terminal sizes (clamped against resize / memory DoS) ---------------------
pub const MIN_COLS: u16 = 20;
pub const MIN_ROWS: u16 = 5;
pub const MAX_COLS: u16 = 1000;
pub const MAX_ROWS: u16 = 1000;

// --- Defaults -----------------------------------------------------------------
/// Public default relay.
pub const DEFAULT_SERVER: &str = "choom.sh";
pub const DEFAULT_SSH_PORT: u16 = 2200;
pub const DEFAULT_WEB_PORT: u16 = 8080;
/// Seconds a session lives on after the host drops.
pub const GRACE_PERIOD_SECS: u64 = 60;
/// Upper bound for a single decoded frame (memory-DoS guard).
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

// --- Resource limits (public-relay hardening) ---------------------------------
pub const DEFAULT_MAX_SESSIONS: usize = 200;
pub const DEFAULT_MAX_VIEWERS: usize = 50;
pub const DEFAULT_RATE_PER_MIN: u32 = 60;

// --- Recording retention ------------------------------------------------------
pub const DEFAULT_RECORD_MAX_AGE_DAYS: u64 = 7;
pub const DEFAULT_RECORD_MAX_MB: u64 = 500;

/// Built-in default public key the relay trusts unless `--no-default-key` is set.
/// The standard client ships the matching private key; convenience for the public
/// service, disabled for private relays in favour of an `authorized_keys` file.
/// (Fresh keypair for 2.0 — not shared with the 1.x default key.)
pub const DEFAULT_TRUSTED_PUBKEY: &str = "ssh-ed25519 \
    AAAAC3NzaC1lZDI1NTE5AAAAIGvWDmJfGTMWxqZhNjYSOlG3n6brc5yckLcXWQy1zOXh choom-2.0-default";

/// Clamp a terminal size to the allowed range (guards resize DoS + emulator memory).
pub fn clamp_size(cols: u16, rows: u16) -> (u16, u16) {
    (
        cols.clamp(MIN_COLS, MAX_COLS),
        rows.clamp(MIN_ROWS, MAX_ROWS),
    )
}

// --- Tokens -------------------------------------------------------------------
use base64::Engine as _;

fn url_token(nbytes: usize) -> String {
    let mut buf = vec![0u8; nbytes];
    getrandom::getrandom(&mut buf).expect("OS RNG unavailable");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&buf)
}

/// A fresh 128-bit share token, base64url (used as the viewer's SSH username).
pub fn new_token() -> String {
    url_token(TOKEN_NBYTES)
}

/// A fresh high-entropy reconnect id (owner-bound resume secret).
pub fn new_reconnect_id() -> String {
    url_token(RECONNECT_NBYTES)
}

/// Constant-time string equality (for comparing a presented token to a stored one).
pub fn ct_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq as _;
    let (a, b) = (a.as_bytes(), b.as_bytes());
    // Length is not secret; bail early so `ct_eq` sees equal-length inputs.
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

/// A viewer's access mode.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Read-only: input dropped, never affects the negotiated size.
    Ro,
    /// Read-write: keystrokes reach the host shell (full access).
    Rw,
}

/// How a viewer is attached.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Ssh,
    Web,
}

/// One entry in the viewer roster (drives `choom clients`, join/leave, web sidebar).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub id: u64,
    pub mode: Mode,
    pub transport: Transport,
    pub ip: String,
    pub cols: u16,
    pub rows: u16,
    /// Unix timestamp (seconds) the viewer connected.
    pub connected_at: u64,
}

/// Control messages multiplexed onto the same channel as terminal data.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Control {
    // host -> relay
    /// Create a new session at the given initial size.
    Hello { cols: u16, rows: u16 },
    /// Take over an existing session after a disconnect.
    Resume { id: String, cols: u16, rows: u16 },
    /// The host's own terminal was resized.
    HostSize { cols: u16, rows: u16 },
    /// Ask the relay to record this session (honoured only if storage is enabled).
    Record,
    /// Request the current viewer roster.
    ListClients,
    /// `choom clients`: one-shot roster request for the session with this reconnect id.
    Query { id: String },
    // relay -> host
    /// Session is live: the share links + negotiated identity.
    Ready(Ready),
    /// Negotiated terminal size the host should apply to its PTY.
    Resize { cols: u16, rows: u16 },
    /// A viewer joined.
    Join(Client),
    /// A viewer (by id) left.
    Leave { id: u64 },
    /// Roster reply to [`Control::ListClients`].
    Clients(Vec<Client>),
    /// A fatal error for this session.
    Error { msg: String },
}

/// Payload of [`Control::Ready`] — everything the host needs to print share links.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    pub ro: String,
    pub rw: String,
    pub web: Option<String>,
    pub id: String,
    pub host: String,
    pub port: u16,
    /// Relay host-key fingerprint (viewer MITM check).
    pub fp: String,
}

/// A single frame on the host<->relay channel.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// host -> relay: raw PTY output bytes from the shared shell.
    Output(Vec<u8>),
    /// relay -> host: keystrokes from read-write viewers.
    Input(Vec<u8>),
    /// Bidirectional control message.
    Control(Control),
}

impl Frame {
    /// Encode a frame to a `postcard` byte buffer.
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_stdvec(self)
    }

    /// Decode a frame from a `postcard` byte buffer.
    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

/// A length-delimited (`u32` big-endian prefix) + `postcard` codec for [`Frame`]s,
/// for use with `tokio_util::codec::Framed` over the host<->relay channel stream.
#[cfg(feature = "codec")]
pub mod codec {
    use super::{Frame, MAX_FRAME_BYTES};
    use bytes::{Buf as _, BufMut as _, BytesMut};
    use std::io;
    use tokio_util::codec::{Decoder, Encoder};

    /// Stateless `Frame` codec. See [`crate::codec`].
    #[derive(Debug, Default, Clone, Copy)]
    pub struct FrameCodec;

    const HEADER: usize = 4;

    impl Decoder for FrameCodec {
        type Item = Frame;
        type Error = io::Error;

        fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<Frame>> {
            if src.len() < HEADER {
                return Ok(None);
            }
            let mut len_bytes = [0u8; HEADER];
            len_bytes.copy_from_slice(&src[..HEADER]);
            let len = u32::from_be_bytes(len_bytes) as usize;
            if len > MAX_FRAME_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "frame too large",
                ));
            }
            if src.len() < HEADER + len {
                src.reserve(HEADER + len - src.len());
                return Ok(None);
            }
            src.advance(HEADER);
            let payload = src.split_to(len);
            let frame = Frame::decode(&payload)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            Ok(Some(frame))
        }
    }

    impl Encoder<Frame> for FrameCodec {
        type Error = io::Error;

        fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> io::Result<()> {
            let payload = item
                .encode()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            if payload.len() > MAX_FRAME_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "frame too large",
                ));
            }
            dst.reserve(HEADER + payload.len());
            dst.put_u32(payload.len() as u32);
            dst.put_slice(&payload);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_keeps_in_range() {
        assert_eq!(clamp_size(0, 0), (MIN_COLS, MIN_ROWS));
        assert_eq!(clamp_size(9999, 9999), (MAX_COLS, MAX_ROWS));
        assert_eq!(clamp_size(80, 24), (80, 24));
    }

    #[test]
    fn tokens_are_unique_and_url_safe() {
        let a = new_token();
        let b = new_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 22); // 16 bytes base64url, no padding
        assert!(a
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
        assert!(new_reconnect_id().len() >= a.len());
    }

    #[test]
    fn ct_eq_matches_string_eq() {
        let t = new_token();
        assert!(ct_eq(&t, &t.clone()));
        assert!(!ct_eq(&t, "different"));
        assert!(!ct_eq("ab", "abc"));
    }

    #[test]
    fn host_user_detection() {
        assert!(is_host_user(USER_NEW));
        assert!(is_host_user(USER_RESUME));
        assert!(is_host_user(USER_QUERY));
        assert!(!is_host_user("some-token"));
    }

    #[test]
    fn frame_roundtrips() {
        let cases = [
            Frame::Output(b"hello \x1b[31mworld\x1b[0m".to_vec()),
            Frame::Input(vec![0x03]),
            Frame::Control(Control::Hello { cols: 80, rows: 24 }),
            Frame::Control(Control::Resize {
                cols: 120,
                rows: 40,
            }),
            Frame::Control(Control::Ready(Ready {
                ro: "ro-token".into(),
                rw: "rw-token".into(),
                web: Some("https://example/s/ro-token".into()),
                id: "sess-1".into(),
                host: DEFAULT_SERVER.into(),
                port: DEFAULT_SSH_PORT,
                fp: "SHA256:abc".into(),
            })),
            Frame::Control(Control::Join(Client {
                id: 1,
                mode: Mode::Rw,
                transport: Transport::Ssh,
                ip: "10.0.0.1".into(),
                cols: 80,
                rows: 24,
                connected_at: 1_700_000_000,
            })),
        ];
        for f in cases {
            let bytes = f.encode().expect("encode");
            let back = Frame::decode(&bytes).expect("decode");
            assert_eq!(f, back);
        }
    }

    #[cfg(feature = "codec")]
    #[test]
    fn codec_roundtrips_with_partial_buffers() {
        use crate::codec::FrameCodec;
        use bytes::{BufMut as _, BytesMut};
        use tokio_util::codec::{Decoder, Encoder};

        let frames = [
            Frame::Control(Control::Hello { cols: 80, rows: 24 }),
            Frame::Output(b"\x1b[2J\x1b[Hsome screen".to_vec()),
            Frame::Input(vec![0x04]),
        ];

        // Encode all three into one buffer (simulates coalesced TCP/SSH data).
        let mut wire = BytesMut::new();
        let mut codec = FrameCodec;
        for f in &frames {
            codec.encode(f.clone(), &mut wire).expect("encode");
        }

        // Feed the bytes one at a time: the decoder must yield each frame intact
        // only once its full length has arrived (no partial/garbled decode).
        let mut feed = BytesMut::new();
        let mut got = Vec::new();
        for byte in wire.iter().copied() {
            feed.put_u8(byte);
            while let Some(frame) = codec.decode(&mut feed).expect("decode") {
                got.push(frame);
            }
        }
        assert_eq!(got, frames);
    }
}
