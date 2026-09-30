# Roadmap: declared stubs

Stubs are declared here and marked `STUB:` in the code, so none of them passes
for a working feature.

## The agent IPC dispatcher (`crates/bsm-ipc`)

The app does not start the agent IPC yet. `bsm-ui` never calls `run_server` or
`run_tcp_server`, so no `bsm_ipc` code is linked into the installed binary. The
dispatcher behind it is a skeleton, and these parts are not real yet. They are
deferred because nothing can reach them until an entry point starts the server,
and because where an agent's recordings are saved is not decided.

- [ ] STUB: `Dispatcher` recording (`spawn_background_tasks`) — write the packets
  through `bsm_encode::output::RecordingSession`, which checks free space and
  finalises the file, then report `disk_stats` from the file it wrote — the
  recording is captured and metered, then discarded. `StartRecording` also
  answers `ok` before the device opens, so a device that fails to open is not
  reported.
- [ ] STUB: device selection (`StartRecording.device_index`, `SetDevice`) — open
  the device asked for, and name it in `audio_started` from the backend — the
  pipeline always opens index 0, and `audio_started` names the index that was
  asked for (`device-N`, or `default-device`), not the device it opened.
- [ ] STUB: `UpdateConfig` — apply the patch — acknowledged and ignored.
- [ ] STUB: `PauseRecording` / `ResumeRecording` — pause and resume capture —
  they change only the state `GetStatus` reports; capture and metering continue.
