//! Phase 3 end-to-end tests.
//!
//! Each test spawns the real `nah --daemon` binary against a [`MockNiri`],
//! drives it with the real `nah` client binary, and asserts the exact bytes
//! recorded by the mock. Clean termination is verified with SIGTERM.

#![cfg(feature = "daemon")]

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::MockNiri;

/// Externally-tagged unit/fieldless payloads: `{"Action":{"Name":{}}}`.
const MOVE_COLUMN_LEFT: &[u8] = b"{\"Action\":{\"MoveColumnLeft\":{}}}\n";
/// `ConsumeOrExpelWindowLeft { id: None }` carries an explicit `null` id.
const CONSUME_OR_EXPEL_LEFT: &[u8] = b"{\"Action\":{\"ConsumeOrExpelWindowLeft\":{\"id\":null}}}\n";

/// A spawned daemon; sends SIGTERM on drop if the test never terminated it.
struct DaemonProcess {
    child: Option<Child>,
    runtime_dir: PathBuf,
}

impl DaemonProcess {
    fn spawn(mock: &MockNiri, runtime_dir: &Path) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_nah"))
            .arg("--daemon")
            .env("NIRI_SOCKET", mock.socket_path())
            .env("XDG_RUNTIME_DIR", runtime_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn nah --daemon");
        Self {
            child: Some(child),
            runtime_dir: runtime_dir.to_path_buf(),
        }
    }

    fn socket_path(&self) -> PathBuf {
        self.runtime_dir.join("nah.sock")
    }

    /// SIGTERM the daemon and wait for it; returns its exit status.
    fn terminate(&mut self) -> std::process::ExitStatus {
        let mut child = self.child.take().expect("daemon already terminated");
        // SAFETY: `child.id()` is a live child pid and SIGTERM is handled by
        // the daemon's installed handler.
        unsafe {
            libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
        }
        child.wait().expect("wait for daemon")
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // SAFETY: same as `terminate`.
            unsafe {
                libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
            }
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.runtime_dir);
    }
}

fn temp_runtime_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before the UNIX epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("nah-{label}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&path).expect("create temp runtime dir");
    path
}

fn run_client(runtime_dir: &Path, command: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nah"))
        .arg(command)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env_remove("NIRI_SOCKET")
        .output()
        .expect("run nah client")
}

#[test]
fn horizontal_output_routes_consume_or_expel_and_status_is_live() {
    let mock = MockNiri::start(); // eDP-1 (Normal) focused
    let runtime = temp_runtime_dir("horizontal");
    let mut daemon = DaemonProcess::spawn(&mock, &runtime);

    let status = wait_for_status(
        &runtime,
        &[
            "focused-output: eDP-1",
            "focused-window: 7",
            "axis: horizontal",
        ],
    );
    assert!(status.contains("focused-window: 7"), "{status}");
    assert!(status.contains("axis: horizontal"), "{status}");
    assert!(
        status.contains("action move-left: ConsumeOrExpelWindowLeft"),
        "{status}"
    );
    assert!(
        status.contains("action move-right: ConsumeOrExpelWindowRight"),
        "{status}"
    );
    assert!(status.contains("action move-up: MoveWindowUp"), "{status}");
    assert!(
        status.contains("action move-down: MoveWindowDown"),
        "{status}"
    );

    let reply = run_client(&runtime, "move-left");
    assert_eq!(reply.status.code(), Some(0), "stderr: {:?}", reply.stderr);
    assert!(reply.stdout.is_empty(), "intent client must stay silent");
    assert!(reply.stderr.is_empty(), "stderr: {:?}", reply.stderr);

    wait_for_action(&mock, CONSUME_OR_EXPEL_LEFT);
    // The axis map was seeded exactly once, from the initial WorkspacesChanged.
    assert_eq!(
        mock.outputs_requests(),
        vec![b"\"Outputs\"\n".to_vec()],
        "Outputs should only be queried for new output names"
    );

    let status = daemon.terminate();
    assert!(status.success(), "SIGTERM exit was {status:?}");
    assert!(
        !daemon.socket_path().exists(),
        "daemon left its socket behind: {}",
        daemon.socket_path().display()
    );
}

#[test]
fn vertical_output_routes_columns_and_consume_or_expel() {
    let mock = MockNiri::start_focused("DP-2"); // 90° → vertical
    let runtime = temp_runtime_dir("vertical");
    let mut daemon = DaemonProcess::spawn(&mock, &runtime);

    let status = wait_for_status(&runtime, &["focused-output: DP-2", "axis: vertical"]);
    assert!(status.contains("axis: vertical"), "{status}");
    assert!(
        status.contains("action move-left: MoveColumnLeft"),
        "{status}"
    );
    assert!(
        status.contains("action move-right: MoveColumnRight"),
        "{status}"
    );
    assert!(
        status.contains("action move-up: ConsumeOrExpelWindowLeft"),
        "{status}"
    );
    assert!(
        status.contains("action move-down: ConsumeOrExpelWindowRight"),
        "{status}"
    );

    // The socket is private to this user.
    let mode = fs::metadata(daemon.socket_path())
        .expect("daemon socket metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "daemon socket mode");

    let reply = run_client(&runtime, "move-left");
    assert_eq!(reply.status.code(), Some(0), "stderr: {:?}", reply.stderr);
    wait_for_action(&mock, MOVE_COLUMN_LEFT);

    // The client can keep using the same daemon afterwards.
    let reply = run_client(&runtime, "move-up");
    assert_eq!(reply.status.code(), Some(0), "stderr: {:?}", reply.stderr);
    wait_for_action(&mock, CONSUME_OR_EXPEL_LEFT);

    let status = daemon.terminate();
    assert!(status.success(), "SIGTERM exit was {status:?}");
    assert!(!daemon.socket_path().exists());
}

#[test]
fn second_daemon_refuses_a_live_socket() {
    let mock = MockNiri::start();
    let runtime = temp_runtime_dir("live-guard");
    let mut first = DaemonProcess::spawn(&mock, &runtime);
    wait_for_status(&runtime, &["focused-output: eDP-1", "axis: horizontal"]);

    let second = Command::new(env!("CARGO_BIN_EXE_nah"))
        .arg("--daemon")
        .env("NIRI_SOCKET", mock.socket_path())
        .env("XDG_RUNTIME_DIR", &runtime)
        .output()
        .expect("run second daemon");
    assert!(
        !second.status.success(),
        "second daemon must not steal a live socket"
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("another daemon is already listening"),
        "stderr: {stderr}"
    );

    // The first daemon is still serving.
    let status = run_client(&runtime, "status");
    assert!(status.status.success());
    assert!(String::from_utf8_lossy(&status.stdout).contains("focused-output: eDP-1"));

    assert!(first.terminate().success());
    assert!(!first.socket_path().exists());
}

#[test]
fn daemon_replaces_a_stale_socket() {
    let mock = MockNiri::start();
    let runtime = temp_runtime_dir("stale");

    // A bound-then-dropped listener leaves the socket file behind; connecting
    // to it fails, so the daemon must unlink it and bind a fresh one.
    {
        let stale = UnixListener::bind(runtime.join("nah.sock")).expect("bind stale socket");
        drop(stale);
    }
    assert!(runtime.join("nah.sock").exists());

    let mut daemon = DaemonProcess::spawn(&mock, &runtime);
    wait_for_status(&runtime, &["focused-output: eDP-1", "axis: horizontal"]);

    assert!(daemon.terminate().success());
    assert!(!daemon.socket_path().exists());
}

#[test]
fn client_without_daemon_reports_not_running() {
    let runtime = temp_runtime_dir("no-daemon");

    let output = run_client(&runtime, "move-left");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "nah: daemon not running\n"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn client_prints_daemon_error_and_exits_one() {
    let runtime = temp_runtime_dir("err-reply");
    let listener = UnixListener::bind(runtime.join("nah.sock")).expect("bind fake daemon");
    let server = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let half = stream.try_clone().expect("clone fake daemon socket");
            let mut request = Vec::new();
            let _ = BufReader::new(half).read_until(b'\n', &mut request);
            let _ = stream.write_all(b"err busy\n");
        }
    });

    let output = run_client(&runtime, "move-left");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&output.stderr), "nah: busy\n");
    assert!(output.stdout.is_empty());
    server.join().expect("fake daemon thread");
}

#[test]
fn client_times_out_when_daemon_never_replies() {
    let runtime = temp_runtime_dir("timeout");
    let listener = UnixListener::bind(runtime.join("nah.sock")).expect("bind fake daemon");
    let server = thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let half = stream.try_clone().expect("clone fake daemon socket");
            let mut request = Vec::new();
            let _ = BufReader::new(half).read_until(b'\n', &mut request);
            // Hold the connection open without ever replying.
            thread::sleep(Duration::from_millis(500));
            drop(stream);
        }
    });

    let started = Instant::now();
    let output = run_client(&runtime, "move-left");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&output.stderr), "nah: timeout\n");
    assert!(started.elapsed() < Duration::from_secs(2));
    server.join().expect("fake daemon thread");
}

#[test]
fn unknown_command_exits_two_with_usage() {
    let output = Command::new(env!("CARGO_BIN_EXE_nah"))
        .arg("frobnicate")
        .output()
        .expect("run nah with an unknown command");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("usage:"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Poll `nah status` until it reports the fully seeded state: the daemon may
/// not have consumed the whole event-stream dump (or finished its synchronous
/// `Outputs` refresh) yet.
fn wait_for_status(runtime_dir: &Path, needles: &[&str]) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = run_client(runtime_dir, "status");
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            if needles.iter().all(|needle| stdout.contains(needle)) {
                return stdout;
            }
        }
        assert!(
            Instant::now() < deadline,
            "`nah status` never contained {needles:?}; last reply: {:?}",
            String::from_utf8_lossy(&output.stdout)
        );
        thread::sleep(Duration::from_millis(25));
    }
}

/// The sender thread delivers asynchronously: poll the mock until the exact
/// action line shows up.
fn wait_for_action(mock: &MockNiri, expected: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let actions = mock.action_requests();
        if actions.iter().any(|line| line == expected) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "action {expected:?} never arrived; recorded: {actions:?}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}
