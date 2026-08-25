use async_trait::async_trait;
use bsm_core::{AudioError, AudioResult, DeviceEntry, PcmFormat, PcmFrame};
use crate::backend::AudioBackend;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use crossbeam_channel::{bounded, Receiver as CbReceiver, Sender as CbSender};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

/// WASAPI backend implemented via `cpal` and a background thread.
/// The capture `Stream` lives inside a dedicated thread so the public
/// `WasapiBackend` type remains `Send + Sync`.
pub struct WasapiBackend {
    rx: Option<CbReceiver<PcmFrame>>,
    stop_tx: Option<CbSender<()>>,
    active: bool,
    actual_format: Option<PcmFormat>,
    device_name: Option<String>,
}

/// Diagnostic information produced by negotiation logic.
pub struct NegotiationInfo {
    pub device_index: u32,
    pub requested_format: PcmFormat,
    pub chosen_channels: u16,
    pub chosen_sample_format: SampleFormat,
    pub chosen_min_sr: u32,
    pub chosen_max_sr: u32,
    pub negotiated_sample_rate: u32,
    pub bit_depth: u16,
    pub candidate_rates: Vec<u32>,
    pub score: u64,
}

impl WasapiBackend {
    pub fn new() -> Self {
        Self { rx: None, stop_tx: None, active: false, actual_format: None, device_name: None }
    }

    /// Query supported sample rates for given device index and channel count.
    /// Returns a vector of common sample rates that fall within supported ranges.
    pub fn enumerate_supported_sample_rates(device_index: u32, channels: u16) -> Result<Vec<u32>, AudioError> {
        let host = cpal::default_host();
        let dev = host.devices().map_err(|e| AudioError::Wasapi(0, format!("device enumerate failed: {}", e)))?.nth(device_index as usize).ok_or(AudioError::DeviceNotFound(format!("index {}", device_index)))?;
        let mut out = Vec::new();
        let supported = dev.supported_input_configs().map_err(|e| AudioError::Wasapi(0, format!("supported configs failed: {}", e)))?;
        let common = [44100u32, 48000u32, 88200u32, 96000u32, 176400u32, 192000u32];
        for cfg in supported {
            if cfg.channels() as u16 != channels { continue; }
            let min = cfg.min_sample_rate().0;
            let max = cfg.max_sample_rate().0;
            for &r in &common {
                if r >= min && r <= max && !out.contains(&r) { out.push(r); }
            }
            // include min and max if not already present
            if !out.contains(&min) { out.push(min); }
            if !out.contains(&max) { out.push(max); }
        }
        out.sort_unstable();
        Ok(out)
    }

    /// Run the negotiation logic and return detailed diagnostics without
    /// opening or starting streams. Useful for UI previews and logging.
    pub fn diagnose_negotiation(device_index: u32, format: PcmFormat) -> Result<NegotiationInfo, AudioError> {
        let host = cpal::default_host();
        let dev = host.devices().map_err(|e| AudioError::Wasapi(0, format!("device enumerate failed: {}", e)))?.nth(device_index as usize).ok_or(AudioError::DeviceNotFound(format!("index {}", device_index)))?;
        let sample_rate = format.sample_rate;
        let channels = format.channels as u16;

        let mut best_score: u64 = u64::MAX;
        let mut chosen_sf: Option<SampleFormat> = None;
        let mut chosen_min_sr: u32 = 0;
        let mut chosen_max_sr: u32 = 0;
        let mut candidate_rates: Vec<u32> = Vec::new();

        let supported_iter = dev.supported_input_configs().map_err(|e| AudioError::Wasapi(0, format!("supported configs failed: {}", e)))?;
        let configs: Vec<_> = supported_iter.collect();

        // First pass: prefer configs that match requested channel count
        for cfg in configs.iter() {
            let sf = cfg.sample_format();
            let ch = cfg.channels() as u16;
            if ch != channels { continue; }
            let min_sr = cfg.min_sample_rate().0;
            let max_sr = cfg.max_sample_rate().0;
            for &r in &[44100u32, 48000u32, 88200u32, 96000u32, 176400u32, 192000u32] {
                if r >= min_sr && r <= max_sr && !candidate_rates.contains(&r) { candidate_rates.push(r); }
            }
            if !candidate_rates.contains(&min_sr) { candidate_rates.push(min_sr); }
            if !candidate_rates.contains(&max_sr) { candidate_rates.push(max_sr); }

            let score: u64 = if sample_rate >= min_sr && sample_rate <= max_sr { 0 } else {
                if sample_rate < min_sr { (min_sr - sample_rate) as u64 } else { (sample_rate - max_sr) as u64 }
            };
            let mut adj = score;
            if format.bit_depth == 32 && sf == SampleFormat::F32 { adj = adj.saturating_sub(1000); }
            if format.bit_depth == 16 && (sf == SampleFormat::I16 || sf == SampleFormat::U16) { adj = adj.saturating_sub(1000); }

            if adj < best_score {
                best_score = adj;
                chosen_sf = Some(sf);
                chosen_min_sr = min_sr;
                chosen_max_sr = max_sr;
            }
        }

        // Second pass: relax channel requirement and take the best available config
        if chosen_sf.is_none() {
            for cfg in configs.iter() {
                let sf = cfg.sample_format();
                let min_sr = cfg.min_sample_rate().0;
                let max_sr = cfg.max_sample_rate().0;
                for &r in &[44100u32, 48000u32, 88200u32, 96000u32, 176400u32, 192000u32] {
                    if r >= min_sr && r <= max_sr && !candidate_rates.contains(&r) { candidate_rates.push(r); }
                }
                if !candidate_rates.contains(&min_sr) { candidate_rates.push(min_sr); }
                if !candidate_rates.contains(&max_sr) { candidate_rates.push(max_sr); }

                let score: u64 = if sample_rate >= min_sr && sample_rate <= max_sr { 0 } else {
                    if sample_rate < min_sr { (min_sr - sample_rate) as u64 } else { (sample_rate - max_sr) as u64 }
                };
                let mut adj = score;
                if format.bit_depth == 32 && sf == SampleFormat::F32 { adj = adj.saturating_sub(1000); }
                if format.bit_depth == 16 && (sf == SampleFormat::I16 || sf == SampleFormat::U16) { adj = adj.saturating_sub(1000); }

                if adj < best_score {
                    best_score = adj;
                    chosen_sf = Some(sf);
                    chosen_min_sr = min_sr;
                    chosen_max_sr = max_sr;
                }
            }
        }

        let chosen_sf = match chosen_sf { Some(s) => s, None => return Err(AudioError::UnsupportedFormat(format!("no supported config for {} channels", channels))) };

        // Determine chosen channels from the selected config if available
        let mut chosen_channels: u16 = channels;
        if let Some(cfg) = configs.iter().find(|c| c.sample_format() == chosen_sf) {
            chosen_channels = cfg.channels() as u16;
        }

        let negotiated_sample_rate: u32 = if sample_rate >= chosen_min_sr && sample_rate <= chosen_max_sr { sample_rate } else if sample_rate < chosen_min_sr { chosen_min_sr } else { chosen_max_sr };
        let bit_depth = match chosen_sf { SampleFormat::F32 => 32, SampleFormat::I16 => 16, SampleFormat::U16 => 16, _ => 16 };

        candidate_rates.sort_unstable();

        Ok(NegotiationInfo {
            device_index,
            requested_format: format,
            chosen_channels,
            chosen_sample_format: chosen_sf,
            chosen_min_sr,
            chosen_max_sr,
            negotiated_sample_rate,
            bit_depth,
            candidate_rates,
            score: best_score,
        })
    }
}

#[async_trait]
impl AudioBackend for WasapiBackend {
    async fn enumerate_devices(&self) -> AudioResult<Vec<DeviceEntry>> {
        let mut out = Vec::new();
        // "System Audio (Loopback)" first — the real record-what's-playing path,
        // independent of the (often-disabled) legacy "Stereo Mix" input device.
        // Windows only; on other platforms the loopback entry is omitted.
        #[cfg(windows)]
        out.push(DeviceEntry {
            index: crate::loopback::LOOPBACK_DEVICE_INDEX,
            name: crate::loopback::LOOPBACK_DEVICE_NAME.to_string(),
            is_default: true,
            is_loopback: true,
        });

        // "System Audio (Monitor)" first on Linux — the default sink's
        // PulseAudio/PipeWire monitor (record what's playing). Marked is_loopback
        // so the UI treats it like the Windows loopback entry (no cpal negotiation).
        #[cfg(target_os = "linux")]
        out.push(DeviceEntry {
            index: crate::monitor::MONITOR_DEVICE_INDEX,
            name: crate::monitor::MONITOR_DEVICE_NAME.to_string(),
            is_default: true,
            is_loopback: true,
        });

        let host = cpal::default_host();
        if let Ok(devs) = host.devices() {
            for (i, d) in devs.enumerate() {
                let name = d.name().unwrap_or_else(|_| "Unnamed".to_string());
                let is_default = host.default_input_device().map(|dd| dd.name().ok()).flatten().map(|n| n == name).unwrap_or(false);
                out.push(DeviceEntry { index: i as u32, name, is_default: is_default && cfg!(not(windows)), is_loopback: false });
            }
        }
        Ok(out)
    }

    async fn open_device(&mut self, device_index: u32, format: PcmFormat) -> AudioResult<()> {
        // System-audio loopback path (default render endpoint, WASAPI loopback).
        if device_index == crate::loopback::LOOPBACK_DEVICE_INDEX {
            self.device_name = Some(crate::loopback::LOOPBACK_DEVICE_NAME.to_string());
            let (tx, rx) = bounded::<PcmFrame>(64);
            let (stop_tx, stop_rx) = bounded::<()>(1);
            let actual = crate::loopback::spawn_loopback_capture(format.clone(), tx, stop_rx)?;
            self.actual_format = Some(actual);
            self.rx = Some(rx);
            self.stop_tx = Some(stop_tx);
            return Ok(());
        }

        // System-audio monitor path (Linux: default sink's PulseAudio/PipeWire monitor).
        #[cfg(target_os = "linux")]
        if device_index == crate::monitor::MONITOR_DEVICE_INDEX {
            self.device_name = Some(crate::monitor::MONITOR_DEVICE_NAME.to_string());
            let (tx, rx) = bounded::<PcmFrame>(64);
            let (stop_tx, stop_rx) = bounded::<()>(1);
            let actual = crate::monitor::spawn_monitor_capture(format.clone(), tx, stop_rx)?;
            self.actual_format = Some(actual);
            self.rx = Some(rx);
            self.stop_tx = Some(stop_tx);
            return Ok(());
        }

        let host = cpal::default_host();
        let dev = host.devices().map_err(|e| AudioError::Wasapi(0, format!("device enumerate failed: {}", e)))?.nth(device_index as usize).ok_or(AudioError::DeviceNotFound(format!("index {}", device_index)))?;
        self.device_name = dev.name().ok();

        // choose default input config as basis
            // Query supported input configs and pick the best match for requested format.
            let sample_rate = format.sample_rate;
            let channels = format.channels as u16;

            let mut best_score: u64 = u64::MAX;
            let mut chosen_sf: Option<SampleFormat> = None;
            let mut chosen_min_sr: u32 = 0;
            let mut chosen_max_sr: u32 = 0;

            let supported = dev.supported_input_configs().map_err(|e| AudioError::Wasapi(0, format!("supported configs failed: {}", e)))?;
                let configs: Vec<_> = supported.collect();
                for cfg in configs.iter() {
                let sf = cfg.sample_format();
                let ch = cfg.channels() as u16;
                if ch != channels { continue; }
                let min_sr = cfg.min_sample_rate().0;
                let max_sr = cfg.max_sample_rate().0;
                let score: u64 = if sample_rate >= min_sr && sample_rate <= max_sr { 0 } else {
                    if sample_rate < min_sr { (min_sr - sample_rate) as u64 } else { (sample_rate - max_sr) as u64 }
                };
                // prefer matching bit depth
                let mut adj = score;
                if format.bit_depth == 32 && sf == SampleFormat::F32 { adj = adj.saturating_sub(1000); }
                if format.bit_depth == 16 && (sf == SampleFormat::I16 || sf == SampleFormat::U16) { adj = adj.saturating_sub(1000); }

                if adj < best_score {
                    best_score = adj;
                    chosen_sf = Some(sf);
                    chosen_min_sr = min_sr;
                    chosen_max_sr = max_sr;
                }
            }

            // If no exact-channel match found, relax channel requirement and pick the best available config.
            if chosen_sf.is_none() {
                for cfg in configs.iter() {
                    let sf = cfg.sample_format();
                    let min_sr = cfg.min_sample_rate().0;
                    let max_sr = cfg.max_sample_rate().0;
                    let score: u64 = if sample_rate >= min_sr && sample_rate <= max_sr { 0 } else {
                        if sample_rate < min_sr { (min_sr - sample_rate) as u64 } else { (sample_rate - max_sr) as u64 }
                    };
                    let mut adj = score;
                    if format.bit_depth == 32 && sf == SampleFormat::F32 { adj = adj.saturating_sub(1000); }
                    if format.bit_depth == 16 && (sf == SampleFormat::I16 || sf == SampleFormat::U16) { adj = adj.saturating_sub(1000); }

                    if adj < best_score {
                        best_score = adj;
                        chosen_sf = Some(sf);
                        chosen_min_sr = min_sr;
                        chosen_max_sr = max_sr;
                    }
                }
            }

            let chosen_sf = match chosen_sf {
                Some(s) => s,
                None => return Err(AudioError::UnsupportedFormat(format!("no supported config for {} channels", channels))),
            };

            // chosen_channels: prefer exact match, otherwise pick first config channels found
            let mut chosen_channels: u16 = channels;
            if let Some(cfg) = configs.iter().find(|c| c.sample_format() == chosen_sf) {
                chosen_channels = cfg.channels() as u16;
            }

            // negotiate sample rate within chosen range
            let negotiated_sample_rate: u32 = if sample_rate >= chosen_min_sr && sample_rate <= chosen_max_sr { sample_rate } else if sample_rate < chosen_min_sr { chosen_min_sr } else { chosen_max_sr };
            let bit_depth = match chosen_sf { SampleFormat::F32 => 32, SampleFormat::I16 => 16, SampleFormat::U16 => 16, _ => 16 };
            // actual_format reports the pipeline-facing format (requested channel count preserved)
            self.actual_format = Some(PcmFormat { sample_rate: negotiated_sample_rate, channels, bit_depth });

        // channel for frames and stop signal
        let (tx, rx) = bounded::<PcmFrame>(64);
        let (stop_tx, stop_rx) = bounded::<()>(1);
        self.rx = Some(rx);
        self.stop_tx = Some(stop_tx);

        // spawn capture thread
        let dev2 = dev.clone();
        // prepare concrete values for the capture thread
        let negotiated_sr = negotiated_sample_rate;
        let chosen_sf_clone = chosen_sf;
        // device-provided channel count (may differ from requested channels)
        let device_channels_for_thread = chosen_channels;
        let requested_channels = channels;

        thread::spawn(move || {
            let config = StreamConfig { channels: device_channels_for_thread, sample_rate: cpal::SampleRate(negotiated_sr), buffer_size: cpal::BufferSize::Default };
            let (frame_tx, stop_rx_inner) = (tx, stop_rx);
            let mut seq: u64 = 0;
            let err_fn = |e| eprintln!("cpal stream error: {}", e);
            let stream_res = match chosen_sf_clone {
                SampleFormat::F32 => dev2.build_input_stream(&config, move |data: &[f32], _| {
                    // convert f32 samples to i16 and perform channel mapping if necessary
                    let mut buf = Vec::with_capacity(data.len() * 2 * (requested_channels as usize));
                    if device_channels_for_thread == requested_channels {
                        for &s in data { let v = (s * i16::MAX as f32) as i16; buf.extend_from_slice(&v.to_le_bytes()); }
                    } else if device_channels_for_thread == 1 && requested_channels == 2 {
                        // duplicate mono -> stereo
                        for &s in data {
                            let v = (s * i16::MAX as f32) as i16;
                            buf.extend_from_slice(&v.to_le_bytes());
                            buf.extend_from_slice(&v.to_le_bytes());
                        }
                    } else {
                        // fallback: down/upmix naive (repeat channels as needed)
                        let dev_ch = device_channels_for_thread as usize;
                        let req_ch = requested_channels as usize;
                        let frames = data.len() / dev_ch;
                        for f in 0..frames {
                            for rch in 0..req_ch {
                                let s = data[f*dev_ch + (rch % dev_ch)];
                                let v = (s * i16::MAX as f32) as i16;
                                buf.extend_from_slice(&v.to_le_bytes());
                            }
                        }
                    }
                    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros() as u64;
                    let frame_count = (data.len() as u32) / config.channels as u32;
                    let pf = PcmFrame { data: buf, format: PcmFormat { sample_rate: config.sample_rate.0, channels: requested_channels as u16, bit_depth: 16 }, timestamp_us: now/1000, sequence: seq, frame_count };
                    let _ = frame_tx.send(pf);
                    seq = seq.wrapping_add(1);
                }, err_fn, None),
                SampleFormat::I16 => dev2.build_input_stream(&config, move |data: &[i16], _| {
                    let buf = map_i16_channels(data, device_channels_for_thread as usize, requested_channels as usize);
                    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros() as u64;
                    let frame_count = (data.len() as u32) / config.channels as u32;
                    let pf = PcmFrame { data: buf, format: PcmFormat { sample_rate: config.sample_rate.0, channels: requested_channels as u16, bit_depth: 16 }, timestamp_us: now/1000, sequence: seq, frame_count };
                    let _ = frame_tx.send(pf);
                    seq = seq.wrapping_add(1);
                }, err_fn, None),
                SampleFormat::U16 => dev2.build_input_stream(&config, move |data: &[u16], _| {
                    let buf = map_u16_channels(data, device_channels_for_thread as usize, requested_channels as usize);
                    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros() as u64;
                    let frame_count = (data.len() as u32) / config.channels as u32;
                    let pf = PcmFrame { data: buf, format: PcmFormat { sample_rate: config.sample_rate.0, channels: requested_channels as u16, bit_depth: 16 }, timestamp_us: now/1000, sequence: seq, frame_count };
                    let _ = frame_tx.send(pf);
                    seq = seq.wrapping_add(1);
                }, err_fn, None),
                _ => dev2.build_input_stream(&config, move |data: &[f32], _| {
                    let buf = map_f32_channels(data, device_channels_for_thread as usize, requested_channels as usize);
                    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros() as u64;
                    let frame_count = (data.len() as u32) / config.channels as u32;
                    let pf = PcmFrame { data: buf, format: PcmFormat { sample_rate: config.sample_rate.0, channels: requested_channels as u16, bit_depth: 16 }, timestamp_us: now/1000, sequence: seq, frame_count };
                    let _ = frame_tx.send(pf);
                    seq = seq.wrapping_add(1);
                }, err_fn, None),
            };

            if let Ok(s) = stream_res { let _ = s.play(); }

            // wait for stop signal
            let _ = stop_rx_inner.recv();
            // when stop received, stream will be dropped here
        });

        Ok(())
    }

    async fn start(&mut self) -> AudioResult<()> {
        self.active = true;
        Ok(())
    }

    async fn stop(&mut self) -> AudioResult<()> {
        self.active = false;
        if let Some(tx) = self.stop_tx.take() { let _ = tx.send(()); }
        Ok(())
    }

    async fn close(&mut self) -> AudioResult<()> {
        self.stop().await
    }

    async fn next_frame(&mut self) -> AudioResult<Option<PcmFrame>> {
        if !self.active { return Ok(None); }
        match self.rx.as_ref() {
            Some(r) => {
                // block in a spawn_blocking so we don't block executor
                let recv = r.clone();
                let res = tokio::task::spawn_blocking(move || recv.recv()).await.map_err(|e| AudioError::Wasapi(0, format!("recv join error: {}", e)))?;
                match res {
                    Ok(f) => Ok(Some(f)),
                    Err(_) => Ok(None),
                }
            }
            None => Ok(None),
        }
    }

    fn is_active(&self) -> bool { self.active }

    fn actual_format(&self) -> Option<PcmFormat> { self.actual_format.clone() }

    fn device_name(&self) -> Option<String> { self.device_name.clone() }
}

impl Default for WasapiBackend { fn default() -> Self { Self::new() } }

// Helper: map interleaved i16 samples from device channel count to requested channel count.
// This mirrors the mapping logic used in the capture callbacks and is exposed for unit testing.
pub(crate) fn map_i16_channels(data: &[i16], device_channels: usize, requested_channels: usize) -> Vec<u8> {
    if device_channels == requested_channels {
        // direct pass-through
        let mut out = Vec::with_capacity(data.len() * 2);
        for &s in data { out.extend_from_slice(&s.to_le_bytes()); }
        return out;
    }

    let dev_ch = device_channels;
    let req_ch = requested_channels;
    let frames = if dev_ch == 0 { 0 } else { data.len() / dev_ch };
    let mut out = Vec::with_capacity(frames * req_ch * 2);
    if dev_ch == 1 && req_ch == 2 {
        // duplicate mono -> stereo
        for f in 0..frames {
            let v = data[f];
            out.extend_from_slice(&v.to_le_bytes());
            out.extend_from_slice(&v.to_le_bytes());
        }
        return out;
    }

    // General naive up/downmix: repeat or wrap channels as needed
    for f in 0..frames {
        for rch in 0..req_ch {
            let v = data[f * dev_ch + (rch % dev_ch)];
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

pub(crate) fn map_u16_channels(data: &[u16], device_channels: usize, requested_channels: usize) -> Vec<u8> {
    if device_channels == requested_channels {
        let mut out = Vec::with_capacity(data.len() * 2);
        for &s in data {
            let v = s as i16;
            out.extend_from_slice(&v.to_le_bytes());
        }
        return out;
    }

    let dev_ch = device_channels;
    let req_ch = requested_channels;
    let frames = if dev_ch == 0 { 0 } else { data.len() / dev_ch };
    let mut out = Vec::with_capacity(frames * req_ch * 2);
    if dev_ch == 1 && req_ch == 2 {
        for f in 0..frames {
            let v = data[f] as i16;
            out.extend_from_slice(&v.to_le_bytes());
            out.extend_from_slice(&v.to_le_bytes());
        }
        return out;
    }

    for f in 0..frames {
        for rch in 0..req_ch {
            let vv = data[f * dev_ch + (rch % dev_ch)];
            let v = vv as i16;
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

pub(crate) fn map_f32_channels(data: &[f32], device_channels: usize, requested_channels: usize) -> Vec<u8> {
    if device_channels == requested_channels {
        let mut out = Vec::with_capacity(data.len() * 2);
        for &s in data {
            let v = (s * i16::MAX as f32) as i16;
            out.extend_from_slice(&v.to_le_bytes());
        }
        return out;
    }

    let dev_ch = device_channels;
    let req_ch = requested_channels;
    let frames = if dev_ch == 0 { 0 } else { data.len() / dev_ch };
    let mut out = Vec::with_capacity(frames * req_ch * 2);
    if dev_ch == 1 && req_ch == 2 {
        for f in 0..frames {
            let v = (data[f] * i16::MAX as f32) as i16;
            out.extend_from_slice(&v.to_le_bytes());
            out.extend_from_slice(&v.to_le_bytes());
        }
        return out;
    }

    for f in 0..frames {
        for rch in 0..req_ch {
            let s = data[f * dev_ch + (rch % dev_ch)];
            let v = (s * i16::MAX as f32) as i16;
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

#[cfg(test)]
mod wasapi_tests {
    use super::map_i16_channels;

    /// The UI's device dropdown is fed by enumerate_devices(); confirm the
    /// "System Audio (Loopback)" entry is present and first, with the loopback
    /// sentinel index that routes open_device() to the WASAPI loopback path.
    #[cfg(windows)]
    #[tokio::test]
    async fn enumerate_lists_loopback_device_first() {
        use crate::backend::AudioBackend;
        let backend = super::WasapiBackend::new();
        let devs = backend.enumerate_devices().await.expect("enumerate");
        assert!(!devs.is_empty());
        let first = &devs[0];
        assert!(first.is_loopback, "first device should be the loopback entry");
        assert_eq!(first.index, crate::loopback::LOOPBACK_DEVICE_INDEX);
        assert_eq!(first.name, crate::loopback::LOOPBACK_DEVICE_NAME);
    }

    #[test]
    fn mono_to_stereo_mapping_i16() {
        // create 4 mono samples
        let mono: Vec<i16> = vec![1000i16, -1000i16, 12345i16, -12345i16];
        let out = map_i16_channels(&mono, 1, 2);
        // 4 frames * 2 channels * 2 bytes
        assert_eq!(out.len(), 4 * 2 * 2);
        for i in 0..4 {
            let l = i16::from_le_bytes([out[i * 4], out[i * 4 + 1]]);
            let r = i16::from_le_bytes([out[i * 4 + 2], out[i * 4 + 3]]);
            assert_eq!(l, mono[i]);
            assert_eq!(r, mono[i]);
        }
    }
}
