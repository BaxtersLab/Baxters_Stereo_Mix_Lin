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

/// A boxed backend is a backend, so code can choose one at run time: the IPC
/// dispatcher takes a factory, the app giving it the real device and its tests
/// the mock (they used to record from this machine's microphone).
#[async_trait]
impl AudioBackend for Box<dyn AudioBackend> {
	async fn enumerate_devices(&self) -> AudioResult<Vec<DeviceEntry>> { (**self).enumerate_devices().await }
	async fn open_device(&mut self, device_index: u32, format: PcmFormat) -> AudioResult<()> {
		(**self).open_device(device_index, format).await
	}
	async fn start(&mut self) -> AudioResult<()> { (**self).start().await }
	async fn stop(&mut self) -> AudioResult<()> { (**self).stop().await }
	async fn close(&mut self) -> AudioResult<()> { (**self).close().await }
	async fn next_frame(&mut self) -> AudioResult<Option<PcmFrame>> { (**self).next_frame().await }
	fn is_active(&self) -> bool { (**self).is_active() }
	fn actual_format(&self) -> Option<PcmFormat> { (**self).actual_format() }
	fn device_name(&self) -> Option<String> { (**self).device_name() }
}
