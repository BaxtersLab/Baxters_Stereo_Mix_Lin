use tokio::sync::{mpsc, broadcast};
use tracing::debug;
use std::time::Instant;

use crate::probe::AudioEncoderInfo;
use bsm_core::{PcmFrame, PcmFormat, EncodeError};
use bsm_core::config::EncoderConfig;

/// Encoded audio packet for muxing.
#[derive(Debug, Clone)]
pub struct AudioPacket {
    pub data: Vec<u8>,
    pub pts: i64,
    pub duration: i64,
    pub is_key: bool,
}

/// Runtime encoder statistics emitted periodically.
#[derive(Debug, Clone)]
pub struct EncoderStats {
    pub encode_time_avg_us: u64,
    pub encode_time_max_us: u64,
    pub bitrate_actual_kbps: f32,
    pub queue_depth: usize,
    pub packets_encoded: u64,
    pub frames_dropped: u64,
}

/// Handle returned when the encoder is started.
pub struct EncoderHandle {
    pub packet_rx: mpsc::Receiver<AudioPacket>,
    pub stats_rx:  broadcast::Receiver<EncoderStats>,
}

/// Start the encoder which consumes `PcmFrame`s and emits `AudioPacket`s.
///
/// The actual encoding (MP3/FLAC) is performed by the muxer.  This task
/// simply re-packages PCM frames into AudioPackets and collects stats.
pub async fn start_encoder(
    mut pcm_rx: mpsc::Receiver<PcmFrame>,
    _encoder_info: AudioEncoderInfo,
    _config: EncoderConfig,
    _input_format: PcmFormat,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> Result<(EncoderHandle, tokio::task::JoinHandle<()>), EncodeError> {
    let (packet_tx, packet_rx) = mpsc::channel::<AudioPacket>(256);
    let (stats_tx, stats_rx)   = broadcast::channel::<EncoderStats>(16);

    let handle = tokio::spawn(async move {
        let mut encode_times: Vec<u64> = Vec::new();
        let mut packets_encoded: u64 = 0;
        let mut frames_dropped: u64 = 0;
        let mut bytes_out: u64 = 0;
        let mut interval_start = Instant::now();

        loop {
            let frame = tokio::select! {
                biased;
                _ = shutdown_rx.recv() => {
                    debug!("encoder: shutdown requested");
                    break;
                }
                maybe = pcm_rx.recv() => {
                    match maybe {
                        Some(f) => f,
                        None => {
                            debug!("encoder: pcm channel closed");
                            break;
                        }
                    }
                }
            };

            let t0 = Instant::now();

            // Pass raw PCM through — the muxer handles format-specific encoding.
            let pkt = AudioPacket {
                data: frame.data.clone(),
                pts: frame.timestamp_us as i64,
                duration: frame.frame_count as i64,
                is_key: true,
            };

            bytes_out += pkt.data.len() as u64;
            packets_encoded += 1;

            if packet_tx.send(pkt).await.is_err() {
                debug!("encoder: packet receiver closed");
                break;
            }

            let elapsed_us = t0.elapsed().as_micros() as u64;
            encode_times.push(elapsed_us);

            if interval_start.elapsed().as_secs_f64() >= 1.0 {
                let avg = if encode_times.is_empty() { 0 } else { encode_times.iter().sum::<u64>() / encode_times.len() as u64 };
                let max = encode_times.iter().copied().max().unwrap_or(0);
                let elapsed = interval_start.elapsed().as_secs_f64();
                let bitrate = if elapsed > 0.0 { (bytes_out * 8) as f32 / 1000.0 / elapsed as f32 } else { 0.0 };

                let stats = EncoderStats {
                    encode_time_avg_us: avg,
                    encode_time_max_us: max,
                    bitrate_actual_kbps: bitrate,
                    queue_depth: pcm_rx.len(),
                    packets_encoded,
                    frames_dropped,
                };

                let _ = stats_tx.send(stats);
                encode_times.clear();
                packets_encoded = 0;
                frames_dropped = 0;
                bytes_out = 0;
                interval_start = Instant::now();
            }
        }

        debug!("encoder: exiting encode task");
    });

    Ok((EncoderHandle { packet_rx, stats_rx }, handle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::runtime::Runtime;
    use bsm_core::PcmFrame;

    #[test]
    fn start_encoder_smoke() {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let (tx, rx) = mpsc::channel::<PcmFrame>(8);
            let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(4);
            let info = AudioEncoderInfo::new("mp3");
            let cfg = EncoderConfig::default();
            let input = PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 };
            let (_handle, _jh) = start_encoder(rx, info, cfg, input, shutdown_rx).await.unwrap();
            let frame = PcmFrame { data: vec![0u8; 480 * 2 * 2], format: PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 }, timestamp_us: 0, sequence: 0, frame_count: 480 };
            let _ = tx.send(frame).await;
            let _ = shutdown_tx.send(());
            drop(tx);
        });
    }
}
