Saddle for Baxter's Stereo Mix (BSM) — integration & management notes

Purpose: capture audio telemetry and forward to UI or central collector.

Current config reference: `saddle/config.example.json` (subscribe: `\\.\.\pipe\bsm-audio-telemetry`; forward WS: `ws://127.0.0.1:9000/bsm`).

Decision points for management:
- Approve WebSocket forward target for UI ingestion or change to HTTP collector.
- Decide on anonymization levels for user-related telemetry.

Security notes:
- Ensure named pipes and audio telemetry do not leak PII; apply masking transforms where needed.

Verification steps:
1. Confirm TCP listener on `127.0.0.1:9000` is reachable from ops machine.
2. Confirm named pipe `\\.\.\pipe\bsm-audio-telemetry` exists when service runs.

Owner: Audio team / Product.

Functions & GUI Points:
- Primary functions: audio mixing, encode, UI telemetry.
- GUI touchpoints: `bsm-ui` via `127.0.0.1:9000` (mix controls, presets), local IPC for audio commands.
- Connected apps: RemoteDexter (telemetry aggregation), BSR (audio pipeline), HRT (audio playback for tuning).

# Saddle — Baxter's Stereo Mix (BSM)

What a saddle is:
- A runtime sidecar that intercepts BSM telemetry (via `bsm-ipc` or logs), applies transformations (masking, sampling, enrichments), and forwards sanitized events to downstream consumers without touching BSM source or binaries.

Key components:
- Subscriber (IPC), transform pipeline, forwarder, config and audit trail.

Recommended workflow:
1. Apply quick fixes via the saddle (e.g., anonymize user fields or sample high-volume events).
2. Confirm via saddle logs and metrics.
3. Implement long‑term code changes on a git branch, then retire or reduce saddle rules.

Files:
- `config.example.json` — sample subscription and forwarding configuration

Operational notes:
- Version the `saddle` config in repo; enable health checks and retention for audit logs.
