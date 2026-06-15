// SPDX-License-Identifier: GPL-3.0-or-later
//! Milestone-6 recorder test: starting a recording and feeding session output produces
//! a private, valid asciicast file that captures the screen content.

use std::time::Duration;

use choom_server::recorder;
use choom_server::session::{App, Config};
use tokio::sync::mpsc;

#[tokio::test]
async fn records_output_to_a_private_cast_file() {
    let dir = std::env::temp_dir().join(format!("choom-rec-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let app = App::new(Config::default(), "SHA256:test".to_string());
    let (to_host, _rx) = mpsc::channel(16);
    let session = app
        .create_session("owner-key".to_string(), 80, 24, to_host, Default::default())
        .expect("create session");

    let path = recorder::start(&session, &dir).expect("start recording");

    // Output emitted after recording starts must be captured.
    session.on_output(b"rec-marker-xyz\r\n");
    tokio::time::sleep(Duration::from_millis(200)).await;

    // File exists, is 0600, and parses as asciicast v2 with our content.
    let meta = std::fs::metadata(&path).expect("cast file exists");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o600,
            "recording must be private"
        );
    }
    let text = std::fs::read_to_string(&path).expect("read cast");
    let mut lines = text.lines();

    let header: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(header["version"], 3);
    assert_eq!(header["term"]["cols"], 80);

    let found_marker = lines.any(|l| {
        serde_json::from_str::<serde_json::Value>(l)
            .map(|ev| ev[1] == "o" && ev[2].as_str().unwrap_or("").contains("rec-marker-xyz"))
            .unwrap_or(false)
    });
    assert!(found_marker, "recording must contain the emitted output");

    let _ = std::fs::remove_dir_all(&dir);
}
