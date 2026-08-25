//! Diagnostic (`BSM_USE_HW=1`, **Linux**): is monitor capture realtime?
//!
//! `monitor_record.rs` proves the capture path produces non-silent audio, but on
//! 2026-08-14 it recorded only **0.70 s of audio in a 2500 ms window** — roughly
//! a third of realtime. A recorder that drops most of a window would silently
//! produce a file shorter than the session, so the shortfall needs a cause.
//!
//! This isolates the two candidates by draining [`AudioBackend::next_frame`]
//! with **no encoder attached**:
//!
//! * if throughput is ~realtime here, the encoder / `RecordingSession` is the
//!   bottleneck;
//! * if it is still ~1/3 realtime, the cost is in the capture path itself —
//!   most likely `next_frame`, which currently does a `tokio::spawn_blocking`
//!   **per 20 ms frame**.
//!
//! Reports rather than asserts throughput: the number is the finding. It only
//! fails if capture produces nothing at all.

#![cfg(target_os = "linux")]

use bsm_audio::backend::AudioBackend;
use bsm_audio::monitor::MONITOR_DEVICE_INDEX;
use bsm_audio::wasapi::WasapiBackend;
use bsm_core::PcmFormat;
use std::time::{Duration, Instant};

#[test]
fn monitor_capture_keeps_up_with_realtime() {
    if std::env::var("BSM_USE_HW").as_deref() != Ok("1") {
        eprintln!("skipping monitor_capture_keeps_up_with_realtime (set BSM_USE_HW=1)");
        return;
    }
    if which_mpv().is_none() {
        panic!("BSM_USE_HW=1 requested but `mpv` is not on PATH — cannot play a tone.");
    }

    let wav_in = std::env::temp_dir().join("bsm_throughput_tone.wav");
    write_sine_wav(&wav_in, 440.0, 5.0);
    let mut player = std::process::Command::new("mpv")
        .args(["--no-video", "--really-quiet", "--loop-file=inf", "--volume=60"])
        .arg(&wav_in)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn mpv playback");
    std::thread::sleep(Duration::from_millis(1200));

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (frames, audio_ms, wall_ms, worst_ms, per_call, first_ms, rest_ms) = rt.block_on(async {
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
        let bytes_per_frame = fmt.channels as usize * 2;

        let window = Duration::from_millis(2500);
        let started = Instant::now();
        let mut pcm_frames: usize = 0;
        let mut calls: Vec<u128> = Vec::new();
        let mut worst: u128 = 0;

        while started.elapsed() < window {
            let t0 = Instant::now();
            match backend.next_frame().await {
                Ok(Some(f)) => {
                    let us = t0.elapsed().as_micros();
                    calls.push(us);
                    worst = worst.max(us);
                    pcm_frames += f.data.len() / bytes_per_frame;
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("capture error: {e:?}");
                    break;
                }
            }
        }
        let wall = started.elapsed();
        backend.stop().await.ok();
        backend.close().await.ok();

        let audio_ms = (pcm_frames as f64 / fmt.sample_rate as f64) * 1000.0;
        let mean = if calls.is_empty() { 0.0 } else { calls.iter().sum::<u128>() as f64 / calls.len() as f64 };
        let first = calls.first().copied().unwrap_or(0) as f64 / 1000.0;
        let rest = if calls.len() > 1 {
            calls[1..].iter().sum::<u128>() as f64 / (calls.len() - 1) as f64 / 1000.0
        } else { 0.0 };
        (calls.len(), audio_ms, wall.as_millis() as f64, worst as f64 / 1000.0, mean / 1000.0, first, rest)
    });

    let _ = player.kill();
    let _ = player.wait();

    let ratio = if wall_ms > 0.0 { audio_ms / wall_ms } else { 0.0 };
    let steady_ratio = if wall_ms - first_ms > 0.0 {
        (audio_ms - 20.0) / (wall_ms - first_ms)
    } else {
        0.0
    };
    eprintln!("--- monitor throughput (no encoder) ---");
    eprintln!("  chunks received : {frames}");
    eprintln!("  audio captured  : {audio_ms:.0} ms");
    eprintln!("  wall clock      : {wall_ms:.0} ms");
    eprintln!("  realtime ratio  : {ratio:.2}x   (1.00 = keeping up)");
    eprintln!("  next_frame mean : {per_call:.2} ms   worst: {worst_ms:.2} ms");
    eprintln!("  (each chunk is ~20 ms of audio)");
    eprintln!("  --- first call vs steady state ---");
    eprintln!("  FIRST call      : {first_ms:.2} ms   <- startup stall");
    eprintln!("  steady mean     : {rest_ms:.2} ms   (calls 2..n)");
    eprintln!("  steady ratio    : {steady_ratio:.2}x  (excludes the first call)");

    assert!(frames > 0, "captured nothing at all from the monitor");
}

fn which_mpv() -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join("mpv"))
            .find(|p| p.is_file())
    })
}

/// Minimal 16-bit stereo WAV so the test needs no fixture on disk.
fn write_sine_wav(path: &std::path::Path, freq: f64, seconds: f64) {
    use std::io::Write;
    let rate = 48000u32;
    let ch = 2u16;
    let n = (rate as f64 * seconds) as u32;
    let data_len = n * ch as u32 * 2;
    let mut f = std::fs::File::create(path).expect("create tone wav");
    f.write_all(b"RIFF").unwrap();
    f.write_all(&(36 + data_len).to_le_bytes()).unwrap();
    f.write_all(b"WAVEfmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap();
    f.write_all(&1u16.to_le_bytes()).unwrap();
    f.write_all(&ch.to_le_bytes()).unwrap();
    f.write_all(&rate.to_le_bytes()).unwrap();
    f.write_all(&(rate * ch as u32 * 2).to_le_bytes()).unwrap();
    f.write_all(&(ch * 2).to_le_bytes()).unwrap();
    f.write_all(&16u16.to_le_bytes()).unwrap();
    f.write_all(b"data").unwrap();
    f.write_all(&data_len.to_le_bytes()).unwrap();
    for i in 0..n {
        let t = i as f64 / rate as f64;
        let v = ((t * freq * std::f64::consts::TAU).sin() * 12000.0) as i16;
        for _ in 0..ch {
            f.write_all(&v.to_le_bytes()).unwrap();
        }
    }
}
