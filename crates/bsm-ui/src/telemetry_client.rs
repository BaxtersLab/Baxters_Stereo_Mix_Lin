use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::sync::mpsc::{self, Receiver};
use std::thread;

/// Spawn a blocking telemetry listener thread that connects to a TCP
/// telemetry endpoint and forwards newline-delimited JSON strings into a
/// `std::sync::mpsc::Receiver<String>` returned to the caller.
///
/// The function will retry connection on failure with a 1s backoff.
/// The telemetry endpoint to connect to, or `None` when telemetry is not
/// configured. Telemetry is **opt-in**.
///
/// Until 2026-08-25 the UI fell back to `127.0.0.1:9000` whenever
/// `BSM_TELEMETRY_ADDR` was unset. That default was a permanent no-op with a
/// cost: the only thing in this workspace that ever bound 9000 was a headless
/// daemon entry point that was never compiled (the root `Cargo.toml` is
/// `[workspace]`-only), and it has since been removed. `agent_server` binds
/// 4000, a different port and protocol. So every run spawned a thread that
/// redialled a dead port every 1.25 s for the life of the process.
///
/// Two further gaps mean restoring a default would not help. The wire protocol
/// requires the client to send `SubscribeTelemetry` before the server streams
/// anything, and [`spawn_telemetry_thread`] never writes -- so a live server
/// would leave both sides waiting on each other. And nothing in the recording
/// path emits telemetry: every `emit_*` call site lives in `bsm-ipc`'s own
/// dispatcher, in `bsm-hrt`, or in tests.
///
/// Takes the value as an argument rather than reading the environment, so it is
/// testable without a test thread mutating process-wide state.
pub fn resolve_addr(env_value: Option<String>) -> Option<String> {
    let value = env_value?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// [`resolve_addr`] applied to `BSM_TELEMETRY_ADDR` in the process environment.
pub fn configured_addr() -> Option<String> {
    resolve_addr(std::env::var("BSM_TELEMETRY_ADDR").ok())
}

pub fn spawn_telemetry_thread(addr: &str) -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    let addr = addr.to_string();
    thread::spawn(move || {
        loop {
            match TcpStream::connect(&addr) {
                Ok(stream) => {
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    loop {
                        line.clear();
                        match reader.read_line(&mut line) {
                            Ok(0) => break, // remote closed
                            Ok(_) => {
                                let s = line.trim_end().to_string();
                                if s.is_empty() { continue; }
                                if tx.send(s).is_err() { return; }
                            }
                            Err(_) => break,
                        }
                    }
                }
                Err(_) => {
                    // connect failed — retry after a short delay
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
            // short backoff before reconnect attempt
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::resolve_addr;

    /// The regression test for the defect fixed on 2026-08-25: with the
    /// variable unset there must be NO address, and therefore no connection
    /// attempt. Fails if anyone reintroduces a hardcoded default here.
    #[test]
    fn unset_means_no_telemetry_connection() {
        assert_eq!(resolve_addr(None), None);
    }

    /// An exported-but-empty variable is the same as unset. Shells make this
    /// easy to do by accident (`export BSM_TELEMETRY_ADDR=`), and dialling ""
    /// would just fail forever.
    #[test]
    fn empty_or_whitespace_means_no_telemetry_connection() {
        assert_eq!(resolve_addr(Some(String::new())), None);
        assert_eq!(resolve_addr(Some("   ".into())), None);
        assert_eq!(resolve_addr(Some("\t\n".into())), None);
    }

    #[test]
    fn a_real_address_is_used_and_trimmed() {
        assert_eq!(
            resolve_addr(Some("127.0.0.1:9000".into())),
            Some("127.0.0.1:9000".to_string())
        );
        assert_eq!(
            resolve_addr(Some("  10.0.0.5:4000\n".into())),
            Some("10.0.0.5:4000".to_string())
        );
    }
}
