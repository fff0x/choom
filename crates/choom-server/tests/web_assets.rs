// SPDX-License-Identifier: GPL-3.0-or-later
//! #57 test: the web view self-hosts the vendored asciinema-player asset at `/assets`
//! and serves a `/download` index — driven through the axum router (no network).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use choom_server::session::{App, Config};
use choom_server::web::{metrics_router, router};
use tower::ServiceExt; // oneshot

async fn get(app: &Arc<App>, path: &str) -> axum::response::Response {
    router(app.clone())
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn body_string(resp: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
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

/// The `/download` index must HTML-escape filenames it lists. A file whose name contains
/// HTML metacharacters must render as inert text, never as live markup (stored XSS guard).
#[tokio::test]
async fn download_index_escapes_filenames() {
    let dir = std::env::temp_dir().join(format!("choom-dl-xss-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // A regular file whose name carries an XSS payload (legal on Linux: only `/` and NUL
    // are disallowed in a path component).
    std::fs::write(dir.join("a<img src=x onerror=alert(1)>.bin"), b"x").unwrap();

    let app = App::new(
        Config {
            downloads_dir: Some(dir.clone()),
            ..Config::default()
        },
        "SHA256:test".to_string(),
    );

    let html = body_string(get(&app, "/download").await).await;
    assert!(
        !html.contains("<img src=x"),
        "filename must not be emitted as live HTML; got:\n{html}"
    );
    assert!(
        html.contains("a&lt;img src=x onerror=alert(1)&gt;.bin"),
        "filename must be HTML-escaped in the index; got:\n{html}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `/download/{file}` serves a whitelisted name (streamed) and rejects anything else —
/// path traversal, header-breaking characters, and the `.`/`..` specials.
#[tokio::test]
async fn download_file_whitelists_and_streams() {
    let dir = std::env::temp_dir().join(format!("choom-dl-ok-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("choom-x86_64.bin"), b"BINARY-CONTENT").unwrap();

    let app = App::new(
        Config {
            downloads_dir: Some(dir.clone()),
            ..Config::default()
        },
        "SHA256:test".to_string(),
    );

    // A clean, whitelisted name streams back with an attachment disposition.
    let resp = get(&app, "/download/choom-x86_64.bin").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-disposition")
            .unwrap()
            .to_str()
            .unwrap(),
        "attachment; filename=\"choom-x86_64.bin\""
    );
    assert_eq!(body_string(resp).await, "BINARY-CONTENT");

    // Names outside `[A-Za-z0-9._-]` are rejected before touching the filesystem. Illegal
    // URI characters are percent-encoded here; axum decodes them back before the handler,
    // so it is the filename whitelist (not the HTTP layer) that rejects them.
    for bad in [
        "/download/bad%20name.bin", // space
        "/download/evil%22%3Bdrop", // `";`
        "/download/x%0d%0aSet-Cookie:%20y", // CR/LF header injection attempt
        "/download/..",
        "/download/.",
    ] {
        assert_eq!(
            get(&app, bad).await.status(),
            StatusCode::BAD_REQUEST,
            "must reject {bad:?}"
        );
    }
    // A well-formed but nonexistent name is a clean 404 (not a 500).
    assert_eq!(
        get(&app, "/download/missing.bin").await.status(),
        StatusCode::NOT_FOUND
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `/metrics` is operator-internal: it must NOT be reachable on the public web router, but
/// IS served by the dedicated metrics router (bound separately, e.g. on loopback).
#[tokio::test]
async fn metrics_is_off_the_public_web_router() {
    let _ = choom_server::render_metrics(); // install the recorder
    let app = App::new(Config::default(), "SHA256:test".to_string());

    assert_eq!(
        get(&app, "/metrics").await.status(),
        StatusCode::NOT_FOUND,
        "/metrics must not be exposed on the public web view"
    );

    let resp = metrics_router(app.clone())
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("text/plain"));
}

/// `/healthz` reports ready (200) while serving and unready (503) once draining.
#[tokio::test]
async fn healthz_reflects_draining() {
    let app = App::new(Config::default(), "SHA256:test".to_string());
    assert_eq!(get(&app, "/healthz").await.status(), StatusCode::OK);
    app.set_draining();
    assert_eq!(
        get(&app, "/healthz").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

/// A read-only session never grants read-write: even the (never-advertised) rw token maps
/// to read-only access, and the label is stored on the session.
#[tokio::test]
async fn read_only_session_downgrades_rw_token() {
    use choom_proto::Mode;
    use choom_server::session::SessionOpts;

    let app = App::new(Config::default(), "SHA256:test".to_string());
    let (to_host, _rx) = tokio::sync::mpsc::channel(4);
    let session = app
        .create_session(
            "owner".to_string(),
            80,
            24,
            to_host,
            SessionOpts {
                name: Some("demo".into()),
                read_only: true,
            },
        )
        .expect("create session");

    assert!(session.read_only);
    assert_eq!(session.name.as_deref(), Some("demo"));
    assert!(matches!(app.lookup_token(&session.ro_token), Some((_, Mode::Ro))));
    assert!(matches!(app.lookup_token(&session.rw_token), Some((_, Mode::Ro))));
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

/// asciinema-player runs its VT engine as WebAssembly, so the CSP must allow it with
/// `'wasm-unsafe-eval'` — otherwise the browser blocks `WebAssembly.instantiate()` and the
/// terminal never renders (black screen / garbled output), even though the page and stream
/// are fine.
#[tokio::test]
async fn csp_allows_webassembly() {
    let app = App::new(Config::default(), "SHA256:test".to_string());
    let resp = get(&app, "/s/sometoken").await;
    let csp = resp
        .headers()
        .get("content-security-policy")
        .expect("CSP header present")
        .to_str()
        .unwrap();
    assert!(
        csp.contains("'wasm-unsafe-eval'"),
        "script-src must include 'wasm-unsafe-eval' for the player's WASM VT; got: {csp}"
    );
}

/// The viewer page carries a download button and decides live-vs-replay from a single
/// `/status` request (no throwaway probe WebSocket).
#[tokio::test]
async fn viewer_page_uses_status_endpoint() {
    let app = App::new(Config::default(), "SHA256:test".to_string());

    let body = axum::body::to_bytes(get(&app, "/s/sometoken").await.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains(r#"id="dl""#), "viewer page needs the download button");
    assert!(
        html.contains("/status"),
        "viewer page must read /status to decide live vs replay"
    );
    assert!(
        !html.contains("new WebSocket("),
        "viewer page must not open a throwaway probe WebSocket"
    );
}

/// `/status` reports `live` while the session exists and `recording` once a `.cast` is
/// registered — the two bits the viewer page needs to pick live vs replay.
#[tokio::test]
async fn status_reports_live_and_recording() {
    let app = App::new(Config::default(), "SHA256:test".to_string());

    // Unknown token: neither live nor recorded.
    let body = body_string(get(&app, "/s/nope/status").await).await;
    assert!(body.contains("\"live\":false") && body.contains("\"recording\":false"), "{body}");

    // Live session: live=true, recording=false (no record_dir).
    let (to_host, _rx) = tokio::sync::mpsc::channel(4);
    let session = app
        .create_session("owner-key".to_string(), 80, 24, to_host, Default::default())
        .expect("create session");
    let body = body_string(get(&app, &format!("/s/{}/status", session.ro_token)).await).await;
    assert!(body.contains("\"live\":true"), "{body}");

    // After the session ends but a recording is registered: live=false, recording=true.
    let dir = std::env::temp_dir().join(format!("choom-status-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cast_file = dir.join("rec.cast");
    std::fs::write(&cast_file, b"{\"version\":2,\"width\":80,\"height\":24}\n").unwrap();
    app.register_recording(&session.ro_token, &session.rw_token, cast_file);
    app.remove_session(&session);
    let body = body_string(get(&app, &format!("/s/{}/status", session.ro_token)).await).await;
    assert!(
        body.contains("\"live\":false") && body.contains("\"recording\":true"),
        "{body}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A recording stays downloadable after the live session ends — `/cast` resolves via the
/// persistent recordings index, not the (now-gone) active session.
#[tokio::test]
async fn recording_downloadable_after_session_ends() {
    let app = App::new(Config::default(), "SHA256:test".to_string());
    let (to_host, _rx) = tokio::sync::mpsc::channel(4);
    let session = app
        .create_session("owner-key".to_string(), 80, 24, to_host, Default::default())
        .expect("create session");
    let ro = session.ro_token.clone();

    let dir = std::env::temp_dir().join(format!("choom-cast-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cast_file = dir.join("rec.cast");
    std::fs::write(&cast_file, b"{\"version\":3}\n[0.0,\"o\",\"hi\"]\n").unwrap();
    app.register_recording(&session.ro_token, &session.rw_token, cast_file.clone());

    let cast = format!("/s/{ro}/cast");
    assert_eq!(get(&app, &cast).await.status(), StatusCode::OK);

    // End the live session, then the download must still work.
    app.remove_session(&session);
    assert!(app.lookup_token(&ro).is_none(), "session token gone after end");
    assert_eq!(
        get(&app, &cast).await.status(),
        StatusCode::OK,
        "recording must stay downloadable after the session ends"
    );

    std::fs::remove_dir_all(&dir).ok();
}
