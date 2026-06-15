// SPDX-License-Identifier: GPL-3.0-or-later
//! Output-only web view: an axum server that streams a session as a live **asciicast**
//! over a WebSocket (consumed by asciinema-player), plus a landing page and the viewer
//! page. There is **no input path** — the WebSocket ignores all client frames, so the
//! browser is structurally a spectator (a deliberate security property).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use choom_proto::{Mode, Transport};
use futures::{SinkExt, StreamExt};
use tokio_util::io::ReaderStream;
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
        .layer(middleware::from_fn(security_headers))
        .with_state(app)
}

/// Build the (separate) metrics router. Kept off [`router`] so `/metrics` is never exposed
/// on the public web listener — it is operator-internal and bound separately (loopback).
pub fn metrics_router(app: Arc<App>) -> Router {
    Router::new()
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

/// Bind and serve the operator-internal Prometheus endpoint until the process exits.
pub async fn serve_metrics(app: Arc<App>, addr: SocketAddr) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, "choom metrics endpoint listening");
    axum::serve(listener, metrics_router(app)).await?;
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
            // `'wasm-unsafe-eval'` is required: asciinema-player runs its VT engine (avt)
            // as WebAssembly, which CSP blocks without it (and `'unsafe-inline'` does not
            // cover it). It is the narrow directive that allows WASM only — not general
            // `eval()` — so it stays tight.
            // `object-src/base-uri/frame-ancestors 'none'` are pure tighten-only additions
            // (the page embeds no plugins, sets no <base>, and must never be framed —
            // consistent with `X-Frame-Options: DENY`). NOTE: `script-src 'unsafe-inline'`
            // and the broad `connect-src ws: wss:` are still here; dropping them (script
            // hash/nonce + `connect-src 'self'`) needs a browser check that the live player
            // keeps working, so it's a deliberate, documented follow-up.
            "default-src 'self'; script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'; \
             style-src 'self' 'unsafe-inline'; connect-src 'self' ws: wss:; \
             object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
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
async fn cast_download(
    State(app): State<Arc<App>>,
    method: Method,
    Path(token): Path<String>,
) -> Response {
    // Resolve via the recordings index (kept past session end), not the live session, so a
    // viewer can still grab the `.cast` after the session is over — until it's GC'd.
    let Some(path) = app.recording_path(&token) else {
        return (StatusCode::NOT_FOUND, "no recording for this session").into_response();
    };
    // The viewer page probes with HEAD to decide whether to show the download button —
    // answer from the file's existence without reading its (potentially large) contents.
    if method == Method::HEAD {
        return match tokio::fs::metadata(&path).await {
            Ok(_) => [(header::CONTENT_TYPE, "application/x-asciicast")].into_response(),
            Err(_) => (StatusCode::NOT_FOUND, "recording unavailable").into_response(),
        };
    }
    // Stream the file instead of reading it whole into memory: a recording has no per-file
    // size cap, so a full read would let many parallel `/cast` requests exhaust RAM.
    stream_file(
        &app,
        &path,
        "application/x-asciicast",
        Some("session.cast"),
    )
    .await
}

/// Stream a file from disk to the client (never read whole into memory), holding a download
/// slot for the full body so the number of concurrent downloads stays bounded. `disposition`
/// is the already-sanitised `Content-Disposition` filename, if any.
async fn stream_file(
    app: &App,
    path: &std::path::Path,
    content_type: &'static str,
    disposition: Option<&str>,
) -> Response {
    let Ok(permit) = app.downloads_inflight.clone().try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "too many concurrent downloads, retry shortly",
        )
            .into_response();
    };
    let Ok(file) = tokio::fs::File::open(path).await else {
        return (StatusCode::NOT_FOUND, "file unavailable").into_response();
    };
    // The closure owns `permit`, so the download slot is released only when the body
    // stream is fully sent (or the client drops and the stream is dropped).
    let stream = ReaderStream::new(file).map(move |chunk| {
        let _slot = &permit;
        chunk
    });
    let mut res = Body::from_stream(stream).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if let Some(name) = disposition {
        if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
            h.insert(header::CONTENT_DISPOSITION, v);
        }
    }
    res
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
            // The filename comes off the filesystem (operator-controlled, but still
            // untrusted from this page's POV) and lands in HTML — escape it so a crafted
            // name like `<img src=x onerror=…>` is rendered as text, not executed (stored
            // XSS). Only whitelisted names (see `download_file`) are actually downloadable;
            // we still link + escape every listed name.
            let esc = html_escape(&name);
            items.push_str(&format!("<li><a href=\"/download/{esc}\">{esc}</a></li>\n"));
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
    // Whitelist the name to `[A-Za-z0-9._-]` (and reject `.`/`..`). This subsumes the old
    // `/`/`..` check and additionally blocks anything that could break out of the
    // `Content-Disposition` quoting or inject a header (`"`, CR/LF, …).
    if !is_safe_filename(&file) {
        return (StatusCode::BAD_REQUEST, "bad filename").into_response();
    }
    // Stream (don't read whole into memory) so large binaries can't be a memory amplifier.
    stream_file(&app, &dir.join(&file), "application/octet-stream", Some(&file)).await
}

/// HTML-escape the five significant characters so a string is safe in element text and in
/// a double-quoted attribute.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// A safe single download filename: non-empty, only `[A-Za-z0-9._-]`, and not `.`/`..`.
fn is_safe_filename(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
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
<style>html,body{margin:0;height:100%;background:#000}#player{height:100%}
#dl{position:fixed;top:8px;right:8px;z-index:2147483647;display:none;padding:.35em .7em;
border-radius:4px;background:rgba(20,20,20,.85);color:#ddd;font:13px/1 system-ui,sans-serif;
text-decoration:none;border:1px solid #555;box-shadow:0 1px 4px rgba(0,0,0,.5)}
#dl:hover{background:rgba(40,40,40,.95);color:#fff}</style>
</head><body>
<a id="dl" download>⤓ Download recording</a>
<div id="player"></div>
<script src="/assets/asciinema-player.min.js"></script>
<script>
  const token = location.pathname.split('/').filter(Boolean).pop();
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  const url = `${proto}://${location.host}/s/${token}/stream`;
  // Show the download button only when this session was actually recorded.
  const cast = `/s/${token}/cast`;
  fetch(cast, { method: 'HEAD' }).then(r => {
    if (r.ok) { const a = document.getElementById('dl'); a.href = cast; a.style.display = 'block'; }
  }).catch(() => {});
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
