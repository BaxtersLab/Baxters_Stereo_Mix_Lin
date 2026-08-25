// bsm-hrt — health/runtime telemetry client
pub mod client;
pub mod actions;
pub mod bridge;

pub use client::{start_hrt_client, HrtClientHandle, HrtInboundMsg};
