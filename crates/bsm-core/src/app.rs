use std::sync::Arc;
use tokio::sync::{broadcast, watch, Mutex};
use crate::config::BsmConfig;

/// Recording session state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingState {
    Idle,
    Recording,
    Paused,
    Stopping,
    Error,
}

/// App-wide shared state. Wrapped in Arc for cross-task access.
pub struct AppState {
    /// Live config — can be mutated at runtime via IPC or UI.
    pub config: Arc<Mutex<BsmConfig>>,

    /// Current recording state. All subsystems watch this.
    pub recording_state: watch::Sender<RecordingState>,

    /// Shutdown signal broadcast. All long-running tasks select on this.
    pub shutdown: broadcast::Sender<()>,

    /// App start timestamp (monotonic) for uptime.
    pub started_at: std::time::Instant,

    /// Name of the currently selected audio device.
    pub device_name: Arc<Mutex<String>>,

    /// Current session output file path (None if not recording).
    pub output_file: Arc<Mutex<Option<String>>>,
}

impl AppState {
    pub fn new(config: BsmConfig) -> Self {
        let (recording_tx, _) = watch::channel(RecordingState::Idle);
        let (shutdown_tx, _) = broadcast::channel(8);
        Self {
            config: Arc::new(Mutex::new(config)),
            recording_state: recording_tx,
            shutdown: shutdown_tx,
            started_at: std::time::Instant::now(),
            device_name: Arc::new(Mutex::new(String::new())),
            output_file: Arc::new(Mutex::new(None)),
        }
    }

    /// Subscribe to recording state changes.
    pub fn recording_state_rx(&self) -> watch::Receiver<RecordingState> {
        self.recording_state.subscribe()
    }

    /// Subscribe to shutdown signal.
    pub fn shutdown_rx(&self) -> broadcast::Receiver<()> {
        self.shutdown.subscribe()
    }

    /// Trigger graceful shutdown — all subscribers will unblock.
    pub fn initiate_shutdown(&self) {
        let _ = self.shutdown.send(());
    }

    /// App uptime in milliseconds.
    pub fn uptime_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }
}
