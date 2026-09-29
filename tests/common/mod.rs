//! Reusable mock niri IPC server for integration tests.
//!
//! It speaks the same newline-delimited JSON protocol as the compositor and
//! accepts connections dynamically, one thread each:
//!
//! 1. `"EventStream"` — answers `{"Ok":"Handled"}`, sends the canned
//!    `WorkspacesChanged` + `WindowsChanged` full-state dump, then keeps the
//!    connection open (like the real compositor) until the client closes.
//! 2. `"Outputs"` — answers with the real `tests/fixtures/outputs.json` map
//!    wrapped as `{"Ok":{"Outputs":{…}}}`.
//! 3. anything else — treated as the action-sender connection: every request
//!    line is recorded verbatim and answered `{"Ok":"Handled"}`.
//!
//! The default seed focuses `eDP-1` (transform `Normal` → horizontal axis).
//! [`MockNiri::start_focused`] focuses another output for vertical-axis tests.
//!
//! Phase 5 tests also drive connection failures: [`MockNiri::shutdown_action_streams`]
//! and [`MockNiri::shutdown_event_streams`] close one side of the mock's
//! connections, and [`MockNiri::go_away`] closes everything and unlinks the
//! socket so reconnects fail with ENOENT. Connection counters let tests wait
//! for reconnects.

#![allow(dead_code)] // The router/client integration tests reuse these accessors.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

/// One request line exactly as read from the client, trailing `\n` included.
pub type RawLine = Vec<u8>;

/// One focused window, sent right after `WorkspacesChanged` exactly like the
/// real full-state dump does.
pub const CANNED_WINDOWS_CHANGED: &str = concat!(
    r#"{"WindowsChanged":{"windows":[{"id":7,"title":"canned window","app_id":"nah-test","#,
    r#""workspace_id":1,"is_focused":true,"is_floating":false}]}}"#,
);

/// A mock compositor listening on a private temp UNIX socket.
///
/// The server thread is detached on drop: if a test fails before the client
/// connects, `Drop` must not block on a pending `accept`.
pub struct MockNiri {
    path: PathBuf,
    state: Arc<MockState>,
    _server: JoinHandle<()>,
}

struct MockState {
    event_stream_requests: Mutex<Vec<RawLine>>,
    action_requests: Mutex<Vec<RawLine>>,
    outputs_requests: Mutex<Vec<RawLine>>,
    workspaces_changed: String,
    outputs_response: String,
    /// Every accepted connection, identified or not, so `go_away` can close
    /// even a connection whose first request line has not arrived yet.
    all_streams: Mutex<Vec<UnixStream>>,
    /// Identified `EventStream` connections (closed by
    /// [`MockNiri::shutdown_event_streams`]).
    event_streams: Mutex<Vec<UnixStream>>,
    /// Identified action connections (closed by
    /// [`MockNiri::shutdown_action_streams`]).
    action_streams: Mutex<Vec<UnixStream>>,
    event_connections: AtomicUsize,
    action_connections: AtomicUsize,
}

impl MockNiri {
    /// Bind a fresh socket and seed the event stream with `eDP-1` focused
    /// (`Normal` transform, horizontal axis).
    pub fn start() -> Self {
        Self::start_focused("eDP-1")
    }

    /// Like [`MockNiri::start`], but the seeded focused workspace sits on
    /// `focused_output` (use `DP-2` for the vertical-axis path).
    pub fn start_focused(focused_output: &str) -> Self {
        let path = unique_socket_path();
        let listener = UnixListener::bind(&path).expect("bind mock niri socket");
        let state = Arc::new(MockState {
            event_stream_requests: Mutex::new(Vec::new()),
            action_requests: Mutex::new(Vec::new()),
            outputs_requests: Mutex::new(Vec::new()),
            workspaces_changed: workspaces_changed(focused_output),
            outputs_response: outputs_response(),
            all_streams: Mutex::new(Vec::new()),
            event_streams: Mutex::new(Vec::new()),
            action_streams: Mutex::new(Vec::new()),
            event_connections: AtomicUsize::new(0),
            action_connections: AtomicUsize::new(0),
        });

        let server = {
            let state = Arc::clone(&state);
            thread::spawn(move || serve(listener, state))
        };

        Self {
            path,
            state,
            _server: server,
        }
    }

    /// Socket path to point `NIRI_SOCKET` at.
    pub fn socket_path(&self) -> &Path {
        &self.path
    }

    /// Every request line received on event-stream connections.
    pub fn event_stream_requests(&self) -> Vec<RawLine> {
        self.state.event_stream_requests.lock().unwrap().clone()
    }

    /// Every request line received on action connections.
    pub fn action_requests(&self) -> Vec<RawLine> {
        self.state.action_requests.lock().unwrap().clone()
    }

    /// Every request line received on `Outputs` connections.
    pub fn outputs_requests(&self) -> Vec<RawLine> {
        self.state.outputs_requests.lock().unwrap().clone()
    }

    /// Number of `EventStream` connections served so far.
    pub fn event_connection_count(&self) -> usize {
        self.state.event_connections.load(Ordering::SeqCst)
    }

    /// Number of identified action connections so far. A connection is only
    /// identifiable once its first request line arrives, so this counts
    /// connections that have actually carried an intent.
    pub fn action_connection_count(&self) -> usize {
        self.state.action_connections.load(Ordering::SeqCst)
    }

    /// Close every live event-stream connection; the listener keeps accepting,
    /// so the daemon can reconnect.
    pub fn shutdown_event_streams(&self) {
        shutdown_streams(&self.state.event_streams);
    }

    /// Close every identified action connection; the listener keeps accepting.
    pub fn shutdown_action_streams(&self) {
        shutdown_streams(&self.state.action_streams);
    }

    /// Close every connection *and* unlink the socket file, so reconnects now
    /// fail with ENOENT: the compositor is gone.
    pub fn go_away(&self) {
        shutdown_streams(&self.state.all_streams);
        shutdown_streams(&self.state.event_streams);
        shutdown_streams(&self.state.action_streams);
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for MockNiri {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Build the seeded `WorkspacesChanged`: workspace 1 focused on
/// `focused_output`, workspace 2 active elsewhere.
fn workspaces_changed(focused_output: &str) -> String {
    let other = if focused_output == "DP-2" {
        "eDP-1"
    } else {
        "DP-2"
    };
    format!(
        concat!(
            r#"{{"WorkspacesChanged":{{"workspaces":["#,
            r#"{{"id":1,"idx":1,"name":null,"output":"{}","is_urgent":false,"#,
            r#""is_active":true,"is_focused":true,"active_window_id":7,"is_hidden":false}},"#,
            r#"{{"id":2,"idx":1,"name":null,"output":"{}","is_urgent":false,"#,
            r#""is_active":true,"is_focused":false,"active_window_id":null,"is_hidden":false}}]}}}}"#,
        ),
        focused_output, other
    )
}

/// The real captured `Outputs` fixture, minified onto one line and wrapped in
/// the `{"Ok":{"Outputs":…}}` reply envelope.
fn outputs_response() -> String {
    let outputs: serde_json::Value = serde_json::from_str(include_str!("../fixtures/outputs.json"))
        .expect("outputs fixture parses");
    format!(
        "{{\"Ok\":{{\"Outputs\":{}}}}}\n",
        serde_json::to_string(&outputs).expect("outputs fixture serializes")
    )
}

fn serve(listener: UnixListener, state: Arc<MockState>) {
    loop {
        match listener.accept() {
            Ok((stream, _peer)) => {
                // Register every accepted connection so `go_away` can close
                // even one whose first request line has not arrived yet.
                if let Ok(clone) = stream.try_clone() {
                    state.all_streams.lock().unwrap().push(clone);
                }
                let state = Arc::clone(&state);
                thread::spawn(move || handle_connection(stream, state));
            }
            Err(_) => return,
        }
    }
}

/// Shut down every socket in `streams`; already-closed entries are ignored.
fn shutdown_streams(streams: &Mutex<Vec<UnixStream>>) {
    for stream in streams.lock().unwrap().iter() {
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
}

fn handle_connection(stream: UnixStream, state: Arc<MockState>) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;

    let mut line = read_line(&mut reader);
    if line.is_empty() {
        return;
    }
    let request = trim(&line);

    if request == b"\"EventStream\"" {
        state.event_stream_requests.lock().unwrap().push(line);
        state.event_connections.fetch_add(1, Ordering::SeqCst);
        if let Ok(clone) = writer.try_clone() {
            state.event_streams.lock().unwrap().push(clone);
        }

        // Reply and the canned full-state dump go out in one write: the client
        // is still blocked reading the reply, so this cannot race its close.
        let mut response = String::from("{\"Ok\":\"Handled\"}\n");
        response.push_str(&state.workspaces_changed);
        response.push('\n');
        response.push_str(CANNED_WINDOWS_CHANGED);
        response.push('\n');
        if writer.write_all(response.as_bytes()).is_err() {
            return;
        }
        let _ = writer.flush();

        // Stay open like the real compositor: drain anything until EOF.
        loop {
            let next = read_line(&mut reader);
            if next.is_empty() {
                return;
            }
        }
    }

    if request == b"\"Outputs\"" {
        state.outputs_requests.lock().unwrap().push(line);
        let _ = writer.write_all(state.outputs_response.as_bytes());
        let _ = writer.flush();
        return;
    }

    // The action-sender connection: record and ack every line until EOF.
    state.action_connections.fetch_add(1, Ordering::SeqCst);
    if let Ok(clone) = writer.try_clone() {
        state.action_streams.lock().unwrap().push(clone);
    }
    loop {
        if line.is_empty() {
            return;
        }
        state.action_requests.lock().unwrap().push(line);
        if writer.write_all(b"{\"Ok\":\"Handled\"}\n").is_err() {
            return;
        }
        let _ = writer.flush();
        line = read_line(&mut reader);
    }
}

/// Read one `\n`-terminated request; empty when the client closed.
fn read_line(reader: &mut impl BufRead) -> RawLine {
    let mut line = Vec::with_capacity(256);
    let _ = reader.read_until(b'\n', &mut line);
    line
}

fn trim(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn unique_socket_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before the UNIX epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("nah-mock-{}-{nanos}.sock", std::process::id()))
}
