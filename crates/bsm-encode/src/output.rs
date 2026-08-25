use crate::muxer::ContainerFormat;
use bsm_core::{EncodeError, EncodeResult};
use bsm_core::config::OutputConfig;
use std::path::{PathBuf, Path};
use chrono::{Local, Utc};
use tracing::{info, error};
use crate::probe;
use crate::encoder;
use crate::muxer;
use tokio::task::JoinHandle;
use tokio::sync::{mpsc, broadcast};

/// Expand a filename template (no extension) using tokens.
pub fn expand_filename_template(template: &str, n: u32) -> String {
    let now = Local::now();
    let date = now.format("%Y-%m-%d").to_string();
    let time = now.format("%H-%M-%S").to_string();
    let agent = std::env::var("BSM_AGENT_ID").unwrap_or_else(|_| "local".into());
    let nstr = format!("{:02}", n);
    template.replace("{date}", &date).replace("{time}", &time).replace("{app}", "BSM").replace("{agent_id}", &agent).replace("{n}", &nstr)
}

/// Check disk free space; requires `fs2` crate available in Cargo.toml.
pub fn check_disk_space(dir: &Path) -> EncodeResult<u64> {
    use fs2::available_space;
    let free = available_space(dir).map_err(|e| EncodeError::OutputFile(format!("disk query failed: {}", e)))?;
    const MIN_FREE: u64 = 512 * 1024 * 1024;
    if free < MIN_FREE { return Err(EncodeError::OutputFile(format!("only {} MiB free", free / (1024*1024)))); }
    Ok(free)
}

/// Resolve a unique output path using `OutputConfig` and `ContainerFormat`.
pub fn resolve_output_path(config: &OutputConfig, format: ContainerFormat) -> EncodeResult<PathBuf> {
    let dir = PathBuf::from(&config.output_folder);
    std::fs::create_dir_all(&dir).map_err(|e| EncodeError::OutputFile(e.to_string()))?;
    let ext = format.extension();
    for n in 1u32..=9999 {
        let stem = expand_filename_template(&config.file_name_pattern, n);
        let p = dir.join(format!("{}.{}", stem, ext));
        if !p.exists() { return Ok(p); }
    }
    Err(EncodeError::OutputFile("could not find unique name".into()))
}

/// RecordingSession coordinates encoder + muxer (higher-level wrapper).
pub struct RecordingSession {
    pub output_path: PathBuf,
    pub encoder_name: String,
    pub encoder_config: bsm_core::config::EncoderConfig,
    pub input_format: bsm_core::PcmFormat,
    pub container_format: ContainerFormat,
}

/// Result summary returned by the mux task.
pub struct MuxResult {
    pub frames_written: u64,
    pub frames_dropped: u64,
    pub bytes_written: u64,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub stopped_at: chrono::DateTime<chrono::Utc>,
}

pub struct SessionHandle {
    pub mux_task: JoinHandle<Result<MuxResult, EncodeError>>,
    pub encoder_task: JoinHandle<()>,
    pub shutdown_tx: broadcast::Sender<()>,
    pub output_file: PathBuf,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
}

impl RecordingSession {
    pub fn new(output_path: PathBuf, encoder_name: String, encoder_config: bsm_core::config::EncoderConfig, input_format: bsm_core::PcmFormat, container_format: ContainerFormat) -> Self {
        Self { output_path, encoder_name, encoder_config, input_format, container_format }
    }

    pub async fn start(self, pcm_rx: mpsc::Receiver<bsm_core::PcmFrame>, mut external_shutdown_rx: broadcast::Receiver<()>) -> EncodeResult<SessionHandle> {
        info!("RecordingSession.start — output: {:?}", self.output_path);

        // Check disk space before opening
        if let Some(dir) = self.output_path.parent() {
            let _ = check_disk_space(dir)?;
        }

        // select encoder
        let enc_info = probe::select_encoder(Some(&self.encoder_name)).ok_or_else(|| EncodeError::NotFound(format!("encoder {} not available", self.encoder_name)))?;

        // internal shutdown channel for the session
        let (session_shutdown_tx, _) = broadcast::channel::<()>(4);

        // start encoder
        let (encoder_handle, encoder_task) = encoder::start_encoder(pcm_rx, enc_info.clone(), self.encoder_config.clone(), self.input_format.clone(), session_shutdown_tx.subscribe()).await?;

        // create and open muxer
        let mut mux = muxer::create_muxer(self.container_format);
        // default bitrate for sessions is 128 kbps unless caller specifies otherwise
        mux.open(&self.output_path, &self.input_format, &enc_info.name, 128)?;

        // spawn mux task
        let mut packet_rx = encoder_handle.packet_rx;
        let mut mux2 = mux;
        let mut shutdown_for_task = session_shutdown_tx.subscribe();

        // forward external shutdown into session shutdown
        let forward_tx = session_shutdown_tx.clone();
        tokio::spawn(async move {
            let _ = external_shutdown_rx.recv().await;
            let _ = forward_tx.send(());
        });

        let out_path = self.output_path.clone();
        let codec = enc_info.name.clone();
        let sr = self.input_format.sample_rate;
        let ch = self.input_format.channels;

        let mux_task = tokio::spawn(async move {
            let started = Utc::now();
            let mut frames_written: u64 = 0;
            let mut bytes_written: u64 = 0;
            let mut frames_dropped: u64 = 0;

            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_for_task.recv() => { break; }
                    Some(pkt) = packet_rx.recv() => {
                        match mux2.write_packet(&pkt) {
                            Ok(()) => { frames_written += 1; bytes_written += pkt.data.len() as u64; }
                            Err(e) => { error!("mux write failed: {:?}", e); frames_dropped += 1; break; }
                        }
                    }
                    else => { break; }
                }
            }

            if let Err(e) = mux2.finalize() {
                error!("mux finalize failed: {:?}", e);
                return Err(e);
            }

            let stopped = Utc::now();
            Ok(MuxResult { frames_written, frames_dropped, bytes_written, started_at: started, stopped_at: stopped })
        });

        Ok(SessionHandle { mux_task, encoder_task, shutdown_tx: session_shutdown_tx, output_file: out_path, codec, sample_rate: sr, channels: ch })
    }
}

impl SessionHandle {
    /// Gracefully stop the session and return a `SessionInfo` summary.
    pub async fn stop(self) -> Result<bsm_core::SessionInfo, EncodeError> {
        let _ = self.shutdown_tx.send(());

        // wait for encoder task
        let _ = self.encoder_task.await;

        // get mux result
        let mux_res = match self.mux_task.await {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => return Err(e),
            Err(join_err) => return Err(EncodeError::MuxFailed(format!("mux task join error: {}", join_err))),
        };

        let duration = (mux_res.stopped_at - mux_res.started_at).num_seconds() as f64 + (mux_res.stopped_at - mux_res.started_at).num_nanoseconds().unwrap_or(0) as f64 / 1e9;

        let info = bsm_core::SessionInfo {
            session_id: "session".into(),
            output_file: self.output_file.to_string_lossy().into_owned(),
            started_at: mux_res.started_at,
            stopped_at: mux_res.stopped_at,
            duration_secs: duration,
            frames_captured: mux_res.frames_written,
            frames_dropped: mux_res.frames_dropped,
            bytes_written: mux_res.bytes_written,
            codec: self.codec,
            sample_rate: self.sample_rate,
            channels: self.channels,
            bitrate_kbps: 0,
        };

        Ok(info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use crate::muxer::ContainerFormat;

    #[test]
    fn expand_template_has_tokens() {
        let s = expand_filename_template("BSM_{date}_{time}_{n}", 3);
        assert!(s.contains("BSM_"));
        assert!(s.ends_with("_03"));
    }

    #[test]
    fn resolve_output_path_creates_dir() {
        let tmp = TempDir::new().unwrap();
        let cfg = OutputConfig { output_folder: tmp.path().to_string_lossy().into_owned(), file_name_pattern: "BSM_{date}_{time}_{n}".into(), ..Default::default() };
        let p = resolve_output_path(&cfg, ContainerFormat::Mp3).unwrap();
        assert!(p.to_string_lossy().ends_with(".mp3"));
    }

    #[test]
    fn recording_session_integration() {
        use tokio::runtime::Runtime;
        use tokio::sync::{mpsc, broadcast};

        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let tmp = TempDir::new().unwrap();
            let out = tmp.path().join("rec_test.wav");

            let session = RecordingSession::new(
                out.clone(),
                "libopus".to_string(),
                bsm_core::config::EncoderConfig::default(),
                bsm_core::PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 },
                ContainerFormat::Wav,
            );

            let (tx, rx) = mpsc::channel::<bsm_core::PcmFrame>(8);
            let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(4);

            let handle = session.start(rx, shutdown_rx).await.unwrap();

            // send a few frames
            for _ in 0..4 {
                let frame = bsm_core::PcmFrame { data: vec![0u8; 480 * 2 * 2], format: bsm_core::PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 }, timestamp_us: 0, sequence: 0, frame_count: 480 };
                let _ = tx.send(frame).await;
            }

            // allow some time for processing
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;

            // stop and get session info
            let info = handle.stop().await.unwrap();
            assert!(info.bytes_written > 0);

            // output file should exist and be non-empty
            let bytes = std::fs::read(&out).unwrap();
            assert!(!bytes.is_empty());

            // RIFF header: size at offset 4, data chunk size at offset 40
            let riff_sz = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
            let data_sz = u32::from_le_bytes(bytes[40..44].try_into().unwrap());
            assert_eq!(riff_sz, 36u32 + data_sz);
            assert_eq!(bytes.len() as u64, 44u64 + data_sz as u64);

            // verify data size matches raw PCM: 4 frames × 480 samples × 2 ch × 2 bytes
            let frame_payload_len = 480 * 2 * 2; // samples * channels * bytes_per_sample
            let expected_data_sz = frame_payload_len * 4;
            assert_eq!(data_sz as usize, expected_data_sz);

            let _ = shutdown_tx.send(());
        });
    }
}
