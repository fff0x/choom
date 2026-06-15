// SPDX-License-Identifier: GPL-3.0-or-later
//! Render an `avt` screen to an ANSI repaint — the snapshot a viewer receives on join
//! (even mid-vim) before the live stream. Mirrors choom 1.x's `render_screen`, but over
//! avt's cell/pen model instead of pyte's.

use avt::{Color, Pen, Vt};
use std::fmt::Write as _;

/// Build an ANSI byte sequence that redraws `vt`'s visible screen exactly.
pub fn snapshot(vt: &Vt) -> Vec<u8> {
    let mut out = String::new();
    // Hide cursor, clear, home.
    out.push_str("\x1b[?25l\x1b[2J\x1b[H");
    for (y, line) in vt.view().enumerate() {
        let _ = write!(out, "\x1b[{};1H", y + 1);
        let mut last = String::new();
        for cell in line.cells() {
            let sgr = sgr(cell.pen());
            if sgr != last {
                out.push_str(&sgr);
                last = sgr;
            }
            let ch = cell.char();
            out.push(if ch == '\0' { ' ' } else { ch });
        }
        out.push_str("\x1b[0m");
    }
    let cur = vt.cursor();
    let _ = write!(out, "\x1b[{};{}H", cur.row + 1, cur.col + 1);
    if cur.visible {
        out.push_str("\x1b[?25h");
    }
    out.into_bytes()
}

/// SGR escape (`ESC[...m`) for a cell's pen — colors and attributes.
fn sgr(pen: &Pen) -> String {
    let mut p = vec!["0".to_string()];
    if pen.is_bold() {
        p.push("1".into());
    }
    if pen.is_faint() {
        p.push("2".into());
    }
    if pen.is_italic() {
        p.push("3".into());
    }
    if pen.is_underline() {
        p.push("4".into());
    }
    if pen.is_blink() {
        p.push("5".into());
    }
    if pen.is_inverse() {
        p.push("7".into());
    }
    if pen.is_strikethrough() {
        p.push("9".into());
    }
    if let Some(c) = pen.foreground() {
        push_color(&mut p, c, false);
    }
    if let Some(c) = pen.background() {
        push_color(&mut p, c, true);
    }
    format!("\x1b[{}m", p.join(";"))
}

/// Append the SGR params for one color. `bg` selects background vs foreground.
fn push_color(p: &mut Vec<String>, c: Color, bg: bool) {
    match c {
        Color::Indexed(n) if n < 8 => p.push(((if bg { 40 } else { 30 }) + n as u16).to_string()),
        Color::Indexed(n) if n < 16 => {
            p.push(((if bg { 100 } else { 90 }) + (n as u16 - 8)).to_string())
        }
        Color::Indexed(n) => {
            p.push(if bg { "48" } else { "38" }.into());
            p.push("5".into());
            p.push(n.to_string());
        }
        Color::RGB(rgb) => {
            p.push(if bg { "48" } else { "38" }.into());
            p.push("2".into());
            p.push(rgb.r.to_string());
            p.push(rgb.g.to_string());
            p.push(rgb.b.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_contains_text_and_positions_cursor() {
        let mut vt = Vt::new(20, 5);
        vt.feed_str("\x1b[2J\x1b[Hhello\x1b[3;1Hworld");
        let snap = String::from_utf8(snapshot(&vt)).unwrap();
        assert!(snap.starts_with("\x1b[?25l\x1b[2J\x1b[H"));
        assert!(snap.contains("hello"));
        assert!(snap.contains("world"));
        // Cursor ends after "world" on row 3 -> col 6.
        assert!(snap.contains("\x1b[3;6H"));
    }

    #[test]
    fn snapshot_emits_color_sgr() {
        let mut vt = Vt::new(10, 2);
        vt.feed_str("\x1b[31mR\x1b[0m");
        let snap = String::from_utf8(snapshot(&vt)).unwrap();
        assert!(snap.contains("\x1b[0;31m") || snap.contains("\x1b[31m") || snap.contains(";31m"));
    }
}
