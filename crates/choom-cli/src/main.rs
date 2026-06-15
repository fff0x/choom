// SPDX-License-Identifier: GPL-3.0-or-later
//! The `choom` binary — a thin entrypoint around the [`choom_cli`] library (which the
//! man-page generator also reuses).

fn main() -> std::process::ExitCode {
    if let Err(e) = choom_cli::run() {
        // One clean line ("choom: outer: cause") instead of anyhow's multi-line
        // "Error: …\n\nCaused by:" Debug dump.
        eprintln!("choom: {e:#}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}
