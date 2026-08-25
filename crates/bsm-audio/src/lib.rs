// bsm-audio — capture backends (WASAPI input, WASAPI loopback, PulseAudio/PipeWire
// monitor, mock, null)
pub mod backend;
pub mod loopback;
pub mod monitor;
pub mod mock;
pub mod null;
pub mod pipeline;
pub mod wasapi;

pub use backend::AudioBackend;
pub use mock::MockAudioBackend;
// NullBackend produces silence when WASAPI is unavailable.
pub use null::NullBackend;
pub use wasapi::WasapiBackend;
