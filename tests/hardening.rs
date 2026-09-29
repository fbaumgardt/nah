//! Phase 5 hardening integration tests.
//!
//! Each test spawns the real `nah --daemon` against [`MockNiri`] on a temp
//! `XDG_RUNTIME_DIR` and drives it with the real `nah` client binary. No live
//! daemon and no compositor is involved:
//!
//! - the sender notices a closed action connection while idle and reconnects
//!   on the 200 ms backoff;
//! - the event stream is retried on the same socket path after the compositor
//!   drops it;
//! - a compositor that is gone makes the daemon exit 0 and unlink its socket;
//! - intents during a disconnect are refused with `err niri-disconnected`.

#![cfg(feature = "daemon")]

mod common;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::MockNiri;

/// `ConsumeOrExpelWindowLeft { id: None }` on a horizontal focused output.
const CONSUME_OR_EXPEL_LEFT: &[u8] = b"{\"Action\":{\"ConsumeOrExpelWindowLeft\":{\"id\":null}}}\n";

/// A spawned daemon whose stderr is captured line by line.
struct DaemonHarness {
    child: Option<Child>,
    runtime_dir: PathBuf,
    stderr: Arc<Mutex<String>>,
    stderr_reader: Option<JoinHandle<()>>,
}

impl DaemonHarness {
    fn spawn(mock: &MockNiri, runtime_dir: &Path, extra_env: &[(&str, &str)]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_nah"));
        command
            .arg("--daemon")
            .env("NIRI_SOCKET", mock.socket_path())
            .env("XDG_RUNTIME_DIR", runtime_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        for (key, value) in extra_env {
            command.env(key, value);
        }

        let mut child = command.spawn().expect("spawn nah --daemon");
        let stderr_pipe = child.stderr.take().expect("daemon stderr is piped");
        let stderr = Arc::new(Mutex::new(String::new()));
        let reader_stderr = Arc::clone(&stderr);
        let stderr_reader = thread::spawn(move || {
            for line in BufReader::new(stderr_pipe).lines().map_while(Result::ok) {
                let mut buffer = reader_stderr.lock().unwrap();
                buffer.push_str(&line);
                buffer.push('\n');
            }
        });

        Self {
            child: Some(child),
            runtime_dir: runtime_dir.to_path_buf(),
            stderr,
            stderr_reader: Some(stderr_reader),
        }
    }

    fn socket_path(&self) -> PathBuf {
        self.runtime_dir.join("nah.sock")
    }

    fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    /// Wait until the captured stderr contains `needle`.
    fn wait_for_log(&self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if self.stderr().contains(needle) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "daemon stderr never contained {needle:?}:\n{}",
                self.stderr()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// SIGTERM the daemon, reap it, and return its exit status.
    fn terminate(&mut self) -> ExitStatus {
        let mut child = self.child.take().expect("daemon already reaped");
        // SAFETY: `child.id()` is a live child pid and SIGTERM is handled by
        // the daemon's installed handler.
        unsafe {
            libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
        }
        let status = child.wait().expect("wait for daemon");
        self.join_reader();
        status
    }

    /// Wait for the daemon to exit on its own.
    fn wait_for_exit(&mut self, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            let status = self
                .child
                .as_mut()
                .expect("daemon already reaped")
                .try_wait()
                .expect("try_wait daemon");
            if let Some(status) = status {
                self.child.take();
                self.join_reader();
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "daemon did not exit within {timeout:?}; stderr:\n{}",
                self.stderr()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn join_reader(&mut self) {
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for DaemonHarness {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // SAFETY: same as `terminate`.
            unsafe {
                libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
            }
            let _ = child.wait();
        }
        self.join_reader();
        let _ = fs::remove_dir_all(&self.runtime_dir);
    }
}

#[test]
fn sender_reconnects_after_the_action_connection_drops() {
    let mock = MockNiri::start();
    let runtime = temp_runtime_dir("sender-reconnect");
    let mut daemon = DaemonHarness::spawn(&mock, &runtime, &[]);

    wait_for_status(&runtime, &["focused-output: eDP-1", "axis: horizontal"]);

    // The mock identifies action connections from their first request line,
    // so send one intent before dropping it.
    let reply = run_client(&runtime, "move-left");
    assert_eq!(reply.status.code(), Some(0), "stderr: {:?}", reply.stderr);
    wait_for_actions(&mock, 1);
    assert_eq!(mock.action_connection_count(), 1);

    mock.shutdown_action_streams();

    // Detected while idle (liveness probe), then reconnected on the 200 ms
    // backoff without any further intents being involved.
    daemon.wait_for_log("sender: disconnected");
    daemon.wait_for_log("sender: reconnected to compositor");
    assert_eq!(
        daemon
            .stderr()
            .matches("sender: reconnected to compositor")
            .count(),
        1,
        "reconnect success must log exactly one line; stderr:\n{}",
        daemon.stderr()
    );

    // The next intent succeeds over the fresh action connection.
    let reply = run_client(&runtime, "move-left");
    assert_eq!(reply.status.code(), Some(0), "stderr: {:?}", reply.stderr);
    wait_for_actions(&mock, 2);
    assert_eq!(mock.action_connection_count(), 2);
    for line in mock.action_requests() {
        assert_eq!(line, CONSUME_OR_EXPEL_LEFT);
    }

    assert!(daemon.terminate().success());
    assert!(
        !daemon.socket_path().exists(),
        "daemon left its socket behind"
    );
    assert!(
        !daemon.stderr().contains("panicked"),
        "stderr:\n{}",
        daemon.stderr()
    );
}

#[test]
fn event_stream_reconnects_and_keeps_routing() {
    let mock = MockNiri::start();
    let runtime = temp_runtime_dir("event-reconnect");
    let mut daemon = DaemonHarness::spawn(&mock, &runtime, &[]);

    wait_for_status(&runtime, &["focused-output: eDP-1", "axis: horizontal"]);
    assert_eq!(mock.event_connection_count(), 1);

    // The compositor drops the event stream once; the daemon resubscribes and
    // receives the full state dump again.
    mock.shutdown_event_streams();
    daemon.wait_for_log("nah: reconnected to compositor");
    assert_eq!(mock.event_connection_count(), 2);

    // State survived the reconnect and routing still works.
    let status = wait_for_status(&runtime, &["focused-output: eDP-1", "axis: horizontal"]);
    assert!(
        status.contains("action move-left: ConsumeOrExpelWindowLeft"),
        "{status}"
    );
    let reply = run_client(&runtime, "move-left");
    assert_eq!(reply.status.code(), Some(0), "stderr: {:?}", reply.stderr);
    wait_for_actions(&mock, 1);

    assert!(daemon.terminate().success());
    assert!(
        !daemon.socket_path().exists(),
        "daemon left its socket behind"
    );
}

#[test]
fn compositor_gone_exits_zero_and_unlinks_the_socket() {
    let mock = MockNiri::start();
    let runtime = temp_runtime_dir("compositor-gone");
    // The production deadline is 30 s; the daemon honors this millisecond
    // override so the suite stays fast.
    let mut daemon = DaemonHarness::spawn(
        &mock,
        &runtime,
        &[("NAH_EVENT_RECONNECT_TIMEOUT_MS", "600")],
    );
    wait_for_status(&runtime, &["focused-output: eDP-1"]);

    // Compositor gone: connections closed and the socket file unlinked, so
    // every reconnect attempt gets ENOENT.
    mock.go_away();

    let status = daemon.wait_for_exit(Duration::from_secs(10));
    assert_eq!(status.code(), Some(0), "stderr:\n{}", daemon.stderr());
    assert!(
        !daemon.socket_path().exists(),
        "daemon left its socket behind: {}",
        daemon.socket_path().display()
    );
    assert!(
        daemon.stderr().contains("compositor unreachable"),
        "stderr:\n{}",
        daemon.stderr()
    );
    assert!(
        !daemon.stderr().contains("panicked"),
        "stderr:\n{}",
        daemon.stderr()
    );
}

#[test]
fn intent_while_disconnected_is_refused_without_panic() {
    let mock = MockNiri::start();
    let runtime = temp_runtime_dir("disconnected-intent");
    let mut daemon = DaemonHarness::spawn(&mock, &runtime, &[]);
    wait_for_status(&runtime, &["focused-output: eDP-1", "axis: horizontal"]);

    // The compositor disappears; the sender notices and the path stays
    // unreachable because the mock unlinked its socket.
    mock.go_away();
    daemon.wait_for_log("sender: disconnected");

    let reply = run_client(&runtime, "move-left");
    assert_eq!(reply.status.code(), Some(1), "stdout: {:?}", reply.stdout);
    assert_eq!(
        String::from_utf8_lossy(&reply.stderr),
        "nah: niri-disconnected\n"
    );
    assert!(reply.stdout.is_empty(), "intent client must stay silent");
    assert!(
        !daemon.stderr().contains("panicked"),
        "stderr:\n{}",
        daemon.stderr()
    );

    // SIGTERM still shuts down cleanly while disconnected.
    assert!(daemon.terminate().success());
    assert!(
        !daemon.socket_path().exists(),
        "daemon left its socket behind"
    );
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

/// Poll `nah status` until it reports the fully seeded state.
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

/// Wait until the mock has recorded `count` action lines.
fn wait_for_actions(mock: &MockNiri, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let actions = mock.action_requests();
        if actions.len() >= count {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{count} action(s) never arrived; recorded: {actions:?}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}
