// bsm-core — core primitives and shared utilities
// Implementations for A-* blocks are added progressively.

pub mod error;
pub mod types;
pub mod config;
pub mod app;
pub mod instance;
pub mod logging;

pub use config::BsmConfig;
pub use app::{AppState, RecordingState};
pub use error::{BsmError, BsmResult, AudioError, AudioResult, EncodeError, EncodeResult, IpcError, IpcResult};
pub use types::{DeviceEntry, PcmFormat, PcmFrame, SessionInfo, Channels, FrameCount, InstanceId, SampleRate};
pub use instance::{Instance, InstanceManager, InstanceState, SingleInstanceGuard, InstanceError};
pub use logging::init_logging;
