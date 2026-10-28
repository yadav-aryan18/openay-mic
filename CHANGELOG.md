# Changelog

All notable changes to OpenAY Mic are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
semantic (major.minor.patch — the wire protocol is frozen at v1, so minors
carry features).

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
