use crate::commands::{IpcCommand, ResponseEnvelope};
use crate::telemetry::TelemetryEmitter;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, broadcast, Notify};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration as StdDuration;
use bsm_core::{PcmFormat, PcmFrame};
use bsm_audio::WasapiBackend;
use bsm_audio::pipeline::{CapturePipeline, DeviceConfig};
use bsm_encode::{start_encoder, AudioEncoderInfo};
use bsm_core::config::EncoderConfig;

/// Seed-BSM-G3-03-11: Shared audio quality metrics updated by the pipeline
/// task and read by the periodic stats emitter and Stats command handler.
#[derive(Debug, Default, Clone)]
pub struct AudioQualityState {
    pub peak_left:        f32,
    pub peak_right:       f32,
    pub underrun_count:   u64,
    pub sample_rate:      u32,
    pub samples_captured: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingState {
    Idle,
    Recording { device_index: Option<u32>, started_at: u64 },
    Paused { device_index: Option<u32>, started_at: u64 },
}

pub struct Dispatcher {
    state: RecordingState,
    telemetry: TelemetryEmitter,
    // Background runtime and task handles
    rt: Option<tokio::runtime::Runtime>,
    pcm_tx: Option<mpsc::Sender<PcmFrame>>,
    shutdown_tx: Option<broadcast::Sender<()>>,
    // Used to coordinate shutdown completion and allow awaiting
    active_tasks: Option<Arc<AtomicUsize>>,
    shutdown_notify: Option<Arc<Notify>>,
    // Seed-BSM-G3-01-11: process start time for uptime reporting
    start_time: Instant,
    // Seed-BSM-G3-03-11: shared audio quality metrics
    audio_quality: Arc<std::sync::Mutex<AudioQualityState>>,
}

impl Dispatcher {
    pub fn new(telemetry: TelemetryEmitter) -> Self {
        Self {
            state: RecordingState::Idle,
            telemetry,
            rt: None,
            pcm_tx: None,
            shutdown_tx: None,
            active_tasks: None,
            shutdown_notify: None,
            start_time: Instant::now(),
            audio_quality: Arc::new(std::sync::Mutex::new(AudioQualityState::default())),
        }
    }

    fn ensure_runtime(&mut self) {
        if self.rt.is_none() {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_io()
                .enable_time()
                .build()
                .expect("failed to build runtime");
            self.rt = Some(rt);
        }
    }

    fn spawn_background_tasks(&mut self, _device_index: Option<u32>) {
        self.ensure_runtime();
        let rt = self.rt.as_ref().unwrap();

        // Create channels for PCM frames and shutdown
        let (pcm_tx, pcm_rx) = mpsc::channel::<PcmFrame>(256);
        let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(4);

        // Track active background tasks and notify when all complete
        let active = Arc::new(AtomicUsize::new(4)); // pipeline + encoder + disk + audio-stats
        let notify = Arc::new(Notify::new());

        // Shared audio quality state (updated by pipeline task, read by stats task).
        let aq = self.audio_quality.clone();

        // Pipeline task: runs CapturePipeline with WasapiBackend and forwards frames into pcm_tx
        let mut pipeline = CapturePipeline::new(WasapiBackend::new(), PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 });

        let mut pipeline_shutdown = shutdown_rx.resubscribe();
        let pcm_tx_clone = pcm_tx.clone();
        let active_clone = active.clone();
        let notify_clone = notify.clone();
        let aq_pipeline = aq.clone();

        let _pipeline_task = rt.handle().spawn(async move {
            // open device (mock uses index 0)
            let _ = pipeline.open_with_config(0, DeviceConfig::with_format(PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 })).await;
            let _ = pipeline.start().await;

            loop {
                tokio::select! {
                    _ = pipeline_shutdown.recv() => {
                        let _ = pipeline.stop().await;
                        break;
                    }
                    frame = pipeline.next_frame() => {
                        match frame {
                            Ok(Some(f)) => {
                                // Seed-BSM-G3-03-11: compute audio quality metrics from PCM.
                                if f.format.bit_depth == 16 && f.data.len() >= 2 {
                                    let samples: Vec<i16> = f.data.chunks_exact(2)
                                        .map(|b| i16::from_le_bytes([b[0], b[1]]))
                                        .collect();
                                    let ch = f.format.channels.max(1) as usize;
                                    let mut peak_l = 0.0f32;
                                    let mut peak_r = 0.0f32;
                                    for (i, s) in samples.iter().enumerate() {
                                        let v = (*s as f32).abs() / i16::MAX as f32;
                                        if i % ch == 0 { peak_l = peak_l.max(v); }
                                        else if i % ch == 1 { peak_r = peak_r.max(v); }
                                    }
                                    if let Ok(mut aq) = aq_pipeline.lock() {
                                        aq.peak_left = peak_l;
                                        aq.peak_right = peak_r;
                                        aq.sample_rate = f.format.sample_rate;
                                        aq.samples_captured += f.frame_count as u64;
                                    }
                                } else if f.data.is_empty() {
                                    if let Ok(mut aq) = aq_pipeline.lock() { aq.underrun_count += 1; }
                                }
                                let _ = pcm_tx_clone.send(f).await;
                            }
                            Ok(None) => break,
                            Err(_) => {
                                if let Ok(mut aq) = aq_pipeline.lock() { aq.underrun_count += 1; }
                                break;
                            },
                        }
                    }
                }
            }
            // mark task done
            if active_clone.fetch_sub(1, Ordering::SeqCst) == 1 {
                notify_clone.notify_waiters();
            }
        });

        // Encoder task: start encoder and forward stats to telemetry
        let encoder_shutdown_for_start = shutdown_rx.resubscribe();
        let mut encoder_shutdown_for_loop = shutdown_rx.resubscribe();
        let telemetry_clone = self.telemetry.clone();
        let active_clone = active.clone();
        let notify_clone = notify.clone();
        let _encoder_task = rt.handle().spawn(async move {
            // choose fake encoder info and default config
            let info = AudioEncoderInfo::new("libopus");
            let cfg = EncoderConfig::default();
            let input_fmt = PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 };
            if let Ok((handle, _jh)) = start_encoder(pcm_rx, info, cfg, input_fmt, encoder_shutdown_for_start).await {
                let mut stats_rx = handle.stats_rx;
                loop {
                    tokio::select! {
                        Ok(stats) = stats_rx.recv() => {
                            telemetry_clone.emit_encoder_stats(
                                stats.bitrate_actual_kbps,
                                stats.encode_time_avg_us,
                                stats.encode_time_max_us,
                                stats.queue_depth,
                                stats.packets_encoded,
                                stats.frames_dropped,
                            );
                        }
                        _ = encoder_shutdown_for_loop.recv() => {
                            break;
                        }
                    }
                }
            }
            // mark task done
            if active_clone.fetch_sub(1, Ordering::SeqCst) == 1 {
                notify_clone.notify_waiters();
            }
        });

        // Disk telemetry task: periodic (1s) emitter
        let disk_telemetry = self.telemetry.clone();
        let mut disk_shutdown = shutdown_rx.resubscribe();
        let active_clone = active.clone();
        let notify_clone = notify.clone();
        let _disk_task = rt.handle().spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = disk_shutdown.recv() => { break; }
                    _ = interval.tick() => {
                        // Fake disk stats for now
                        disk_telemetry.emit_disk_stats(1.23, 1024.0, "./output.wav".into());
                    }
                }
            }
            // mark task done
            if active_clone.fetch_sub(1, Ordering::SeqCst) == 1 {
                notify_clone.notify_waiters();
            }
        });

        // Seed-BSM-G3-01-11: audio quality stats emitter — fires every 5 seconds.
        let audio_stats_telemetry = self.telemetry.clone();
        let mut audio_stats_shutdown = shutdown_rx.resubscribe();
        let active_clone = active.clone();
        let notify_clone = notify.clone();
        let aq_stats = aq.clone();
        let _audio_stats_task = rt.handle().spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                tokio::select! {
                    _ = audio_stats_shutdown.recv() => { break; }
                    _ = interval.tick() => {
                        let (pl, pr, _underruns, _sr, qdepth) = {
                            let s = aq_stats.lock().unwrap();
                            (s.peak_left, s.peak_right, s.underrun_count, s.sample_rate, 0usize)
                        };
                        audio_stats_telemetry.emit_audio_stats(
                            pl * 0.707, // approx RMS from peak
                            pr * 0.707,
                            pl,
                            pr,
                            pl >= 0.99 || pr >= 0.99, // clipping
                            qdepth,
                        );
                    }
                }
            }
            // mark task done
            if active_clone.fetch_sub(1, Ordering::SeqCst) == 1 {
                notify_clone.notify_waiters();
            }
        });

        self.pcm_tx = Some(pcm_tx);
        self.shutdown_tx = Some(shutdown_tx);
        self.active_tasks = Some(active);
        self.shutdown_notify = Some(notify);
        // Note: JoinHandles are intentionally detached — shutdown managed via channels
    }

    /// Synchronously shutdown background tasks; intended for blocking contexts.
    pub fn shutdown_blocking(&mut self) {
        if let Some(tx) = &self.shutdown_tx {
            let _ = tx.send(());
        }

        if let Some(active) = &self.active_tasks {
            while active.load(Ordering::SeqCst) > 0 {
                std::thread::sleep(StdDuration::from_millis(20));
            }
        }

        // Drop the runtime to allow resources to be cleaned up
        let _ = self.rt.take();
        self.pcm_tx = None;
        self.shutdown_tx = None;
        self.active_tasks = None;
        self.shutdown_notify = None;
    }

    /// Blocking shutdown with optional timeout. Returns `true` if completed
    /// within the timeout, `false` if the timeout elapsed.
    pub fn shutdown_blocking_with_timeout(&mut self, max_wait: Option<StdDuration>) -> bool {
        if let Some(tx) = &self.shutdown_tx {
            let _ = tx.send(());
        }

        let mut completed = true;
        if let Some(active) = &self.active_tasks {
            if active.load(Ordering::SeqCst) > 0 {
                if let Some(max) = max_wait {
                    let deadline = std::time::Instant::now() + max;
                    while active.load(Ordering::SeqCst) > 0 {
                        if std::time::Instant::now() >= deadline { completed = false; break; }
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                } else {
                    while active.load(Ordering::SeqCst) > 0 {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                }
            }
        }

        let _ = self.rt.take();
        self.pcm_tx = None;
        self.shutdown_tx = None;
        self.active_tasks = None;
        self.shutdown_notify = None;
        completed
    }

    /// Async shutdown that can be awaited from an async handler.
    pub async fn shutdown(&mut self) {
        let _ = self.shutdown_with_timeout(None).await;
    }

    /// Async shutdown with an optional timeout. Returns `true` if all background
    /// tasks completed within the timeout, `false` if the timeout elapsed.
    pub async fn shutdown_with_timeout(&mut self, max_wait: Option<StdDuration>) -> bool {
        if let Some(tx) = &self.shutdown_tx {
            let _ = tx.send(());
        }

        let mut completed = true;
        if let Some(active) = &self.active_tasks {
            if active.load(Ordering::SeqCst) > 0 {
                if let Some(notify) = &self.shutdown_notify {
                    if let Some(max) = max_wait {
                        // Wait with timeout using tokio notify; compute deadline
                        let deadline = tokio::time::Instant::now() + max;
                        loop {
                            if active.load(Ordering::SeqCst) == 0 {
                                break;
                            }
                            let now = tokio::time::Instant::now();
                            if now >= deadline {
                                completed = false;
                                break;
                            }
                            let remaining = deadline - now;
                            let res = tokio::time::timeout(remaining, notify.notified()).await;
                            match res {
                                Ok(_) => {
                                    if active.load(Ordering::SeqCst) == 0 { break; }
                                    // loop and check again until either active==0 or deadline exceeded
                                }
                                Err(_) => { completed = false; break; }
                            }
                        }
                    } else {
                        // No timeout: await until active reaches zero
                        while active.load(Ordering::SeqCst) > 0 {
                            notify.notified().await;
                        }
                    }
                } else {
                    // No notify available: fallback to polling sleep
                    if let Some(max) = max_wait {
                        let deadline = tokio::time::Instant::now() + max;
                        while active.load(Ordering::SeqCst) > 0 {
                            if tokio::time::Instant::now() >= deadline { completed = false; break; }
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        }
                    } else {
                        while active.load(Ordering::SeqCst) > 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        }
                    }
                }
            }
        }

        // Drop the runtime and clear state
        let _ = self.rt.take();
        self.pcm_tx = None;
        self.shutdown_tx = None;
        self.active_tasks = None;
        self.shutdown_notify = None;
        completed
    }

    /// Handle a single `IpcCommand` and return a response envelope.
    pub fn handle_command(&mut self, cmd: IpcCommand, id: impl Into<String>) -> ResponseEnvelope {
        let id = id.into();
        match cmd {
            IpcCommand::StartRecording { device_index } => {
                self.spawn_background_tasks(device_index);
                self.state = RecordingState::Recording { device_index, started_at: now_ms() };
                let device_name = match device_index { Some(i) => format!("device-{}", i), None => "default-device".to_string() };
                self.telemetry.emit_audio_started(device_name, 48000, 2);
                ResponseEnvelope::ok(id)
            }

            IpcCommand::StopRecording => {
                // signal shutdown to background tasks
                if let Some(tx) = &self.shutdown_tx {
                    let _ = tx.send(());
                }
                let duration_ms = match &self.state {
                    RecordingState::Recording { started_at, .. } | RecordingState::Paused { started_at, .. } => now_ms().saturating_sub(*started_at),
                    _ => 0,
                };
                self.state = RecordingState::Idle;
                self.telemetry.emit_audio_stopped(duration_ms, 0);
                ResponseEnvelope::ok(id)
            }

            IpcCommand::PauseRecording => {
                match &self.state {
                    RecordingState::Recording { device_index, started_at } => {
                        self.state = RecordingState::Paused { device_index: *device_index, started_at: *started_at };
                        ResponseEnvelope::ok(id)
                    }
                    _ => ResponseEnvelope::err(id, "not_recording"),
                }
            }

            IpcCommand::ResumeRecording => {
                match &self.state {
                    RecordingState::Paused { device_index, started_at } => {
                        self.state = RecordingState::Recording { device_index: *device_index, started_at: *started_at };
                        ResponseEnvelope::ok(id)
                    }
                    _ => ResponseEnvelope::err(id, "not_paused"),
                }
            }

            IpcCommand::GetStatus => {
                let data = match &self.state {
                    RecordingState::Idle => serde_json::json!({"state":"idle"}),
                    RecordingState::Recording { device_index, started_at } => serde_json::json!({"state":"recording","device_index":device_index,"started_at":started_at}),
                    RecordingState::Paused { device_index, started_at } => serde_json::json!({"state":"paused","device_index":device_index,"started_at":started_at}),
                };
                ResponseEnvelope::ok_data(id, data)
            }

            IpcCommand::SetDevice { device_index } => {
                let _ = device_index;
                ResponseEnvelope::ok(id)
            }

            IpcCommand::UpdateConfig { patch: _ } => {
                ResponseEnvelope::ok(id)
            }

            IpcCommand::Shutdown => {
                if let Some(tx) = &self.shutdown_tx { let _ = tx.send(()); }
                self.telemetry.emit_error("shutdown".into(), "shutdown requested".into(), "bsm-ipc".into());
                ResponseEnvelope::ok(id)
            }

            // Seed-BSM-G3-02-11: health probe
            IpcCommand::Health => {
                let uptime_ms = self.start_time.elapsed().as_millis() as u64;
                let recording = matches!(&self.state, RecordingState::Recording { .. });
                let data = serde_json::json!({
                    "status": "ok",
                    "uptime_ms": uptime_ms,
                    "recording_active": recording,
                    "state": match &self.state {
                        RecordingState::Idle => "idle",
                        RecordingState::Recording { .. } => "recording",
                        RecordingState::Paused { .. } => "paused",
                    },
                });
                ResponseEnvelope::ok_data(id, data)
            }

            // Seed-BSM-G3-02-11: audio quality stats
            IpcCommand::Stats => {
                let (peak_l, peak_r, underruns, sr, samples) = {
                    let aq = self.audio_quality.lock().unwrap();
                    (aq.peak_left, aq.peak_right, aq.underrun_count, aq.sample_rate, aq.samples_captured)
                };
                let data = serde_json::json!({
                    "peak_amplitude": (peak_l + peak_r) / 2.0_f32,
                    "peak_left": peak_l,
                    "peak_right": peak_r,
                    "buffer_underruns": underruns,
                    "sample_rate": sr,
                    "samples_captured": samples,
                });
                ResponseEnvelope::ok_data(id, data)
            }

            _ => {
                // Unsupported command for dispatcher-level handling.
                ResponseEnvelope::err(id, "unsupported_command")
            }
        }
    }

    pub fn state(&self) -> &RecordingState { &self.state }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::TelemetryEmitter;
    use tokio::sync::broadcast;

    #[test]
    fn start_stop_flow_emits_telemetry_and_changes_state() {
        let (tx, mut rx) = broadcast::channel(8);
        let emitter = TelemetryEmitter::new(tx.clone());
        let mut d = Dispatcher::new(emitter.clone());

        // Start recording
        let resp = d.handle_command(IpcCommand::StartRecording { device_index: Some(1) }, "id1");
        assert!(resp.ok);
        assert!(matches!(d.state(), RecordingState::Recording { .. }));
        // receive telemetry
        let msg = rx.try_recv().unwrap();
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["event"], "audio_started");

        // Get status
        let resp2 = d.handle_command(IpcCommand::GetStatus, "id2");
        assert!(resp2.ok);
        assert!(resp2.data.is_some());

        // Stop recording
        let resp3 = d.handle_command(IpcCommand::StopRecording, "id3");
        assert!(resp3.ok);
        assert!(matches!(d.state(), RecordingState::Idle));
        let msg2 = rx.try_recv().unwrap();
        let v2: serde_json::Value = serde_json::from_str(&msg2).unwrap();
        assert_eq!(v2["event"], "audio_stopped");
    }

    #[test]
    fn pause_resume_transitions() {
        let (tx, _rx) = broadcast::channel(4);
        let emitter = TelemetryEmitter::new(tx);
        let mut d = Dispatcher::new(emitter);

        let _ = d.handle_command(IpcCommand::StartRecording { device_index: None }, "s1");
        assert!(matches!(d.state(), RecordingState::Recording { .. }));
        let p = d.handle_command(IpcCommand::PauseRecording, "p1");
        assert!(p.ok);
        assert!(matches!(d.state(), RecordingState::Paused { .. }));
        let r = d.handle_command(IpcCommand::ResumeRecording, "r1");
        assert!(r.ok);
        assert!(matches!(d.state(), RecordingState::Recording { .. }));
    }
}
