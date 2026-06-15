// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-session asciicast recording (opt-in) and retention GC.
//!
//! A recorder subscribes to the session's live output (like a viewer) and appends
//! asciicast events to a `0600` `.cast` file: the header, the current screen as the
//! first output event (so nothing before recording started is lost), then live output.
//! Recordings can contain secrets shown on screen, hence opt-in + private perms.

use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::AsyncWriteExt as _;
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, warn};

use crate::asciicast::{self, Code};
use crate::session::SessionShared;

/// Begin recording `session` into `dir` (idempotent). Returns the file path.
pub fn start(session: &Arc<SessionShared>, dir: &Path) -> std::io::Result<PathBuf> {
    if let Some(existing) = session.record_path.lock().unwrap().clone() {
        return Ok(existing);
    }
    std::fs::create_dir_all(dir)?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("{}-{}.cast", session.sid, ts));

    let (cols, rows, snapshot) = {
        let st = session.state.lock().unwrap();
        (st.neg_cols, st.neg_rows, crate::render::snapshot(&st.vt))
    };

    let std_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    let mut file = tokio::fs::File::from_std(std_file);
    *session.record_path.lock().unwrap() = Some(path.clone());

    let mut rx = session.output_tx.subscribe();
    let cancel = session.cancel.clone();
    let sid = session.sid;
    let header = asciicast::header_line(cols, rows);
    let snap_event = asciicast::event_line(0.0, Code::Output, &String::from_utf8_lossy(&snapshot));

    tokio::spawn(async move {
        let start = Instant::now();
        let mut clock = asciicast::Clock::default();
        if file.write_all(header.as_bytes()).await.is_err()
            || file.write_all(snap_event.as_bytes()).await.is_err()
        {
            return;
        }
        let _ = file.flush().await;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                msg = rx.recv() => match msg {
                    Ok(bytes) => {
                        let interval = clock.interval(start.elapsed().as_secs_f64());
                        let line = asciicast::event_line(
                            interval, Code::Output, &String::from_utf8_lossy(&bytes),
                        );
                        if file.write_all(line.as_bytes()).await.is_err() {
                            break;
                        }
                        let _ = file.flush().await;
                    }
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                },
            }
        }
        let _ = file.flush().await;
        debug!(sid, "recording finalised");
    });

    Ok(path)
}

/// Prune recordings by age then by total size (oldest first). Runs synchronously.
pub fn gc(dir: &Path, max_age_days: u64, max_mb: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut casts: Vec<(PathBuf, SystemTime, u64)> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("cast") {
                return None;
            }
            let meta = e.metadata().ok()?;
            Some((p, meta.modified().unwrap_or(UNIX_EPOCH), meta.len()))
        })
        .collect();

    if max_age_days > 0 {
        if let Some(cutoff) =
            SystemTime::now().checked_sub(Duration::from_secs(max_age_days * 86400))
        {
            casts.retain(|(p, mtime, _)| {
                if *mtime < cutoff {
                    let _ = std::fs::remove_file(p);
                    false
                } else {
                    true
                }
            });
        }
    }

    if max_mb > 0 {
        let cap = max_mb * 1024 * 1024;
        let mut total: u64 = casts.iter().map(|(_, _, s)| *s).sum();
        if total > cap {
            casts.sort_by_key(|(_, m, _)| *m); // oldest first
            for (p, _, sz) in &casts {
                if total <= cap {
                    break;
                }
                if std::fs::remove_file(p).is_ok() {
                    total = total.saturating_sub(*sz);
                } else {
                    warn!(path = %p.display(), "failed to prune recording");
                }
            }
        }
    }
}
