//! Capstone (Gap #2, `BSM_USE_HW=1`, Windows): the WHOLE app path —
//! WASAPI loopback capture of real system audio → RecordingSession (encoder →
//! WAV muxer) → file → decode → confirm the file contains the captured audio,
//! not silence. Proves "records system audio to a valid file" end to end.

#![cfg(windows)]

use bsm_audio::backend::AudioBackend;
use bsm_audio::loopback::LOOPBACK_DEVICE_INDEX;
use bsm_audio::wasapi::WasapiBackend;
use bsm_core::PcmFormat;
use bsm_encode::muxer::ContainerFormat;
use bsm_encode::output::RecordingSession;
use tokio::sync::{broadcast, mpsc};

#[test]
fn live_loopback_records_system_audio_to_wav() {
    if std::env::var("BSM_USE_HW").as_deref() != Ok("1") {
        eprintln!("skipping live_loopback_records_system_audio_to_wav (set BSM_USE_HW=1)");
        return;
    }

    // Write + loop-play a tone through the default output.
    let wav_in = std::env::temp_dir().join("bsm_capstone_tone.wav");
    write_sine_wav(&wav_in, 440.0, 5.0);
    let mut player = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "$p=New-Object System.Media.SoundPlayer '{}'; $p.PlayLooping(); Start-Sleep -Seconds 9",
                wav_in.display()
            ),
        ])
        .spawn()
        .expect("spawn playback");
    std::thread::sleep(std::time::Duration::from_millis(700));

    let dir = tempfile::TempDir::new().unwrap();
    let out = dir.path().join("capture.wav");
    let out2 = out.clone();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let info = rt.block_on(async move {
        // Open the loopback backend and start capture.
        let mut backend = WasapiBackend::new();
        backend
            .open_device(LOOPBACK_DEVICE_INDEX, PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 })
            .await
            .expect("open loopback");
        backend.start().await.expect("start");
        let fmt = backend.actual_format().unwrap_or(PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 });

        // Recording session writing WAV using the captured format.
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

        // Pump loopback frames into the session for ~2.5 s.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(2500);
        while tokio::time::Instant::now() < deadline {
            match backend.next_frame().await {
                Ok(Some(frame)) => {
                    if tx.send(frame).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
        backend.stop().await.ok();
        drop(tx);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        handle.stop().await.unwrap()
    });
    let _ = player.kill();

    eprintln!(
        "recorded: {} bytes, {} frames, {} Hz x{}ch",
        info.bytes_written, info.frames_captured, info.sample_rate, info.channels
    );
    assert!(info.bytes_written > 0, "nothing recorded");

    // Validate the WAV file: header + non-silent PCM.
    let bytes = std::fs::read(&out).unwrap();
    assert!(bytes.len() > 44, "wav too small");
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    let data_sz = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]) as usize;
    assert!(data_sz > 0, "empty data chunk");
    let data = &bytes[44..(44 + data_sz).min(bytes.len())];
    let peak = data
        .chunks_exact(2)
        .map(|s| i16::from_le_bytes([s[0], s[1]]).saturating_abs())
        .max()
        .unwrap_or(0);
    eprintln!("recorded WAV peak_abs={peak}");
    assert!(peak > 200, "recorded file is silent (peak={peak}) — loopback→file chain not capturing audio");
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
