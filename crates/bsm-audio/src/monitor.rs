//! Linux system-audio capture via the PulseAudio / PipeWire **monitor** source.
//!
//! This is the Linux counterpart of the Windows WASAPI [`crate::loopback`] path:
//! the real "record whatever is playing" capture. On Linux every PulseAudio /
//! PipeWire sink automatically exposes a `.monitor` source carrying the post-mix
//! output, so — unlike Windows 10's hidden "Stereo Mix" — no special device or
//! trickery is needed.
//!
//! We record from `@DEFAULT_MONITOR@` (the *default sink's* monitor) using the
//! libpulse "simple" blocking API. That API is served by both PulseAudio and
//! PipeWire (via `pipewire-pulse`), so the same code works on either sound
//! server. PulseAudio performs any needed resampling/format conversion, so we
//! request S16LE at the pipeline's channel count and rate and get frames back in
//! exactly the shape the encoder expects — matching the WASAPI loopback output.
//!
//! Capture runs on a dedicated thread (the libpulse handle is not shared); only
//! decoded [`PcmFrame`]s cross back to the async backend via a channel.

use bsm_core::{AudioError, PcmFormat, PcmFrame};
use crossbeam_channel::{Receiver as CbReceiver, Sender as CbSender};

/// Synthetic device index the UI/backend uses to mean "System Audio (Monitor)".
/// Distinct from the Windows loopback sentinel ([`crate::loopback::LOOPBACK_DEVICE_INDEX`]
/// = `u32::MAX`) so device routing can never collide.
pub const MONITOR_DEVICE_INDEX: u32 = u32::MAX - 1;
pub const MONITOR_DEVICE_NAME: &str = "System Audio (Monitor)";

/// Frames captured per read chunk — ~20 ms at `rate` (keeps stop latency low and
/// frames timely). Pure, so it is unit-tested on every platform. (Only wired into
/// live capture on Linux; unused in a non-Linux non-test build.)
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn frames_per_chunk(rate: u32) -> usize {
    (rate / 50).max(1) as usize
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use libpulse_binding::def::BufferAttr;
    use libpulse_binding::sample::{Format, Spec};
    use libpulse_binding::stream::Direction;
    use libpulse_simple_binding::Simple;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Open the default sink's monitor and spawn the capture thread. Returns the
    /// negotiated pipeline format (blocks only until the stream is opened).
    pub(crate) fn spawn(
        requested: PcmFormat,
        frame_tx: CbSender<PcmFrame>,
        stop_rx: CbReceiver<()>,
    ) -> Result<PcmFormat, AudioError> {
        let channels = requested.channels.max(1);
        let rate = if requested.sample_rate == 0 { 48000 } else { requested.sample_rate };

        let spec = Spec { format: Format::S16le, channels: channels as u8, rate };
        if !spec.is_valid() {
            return Err(AudioError::UnsupportedFormat(format!(
                "invalid PulseAudio spec: {channels}ch @ {rate}Hz S16LE"
            )));
        }

        let bytes_per_frame_setup = channels as usize * 2; // S16LE
        let chunk_bytes_setup = frames_per_chunk(rate) * bytes_per_frame_setup;

        // Ask the server for ~20 ms fragments instead of letting it choose.
        //
        // Passing `None` here cost **1.94 s on the first read**, measured
        // 2026-08-14: the server picked its default record buffer and the first
        // `read()` blocked until that buffer filled. Steady state was already
        // exactly realtime (20.08 ms per 20 ms chunk), so the whole shortfall in
        // `monitor_record.rs` — 0.70 s of audio in a 2500 ms window — was this
        // one stall, not a throughput problem.
        //
        // `fragsize` is the only field that matters for a record stream; the
        // playback fields stay `u32::MAX`, which means "server default".
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: chunk_bytes_setup as u32,
        };

        // `@DEFAULT_MONITOR@` = the monitor source of the current default sink.
        let simple = Simple::new(
            None,                      // connect to the default server
            "Baxter's Stereo Mix",     // application name (shown in pavucontrol)
            Direction::Record,
            Some("@DEFAULT_MONITOR@"), // default sink's monitor source
            "System audio (monitor)",  // stream description
            &spec,
            None,                      // default channel map
            Some(&attr),               // ~20 ms fragments — see comment above
        )
        .map_err(|e| {
            AudioError::DeviceOpenFailed(format!(
                "PulseAudio/PipeWire monitor open failed: {e} \
                 (is a PulseAudio or PipeWire sound server running?)"
            ))
        })?;

        let negotiated = PcmFormat { sample_rate: rate, channels, bit_depth: 16 };
        let frame_format = negotiated.clone(); // moved into the capture thread
        let bytes_per_frame = channels as usize * 2; // S16LE
        let chunk_bytes = frames_per_chunk(rate) * bytes_per_frame;

        std::thread::spawn(move || {
            let mut seq: u64 = 0;
            let mut buf = vec![0u8; chunk_bytes];
            loop {
                if stop_rx.try_recv().is_ok() {
                    break;
                }
                // Blocking read of exactly `buf.len()` bytes (~20 ms of audio).
                // A monitor source emits silence when nothing plays, so this
                // never stalls on a quiet system.
                if simple.read(&mut buf).is_err() {
                    break; // server gone / stream error
                }
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let pf = PcmFrame {
                    data: buf.clone(),
                    format: frame_format.clone(),
                    timestamp_us: now,
                    sequence: seq,
                    frame_count: (buf.len() / bytes_per_frame) as u32,
                };
                if frame_tx.send(pf).is_err() {
                    break; // consumer gone
                }
                seq = seq.wrapping_add(1);
            }
            // `simple` is dropped here → the record stream is closed.
        });

        Ok(negotiated)
    }
}

/// Spawn PulseAudio/PipeWire monitor capture. On non-Linux this is unsupported.
/// (Only called from `open_device` on Linux; the non-Linux definition exists for
/// symmetry and is exercised by a unit test.)
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn spawn_monitor_capture(
    requested: PcmFormat,
    frame_tx: CbSender<PcmFrame>,
    stop_rx: CbReceiver<()>,
) -> Result<PcmFormat, AudioError> {
    #[cfg(target_os = "linux")]
    {
        imp::spawn(requested, frame_tx, stop_rx)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (requested, frame_tx, stop_rx);
        Err(AudioError::UnsupportedFormat(
            "PipeWire/PulseAudio monitor capture is Linux-only (Windows uses WASAPI loopback)"
                .into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_per_chunk_is_about_20ms() {
        assert_eq!(frames_per_chunk(48000), 960); // 48000/50
        assert_eq!(frames_per_chunk(44100), 882);
        assert!(frames_per_chunk(0) >= 1, "must never be zero (no divide-by-zero downstream)");
    }

    #[test]
    fn monitor_sentinel_is_distinct_from_loopback() {
        assert_ne!(MONITOR_DEVICE_INDEX, crate::loopback::LOOPBACK_DEVICE_INDEX);
    }

    /// Off Linux the monitor path is a clean Unsupported error (Windows uses
    /// WASAPI loopback instead). Runs on the Windows CI.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn monitor_capture_unsupported_off_linux() {
        let (tx, _rx) = crossbeam_channel::bounded::<PcmFrame>(1);
        let (_stop_tx, stop_rx) = crossbeam_channel::bounded::<()>(1);
        let res = spawn_monitor_capture(
            PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 },
            tx,
            stop_rx,
        );
        assert!(matches!(res, Err(AudioError::UnsupportedFormat(_))));
    }
}
