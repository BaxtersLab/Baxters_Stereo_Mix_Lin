use serde::{Deserialize, Serialize};
use bsm_core::{IpcError, IpcResult};

// ---------------------------------------------------------------------------
// Request envelope
// ---------------------------------------------------------------------------

/// Raw JSON envelope received from the client.
#[derive(Debug, Deserialize)]
pub struct CommandEnvelope {
    pub cmd:     String,
    pub id:      String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

impl CommandEnvelope {
    /// Parse a newline-terminated JSON string into a CommandEnvelope.
    pub fn parse(raw: &str) -> IpcResult<Self> {
        serde_json::from_str(raw.trim())
            .map_err(|e| IpcError::Serialization(format!("invalid JSON: {}", e)))
    }
}

// ---------------------------------------------------------------------------
// Typed command enum
// ---------------------------------------------------------------------------

/// Fully-typed IPC command, derived from the CommandEnvelope.
#[derive(Debug, Clone, PartialEq)]
pub enum IpcCommand {
    StartRecording { device_index: Option<u32> },
    StopRecording,
    PauseRecording,
    ResumeRecording,
    GetStatus,
        SubscribeTelemetry,
    SetDevice  { device_index: u32 },
    UpdateConfig { patch: serde_json::Value },
    Shutdown,
    /// Seed-BSM-G3-02-11: liveness probe; returns status + uptime + device info.
    Health,
    /// Seed-BSM-G3-02-11: returns current buffer/encoding audio quality stats.
    Stats,
}

impl IpcCommand {
    /// Interpret a CommandEnvelope into a typed IpcCommand.
    pub fn from_envelope(env: &CommandEnvelope) -> IpcResult<Self> {
        match env.cmd.as_str() {
            "start_recording" => {
                let device_index = env.payload.get("device_index")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32);
                Ok(IpcCommand::StartRecording { device_index })
            }
            "stop_recording"   => Ok(IpcCommand::StopRecording),
            "pause_recording"  => Ok(IpcCommand::PauseRecording),
            "resume_recording" => Ok(IpcCommand::ResumeRecording),
            "get_status"       => Ok(IpcCommand::GetStatus),
            "subscribe_telemetry" => Ok(IpcCommand::SubscribeTelemetry),
            "set_device" => {
                let device_index = env.payload.get("device_index")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32)
                    .ok_or_else(|| IpcError::Serialization("set_device: missing device_index".into()))?;
                Ok(IpcCommand::SetDevice { device_index })
            }
            "update_config" => {
                let patch = env.payload.clone();
                Ok(IpcCommand::UpdateConfig { patch })
            }
            "shutdown" => Ok(IpcCommand::Shutdown),
            // Seed-BSM-G3-02-11
            "health" => Ok(IpcCommand::Health),
            "stats"  => Ok(IpcCommand::Stats),
            other => Err(IpcError::UnknownCommand(other.to_owned())),
        }
    }
}

// ---------------------------------------------------------------------------
// Response envelope
// ---------------------------------------------------------------------------

/// Typed response, serialized back to JSON.
#[derive(Debug, Serialize)]
pub struct ResponseEnvelope {
    pub id:    String,
    pub ok:    bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub data:  Option<serde_json::Value>,
}

impl ResponseEnvelope {
    /// Build a success response with no data.
    pub fn ok(id: impl Into<String>) -> Self {
        Self { id: id.into(), ok: true, error: None, data: None }
    }

    /// Build a success response with a data payload.
    pub fn ok_data(id: impl Into<String>, data: serde_json::Value) -> Self {
        Self { id: id.into(), ok: true, error: None, data: Some(data) }
    }

    /// Build an error response.
    pub fn err(id: impl Into<String>, msg: impl Into<String>) -> Self {
        Self { id: id.into(), ok: false, error: Some(msg.into()), data: None }
    }

    /// Serialize to a compact JSON string (no trailing newline).
    pub fn to_json(&self) -> IpcResult<String> {
        serde_json::to_string(self)
            .map_err(|e| IpcError::Serialization(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Round-trip helpers
// ---------------------------------------------------------------------------

/// Parse a raw command line into (typed command, correlation id).
pub fn parse_command(raw: &str) -> IpcResult<(IpcCommand, String)> {
    let env = CommandEnvelope::parse(raw)?;
    let id  = env.id.clone();
    let cmd = IpcCommand::from_envelope(&env)?;
    Ok((cmd, id))
}

/// Serialise a response envelope to a raw JSON string.
pub fn serialize_response(resp: &ResponseEnvelope) -> IpcResult<String> {
    resp.to_json()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_start_recording_no_device() {
        let json = r#"{"cmd":"start_recording","id":"abc","payload":{}}"#;
        let (cmd, id) = parse_command(json).unwrap();
        assert_eq!(id, "abc");
        assert!(matches!(cmd, IpcCommand::StartRecording { device_index: None }));
    }

    #[test]
    fn parse_start_recording_with_device() {
        let json = r#"{"cmd":"start_recording","id":"xyz","payload":{"device_index":2}}"#;
        let (cmd, _) = parse_command(json).unwrap();
        assert!(matches!(cmd, IpcCommand::StartRecording { device_index: Some(2) }));
    }

    #[test]
    fn parse_set_device_missing_index_is_error() {
        let json = r#"{"cmd":"set_device","id":"1","payload":{}}"#;
        assert!(parse_command(json).is_err());
    }

    #[test]
    fn parse_unknown_command_is_error() {
        let json = r#"{"cmd":"fly_a_kite","id":"1","payload":{}}"#;
        assert!(parse_command(json).is_err());
    }

    #[test]
    fn response_ok_serializes_without_error_field() {
        let r = ResponseEnvelope::ok("my-id");
        let s = r.to_json().unwrap();
        assert!(s.contains("\"ok\":true"));
        assert!(!s.contains("\"error\""));
        assert!(!s.contains("\"data\""));
    }

    #[test]
    fn response_err_serializes_error_field() {
        let r = ResponseEnvelope::err("my-id", "something went wrong");
        let s = r.to_json().unwrap();
        assert!(s.contains("\"ok\":false"));
        assert!(s.contains("\"error\""));
    }

    #[test]
    fn response_ok_data_serializes_data_field() {
        let data = serde_json::json!({"state": "idle"});
        let r    = ResponseEnvelope::ok_data("my-id", data);
        let s    = r.to_json().unwrap();
        assert!(s.contains("\"data\""));
        assert!(s.contains("\"state\""));
    }

    #[test]
    fn parse_shutdown_command() {
        let json = r#"{"cmd":"shutdown","id":"s1","payload":{}}"#;
        let (cmd, _) = parse_command(json).unwrap();
        assert!(matches!(cmd, IpcCommand::Shutdown));
    }

    #[test]
    fn parse_get_status() {
        let json = r#"{"cmd":"get_status","id":"g1","payload":{}}"#;
        let (cmd, _) = parse_command(json).unwrap();
        assert!(matches!(cmd, IpcCommand::GetStatus));
    }
}
