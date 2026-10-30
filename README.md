# OpenAY Mic

An Android phone becomes a microphone for your desktop: the phone captures
audio and streams it to a receiver that presents it as a native audio
device. On Linux that is a PipeWire source — a real microphone for every
application; on Windows the same receiver renders through the native audio
stack (WASAPI). End-to-end software-path latency measures p50 ≈ 47 ms with
the default configuration (measured; see the Status section).

## Components

| Path               | What it is                                                                    |
|--------------------|-------------------------------------------------------------------------------|
| `shared/`          | Canonical wire-protocol spec (`protocol.md`) + golden test vectors             |
| `android/`         | Android client — Kotlin/Compose UI, C++ NDK capture engine (Oboe), transports |
| `android/native/`  | Portable C++ core (protocol + transports); host-buildable for fast testing    |
| `desktop/`         | Rust server — protocol/transport crates, audio backends, Iced GUI              |
| `scripts/`         | Environment template (`env.sh.example`), build/test orchestration, session loggers |

## Platform support

| Platform | Receiver | Audio output | Build |
|----------|----------|--------------|-------|
| Linux    | `openay-server` / `openay-gui` | PipeWire virtual source (`openay_mic`) — a system-wide microphone | `cargo build --release` |
| Windows  | `openay-server.exe` / `openay-gui.exe` | WASAPI playback (pair with a virtual-audio-cable driver to expose a microphone) | `scripts/build_windows.sh` |

One source tree serves both: the engine, jitter buffer, and transports are
shared, and only the audio backend differs (see
[`docs/testing-checklist.md`](docs/testing-checklist.md) for a complete
test walkthrough of both platforms, including session-log collection).

## Transports & codecs

- **USB** (TCP over `adb`), **Wi-Fi** (UDP, drop-late-packets policy).
- **Bluetooth** (RFCOMM/SPP, bypassing the BT audio stack) is implemented on
  the `feature/bluetooth` branch: desktop SPP service via BlueZ, phone-side
  client with a bonded-device picker.
- **Raw PCM** (16-bit LE mono 48 kHz) and **Opus**
  (`OPUS_APPLICATION_RESTRICTED_LOWDELAY`, 10 ms frames).
- Wire format: see [`shared/protocol.md`](shared/protocol.md). The protocol
  is frozen (v1) — every latency feature is receiver-side.

## Building & testing

All tooling is user-space; no root required.

Create your local environment once from the committed template — the
machine-specific copy is gitignored and never committed:

```bash
cp scripts/env.sh.example scripts/env.sh    # adjust the marked ADJUST values
source scripts/env.sh                       # Rust, cmake, pkg-config(libopus), JDK, ANDROID_HOME

# Desktop (Rust)
cargo test --workspace                       # unit + golden-vector + codec tests
cargo test -p openay-server                  # + adaptive-depth scenarios (headless;
                                             #   compiled out of the workspace run by
                                             #   feature unification with openay-gui)
cargo run -p openay-loopback -- bench udp 41001 20000   # loopback latency bench

# Native core (C++, host build)
cmake -S android/native -B android/native/build-host
cmake --build android/native/build-host -j
ctest --test-dir android/native/build-host --output-on-failure

# Cross-language interop (C++ <-> Rust over UDP and TCP)
scripts/run_phase2.sh

# Full regression gate: everything above plus adaptive-jitter scenarios through the
# lossy-network proxy, QA-kit self-tests, software latency probe (needs a
# PipeWire daemon) and CPU budget assertions
scripts/run_phase6.sh

# Analysis tools
cargo run -p openay-proxy -- --listen 127.0.0.1:41860 --forward 127.0.0.1:41700 \
  --profile burst            # deterministic loss/burst/jitter UDP forwarder
scripts/latency_probe.sh     # software ingest->present latency (p50/p95)
scripts/cpu_profile.sh       # idle / PCM / Opus %CPU + RSS, --assert budgets
scripts/gen_click_track.py   # acoustic click track for the hardware audit
```

### Windows

From a checkout of `feature/windows-client` (the branch also carries the
GUI's Windows build):

```bash
scripts/build_windows.sh     # -> dist/openay-windows/: openay-gui.exe,
                             #    openay-server.exe, openay-log.ps1, README-WINDOWS.txt, adb/
```

The script cross-compiles with an MSVC target using user-space tooling (no
admin) and produces self-contained executables — no redistributable install
is needed on the target machine.

### Test-session logging

Both platforms wrap the app with a session logger so every run is captured
for debugging:

- Ubuntu: `scripts/devlog.sh [--gui] [--adb]` — app log, environment
  snapshot, virtual-source state, optional phone logcat, under
  `~/OpenAY-logs/<timestamp>/`.
- Windows: `.\openay-log.ps1 <args>` — app log + device snapshot under
  `%USERPROFILE%\OpenAY-logs\<timestamp>\`.

See [`docs/testing-checklist.md`](docs/testing-checklist.md) for the full
procedure and known-good baselines.

## Reproducing the environment

`scripts/env.sh.example` documents every toolchain input; the header of the
file you copy is the authoritative checklist. In brief:

1. **Rust** — install via [rustup](https://rustup.rs).
2. **C/C++ toolchain** — a conda env (or your distro's packages) providing
   `cmake`, `pkg-config`, `libopus` + headers, `openjdk`, `meson`, `ninja`,
   and `glib`. The example file shows the one-line `conda create` command.
3. **Android SDK** — point `ANDROID_HOME` at it; the project's Gradle wrapper
   and NDK usage are pinned in `android/`.
4. **User-space PipeWire** (Linux) — build from source into `$PW_PREFIX` at
   the same version your system daemon runs (`pw-cli --version`); the meson
   commands are in the example file. `pipewire-rs` compiles against these
   headers, and `LD_LIBRARY_PATH` makes our binaries load this matching
   `libpipewire` while talking to the system daemon over the standard socket.
5. **bindgen inputs** — `LIBCLANG_PATH` (any libclang) and
   `BINDGEN_EXTRA_CLANG_ARGS` (GCC headers), with defaults that may need
   adjusting per host; both are marked ADJUST in the example file.

If `scripts/env.sh` is missing, the runner scripts
(`run_phase2.sh`, `run_phase6.sh`, `latency_probe.sh`, `cpu_profile.sh`) exit
with instructions instead of failing later in a build.

## Licenses

- **Project code** — [MIT](LICENSE) (Copyright © 2026 Aryan Yadav).
- **Fonts** — Chakra Petch and IBM Plex Mono are distributed under the
  SIL Open Font License 1.1; the license texts ship alongside the fonts in
  [`shared/fonts/`](shared/fonts/) (`ChakraPetch-OFL.txt`,
  `IBMPlexMono-OFL.txt`) and apply to the copies bundled in the Android app
  (`android/app/src/main/res/font/`).
- **Vendored `iced_tiny_skia`** — a patched fork of the MIT-licensed
  upstream (`desktop/vendor/iced_tiny_skia/`); its
  [MIT license](desktop/vendor/iced_tiny_skia/LICENSE) and patch notes are
  kept in that directory.
- Third-party dependencies (crates.io crates, Oboe, Opus, PipeWire) are
  pulled from upstream at build time and remain under their own licenses.

## Status

- [x] Phase 1 — monorepo scaffold, packet protocol defined in C++ + Rust
- [x] Phase 2 — UDP/TCP transports, Opus low-delay integration, interop tests
  - `scripts/run_phase2.sh`: 14/14 checks pass (builds, ctest, cargo test,
    C++<->Rust interop over UDP+TCP both directions, MTU-boundary payloads,
    loopback benches p99 <= 1.3 ms vs <5 ms target)
- [x] Phase 3 — Oboe low-latency capture engine on Android
  - Lock-free SPSC ring between the RT audio callback and the network thread
    (no malloc/mutex/logging in the callback), PCM + Opus packetization,
    JNI Start/Stop/Config/Stats, foreground service with microphone type
  - Host: ctest 5/5 (incl. ring stress + live pipeline), ASan/UBSan/TSan clean
  - On emulator: full chain device->host over UDP (10.0.2.2) and TCP (adb
    reverse), zero loss; on-device Opus encoding verified; real-audio content
    path proven on host (440 Hz sine -> WAV, RMS matches reference)
- [x] Phase 4 — PipeWire virtual source node + jitter buffer
  - `openay-server`: UDP/TCP receiver → seq tracking → PCM/Opus decode →
    lock-free jitter buffer → native PipeWire source (`openay_mic`,
    Audio/Source/Virtual via null-audio-sink + link-factory)
  - Live against the system daemon: recordable as a real "OpenAY Mic"
    source; 440 Hz tone round-trips with zero packet loss and bit-exact
    amplitude (RMS 9267 per 100 ms bucket, no dropouts)
- [x] Phase 5 — Compose UI (Android) / Iced GUI (desktop)
  - Shared design system (`shared/design.md`, "studio rack at night"):
    warm-graphite palette, Chakra Petch + IBM Plex Mono, signature
    "The Chain" live signal-path strip on both platforms
  - Android: full Compose redesign (ON AIR toggle, transport/codec/frame
    controls, live level ring, network card) — verified on emulator
  - Desktop: `openay-gui` (Iced) with VU ladder, engine settings
    slide-over, autostart; the window close button quits the app cleanly;
    `openay-server` refactored to lib+bin
- [x] Phase 6 — QA, profiling & latency tuning
  - Adaptive jitter buffer: `openay-jitter::DepthController` (+2 ms per
    underrun to a 20 ms ceiling, −1 ms per 60 s underrun-free toward the
    user floor; injectable clock, fake-clock unit tests); live retarget
    via `Arc<AtomicU32>` with no pipeline restart;
    `EngineStatus.effective_target_ms` surfaced in the GUI CONSOLE card
    (amber ↑ marker when raised)
  - `openay-proxy`: deterministic seeded loss profiles (uniform 2%,
    Gilbert-Elliott bursts ~9%, 0–60 ms delay + 1% dups) for reproducible
    network-degradation validation
  - Xrun visibility: rate-limited desktop underrun-episode stderr lines
    (with effective target); Android service logs xrun increases between
    its 2 s stats refreshes
  - Software latency probe (`scripts/latency_probe.sh` +
    `analyze_latency.py`, onset-marked sender): measured on this host,
    p50 ≈ 47–50 ms / p95 ≈ 51–57 ms over the full UDP → decode → jitter →
    PipeWire chain (anchor error bars documented; hardware audit remains
    ground truth)
  - `node.latency` evaluation: `OPENAY_NODE_LATENCY=480/48000` (requested
    on both the engine stream and the null-sink driver) cuts the measured
    p50 to ≈ 39–42 ms / p95 ≈ 47 ms for idle 0.51 %CPU / Opus-active
    0.71 %CPU — ~10 ms glass-to-glass for a few tenths of a percent CPU,
    well inside the budgets
  - CPU budgets asserted: idle 0.20–0.40 %CPU (<1 %), Opus-active
    0.51–0.71 %CPU (<3 %), RSS ≈ 8 MiB
  - Hardware glass-to-glass audit kit: click-track generator/analyzer,
    procedure doc (`docs/latency-audit.md`) and results template
  - `scripts/run_phase6.sh`: 11/11 checks pass (builds, ctest, cargo
    workspace + headless adaptive scenarios, python self-tests, proxy
    delivery-ratio smoke, latency-probe verdict, CPU budget assertions)
- [x] Windows client (`feature/windows-client`)
  - Same receiver engine with a cpal/WASAPI audio-output backend;
    cross-compiled with an MSVC target using user-space tooling into
    self-contained executables
  - Iced GUI console with full Linux parity (The Chain, VU ladder, ON AIR,
    settings slide-over) plus a Win32 system tray with the same
    Show/Start/Stop/Quit menu and live state icon
  - Both `openay-gui.exe` and `openay-server.exe` ship with a session
    logger (`openay-log.ps1`); verified end-to-end on hardware
- [ ] Bluetooth transport (`feature/bluetooth`) — implemented and tested on
  the branch (desktop BlueZ SPP service, phone client, host ctest for the
  fd-bridge); pending merge
