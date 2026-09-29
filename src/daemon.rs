//! The long-lived daemon: event-stream consumer, intent socket, action sender.
//!
//! Thread layout:
//!
//! - **main** — supervisor: waits for the event thread's result while checking
//!   the SIGTERM/SIGINT flag, then unlinks the intent socket and exits.
//! - **`nah-events`** — owns the event-stream connection, updates
//!   [`SharedState`], and seeds/refreshes the output → axis map from a
//!   short-lived third `"Outputs"` connection. If the stream drops it retries
//!   the same `$NIRI_SOCKET` path with exponential backoff for up to 30 s and
//!   then exits cleanly (compositor gone).
//! - **`nah-server`** — accepts intent connections (one `nah-conn` thread each).
//! - **`nah-sender`** — owns the persistent action connection (see
//!   [`crate::sender`]).

use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::backoff::Backoff;
use crate::protocol::{Event, Reply, Request, Response};
use crate::sender::SenderHandle;
use crate::server::{Server, ServerError};
use crate::state::{transform_to_axis, Axis, SharedState};

/// How often the supervisor checks the shutdown flag while the event thread
/// is running. Bounds SIGTERM latency to one poll interval.
const SHUTDOWN_POLL: Duration = Duration::from_millis(100);

/// How long the event connection may stay down before the daemon gives up.
///
/// The compositor's socket path is instance-specific and `$NIRI_SOCKET` is
/// captured at startup. Under `spawn-at-startup`, a restarted compositor
/// starts a *fresh* daemon with the fresh path; a surviving daemon can only
/// retry the old path and cannot even discover that a new compositor instance
/// now owns a different one. Dying after a bounded retry is therefore the only
/// correct end, and the fresh daemon re-seeds state from the new full-state
/// dump.
const DEFAULT_EVENT_RECONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Test-only override for [`DEFAULT_EVENT_RECONNECT_TIMEOUT`], in milliseconds.
/// Integration tests cannot wait 30 s to observe the clean compositor-gone
/// exit; production runs leave it unset.
const EVENT_RECONNECT_TIMEOUT_ENV: &str = "NAH_EVENT_RECONNECT_TIMEOUT_MS";

/// Set by the SIGTERM/SIGINT handler; read by the supervisor.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Why the daemon stopped.
#[derive(Debug)]
pub enum DaemonError {
    /// `$NIRI_SOCKET` is unset.
    SocketUnset,
    /// An I/O error on the event-stream connection.
    Io(io::Error),
    /// A malformed or unexpected protocol message.
    Protocol(String),
    /// The compositor closed the event stream.
    StreamEnded,
    /// The compositor answered `EventStream` with an error.
    Rejected(String),
    /// The intent socket could not be set up.
    Server(ServerError),
    /// A background thread could not be started.
    ThreadSpawn(io::Error),
    /// The event connection stayed down past the reconnect deadline.
    ///
    /// This is the *clean* compositor-gone exit: [`run`] maps it to
    /// `Ok(())` — after letting [`SocketGuard`] unlink the daemon socket —
    /// so the process exits 0 instead of reporting a failure.
    CompositorGone,
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DaemonError::SocketUnset => {
                f.write_str("NIRI_SOCKET is not set; is the compositor running?")
            }
            DaemonError::Io(error) => write!(f, "event stream I/O error: {error}"),
            DaemonError::Protocol(message) => write!(f, "event stream protocol error: {message}"),
            DaemonError::StreamEnded => f.write_str("event stream ended unexpectedly"),
            DaemonError::Rejected(message) => {
                write!(f, "compositor rejected EventStream: {message}")
            }
            DaemonError::Server(error) => write!(f, "{error}"),
            DaemonError::ThreadSpawn(error) => write!(f, "failed to spawn daemon thread: {error}"),
            DaemonError::CompositorGone => {
                f.write_str("compositor connection lost and did not come back")
            }
        }
    }
}

impl std::error::Error for DaemonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DaemonError::Io(error) | DaemonError::ThreadSpawn(error) => Some(error),
            DaemonError::Server(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for DaemonError {
    fn from(error: io::Error) -> Self {
        DaemonError::Io(error)
    }
}

impl From<ServerError> for DaemonError {
    fn from(error: ServerError) -> Self {
        DaemonError::Server(error)
    }
}

/// Connect, subscribe, and pump events until the compositor goes away or the
/// process is signalled.
///
/// A dropped event stream is retried on the backoff schedule; if it stays down
/// past the deadline the daemon exits cleanly (status 0) because a restarted
/// compositor spawns its own fresh daemon with the new `$NIRI_SOCKET`.
pub fn run() -> Result<(), DaemonError> {
    let niri_socket = env::var_os("NIRI_SOCKET").ok_or(DaemonError::SocketUnset)?;

    // Bind the intent socket first: this is also the live-daemon guard, and
    // doing it before touching the compositor keeps `nah status`'s failure
    // mode simple.
    let shared = Arc::new(Mutex::new(SharedState::new()));
    let server = Server::bind(Arc::clone(&shared))?;
    let _socket_guard = SocketGuard(server.path().to_path_buf());
    eprintln!("nah: listening on {}", server.path().display());

    let sender = SenderHandle::spawn().map_err(DaemonError::ThreadSpawn)?;
    install_signal_handlers();

    let (done_tx, done_rx) = mpsc::channel();
    {
        let niri_socket = niri_socket.clone();
        let shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("nah-events".to_string())
            .spawn(move || {
                let result = event_loop(&niri_socket, &shared);
                let _ = done_tx.send(result);
            })
            .map_err(DaemonError::ThreadSpawn)?;
    }
    thread::Builder::new()
        .name("nah-server".to_string())
        .spawn(move || server.run(sender))
        .map_err(DaemonError::ThreadSpawn)?;

    loop {
        if shutdown_requested() {
            eprintln!("nah: shutting down");
            return Ok(());
        }
        match done_rx.recv_timeout(SHUTDOWN_POLL) {
            // Compositor gone: the socket guard below unlinks the intent
            // socket on the way out and main() turns this into exit 0.
            Ok(Err(DaemonError::CompositorGone)) => return Ok(()),
            Ok(result) => return result,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                return Err(DaemonError::Protocol(
                    "event thread exited without reporting a result".to_string(),
                ));
            }
        }
    }
}

/// Unlinks the intent socket on every exit path, including early errors.
struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Keep the event stream up: connect, pump events, and on any stream failure
/// retry the same `$NIRI_SOCKET` path with exponential backoff until the
/// compositor-gone deadline expires, then return [`DaemonError::CompositorGone`]
/// for the supervisor's clean exit.
///
/// The *initial* connect is not retried: the daemon is started by the
/// compositor (`spawn-at-startup`) or after it (systemd), so a failure here is
/// a configuration error to surface, not a transient drop.
fn event_loop(niri_socket: &OsString, shared: &Arc<Mutex<SharedState>>) -> Result<(), DaemonError> {
    let reconnect_timeout = event_reconnect_timeout();
    let mut reader = connect_event_stream(niri_socket)?;

    // `None` = no determination yet; `Some(None)` = determined to be no output.
    let mut last_output: Option<Option<String>> = None;
    // Reused across iterations; events arrive only a few per second, but the
    // 64-event server backlog makes prompt reads worth keeping cheap.
    let mut line = Vec::with_capacity(8192);

    loop {
        let failure = match pump_events(
            &mut reader,
            niri_socket,
            shared,
            &mut last_output,
            &mut line,
        ) {
            Ok(()) => DaemonError::StreamEnded, // unreachable; keeps the loop total
            Err(error) => error,
        };
        eprintln!("nah: event stream lost: {failure}");
        reader = reconnect_event_stream(niri_socket, reconnect_timeout)?;
    }
}

/// Connect to the event stream, subscribe, and read the handshake reply.
fn connect_event_stream(niri_socket: &OsString) -> Result<BufReader<UnixStream>, DaemonError> {
    let stream = UnixStream::connect(niri_socket)?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    // Unit variants of `Request` serialize as bare strings, so this is
    // literally `"EventStream"\n` — never `{"EventStream":{}}`.
    let mut request = serde_json::to_string(&Request::EventStream)
        .map_err(|error| DaemonError::Protocol(error.to_string()))?;
    request.push('\n');
    writer.write_all(request.as_bytes())?;
    writer.flush()?;

    let reply = read_reply(&mut reader)?;
    match reply {
        Ok(Response::Handled) => {}
        Ok(other) => {
            return Err(DaemonError::Protocol(format!(
                "unexpected reply to EventStream: {other:?}"
            )));
        }
        Err(message) => return Err(DaemonError::Rejected(message)),
    }

    // `writer` is a dup of the same socket description; dropping it leaves
    // `reader` open. The event stream sends only, so no write half is needed
    // after the handshake.
    Ok(reader)
}

/// Retry `niri_socket` with the shared 200 ms → 5 s backoff for at most
/// `timeout` of continuous failure, then give up with
/// [`DaemonError::CompositorGone`].
///
/// The path is the one captured at startup and is deliberately never re-read
/// from the environment: the compositor's socket path is instance-specific, so
/// a surviving daemon has no way to learn a new instance's path. Under
/// `spawn-at-startup`, the fresh compositor starts a fresh daemon instead, and
/// that daemon gets the fresh full-state dump; the old daemon exiting cleanly
/// (supervisor unlinks the intent socket, exit 0) is correct.
fn reconnect_event_stream(
    niri_socket: &OsString,
    timeout: Duration,
) -> Result<BufReader<UnixStream>, DaemonError> {
    let started = Instant::now();
    let mut backoff = Backoff::new();

    loop {
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            eprintln!(
                "nah: compositor unreachable for {}s; exiting",
                timeout.as_secs_f64()
            );
            return Err(DaemonError::CompositorGone);
        }

        thread::sleep(backoff.next_delay().min(remaining));
        match connect_event_stream(niri_socket) {
            Ok(reader) => {
                eprintln!("nah: reconnected to compositor");
                return Ok(reader);
            }
            Err(error) => eprintln!("nah: event stream reconnect failed: {error}"),
        }
    }
}

/// Apply events to the shared state until the stream fails. Never returns
/// `Ok`: every exit is an I/O error or [`DaemonError::StreamEnded`].
fn pump_events(
    reader: &mut impl BufRead,
    niri_socket: &OsString,
    shared: &Arc<Mutex<SharedState>>,
    last_output: &mut Option<Option<String>>,
    line: &mut Vec<u8>,
) -> Result<(), DaemonError> {
    loop {
        line.clear();
        let read = reader.read_until(b'\n', line)?;
        if read == 0 {
            return Err(DaemonError::StreamEnded);
        }
        trim_line(line);

        let event = match serde_json::from_slice(line) {
            Ok(event) => event,
            Err(error) => {
                // Unknown *variants* arrive as `Event::Unknown`; a parse error
                // here means genuinely malformed JSON. Keep the stream alive.
                eprintln!("nah: ignoring unparsable event line: {error}");
                continue;
            }
        };

        let refresh_outputs = {
            let mut state = lock_shared(shared);
            state.tracker_mut().update(&event);
            maybe_log_focus(&state, last_output);

            // Transforms can change, but the event stream does not report
            // output changes: refresh the map whenever a *new* output name
            // shows up in `WorkspacesChanged`. Never poll otherwise.
            match &event {
                Event::WorkspacesChanged { workspaces } => workspaces
                    .iter()
                    .filter_map(|workspace| workspace.output.as_deref())
                    .any(|name| !state.contains_output(name)),
                _ => false,
            }
        };

        // The lock is released: the query below performs network I/O.
        if refresh_outputs {
            refresh_output_axes(niri_socket, shared);
        }
    }
}

/// The compositor-gone deadline, with a test-only millisecond override.
fn event_reconnect_timeout() -> Duration {
    env::var(EVENT_RECONNECT_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_EVENT_RECONNECT_TIMEOUT)
}

/// Log one line whenever the focused output changes.
fn maybe_log_focus(state: &SharedState, last_output: &mut Option<Option<String>>) {
    let current = state.tracker().focused_output().map(str::to_owned);
    if last_output.as_ref() != Some(&current) {
        match &current {
            Some(name) => {
                let workspace = state
                    .tracker()
                    .focused_workspace_id()
                    .map_or_else(|| "?".to_string(), |id| id.to_string());
                eprintln!("focused output: {name} (workspace {workspace})");
            }
            None => eprintln!("focused output: (none)"),
        }
        *last_output = Some(current);
    }
}

/// Query `"Outputs"` on a short-lived connection and replace the axis map.
fn refresh_output_axes(niri_socket: &OsString, shared: &Arc<Mutex<SharedState>>) {
    match fetch_output_axes(niri_socket) {
        Ok(axes) => {
            let mut state = lock_shared(shared);
            state.set_output_axes(axes);
            let summary = state
                .output_axis_pairs()
                .iter()
                .map(|(name, axis)| format!("{name}={}", axis.as_str()))
                .collect::<Vec<_>>()
                .join(", ");
            eprintln!("output map updated: {summary}");
        }
        Err(error) => eprintln!("nah: output map refresh failed: {error}"),
    }
}

/// Fetch output → main-axis from one `"Outputs"` request/response on a fresh
/// connection. Outputs without a logical geometry (disabled) are skipped.
fn fetch_output_axes(niri_socket: &OsString) -> Result<HashMap<String, Axis>, String> {
    let stream =
        UnixStream::connect(niri_socket).map_err(|error| format!("connect failed: {error}"))?;
    let mut writer = stream
        .try_clone()
        .map_err(|error| format!("clone failed: {error}"))?;
    let mut reader = BufReader::new(stream);

    let mut request = serde_json::to_string(&Request::Outputs)
        .map_err(|error| format!("encode failed: {error}"))?;
    request.push('\n');
    writer
        .write_all(request.as_bytes())
        .map_err(|error| format!("write failed: {error}"))?;
    writer
        .flush()
        .map_err(|error| format!("flush failed: {error}"))?;

    let mut line = Vec::with_capacity(4096);
    let read = reader
        .read_until(b'\n', &mut line)
        .map_err(|error| format!("read failed: {error}"))?;
    if read == 0 {
        return Err("compositor closed the Outputs connection".to_string());
    }
    trim_line(&mut line);

    let reply: Reply =
        serde_json::from_slice(&line).map_err(|error| format!("bad Outputs reply: {error}"))?;
    match reply {
        Ok(Response::Outputs(outputs)) => Ok(outputs
            .into_iter()
            .filter_map(|(name, output)| {
                let logical = output.logical?;
                Some((name, transform_to_axis(logical.transform)))
            })
            .collect()),
        Ok(other) => Err(format!("unexpected Outputs reply: {other:?}")),
        Err(message) => Err(format!("compositor rejected Outputs: {message}")),
    }
}

fn read_reply(reader: &mut impl BufRead) -> Result<Reply, DaemonError> {
    let mut line = Vec::with_capacity(256);
    let read = reader.read_until(b'\n', &mut line)?;
    if read == 0 {
        return Err(DaemonError::StreamEnded);
    }
    trim_line(&mut line);
    serde_json::from_slice(&line)
        .map_err(|error| DaemonError::Protocol(format!("bad reply to EventStream: {error}")))
}

/// SIGTERM/SIGINT handler: only an atomic store, which is async-signal-safe.
extern "C" fn handle_shutdown_signal(_signal: libc::c_int) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

fn install_signal_handlers() {
    // SAFETY: the handler only performs a lock-free atomic store, which is
    // async-signal-safe. The previous handler returned by `signal` is ignored.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            handle_shutdown_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            handle_shutdown_signal as *const () as libc::sighandler_t,
        );
    }
}

fn shutdown_requested() -> bool {
    SHUTDOWN.load(Ordering::SeqCst)
}

fn lock_shared(shared: &Mutex<SharedState>) -> std::sync::MutexGuard<'_, SharedState> {
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
