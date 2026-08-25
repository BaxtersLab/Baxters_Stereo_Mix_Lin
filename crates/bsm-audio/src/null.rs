// bsm-audio — NullBackend
//
// Seed-BSM-G1-03-11: Fallback audio backend that produces silence (zero-filled
// PCM frames). Used when WASAPI initialisation fails so that BSM keeps running
// (UI, HRT monitoring, and telemetry still function) even without audio
// hardware access.

use async_trait::async_trait;
use bsm_core::{AudioResult, DeviceEntry, PcmFormat, PcmFrame};
use crate::backend::AudioBackend;
use std::time::Instant;

/// Silence-producing fallback backend.
///
/// Every `next_frame()` call returns a 10 ms zero-filled frame. A 10 ms sleep
/// is injected to avoid busy-looping the async executor.
pub struct NullBackend {
    active: bool,
    format: Option<PcmFormat>,
    started_at: Option<Instant>,
    sequence: u64,
}

impl NullBackend {
    pub fn new() -> Self {
        Self {
            active: false,
            format: None,
            started_at: None,
            sequence: 0,
        }
    }
}

impl Default for NullBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AudioBackend for NullBackend {
    async fn enumerate_devices(&self) -> AudioResult<Vec<DeviceEntry>> {
        Ok(vec![DeviceEntry {
            index: 0,
            name: "Null (Silence)".into(),
            is_default: false,
            is_loopback: false,
        }])
    }

    async fn open_device(&mut self, _device_index: u32, format: PcmFormat) -> AudioResult<()> {
        self.format = Some(format);
        Ok(())
    }

    async fn start(&mut self) -> AudioResult<()> {
        self.active = true;
        self.started_at = Some(Instant::now());
        tracing::info!("NullBackend: producing silence (WASAPI unavailable)");
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

        let fmt = self.format.clone().unwrap_or_else(|| PcmFormat {
            sample_rate: 48000,
            channels: 2,
            bit_depth: 16,
        });

        // 10 ms of silence per frame.
        let frame_count: u32 = fmt.sample_rate / 100;
        let data = vec![0u8; frame_count as usize * fmt.bytes_per_frame() as usize];
        let timestamp_us = self
            .started_at
            .map(|s| s.elapsed().as_micros() as u64)
            .unwrap_or(0);
        let sequence = self.sequence;
        self.sequence += 1;

        // Yield 10 ms to avoid busy-looping.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        Ok(Some(PcmFrame {
            data,
            format: fmt,
            timestamp_us,
            sequence,
            frame_count,
        }))
    }

    fn is_active(&self) -> bool {
        self.active
    }

    fn actual_format(&self) -> Option<PcmFormat> {
        self.format.clone()
    }

    fn device_name(&self) -> Option<String> {
        Some("Null (Silence)".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn null_backend_produces_silence() {
        let mut b = NullBackend::new();
        let fmt = PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 };
        b.open_device(0, fmt.clone()).await.unwrap();
        b.start().await.unwrap();

        let frame = b.next_frame().await.unwrap().unwrap();
        assert!(frame.data.iter().all(|&b| b == 0), "all bytes must be zero");
        assert!(frame.frame_count > 0);
    }

    #[tokio::test]
    async fn null_backend_stopped_returns_none() {
        let mut b = NullBackend::new();
        let frame = b.next_frame().await.unwrap();
        assert!(frame.is_none(), "stopped backend must return None");
    }
}
