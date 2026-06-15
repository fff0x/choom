// SPDX-License-Identifier: GPL-3.0-or-later
//! Milestone-2 spike, kept as a regression test: confirm `avt` is a faithful
//! replacement for choom 1.x's `pyte` screen model.
//!
//! The fixture `fixtures/screen_golden.json` is produced by `gen_pyte_golden.py`
//! feeding a representative full-screen redraw (the join-snapshot scenario: clear,
//! CUP, SGR colors, autowrap across the margin, erase, explicit final cursor) into
//! **pyte**. Here we feed the *same* input into **avt** and assert the visible grid
//! and cursor agree — i.e. avt reproduces the screen pyte's `render_screen` would
//! repaint. M3 expands this fixture set; this spike proves the approach holds.

/// Trailing padding differs between emulators (pyte pads to full width); trim it so
/// only the meaningful cell content and column positions are compared.
fn trim_grid(lines: impl IntoIterator<Item = String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|l| l.trim_end().to_string())
        .collect()
}

#[test]
fn avt_matches_pyte_snapshot() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/screen_golden.json")).expect("parse fixture");

    let cols = fixture["cols"].as_u64().unwrap() as usize;
    let rows = fixture["rows"].as_u64().unwrap() as usize;
    let input = fixture["input"].as_str().unwrap();
    let want_display: Vec<String> = fixture["display"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let want_cursor = (
        fixture["cursor"]["col"].as_u64().unwrap() as usize,
        fixture["cursor"]["row"].as_u64().unwrap() as usize,
    );

    let mut vt = avt::Vt::new(cols, rows);
    vt.feed_str(input);

    let got_display = trim_grid(vt.view().map(|line| line.text()));
    assert_eq!(
        got_display,
        trim_grid(want_display),
        "avt visible grid must match pyte's"
    );

    let cursor = vt.cursor();
    assert_eq!(
        (cursor.col, cursor.row),
        want_cursor,
        "avt cursor must match pyte's"
    );
}
