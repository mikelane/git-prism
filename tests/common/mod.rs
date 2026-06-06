//! Shared test helpers for shim telemetry end-to-end tests.
//!
//! This module is NOT compiled as its own test binary — `tests/common/mod.rs`
//! is a plain module that each integration test opts into with `mod common;`.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

/// Spawn a one-shot HTTP capture server that answers immediately (healthy collector).
///
/// Returns the bound base URL and a receiver that yields `true` once ANY HTTP
/// request body is received.
///
/// # Harness failure detection
///
/// The receiver distinguishes three outcomes:
///
/// - `Ok(true)` — a request body arrived (telemetry was emitted)
/// - `Err(RecvTimeoutError::Timeout)` — no request within the timeout window
///   (legitimate "no telemetry" result in opt-out / passthrough tests)
/// - `Err(RecvTimeoutError::Disconnected)` — the capture-server thread died
///   (channel sender was dropped without sending); this is a harness failure,
///   NOT a telemetry signal. Callers must treat `Disconnected` as a test
///   failure rather than silently reading it as "no telemetry received".
pub fn spawn_capture_server() -> (String, mpsc::Receiver<bool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{}", addr);
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .ok();
            let mut buf = [0u8; 4096];
            let got = matches!(stream.read(&mut buf), Ok(n) if n > 0);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            if got {
                let _ = tx.send(true);
            }
        }
    });

    (base, rx)
}
