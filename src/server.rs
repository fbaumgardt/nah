//! The daemon's private UNIX socket.
//!
//! Protocol (one request per line): `move-left|move-right|move-up|move-down`
//! and `status`. The first reply line is always `ok` or `err <reason>`; a
//! `status` success continues with `key: value` payload lines terminated by a
//! single `.` line.
//!
//! The listener lives at `$XDG_RUNTIME_DIR/nah.sock` with mode `0600`. Before
//! binding, an existing socket that still accepts connections means another
//! daemon is live: this process logs and exits instead of stealing it. Peers
//! are checked with `SO_PEERCRED` and closed when their uid differs from ours.

use std::fmt;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use crate::protocol::Action;
use crate::router::{route, Intent};
use crate::sender::{EnqueueError, SenderHandle};
use crate::state::{Axis, SharedState};

/// Why the intent socket could not be set up.
#[derive(Debug)]
pub enum ServerError {
    /// `$XDG_RUNTIME_DIR` is unset, so the socket path is undefined.
    RuntimeDirUnset,
    /// Another daemon is already accepting connections on the socket path.
    AlreadyRunning(PathBuf),
    /// Bind/permission/unlink failure.
    Io(io::Error),
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerError::RuntimeDirUnset => {
                f.write_str("XDG_RUNTIME_DIR is not set; cannot determine the daemon socket path")
            }
            ServerError::AlreadyRunning(path) => {
                write!(
                    f,
                    "another daemon is already listening on {}",
                    path.display()
                )
            }
            ServerError::Io(error) => write!(f, "daemon socket error: {error}"),
        }
    }
}

impl std::error::Error for ServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ServerError::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for ServerError {
    fn from(error: io::Error) -> Self {
        ServerError::Io(error)
    }
}

/// A bound intent socket plus the state its connections read.
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
    shared: Arc<Mutex<SharedState>>,
}

impl Server {
    /// Bind `$XDG_RUNTIME_DIR/nah.sock`, refusing to disturb a live daemon.
    pub fn bind(shared: Arc<Mutex<SharedState>>) -> Result<Self, ServerError> {
        let path = crate::client::socket_path().map_err(|_| ServerError::RuntimeDirUnset)?;

        // Live-daemon guard: if something is accepting connections, it is
        // another instance and this one must not steal the socket.
        if UnixStream::connect(&path).is_ok() {
            return Err(ServerError::AlreadyRunning(path));
        }

        // No live listener: clear a stale socket (or refuse to clobber a
        // non-socket inode the connect failed on).
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(ServerError::Io(error)),
        }

        let listener = UnixListener::bind(&path)?;
        if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(0o600)) {
            let _ = fs::remove_file(&path);
            return Err(ServerError::Io(error));
        }

        Ok(Self {
            listener,
            path,
            shared,
        })
    }

    /// Path of the bound socket (used for the startup log and cleanup).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Accept connections forever, one thread per connection.
    pub fn run(self, sender: SenderHandle) {
        loop {
            match self.listener.accept() {
                Ok((stream, _peer)) => {
                    match peer_is_current_user(&stream) {
                        Ok(true) => {}
                        Ok(false) => {
                            eprintln!("nah: rejecting connection from a different uid");
                            continue;
                        }
                        Err(error) => {
                            eprintln!(
                                "nah: rejecting connection without peer credentials: {error}"
                            );
                            continue;
                        }
                    }

                    let shared = Arc::clone(&self.shared);
                    let sender = sender.clone();
                    let spawn = thread::Builder::new()
                        .name("nah-conn".to_string())
                        .spawn(move || handle_connection(stream, shared, sender));
                    if let Err(error) = spawn {
                        eprintln!("nah: failed to spawn connection thread: {error}");
                    }
                }
                Err(error) => {
                    eprintln!("nah: accept failed: {error}");
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
}

/// `SO_PEERCRED` check: is the peer the same uid as this daemon?
fn peer_is_current_user(stream: &UnixStream) -> io::Result<bool> {
    let credentials = rustix::net::sockopt::socket_peercred(stream)?;
    let euid = unsafe { libc::geteuid() };
    Ok(credentials.uid.as_raw() == euid)
}

/// Serve one client until it closes the connection or a write fails.
fn handle_connection(stream: UnixStream, shared: Arc<Mutex<SharedState>>, sender: SenderHandle) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;
    let mut line = Vec::with_capacity(128);

    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => return,
            Ok(_) => {}
            Err(_) => return,
        }
        trim_line(&mut line);

        let request = match std::str::from_utf8(&line) {
            Ok(request) => request,
            Err(_) => {
                if !write_line(&mut writer, "err request is not valid utf-8") {
                    return;
                }
                continue;
            }
        };
        if !reply(&mut writer, request, &shared, &sender) {
            return;
        }
    }
}

/// Answer one request line. Returns `false` when the client went away.
fn reply(
    writer: &mut UnixStream,
    request: &str,
    shared: &Mutex<SharedState>,
    sender: &SenderHandle,
) -> bool {
    if request == "status" {
        let payload = {
            let state = lock_shared(shared);
            status_payload(&state)
        };
        if !write_line(writer, "ok") {
            return false;
        }
        for line in payload {
            if !write_line(writer, &line) {
                return false;
            }
        }
        return write_line(writer, ".");
    }

    let response = match Intent::parse(request) {
        Ok(intent) => {
            let action: Action = {
                let state = lock_shared(shared);
                route(&intent, state.focused_axis())
            };
            match sender.try_send(action) {
                Ok(()) => "ok".to_string(),
                Err(EnqueueError::Busy) => "err busy".to_string(),
                Err(EnqueueError::Disconnected) => "err niri-disconnected".to_string(),
            }
        }
        Err(_) => format!("err unknown intent: {request}"),
    };
    write_line(writer, &response)
}

/// `status` payload lines, terminated by the caller with a lone `.`.
fn status_payload(state: &SharedState) -> Vec<String> {
    let tracker = state.tracker();
    let mut lines = Vec::with_capacity(3 + Intent::ALL.len());

    lines.push(format!(
        "focused-output: {}",
        tracker.focused_output().unwrap_or("none")
    ));
    lines.push(format!(
        "focused-window: {}",
        match tracker.focused_window_id() {
            Some(id) => id.to_string(),
            None => "none".to_string(),
        }
    ));

    let axis = state.focused_axis();
    lines.push(format!("axis: {}", axis.map_or("unknown", Axis::as_str)));
    for intent in Intent::ALL {
        let action = route(&intent, axis);
        lines.push(format!("action {}: {}", intent.as_str(), action.name()));
    }

    lines
}

fn write_line(writer: &mut UnixStream, line: &str) -> bool {
    writeln!(writer, "{line}").is_ok()
}

fn lock_shared(shared: &Mutex<SharedState>) -> MutexGuard<'_, SharedState> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

fn trim_line(line: &mut Vec<u8>) {
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Event, Workspace};
    use std::collections::HashMap;

    fn workspace(id: u64, output: Option<&str>, focused: bool) -> Workspace {
        Workspace {
            id,
            idx: 1,
            name: None,
            output: output.map(str::to_string),
            is_urgent: false,
            is_active: focused,
            is_focused: focused,
            active_window_id: None,
            is_hidden: false,
        }
    }

    fn seeded(output: Option<&str>) -> SharedState {
        let mut state = SharedState::new();
        state.tracker_mut().update(&Event::WorkspacesChanged {
            workspaces: vec![workspace(1, output, true)],
        });
        state
    }

    #[test]
    fn status_payload_reports_vertical_mapping() {
        let mut state = seeded(Some("DP-2"));
        state.set_output_axes(HashMap::from([("DP-2".to_string(), Axis::Vertical)]));

        let payload = status_payload(&state);
        assert!(payload.contains(&"focused-output: DP-2".to_string()));
        assert!(payload.contains(&"focused-window: none".to_string()));
        assert!(payload.contains(&"axis: vertical".to_string()));
        assert!(payload.contains(&"action move-left: MoveColumnLeft".to_string()));
        assert!(payload.contains(&"action move-right: MoveColumnRight".to_string()));
        assert!(payload.contains(&"action move-up: ConsumeOrExpelWindowLeft".to_string()));
        assert!(payload.contains(&"action move-down: ConsumeOrExpelWindowRight".to_string()));
    }

    #[test]
    fn status_payload_falls_back_to_horizontal_without_axis() {
        let state = seeded(Some("DP-2"));

        let payload = status_payload(&state);
        assert!(payload.contains(&"axis: unknown".to_string()));
        assert!(payload.contains(&"action move-left: ConsumeOrExpelWindowLeft".to_string()));
        assert!(payload.contains(&"action move-up: MoveWindowUp".to_string()));
    }

    #[test]
    fn status_payload_handles_headless_workspace() {
        let state = seeded(None);

        let payload = status_payload(&state);
        assert!(payload.contains(&"focused-output: none".to_string()));
        assert!(payload.contains(&"axis: unknown".to_string()));
        assert!(payload.contains(&"action move-left: ConsumeOrExpelWindowLeft".to_string()));
    }
}
