// SPDX-License-Identifier: GPL-3.0-or-later
//! The canonical asciicast event stream — the single source of truth that the relay
//! projects to every consumer (recording, live web, SSH snapshot).
//!
//! # Milestone-2 spike findings (live web / ALiS)
//!
//! The live web view is served to **asciinema-player** over a WebSocket. The player's
//! websocket driver accepts two wire formats:
//!   * **raw** — a JSON asciicast header line, then one `[time, code, data]` event line
//!     per message. This is *exactly* the data we already write to the `.cast` file, so
//!     "live stream" and "recording" become the same bytes — the unification goal.
//!   * **ALiS** — asciinema's compact binary live protocol; an optimisation over raw.
//!
//! Decision for M4: implement the **raw** format first (lowest risk, identical to the
//! recording, trivial to encode — see [`event_line`]). Evaluate ALiS as a later
//! optimisation. Encoding cost is negligible; the only real latency is the WS hop plus
//! the player's internal buffering, which must be validated in a browser during M4
//! (cannot be measured headlessly here). Flush per output chunk — never batch — to keep
//! it interactive.
//!
//! Format note: this encoder emits **asciicast v3** — header `{"version":3,"term":{...}}`
//! and events `[interval, code, data]` where `interval` is the time since the *previous*
//! event (relative timing). Verify live-WS support against the asciinema-player build in
//! use; v2 (absolute timestamps) is the fallback if needed.

use serde_json::json;

/// asciicast event code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    /// Terminal output.
    Output,
    /// Terminal resize (`data` is `"<cols>x<rows>"`).
    Resize,
}

impl Code {
    fn as_str(self) -> &'static str {
        match self {
            Code::Output => "o",
            Code::Resize => "r",
        }
    }
}

/// The asciicast v3 header line (terminates with `\n`): `{"version":3,"term":{...}}`.
/// Used for **recordings** (`asciinema play` reads v3).
pub fn header_line(cols: u16, rows: u16) -> String {
    let mut s = json!({ "version": 3, "term": { "cols": cols, "rows": rows } }).to_string();
    s.push('\n');
    s
}

/// The live-stream **reset** message for the asciinema-player WebSocket driver: terminal
/// size, an explicit stream clock (`time`,
/// so the player sets its clock instead of guessing/"correcting the epoch"), and the
/// current screen as `init` so a joiner sees it at once. This is the message
/// asciinema-player's websocket driver consumes first to enter the playing/live state.
pub fn live_reset_line(cols: u16, rows: u16, init: &str) -> String {
    let mut s = json!({ "cols": cols, "rows": rows, "time": 0.0, "init": init }).to_string();
    s.push('\n');
    s
}

/// One asciicast v3 event line (terminates with `\n`): `[interval, code, data]`.
///
/// `interval` is seconds since the *previous* event (relative timing). `data` is the
/// output text (for [`Code::Output`]) or `"<cols>x<rows>"` (for [`Code::Resize`]).
pub fn event_line(interval: f64, code: Code, data: &str) -> String {
    let mut s = json!([interval, code.as_str(), data]).to_string();
    s.push('\n');
    s
}

/// Tracks the previous event time to emit v3 relative intervals. Construct with
/// [`Clock::default`] at session/stream start.
#[derive(Default)]
pub struct Clock {
    last: f64,
}

impl Clock {
    /// The interval (delta) for an event at absolute time `now` (seconds since start).
    pub fn interval(&mut self, now: f64) -> f64 {
        let d = (now - self.last).max(0.0);
        self.last = now;
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_valid_v3() {
        let v: serde_json::Value = serde_json::from_str(&header_line(80, 24)).unwrap();
        assert_eq!(v["version"], 3);
        assert_eq!(v["term"]["cols"], 80);
        assert_eq!(v["term"]["rows"], 24);
    }

    #[test]
    fn clock_yields_relative_intervals() {
        let mut c = Clock::default();
        assert_eq!(c.interval(0.0), 0.0);
        assert_eq!(c.interval(1.5), 1.5);
        assert_eq!(c.interval(2.0), 0.5);
    }

    #[test]
    fn output_event_escapes_and_roundtrips() {
        // Control bytes + a quote must survive as a single JSON string field.
        let line = event_line(1.5, Code::Output, "hi\r\n\x1b[31m\"x\"");
        assert!(line.ends_with('\n'));
        let v: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(v[0], 1.5);
        assert_eq!(v[1], "o");
        assert_eq!(v[2], "hi\r\n\x1b[31m\"x\"");
    }

    #[test]
    fn resize_event_shape() {
        let v: serde_json::Value =
            serde_json::from_str(event_line(0.0, Code::Resize, "120x40").trim_end()).unwrap();
        assert_eq!(v[1], "r");
        assert_eq!(v[2], "120x40");
    }
}
