// SPDX-License-Identifier: GPL-3.0-or-later
//! Output-only web view: an axum server that streams a session as a live **asciicast**
//! over a WebSocket (consumed by asciinema-player), plus a landing page and the viewer
//! page. There is **no input path** — the WebSocket ignores all client frames, so the
//! browser is structurally a spectator (a deliberate security property).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use choom_proto::{Mode, Transport};
use futures::{SinkExt, StreamExt};
use tracing::info;

use crate::asciicast::{self, Code};
use crate::session::{App, SessionShared};

/// Build the axum router for the web view.
pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(landing))
        .route("/s/{token}", get(viewer_page))
        .route("/s/{token}/stream", get(ws_stream))
        .route("/s/{token}/cast", get(cast_download))
        .route("/assets/asciinema-player.min.js", get(asset_js))
        .route("/assets/asciinema-player.css", get(asset_css))
        .route("/download", get(download_index))
        .route("/download/{file}", get(download_file))
        .route("/metrics", get(metrics))
        .layer(middleware::from_fn(security_headers))
        .with_state(app)
}

/// Bind and serve the web view until the process exits.
pub async fn serve_web(app: Arc<App>, addr: SocketAddr) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, "choom web view listening");
    axum::serve(listener, router(app)).await?;
    Ok(())
}

/// Add hardening headers to every response (token-in-URL, no cookies -> CSRF/CSWSH
/// are structurally moot; the rest limits leak surface and framing).
async fn security_headers(req: axum::extract::Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self' 'unsafe-inline'; \
             style-src 'self' 'unsafe-inline'; connect-src 'self' ws: wss:",
        ),
    );
    res
}

async fn landing() -> Html<&'static str> {
    Html(LANDING_HTML)
}

async fn viewer_page(Path(_token): Path<String>) -> Html<&'static str> {
    // The token stays in the URL; the page reads it from location and opens the WS.
    Html(VIEWER_HTML)
}

/// `GET /s/{token}/stream` — upgrade to a WebSocket carrying the live asciicast.
async fn ws_stream(
    State(app): State<Arc<App>>,
    Path(token): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    match app.lookup_token(&token) {
        // Any valid token may *view* (output-only); rw grants nothing extra on the web.
        Some((session, _mode)) => ws.on_upgrade(move |socket| stream_session(socket, session)),
        None => (StatusCode::NOT_FOUND, "unknown or expired session").into_response(),
    }
}

/// Stream one web viewer: asciicast header, the join snapshot as an output event, then
/// live output. Client frames are ignored (output-only).
async fn stream_session(socket: WebSocket, session: Arc<SessionShared>) {
    let (cols, rows) = {
        let st = session.state.lock().unwrap();
        (st.neg_cols, st.neg_rows)
    };
    let (id, snapshot, mut rx) =
        session.add_viewer(Mode::Ro, Transport::Web, "web".to_string(), cols, rows);

    let (mut tx, mut client_rx) = socket.split();
    let start = Instant::now();

    // asciinema-player's live WS driver wants a single "reset" message carrying the size,
    // a stream clock, and the current screen as `init` — then absolute-time events. (We
    // used to send a bare {cols,rows} header + the snapshot as a t=0 event, which the
    // player treated as a finite recording: it played to the end and paused with a replay
    // button instead of staying live.)
    let reset = asciicast::live_reset_line(cols, rows, &String::from_utf8_lossy(&snapshot));
    if tx.send(Message::text(reset)).await.is_err() {
        session.remove_viewer(id);
        return;
    }

    let cancel = session.cancel.clone();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            msg = rx.recv() => match msg {
                Ok(bytes) => {
                    let t = start.elapsed().as_secs_f64();
                    let line = asciicast::event_line(t, Code::Output, &String::from_utf8_lossy(&bytes));
                    if tx.send(Message::text(line)).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            // Reading is required to observe a client close; all content is ignored.
            incoming = client_rx.next() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
            },
        }
    }
    session.remove_viewer(id);
}

/// `GET /s/{token}/cast` — download this session's asciicast recording, if any.
async fn cast_download(State(app): State<Arc<App>>, Path(token): Path<String>) -> Response {
    let Some((session, _)) = app.lookup_token(&token) else {
        return (StatusCode::NOT_FOUND, "unknown or expired session").into_response();
    };
    let path = session.record_path.lock().unwrap().clone();
    let Some(path) = path else {
        return (StatusCode::NOT_FOUND, "no recording for this session").into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/x-asciicast"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"session.cast\"",
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "recording unavailable").into_response(),
    }
}

// Vendored asciinema-player (the live web view's renderer), served self-hosted so the
// page loads no third-party CDN. Embedded gzip-compressed and served with
// `Content-Encoding: gzip` (every browser accepts it) — keeps the binary small.
const PLAYER_JS_GZ: &[u8] = include_bytes!("../assets/asciinema-player.min.js.gz");
const PLAYER_CSS_GZ: &[u8] = include_bytes!("../assets/asciinema-player.css.gz");

async fn asset_js() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CONTENT_ENCODING, "gzip"),
        ],
        PLAYER_JS_GZ,
    )
        .into_response()
}

async fn asset_css() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CONTENT_ENCODING, "gzip"),
        ],
        PLAYER_CSS_GZ,
    )
        .into_response()
}

/// `GET /download` — list prebuilt binaries served from `--downloads`, or install help.
async fn download_index(State(app): State<Arc<App>>) -> Response {
    let Some(dir) = &app.cfg.downloads_dir else {
        return Html(DOWNLOAD_EMPTY_HTML).into_response();
    };
    let mut items = String::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_file())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort();
        for name in names {
            // name is a single path component; href-encode the obvious specials.
            let safe = name.replace('"', "%22");
            items.push_str(&format!(
                "<li><a href=\"/download/{safe}\">{name}</a></li>\n"
            ));
        }
    }
    if items.is_empty() {
        return Html(DOWNLOAD_EMPTY_HTML).into_response();
    }
    Html(format!(
        "<!doctype html><meta charset=utf-8><title>choom downloads</title>\
         <h1>Downloads</h1><ul>{items}</ul>"
    ))
    .into_response()
}

/// `GET /download/{file}` — serve one prebuilt binary (single path segment; no traversal).
async fn download_file(State(app): State<Arc<App>>, Path(file): Path<String>) -> Response {
    let Some(dir) = &app.cfg.downloads_dir else {
        return (StatusCode::NOT_FOUND, "downloads not configured").into_response();
    };
    if file.is_empty() || file.contains('/') || file.contains("..") {
        return (StatusCode::BAD_REQUEST, "bad filename").into_response();
    }
    match tokio::fs::read(dir.join(&file)).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{file}\""),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "no such file").into_response(),
    }
}

/// `GET /metrics` — Prometheus exposition (unauthenticated counters only, no tokens).
async fn metrics(State(app): State<Arc<App>>) -> Response {
    let body = crate::render_metrics().unwrap_or_else(|| {
        format!(
            "# metrics recorder unavailable\nchoom_sessions_active {}\n",
            app.session_count()
        )
    });
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], body).into_response()
}

const DOWNLOAD_EMPTY_HTML: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>choom downloads</title></head><body>
<h1>Downloads</h1>
<p>No prebuilt binaries are published here. Build from source:</p>
<pre>cargo build --release   # -&gt; target/release/choom</pre>
</body></html>"#;

const LANDING_HTML: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>choom</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>body{font:16px/1.5 system-ui,sans-serif;max-width:42rem;margin:3rem auto;padding:0 1rem}code{background:#eee;padding:.1em .3em;border-radius:3px}footer{margin-top:2rem;color:#888;font-size:.85rem}footer a{color:inherit}</style>
</head><body>
<h1>choom</h1>
<p>Minimal SSH terminal sharing relay. A host shares its shell; viewers attach with
stock <code>ssh &lt;token&gt;@host</code> or open the read-only web link they were given.</p>
<p><strong>Warning:</strong> this relay sees session contents in cleartext. Only host on
a relay you trust; run your own for anything sensitive.</p>
<footer>choom &mdash; <a href="https://www.gnu.org/licenses/gpl-3.0.html">GPL-3.0-or-later</a>
&middot; &copy; 2026 Max &#39;ff0x&#39; Reineke &lt;ff0x@this-is-fine.io&gt;</footer>
</body></html>"#;

// The viewer page references a vendored asciinema-player (added as a static asset in a
// follow-up; logic-wise the WS already speaks the player's raw asciicast format).
const VIEWER_HTML: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>choom — session</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<link rel="stylesheet" href="/assets/asciinema-player.css">
<style>html,body{margin:0;height:100%;background:#000}#player{height:100%}</style>
</head><body>
<div id="player"></div>
<script src="/assets/asciinema-player.min.js"></script>
<script>
  const token = location.pathname.split('/').filter(Boolean).pop();
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  const url = `${proto}://${location.host}/s/${token}/stream`;
  if (window.AsciinemaPlayer) {
    // autoPlay is essential: without it the player shows a black poster and does not
    // even open the WebSocket until clicked. fit:'both' needs the sized #player above.
    AsciinemaPlayer.create({ driver: 'websocket', url }, document.getElementById('player'),
      { fit: 'both', autoPlay: true, terminalFontSize: 'small' });
  } else {
    document.body.innerHTML = '<p style="color:#ccc;font-family:monospace;padding:1rem">'
      + 'asciinema-player asset missing (vendoring pending). WS: ' + url + '</p>';
  }
</script>
</body></html>"#;
