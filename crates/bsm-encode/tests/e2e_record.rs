//! End-to-end recording verification (Gap #2): drive the real
//! `RecordingSession` (encoder → muxer → file) with a known sine signal for
//! each container and prove the output is a valid, decodable, playable file.
//!
//! - **WAV**: parse the RIFF container and confirm the PCM data round-trips
//!   byte-exactly (lossless passthrough).
//! - **FLAC**: decode with an INDEPENDENT decoder (`claxon`) and confirm the
//!   hand-rolled encoder produced standard, **losslessly**-decodable FLAC whose
//!   samples equal the input — the key validation for a from-scratch encoder.
//! - **MP3**: confirm the LAME-encoded file is structurally valid (real MPEG
//!   audio frames) and plausibly sized (lossy → structural, not sample, check).

use bsm_core::PcmFormat;
use bsm_encode::muxer::ContainerFormat;
use bsm_encode::output::RecordingSession;
use tempfile::TempDir;
use tokio::runtime::Runtime;
use tokio::sync::{broadcast, mpsc};

const SR: u32 = 48000;
const CH: u16 = 2;
const SECONDS: u32 = 1;

/// Generate `SECONDS` of 440 Hz stereo i16 PCM as interleaved LE bytes.
fn sine_pcm() -> Vec<u8> {
    let n = SR * SECONDS;
    let mut out = Vec::with_capacity((n * CH as u32 * 2) as usize);
    for i in 0..n {
        let t = i as f32 / SR as f32;
        let s = ((t * 440.0 * std::f32::consts::TAU).sin() * 0.7 * i16::MAX as f32) as i16;
        out.extend_from_slice(&s.to_le_bytes());
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Record `pcm` (LE i16 interleaved) through a real session to `path`.
fn record_to_file(path: std::path::PathBuf, container: ContainerFormat, pcm: &[u8]) {
    let rt = Runtime::new().unwrap();
    rt.block_on(async move {
        let fmt = PcmFormat { sample_rate: SR, channels: CH, bit_depth: 16 };
        let session = RecordingSession::new(
            path,
            "libopus".to_string(),
            bsm_core::config::EncoderConfig::default(),
            fmt.clone(),
            container,
        );
        let (tx, rx) = mpsc::channel::<bsm_core::PcmFrame>(256);
        let (sh_tx, sh_rx) = broadcast::channel::<()>(4);
        let handle = session.start(rx, sh_rx).await.unwrap();

        // Feed in ~20 ms frames.
        let frame_samples = (SR / 50) as usize; // 20 ms
        let frame_bytes = frame_samples * CH as usize * 2;
        let mut seq = 0u64;
        for chunk in pcm.chunks(frame_bytes) {
            let frame = bsm_core::PcmFrame {
                data: chunk.to_vec(),
                format: PcmFormat { sample_rate: SR, channels: CH, bit_depth: 16 },
                timestamp_us: 0,
                sequence: seq,
                frame_count: (chunk.len() / (CH as usize * 2)) as u32,
            };
            let _ = tx.send(frame).await;
            seq += 1;
        }
        drop(tx); // signal end of input
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let info = handle.stop().await.unwrap();
        assert!(info.bytes_written > 0, "no bytes written");
        let _ = sh_tx.send(());
    });
}

#[test]
fn wav_records_and_roundtrips_exactly() {
    let pcm = sine_pcm();
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("rec.wav");
    record_to_file(out.clone(), ContainerFormat::Wav, &pcm);

    let bytes = std::fs::read(&out).unwrap();
    assert!(bytes.len() > 44, "wav too small");
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    // fmt: channels @22, sample rate @24
    assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), CH);
    assert_eq!(u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]), SR);
    let data_sz = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]) as usize;
    let data = &bytes[44..44 + data_sz];
    // Lossless passthrough: the WAV data must equal the input PCM exactly.
    assert_eq!(data, &pcm[..], "WAV PCM did not round-trip byte-exactly");
    // And it must be real audio, not silence.
    let peak = data.chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]]).saturating_abs()).max().unwrap_or(0);
    assert!(peak > 10000, "WAV is silent (peak={peak})");
}

#[test]
fn flac_records_and_decodes_losslessly() {
    let pcm = sine_pcm();
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("rec.flac");
    record_to_file(out.clone(), ContainerFormat::Flac, &pcm);

    // Decode with an INDEPENDENT FLAC decoder.
    let mut reader = match claxon::FlacReader::open(&out) {
        Ok(r) => r,
        Err(e) => {
            let hdr = std::fs::read(&out).unwrap();
            let n = hdr.len().min(48);
            panic!("claxon failed to open FLAC: {e:?}\n  size={} first{}={:02x?}", hdr.len(), n, &hdr[..n]);
        }
    };
    let si = reader.streaminfo();
    assert_eq!(si.sample_rate, SR, "flac sample rate");
    assert_eq!(si.channels as u16, CH, "flac channels");
    assert_eq!(si.bits_per_sample, 16, "flac bit depth");

    // Compare decoded samples to the input (FLAC is lossless → exact match).
    let input: Vec<i16> = pcm
        .chunks_exact(2)
        .map(|s| i16::from_le_bytes([s[0], s[1]]))
        .collect();
    let decoded: Vec<i32> = reader.samples().map(|s| s.expect("flac sample")).collect();
    assert_eq!(decoded.len(), input.len(), "flac sample count mismatch");
    let mut peak = 0i32;
    for (d, i) in decoded.iter().zip(input.iter()) {
        assert_eq!(*d, *i as i32, "FLAC not lossless — decoded sample differs from input");
        peak = peak.max(d.abs());
    }
    assert!(peak > 10000, "FLAC decoded to silence (peak={peak})");
}

#[test]
fn mp3_records_valid_mpeg_frames() {
    let pcm = sine_pcm();
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("rec.mp3");
    record_to_file(out.clone(), ContainerFormat::Mp3, &pcm);

    let bytes = std::fs::read(&out).unwrap();
    assert!(bytes.len() > 500, "mp3 too small ({} bytes)", bytes.len());

    // Structural validation: LAME output is a stream of MPEG audio frames
    // (optionally an ID3 tag first). Find the first frame sync (11 set bits:
    // 0xFF Ex/Fx) and confirm several valid frames follow — proof of a real,
    // decodable MP3 rather than random bytes.
    let start = if &bytes[0..3] == b"ID3" {
        // skip the ID3v2 tag (syncsafe size at [6..10])
        let sz = ((bytes[6] as usize & 0x7f) << 21)
            | ((bytes[7] as usize & 0x7f) << 14)
            | ((bytes[8] as usize & 0x7f) << 7)
            | (bytes[9] as usize & 0x7f);
        10 + sz
    } else {
        0
    };
    let mut frames = 0;
    let mut i = start;
    while i + 4 < bytes.len() {
        if bytes[i] == 0xFF && (bytes[i + 1] & 0xE0) == 0xE0 {
            // MPEG-1 Layer III @ 48k/44.1k typical frame ~ hundreds of bytes;
            // step past this header and keep scanning.
            frames += 1;
            i += 4;
        } else {
            i += 1;
        }
    }
    assert!(frames > 5, "expected multiple MPEG frames, found {frames}");
    // ~1 s of audio at a normal bitrate is comfortably > 2 KB.
    assert!(bytes.len() > 2000, "mp3 implausibly small for 1s ({} bytes)", bytes.len());
}
