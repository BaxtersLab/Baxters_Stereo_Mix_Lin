use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::sync::mpsc::{self, Receiver};
use std::thread;

/// Spawn a blocking telemetry listener thread that connects to a TCP
/// telemetry endpoint and forwards newline-delimited JSON strings into a
/// `std::sync::mpsc::Receiver<String>` returned to the caller.
///
/// The function will retry connection on failure with a 1s backoff.
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
