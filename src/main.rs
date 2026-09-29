//! `nah` — output-aware keybind router for niri/biri.
//!
//! `nah --daemon` follows the compositor event stream and tracks the focused
//! output (Phase 1); `nah --selftest` proves the dedicated action-sender
//! connection end to end (Phase 2). The router, socket server, and
//! per-keypress client arrive in later phases; the module boundaries already
//! reserve their slots.

mod client;

#[cfg(feature = "daemon")]
mod backoff;
#[cfg(feature = "daemon")]
mod daemon;
#[cfg(feature = "daemon")]
mod protocol;
#[cfg(feature = "daemon")]
mod router;
#[cfg(feature = "daemon")]
mod sender;
#[cfg(feature = "daemon")]
mod server;
#[cfg(feature = "daemon")]
mod state;

use std::env;
#[cfg(feature = "daemon")]
use std::io::{BufRead, BufReader, Write};
#[cfg(feature = "daemon")]
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

const USAGE: &str = "\
usage: nah --daemon
       nah --selftest
       nah status
       nah move-left | move-right | move-up | move-down

`nah --daemon` tracks the focused output and routes intents over
$XDG_RUNTIME_DIR/nah.sock; `nah <intent>` is the per-keypress client and
`nah status` prints the daemon's live state. `nah --selftest` proves the
action-sender connection end to end.";

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if args.next().is_some() {
        eprintln!("nah: unexpected extra arguments\n{USAGE}");
        return ExitCode::from(2);
    }

    match command.as_str() {
        "--daemon" => run_daemon(),
        "--selftest" => run_selftest(),
        "status" | "move-left" | "move-right" | "move-up" | "move-down" => client::run(&command),
        "--help" | "-h" => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
        other => {
            eprintln!("nah: unknown command: {other}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(feature = "daemon")]
fn run_daemon() -> ExitCode {
    match daemon::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("nah: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(feature = "daemon"))]
fn run_daemon() -> ExitCode {
    eprintln!("nah: --daemon was not built in (rebuild with the `daemon` feature)");
    ExitCode::from(2)
}

#[cfg(feature = "daemon")]
fn run_selftest() -> ExitCode {
    match selftest() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("nah: selftest failed: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(feature = "daemon"))]
fn run_selftest() -> ExitCode {
    eprintln!("nah: --selftest was not built in (rebuild with the `daemon` feature)");
    ExitCode::from(2)
}

/// Phase 2 end-to-end check: event-stream handshake, then two
/// `ToggleWindowFloating` actions over a dedicated sender connection.
///
/// The second toggle restores the focused window's previous floating state.
/// The selftest only reports success once both action replies are `Ok`.
#[cfg(feature = "daemon")]
fn selftest() -> Result<(), String> {
    use crate::protocol::{Action, Response};
    use crate::sender::Sender;

    // (a) Sanity-check the event-stream connection, then drop it: send
    // `"EventStream"` and read the one-line `{"Ok":"Handled"}` reply. The
    // connection accepts no further requests after this, which is why the
    // actions below use `Sender`'s second connection.
    let handshake_reply = event_stream_handshake()?;
    eprintln!("nah selftest: event stream handshake reply: {handshake_reply}");

    // (b) The action sender opens its own connection to the same socket.
    let mut sender = Sender::connect().map_err(|error| error.to_string())?;

    // (c, d) Toggle floating on and immediately off again.
    let mut failures = Vec::new();
    for attempt in 1..=2 {
        let action = Action::ToggleWindowFloating { id: None };
        let (reply, raw_line) = sender
            .send_action_with_reply_line(&action)
            .map_err(|error| error.to_string())?;
        eprintln!("nah selftest: ToggleWindowFloating #{attempt} reply: {raw_line}");
        match reply {
            Ok(Response::Handled) => {}
            Ok(other) => failures.push(format!("attempt {attempt}: unexpected reply {other:?}")),
            Err(message) => failures.push(format!("attempt {attempt}: {message}")),
        }
    }

    if failures.is_empty() {
        eprintln!("nah selftest: OK (floating state restored)");
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Send `"EventStream"`, read the single `{"Ok":"Handled"}` reply, and close
/// the connection. Returns the raw reply line for logging.
#[cfg(feature = "daemon")]
fn event_stream_handshake() -> Result<String, String> {
    use crate::protocol::{Reply, Request};

    let socket = env::var_os("NIRI_SOCKET")
        .ok_or_else(|| "NIRI_SOCKET is not set; is the compositor running?".to_string())?;
    let stream = UnixStream::connect(&socket)
        .map_err(|error| format!("connecting to {}: {error}", socket.to_string_lossy()))?;
    let mut writer = stream.try_clone().map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(stream);

    // `Request::EventStream` is a unit variant, so this is exactly
    // `"EventStream"\n` — never `{"EventStream":{}}`.
    let mut request = serde_json::to_string(&Request::EventStream)
        .map_err(|error| format!("encoding EventStream: {error}"))?;
    request.push('\n');
    writer
        .write_all(request.as_bytes())
        .map_err(|error| format!("writing EventStream: {error}"))?;
    writer
        .flush()
        .map_err(|error| format!("flushing EventStream: {error}"))?;

    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| format!("reading EventStream reply: {error}"))?;
    if line.is_empty() {
        return Err(
            "compositor closed the connection during the EventStream handshake".to_string(),
        );
    }
    let reply: Reply = serde_json::from_str(line.trim_end())
        .map_err(|error| format!("EventStream reply was not protocol JSON: {error}"))?;
    match reply {
        Ok(crate::protocol::Response::Handled) => Ok(line.trim_end().to_string()),
        Ok(other) => Err(format!("unexpected EventStream reply: {other:?}")),
        Err(message) => Err(format!("compositor rejected EventStream: {message}")),
    }
}
