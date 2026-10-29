# Changelog

All notable changes to OpenAY Mic are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
semantic (major.minor.patch — the wire protocol is frozen at v1, so minors
carry features).

## [0.3.0] - 2026-10-30

### Added
- **LISTEN (mic test) toggle**: play your own received mic audio on a local
  output device. Desktop CLI `--monitor` and a GUI settings toggle; additive
  by design — on Linux the PipeWire virtual microphone keeps running
  alongside the monitor, so listening can be toggled freely without
  interrupting the mic. Best-effort: a monitor device failure only stops the
  monitor. (GUI: `listen` in config.toml; on Windows the toggle gates the
  WASAPI rendering that is otherwise always on.)
- **Control-plane vocabulary** on the CONTROL payload type (additive, fully
  backward compatible): `hello`/`hello-ack` capability handshake and a
  receiver-side liveness `ping` (1 s cadence) answered with `pong`. The
  desktop engine pings the last known sender and answers `hello`; any
  received packet refreshes peer liveness. Specified in
  `shared/protocol.md` ("Control messages"); reference implementations in
  `openay_protocol::control` (Rust) and `openay/control.h` (C++).
- **Peer-stale visibility**: after at least one packet, 5 s of silence
  raises `EngineStatus.peer_stale` (GUI/CLI can surface it; logged on state
  transitions). The stream itself stays up — a paused sender is legitimate.
- **Receiver-side output gain**: `EngineConfig.gain` (0–4, clamped),
  `openay-server --gain`, a GUI OUTPUT GAIN slider, and `gain` in the GUI
  config file — applied after decode, before the jitter buffer, so the
  phone's capture/encode chain stays untouched by volume changes.
- **Capture voice processing (Android)**: opt-in acoustic echo cancellation
  and noise suppression via the platform audio session (Oboe stream session
  id allocation on API 28+; effects are attached Kotlin-side per toggle).
- **Call auto-mute (Android)**: capture is muted while the device is in a
  call/communication audio mode (no extra permission); silence is streamed
  so timing and liveness continue.
- **Wi-Fi lock (Android)**: a high-performance/low-latency multicast Wi-Fi
  lock is held for the duration of UDP capture (WAKE_LOCK permission).
- Windows dist bundling of adb (`scripts/build_windows.sh`) with Apache-2.0
  attribution, so USB forwarding needs no separate SDK install.
- **UDP peer latch** (desktop receiver): after the first accepted datagram,
  only the latched source is ingested or replied to; a different source is
  counted (`EngineStatus.foreign_packets`) and rate-limit-logged until the
  latched peer has been quiet for the 5 s stale window, then it may take
  over (phone roaming to a new address). This keeps other LAN hosts from
  blending audio into the virtual microphone or forging control replies.

### Fixed
- Jitter prebuffer decay no longer treats keepalive control traffic as
  audio flow: a paused sender that stays alive via pings keeps its
  network-proven prebuffer depth instead of decaying toward underruns
  (`Ingest.audio_received` drives the depth controller's flowing signal).
- TCP receive path (phone side) no longer drops bytes of a packet split
  across a poll-timeout boundary: partially read bytes are retained and the
  next `Recv` resumes mid-packet (regression-tested; the phone answers
  receiver pings on its TCP audio socket with a zero timeout).
- A failed session-bye control send is no longer recorded as a stream
  error (the receiver closing first is a normal disconnect), and the
  receiver's bye log is throttled to once per connection.
- Android: restarting capture with different settings now fully releases
  the previous session's Wi-Fi lock, voice effects, and call-mute watcher
  before the new session starts (previously inherited/stale).

## [0.2.0] - 2026-10-28

### Added
- **Windows client** (`feature/windows-client`, merged): the same receiver
  engine with a cpal/WASAPI audio-output backend; Iced GUI console with full
  Linux parity (The Chain, VU ladder, ON AIR, settings slide-over) and a
  Win32 system tray (Show Console / Start / Stop / Quit, live state icon);
  release-mode builds suppress the console window; self-contained
  executables (no VC++ redistributable); `scripts/build_windows.sh`
  cross-builds them with user-space tooling.
- **Session loggers** for both platforms: `scripts/devlog.sh` (Ubuntu: app
  log, environment snapshot, virtual-source state, optional phone logcat)
  and `dist/openay-log.ps1` (Windows: app log + device snapshot).
- Adaptive jitter buffer: `DepthController` (+2 ms per underrun to a 20 ms
  ceiling, −1 ms per 60 s underrun-free toward the user floor, injectable
  clock) with live retargeting — no pipeline restart;
  `EngineStatus.effective_target_ms` surfaced in the GUI CONSOLE card.
- `openay-proxy`: deterministic seeded loss profiles (uniform 2%,
  Gilbert-Elliott bursts, 0–60 ms delay + 1% duplicates) for reproducible
  network-degradation validation.
- Software latency probe (`scripts/latency_probe.sh` +
  `analyze_latency.py`) with an onset-marked sender; measured p50 ≈ 47 ms
  over the full chain (p50 ≈ 39–42 ms with `OPENAY_NODE_LATENCY=480/48000`).
- CPU budget gate: `scripts/cpu_profile.sh --assert` (idle < 1 %CPU,
  Opus-active < 3 %CPU; measured idle 0.20–0.40 %, Opus 0.51–0.71 %).
- Hardware glass-to-glass audit kit: click-track generator/analyzer,
  procedure doc, results template.
- GitHub Actions CI: Linux (workspace tests, clippy, C++ host ctest) and
  Windows (headless suite + both executables).

### Fixed
- Non-finite jitter-target values are rejected instead of zeroing the
  prebuffer latch; a non-finite depth base no longer disables the decay
  floor; sub-millisecond decay windows no longer spin the decay loop.
- Duplicate packets are no longer played twice (deduplicated by sequence
  number).
- Adaptive depth stops decaying while no audio flows, so a paused sender
  does not lose its network-proven depth.
- TCP connection tasks are tracked and deterministically reaped on Stop;
  the shutdown stats line no longer races live ingestion.
- Android capture pipeline: fixed a use-after-free of the audio source on
  teardown (found by ASan).

## [0.1.0] - 2026-08-27

First prototype: wire protocol defined in C++ + Rust with golden vectors;
UDP/TCP transports and Opus low-delay integration; Oboe low-latency capture
engine on Android; PipeWire virtual source with lock-free jitter buffer;
Compose (Android) and Iced (Linux) studio-console UIs.
