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
| `scripts/`        | Environment setup (`env.sh`) and test orchestration                         |

## Transports & codecs

- **USB** (TCP over `adb forward`), **Wi-Fi** (UDP, drop-late-packets policy),
  **Bluetooth** (RFCOMM/SPP, bypasses the BT audio stack).
- **Raw PCM** (16-bit LE mono 48 kHz) and **Opus**
  (`OPUS_APPLICATION_RESTRICTED_LOWDELAY`, 10 ms frames).
- Wire format: see [`shared/protocol.md`](shared/protocol.md).

## Building & testing (Phase 4 state)

All tooling is user-space; no root required.

```bash
source scripts/env.sh        # Rust, cmake, pkg-config(libopus), JDK, ANDROID_HOME

# Desktop (Rust)
cargo test --workspace                       # unit + golden-vector + codec tests
cargo run -p openay-loopback -- bench udp 41001 20000   # loopback latency bench

# Native core (C++, host build)
cmake -S android/native -B android/native/build-host
cmake --build android/native/build-host -j
ctest --test-dir android/native/build-host --output-on-failure

# Cross-language interop (C++ <-> Rust over UDP and TCP)
scripts/run_phase2.sh
```

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
- [ ] Phase 5 — Compose UI (Android) / Slint tray app (desktop)
- [ ] Phase 6 — latency audit, xrun handling, CPU profiling
