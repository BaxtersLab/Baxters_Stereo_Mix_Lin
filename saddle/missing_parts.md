BSM — Missing / Unfinished Items

- Multiple placeholder modules added to allow workspace compilation (see `change_log.txt`): `bsm-audio` backend, `bsm-encode` probe/encoder/muxer placeholders, and `bsm-ipc` server/telemetry placeholders.
- Telemetry and IPC implementations are intentionally minimal and require full implementations for production.

Recommended action: replace placeholder modules with full audio backend, encoder integration with FFmpeg, and IPC server/telemetry implementations.

---

## Quality Audit Notes (2026-03-29)

**Score: 2/5** — Good saddle config with anonymization, but core modules are placeholders.

### Critical Improvements

1. **P0 — Audio backend**: Replace `bsm-audio` placeholder with actual WASAPI loopback implementation
2. **P0 — Encoder integration**: Integrate real FFmpeg encoder in `bsm-encode` (aac/mp3 presets)
3. **P0 — IPC server**: Replace minimal `bsm-ipc` stub with event-emitting server

### Additional Improvements

4. **Telemetry enrichment**: Add audio quality metrics (sample rate, bitrate, buffer underruns)
5. **Fallback behavior**: Define recovery path if audio backend initialization fails
6. **Device selection**: Document audio input device selection logic and configuration
7. ~~**Pipe naming**: Config uses `\\\\.\\.\\pipe\\bsr-audio-telemetry` — should be `bsm-audio-telemetry` (BSM, not BSR)~~ **FIXED 2026-03-29**

### Cross-Suite Alignment

- BSM's anonymization transform (mask user field) is a good security pattern
- Core implementation completeness is the primary blocker — saddle infrastructure is ready
