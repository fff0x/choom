// SPDX-License-Identifier: GPL-3.0-or-later
//! Milestone-6 metrics test: the Prometheus recorder is installed and session activity
//! shows up in the exposition text.

use choom_server::session::{App, Config};
use tokio::sync::mpsc;

#[tokio::test]
async fn metrics_track_session_activity() {
    // Install the process-wide recorder *before* generating activity (the metrics
    // macros are no-ops until a recorder exists).
    let _ = choom_server::render_metrics();

    let app = App::new(Config::default(), "SHA256:test".to_string());
    let (tx, _rx) = mpsc::channel(8);
    let session = app
        .create_session("owner".to_string(), 80, 24, tx, Default::default())
        .expect("create session");
    session.on_output(b"some bytes");

    let out = choom_server::render_metrics().expect("recorder installed");
    assert!(
        out.contains("choom_sessions_created_total"),
        "exposition should include session counter; got:\n{out}"
    );
    assert!(
        out.contains("choom_output_bytes_total"),
        "exposition should include byte counter; got:\n{out}"
    );
}
