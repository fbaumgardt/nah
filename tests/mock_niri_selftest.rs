//! End-to-end mock-compositor test for the Phase 2 action sender.
//!
//! Spawns the real `nah --selftest` binary against the mock niri server from
//! `tests/common` and asserts the exact bytes on the wire, the logged replies,
//! and the exit status.

#![cfg(feature = "daemon")]

mod common;

use std::process::Command;

use common::MockNiri;

/// `Request::EventStream` is a unit variant, so it serializes as a bare JSON
/// string: exactly this line, including the trailing newline.
const EVENT_STREAM_REQUEST: &[u8] = b"\"EventStream\"\n";

/// niri-ipc 26.4.0 defines `ToggleWindowFloating { id: Option<u64> }`, so the
/// canonical request line carries an explicit `null` id. Verified against the
/// live compositor: `niri msg --print-request action toggle-window-floating`
/// prints `{"Action":{"ToggleWindowFloating":{"id":null}}}`.
const TOGGLE_FLOATING_REQUEST: &[u8] = b"{\"Action\":{\"ToggleWindowFloating\":{\"id\":null}}}\n";

#[test]
fn selftest_handshakes_and_sends_exact_action_bytes() {
    let mock = MockNiri::start();

    let output = Command::new(env!("CARGO_BIN_EXE_nah"))
        .arg("--selftest")
        .env("NIRI_SOCKET", mock.socket_path())
        .output()
        .expect("run nah --selftest");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "nah --selftest failed with {:?}\nstderr:\n{stderr}",
        output.status
    );

    assert_eq!(
        mock.event_stream_requests(),
        vec![EVENT_STREAM_REQUEST.to_vec()],
        "event-stream handshake bytes"
    );
    let expected_actions = vec![
        TOGGLE_FLOATING_REQUEST.to_vec(),
        TOGGLE_FLOATING_REQUEST.to_vec(),
    ];
    assert_eq!(mock.action_requests(), expected_actions, "action bytes");

    // The handshake plus both action replies are logged verbatim.
    let handled_lines = stderr.matches("reply: {\"Ok\":\"Handled\"}").count();
    assert_eq!(
        handled_lines, 3,
        "expected handshake and two action replies in stderr:\n{stderr}"
    );
}
