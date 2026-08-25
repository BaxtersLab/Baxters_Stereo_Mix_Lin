use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use crate::error::BsmError;

/// Top-level configuration that other modules will read from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BsmConfig {
    pub config_version: u32,

    pub audio: AudioConfig,
    pub encoder: EncoderConfig,
    pub output: OutputConfig,
    pub hotkeys: HotkeyConfig,
    pub hrt: HrtConfig,
    pub ui: UiConfig,
    pub logging: LoggingConfig,
}

impl Default for BsmConfig {
    fn default() -> Self {
        Self {
            config_version: 1,
            audio: AudioConfig::default(),
            encoder: EncoderConfig::default(),
            output: OutputConfig::default(),
            hotkeys: HotkeyConfig::default(),
            hrt: HrtConfig::default(),
            ui: UiConfig::default(),
            logging: LoggingConfig::default(),
        }
    }
}

// --- AudioConfig ---
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfig {
    pub device_index: u32,
    pub sample_rate: u32,
    pub channels: u16,
    pub bit_depth: u16,
    pub buffer_ms: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            device_index: 0,
            sample_rate: 48000,
            channels: 2,
            bit_depth: 16,
            buffer_ms: 10,
        }
    }
}

// --- EncoderConfig ---
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncoderConfig {
    pub codec: AudioCodec,
    pub bitrate_kbps: u32,
    pub vbr: bool,
    pub preset: EncoderPreset,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AudioCodec {
    Aac,
    Opus,
    Flac,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EncoderPreset {
    Fast,
    Balanced,
    Quality,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            codec: AudioCodec::Aac,
            bitrate_kbps: 192,
            vbr: false,
            preset: EncoderPreset::Balanced,
        }
    }
}

// --- OutputConfig ---
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    pub output_folder: String,
    pub container: ContainerFormat,
    pub auto_remux_to_mp4: bool,
    pub file_name_pattern: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContainerFormat {
    Mp3,
    Flac,
    Wav,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            output_folder: default_output_folder(),
            container: ContainerFormat::Mp3,
            auto_remux_to_mp4: false,
            file_name_pattern: "BSM_{date}_{time}_{n}".to_string(),
        }
    }
}

fn default_output_folder() -> String {
    dirs::audio_dir()
        .map(|p| p.join("BSM_output").to_string_lossy().into_owned())
        .or_else(|| dirs::home_dir().map(|h| h.join("Music").join("BSM_output").to_string_lossy().into_owned()))
        .unwrap_or_else(|| "./BSM_output".to_string())
}

// --- HotkeyConfig ---
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeyConfig {
    pub toggle_recording: String,
    pub toggle_pause: String,
    pub show_ui: String,
    pub emergency_stop: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            toggle_recording: "Ctrl+Shift+R".to_string(),
            toggle_pause: "Ctrl+Shift+P".to_string(),
            show_ui: "Ctrl+Shift+M".to_string(),
            emergency_stop: "Ctrl+Shift+Escape".to_string(),
        }
    }
}

// --- HrtConfig ---
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HrtConfig {
    pub enabled: bool,
    pub pipe_path: String,
    pub throttle_threshold_c: f32,
    pub auto_stop_threshold_c: f32,
}

impl Default for HrtConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            pipe_path: r"\\.\pipe\hrt-command".to_string(),
            throttle_threshold_c: 85.0,
            auto_stop_threshold_c: 95.0,
        }
    }
}

// --- UiConfig ---
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    pub show_on_launch: bool,
    pub headless: bool,
    pub window_width: u32,
    pub window_height: u32,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            show_on_launch: true,
            headless: false,
            window_width: 520,
            window_height: 340,
        }
    }
}

// --- LoggingConfig ---
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub level: String,
    pub log_to_file: bool,
    pub log_file: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            log_to_file: true,
            log_file: default_log_path(),
        }
    }
}

fn default_log_path() -> String {
    dirs::config_dir()
        .map(|p| p.join("BaxtersStereoMix").join("bsm.log").to_string_lossy().into_owned())
        .unwrap_or_else(|| "./bsm.log".to_string())
}

/// Load config from the default location. Creates default if missing.
pub fn load_config() -> Result<BsmConfig, BsmError> {
    let path = config_path();
    if !path.exists() {
        let default_cfg = BsmConfig::default();
        save_config(&default_cfg)?;
        return Ok(default_cfg);
    }
    let contents = std::fs::read_to_string(&path).map_err(|e| BsmError::Io(e))?;
    let config: BsmConfig = toml::from_str(&contents).map_err(|e| BsmError::Config(format!("Failed to parse config: {}", e)))?;
    Ok(config)
}

/// Save config to the default location. Creates directories as needed.
pub fn save_config(config: &BsmConfig) -> Result<(), BsmError> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| BsmError::Io(e))?;
    }
    let contents = toml::to_string_pretty(config).map_err(|e| BsmError::Serialization(format!("Failed to serialize config: {}", e)))?;
    std::fs::write(&path, contents).map_err(|e| BsmError::Io(e))?;
    Ok(())
}

/// Validate config, returning a list of human-readable error strings.
pub fn validate_config(config: &BsmConfig) -> Vec<String> {
    let mut errors = Vec::new();

    if config.audio.sample_rate == 0 {
        errors.push("audio.sample_rate must be > 0".into());
    }
    if config.audio.channels == 0 || config.audio.channels > 8 {
        errors.push("audio.channels must be 1–8".into());
    }
    if config.audio.buffer_ms == 0 {
        errors.push("audio.buffer_ms must be > 0".into());
    }
    if config.encoder.bitrate_kbps == 0 {
        errors.push("encoder.bitrate_kbps must be > 0".into());
    }
    if config.output.output_folder.is_empty() {
        errors.push("output.output_folder must not be empty".into());
    }
    if config.output.file_name_pattern.is_empty() {
        errors.push("output.file_name_pattern must not be empty".into());
    }

    errors
}

fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("BaxtersStereoMix")
        .join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_validates() {
        let cfg = BsmConfig::default();
        let errs = validate_config(&cfg);
        assert!(errs.is_empty(), "default config should validate, got: {:?}", errs);
    }

    #[test]
    fn toml_roundtrip() {
        let cfg = BsmConfig::default();
        let s = toml::to_string_pretty(&cfg).expect("serialize");
        let cfg2: BsmConfig = toml::from_str(&s).expect("deserialize");
        assert_eq!(cfg.config_version, cfg2.config_version);
        assert_eq!(cfg.audio.sample_rate, cfg2.audio.sample_rate);
        assert_eq!(cfg.output.file_name_pattern, cfg2.output.file_name_pattern);
    }
}
