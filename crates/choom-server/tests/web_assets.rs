// SPDX-License-Identifier: GPL-3.0-or-later
//! #57 test: the web view self-hosts the vendored asciinema-player asset at `/assets`
//! and serves a `/download` index — driven through the axum router (no network).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use choom_server::session::{App, Config};
use choom_server::web::router;
use tower::ServiceExt; // oneshot

async fn get(app: &Arc<App>, path: &str) -> axum::response::Response {
    router(app.clone())
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn serves_vendored_player_and_download_index() {
    let app = App::new(Config::default(), "SHA256:test".to_string());

    // The vendored asciinema-player JS is served self-hosted (no CDN), gzip-compressed.
    let resp = get(&app, "/assets/asciinema-player.min.js").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("javascript"));
    assert_eq!(
        resp.headers().get("content-encoding").map(|v| v.as_bytes()),
        Some(&b"gzip"[..]),
        "asset should be served gzip-compressed"
    );
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(
        body.len() > 20_000,
        "gzipped player bundle should be substantial"
    );
    assert_eq!(&body[..2], &[0x1f, 0x8b], "gzip magic bytes");

    // The CSS is served too.
    assert_eq!(
        get(&app, "/assets/asciinema-player.css").await.status(),
        StatusCode::OK
    );

    // /download with no configured dir returns the build-from-source help page.
    let resp = get(&app, "/download").await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Path traversal on /download/{file} is never served (rejected by the router or
    // the handler's filename check).
    assert_ne!(get(&app, "/download/..").await.status(), StatusCode::OK);
    assert_ne!(
        get(&app, "/download/anything").await.status(),
        StatusCode::OK,
        "no downloads dir configured"
    );
}

/// The viewer page must start the player with `autoPlay: true`. Without it the
/// asciinema-player shows a black poster and never even opens the WebSocket, which is
/// the "session stays black in the browser" bug. The container also needs a height for
/// `fit: 'both'` not to collapse to nothing.
#[tokio::test]
async fn viewer_page_autoplays_the_live_stream() {
    let app = App::new(Config::default(), "SHA256:test".to_string());
    let resp = get(&app, "/s/sometoken").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(
        html.contains("autoPlay: true"),
        "viewer page must auto-play the live stream (else the browser stays black)"
    );
    assert!(
        html.contains("driver: 'websocket'"),
        "viewer page must use the websocket (live) driver"
    );
    assert!(
        html.contains("#player{height"),
        "the player container needs a height for fit:'both'"
    );
}
