use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::warn;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Timestamp helper
// ---------------------------------------------------------------------------

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Event payload structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioStartedPayload {
    pub device_name: String,
    pub sample_rate: u32,
    pub channels:    u16,
    pub ts_ms:       u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioStoppedPayload {
    pub duration_ms:   u64,
    pub total_frames:  u64,
    pub ts_ms:         u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioStatsPayload {
    pub rms_left:    f32,
    pub rms_right:   f32,
    pub peak_left:   f32,
    pub peak_right:  f32,
    pub clipping:    bool,
    pub queue_depth: usize,
    pub ts_ms:       u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncoderStatsPayload {
    pub bitrate_kbps:       f32,
    pub encode_time_avg_us: u64,
    pub encode_time_max_us: u64,
    pub queue_depth:        usize,
    pub packets_encoded:    u64,
    pub frames_dropped:     u64,
    pub ts_ms:              u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskStatsPayload {
    pub file_size_mb: f64,
    pub disk_free_mb: f64,
    pub output_path:  String,
    pub ts_ms:        u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPayload {
    pub code:      String,
    pub message:   String,
    pub component: String,
    pub ts_ms:     u64,
}

// ---------------------------------------------------------------------------
// Generic event envelope
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct TelemetryEnvelope {
    pub event:  String,
    pub ts_ms:  u64,
    pub data:   serde_json::Value,
}

impl TelemetryEnvelope {
    fn new(event: impl Into<String>, data: impl Serialize) -> Self {
        Self {
            event: event.into(),
            ts_ms: now_ms(),
            data:  serde_json::to_value(data).unwrap_or(serde_json::Value::Null),
        }
    }

    fn to_json(&self) -> Option<String> {
        serde_json::to_string(self).ok()
    }
}

// ---------------------------------------------------------------------------
// TelemetryEmitter
// ---------------------------------------------------------------------------

/// Wraps the broadcast::Sender<String> and exposes typed emit methods.
#[derive(Clone)]
pub struct TelemetryEmitter {
    tx: broadcast::Sender<String>,
}

impl TelemetryEmitter {
    pub fn new(tx: broadcast::Sender<String>) -> Self {
        Self { tx }
    }

    /// Subscribe to raw telemetry JSON events. Returns a `broadcast::Receiver<String>`
    /// which will receive every emitted telemetry JSON string. Callers may `recv()`
    /// or `try_recv()` on the returned receiver.
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    /// Emit a raw pre-serialized event JSON string.
    pub fn emit_raw(&self, json: String) {
        if let Err(e) = self.tx.send(json) {
            // No subscribers — this is fine (headless or no client connected)
            let _ = e; // suppress unused-result warning
        }
    }

    /// Serialize and emit a TelemetryEnvelope. Logs on serialization failure.
    fn emit(&self, env: TelemetryEnvelope) {
        if let Some(json) = env.to_json() {
            self.emit_raw(json);
        } else {
            warn!("telemetry: failed to serialize event '{}'", env.event);
        }
    }

    // -----------------------------------------------------------------------
    // Typed emit helpers
    // -----------------------------------------------------------------------

    pub fn emit_audio_started(&self, device_name: String, sample_rate: u32, channels: u16) {
        let payload = AudioStartedPayload { device_name, sample_rate, channels, ts_ms: now_ms() };
        self.emit(TelemetryEnvelope::new("audio_started", payload));
    }

    pub fn emit_audio_stopped(&self, duration_ms: u64, total_frames: u64) {
        let payload = AudioStoppedPayload { duration_ms, total_frames, ts_ms: now_ms() };
        self.emit(TelemetryEnvelope::new("audio_stopped", payload));
    }

    pub fn emit_audio_stats(
        &self,
        rms_left: f32,
        rms_right: f32,
        peak_left: f32,
        peak_right: f32,
        clipping: bool,
        queue_depth: usize,
    ) {
        let payload = AudioStatsPayload {
            rms_left,
            rms_right,
            peak_left,
            peak_right,
            clipping,
            queue_depth,
            ts_ms: now_ms(),
        };
        self.emit(TelemetryEnvelope::new("audio_stats", payload));
    }

    pub fn emit_encoder_stats(
        &self,
        bitrate_kbps: f32,
        encode_time_avg_us: u64,
        encode_time_max_us: u64,
        queue_depth: usize,
        packets_encoded: u64,
        frames_dropped: u64,
    ) {
        let payload = EncoderStatsPayload {
            bitrate_kbps,
            encode_time_avg_us,
            encode_time_max_us,
            queue_depth,
            packets_encoded,
            frames_dropped,
            ts_ms: now_ms(),
        };
        self.emit(TelemetryEnvelope::new("encoder_stats", payload));
    }

    pub fn emit_disk_stats(&self, file_size_mb: f64, disk_free_mb: f64, output_path: String) {
        let payload = DiskStatsPayload { file_size_mb, disk_free_mb, output_path, ts_ms: now_ms() };
        self.emit(TelemetryEnvelope::new("disk_stats", payload));
    }

    pub fn emit_error(&self, code: String, message: String, component: String) {
        let payload = ErrorPayload { code, message, component, ts_ms: now_ms() };
        self.emit(TelemetryEnvelope::new("error_occurred", payload));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_emitter() -> (TelemetryEmitter, broadcast::Receiver<String>) {
        let (tx, rx) = broadcast::channel(16);
        (TelemetryEmitter::new(tx), rx)
    }

    #[test]
    fn audio_started_event_is_valid_json() {
        let (emitter, mut rx) = make_emitter();
        emitter.emit_audio_started("Stereo Mix".into(), 48000, 2);
        let msg = rx.try_recv().unwrap();
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["event"], "audio_started");
        assert_eq!(v["data"]["sample_rate"], 48000);
        assert_eq!(v["data"]["channels"], 2);
    }

    #[test]
    fn audio_stopped_event_fields_present() {
        let (emitter, mut rx) = make_emitter();
        emitter.emit_audio_stopped(30_000, 1_440_000);
        let msg = rx.try_recv().unwrap();
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["event"], "audio_stopped");
        assert!(v["data"]["duration_ms"].is_number());
    }

    #[test]
    fn error_event_contains_component() {
        let (emitter, mut rx) = make_emitter();
        emitter.emit_error("WASAPI_ERR".into(), "device lost".into(), "bsm-audio".into());
        let msg = rx.try_recv().unwrap();
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["event"], "error_occurred");
        assert_eq!(v["data"]["component"], "bsm-audio");
    }

    #[test]
    fn emit_with_no_subscribers_does_not_panic() {
        let (tx, rx) = broadcast::channel::<String>(4);
        drop(rx);
        let emitter = TelemetryEmitter::new(tx);
        emitter.emit_audio_started("test".into(), 48000, 2);
        // No panic expected
    }
}
