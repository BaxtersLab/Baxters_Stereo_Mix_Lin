use async_trait::async_trait;
use bsm_core::{AudioResult, DeviceEntry, PcmFormat, PcmFrame};

/// Platform abstraction boundary for audio capture.
/// All backends (WASAPI, mock) implement this trait.
#[async_trait]
pub trait AudioBackend: Send + Sync {
	/// Enumerate available loopback/capture devices.
	async fn enumerate_devices(&self) -> AudioResult<Vec<DeviceEntry>>;

	/// Open the device at the given index and prepare for capture.
	/// Does NOT start streaming — call start() after open().
	async fn open_device(&mut self, device_index: u32, format: PcmFormat) -> AudioResult<()>;

	/// Start streaming PCM frames. Must have called open_device() first.
	async fn start(&mut self) -> AudioResult<()>;

	/// Stop streaming. Flushes any buffered data. Device remains open.
	async fn stop(&mut self) -> AudioResult<()>;

	/// Close the device and release all platform resources.
	async fn close(&mut self) -> AudioResult<()>;

	/// Retrieve the next available PCM frame. Blocks until data is ready.
	/// Returns Ok(None) when the backend has stopped gracefully.
	async fn next_frame(&mut self) -> AudioResult<Option<PcmFrame>>;

	/// Returns true if the backend is currently streaming.
	fn is_active(&self) -> bool;

	/// Returns the actual PCM format negotiated with the device.
	/// May differ from the requested format (device may coerce sample rate).
	fn actual_format(&self) -> Option<PcmFormat>;

	/// Returns the name of the currently open device, if any.
	fn device_name(&self) -> Option<String>;
}
