use crate::client::HrtInboundMsg;
use bsm_core::AppState;
use bsm_ipc::telemetry::TelemetryEmitter;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

/// Spawn the HRT bridge task which listens for inbound HRT messages and routes
/// them into local subsystems (actions, telemetry, shutdown hooks).
///
/// - `inbound_rx`: receives `HrtInboundMsg` from the HRT client.
/// - `shutdown_rx`: receives shutdown notifications to stop the bridge.
///
/// Returns a `JoinHandle` for the spawned task.
pub fn spawn_bridge(
	app_state: Arc<AppState>,
	telemetry: TelemetryEmitter,
	mut inbound_rx: broadcast::Receiver<HrtInboundMsg>,
	mut shutdown_rx: broadcast::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
	tokio::spawn(async move {
		info!("hrt: bridge started");

		loop {
			tokio::select! {
				_ = shutdown_rx.recv() => {
					info!("hrt: bridge shutdown received");
					break;
				}

				msg = inbound_rx.recv() => match msg {
					Ok(msg) => {
						debug!(msg = %msg.msg, "hrt: bridge routing message");
						// Forward to the actions handler which performs local side-effects
						// such as emitting telemetry or initiating shutdown.
						let app = app_state.clone();
						let telem = telemetry.clone();
						crate::actions::handle_inbound_message(app, telem, msg);
					}
					Err(broadcast::error::RecvError::Lagged(n)) => {
						warn!(lost = n, "hrt: bridge lagged and dropped messages");
					}
					Err(broadcast::error::RecvError::Closed) => {
						info!("hrt: inbound channel closed, stopping bridge");
						break;
					}
				}
			}
		}

		info!("hrt: bridge stopped");
	})
}
