use thiserror::Error;

#[derive(Error, Debug)]
pub enum BsmError {
    #[error("config error: {0}")]
    Config(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("audio error: {0}")]
    Audio(String),

    #[error("encoder error: {0}")]
    Encoder(String),

    #[error("muxer error: {0}")]
    Muxer(String),

    #[error("IPC error: {0}")]
    Ipc(String),

    #[error("HRT error: {0}")]
    Hrt(String),

    #[error("already running")]
    AlreadyRunning,

    #[error("operation not supported: {0}")]
    NotSupported(String),

    #[error("unknown error: {0}")]
    Unknown(String),
}

pub type BsmResult<T> = Result<T, BsmError>;

/// Audio-layer specific error (used inside bsm-audio, re-exported).
#[derive(Debug, Error)]
pub enum AudioError {
    #[error("device not found: {0}")]
    DeviceNotFound(String),

    #[error("device open failed: {0}")]
    DeviceOpenFailed(String),

    #[error("buffer overrun")]
    BufferOverrun,

    #[error("device disconnected")]
    DeviceDisconnected,

    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),

    #[error("WASAPI error (HRESULT {0:#010x}): {1}")]
    Wasapi(u32, String),

    #[error("not active")]
    NotActive,
}

pub type AudioResult<T> = Result<T, AudioError>;

/// Encoder-layer specific error (used inside bsm-encode, re-exported).
#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("encoder not found: {0}")]
    NotFound(String),

    #[error("encoder open failed: {0}")]
    OpenFailed(String),

    #[error("encode failed: {0}")]
    EncodeFailed(String),

    #[error("mux failed: {0}")]
    MuxFailed(String),

    #[error("output file error: {0}")]
    OutputFile(String),

    #[error("codec not supported: {0}")]
    UnsupportedCodec(String),
}

pub type EncodeResult<T> = Result<T, EncodeError>;

/// IPC-layer specific error.
#[derive(Debug, Error)]
pub enum IpcError {
    #[error("pipe error: {0}")]
    Pipe(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("command unknown: {0}")]
    UnknownCommand(String),

    #[error("timeout")]
    Timeout,

    #[error("disconnected")]
    Disconnected,
}

pub type IpcResult<T> = Result<T, IpcError>;

