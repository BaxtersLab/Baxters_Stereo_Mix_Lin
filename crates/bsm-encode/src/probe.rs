use serde::{Deserialize, Serialize};
use tracing::debug;

/// Basic info about an audio encoder available in this build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioEncoderInfo {
    pub name: String,
    pub long_name: Option<String>,
    pub sample_rates: Vec<u32>,
    pub channels: Vec<u16>,
}

impl AudioEncoderInfo {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), long_name: None, sample_rates: vec![48000,44100], channels: vec![1,2] }
    }
}

/// Return the list of natively supported encoders.
pub fn probe_audio_encoders() -> Vec<AudioEncoderInfo> {
    debug!("probe: returning native encoder list (WAV, MP3/LAME, FLAC)");
    vec![
        AudioEncoderInfo { name: "pcm_s16le".into(), long_name: Some("WAV PCM 16-bit".into()), sample_rates: vec![48000,44100,96000], channels: vec![1,2] },
        AudioEncoderInfo { name: "mp3".into(), long_name: Some("MP3 (LAME)".into()), sample_rates: vec![48000,44100], channels: vec![1,2] },
        AudioEncoderInfo { name: "flac".into(), long_name: Some("FLAC (native)".into()), sample_rates: vec![48000,44100,96000], channels: vec![1,2] },
    ]
}

/// Select preferred encoder by name hint; falls back to the first available.
pub fn select_encoder(preferred: Option<&str>) -> Option<AudioEncoderInfo> {
    let list = probe_audio_encoders();
    if let Some(pref) = preferred {
        for e in &list { if e.name == pref { return Some(e.clone()); } }
    }
    list.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_returns_at_least_one() {
        let enc = probe_audio_encoders();
        assert!(!enc.is_empty());
    }

    #[test]
    fn select_encoder_prefers_requested() {
        let enc = select_encoder(Some("mp3")).unwrap();
        assert_eq!(enc.name, "mp3");
    }
}
