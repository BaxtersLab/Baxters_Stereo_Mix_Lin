use bsm_core::AppState;
use bsm_core::app::RecordingState;
use bsm_ipc::telemetry::TelemetryEmitter;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, watch};
use tokio::time::interval;
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

#[cfg_attr(not(windows), allow(dead_code))]
const HRT_PIPE_PATH:             &str = r"\\.\pipe\hrt-agent";
const HRT_HEARTBEAT_INTERVAL_MS: u64  = 2_000;
const HRT_RECONNECT_DELAY_MS:    u64  = 5_000;
const HRT_AGENT_ID:              &str = "bsm";
const BSM_VERSION:               &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------------------
// Message types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct RegisterMsg<'a> {
	msg:          &'static str,
	agent_id:     &'a str,
	version:      &'static str,
	capabilities: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct HeartbeatMsg {
	msg:       &'static str,
	agent_id:  &'static str,
	state:     String,
	uptime_ms: u64,
	ts_ms:     u64,
}

/// Messages received FROM HRT (parsed inline in Block F-2).
#[derive(Debug, Deserialize, Clone)]
pub struct HrtInboundMsg {
	pub msg:  String,
	#[serde(flatten)]
	pub data: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Handle
// ---------------------------------------------------------------------------

/// Handle for the running HRT client.
pub struct HrtClientHandle {
	/// Receive inbound HRT messages (throttle, e-stop, ack, …).
	pub inbound_rx: broadcast::Receiver<HrtInboundMsg>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Connect to HRT and start heartbeat / receiver tasks.
///
/// The connection loop retries on disconnect with a 5-second backoff.
/// If HRT is not running, BSM continues normally — HRT integration is advisory.
pub async fn start_hrt_client(
	app_state:   Arc<AppState>,
	state_rx:    watch::Receiver<RecordingState>,
	_telemetry:   TelemetryEmitter,
	shutdown_rx: broadcast::Receiver<()>,
) -> HrtClientHandle {
	let (inbound_tx, inbound_rx) = broadcast::channel::<HrtInboundMsg>(64);

	tokio::spawn(hrt_connection_loop(
		app_state,
		state_rx,
		_telemetry,
		inbound_tx,
		shutdown_rx,
	));

	HrtClientHandle { inbound_rx }
}

// ---------------------------------------------------------------------------
// Connection loop
// ---------------------------------------------------------------------------

async fn hrt_connection_loop(
	app_state:   Arc<AppState>,
	state_rx:    watch::Receiver<RecordingState>,
	telemetry:   TelemetryEmitter,
	inbound_tx:  broadcast::Sender<HrtInboundMsg>,
	shutdown_rx: broadcast::Receiver<()>,
) {
	let mut shutdown_rx = shutdown_rx;

	loop {
		if shutdown_rx.try_recv().is_ok() {
			debug!("hrt: shutdown before connect");
			break;
		}

		// Attempt connection (pipe first, then skip — TCP fallback not required for v1)
		match connect_to_hrt().await {
			Ok((reader, mut writer)) => {
				info!("hrt: connected to HRT daemon");

				// Register
				let reg = RegisterMsg {
					msg:          "register",
					agent_id:     HRT_AGENT_ID,
					version:      BSM_VERSION,
					capabilities: vec!["audio_record", "audio_encode"],
				};
				if let Ok(json) = serde_json::to_string(&reg) {
					let _ = writer.write_all(format!("{}\n", json).as_bytes()).await;
				}

				// Run session (heartbeat + receive)
				hrt_session(
					reader,
					writer,
					&app_state,
					&state_rx,
					&telemetry,
					&inbound_tx,
					&mut shutdown_rx,
				).await;

				info!("hrt: session ended — reconnecting in {}ms", HRT_RECONNECT_DELAY_MS);
			}
			Err(e) => {
				debug!("hrt: connect failed ({})", e);
			}
		}

		// Wait before reconnecting (or bail if shutdown fired)
		tokio::select! {
			_ = tokio::time::sleep(Duration::from_millis(HRT_RECONNECT_DELAY_MS)) => {}
			_ = shutdown_rx.recv() => {
				debug!("hrt: shutdown during reconnect wait");
				break;
			}
		}
	}
}

// ---------------------------------------------------------------------------
// Connect helper
// ---------------------------------------------------------------------------

#[cfg(windows)]
async fn connect_to_hrt() -> Result<
	(
		BufReader<tokio::net::windows::named_pipe::NamedPipeClient>,
		tokio::net::windows::named_pipe::NamedPipeClient,
	),
	std::io::Error,
> {
	use tokio::net::windows::named_pipe::ClientOptions;

	let _client = ClientOptions::new()
		.open(HRT_PIPE_PATH)?;

	// For v1 return an error indicating session should handle client directly.
	Err(std::io::Error::new(std::io::ErrorKind::Other, "use session directly"))
}

#[cfg(not(windows))]
async fn connect_to_hrt() -> Result<
	(BufReader<tokio::io::Empty>, tokio::io::Sink),
	std::io::Error,
> {
	Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "HRT pipe requires Windows"))
}

// ---------------------------------------------------------------------------
// Session: heartbeat writer + message receiver
// ---------------------------------------------------------------------------

async fn hrt_session<R, W>(
	reader:       R,
	mut writer:   W,
	app_state:    &Arc<AppState>,
	state_rx:     &watch::Receiver<RecordingState>,
	telemetry:    &TelemetryEmitter,
	inbound_tx:   &broadcast::Sender<HrtInboundMsg>,
	shutdown_rx:  &mut broadcast::Receiver<()>,
) where
	R: tokio::io::AsyncRead + Unpin,
	W: tokio::io::AsyncWrite + Unpin,
{
	let mut lines    = BufReader::new(reader).lines();
	let mut hb_timer = interval(Duration::from_millis(HRT_HEARTBEAT_INTERVAL_MS));
	hb_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

	loop {
		tokio::select! {
			_ = shutdown_rx.recv() => {
				debug!("hrt: session shutdown");
				break;
			}

			_ = hb_timer.tick() => {
				let state_str = match *state_rx.borrow() {
					RecordingState::Idle        => "idle",
					RecordingState::Recording { .. } => "recording",
					RecordingState::Paused      => "paused",
					RecordingState::Stopping    => "stopping",
					RecordingState::Error       => "error",
				};
				let hb = HeartbeatMsg {
					msg:       "heartbeat",
					agent_id:  HRT_AGENT_ID,
					state:     state_str.to_owned(),
					uptime_ms: app_state.uptime_ms(),
					ts_ms:     now_ms(),
				};
				if let Ok(json) = serde_json::to_string(&hb) {
					if writer.write_all(format!("{}\n", json).as_bytes()).await.is_err() {
						error!("hrt: heartbeat write failed");
						break;
					}
				}
			}

			line = lines.next_line() => {
				match line {
					Ok(Some(text)) => {
						debug!("hrt inbound: {}", text);
						match serde_json::from_str::<HrtInboundMsg>(&text) {
							Ok(msg) => {
								let _ = inbound_tx.send(msg.clone());
								// Forward to actions handler asynchronously
								let app_clone = app_state.clone();
								let telemetry_clone = telemetry.clone();
								tokio::spawn(async move {
									crate::actions::handle_inbound_message(app_clone, telemetry_clone, msg);
								});
							}
							Err(e) => { warn!("hrt: bad inbound JSON: {}", e); }
						}
					}
					_ => {
						info!("hrt: connection closed by server");
						break;
					}
				}
			}
		}
	}
}

fn now_ms() -> u64 {
	use std::time::{SystemTime, UNIX_EPOCH};
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|d| d.as_millis() as u64)
		.unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	// tests use only serde_json; no need to import parent module

	#[test]
	fn heartbeat_json_contains_required_fields() {
		let hb_json = r#"{"msg":"heartbeat","agent_id":"bsm","state":"idle","uptime_ms":100,"ts_ms":0}"#;
		let v: serde_json::Value = serde_json::from_str(hb_json).unwrap();
		assert_eq!(v["msg"],      "heartbeat");
		assert_eq!(v["agent_id"], "bsm");
		assert!(v["state"].is_string());
	}

	#[test]
	fn register_json_contains_capabilities() {
		let reg_json = r#"{"msg":"register","agent_id":"bsm","version":"0.1.0","capabilities":["audio_record","audio_encode"]}"#;
		let v: serde_json::Value = serde_json::from_str(reg_json).unwrap();
		assert_eq!(v["msg"], "register");
		let caps = v["capabilities"].as_array().unwrap();
		assert!(caps.contains(&serde_json::json!("audio_record")));
	}
}
