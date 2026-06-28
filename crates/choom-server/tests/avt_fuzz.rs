// SPDX-License-Identifier: GPL-3.0-or-later
//! Robustness fuzz: feed adversarial / random host output through the one attacker-reachable
//! parsing path — `SessionShared::on_output` → `avt` screen model → `render::snapshot` — and
//! assert it never panics.
//!
//! Why this matters: the release profile is `panic = "abort"` (single static musl binary),
//! so on a multi-tenant relay *any* panic in a session task aborts the whole process and
//! drops every session. The realistic trigger is a host (the default host key is publicly
//! bundled) streaming hostile bytes into the screen model. This test feeds it a wide variety
//! of inputs — every byte value, truncated/oversplit multibyte UTF-8, long escape/OSC/CSI
//! sequences, and seeded pseudo-random chunks split at arbitrary boundaries — to gain
//! confidence that path can't be made to panic. Run under dev (`panic = "unwind"`), so any
//! panic surfaces here as a test failure rather than a production outage.

use std::sync::Arc;

use choom_proto::Frame;
use choom_server::session::{App, Config, SessionOpts};
use tokio::sync::mpsc;

fn session() -> Arc<choom_server::session::SessionShared> {
    let app = App::new(Config::default(), "SHA256:test".into());
    let (tx, _rx) = mpsc::channel::<Frame>(1024);
    app.create_session("owner".into(), 80, 24, tx, SessionOpts::default())
        .unwrap()
}

fn render(s: &choom_server::session::SessionShared) {
    let st = s.state.lock().unwrap();
    // Must not panic regardless of what was fed in.
    let _ = choom_server::render::snapshot(&st.vt);
}

/// splitmix64 — a tiny, dependency-free deterministic PRNG for reproducible fuzzing.
struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn byte(&mut self) -> u8 {
        (self.next_u64() & 0xff) as u8
    }
    fn range(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

#[test]
fn every_byte_value_in_isolation_and_pairs() {
    let s = session();
    for b in 0u16..=255 {
        s.on_output(&[b as u8]);
    }
    render(&s);
    // All ordered byte pairs (exercises lead-byte + arbitrary continuation across frames).
    let s = session();
    for hi in 0u16..=255 {
        for lo in 0u16..=255 {
            s.on_output(&[hi as u8, lo as u8]);
        }
    }
    render(&s);
}

#[test]
fn truncated_multibyte_sequences_one_byte_at_a_time() {
    let s = session();
    // Every multibyte UTF-8 char fed byte-by-byte (the boundary-split path), plus lone lead
    // bytes that never complete, interleaved with ASCII.
    let samples: &[&str] = &["é", "中", "😀", "a好b", "🇩🇪", "n\u{0303}"];
    for sample in samples {
        for &byte in sample.as_bytes() {
            s.on_output(&[byte]);
            s.on_output(b"x");
        }
    }
    // Dangling lead bytes with no continuation.
    for &lead in &[0xC3u8, 0xE4, 0xF0, 0xFF, 0x80] {
        s.on_output(&[lead]);
    }
    render(&s);
}

#[test]
fn long_escape_and_control_sequences() {
    let s = session();
    // Unterminated / oversized CSI, OSC, DCS, and SGR storms.
    s.on_output(b"\x1b[");
    s.on_output(&vec![b'9'; 70_000]); // huge numeric parameter run, never terminated
    s.on_output(b"m");
    s.on_output(b"\x1b]0;");
    s.on_output(&vec![b'A'; 70_000]); // huge OSC string, no ST
    s.on_output(b"\x07");
    s.on_output(b"\x1bP");
    s.on_output(&vec![0x1bu8; 10_000]); // ESC storm
    s.on_output(b"\x1b[1;1H\x1b[2J\x1b[?1049h\x1b[?25l");
    render(&s);
}

#[test]
fn seeded_random_chunks_split_at_arbitrary_boundaries() {
    for seed in 0..32u64 {
        let mut rng = Rng(seed.wrapping_mul(0xD1B5_4A32_D192_ED03).wrapping_add(1));
        let s = session();
        for _ in 0..2_000 {
            let len = 1 + rng.range(64);
            let mut chunk = Vec::with_capacity(len);
            for _ in 0..len {
                // Bias toward control + UTF-8 lead bytes to stress the emulator/decoder.
                let b = match rng.range(4) {
                    0 => 0x1b,
                    1 => 0xC0 | (rng.byte() & 0x3f),
                    _ => rng.byte(),
                };
                chunk.push(b);
            }
            s.on_output(&chunk);
        }
        render(&s);
    }
}
