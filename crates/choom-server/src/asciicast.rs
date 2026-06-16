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
//! Format note: recordings are written as **asciicast v2** — header
//! `{"version":2,"width":W,"height":H}` and events `[time, code, data]` with **absolute**
//! timestamps (seconds since start). v2 is what the vendored asciinema-player can replay
//! from a URL (it has no v3 file parser), and what `asciinema play` reads — so the same
//! `.cast` file works in the browser viewer and on the CLI. The live web view uses a
//! separate WebSocket reset format ([`live_reset_line`]), not this file header.

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

/// The asciicast v2 header line (terminates with `\n`): `{"version":2,"width":W,"height":H}`.
/// Used for **recordings**: v2 is replayable both by `asciinema play` and by the vendored
/// asciinema-player in the browser (it has no v3 file parser).
pub fn header_line(cols: u16, rows: u16) -> String {
    let mut s = json!({ "version": 2, "width": cols, "height": rows }).to_string();
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

/// One asciicast v2 event line (terminates with `\n`): `[time, code, data]`.
///
/// `time` is the **absolute** time in seconds since the stream/recording start. `data` is
/// the output text (for [`Code::Output`]) or `"<cols>x<rows>"` (for [`Code::Resize`]).
pub fn event_line(time: f64, code: Code, data: &str) -> String {
    let mut s = json!([time, code.as_str(), data]).to_string();
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_valid_v2() {
        let v: serde_json::Value = serde_json::from_str(&header_line(80, 24)).unwrap();
        assert_eq!(v["version"], 2);
        assert_eq!(v["width"], 80);
        assert_eq!(v["height"], 24);
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
