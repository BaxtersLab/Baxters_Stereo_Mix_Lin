//! bsm-encode
//!
//! Encoding and muxing helpers for Baxter's Stereo Mix (BSM).

pub mod flac_enc;
pub mod probe;
pub mod encoder;
pub mod muxer;
pub mod output;
pub mod ipc;

pub use probe::{AudioEncoderInfo, probe_audio_encoders, select_encoder};
pub use encoder::{start_encoder, AudioPacket, EncoderHandle, EncoderStats};
