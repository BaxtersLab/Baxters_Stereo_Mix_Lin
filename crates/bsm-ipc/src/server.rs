#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use crate::commands::{parse_command, IpcCommand};
use crate::dispatcher::Dispatcher;
use crate::telemetry::TelemetryEmitter;
use std::error::Error;

#[cfg(windows)]
mod windows_pipe {
    use std::ffi::OsStr;
    use std::io::{BufRead, BufReader, Write};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{FromRawHandle, AsRawHandle};
    use std::ptr;
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    use winapi::um::winbase::{PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE, PIPE_READMODE_BYTE, PIPE_WAIT, PIPE_UNLIMITED_INSTANCES};
    use winapi::um::namedpipeapi::{CreateNamedPipeW, ConnectNamedPipe, DisconnectNamedPipe};
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::winnt::{HANDLE, GENERIC_READ, GENERIC_WRITE};
    use winapi::um::fileapi::{CreateFileW, OPEN_EXISTING};
    use std::fs::File;

    fn to_wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
    }

    // Spawn acceptor; `shutdown` is an Arc<AtomicBool> that will be set by the
    // caller to request the acceptor exit. The acceptor uses a local Tokio
    // runtime to call `spawn_blocking` for connection handlers.
    pub fn spawn_named_pipe_acceptor(pipe_name: &str, telemetry: Arc<crate::telemetry::TelemetryEmitter>, shutdown: Arc<AtomicBool>) {
        // Construct full pipe path
        let full = if pipe_name.starts_with("\\\\.\\pipe\\") {
            pipe_name.to_string()
        } else {
            format!("\\\\.\\pipe\\{}", pipe_name)
        };
        let wide = to_wide(&full);
        // Create a local Tokio runtime so we can use spawn_blocking for handlers
        let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(%e, "failed to create runtime for pipe acceptor");
                return;
            }
        };

        loop {
            if shutdown.load(Ordering::SeqCst) {
                tracing::info!("named-pipe acceptor shutdown requested");
                break;
            }

            unsafe {
                let handle: HANDLE = CreateNamedPipeW(
                    wide.as_ptr(),
                    PIPE_ACCESS_DUPLEX,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                    PIPE_UNLIMITED_INSTANCES,
                    65536,
                    65536,
                    0,
                    ptr::null_mut(),
                );

                let invalid_handle: HANDLE = (-1isize) as HANDLE;
                if handle == invalid_handle {
                    // Sleep briefly then retry
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    continue;
                }

                // Block until client connects
                let res = ConnectNamedPipe(handle, ptr::null_mut());
                if res == 0 {
                    // error, clean up and continue
                    CloseHandle(handle);
                    continue;
                }

                // Wrap handle in File and process connection using tokio's blocking pool
                let file = File::from_raw_handle(handle as _);
                let telemetry_clone = telemetry.clone();
                let shutdown_clone = shutdown.clone();
                let _ = rt.spawn_blocking(move || {
                    let mut reader = BufReader::new(&file);
                    let mut line = String::new();
                    let mut writer = &file;
                    let mut dispatcher = crate::dispatcher::Dispatcher::new(telemetry_clone.as_ref().clone());

                    loop {
                        if shutdown_clone.load(Ordering::SeqCst) {
                                    // Ask dispatcher to shutdown background tasks (bounded) and flush response
                                    let resp = dispatcher.handle_command(crate::commands::IpcCommand::Shutdown, "shutdown");
                                    // Wait up to 5s for background tasks to finish
                                    let _ = dispatcher.shutdown_blocking_with_timeout(Some(std::time::Duration::from_secs(5)));
                                    if let Ok(s) = resp.to_json() {
                                        let _ = writeln!(writer, "{}", s);
                                    }
                                    let _ = writer.flush();
                                    break;
                        }
                        line.clear();
                        match reader.read_line(&mut line) {
                            Ok(0) => break, // EOF
                            Ok(_) => {
                                let raw = line.trim_end().to_string();
                                if raw.is_empty() { continue; }
                                if shutdown_clone.load(Ordering::SeqCst) {
                                    let resp = dispatcher.handle_command(crate::commands::IpcCommand::Shutdown, "shutdown");
                                    let _ = dispatcher.shutdown_blocking_with_timeout(Some(std::time::Duration::from_secs(5)));
                                    if let Ok(s) = resp.to_json() {
                                        let _ = writeln!(writer, "{}", s);
                                    }
                                    let _ = writer.flush();
                                    break;
                                }
                                match crate::commands::parse_command(&raw) {
                                    Ok((cmd, id)) => {
                                        match cmd {
                                            crate::commands::IpcCommand::SubscribeTelemetry => {
                                                // Acknowledge subscribe
                                                let resp = crate::commands::ResponseEnvelope::ok(id.clone());
                                                if let Ok(s) = resp.to_json() { let _ = writeln!(writer, "{}", s); }

                                                // Start streaming telemetry to this blocking client using a polling receiver
                                                let mut rx = telemetry_clone.as_ref().subscribe();
                                                loop {
                                                    match rx.try_recv() {
                                                        Ok(msg) => { let _ = writeln!(writer, "{}", msg); let _ = writer.flush(); }
                                                        Err(tokio::sync::broadcast::error::TryRecvError::Empty) => { std::thread::sleep(std::time::Duration::from_millis(50)); }
                                                        Err(_) => break,
                                                    }
                                                    if shutdown_clone.load(Ordering::SeqCst) { break; }
                                                }
                                                // After streaming, break out and close connection
                                                break;
                                            }
                                            other => {
                                                let resp = dispatcher.handle_command(other, id.clone());
                                                if let Ok(s) = resp.to_json() { let _ = writeln!(writer, "{}", s); }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        let err = serde_json::json!({ "id": null, "ok": false, "error": format!("{}", e) });
                                        let s = serde_json::to_string(&err).unwrap_or_else(|_| "{}".into());
                                        let _ = writeln!(writer, "{}", s);
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }

                    // Disconnect and close handle when done
                    let _ = DisconnectNamedPipe(file.as_raw_handle() as HANDLE);
                    // File will be closed on drop
                });
            }
        }
    }

    // Wake the acceptor by briefly opening the pipe as a client. Used to
    // unblock ConnectNamedPipe during shutdown.
    pub fn wake_acceptor(pipe_name: &str) {
        let full = if pipe_name.starts_with("\\\\.\\pipe\\") {
            pipe_name.to_string()
        } else {
            format!("\\\\.\\pipe\\{}", pipe_name)
        };
        let wide = to_wide(&full);
        unsafe {
            let handle = CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null_mut(),
                OPEN_EXISTING,
                0,
                ptr::null_mut(),
            );
            if handle as isize != -1 {
                let _ = CloseHandle(handle);
            }
        }
    }
}

/// Run a simple line-based TCP IPC server that accepts newline-terminated
/// JSON command envelopes and replies with newline-terminated JSON responses.
pub async fn run_server(addr: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
    // Telemetry channel shared by dispatcher instances (single dispatcher per server)
    let (tx, _rx) = broadcast::channel::<String>(32);
    let telemetry = TelemetryEmitter::new(tx.clone());
    let shutdown = Arc::new(AtomicBool::new(false));

    // Central ctrl-c handler: set shutdown flag and wake Windows acceptor if present
    {
        let shutdown = shutdown.clone();
        // `addr` is only used by the Windows named-pipe acceptor wake-up below.
        #[cfg_attr(not(windows), allow(unused_variables))]
        let addr = addr.to_string();
        tokio::spawn(async move {
            if let Err(e) = tokio::signal::ctrl_c().await {
                tracing::warn!(%e, "failed to listen for ctrl_c");
            }
            tracing::info!("shutdown requested (ctrl-c)");
            shutdown.store(true, Ordering::SeqCst);
            #[cfg(windows)]
            {
                windows_pipe::wake_acceptor(&addr);
            }
        });
    }

    #[cfg(unix)]
    {
        let listener = UnixListener::bind(addr)?;
        tracing::info!("bsm-ipc: listening (unix socket) {addr}");
        loop {
            if shutdown.load(Ordering::SeqCst) { break; }
            let (socket, _) = listener.accept().await?;
            let telemetry = telemetry.clone();
            let shutdown_clone = shutdown.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_connection_unix(socket, telemetry, shutdown_clone).await {
                    tracing::warn!(%e, "client handler error");
                }
            });
        }
        Ok(())
    }

    #[cfg(windows)]
    {
        // On Windows use a blocking Named Pipe acceptor thread and hand each
        // connected pipe to a blocking handler thread. This avoids needing
        // tokio 0.1 bindings here and keeps integration simple.
        use std::thread;

        let pipe_name = addr.to_string();
        let telemetry = Arc::new(telemetry);
        let shutdown_clone = shutdown.clone();
        let pipe_name_clone = pipe_name.clone();
        let telemetry_clone_for_thread = telemetry.clone();
        let handle = thread::spawn(move || {
            windows_pipe::spawn_named_pipe_acceptor(&pipe_name_clone, telemetry_clone_for_thread, shutdown_clone);
        });

        // Join the acceptor thread on shutdown (spawn blocking join so we don't block async runtime)
        tokio::spawn(async move {
            // Wait until shutdown flag is set
            while !shutdown.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            // Wake acceptor in case it's blocked
            windows_pipe::wake_acceptor(&pipe_name);
            let _ = tokio::task::spawn_blocking(move || {
                let _ = handle.join();
            }).await;
        });

        Ok(())
    }
}

#[cfg(windows)]
#[allow(dead_code)]
async fn handle_connection_tcp(stream: tokio::net::TcpStream, telemetry: TelemetryEmitter, shutdown: Arc<AtomicBool>) -> Result<(), Box<dyn Error + Send + Sync>> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r).lines();

    let mut dispatcher = Dispatcher::new(telemetry.clone());
    while let Some(line) = reader.next_line().await? {
        if shutdown.load(Ordering::SeqCst) {
            // ask dispatcher to shutdown and await completion (bounded)
            let resp = dispatcher.handle_command(IpcCommand::Shutdown, "shutdown");
            let _ = dispatcher.shutdown_with_timeout(Some(std::time::Duration::from_secs(5))).await;
            let s = resp.to_json()?;
            w.write_all(s.as_bytes()).await?;
            w.write_all(b"\n").await?;
            break;
        }
        if line.trim().is_empty() { continue; }
        match parse_command(&line) {
            Ok((cmd, id)) => {
                match cmd {
                    IpcCommand::SubscribeTelemetry => {
                        // acknowledge
                        let ack = crate::commands::ResponseEnvelope::ok(id.clone());
                        let s = ack.to_json()?;
                        w.write_all(s.as_bytes()).await?;
                        w.write_all(b"\n").await?;

                        // start streaming telemetry to this client. Take a subscription receiver
                        let mut rx = telemetry.subscribe();
                        // spawn a task that writes telemetry messages to the client
                        let mut w_clone = w;
                        tokio::spawn(async move {
                            loop {
                                match rx.recv().await {
                                    Ok(msg) => {
                                        if let Err(e) = w_clone.write_all(msg.as_bytes()).await {
                                            tracing::debug!(%e, "failed to write telemetry to client; closing stream");
                                            break;
                                        }
                                        if let Err(e) = w_clone.write_all(b"\n").await {
                                            tracing::debug!(%e, "failed to write newline to client; closing stream");
                                            break;
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                        });

                        // After starting telemetry stream, stop processing further commands on this connection.
                        break;
                    }
                    other => {
                        let resp = dispatcher.handle_command(other, id.clone());
                        let json = resp.to_json()?;
                        w.write_all(json.as_bytes()).await?;
                        w.write_all(b"\n").await?;
                    }
                }
            }
            Err(e) => {
                let err = serde_json::json!({ "id": null, "ok": false, "error": format!("{}", e) });
                let s = serde_json::to_string(&err)?;
                w.write_all(s.as_bytes()).await?;
                w.write_all(b"\n").await?;
            }
        }
    }

    Ok(())
}

/// Run a TCP-based IPC listener on `addr` and dispatch connections.
/// This is enabled on Windows (where the generic named-pipe acceptor is used
/// by default) so we provide an explicit TCP listener to allow UI clients to
/// connect over TCP (127.0.0.1:9000).
#[cfg(windows)]
pub async fn run_tcp_server(addr: &str, telemetry: TelemetryEmitter, shutdown: Arc<AtomicBool>) -> Result<(), Box<dyn Error + Send + Sync>> {
    use tokio::net::TcpListener;
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("bsm-ipc: tcp listening {addr}");
    loop {
        if shutdown.load(Ordering::SeqCst) { break; }
        let (socket, _) = listener.accept().await?;
        let telemetry_clone = telemetry.clone();
        let shutdown_clone = shutdown.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection_tcp(socket, telemetry_clone, shutdown_clone).await {
                tracing::warn!(%e, "tcp client handler error");
            }
        });
    }
    Ok(())
}

#[cfg(unix)]
async fn handle_connection_unix(stream: UnixStream, telemetry: TelemetryEmitter, shutdown: Arc<AtomicBool>) -> Result<(), Box<dyn Error + Send + Sync>> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r).lines();

    let mut dispatcher = Dispatcher::new(telemetry.clone());

    while let Some(line) = reader.next_line().await? {
        if shutdown.load(Ordering::SeqCst) {
            let resp = dispatcher.handle_command(IpcCommand::Shutdown, "shutdown");
            let _ = dispatcher.shutdown_with_timeout(Some(std::time::Duration::from_secs(5))).await;
            let s = resp.to_json()?;
            w.write_all(s.as_bytes()).await?;
            w.write_all(b"\n").await?;
            break;
        }
        if line.trim().is_empty() { continue; }
        match parse_command(&line) {
            Ok((cmd, id)) => {
                match cmd {
                    IpcCommand::SubscribeTelemetry => {
                        let ack = crate::commands::ResponseEnvelope::ok(id.clone());
                        let s = ack.to_json()?;
                        w.write_all(s.as_bytes()).await?;
                        w.write_all(b"\n").await?;

                        let mut rx = telemetry.subscribe();
                        let mut w_clone = w;
                        tokio::spawn(async move {
                            loop {
                                match rx.recv().await {
                                    Ok(msg) => {
                                        if let Err(e) = w_clone.write_all(msg.as_bytes()).await {
                                            tracing::debug!(%e, "failed to write telemetry to client; closing stream");
                                            break;
                                        }
                                        if let Err(e) = w_clone.write_all(b"\n").await {
                                            tracing::debug!(%e, "failed to write newline to client; closing stream");
                                            break;
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                        });

                        break;
                    }
                    other => {
                        let resp = dispatcher.handle_command(other, id.clone());
                        let json = resp.to_json()?;
                        w.write_all(json.as_bytes()).await?;
                        w.write_all(b"\n").await?;
                    }
                }
            }
            Err(e) => {
                let err = serde_json::json!({ "id": null, "ok": false, "error": format!("{}", e) });
                let s = serde_json::to_string(&err)?;
                w.write_all(s.as_bytes()).await?;
                w.write_all(b"\n").await?;
            }
        }
    }

    Ok(())
}

// These tests exercise the Windows-only TCP fallback handler
// (`handle_connection_tcp`); Linux uses the unix-socket path, so this whole
// module is Windows-only.
#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::io::{AsyncWriteExt, AsyncBufReadExt, BufReader};
    use tokio::time::{timeout, Duration};
    use std::sync::{Arc, atomic::AtomicBool};

    #[tokio::test]
    async fn server_accepts_and_replies_get_status() {
        // Bind to ephemeral port
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Spawn server task using existing listener
        tokio::spawn(async move {
            // create telemetry
            let (tx, _rx) = broadcast::channel::<String>(8);
            let telemetry = TelemetryEmitter::new(tx);
            let incoming = listener;
            if let Ok((socket, _)) = incoming.accept().await {
                let shutdown = Arc::new(AtomicBool::new(false));
                let _ = handle_connection_tcp(socket, telemetry, shutdown).await;
            }
        });

        // Connect client, send get_status and read reply
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let req = r#"{"cmd":"get_status","id":"t1","payload":{}}"#;
        stream.write_all(req.as_bytes()).await.unwrap();
        stream.write_all(b"\n").await.unwrap();

        let mut reader = BufReader::new(stream).lines();
        let line = timeout(Duration::from_secs(2), reader.next_line()).await.unwrap().unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(v["ok"].as_bool().unwrap());
    }
}

