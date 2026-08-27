# OpenAY Mic

Ultra-low-latency virtual microphone: an Android phone captures audio and
streams it to a Linux desktop, where it appears as a native PipeWire source
(a real microphone for every application). Designed to beat proprietary
solutions like WO Mic on latency, security, and UX.

## Components

| Path              | What it is                                                                 |
|-------------------|----------------------------------------------------------------------------|
| `shared/`         | Canonical wire-protocol spec (`protocol.md`) + golden test vectors          |
| `android/`        | Android client — Kotlin/Compose UI, C++ NDK capture engine (Oboe), transports |
| `android/native/` | Portable C++ core (protocol + transports); host-buildable for fast testing  |
| `desktop/`        | Rust server — protocol/transport crates, PipeWire virtual-mic node, native GUI |
| `scripts/`        | Environment template (`env.sh.example`) and test orchestration               |

## Transports & codecs

- **USB** (TCP over `adb forward`), **Wi-Fi** (UDP, drop-late-packets policy),
  **Bluetooth** (RFCOMM/SPP, bypasses the BT audio stack).
- **Raw PCM** (16-bit LE mono 48 kHz) and **Opus**
  (`OPUS_APPLICATION_RESTRICTED_LOWDELAY`, 10 ms frames).
- Wire format: see [`shared/protocol.md`](shared/protocol.md).

## Building & testing (Phase 6 state)

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

# Phase 6 gate: everything above plus adaptive-jitter scenarios through the
# lossy-network proxy, QA-kit self-tests, software latency probe (needs a
# PipeWire daemon) and CPU budget assertions
scripts/run_phase6.sh

# Phase 6 tools
cargo run -p openay-proxy -- --listen 127.0.0.1:41860 --forward 127.0.0.1:41700 \
  --profile burst            # deterministic loss/burst/jitter UDP forwarder
scripts/latency_probe.sh     # software ingest->present latency (p50/p95)
scripts/cpu_profile.sh       # idle / PCM / Opus %CPU + RSS, --assert budgets
scripts/gen_click_track.py   # acoustic click track for the hardware audit
```

## Reproducing the environment

`scripts/env.sh.example` documents every toolchain input; the header of the
file you copy is the authoritative checklist. In brief:

1. **Rust** — install via [rustup](https://rustup.rs).
2. **C/C++ toolchain** — a conda env (or your distro's packages) providing
   `cmake`, `pkg-config`, `libopus` + headers, `openjdk`, `meson`, `ninja`,
   and `glib`. The example file shows the one-line `conda create` command.
3. **Android SDK** — point `ANDROID_HOME` at it; the project's Gradle wrapper
   and NDK usage are pinned in `android/`.
4. **User-space PipeWire** — build from source into `$PW_PREFIX` at the same
   version your system daemon runs (`pw-cli --version`); the meson commands
   are in the example file. `pipewire-rs` compiles against these headers, and
   `LD_LIBRARY_PATH` makes our binaries load this matching `libpipewire`
   while talking to the system daemon over the standard socket.
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
- [x] Phase 2 — UDP/TCP/(BT) transports, Opus low-delay integration, interop tests
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
  - Desktop: `openay-gui` (Iced, best-effort tray) with VU ladder, engine
    settings slide-over, autostart; the window close button quits the app
    cleanly (no hide-to-tray); `openay-server` refactored to lib+bin
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
