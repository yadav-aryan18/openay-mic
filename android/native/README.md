# OpenAY Mic — native core (android/native)

C++17, POSIX-socket implementation of the OpenAY Mic wire protocol (Phase 1+2)
plus the Phase 3 **capture engine** (Oboe audio callback → lock-free ring →
network thread). It uses only `<sys/socket.h>`-family APIs plus `std::thread`,
so the same sources build on Linux hosts and Android/bionic. The Android-only
pieces (Oboe source, JNI bridge, vendored opus) are wired only when
`CMAKE_SYSTEM_NAME STREQUAL "Android"`.

The canonical wire-format spec is `shared/protocol.md` and the golden vectors
are `shared/test-vectors.json` — this directory must stay byte-compatible with
them.

## Layout

```
android/native/
├── CMakeLists.txt              # host build + NDK (openaymic) build
├── include/openay/
│   ├── protocol.h              # packet struct, encode/decode, big-endian helpers
│   ├── stats.h                 # PacketStats, FormatStats, SeqTracker (header-only)
│   ├── transport.h             # UdpSender/UdpReceiver/TcpClient/TcpServer/TcpConn,
│   │                           #   BytePipe (Bluetooth RFCOMM seam, not wired yet)
│   ├── opus_codec.h            # 48 kHz mono 10 ms Opus encoder/decoder wrappers
│   ├── audio_source.h          # IAudioSource + RT-thread callback contract
│   ├── test_source.h           # host-only real-time 440 Hz sine source
│   ├── ring_buffer.h           # lock-free SPSC ring (drop-whole-block policy)
│   ├── capture_pipeline.h      # callback → ring → network thread + IPacketSink
│   ├── capture_engine.h        # portable facade (TestSource/OboeSource + StatsJson)
│   └── oboe_source.h           # Android-only Oboe input stream (guarded header)
├── src/
│   ├── protocol.cpp
│   ├── udp_transport.cpp       # one datagram = one packet
│   ├── tcp_transport.cpp       # stream framing + 64 KiB bad-magic resync
│   ├── opus_codec.cpp          # host: built when OPENAY_HAVE_OPUS (pkg-config)
│   ├── ring_buffer.cpp
│   ├── test_source.cpp         # sleep_until-paced sine generator
│   ├── capture_pipeline.cpp    # RT push path + network loop + UDP/TCP adapters
│   ├── capture_engine.cpp      # portable facade; #ifdef __ANDROID__ wiring
│   ├── android/oboe_source.cpp # Android-only Oboe input stream
│   └── internal_log.h
├── jni/
│   └── capture_jni.cpp         # Android-only JNI bridge (openaymic)
├── tools/
│   └── openay_loopback.cpp     # send/recv/bench/tone-udp CLI (see below)
├── tests/
│   ├── test_protocol.cpp       # golden vectors from shared/test-vectors.json
│   ├── test_transport.cpp      # UDP/TCP loopback, SeqTracker, malformed, resync
│   ├── test_opus.cpp           # 440 Hz fidelity test (requires libopus)
│   ├── test_ring.cpp           # SPSC stress + drop-whole-block/overrun tests
│   └── test_capture.cpp        # end-to-end pipeline (PCM + Opus) + engine smoke
└── build-host/                 # out-of-source host build (created by cmake)
```

Protocol summary (see `shared/protocol.md`): 6-byte big-endian header
`magic=0xA7 type seq(16) len(16)` + payload; types 0=PCM, 1=Opus, 2=Control;
per-direction seq counter mod 2^16. UDP: one datagram per packet, malformed
datagrams counted and dropped. TCP/RFCOMM: byte stream, 6-byte header then
exactly `payload_len` bytes; on a bad header the receiver scans up to 64 KiB
for the next `0xA7` and resumes, else the connection is a hard failure.

## Capture engine architecture (Phase 3)

```
   Android audio hardware
            │
            ▼   (Oboe audio RT thread — hard RT constraints: no I/O, no malloc,
 OboeSource::onAudioReady    no mutex, no logging; ONLY lock-free ring writes)
            │  Deliver(samples, frames)
            ▼
 CapturePipeline::OnAudio ──► int16 → little-endian bytes ──► SpscRingBuffer
            │                                                    (16 KiB, SPSC,
            │                                                     drop-whole-block)
            │  network thread (poll ≤ 1 ms step)
            ▼  pop frame_ms*48 samples
   encode: PCM passthrough / Opus (48 kHz mono, RESTRICTED_LOWDELAY)
            │
            ▼  Packet{type, seq++ mod 65536, payload} → IPacketSink::Send
   ┌────────┴─────────┐
   ▼                  ▼
 UdpPacketSink      TcpPacketSink
 (UdpSender)        (TcpClient)
   │                  │
   ▼                  ▼
 UDP datagram       TCP stream (adb forward)
```

Thread ownership:

| Thread | Runs |
|--------|------|
| audio RT | `OboeSource::onAudioReady` → `CapturePipeline::OnAudio` (atomic ring push + µs timing + CAS-max level peak) |
| network | `CapturePipeline::NetworkLoop`: poll ring, encode, send; drains ≤ 50 ms on Stop |
| control | engine facade (`Configure`/`Start`/`Stop`/`StatsJson`) and JNI calls from the service thread |

Error policy: nothing is swallowed. Send/encode failures increment counters
(`send_errors`, `encode_errors`) and set a sticky `last_error`; `Healthy()`
turns false. Stop order: stop source → drain ring (≤ 50 ms) → join thread →
close sink.

`StatsJson()` (exact field order, fixed vocabulary):

```json
{"running":true,"transport":"udp","host":"10.0.2.2","port":41700,"codec":"opus",
 "frame_ms":10,"sharing":"exclusive","sample_rate":48000,"sent":1234,"bytes":1186560,
 "ring_overruns":0,"encode_errors":0,"send_errors":0,"xruns":0,"callback_us_p50":0,
 "last_error":"","level_peak":40}
```

`level_peak` (Phase 5 input metering) is the peak input level of the current
poll interval as a percent 0..100, `round(peak/32767*100)`; reading it consumes
the peak (each poll covers exactly its own interval), so the UI's 500 ms
`nativeGetStats()` loop drives the live level animation for free. It is 0 when
not running.

`sharing`/`xruns`/`last_error` come from `OboeSource` on Android
("exclusive"/"shared", `AudioStream::getXRunCount()`, `onErrorAfterClose`); on
host they are "exclusive"/0/"".

### JNI surface (Android only, target `openaymic`)

`System.loadLibrary("openaymic")` → `com.openay.mic.NativeBridge`:

| Kotlin | JNI symbol | Semantics |
|--------|-----------|-----------|
| `nativeStart(transport, host, port, codec, frameMs): Boolean` | `Java_com_openay_mic_NativeBridge_nativeStart` | `"udp"`/`"tcp"`; `"pcm"`/`"opus"`; frameMs 5 or 10 (Opus is 10 ms only per protocol). Replaces a running stream. `false` on any configure/start error (reason in `last_error`). |
| `nativeStop(): Boolean` | `Java_com_openay_mic_NativeBridge_nativeStop` | Clean teardown. |
| `nativeIsRunning(): Boolean` | `Java_com_openay_mic_NativeBridge_nativeIsRunning` | |
| `nativeGetStats(): String` | `Java_com_openay_mic_NativeBridge_nativeGetStats` | UTF-8 JSON from `StatsJson()`. |

The engine is a process-wide singleton guarded by one `std::mutex`; all calls
arrive from the service thread, never from the audio RT thread.

## Host build & test

```sh
source scripts/env.sh                    # conda toolchain: cmake, g++, libopus 1.6.1
cmake -S android/native -B android/native/build-host -DCMAKE_BUILD_TYPE=Release
cmake --build android/native/build-host -j$(nproc)
ctest --test-dir android/native/build-host --output-on-failure
```

The build is warning-clean under `-Wall -Wextra` and the full suite must pass
(protocol, transport, opus, ring, capture). `OPENAY_HAVE_OPUS` is auto-detected
via `pkg_check_modules(opus)`; when libopus is absent the codec sources,
`test_opus`, and the Opus variant of `test_capture` are excluded and the rest
of the core still builds (requesting Opus through the pipeline then fails with
`last_error = "unsupported_codec"`). Note: conda-forge opus installs its
headers into `<includedir>/opus/` while pkg-config only reports
`-I<includedir>`; CMake adds the subdirectory so `#include <opus.h>` resolves
in both layouts.

### loopback tool

```
openay_loopback send-udp <host> <port> <count> [payload_size=480] [interval_us=0]
openay_loopback recv-udp <port> <count> [payload_size=480]
openay_loopback send-tcp <host> <port> <count> [payload_size=480]
openay_loopback recv-tcp <port> <count> [payload_size=480]
openay_loopback bench <udp|tcp> <port> <count> [payload_size=480]
openay_loopback tone-udp <host> <port> <seconds> [freq=440] [codec=pcm]
```

Payloads use the deterministic xorshift32 filler from the spec (seed = seq),
which keeps phone↔desktop content verification language-independent. `bench`
prefixes an 8-byte little-endian `CLOCK_MONOTONIC` nanosecond timestamp and
reports p50/p95/p99/max one-way latency; it exits 0 iff p99 < 5000 µs.

`tone-udp` is the Phase 3 end-to-end validation driver: it streams **real
sine audio** (TestSource → CapturePipeline, 10 ms frames) to host:port and
prints `TONE seconds=<s> packets=<n> overruns=<n> send_errors=<n>`, exiting 0
only when the stream stayed healthy. Use it against the desktop receiver, or
against the interop receiver:

```sh
openay_loopback recv-udp 43111 300 960 &     # expect 300 packets in one process
openay_loopback tone-udp 127.0.0.1 43111 3
# RECV ok=300 lost=0 dup=0 ooo=0 malformed=0 content_errors=450
```

The structural fields are perfect (ok/lost/dup/ooo/malformed); the
`content_errors` are expected — `recv-udp`'s payload check verifies the
deterministic xorshift filler (seed = seq), which real audio cannot match.
The point of the gate is framing: every datagram decodes, sequences are
contiguous from 0, payload sizes are right.

## How Gradle builds `openaymic` (Android)

In `android/app/build.gradle.kts`:

```kotlin
android {
    externalNativeBuild {
        cmake {
            path = file("../native/CMakeLists.txt")   // relative to the app module
        }
    }
    defaultConfig {
        externalNativeBuild {
            cmake {
                arguments += listOf("-DOPENAY_BUILD_TESTS=OFF")
            }
        }
        ndk { abiFilters += listOf("arm64-v8a", "x86_64") }
    }
}
```

The NDK branch of the CMake build:

- `find_package(oboe REQUIRED CONFIG)` — the **oboe** library arrives via
  prefab (Gradle resolves the package into the externalNativeBuild context);
  the imported target is `oboe::oboe`.
- libopus is **vendored**: `FetchContent_Declare(opus URL
  https://downloads.opuscodec.org/releases/opus-1.5.2.tar.gz)` built static
  with `OPUS_BUILD_SHARED_LIBRARY=OFF`, `OPUS_BUILD_PROGRAMS=OFF`,
  `OPUS_BUILD_TESTING=OFF`, `OPUS_INSTALL_PKG_CONFIG_MODULE=OFF` (+
  `OPUS_INSTALL_CMAKE_PACKAGE=OFF`, `OPUS_DISABLE_DOCS=ON`).
- `openaymic` (SHARED, the `System.loadLibrary` name) compiles
  `jni/capture_jni.cpp`, `src/android/oboe_source.cpp`, and
  `src/opus_codec.cpp` — the last against the vendored opus (`OPENAY_HAVE_OPUS=1`,
  include dir `${opus_SOURCE_DIR}/include`, link `opus`). It links
  `openay_core` + `oboe::oboe` + `android` + `log`. The host build never
  enters this branch and keeps using pkg-config libopus.

Notes for later phases:

- **Bluetooth RFCOMM**: `BytePipe` in `include/openay/transport.h` is the
  reserved seam for the Kotlin/JNI RFCOMM bridge (byte-stream semantics like
  TCP). Nothing wires it today; a JNI implementation will `Push`/`Pull` from
  an NDK socket handle and can reuse TcpConn's framing/resync logic.
- **Sockets**: the transports bind `127.0.0.1` (loopback) today; a future
  server-side receiver will need `INADDR_ANY` to accept the phone's Wi-Fi
  traffic — a one-line change in `UdpReceiver::Bind` / `TcpServer::Listen`.
- **Opus 5 ms frames**: `shared/protocol.md` defines Opus as one packet per
  10 ms frame, so the pipeline rejects `codec=opus, frame_ms=5`
  (`last_error = "invalid_config"`). PCM supports both 5 ms and 10 ms.
