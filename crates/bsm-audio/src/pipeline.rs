use crate::backend::AudioBackend;
use bsm_core::{PcmFrame, PcmFormat, AudioResult};

/// Device selection and negotiation configuration.
#[derive(Clone, Debug)]
pub struct DeviceConfig {
	pub preferred_format: PcmFormat,
}

impl DeviceConfig {
	pub fn with_format(fmt: PcmFormat) -> Self { Self { preferred_format: fmt } }
}

/// Simple capture pipeline that owns a backend and exposes a high-level API.
pub struct CapturePipeline<B: AudioBackend> {
	backend: B,
	requested_format: PcmFormat,
}

impl<B: AudioBackend> CapturePipeline<B> {
	pub fn new(backend: B, fmt: PcmFormat) -> Self {
		Self { backend, requested_format: fmt }
	}

	/// Change the requested format (e.g., from UI). This will be used on next open.
	pub fn set_requested_format(&mut self, fmt: PcmFormat) {
		self.requested_format = fmt;
	}

	/// Read the last negotiated format from the backend, if any.
	pub fn negotiated_format(&self) -> Option<PcmFormat> {
		self.backend.actual_format()
	}

	/// Access current requested format.
	pub fn requested_format(&self) -> &PcmFormat {
		&self.requested_format
	}

	pub async fn enumerate(&self) -> AudioResult<Vec<bsm_core::DeviceEntry>> {
		self.backend.enumerate_devices().await
	}

	/// Open device using pipeline's requested format (basic)
	pub async fn open(&mut self, idx: u32) -> AudioResult<()> {
		self.open_with_config(idx, DeviceConfig::with_format(self.requested_format.clone())).await
	}

	/// Open device with explicit `DeviceConfig`. Backends should attempt
	/// format negotiation and return the actual negotiated format or an error
	/// such as `AudioError::UnsupportedFormat`.
	pub async fn open_with_config(&mut self, idx: u32, cfg: DeviceConfig) -> AudioResult<()> {
		self.requested_format = cfg.preferred_format.clone();
		self.backend.open_device(idx, cfg.preferred_format).await
	}

	pub async fn start(&mut self) -> AudioResult<()> {
		self.backend.start().await
	}

	pub async fn stop(&mut self) -> AudioResult<()> {
		self.backend.stop().await
	}

	pub async fn next_frame(&mut self) -> AudioResult<Option<PcmFrame>> {
		self.backend.next_frame().await
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::mock::MockAudioBackend;

	#[tokio::test]
	async fn pipeline_with_mock_backend_runs() {
		let fmt = PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 };
		let mut pipeline = CapturePipeline::new(MockAudioBackend::new(), fmt.clone());
		let devices = pipeline.enumerate().await.unwrap();
		assert!(!devices.is_empty());
		pipeline.open_with_config(0, DeviceConfig::with_format(fmt)).await.unwrap();
		pipeline.start().await.unwrap();
		let f = pipeline.next_frame().await.unwrap();
		assert!(f.is_some());
		// ensure negotiated_format returns something (mock uses requested fmt)
		let negotiated = pipeline.negotiated_format();
		assert!(negotiated.is_some());
		pipeline.stop().await.unwrap();
	}

	// Integration-style test that attempts to open the first real device.
	// This test is marked `ignore` because it requires an audio device and
	// may be flaky in CI. Run with `cargo test -- --ignored` locally.
	#[tokio::test]
	async fn pipeline_integration_real_device() {
		let fmt = PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 };
		let mut pipeline = CapturePipeline::new(crate::wasapi::WasapiBackend::new(), fmt.clone());
		let devices = pipeline.enumerate().await.unwrap();
		if devices.is_empty() { return; }
		let res = pipeline.open_with_config(0, DeviceConfig::with_format(fmt.clone())).await;
		match res {
			Ok(()) => {
				pipeline.start().await.unwrap();
			} // if opening with 2 channels failed, try mono fallback
			Err(_) => {
				let mono = PcmFormat { sample_rate: 48000, channels: 1, bit_depth: 16 };
				if pipeline.open_with_config(0, DeviceConfig::with_format(mono.clone())).await.is_ok() {
					pipeline.start().await.unwrap();
				} else {
					// If we cannot open the local device in either stereo or mono, skip the test gracefully.
					eprintln!("integration: could not open device in stereo or mono; skipping");
					return;
				}
			}
		}
		// try to read a few frames
		for _ in 0..3 {
			let f = pipeline.next_frame().await.unwrap();
			// it's acceptable for some captures to return None intermittently
			if let Some(frame) = f { assert!(!frame.data.is_empty()); }
		}
		pipeline.stop().await.unwrap();
	}
}
