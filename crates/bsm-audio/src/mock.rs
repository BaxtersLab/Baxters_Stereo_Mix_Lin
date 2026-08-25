// Mock audio backend — generates synthetic sine-wave PCM frames.
// Used in unit tests and headless CI runs only. Never used in production.

use async_trait::async_trait;
use bsm_core::{AudioResult, DeviceEntry, PcmFormat, PcmFrame};
use super::backend::AudioBackend;
use std::time::{Duration, Instant};

pub struct MockAudioBackend {
	active:      bool,
	format:      Option<PcmFormat>,
	device_name: String,
	sequence:    u64,
	started_at:  Option<Instant>,
	tone_hz:     f64,
}

impl MockAudioBackend {
	pub fn new() -> Self {
		Self {
			active:      false,
			format:      None,
			device_name: "Mock Loopback Device".into(),
			sequence:    0,
			started_at:  None,
			tone_hz:     440.0,
		}
	}

	fn generate_frame(&mut self, frame_count: u32) -> PcmFrame {
		let fmt = self.format.clone().unwrap_or(PcmFormat {
			sample_rate: 48000,
			channels:    2,
			bit_depth:   16,
		});

		let mut data = Vec::with_capacity(
			frame_count as usize * fmt.bytes_per_frame() as usize,
		);

		let elapsed_samples = self.sequence * frame_count as u64;
		for i in 0..frame_count {
			let t = (elapsed_samples + i as u64) as f64 / fmt.sample_rate as f64;
			let sample = (f64::sin(2.0 * std::f64::consts::PI * self.tone_hz * t)
						 * i16::MAX as f64) as i16;
			for _ in 0..fmt.channels {
				data.extend_from_slice(&sample.to_le_bytes());
			}
		}

		let ts = self.started_at
			.map(|s| s.elapsed().as_micros() as u64)
			.unwrap_or(0);

		let seq = self.sequence;
		self.sequence += 1;

		PcmFrame {
			data,
			format: fmt,
			timestamp_us: ts,
			sequence: seq,
			frame_count,
		}
	}
}

#[async_trait]
impl AudioBackend for MockAudioBackend {
	async fn enumerate_devices(&self) -> AudioResult<Vec<DeviceEntry>> {
		Ok(vec![DeviceEntry {
			index:       0,
			name:        "Mock Loopback Device".into(),
			is_default:  true,
			is_loopback: true,
		}])
	}

	async fn open_device(&mut self, _device_index: u32, format: PcmFormat) -> AudioResult<()> {
		self.format = Some(format);
		Ok(())
	}

	async fn start(&mut self) -> AudioResult<()> {
		self.active     = true;
		self.started_at = Some(Instant::now());
		Ok(())
	}

	async fn stop(&mut self) -> AudioResult<()> {
		self.active = false;
		Ok(())
	}

	async fn close(&mut self) -> AudioResult<()> {
		self.active = false;
		self.format = None;
		Ok(())
	}

	async fn next_frame(&mut self) -> AudioResult<Option<PcmFrame>> {
		if !self.active {
			return Ok(None);
		}
		let frames_per_buf: u32 = self
			.format
			.as_ref()
			.map(|f| f.sample_rate / 100)
			.unwrap_or(480);

		tokio::time::sleep(Duration::from_millis(10)).await;

		Ok(Some(self.generate_frame(frames_per_buf)))
	}

	fn is_active(&self) -> bool { self.active }

	fn actual_format(&self) -> Option<PcmFormat> { self.format.clone() }

	fn device_name(&self) -> Option<String> {
		Some(self.device_name.clone())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn mock_enumerate_returns_one_device() {
		let backend = MockAudioBackend::new();
		let devices = backend.enumerate_devices().await.unwrap();
		assert_eq!(devices.len(), 1);
		assert!(devices[0].is_loopback);
	}

	#[tokio::test]
	async fn mock_produces_frames_when_active() {
		let mut backend = MockAudioBackend::new();
		let fmt = PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 };
		backend.open_device(0, fmt).await.unwrap();
		backend.start().await.unwrap();
		assert!(backend.is_active());
		let frame = backend.next_frame().await.unwrap();
		assert!(frame.is_some());
		let f = frame.unwrap();
		assert!(!f.data.is_empty());
		assert_eq!(f.format.sample_rate, 48000);
		backend.stop().await.unwrap();
		assert!(!backend.is_active());
	}

	#[tokio::test]
	async fn mock_returns_none_when_stopped() {
		let mut backend = MockAudioBackend::new();
		let fmt = PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 };
		backend.open_device(0, fmt).await.unwrap();
		let frame = backend.next_frame().await.unwrap();
		assert!(frame.is_none());
	}
}
