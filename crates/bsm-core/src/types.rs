use serde::{Deserialize, Serialize};

/// Shared primitive types used across crates.
pub type SampleRate = u32;
pub type Channels = u16;
pub type FrameCount = u64;

/// An enumerated audio device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceEntry {
    pub index: u32,
    pub name: String,
    pub is_default: bool,
    pub is_loopback: bool,
}

/// PCM audio format descriptor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PcmFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub bit_depth: u16,
}

impl PcmFormat {
    pub fn bytes_per_sample(&self) -> u16 {
        self.bit_depth / 8
    }

    pub fn bytes_per_frame(&self) -> u32 {
        self.bytes_per_sample() as u32 * self.channels as u32
    }

    pub fn byte_rate(&self) -> u32 {
        self.bytes_per_frame() * self.sample_rate
    }
}

/// A single PCM audio buffer from the capture pipeline.
#[derive(Debug, Clone)]
pub struct PcmFrame {
    pub data: Vec<u8>,
    pub format: PcmFormat,
    pub timestamp_us: u64,
    pub sequence: u64,
    pub frame_count: u32,
}

/// Recording session summary emitted on stop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub output_file: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub stopped_at: chrono::DateTime<chrono::Utc>,
    pub duration_secs: f64,
    pub frames_captured: u64,
    pub frames_dropped: u64,
    pub bytes_written: u64,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub bitrate_kbps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InstanceId(pub String);

impl From<&str> for InstanceId {
    fn from(s: &str) -> Self {
        InstanceId(s.to_string())
    }
}

impl From<String> for InstanceId {
    fn from(s: String) -> Self {
        InstanceId(s)
    }
}
