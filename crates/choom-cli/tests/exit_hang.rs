// SPDX-License-Identifier: GPL-3.0-or-later
//! End-to-end regression test for the "`exit` doesn't quit until you press Return again"
//! bug: the host runs the shell behind `tokio::io::stdin()`, whose blocking read cannot
//! be cancelled. The runtime drop used to block on that parked read, so the process hung
//! after the shell exited until the next keypress. `run_with` now abandons blocking
//! threads on shutdown, so the host must exit promptly when the shell ends.
//!
//! This drives the real `choom` binary as a host attached to a PTY, points it at a real
//! relay, types `exit`, and asserts the process is gone within a few seconds.

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Pick a shell that exists, else skip (keeps the test portable across CI images).
fn shell() -> Option<&'static str> {
    ["/bin/sh", "/bin/bash"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
}

fn wait_exit(child: &mut Child, within: Duration) -> Option<i32> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.code().unwrap_or(-1)),
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return None,
        }
    }
    None
}

#[test]
fn host_exits_promptly_when_shell_exits() {
    let Some(sh) = shell() else {
        eprintln!("no /bin/sh; skipping");
        return;
    };
    let bin = env!("CARGO_BIN_EXE_choom");
    // An ephemeral-ish high port; the relay binds 127.0.0.1 only.
    let port = "2238";

    // --- relay ---
    let mut relay = Command::new(bin)
        .args(["server", "--ssh-addr", &format!("127.0.0.1:{port}"), "-H", "127.0.0.1", "-p", port])
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn relay");
    std::thread::sleep(Duration::from_millis(1000));

    // --- host attached to a PTY ---
    let (pty, pts) = pty_process::blocking::open().expect("open pty");
    pty.resize(pty_process::Size::new(24, 80)).ok();
    let mut host = pty_process::blocking::Command::new(bin)
        .args(["host", "-s", "127.0.0.1", "-p", port])
        .env("RUST_LOG", "warn")
        .env("SHELL", sh)
        .spawn(pts)
        .expect("spawn host");

    // A single thread owns the PTY master: it drains the host's output (so the host never
    // blocks on a full PTY buffer), reports when the share banner appears, and — once the
    // session is up — types `exit\n` into the shell.
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut pty = pty;
        let mut buf = [0u8; 4096];
        let mut acc = String::new();
        let mut typed_exit = false;
        loop {
            match pty.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => acc.push_str(&String::from_utf8_lossy(&buf[..n])),
            }
            if !typed_exit && acc.contains("shell is now shared") {
                typed_exit = true;
                let _ = tx.send(());
                let _ = pty.write_all(b"exit\n");
                let _ = pty.flush();
            }
        }
    });

    // Confirm the session came up (host connected + shell started + banner printed).
    assert!(
        rx.recv_timeout(Duration::from_secs(5)).is_ok(),
        "host never printed its share banner; did it connect?"
    );

    // The host must terminate promptly after the shell exits. Before the fix it hung here
    // until another byte arrived on stdin.
    let code = wait_exit(&mut host, Duration::from_secs(6));
    let exited = code.is_some();

    // Cleanup before asserting.
    if !exited {
        let _ = host.kill();
    }
    let _ = host.wait();
    let _ = relay.kill();
    let _ = relay.wait();

    assert!(
        exited,
        "host did not exit within 6s after the shell exited (exit-hang regression)"
    );
}
