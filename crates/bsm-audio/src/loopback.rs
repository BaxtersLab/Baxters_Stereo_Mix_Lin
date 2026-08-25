//! True WASAPI render-endpoint **loopback** capture.
//!
//! Captures whatever is currently playing on the default output device — the
//! real "record system audio" path — **without** depending on the legacy
//! "Stereo Mix" input device that Windows 10 hides/disables by default.
//!
//! Implemented directly against WASAPI (`IAudioClient` initialised with
//! `AUDCLNT_STREAMFLAGS_LOOPBACK` on the default *render* endpoint). All COM
//! objects live on a dedicated capture thread (they are `!Send`); only decoded
//! `PcmFrame`s cross back to the async backend via a channel. Output is i16 PCM
//! at the render mix's sample rate, mapped to the requested channel count —
//! matching the cpal input path's frame shape.

use bsm_core::{AudioError, PcmFormat, PcmFrame};
use crossbeam_channel::{Receiver as CbReceiver, Sender as CbSender};

/// Synthetic device index the UI/backend uses to mean "System Audio
/// (Loopback)". Chosen far above any real cpal device index.
pub const LOOPBACK_DEVICE_INDEX: u32 = u32::MAX;
pub const LOOPBACK_DEVICE_NAME: &str = "System Audio (Loopback)";

/// The render mix format we read from WASAPI, reduced to what conversion needs.
/// (Only constructed by the Windows loopback impl; unused in a non-Windows build.)
#[derive(Debug, Clone, Copy)]
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) struct MixInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits: u16,
    pub is_float: bool,
}

impl MixInfo {
    /// The pipeline-facing format: i16 PCM at the mix rate, requested channels.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn to_pcm_format(&self, requested_channels: u16) -> PcmFormat {
        PcmFormat {
            sample_rate: self.sample_rate,
            channels: requested_channels,
            bit_depth: 16,
        }
    }
}

/// Convert an interleaved render-mix buffer to interleaved **i16**, mapping the
/// mix's channel count to `dst_channels`. Pure (no COM) so it is unit-tested.
///
/// - float mix (f32) → clamp/scale to i16
/// - 16-bit PCM mix → pass through (with channel map)
/// - 32-bit int PCM mix → shift down to i16
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn mix_to_i16(
    bytes: &[u8],
    info: MixInfo,
    dst_channels: u16,
) -> Vec<u8> {
    let src_ch = info.channels.max(1) as usize;
    let dst_ch = dst_channels.max(1) as usize;
    let bytes_per_sample = (info.bits / 8).max(1) as usize;
    let stride = src_ch * bytes_per_sample;
    if stride == 0 || bytes.len() < stride {
        return Vec::new();
    }
    let frames = bytes.len() / stride;

    let sample_i16 = |ch: usize, frame: usize| -> i16 {
        let idx = (frame * src_ch + ch) * bytes_per_sample;
        if idx + bytes_per_sample > bytes.len() {
            return 0;
        }
        if info.is_float && info.bits == 32 {
            let v = f32::from_le_bytes([bytes[idx], bytes[idx + 1], bytes[idx + 2], bytes[idx + 3]]);
            (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
        } else if info.bits == 16 {
            i16::from_le_bytes([bytes[idx], bytes[idx + 1]])
        } else if info.bits == 32 {
            // 32-bit int PCM → top 16 bits
            let v = i32::from_le_bytes([bytes[idx], bytes[idx + 1], bytes[idx + 2], bytes[idx + 3]]);
            (v >> 16) as i16
        } else {
            0
        }
    };

    let mut out = Vec::with_capacity(frames * dst_ch * 2);
    for f in 0..frames {
        for dch in 0..dst_ch {
            // src==dst: direct; dst>src: repeat channels; dst<src: take first dst.
            let sch = if src_ch == dst_ch {
                dch
            } else if dst_ch > src_ch {
                dch % src_ch
            } else {
                dch // dst < src: take the first dst channels
            };
            out.extend_from_slice(&sample_i16(sch, f).to_le_bytes());
        }
    }
    out
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use windows::core::GUID;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
        MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED,
    };

    const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
    const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
    // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
    const SUBTYPE_IEEE_FLOAT: GUID =
        GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

    unsafe fn read_mix_info(pwfx: *const WAVEFORMATEX) -> MixInfo {
        use std::ptr::{addr_of, read_unaligned};
        // WAVEFORMATEX / WAVEFORMATEXTENSIBLE are `#[repr(packed)]`; read fields
        // through raw pointers (never take a reference into a packed struct).
        let format_tag = read_unaligned(addr_of!((*pwfx).wFormatTag));
        let channels = read_unaligned(addr_of!((*pwfx).nChannels));
        let sample_rate = read_unaligned(addr_of!((*pwfx).nSamplesPerSec));
        let bits = read_unaligned(addr_of!((*pwfx).wBitsPerSample));
        let mut is_float = format_tag == WAVE_FORMAT_IEEE_FLOAT;
        if format_tag == WAVE_FORMAT_EXTENSIBLE {
            let ext = pwfx as *const WAVEFORMATEXTENSIBLE;
            let sub = read_unaligned(addr_of!((*ext).SubFormat));
            if sub == SUBTYPE_IEEE_FLOAT {
                is_float = true;
            }
        }
        MixInfo {
            sample_rate,
            channels,
            bits,
            is_float,
        }
    }

    /// Spawn the loopback capture thread. Returns the negotiated pipeline format
    /// (blocks only until setup succeeds/fails on the thread).
    pub(crate) fn spawn(
        requested: PcmFormat,
        frame_tx: CbSender<PcmFrame>,
        stop_rx: CbReceiver<()>,
    ) -> Result<PcmFormat, AudioError> {
        let (setup_tx, setup_rx) = mpsc::channel::<Result<PcmFormat, AudioError>>();
        let requested_channels = requested.channels.max(1);

        std::thread::spawn(move || unsafe {
            // COM must be initialised on the thread that uses the interfaces.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

            let setup = (|| -> Result<(IAudioClient, IAudioCaptureClient, MixInfo, *mut WAVEFORMATEX), AudioError> {
                let enumerator: IMMDeviceEnumerator =
                    CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                        .map_err(|e| AudioError::Wasapi(e.code().0 as u32, format!("CoCreateInstance: {e}")))?;
                let device = enumerator
                    .GetDefaultAudioEndpoint(eRender, eConsole)
                    .map_err(|e| AudioError::Wasapi(e.code().0 as u32, format!("GetDefaultAudioEndpoint(render): {e}")))?;
                let audio_client: IAudioClient = device
                    .Activate(CLSCTX_ALL, None)
                    .map_err(|e| AudioError::Wasapi(e.code().0 as u32, format!("Activate IAudioClient: {e}")))?;
                let pwfx = audio_client
                    .GetMixFormat()
                    .map_err(|e| AudioError::Wasapi(e.code().0 as u32, format!("GetMixFormat: {e}")))?;
                let mix = read_mix_info(pwfx);
                // 200 ms buffer (REFERENCE_TIME = 100 ns units).
                let buffer_duration: i64 = 2_000_000;
                audio_client
                    .Initialize(
                        AUDCLNT_SHAREMODE_SHARED,
                        AUDCLNT_STREAMFLAGS_LOOPBACK,
                        buffer_duration,
                        0,
                        pwfx,
                        None,
                    )
                    .map_err(|e| AudioError::Wasapi(e.code().0 as u32, format!("Initialize(loopback): {e}")))?;
                let capture: IAudioCaptureClient = audio_client
                    .GetService()
                    .map_err(|e| AudioError::Wasapi(e.code().0 as u32, format!("GetService(capture): {e}")))?;
                audio_client
                    .Start()
                    .map_err(|e| AudioError::Wasapi(e.code().0 as u32, format!("Start: {e}")))?;
                Ok((audio_client, capture, mix, pwfx))
            })();

            match setup {
                Ok((audio_client, capture, mix, pwfx)) => {
                    let _ = setup_tx.send(Ok(mix.to_pcm_format(requested_channels)));
                    capture_loop(&audio_client, &capture, mix, requested_channels, &frame_tx, &stop_rx);
                    let _ = audio_client.Stop();
                    CoTaskMemFree(Some(pwfx as *const _));
                }
                Err(e) => {
                    let _ = setup_tx.send(Err(e));
                }
            }
            CoUninitialize();
        });

        setup_rx
            .recv()
            .map_err(|e| AudioError::Wasapi(0, format!("loopback setup channel closed: {e}")))?
    }

    unsafe fn capture_loop(
        audio_client: &IAudioClient,
        capture: &IAudioCaptureClient,
        mix: MixInfo,
        requested_channels: u16,
        frame_tx: &CbSender<PcmFrame>,
        stop_rx: &CbReceiver<()>,
    ) {
        // Poll at roughly half the buffer period.
        let poll = Duration::from_millis(10);
        let mut seq: u64 = 0;
        let _ = audio_client; // (kept alive by caller; referenced for symmetry)

        loop {
            if stop_rx.try_recv().is_ok() {
                break;
            }
            let mut produced = false;
            loop {
                let avail = match capture.GetNextPacketSize() {
                    Ok(n) => n,
                    Err(_) => break,
                };
                if avail == 0 {
                    break;
                }
                let mut data_ptr: *mut u8 = std::ptr::null_mut();
                let mut num_frames: u32 = 0;
                let mut flags: u32 = 0;
                if capture
                    .GetBuffer(&mut data_ptr, &mut num_frames, &mut flags, None, None)
                    .is_err()
                {
                    break;
                }
                if num_frames > 0 {
                    let src_bytes_per_frame = (mix.channels as usize) * (mix.bits as usize / 8);
                    let byte_len = num_frames as usize * src_bytes_per_frame;
                    let i16_data = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data_ptr.is_null() {
                        // Silent packet — emit zeros of the right shape.
                        vec![0u8; num_frames as usize * requested_channels as usize * 2]
                    } else {
                        let slice = std::slice::from_raw_parts(data_ptr, byte_len);
                        mix_to_i16(slice, mix, requested_channels)
                    };
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let pf = PcmFrame {
                        data: i16_data,
                        format: mix.to_pcm_format(requested_channels),
                        timestamp_us: now,
                        sequence: seq,
                        frame_count: num_frames,
                    };
                    if frame_tx.send(pf).is_err() {
                        let _ = capture.ReleaseBuffer(num_frames);
                        return; // consumer gone
                    }
                    seq = seq.wrapping_add(1);
                    produced = true;
                }
                let _ = capture.ReleaseBuffer(num_frames);
            }
            if !produced {
                std::thread::sleep(poll);
            }
        }
    }
}

/// Spawn WASAPI loopback capture. On non-Windows this is unsupported.
pub(crate) fn spawn_loopback_capture(
    requested: PcmFormat,
    frame_tx: CbSender<PcmFrame>,
    stop_rx: CbReceiver<()>,
) -> Result<PcmFormat, AudioError> {
    #[cfg(windows)]
    {
        imp::spawn(requested, frame_tx, stop_rx)
    }
    #[cfg(not(windows))]
    {
        let _ = (requested, frame_tx, stop_rx);
        Err(AudioError::UnsupportedFormat(
            "WASAPI loopback is Windows-only (Linux uses a PipeWire/PulseAudio monitor backend)".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_stereo_passthrough_scales_to_i16() {
        let info = MixInfo { sample_rate: 48000, channels: 2, bits: 32, is_float: true };
        // one stereo frame: L=+1.0, R=-1.0
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1.0f32.to_le_bytes());
        bytes.extend_from_slice(&(-1.0f32).to_le_bytes());
        let out = mix_to_i16(&bytes, info, 2);
        assert_eq!(out.len(), 4); // 1 frame * 2 ch * 2 bytes
        let l = i16::from_le_bytes([out[0], out[1]]);
        let r = i16::from_le_bytes([out[2], out[3]]);
        assert_eq!(l, i16::MAX);
        assert_eq!(r, -i16::MAX);
    }

    #[test]
    fn float_mono_upmixes_to_stereo() {
        let info = MixInfo { sample_rate: 48000, channels: 1, bits: 32, is_float: true };
        let bytes = 0.5f32.to_le_bytes().to_vec();
        let out = mix_to_i16(&bytes, info, 2);
        assert_eq!(out.len(), 4);
        let l = i16::from_le_bytes([out[0], out[1]]);
        let r = i16::from_le_bytes([out[2], out[3]]);
        assert_eq!(l, r);
        assert!((l - (i16::MAX / 2)).abs() <= 1);
    }

    #[test]
    fn i16_stereo_passthrough() {
        let info = MixInfo { sample_rate: 44100, channels: 2, bits: 16, is_float: false };
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1234i16.to_le_bytes());
        bytes.extend_from_slice(&(-4321i16).to_le_bytes());
        let out = mix_to_i16(&bytes, info, 2);
        assert_eq!(i16::from_le_bytes([out[0], out[1]]), 1234);
        assert_eq!(i16::from_le_bytes([out[2], out[3]]), -4321);
    }

    #[test]
    fn surround_downmix_takes_first_two() {
        let info = MixInfo { sample_rate: 48000, channels: 6, bits: 32, is_float: true };
        let vals = [0.1f32, 0.2, 0.3, 0.4, 0.5, 0.6];
        let mut bytes = Vec::new();
        for v in vals { bytes.extend_from_slice(&v.to_le_bytes()); }
        let out = mix_to_i16(&bytes, info, 2);
        assert_eq!(out.len(), 4);
        let l = i16::from_le_bytes([out[0], out[1]]);
        let r = i16::from_le_bytes([out[2], out[3]]);
        assert!((l - (0.1 * i16::MAX as f32) as i16).abs() <= 2);
        assert!((r - (0.2 * i16::MAX as f32) as i16).abs() <= 2);
    }

    #[test]
    fn empty_input_is_safe() {
        let info = MixInfo { sample_rate: 48000, channels: 2, bits: 32, is_float: true };
        assert!(mix_to_i16(&[], info, 2).is_empty());
    }

    /// Hardware-gated LIVE proof (Windows, `BSM_USE_HW=1`): play a tone through
    /// the default output and confirm WASAPI loopback actually captures it —
    /// i.e. real system-audio capture with NO "Stereo Mix" input device.
    #[cfg(windows)]
    #[test]
    fn live_loopback_captures_real_system_audio() {
        use std::time::{Duration, Instant};

        if std::env::var("BSM_USE_HW").as_deref() != Ok("1") {
            eprintln!("skipping live_loopback_captures_real_system_audio (set BSM_USE_HW=1)");
            return;
        }

        // 1. Write a 440 Hz stereo sine WAV.
        let wav = std::env::temp_dir().join("bsm_live_tone.wav");
        write_sine_wav(&wav, 440.0, 4.0);

        // 2. Play it (looping) through the DEFAULT output endpoint in the
        //    background — this is exactly what loopback should capture.
        let mut player = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "$p=New-Object System.Media.SoundPlayer '{}'; $p.PlayLooping(); Start-Sleep -Seconds 8",
                    wav.display()
                ),
            ])
            .spawn()
            .expect("spawn playback");
        std::thread::sleep(Duration::from_millis(700)); // let audio start

        // 3. Capture via loopback for ~3s and measure the peak sample.
        let (tx, rx) = crossbeam_channel::bounded::<PcmFrame>(256);
        let (stop_tx, stop_rx) = crossbeam_channel::bounded::<()>(1);
        let fmt = spawn_loopback_capture(
            PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 },
            tx,
            stop_rx,
        )
        .expect("loopback capture start");
        eprintln!("loopback negotiated format: {fmt:?}");

        let mut frames = 0usize;
        let mut max_abs = 0i16;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(pf) = rx.recv_timeout(Duration::from_millis(500)) {
                frames += 1;
                for s in pf.data.chunks_exact(2) {
                    let v = i16::from_le_bytes([s[0], s[1]]).saturating_abs();
                    if v > max_abs {
                        max_abs = v;
                    }
                }
            }
        }
        let _ = stop_tx.send(());
        let _ = player.kill();

        eprintln!("captured frames={frames} peak_abs={max_abs}");
        assert!(frames > 0, "no frames captured from loopback");
        // Non-silence floor well below any real signal but far above the ~0 a
        // broken/silent loopback would yield. (Absolute level tracks system
        // master volume — loopback captures the post-mix signal faithfully.)
        assert!(
            max_abs > 200,
            "loopback captured silence (peak_abs={max_abs}) — not capturing system audio (is output muted?)"
        );
    }

    /// Minimal 16-bit stereo 48 kHz sine WAV writer (test helper, no deps).
    #[cfg(windows)]
    fn write_sine_wav(path: &std::path::Path, freq: f32, seconds: f32) {
        use std::io::Write;
        let sr = 48000u32;
        let ch = 2u16;
        let n = (sr as f32 * seconds) as u32;
        let data_bytes = n * ch as u32 * 2;
        let mut f = std::fs::File::create(path).expect("create wav");
        let byte_rate = sr * ch as u32 * 2;
        f.write_all(b"RIFF").unwrap();
        f.write_all(&(36 + data_bytes).to_le_bytes()).unwrap();
        f.write_all(b"WAVE").unwrap();
        f.write_all(b"fmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap(); // PCM
        f.write_all(&ch.to_le_bytes()).unwrap();
        f.write_all(&sr.to_le_bytes()).unwrap();
        f.write_all(&byte_rate.to_le_bytes()).unwrap();
        f.write_all(&(ch * 2).to_le_bytes()).unwrap(); // block align
        f.write_all(&16u16.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_bytes.to_le_bytes()).unwrap();
        let mut buf = Vec::with_capacity(data_bytes as usize);
        for i in 0..n {
            let t = i as f32 / sr as f32;
            let s = (( t * freq * std::f32::consts::TAU).sin() * 0.6 * i16::MAX as f32) as i16;
            buf.extend_from_slice(&s.to_le_bytes());
            buf.extend_from_slice(&s.to_le_bytes());
        }
        f.write_all(&buf).unwrap();
    }
}
