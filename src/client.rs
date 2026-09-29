//! The default (client) mode of `nah`.
//!
//! This module must stay std-only: the client is spawned once per keypress and
//! therefore ships without serde or any other dependency. It connects to
//! `$XDG_RUNTIME_DIR/nah.sock`, writes one intent line, and waits at most
//! [`REPLY_TIMEOUT`] for the daemon's reply. It never panics: every failure is
//! a one-line stderr message and a non-zero exit code.

use std::env;
use std::fmt::Write as _;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

/// How long the client waits for each daemon reply line.
pub const REPLY_TIMEOUT: Duration = Duration::from_millis(100);

/// The daemon socket path: `$XDG_RUNTIME_DIR/nah.sock`.
///
/// Shared with the daemon's [`crate::server`]; the daemon fails loudly when it
/// is `Err`.
pub fn socket_path() -> Result<PathBuf, &'static str> {
    match env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => Ok(PathBuf::from(dir).join("nah.sock")),
        _ => Err("XDG_RUNTIME_DIR is not set"),
    }
}

/// Run one client command (`status` or a `move-*` intent).
pub fn run(command: &str) -> ExitCode {
    match request(command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("nah: {message}");
            ExitCode::FAILURE
        }
    }
}

fn request(command: &str) -> Result<(), String> {
    let path = socket_path().map_err(str::to_string)?;
    let stream = UnixStream::connect(&path).map_err(|_| "daemon not running".to_string())?;
    stream
        .set_read_timeout(Some(REPLY_TIMEOUT))
        .map_err(|_| "daemon not running".to_string())?;

    let mut writer = stream
        .try_clone()
        .map_err(|_| "daemon not running".to_string())?;
    let mut reader = BufReader::new(stream);

    let mut request = String::with_capacity(command.len() + 1);
    let _ = writeln!(request, "{command}");
    writer
        .write_all(request.as_bytes())
        .map_err(|_| "daemon not running".to_string())?;
    writer
        .flush()
        .map_err(|_| "daemon not running".to_string())?;

    let first = read_line(&mut reader)?;
    if first == "ok" {
        if command == "status" {
            return print_payload(&mut reader);
        }
        return Ok(());
    }
    if let Some(reason) = first.strip_prefix("err ") {
        return Err(reason.to_string());
    }
    Err("unexpected reply from daemon".to_string())
}

/// Print the `key: value` payload lines up to the lone `.` terminator.
fn print_payload(reader: &mut impl BufRead) -> Result<(), String> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    loop {
        let line = read_line(reader)?;
        if line == "." {
            return Ok(());
        }
        writeln!(stdout, "{line}").map_err(|_| "cannot write to stdout".to_string())?;
    }
}

/// Read one line, mapping timeout and EOF to the client's exit messages.
fn read_line(reader: &mut impl BufRead) -> Result<String, String> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => Err("daemon not running".to_string()),
        Ok(_) => Ok(trim_newline(line)),
        Err(error) if is_timeout(&error) => Err("timeout".to_string()),
        Err(_) => Err("daemon not running".to_string()),
    }
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn trim_newline(mut line: String) -> String {
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_newline_handles_lf_crlf_and_plain() {
        assert_eq!(trim_newline("ok\n".to_string()), "ok");
        assert_eq!(trim_newline("ok\r\n".to_string()), "ok");
        assert_eq!(trim_newline("ok".to_string()), "ok");
        assert_eq!(trim_newline(String::new()), "");
    }

    #[test]
    fn timeout_error_kinds_are_recognized() {
        assert!(is_timeout(&io::Error::from(io::ErrorKind::WouldBlock)));
        assert!(is_timeout(&io::Error::from(io::ErrorKind::TimedOut)));
        assert!(!is_timeout(&io::Error::from(io::ErrorKind::NotFound)));
    }
}
