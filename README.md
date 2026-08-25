# Baxter's Stereo Mix Recorder — Linux

A stand-alone, deterministic, telemetry-rich Rust application that captures and
records **system audio** — whatever is currently playing — to **WAV**, **FLAC**
(lossless), or **MP3** (LAME), with real-time level metering, a modular backend
abstraction, and agent-ready IPC.

On Linux this needs no "Stereo Mix" device and no loopback trickery. Every
PulseAudio / PipeWire sink already exposes a `.monitor` source carrying the
post-mix output, and Stereo Mix records the default sink's monitor
(`@DEFAULT_MONITOR@`) through the libpulse "simple" API. That API is served by
both PulseAudio and PipeWire (via `pipewire-pulse`), so one backend covers both
sound servers.

## Relationship to the Windows version

Same workspace, **different capture core.** The two capture backends are
compile-gated and neither is built on the other's platform:

| platform | capture path | source |
|---|---|---|
| Linux | PulseAudio / PipeWire monitor source | `crates/bsm-audio/src/monitor.rs` |
| Windows | WASAPI render-endpoint loopback | `crates/bsm-audio/src/loopback.rs` |

Everything above the backend — encoders, muxers, output naming, IPC, telemetry,
UI — is shared. This repository is the Linux line, kept in its own path rather
than as a fork, because the code that does the actual work is not the same code.

## Crates

- `bsm-core` — config, app state, PCM types, errors, logging
- `bsm-audio` — capture backends: PulseAudio/PipeWire monitor (Linux), WASAPI
  loopback (Windows), cpal device input, mock, null
- `bsm-encode` — encoder + container muxers (WAV / FLAC / MP3) + output paths
- `bsm-ipc` — agent IPC (server, commands, dispatcher, telemetry)
- `bsm-hrt` — health / runtime telemetry client
- `bsm-ui` — egui recorder UI (the `bsm-ui` binary)

## Build

Prerequisites on Debian/Ubuntu:

```sh
sudo apt install build-essential libgtk-3-dev libasound2-dev \
                 libpulse-dev libmp3lame-dev libclang-dev
```

⚠ **The build directory must not contain spaces.** `mp3lame-sys` compiles LAME
through autotools/libtool, which splits the path at the space and fails with
`install: target '…/libmp3lame.la' is not a directory`. The *source* tree may
contain spaces; only the target directory matters:

```sh
export CARGO_TARGET_DIR=~/.cache/bsm-target   # any space-free path
cargo build --workspace
cargo test  --workspace        # unit + integration tests, no hardware needed
```

## Hardware-gated tests

The suite is green without a sound server, so a green run does **not** by itself
prove capture works. The tests that touch real audio are gated behind
`BSM_USE_HW=1` and must be cited whenever capture is claimed to work:

```sh
BSM_USE_HW=1 cargo test -p bsm-ui --test monitor_record     -- --nocapture
BSM_USE_HW=1 cargo test -p bsm-ui --test monitor_throughput -- --nocapture
```

`monitor_record` is the capstone: real monitor capture → `RecordingSession` →
WAV on disk, verified by reading the file back.

## Run

```sh
cargo run -p bsm-ui            # launches the recorder GUI
```

In the GUI: click **Refresh Devices**, pick **System Audio (Monitor)**, choose a
container (WAV/FLAC/MP3) and an output folder, then **Record**.

### Telemetry (optional)

The recorder can forward its telemetry to an external agent over a
newline-delimited JSON socket. It is **opt-in** — set `BSM_TELEMETRY_ADDR` to the
endpoint and the UI will connect to it, retrying on failure:

```sh
BSM_TELEMETRY_ADDR=127.0.0.1:9000 cargo run -p bsm-ui
```

With the variable unset the recorder makes no connection attempt at all. There is
no default endpoint by design: nothing in this repository serves one, and a
fallback address only produces a thread redialling a dead port.

---

# Licence and open-source notices

**Baxter's Stereo Mix Recorder is MIT** — the full text is in [`LICENSE`](LICENSE).

## It statically links LAME, and LAME is LGPL

MP3 encoding comes from **LAME 3.100**, reached through `mp3lame-encoder` →
`mp3lame-sys`. That crate does **not** load a system `libmp3lame.so`: it vendors
LAME's C source and compiles it in (`cargo:rustc-link-lib=static=mp3lame`), so
**every Stereo Mix binary contains LAME**.

| component | version | licence |
|---|---|---|
| Baxter's Stereo Mix Recorder | 0.1.0 | **MIT** |
| `mp3lame-encoder` | 0.2.2 | LGPL-3.0 |
| `mp3lame-sys` | 0.1.11 | LGPL-3.0 |
| LAME, vendored inside `mp3lame-sys` | 3.100 | GNU Library GPL **v2 or, at your option, any later version** |

Static linking is the case the LGPL cares most about, because a user cannot
simply drop in their own build of the library. **The obligation is met because
this project ships its complete source under MIT:** anyone can substitute a
modified LAME and rebuild, which is what LGPL-3 §4(d) asks for.

**That argument depends on the source shipping with the program.** Distributing
a Stereo Mix binary *without* its source would break it, and would then require
either dynamic linking against the system `libmp3lame0` or shipping object files
sufficient for relinking. Ubuntu packages `libmp3lame0`, so the dynamic route is
available if the distribution model ever changes.

## Licence texts travel with the distribution

```
licenses/LGPL-3.0.txt      GNU Lesser General Public License v3
licenses/GPL-3.0.txt       GNU General Public License v3 — LGPL-3 §4(b) requires
                           both, since LGPL-3 is written as an addendum to GPL-3
licenses/LAME-COPYING.txt  LAME's own COPYING (GNU Library GPL v2)
licenses/LAME-LICENSE.txt  LAME's own LICENSE note
```

LAME is unmodified: it is used exactly as vendored by `mp3lame-sys` 0.1.11, and
nothing in this repository patches it.

**Acknowledgement.** LAME's LICENSE asks that projects using it say so and link
to the project. Baxter's Stereo Mix Recorder uses the LAME MP3 encoder —
<https://lame.sourceforge.io/> — © its contributors.

## This is not decoration

The full reasoning, including what to do if the distribution model changes, is
in [`THIRD_PARTY_LICENSES`](THIRD_PARTY_LICENSES). Both that file and this
section are pinned by
[`crates/bsm-encode/tests/third_party_licenses.rs`](crates/bsm-encode/tests/third_party_licenses.rs),
which fails the build if the notices, the licence texts, or the MIT grant go
missing. A licence obligation that lives only in someone's memory is one release
away from being forgotten.

The remaining Rust dependencies are permissively licensed (MIT / Apache-2.0).
