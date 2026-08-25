use tokio::net::TcpListener;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
use std::collections::HashMap;
use crate::output::{RecordingSession};
use crate::output;
use crate::muxer::ContainerFormat;
use std::time::Duration;
use tokio::task::JoinHandle;
use std::path::Path;
use std::env;
use std::path::Component;

#[derive(Deserialize)]
#[serde(tag = "cmd")]
enum Command {
    #[serde(rename = "start")]
    Start { token: Option<String>, output_dir: String, filename_template: Option<String>, encoder: Option<String>, format: Option<String> },
    #[serde(rename = "stop")]
    Stop { id: String },
    #[serde(rename = "status")]
    Status { id: Option<String> },
}

#[derive(Serialize)]
struct Response<T> { status: &'static str, data: Option<T>, error: Option<String> }

pub async fn run_agent_server(addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(addr).await?;
    // sessions map: session_id -> RecordingSessionHandle
    let sessions: Arc<Mutex<HashMap<String, RecordingSessionHandle>>> = Arc::new(Mutex::new(HashMap::new()));

    loop {
        let (socket, peer) = listener.accept().await?;
        let sessions = sessions.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(socket, peer, sessions).await {
                let _ = eprintln!("ipc handler error: {}", e);
            }
        });
    }
}

struct RecordingSessionHandle {
    origin: String,
    handle: output::SessionHandle,
    timeout: JoinHandle<()>,
}
async fn handle_conn(mut socket: tokio::net::TcpStream, peer: std::net::SocketAddr, sessions: Arc<Mutex<HashMap<String, RecordingSessionHandle>>>) -> Result<(), Box<dyn std::error::Error>> {
    let origin = peer.ip().to_string();
    let (r, mut w) = socket.split();
    let mut reader = BufReader::new(r);
    let mut buf = String::new();
    reader.read_line(&mut buf).await?;
    if buf.trim().is_empty() { return Ok(()); }
    let cmd: Command = serde_json::from_str(&buf).map_err(|e| format!("invalid json: {}", e))?;

    match cmd {
        Command::Start { token, output_dir, filename_template, encoder, format } => {
            // Authentication: if BSM_API_TOKEN is set, require matching token
            if let Ok(expected) = env::var("BSM_API_TOKEN") {
                if token.as_deref() != Some(&expected) {
                    let resp: Response<serde_json::Value> = Response { status: "error", data: None, error: Some("authentication failed".into()) };
                    let s = serde_json::to_string(&resp)? + "\n";
                    w.write_all(s.as_bytes()).await?;
                    return Ok(());
                }
            }

            // Template sanitization and size limits
            let template = filename_template.unwrap_or_else(|| "BSM_{date}_{time}_{n}".into());
            if template.len() > 128 || template.contains('/') || template.contains('\\') {
                let resp: Response<serde_json::Value> = Response { status: "error", data: None, error: Some("invalid filename_template".into()) };
                let s = serde_json::to_string(&resp)? + "\n";
                w.write_all(s.as_bytes()).await?;
                return Ok(());
            }

            let fmt = match format.as_deref() {
                Some("wav") | None => ContainerFormat::Wav,
                Some("mp3") => ContainerFormat::Mp3,
                Some("flac") => ContainerFormat::Flac,
                _ => ContainerFormat::Wav,
            };

            // Output folder must be relative and not escape an allowed root.
            let allowed_root = env::var("BSM_OUTPUT_ROOT").unwrap_or_else(|_| "./recordings".into());
            let p = Path::new(&output_dir);
            if p.is_absolute() {
                let resp: Response<serde_json::Value> = Response { status: "error", data: None, error: Some("output_dir must be relative".into()) };
                let s = serde_json::to_string(&resp)? + "\n";
                w.write_all(s.as_bytes()).await?;
                return Ok(());
            }
            if p.components().any(|c| matches!(c, Component::ParentDir)) {
                let resp: Response<serde_json::Value> = Response { status: "error", data: None, error: Some("output_dir contains parent traversal".into()) };
                let s = serde_json::to_string(&resp)? + "\n";
                w.write_all(s.as_bytes()).await?;
                return Ok(());
            }

            let cfg = bsm_core::config::OutputConfig { output_folder: format!("{}/{}", allowed_root.trim_end_matches('/'), output_dir), file_name_pattern: template.clone(), ..Default::default() };
            let path = match output::resolve_output_path(&cfg, fmt) {
                Ok(p) => p,
                Err(e) => {
                    let resp: Response<serde_json::Value> = Response { status: "error", data: None, error: Some(format!("resolve output path failed: {}", e)) };
                    let s = serde_json::to_string(&resp)? + "\n";
                    w.write_all(s.as_bytes()).await?;
                    return Ok(());
                }
            };

            // rate-limit: limit concurrent sessions per origin
            const MAX_PER_ORIGIN: usize = 4;
            const MAX_TOTAL: usize = 64;
            let map = sessions.lock().await;
            let per_origin = map.values().filter(|rs| rs.origin == origin).count();
            if per_origin >= MAX_PER_ORIGIN || map.len() >= MAX_TOTAL {
                let resp: Response<serde_json::Value> = Response { status: "error", data: None, error: Some("too many sessions".into()) };
                let s = serde_json::to_string(&resp)? + "\n";
                w.write_all(s.as_bytes()).await?;
                return Ok(());
            }

            drop(map);

            // make channels and start session
            let (_tx, rx) = tokio::sync::mpsc::channel::<bsm_core::PcmFrame>(256);
            let (_shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel::<()>(4);

            let sess = RecordingSession::new(path.clone(), encoder.unwrap_or_else(|| "libopus".into()), bsm_core::config::EncoderConfig::default(), bsm_core::PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 }, fmt);
            let handle = sess.start(rx, shutdown_rx).await?;

            let id = format!("session-{}", chrono::Utc::now().timestamp_millis());

            // spawn timeout task to auto-stop after 5 hours
            let timeout_handle = {
                let shutdown = handle.shutdown_tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(5 * 3600)).await;
                    let _ = shutdown.send(());
                })
            };

            let rs = RecordingSessionHandle { origin: origin.clone(), handle, timeout: timeout_handle };
            sessions.lock().await.insert(id.clone(), rs);

            // respond with id
            let resp = Response { status: "ok", data: Some(serde_json::json!({ "id": id, "output": path.to_string_lossy() })), error: None };
            let s = serde_json::to_string(&resp)? + "\n";
            w.write_all(s.as_bytes()).await?;
        }
        Command::Stop { id } => {
            let mut map = sessions.lock().await;
            if let Some(rs) = map.remove(&id) {
                // cancel timeout
                rs.timeout.abort();
                // stop and wait
                match rs.handle.stop().await {
                    Ok(info) => {
                        let resp = Response { status: "ok", data: Some(info), error: None };
                        let s = serde_json::to_string(&resp)? + "\n";
                        w.write_all(s.as_bytes()).await?;
                    }
                    Err(e) => {
                        let resp: Response<serde_json::Value> = Response { status: "error", data: None, error: Some(format!("stop failed: {}", e)) };
                        let s = serde_json::to_string(&resp)? + "\n";
                        w.write_all(s.as_bytes()).await?;
                    }
                }
            } else {
                let resp: Response<serde_json::Value> = Response { status: "error", data: None, error: Some("id not found".into()) };
                let s = serde_json::to_string(&resp)? + "\n";
                w.write_all(s.as_bytes()).await?;
            }
        }
        Command::Status { id } => {
            let m = sessions.lock().await;
            if let Some(id) = id {
                let ok = m.contains_key(&id);
                let resp = Response { status: "ok", data: Some(serde_json::json!({ "id": id, "active": ok })), error: None };
                let s = serde_json::to_string(&resp)? + "\n";
                w.write_all(s.as_bytes()).await?;
            } else {
                let list: Vec<_> = m.keys().cloned().collect();
                let resp = Response { status: "ok", data: Some(serde_json::json!({ "sessions": list })), error: None };
                let s = serde_json::to_string(&resp)? + "\n";
                w.write_all(s.as_bytes()).await?;
            }
        }
    }

    Ok(())
}
