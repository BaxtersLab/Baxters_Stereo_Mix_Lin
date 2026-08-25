//! Capstone (`BSM_USE_HW=1`, **Linux**): the WHOLE app path —
//! PulseAudio/PipeWire **monitor** capture of real system audio → RecordingSession
//! (encoder → WAV muxer) → file → decode → confirm the file contains the captured
//! audio, not silence. Proves "records system audio to a valid file" end to end on
//! Linux.
//!
//! This is the Linux counterpart of `loopback_record.rs`, which is `#![cfg(windows)]`
//! and drives playback through PowerShell's SoundPlayer — so it can never run here.
//! Written 2026-08-01 during the Ubuntu 26.04 intake, where `cargo test` showed
//! "0 tests" for the capstone and real Linux capture was therefore unproven.
//!
//! Playback uses **mpv** (a documented dependency of the Baxters OS payload, and the
//! same engine StreamCast Tuner uses). If mpv is absent the test skips loudly rather
//! than passing vacuously.

#![cfg(target_os = "linux")]

use bsm_audio::backend::AudioBackend;
use bsm_audio::monitor::MONITOR_DEVICE_INDEX;
use bsm_audio::wasapi::WasapiBackend;
use bsm_core::PcmFormat;
use bsm_encode::muxer::ContainerFormat;
use bsm_encode::output::RecordingSession;
use tokio::sync::{broadcast, mpsc};

#[test]
fn live_monitor_records_system_audio_to_wav() {
    if std::env::var("BSM_USE_HW").as_deref() != Ok("1") {
        eprintln!("skipping live_monitor_records_system_audio_to_wav (set BSM_USE_HW=1)");
        return;
    }
    if which_mpv().is_none() {
        panic!(
            "BSM_USE_HW=1 was requested but `mpv` is not on PATH — cannot play a tone to \
             capture. Install mpv (apt install mpv) or unset BSM_USE_HW."
        );
    }

    // Write a tone and loop-play it through the DEFAULT SINK, so it lands in that
    // sink's .monitor source — which is exactly what the backend records.
    let wav_in = std::env::temp_dir().join("bsm_capstone_tone_linux.wav");
    write_sine_wav(&wav_in, 440.0, 5.0);
    let mut player = std::process::Command::new("mpv")
        .args([
            "--no-video",
            "--really-quiet",
            "--loop-file=inf",
            "--volume=90",
        ])
        .arg(&wav_in)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn mpv playback");
    // Let the sink actually open and start mixing before we capture.
    std::thread::sleep(std::time::Duration::from_millis(1200));

    let dir = tempfile::TempDir::new().unwrap();
    let out = dir.path().join("capture.wav");
    let out2 = out.clone();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let captured = rt.block_on(async move {
        let mut backend = WasapiBackend::new();
        backend
            .open_device(
                MONITOR_DEVICE_INDEX,
                PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 },
            )
            .await
            .expect("open default sink monitor");
        backend.start().await.expect("start monitor capture");

        let fmt = backend
            .actual_format()
            .unwrap_or(PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 });
        let dev = backend.device_name().unwrap_or_else(|| "<unnamed>".into());
        eprintln!("capturing from: {dev} @ {} Hz x{}ch", fmt.sample_rate, fmt.channels);

        let session = RecordingSession::new(
            out2,
            "libopus".to_string(),
            bsm_core::config::EncoderConfig::default(),
            fmt.clone(),
            ContainerFormat::Wav,
        );
        let (tx, rx) = mpsc::channel::<bsm_core::PcmFrame>(256);
        let (_sh_tx, sh_rx) = broadcast::channel::<()>(4);
        let handle = session.start(rx, sh_rx).await.unwrap();

        // Pump monitor frames into the session for ~2.5 s.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(2500);
        while tokio::time::Instant::now() < deadline {
            match backend.next_frame().await {
                Ok(Some(frame)) => {
                    if tx.send(frame).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("capture error: {e:?}");
                    break;
                }
            }
        }
        backend.stop().await.ok();
        backend.close().await.ok();
        drop(tx);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        handle.stop().await.unwrap()
    });
    let _ = player.kill();
    let _ = std::fs::remove_file(&wav_in);

    eprintln!(
        "recorded: {} bytes, {} frames, {} Hz x{}ch",
        captured.bytes_written, captured.frames_captured, captured.sample_rate, captured.channels
    );
    assert!(captured.bytes_written > 0, "nothing recorded");

    // Validate the WAV file: header + non-silent PCM.
    let bytes = std::fs::read(&out).unwrap();
    assert!(bytes.len() > 44, "wav too small ({} bytes)", bytes.len());
    assert_eq!(&bytes[0..4], b"RIFF", "not a RIFF file");
    assert_eq!(&bytes[8..12], b"WAVE", "not a WAVE file");
    let data_sz = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]) as usize;
    assert!(data_sz > 0, "empty data chunk");
    let data = &bytes[44..(44 + data_sz).min(bytes.len())];

    let peak = data
        .chunks_exact(2)
        .map(|s| i16::from_le_bytes([s[0], s[1]]).saturating_abs())
        .max()
        .unwrap_or(0);
    // Mean absolute level too: a single stray spike could satisfy a peak check on an
    // otherwise silent buffer, which would let a broken capture path pass.
    let (sum, count) = data.chunks_exact(2).fold((0u64, 0u64), |(s, c), b| {
        (s + i16::from_le_bytes([b[0], b[1]]).unsigned_abs() as u64, c + 1)
    });
    let mean_abs = if count > 0 { sum / count } else { 0 };
    eprintln!("recorded WAV peak_abs={peak} mean_abs={mean_abs} samples={count}");

    assert!(
        peak > 200,
        "recorded file is silent (peak={peak}) — monitor→file chain not capturing audio"
    );
    assert!(
        mean_abs > 20,
        "recorded file is near-silent (mean_abs={mean_abs}, peak={peak}) — a spike, not \
         sustained audio; the monitor→file chain is not really capturing"
    );
}

fn which_mpv() -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join("mpv"))
            .find(|p| p.is_file())
    })
}

/// 16-bit stereo 48 kHz sine WAV writer (no deps).
fn write_sine_wav(path: &std::path::Path, freq: f32, seconds: f32) {
    use std::io::Write;
    let sr = 48000u32;
    let ch = 2u16;
    let n = (sr as f32 * seconds) as u32;
    let data_bytes = n * ch as u32 * 2;
    let byte_rate = sr * ch as u32 * 2;
    let mut f = std::fs::File::create(path).expect("create wav");
    f.write_all(b"RIFF").unwrap();
    f.write_all(&(36 + data_bytes).to_le_bytes()).unwrap();
    f.write_all(b"WAVE").unwrap();
    f.write_all(b"fmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap();
    f.write_all(&1u16.to_le_bytes()).unwrap();
    f.write_all(&ch.to_le_bytes()).unwrap();
    f.write_all(&sr.to_le_bytes()).unwrap();
    f.write_all(&byte_rate.to_le_bytes()).unwrap();
    f.write_all(&(ch * 2).to_le_bytes()).unwrap();
    f.write_all(&16u16.to_le_bytes()).unwrap();
    f.write_all(b"data").unwrap();
    f.write_all(&data_bytes.to_le_bytes()).unwrap();
    let mut buf = Vec::with_capacity(data_bytes as usize);
    for i in 0..n {
        let t = i as f32 / sr as f32;
        let s = ((t * freq * std::f32::consts::TAU).sin() * 0.6 * i16::MAX as f32) as i16;
        buf.extend_from_slice(&s.to_le_bytes());
        buf.extend_from_slice(&s.to_le_bytes());
    }
    f.write_all(&buf).unwrap();
}
