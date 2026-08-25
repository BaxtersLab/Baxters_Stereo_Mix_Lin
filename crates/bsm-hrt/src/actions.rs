use crate::client::HrtInboundMsg;
use bsm_core::AppState;
use bsm_ipc::telemetry::TelemetryEmitter;
use std::sync::Arc;
use tracing::{info, warn};

fn now_ms() -> u64 {
	use std::time::{SystemTime, UNIX_EPOCH};
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|d| d.as_millis() as u64)
		.unwrap_or(0)
}

/// Handle an inbound HRT message and perform local actions.
pub fn handle_inbound_message(app_state: Arc<AppState>, telemetry: TelemetryEmitter, msg: HrtInboundMsg) {
	let tag = msg.msg.to_lowercase();
	match tag.as_str() {
		"throttle" => {
			// Try to extract a throttle level from the payload
			let level = msg.data.get("level")
				.and_then(|v| v.as_f64())
				.or_else(|| msg.data.get("value").and_then(|v| v.as_f64()))
				.or_else(|| msg.data.get("percent").and_then(|v| v.as_f64()));
			if let Some(l) = level {
				info!(level = l, "hrt: throttle command received");
				// Emit telemetry about throttle
				let envelope = serde_json::json!({
					"event": "hrt_throttle",
					"ts_ms": now_ms(),
					"data": { "level": l }
				});
				telemetry.emit_raw(envelope.to_string());
			} else {
				warn!("hrt: throttle message missing level");
			}
		}

		"e-stop" | "estop" | "emergency_stop" => {
			info!("hrt: emergency stop received — initiating shutdown");
			// Trigger application shutdown
			app_state.initiate_shutdown();
			let envelope = serde_json::json!({
				"event": "hrt_emergency_stop",
				"ts_ms": now_ms(),
				"data": msg.data
			});
			telemetry.emit_raw(envelope.to_string());
		}

		"ack" | "acknowledge" => {
			info!("hrt: ack received: {:?}", msg.data);
			let envelope = serde_json::json!({
				"event": "hrt_ack",
				"ts_ms": now_ms(),
				"data": msg.data
			});
			telemetry.emit_raw(envelope.to_string());
		}

		other => {
			warn!(msg = %other, "hrt: unknown inbound message");
			let envelope = serde_json::json!({
				"event": "hrt_unknown",
				"ts_ms": now_ms(),
				"data": { "msg": msg.msg, "payload": msg.data }
			});
			telemetry.emit_raw(envelope.to_string());
		}
	}
}
