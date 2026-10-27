# Testing checklist — desktop receiver (Ubuntu ↔ Windows)

Everything you need to exercise the desktop app on both operating systems,
with logging wired in so any misbehavior is captured for debugging.

## What to run where

| OS | App | Path | Audio result |
|----|-----|------|--------------|
| Ubuntu | GUI console | `~/dev/OpenAY-Mic` → `scripts/devlog.sh --gui` | `openay_mic` virtual source (select as mic in any app) |
| Ubuntu | Headless server | `scripts/devlog.sh` | same, no window |
| Windows | Receiver exe | `WD-SN570 drive:\openay-mic-win\dist\openay-windows\` → `.\openay-log.ps1 --output` | plays on the chosen output device (virtual cable = mic for apps) |

Builds are refreshed with: Ubuntu `cargo build --release -p openay-gui -p
openay-server --features openay-server/pipewire`; Windows
`scripts/build_windows.sh` (run from the worktree on the WD partition).

## Phone setup (both OSes, once)

1. Install the APK: `android/app/build/outputs/apk/debug/app-debug.apk`.
2. Ubuntu: allow incoming UDP 41700 (`sudo ufw allow 41700/udp` if ufw is on).
   Windows: the `netsh advfirewall` rules in README-WINDOWS.txt (elevated prompt).
3. Transport Wi-Fi: app → UDP, host = PC IP (`hostname -I` / `ipconfig`), port 41700, ON AIR.
   Transport USB: Ubuntu `adb reverse tcp:41700 tcp:41700` → app TCP, host 127.0.0.1.
   Windows: `adb.exe forward tcp:41700 tcp:41700` → same app settings.

## Ubuntu session (10 minutes)

1. `source scripts/env.sh`, then `scripts/devlog.sh` (headless) or `scripts/devlog.sh --gui`.
   Confirm the log banner and that `pw-cli ls Node | grep openay_mic` shows the source.
2. Start the phone (Wi-Fi). Watch the app log: `SRV ... received=` climbs ~+500/5 s
   with `lost=0`; the GUI shows the level ring and LINK counters.
3. Speak; verify with a recording:
   `pw-cat --record --target openay_mic --rate 48000 --channels 1 --format s16 /tmp/t.wav`
   then `pw-play /tmp/t.wav`.
4. Select "OpenAY Mic" as the microphone in Discord/OBS/Audacity; confirm speech.
5. Resilience probes: toggle phone ON AIR off/on (underrun + re-prebuffer in the
   log; audio recovers); pause 60 s (depth stays — no decay while idle); let it
   run 10 min (no drift).
6. Ctrl-C the session; note the printed log directory.

## Windows session (10 minutes)

1. In `dist\openay-windows\`: `.\openay-log.ps1 --list-devices` (pick a device name).
2. Plain monitoring: `.\openay-log.ps1 --transport udp --output` → start the phone →
   hear the tone/voice on the default output.
3. As a microphone for apps: install VB-Cable (or similar), then
   `.\openay-log.ps1 --transport udp --output --output-device "CABLE Input"`;
   in the consumer app select "CABLE Output" as its microphone.
4. USB path: `adb.exe forward tcp:41700 tcp:41700` → app TCP → 127.0.0.1:41700.
5. Same resilience probes as Ubuntu; Ctrl-C; the log path is printed.

## If something misbehaves

- Grab the session's `~/OpenAY-logs/<ts>/` (Ubuntu) or
  `%USERPROFILE%\OpenAY-logs\<ts>\` (Windows) folder — app log, env snapshot,
  device list, (Ubuntu, with `--adb`) phone logcat — and attach it.
- Known-good baselines for comparison: Ubuntu latency p50 ≈ 47 ms (software
  path), CPU idle < 1 % / Opus < 3 %; SRV `lost=0 ooo≈0` on a quiet Wi-Fi LAN.

## Branch map (for development/debugging)

- `main` — Linux product (PipeWire), all Phase 6 hardening.
- `feature/windows-client` (worktree `WD-SN570\openay-mic-win`) — Windows exe +
  cpal/WASAPI audio output; merged with main's audit fixes.
- `feature/bluetooth` (worktree `WD-SN570/openay-mic-bt`) — Bluetooth SPP
  transport both sides (desktop: `openay-server --transport bt`; phone: BT
  segment + device picker).
