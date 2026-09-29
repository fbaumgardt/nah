//! The dedicated action-sender connection.
//!
//! The compositor turns an event-stream connection read-only after
//! `"EventStream"`, so Actions must travel over a second connection. [`Sender`]
//! opens its own `UnixStream` to `$NIRI_SOCKET`, writes one newline-delimited
//! JSON request per call, flushes, and reads exactly one reply line.
//!
//! Phase 5 adds background reconnect: the sender thread notices a closed
//! connection even while idle, then retries on the shared exponential backoff
//! (200 ms → 5 s cap) until the compositor is reachable again. Intents that
//! arrive while the connection is down are refused and dropped — never queued
//! across reconnects.

use std::env;
use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::backoff::Backoff;
use crate::protocol::{Action, Reply, Request};

/// Why an action could not be sent or its reply could not be read.
#[derive(Debug)]
pub enum SenderError {
    /// `$NIRI_SOCKET` is unset.
    SocketUnset,
    /// An I/O error on the sender connection.
    Io(io::Error),
    /// The request could not be encoded or the reply was not protocol JSON.
    Protocol(String),
    /// The compositor closed the connection before sending a full reply line.
    ConnectionClosed,
}

impl fmt::Display for SenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SenderError::SocketUnset => {
                f.write_str("NIRI_SOCKET is not set; is the compositor running?")
            }
            SenderError::Io(error) => write!(f, "action sender I/O error: {error}"),
            SenderError::Protocol(message) => write!(f, "action sender protocol error: {message}"),
            SenderError::ConnectionClosed => {
                f.write_str("compositor closed the action connection without replying")
            }
        }
    }
}

impl std::error::Error for SenderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SenderError::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for SenderError {
    fn from(error: io::Error) -> Self {
        SenderError::Io(error)
    }
}

/// A persistent `$NIRI_SOCKET` connection used only to send Actions.
///
/// The compositor processes requests one by one per connection, so writing one
/// request and reading one reply per call cannot mispair them.
#[derive(Debug)]
pub struct Sender {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Sender {
    /// Connect to `$NIRI_SOCKET`, failing loudly when it is unset.
    pub fn connect() -> Result<Self, SenderError> {
        let socket = env::var_os("NIRI_SOCKET").ok_or(SenderError::SocketUnset)?;
        Self::connect_to(socket)
    }

    /// Connect to an explicit socket path.
    ///
    /// Tests point this at a mock compositor; later phases may reuse it when
    /// reconnecting to a cached path.
    pub fn connect_to(path: impl AsRef<Path>) -> Result<Self, SenderError> {
        let stream = UnixStream::connect(path)?;
        let writer = stream.try_clone()?;
        Ok(Self {
            reader: BufReader::new(stream),
            writer,
        })
    }

    /// Send one Action and return the parsed [`Reply`].
    #[allow(dead_code)] // Required sender API; the Phase 3 router is its first caller.
    pub fn send_action(&mut self, action: &Action) -> Result<Reply, SenderError> {
        self.send_action_with_reply_line(action)
            .map(|(reply, _)| reply)
    }

    /// Send one Action, returning the parsed [`Reply`] plus the exact reply
    /// line (trailing newline stripped) for verbatim logging.
    pub fn send_action_with_reply_line(
        &mut self,
        action: &Action,
    ) -> Result<(Reply, String), SenderError> {
        // `Request::Action` is the same externally-tagged envelope every niri
        // IPC client uses: `{"Action":{...}}`.
        let mut request = serde_json::to_string(&Request::Action(action.clone()))
            .map_err(|error| SenderError::Protocol(error.to_string()))?;
        request.push('\n');
        self.writer.write_all(request.as_bytes())?;
        self.writer.flush()?;

        let mut line = Vec::with_capacity(256);
        let read = self.reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            return Err(SenderError::ConnectionClosed);
        }
        trim_line(&mut line);
        let line = String::from_utf8(line)
            .map_err(|error| SenderError::Protocol(format!("reply was not UTF-8: {error}")))?;
        let reply = serde_json::from_str(&line)
            .map_err(|error| SenderError::Protocol(format!("bad reply: {error}")))?;
        Ok((reply, line))
    }

    /// Non-blocking check whether the compositor closed the connection.
    ///
    /// The action connection never carries unsolicited data, so the idle
    /// sender uses this probe to notice a peer close without waiting for the
    /// next intent. A pending byte would violate the request/reply contract
    /// and is treated as unusable too; either way the caller drops the cached
    /// connection and starts reconnecting.
    fn peer_closed(&self) -> bool {
        let fd = self.reader.get_ref().as_raw_fd();
        let mut probe = 0u8;
        // SAFETY: `fd` belongs to the live stream for the duration of the
        // call and `probe` is a valid one-byte buffer. `MSG_PEEK` consumes
        // nothing and `MSG_DONTWAIT` keeps the probe non-blocking.
        let result = unsafe {
            libc::recv(
                fd,
                &mut probe as *mut u8 as *mut libc::c_void,
                1,
                libc::MSG_DONTWAIT | libc::MSG_PEEK,
            )
        };
        if result == 0 {
            return true;
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            return error.kind() != io::ErrorKind::WouldBlock
                && error.kind() != io::ErrorKind::Interrupted;
        }
        true
    }
}

/// Strip the trailing `\n` (and a defensive `\r`) from one protocol line.
fn trim_line(line: &mut Vec<u8>) {
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
}

/// Capacity of the intent queue between the socket-server threads and the
/// sender thread. Deliberately tiny: intents are keypresses, and the contract
/// is to reject (`err busy`) rather than build a laggy backlog.
pub const SENDER_QUEUE_CAPACITY: usize = 16;

/// How often the idle sender probes the action connection for a peer close.
///
/// The compositor never sends unsolicited data on this connection, so a close
/// is noticed at the next probe instead of at the next intent. 50 ms keeps
/// `err niri-disconnected` prompt under key-repeat while costing one
/// non-blocking `recv` wake per interval.
const SENDER_LIVENESS_POLL: Duration = Duration::from_millis(50);

/// Why [`SenderHandle::try_send`] refused an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueError {
    /// The bounded queue is full; the intent is dropped and the client is
    /// told `err busy`.
    Busy,
    /// The action connection is down (or the sender thread is gone); the
    /// intent is dropped and the client is told `err niri-disconnected`.
    Disconnected,
}

/// Cloneable handle to the daemon's dedicated action-sender thread.
///
/// The thread owns the persistent `$NIRI_SOCKET` action connection. The socket
/// server enqueues actions with [`SenderHandle::try_send`], which never blocks:
/// a full queue, a down connection, or a dead sender thread is reported to the
/// caller instead. While disconnected, intents are refused and dropped — never
/// queued across reconnects — and the sender reconnects in the background on
/// the shared exponential backoff (200 ms initial, ×2, 5 s cap).
#[derive(Debug, Clone)]
pub struct SenderHandle {
    tx: SyncSender<Action>,
    connected: Arc<AtomicBool>,
}

impl SenderHandle {
    /// Start the sender thread. It connects once eagerly (logging either
    /// outcome) so the initial connected/disconnected state is known before
    /// the first intent can arrive, then serves the bounded queue until every
    /// handle is dropped.
    pub fn spawn() -> io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel(SENDER_QUEUE_CAPACITY);
        let initial = connect();
        if initial.is_some() {
            eprintln!("sender: connected to compositor");
        }
        let connected = Arc::new(AtomicBool::new(initial.is_some()));
        let thread_connected = Arc::clone(&connected);
        thread::Builder::new()
            .name("nah-sender".to_string())
            .spawn(move || sender_loop(initial, &thread_connected, &rx))?;
        Ok(Self { tx, connected })
    }

    /// Enqueue one action without blocking the caller.
    ///
    /// The connected flag is checked first so the caller can answer
    /// `err niri-disconnected` instead of pretending a dropped intent was
    /// accepted. (A connection can still die between this check and delivery;
    /// that intent is dropped too, per the documented policy.)
    pub fn try_send(&self, action: Action) -> Result<(), EnqueueError> {
        if !self.connected.load(Ordering::Acquire) {
            return Err(EnqueueError::Disconnected);
        }
        self.tx.try_send(action).map_err(|error| match error {
            TrySendError::Full(_) => EnqueueError::Busy,
            TrySendError::Disconnected(_) => EnqueueError::Disconnected,
        })
    }
}

/// Owner of the persistent action connection.
///
/// One loop with two states:
///
/// - **connected** — cached `Sender`; blocks on the bounded queue with a short
///   timeout so a peer close is noticed while idle;
/// - **disconnected** — no connection; sleeps out the exponential backoff
///   between background reconnect attempts and drops any racing intent.
fn sender_loop(mut connection: Option<Sender>, connected: &AtomicBool, rx: &Receiver<Action>) {
    let mut backoff = Backoff::new();
    loop {
        if connection.is_some() {
            match rx.recv_timeout(SENDER_LIVENESS_POLL) {
                Ok(action) => {
                    let result = {
                        let sender = connection.as_mut().expect("connection is Some");
                        send_one(sender, &action)
                    };
                    if let Err(error) = result {
                        // Drop the intent: nothing is queued across reconnects.
                        eprintln!("sender: disconnected: {error} (intent dropped)");
                        connection = None;
                        connected.store(false, Ordering::Release);
                        backoff.reset();
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    let closed = {
                        let sender = connection.as_ref().expect("connection is Some");
                        sender.peer_closed()
                    };
                    if closed {
                        eprintln!("sender: disconnected: compositor closed the action connection");
                        connection = None;
                        connected.store(false, Ordering::Release);
                        backoff.reset();
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            // While disconnected the flag is false, so `try_send` should
            // already have refused any client intent; a racing enqueue lands
            // here and is dropped rather than carried across the reconnect.
            match rx.recv_timeout(backoff.peek()) {
                Ok(_action) => {}
                Err(RecvTimeoutError::Timeout) => match connect() {
                    Some(sender) => {
                        connection = Some(sender);
                        connected.store(true, Ordering::Release);
                        backoff.reset();
                        eprintln!("sender: reconnected to compositor");
                    }
                    None => {
                        let _ = backoff.next_delay();
                    }
                },
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

/// Send one action. A compositor `Err` reply is logged but is not a connection
/// failure; only I/O errors mark the connection as down.
fn send_one(sender: &mut Sender, action: &Action) -> Result<(), SenderError> {
    match sender.send_action_with_reply_line(action) {
        Ok((Ok(_response), _raw)) => Ok(()),
        Ok((Err(message), raw)) => {
            eprintln!("sender: compositor rejected action ({raw}): {message}");
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Connect to `$NIRI_SOCKET`, logging failures. Successful call sites log
/// exactly one line themselves ("connected" on startup, "reconnected" after a
/// drop).
fn connect() -> Option<Sender> {
    match Sender::connect() {
        Ok(sender) => Some(sender),
        Err(error) => {
            eprintln!("sender: connect failed: {error}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_line_handles_lf_and_crlf() {
        let mut lf = b"{\"Ok\":\"Handled\"}\n".to_vec();
        trim_line(&mut lf);
        assert_eq!(lf, b"{\"Ok\":\"Handled\"}");

        let mut crlf = b"{\"Ok\":\"Handled\"}\r\n".to_vec();
        trim_line(&mut crlf);
        assert_eq!(crlf, b"{\"Ok\":\"Handled\"}");

        let mut none = b"{\"Ok\":\"Handled\"}".to_vec();
        trim_line(&mut none);
        assert_eq!(none, b"{\"Ok\":\"Handled\"}");
    }

    #[test]
    fn peer_closed_detects_the_compositor_closing_the_connection() {
        let (client, server) = UnixStream::pair().expect("socket pair");
        let writer = client.try_clone().expect("clone client end");
        let sender = Sender {
            reader: BufReader::new(client),
            writer,
        };

        assert!(!sender.peer_closed(), "open peer must look alive");
        drop(server);
        assert!(sender.peer_closed(), "closed peer must be detected");
    }

    #[test]
    fn queue_capacity_is_small_and_bounded() {
        assert_eq!(SENDER_QUEUE_CAPACITY, 16);
    }

    #[test]
    fn try_send_reports_busy_when_the_queue_is_full() {
        use std::os::unix::net::UnixListener;
        use std::time::{SystemTime, UNIX_EPOCH};

        // A fake compositor that accepts the action connection but never
        // replies parks the sender thread on its first action, so the bounded
        // queue backs up and `try_send` must report `Busy` instead of
        // blocking. (A UNIX `connect` succeeds via the listen backlog even
        // though `accept` is never called.)
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before the UNIX epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "nah-sender-test-{}-{nanos}.sock",
            std::process::id()
        ));
        let _listener = UnixListener::bind(&path).expect("bind fake compositor");
        std::env::set_var("NIRI_SOCKET", &path);

        let handle = SenderHandle::spawn().expect("spawn sender thread");
        let action = Action::ToggleWindowFloating { id: None };
        let mut busy = 0;
        for _ in 0..SENDER_QUEUE_CAPACITY + 4 {
            if handle.try_send(action.clone()) == Err(EnqueueError::Busy) {
                busy += 1;
            }
        }
        assert!(busy > 0, "queue never reported Busy");

        drop(handle);
        let _ = std::fs::remove_file(&path);
    }
}
