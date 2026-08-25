//! Pure-Rust FLAC encoder — FIXED prediction order 1 with Rice coding.
//!
//! Produces standard-compliant FLAC files playable by any decoder.
//! No external dependencies required.

// ---------------------------------------------------------------------------
// Bit-stream writer (MSB-first)
// ---------------------------------------------------------------------------

pub(crate) struct BitWriter {
    buf: Vec<u8>,
    current: u8,
    bit_pos: u8, // 0..7 — number of bits written into `current`
}

impl BitWriter {
    pub fn new() -> Self {
        Self { buf: Vec::with_capacity(65536), current: 0, bit_pos: 0 }
    }

    /// Write the top `count` bits of `val` (MSB first).  `count` must be <= 64.
    pub fn write_bits(&mut self, val: u64, count: u8) {
        debug_assert!(count <= 64);
        for i in (0..count).rev() {
            let bit = ((val >> i) & 1) as u8;
            self.current |= bit << (7 - self.bit_pos);
            self.bit_pos += 1;
            if self.bit_pos == 8 {
                self.buf.push(self.current);
                self.current = 0;
                self.bit_pos = 0;
            }
        }
    }

    /// Unary code: `count` zeros followed by a one.
    pub fn write_unary(&mut self, count: u32) {
        for _ in 0..count {
            self.write_bits(0, 1);
        }
        self.write_bits(1, 1);
    }

    /// Pad with zeros to the next byte boundary.
    pub fn byte_align(&mut self) {
        if self.bit_pos > 0 {
            self.buf.push(self.current);
            self.current = 0;
            self.bit_pos = 0;
        }
    }

    /// Completed bytes so far (excludes any partial byte).
    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Consume and return all bytes (flushes partial byte with zero padding).
    pub fn into_bytes(mut self) -> Vec<u8> {
        self.byte_align();
        self.buf
    }
}

// ---------------------------------------------------------------------------
// CRC helpers (FLAC-specific polynomials)
// ---------------------------------------------------------------------------

/// CRC-8 — polynomial 0x07 (x^8 + x^2 + x + 1), init 0.
pub(crate) fn crc8(data: &[u8]) -> u8 {
    let mut crc: u8 = 0;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x07 } else { crc << 1 };
        }
    }
    crc
}

/// CRC-16 — polynomial 0x8005 (x^16 + x^15 + x^2 + 1), init 0.
pub(crate) fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x8005 } else { crc << 1 };
        }
    }
    crc
}

// ---------------------------------------------------------------------------
// STREAMINFO metadata block builder
// ---------------------------------------------------------------------------

/// Build the 34-byte STREAMINFO data.
pub(crate) fn build_streaminfo(
    min_block: u16,
    max_block: u16,
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
    total_samples: u64,
) -> [u8; 34] {
    let mut si = [0u8; 34];
    // min / max block size
    si[0] = (min_block >> 8) as u8;
    si[1] = min_block as u8;
    si[2] = (max_block >> 8) as u8;
    si[3] = max_block as u8;
    // min / max frame size — 0 = unknown
    // si[4..10] already zero
    // 20-bit sample rate | 3-bit (channels-1) | 5-bit (bps-1) | 36-bit total samples
    let sr = sample_rate;
    let ch = (channels - 1) as u32;
    let bps = (bits_per_sample - 1) as u32;
    let ts = total_samples;
    si[10] = (sr >> 12) as u8;
    si[11] = (sr >> 4) as u8;
    si[12] = ((sr & 0xF) << 4) as u8 | ((ch & 0x7) << 1) as u8 | ((bps >> 4) & 0x1) as u8;
    si[13] = ((bps & 0xF) << 4) as u8 | ((ts >> 32) & 0xF) as u8;
    si[14] = (ts >> 24) as u8;
    si[15] = (ts >> 16) as u8;
    si[16] = (ts >> 8) as u8;
    si[17] = ts as u8;
    // Bytes 18-33: MD5 signature — leave as zeros
    si
}

// ---------------------------------------------------------------------------
// Frame encoding
// ---------------------------------------------------------------------------

/// 4-bit block-size code for the frame header.
fn block_size_code(bs: usize) -> u8 {
    match bs {
        192 => 1,
        576 => 2,
        1152 => 3,
        2304 => 4,
        4608 => 5,
        256 => 8,
        512 => 9,
        1024 => 10,
        2048 => 11,
        4096 => 12,
        8192 => 13,
        16384 => 14,
        32768 => 15,
        _ if bs <= 256 => 6,
        _ => 7,
    }
}

/// 4-bit sample-rate code for the frame header.
fn sample_rate_code(sr: u32) -> u8 {
    match sr {
        88200 => 1,
        176400 => 2,
        192000 => 3,
        8000 => 4,
        16000 => 5,
        22050 => 6,
        24000 => 7,
        32000 => 8,
        44100 => 9,
        48000 => 10,
        96000 => 11,
        _ if sr % 1000 == 0 && sr / 1000 <= 255 => 12,
        _ if sr <= 65535 => 13,
        _ => 14,
    }
}

/// 3-bit sample-size code for the frame header.
fn sample_size_code(bps: u16) -> u8 {
    match bps {
        8 => 1,
        12 => 2,
        16 => 4,
        20 => 5,
        24 => 6,
        32 => 7,
        _ => 0,
    }
}

/// UTF-8-like coding for the frame number (FLAC custom format).
fn write_utf8_coded(w: &mut BitWriter, val: u32) {
    if val < 0x80 {
        w.write_bits(val as u64, 8);
    } else if val < 0x800 {
        w.write_bits((0xC0 | (val >> 6)) as u64, 8);
        w.write_bits((0x80 | (val & 0x3F)) as u64, 8);
    } else if val < 0x10000 {
        w.write_bits((0xE0 | (val >> 12)) as u64, 8);
        w.write_bits((0x80 | ((val >> 6) & 0x3F)) as u64, 8);
        w.write_bits((0x80 | (val & 0x3F)) as u64, 8);
    } else if val < 0x200000 {
        w.write_bits((0xF0 | (val >> 18)) as u64, 8);
        w.write_bits((0x80 | ((val >> 12) & 0x3F)) as u64, 8);
        w.write_bits((0x80 | ((val >> 6) & 0x3F)) as u64, 8);
        w.write_bits((0x80 | (val & 0x3F)) as u64, 8);
    } else {
        w.write_bits((0xF8 | (val >> 24)) as u64, 8);
        w.write_bits((0x80 | ((val >> 18) & 0x3F)) as u64, 8);
        w.write_bits((0x80 | ((val >> 12) & 0x3F)) as u64, 8);
        w.write_bits((0x80 | ((val >> 6) & 0x3F)) as u64, 8);
        w.write_bits((0x80 | (val & 0x3F)) as u64, 8);
    }
}

/// Optimal Rice parameter for a partition of residuals (capped at 14).
fn choose_rice_param(residuals: &[i32]) -> u8 {
    if residuals.is_empty() {
        return 0;
    }
    let sum: u64 = residuals
        .iter()
        .map(|&r| {
            if r >= 0 {
                2 * r as u64
            } else {
                ((-2i64 * r as i64) - 1) as u64
            }
        })
        .sum();
    let mean = sum / residuals.len() as u64;
    if mean == 0 {
        return 0;
    }
    let k = (64 - mean.leading_zeros()).saturating_sub(1) as u8;
    k.min(14)
}

/// Encode one block of interleaved 16-bit PCM into a complete FLAC frame.
pub(crate) fn encode_frame(
    interleaved: &[i16],
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
    frame_number: u32,
) -> Vec<u8> {
    let block_size = interleaved.len() / channels as usize;

    // De-interleave into per-channel buffers
    let mut ch_data: Vec<Vec<i16>> = (0..channels as usize)
        .map(|_| Vec::with_capacity(block_size))
        .collect();
    for (i, &s) in interleaved.iter().enumerate() {
        ch_data[i % channels as usize].push(s);
    }

    let mut w = BitWriter::new();

    // ── Frame header ──────────────────────────────────────────────────
    // 14-bit sync  +  1-bit reserved(0)  +  1-bit blocking-strategy(0)
    w.write_bits(0b11111111_11111000u64, 16);

    let bs_code = block_size_code(block_size);
    let sr_code = sample_rate_code(sample_rate);
    w.write_bits(bs_code as u64, 4);
    w.write_bits(sr_code as u64, 4);

    // Channel assignment (independent stereo)
    w.write_bits((channels - 1) as u64, 4);

    // Sample size
    w.write_bits(sample_size_code(bits_per_sample) as u64, 3);

    // Reserved
    w.write_bits(0, 1);

    // Frame number (UTF-8 coded)
    write_utf8_coded(&mut w, frame_number);

    // Optional trailing block-size / sample-rate bytes
    if bs_code == 6 {
        w.write_bits((block_size - 1) as u64, 8);
    } else if bs_code == 7 {
        w.write_bits((block_size - 1) as u64, 16);
    }
    if sr_code == 12 {
        w.write_bits((sample_rate / 1000) as u64, 8);
    } else if sr_code == 13 {
        w.write_bits(sample_rate as u64, 16);
    } else if sr_code == 14 {
        w.write_bits((sample_rate / 10) as u64, 16);
    }

    // CRC-8 of header bytes written so far
    let hdr_crc = crc8(w.bytes());
    w.write_bits(hdr_crc as u64, 8);

    // ── Subframes ─────────────────────────────────────────────────────
    for ch in 0..channels as usize {
        encode_subframe(&mut w, &ch_data[ch], bits_per_sample);
    }

    // Byte-align after subframes
    w.byte_align();

    // CRC-16 over everything written so far (frame header + subframes + padding)
    let frame_crc = crc16(w.bytes());
    w.write_bits(frame_crc as u64, 16);

    w.into_bytes()
}

// ---------------------------------------------------------------------------
// Subframe encoding — chooses CONSTANT, VERBATIM, or FIXED-1
// ---------------------------------------------------------------------------

fn encode_subframe(w: &mut BitWriter, samples: &[i16], bps: u16) {
    if samples.is_empty() {
        return;
    }

    // Check if all samples are the same → CONSTANT
    let first = samples[0];
    if samples.iter().all(|&s| s == first) {
        w.write_bits(0, 1);          // zero padding
        w.write_bits(0b000000, 6);   // CONSTANT
        w.write_bits(0, 1);          // no wasted bits
        let mask = (1u64 << bps) - 1;
        w.write_bits((first as i32 as u64) & mask, bps as u8);
        return;
    }

    // For very short blocks (<=1 sample), use VERBATIM
    if samples.len() <= 1 {
        w.write_bits(0, 1);
        w.write_bits(0b000001, 6);   // VERBATIM
        w.write_bits(0, 1);
        let mask = (1u64 << bps) - 1;
        for &s in samples {
            w.write_bits((s as i32 as u64) & mask, bps as u8);
        }
        return;
    }

    // FIXED order 1 — good compression for most audio signals
    encode_subframe_fixed1(w, samples, bps);
}

fn encode_subframe_fixed1(w: &mut BitWriter, samples: &[i16], bps: u16) {
    // Subframe header
    w.write_bits(0, 1);          // zero padding
    w.write_bits(0b001001, 6);   // FIXED, order 1
    w.write_bits(0, 1);          // no wasted bits

    // Warm-up sample (1 for order 1) — signed two's complement in `bps` bits
    let mask = (1u64 << bps) - 1;
    w.write_bits((samples[0] as i32 as u64) & mask, bps as u8);

    // Residuals: r[i] = s[i] − s[i−1]
    let residuals: Vec<i32> = (1..samples.len())
        .map(|i| samples[i] as i32 - samples[i - 1] as i32)
        .collect();

    encode_residuals(w, &residuals);
}

// ---------------------------------------------------------------------------
// Rice-coded residual encoding
// ---------------------------------------------------------------------------

fn encode_residuals(w: &mut BitWriter, residuals: &[i32]) {
    // Coding method 00 = partitioned Rice with 4-bit parameter
    w.write_bits(0b00, 2);
    // Partition order 0 → single partition covering all residuals
    w.write_bits(0, 4);

    let k = choose_rice_param(residuals);

    // Safety check — if any quotient would exceed 128, use escape code
    let max_q: u32 = residuals
        .iter()
        .map(|&r| {
            let f = if r >= 0 { 2 * r as u32 } else { ((-2i64 * r as i64) - 1) as u32 };
            f >> k
        })
        .max()
        .unwrap_or(0);

    if max_q > 128 {
        // Escape: parameter = 0b1111, then 5-bit raw-bps
        w.write_bits(0b1111, 4);
        let raw_bps: u8 = 17; // enough for 16-bit order-1 residuals (−65535..65535)
        w.write_bits(raw_bps as u64, 5);
        let rmask = (1u64 << raw_bps) - 1;
        for &r in residuals {
            w.write_bits((r as u64) & rmask, raw_bps);
        }
    } else {
        w.write_bits(k as u64, 4);
        for &r in residuals {
            let folded = if r >= 0 { 2 * r as u32 } else { ((-2i64 * r as i64) - 1) as u32 };
            let quotient = folded >> k;
            let remainder = folded & ((1u32 << k) - 1);
            w.write_unary(quotient);
            if k > 0 {
                w.write_bits(remainder as u64, k);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc8_known() {
        assert_eq!(crc8(&[]), 0);
        assert_eq!(crc8(&[0xFF]), 0xF3);
    }

    #[test]
    fn crc16_known() {
        assert_eq!(crc16(&[]), 0);
    }

    #[test]
    fn bitwriter_full_byte() {
        let mut w = BitWriter::new();
        w.write_bits(0xFF, 8);
        w.write_bits(0x00, 8);
        assert_eq!(w.bytes(), &[0xFF, 0x00]);
    }

    #[test]
    fn bitwriter_nibbles() {
        let mut w = BitWriter::new();
        w.write_bits(0b1010, 4);
        w.write_bits(0b0101, 4);
        assert_eq!(w.bytes(), &[0b10100101]);
    }

    #[test]
    fn streaminfo_sample_rate() {
        let si = build_streaminfo(4096, 4096, 48000, 2, 16, 0);
        assert_eq!(si.len(), 34);
        let sr = ((si[10] as u32) << 12) | ((si[11] as u32) << 4) | ((si[12] as u32) >> 4);
        assert_eq!(sr, 48000);
    }

    #[test]
    fn frame_starts_with_sync() {
        let samples = vec![0i16; 4096 * 2];
        let frame = encode_frame(&samples, 2, 48000, 16, 0);
        assert_eq!(frame[0], 0xFF);
        assert_eq!(frame[1] & 0xFC, 0xF8);
    }

    #[test]
    fn frame_encodes_nonzero_audio() {
        // simple ramp
        let mut samples = Vec::with_capacity(1024 * 2);
        for i in 0..1024 {
            let v = (i as i16).wrapping_mul(13);
            samples.push(v);
            samples.push(v);
        }
        let frame = encode_frame(&samples, 2, 44100, 16, 0);
        assert!(frame.len() > 10);
        assert_eq!(frame[0], 0xFF);
    }
}
