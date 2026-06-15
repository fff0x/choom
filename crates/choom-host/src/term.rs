// SPDX-License-Identifier: GPL-3.0-or-later
//! Host terminal helpers: size detection, raw mode (with restore-on-drop), the
//! `SIGWINCH` stream, and stripping device-attribute replies from viewer input.

use choom_proto::clamp_size;
use rustix::termios::{self, OptionalActions};

/// The host terminal's current size, or `80x24` when not a tty / not interactive.
pub fn initial_size(interactive: bool) -> (u16, u16) {
    if interactive {
        if let Ok(ws) = termios::tcgetwinsize(rustix::stdio::stdin()) {
            if ws.ws_col > 0 && ws.ws_row > 0 {
                return clamp_size(ws.ws_col, ws.ws_row);
            }
        }
    }
    (80, 24)
}

/// Restores the saved terminal attributes when dropped.
pub struct RawGuard {
    saved: termios::Termios,
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = termios::tcsetattr(rustix::stdio::stdin(), OptionalActions::Now, &self.saved);
    }
}

/// Put the host's stdin tty into raw mode for the session. `None` if stdin is not a
/// tty (e.g. piped, or non-interactive) — the caller then skips local echo handling.
pub fn enter_raw() -> Option<RawGuard> {
    let fd = rustix::stdio::stdin();
    if !termios::isatty(fd) {
        return None;
    }
    let saved = termios::tcgetattr(fd).ok()?;
    let mut raw = saved.clone();
    raw.make_raw();
    termios::tcsetattr(fd, OptionalActions::Now, &raw).ok()?;
    Some(RawGuard { saved })
}

/// A `SIGWINCH` stream (host terminal resized), or `None` when not interactive.
pub fn winch_stream(interactive: bool) -> Option<tokio::signal::unix::Signal> {
    if !interactive {
        return None;
    }
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change()).ok()
}

/// Remove terminal device-attribute (DA) replies — `ESC [ ? … c` / `ESC [ > … c` —
/// from viewer input, so a viewer's terminal answering the host shell's DA query
/// doesn't dump `62;52;c`-style garbage at the prompt (seen around vim).
pub fn strip_da_replies(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == 0x1b
            && i + 2 < input.len()
            && input[i + 1] == b'['
            && (input[i + 2] == b'?' || input[i + 2] == b'>')
        {
            let mut j = i + 3;
            while j < input.len() && (input[j].is_ascii_digit() || input[j] == b';') {
                j += 1;
            }
            if j < input.len() && input[j] == b'c' {
                i = j + 1; // drop the whole DA reply
                continue;
            }
        }
        out.push(input[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::strip_da_replies;

    #[test]
    fn strips_da_replies_keeps_real_input() {
        let input = b"ab\x1b[?62;52;0ccd\x1b[>0;276;0ce";
        assert_eq!(strip_da_replies(input), b"abcde");
    }

    #[test]
    fn leaves_normal_escapes_untouched() {
        // A cursor-position request (no '?'/'>') must pass through unchanged.
        let input = b"x\x1b[1;1Hy";
        assert_eq!(strip_da_replies(input), input);
    }

    #[test]
    fn incomplete_da_is_not_stripped() {
        let input = b"\x1b[?62;52"; // no trailing 'c'
        assert_eq!(strip_da_replies(input), input);
    }
}
