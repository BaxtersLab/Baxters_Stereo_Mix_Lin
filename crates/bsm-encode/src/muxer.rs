use std::path::Path;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::fs::File;
use tracing::debug;

use crate::encoder::AudioPacket;
use crate::flac_enc;
use bsm_core::{EncodeError, EncodeResult, PcmFormat};

/// Container formats supported by the muxer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerFormat { Mp3, Flac, Wav }

impl ContainerFormat {
    pub fn extension(&self) -> &'static str {
        match self { ContainerFormat::Mp3 => "mp3", ContainerFormat::Flac => "flac", ContainerFormat::Wav => "wav" }
    }
}

/// Trait to implement container writers.
pub trait AudioMuxer: Send {
    fn open(&mut self, path: &Path, pcm_format: &PcmFormat, codec_name: &str, bitrate_kbps: u32) -> EncodeResult<()>;
    fn write_packet(&mut self, pkt: &AudioPacket) -> EncodeResult<()>;
    fn finalize(&mut self) -> EncodeResult<()>;
    fn container_format(&self) -> ContainerFormat;
}

// ---------------------------------------------------------------------------
// WAV muxer (native)
// ---------------------------------------------------------------------------

pub struct WavMuxer {
    writer: Option<BufWriter<File>>,
    bytes_written: u32,
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
}

impl WavMuxer {
    pub fn new() -> Self {
        Self { writer: None, bytes_written: 0, channels: 2, sample_rate: 48000, bits_per_sample: 16 }
    }

    fn write_riff_header(w: &mut impl Write, channels: u16, sample_rate: u32, bits: u16) -> EncodeResult<()> {
        let block_align = channels * (bits / 8);
        let byte_rate = sample_rate * block_align as u32;
        w.write_all(b"RIFF").map_err(|e| EncodeError::MuxFailed(e.to_string()))?;
        w.write_all(&0u32.to_le_bytes()).map_err(|e| EncodeError::MuxFailed(e.to_string()))?; // placeholder
        w.write_all(b"WAVE").map_err(|e| EncodeError::MuxFailed(e.to_string()))?;
        w.write_all(b"fmt ").map_err(|e| EncodeError::MuxFailed(e.to_string()))?;
        w.write_all(&16u32.to_le_bytes()).map_err(|e| EncodeError::MuxFailed(e.to_string()))?;
        w.write_all(&1u16.to_le_bytes()).map_err(|e| EncodeError::MuxFailed(e.to_string()))?;
        w.write_all(&channels.to_le_bytes()).map_err(|e| EncodeError::MuxFailed(e.to_string()))?;
        w.write_all(&sample_rate.to_le_bytes()).map_err(|e| EncodeError::MuxFailed(e.to_string()))?;
        w.write_all(&byte_rate.to_le_bytes()).map_err(|e| EncodeError::MuxFailed(e.to_string()))?;
        w.write_all(&block_align.to_le_bytes()).map_err(|e| EncodeError::MuxFailed(e.to_string()))?;
        w.write_all(&bits.to_le_bytes()).map_err(|e| EncodeError::MuxFailed(e.to_string()))?;
        w.write_all(b"data").map_err(|e| EncodeError::MuxFailed(e.to_string()))?;
        w.write_all(&0u32.to_le_bytes()).map_err(|e| EncodeError::MuxFailed(e.to_string()))?; // placeholder
        Ok(())
    }
}

impl AudioMuxer for WavMuxer {
    fn open(&mut self, path: &Path, pcm_format: &PcmFormat, _codec_name: &str, _bitrate_kbps: u32) -> EncodeResult<()> {
        let file = File::create(path).map_err(|e| EncodeError::OutputFile(e.to_string()))?;
        let mut w = BufWriter::new(file);
        self.channels = pcm_format.channels as u16;
        self.sample_rate = pcm_format.sample_rate;
        self.bits_per_sample = pcm_format.bit_depth as u16;
        Self::write_riff_header(&mut w, self.channels, self.sample_rate, self.bits_per_sample)?;
        self.writer = Some(w);
        debug!("WavMuxer opened {:?}", path);
        Ok(())
    }

    fn write_packet(&mut self, pkt: &AudioPacket) -> EncodeResult<()> {
        let w = self.writer.as_mut().ok_or_else(|| EncodeError::OutputFile("not open".into()))?;
        w.write_all(&pkt.data).map_err(|e| EncodeError::MuxFailed(e.to_string()))?;
        self.bytes_written = self.bytes_written.wrapping_add(pkt.data.len() as u32);
        Ok(())
    }

    fn finalize(&mut self) -> EncodeResult<()> {
        if let Some(mut w) = self.writer.take() {
            w.flush().map_err(|e| EncodeError::MuxFailed(e.to_string()))?;
            let mut file = w.into_inner().map_err(|e| EncodeError::OutputFile(e.into_error().to_string()))?;
            let riff_sz = 36u32.wrapping_add(self.bytes_written);
            file.seek(SeekFrom::Start(4)).map_err(|e| EncodeError::OutputFile(e.to_string()))?;
            file.write_all(&riff_sz.to_le_bytes()).map_err(|e| EncodeError::OutputFile(e.to_string()))?;
            file.seek(SeekFrom::Start(40)).map_err(|e| EncodeError::OutputFile(e.to_string()))?;
            file.write_all(&self.bytes_written.to_le_bytes()).map_err(|e| EncodeError::OutputFile(e.to_string()))?;
        }
        debug!("WavMuxer finalized");
        Ok(())
    }

    fn container_format(&self) -> ContainerFormat { ContainerFormat::Wav }
}

// ---------------------------------------------------------------------------
// MP3 muxer (native, via vendored LAME)
// ---------------------------------------------------------------------------

pub struct Mp3Muxer {
    writer: Option<BufWriter<File>>,
    encoder: Option<mp3lame_encoder::Encoder>,
    mp3_buf: Vec<u8>,
    channels: u16,
}

impl Mp3Muxer {
    pub fn new() -> Self {
        Self { writer: None, encoder: None, mp3_buf: Vec::new(), channels: 2 }
    }
}

/// Map a u32 bitrate to the nearest `mp3lame_encoder::Bitrate` variant.
fn nearest_bitrate(kbps: u32) -> mp3lame_encoder::Bitrate {
    use mp3lame_encoder::Bitrate;
    match kbps {
        0..=12 => Bitrate::Kbps8,
        13..=20 => Bitrate::Kbps16,
        21..=28 => Bitrate::Kbps24,
        29..=36 => Bitrate::Kbps32,
        37..=44 => Bitrate::Kbps40,
        45..=56 => Bitrate::Kbps48,
        57..=72 => Bitrate::Kbps64,
        73..=88 => Bitrate::Kbps80,
        89..=104 => Bitrate::Kbps96,
        105..=120 => Bitrate::Kbps112,
        121..=144 => Bitrate::Kbps128,
        145..=176 => Bitrate::Kbps160,
        177..=208 => Bitrate::Kbps192,
        209..=240 => Bitrate::Kbps224,
        241..=288 => Bitrate::Kbps256,
        _ => Bitrate::Kbps320,
    }
}

impl AudioMuxer for Mp3Muxer {
    fn open(&mut self, path: &Path, pcm_format: &PcmFormat, _codec_name: &str, bitrate_kbps: u32) -> EncodeResult<()> {
        let file = File::create(path).map_err(|e| EncodeError::OutputFile(e.to_string()))?;
        self.writer = Some(BufWriter::new(file));
        self.channels = pcm_format.channels as u16;

        let mut builder = mp3lame_encoder::Builder::new()
            .ok_or_else(|| EncodeError::OpenFailed("LAME init failed".into()))?;
        builder.set_num_channels(pcm_format.channels as u8)
            .map_err(|e| EncodeError::OpenFailed(format!("LAME channels: {e}")))?;
        builder.set_sample_rate(pcm_format.sample_rate)
            .map_err(|e| EncodeError::OpenFailed(format!("LAME sample rate: {e}")))?;
        builder.set_brate(nearest_bitrate(bitrate_kbps))
            .map_err(|e| EncodeError::OpenFailed(format!("LAME bitrate: {e}")))?;
        builder.set_quality(mp3lame_encoder::Quality::Best)
            .map_err(|e| EncodeError::OpenFailed(format!("LAME quality: {e}")))?;

        self.encoder = Some(builder.build()
            .map_err(|e| EncodeError::OpenFailed(format!("LAME build: {e}")))?);

        debug!("Mp3Muxer opened {:?}", path);
        Ok(())
    }

    fn write_packet(&mut self, pkt: &AudioPacket) -> EncodeResult<()> {
        let encoder = self.encoder.as_mut()
            .ok_or_else(|| EncodeError::EncodeFailed("MP3 encoder not initialised".into()))?;
        let writer = self.writer.as_mut()
            .ok_or_else(|| EncodeError::OutputFile("not open".into()))?;

        // Convert raw bytes → interleaved i16 samples
        let samples: Vec<i16> = pkt.data
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();

        // Reserve enough space and encode
        let needed = mp3lame_encoder::max_required_buffer_size(samples.len());
        self.mp3_buf.clear();
        self.mp3_buf.reserve(needed);

        let input = mp3lame_encoder::InterleavedPcm(&samples);
        encoder.encode_to_vec(input, &mut self.mp3_buf)
            .map_err(|e| EncodeError::EncodeFailed(format!("LAME encode: {e}")))?;

        if !self.mp3_buf.is_empty() {
            writer.write_all(&self.mp3_buf)
                .map_err(|e| EncodeError::MuxFailed(e.to_string()))?;
        }
        Ok(())
    }

    fn finalize(&mut self) -> EncodeResult<()> {
        if let Some(ref mut encoder) = self.encoder {
            self.mp3_buf.clear();
            self.mp3_buf.reserve(7200);
            encoder.flush_to_vec::<mp3lame_encoder::FlushNoGap>(&mut self.mp3_buf)
                .map_err(|e| EncodeError::EncodeFailed(format!("LAME flush: {e}")))?;
            if let Some(ref mut w) = self.writer {
                if !self.mp3_buf.is_empty() {
                    w.write_all(&self.mp3_buf)
                        .map_err(|e| EncodeError::MuxFailed(e.to_string()))?;
                }
                w.flush().map_err(|e| EncodeError::MuxFailed(e.to_string()))?;
            }
        }
        debug!("Mp3Muxer finalized");
        Ok(())
    }

    fn container_format(&self) -> ContainerFormat { ContainerFormat::Mp3 }
}

// ---------------------------------------------------------------------------
// FLAC muxer (pure-Rust encoder via flac_enc)
// ---------------------------------------------------------------------------

const FLAC_BLOCK_SIZE: usize = 4096;

pub struct FlacMuxer {
    writer: Option<BufWriter<File>>,
    pcm_buffer: Vec<i16>,
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
    frame_number: u32,
    total_samples: u64,
}

impl FlacMuxer {
    pub fn new() -> Self {
        Self {
            writer: None,
            pcm_buffer: Vec::new(),
            channels: 2,
            sample_rate: 48000,
            bits_per_sample: 16,
            frame_number: 0,
            total_samples: 0,
        }
    }

    /// Write queued full blocks.
    fn flush_blocks(&mut self) -> EncodeResult<()> {
        let samples_per_block = FLAC_BLOCK_SIZE * self.channels as usize;
        while self.pcm_buffer.len() >= samples_per_block {
            let block: Vec<i16> = self.pcm_buffer.drain(..samples_per_block).collect();
            let frame = flac_enc::encode_frame(
                &block,
                self.channels,
                self.sample_rate,
                self.bits_per_sample,
                self.frame_number,
            );
            self.frame_number += 1;
            self.total_samples += FLAC_BLOCK_SIZE as u64;

            let w = self.writer.as_mut()
                .ok_or_else(|| EncodeError::OutputFile("not open".into()))?;
            w.write_all(&frame).map_err(|e| EncodeError::MuxFailed(e.to_string()))?;
        }
        Ok(())
    }
}

impl AudioMuxer for FlacMuxer {
    fn open(&mut self, path: &Path, pcm_format: &PcmFormat, _codec_name: &str, _bitrate_kbps: u32) -> EncodeResult<()> {
        // read+write: finalize() reads back STREAMINFO to patch total_samples,
        // so the handle must have read access (a write-only File::create handle
        // fails read_exact with "Access is denied" on Windows).
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|e| EncodeError::OutputFile(e.to_string()))?;
        let mut w = BufWriter::new(file);
        self.channels = pcm_format.channels as u16;
        self.sample_rate = pcm_format.sample_rate;
        self.bits_per_sample = pcm_format.bit_depth as u16;
        self.frame_number = 0;
        self.total_samples = 0;
        self.pcm_buffer.clear();

        // Write fLaC marker
        w.write_all(b"fLaC").map_err(|e| EncodeError::MuxFailed(e.to_string()))?;

        // Metadata block header: is_last=1, type=STREAMINFO(0), length=34
        w.write_all(&[0x80, 0x00, 0x00, 0x22])
            .map_err(|e| EncodeError::MuxFailed(e.to_string()))?;

        // STREAMINFO with total_samples = 0 (patched on finalize)
        let si = flac_enc::build_streaminfo(
            FLAC_BLOCK_SIZE as u16,
            FLAC_BLOCK_SIZE as u16,
            self.sample_rate,
            self.channels,
            self.bits_per_sample,
            0,
        );
        w.write_all(&si).map_err(|e| EncodeError::MuxFailed(e.to_string()))?;

        self.writer = Some(w);
        debug!("FlacMuxer opened {:?}", path);
        Ok(())
    }

    fn write_packet(&mut self, pkt: &AudioPacket) -> EncodeResult<()> {
        // Convert raw bytes → i16 and buffer
        let samples: Vec<i16> = pkt.data
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        self.pcm_buffer.extend_from_slice(&samples);

        // Encode any complete blocks
        self.flush_blocks()
    }

    fn finalize(&mut self) -> EncodeResult<()> {
        // Encode remaining samples as a shorter final block
        if !self.pcm_buffer.is_empty() {
            let remaining: Vec<i16> = self.pcm_buffer.drain(..).collect();
            let block_samples = remaining.len() / self.channels as usize;
            if block_samples > 0 {
                let frame = flac_enc::encode_frame(
                    &remaining[..block_samples * self.channels as usize],
                    self.channels,
                    self.sample_rate,
                    self.bits_per_sample,
                    self.frame_number,
                );
                self.total_samples += block_samples as u64;
                if let Some(ref mut w) = self.writer {
                    w.write_all(&frame).map_err(|e| EncodeError::MuxFailed(e.to_string()))?;
                }
            }
        }

        // Patch total_samples in STREAMINFO
        if let Some(mut w) = self.writer.take() {
            w.flush().map_err(|e| EncodeError::MuxFailed(e.to_string()))?;
            let mut file = w.into_inner()
                .map_err(|e| EncodeError::OutputFile(e.into_error().to_string()))?;
            let ts = self.total_samples;
            // STREAMINFO data starts at file offset 8.
            // total_samples is 36 bits at bit offset 108 within STREAMINFO:
            //   byte 13 (offset 21): upper nibble = (bps-1)[3:0], lower nibble = ts[35:32]
            //   bytes 14-17 (offsets 22-25): ts[31:0]
            file.seek(SeekFrom::Start(21)).map_err(|e| EncodeError::OutputFile(e.to_string()))?;
            let mut patch = [0u8; 5];
            file.read_exact(&mut patch).map_err(|e| EncodeError::OutputFile(e.to_string()))?;
            patch[0] = (patch[0] & 0xF0) | ((ts >> 32) & 0x0F) as u8;
            patch[1] = (ts >> 24) as u8;
            patch[2] = (ts >> 16) as u8;
            patch[3] = (ts >> 8) as u8;
            patch[4] = ts as u8;
            file.seek(SeekFrom::Start(21)).map_err(|e| EncodeError::OutputFile(e.to_string()))?;
            file.write_all(&patch).map_err(|e| EncodeError::OutputFile(e.to_string()))?;
        }
        debug!("FlacMuxer finalized — {} total samples", self.total_samples);
        Ok(())
    }

    fn container_format(&self) -> ContainerFormat { ContainerFormat::Flac }
}

pub fn create_muxer(format: ContainerFormat) -> Box<dyn AudioMuxer> {
    match format {
        ContainerFormat::Wav => Box::new(WavMuxer::new()),
        ContainerFormat::Mp3 => Box::new(Mp3Muxer::new()),
        ContainerFormat::Flac => Box::new(FlacMuxer::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;
    use bsm_core::PcmFormat;

    fn dummy_pcm() -> PcmFormat { PcmFormat { sample_rate: 48000, channels: 2, bit_depth: 16 } }

    #[test]
    fn wav_writes_and_patches_header() {
        let tmp = NamedTempFile::new().unwrap();
        let mut m = WavMuxer::new();
        m.open(tmp.path(), &dummy_pcm(), "pcm_s16le", 128).unwrap();
        m.write_packet(&AudioPacket { data: vec![0u8; 4], pts:0,duration:1024,is_key:true }).unwrap();
        m.finalize().unwrap();
        let bytes = std::fs::read(tmp.path()).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");

        // check common header fields at canonical offsets
        let channels = u16::from_le_bytes(bytes[22..24].try_into().unwrap());
        let sample_rate = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
        let bits_per_sample = u16::from_le_bytes(bytes[34..36].try_into().unwrap());
        let data_chunk_size = u32::from_le_bytes(bytes[40..44].try_into().unwrap());

        assert_eq!(channels, 2);
        assert_eq!(sample_rate, 48000);
        assert_eq!(bits_per_sample, 16);
        assert_eq!(data_chunk_size, 4);
    }
}
