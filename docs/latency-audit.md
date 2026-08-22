# OpenAY Mic — Hardware glass-to-glass latency audit

This document is the operator runbook for the acoustic (hardware) latency
audit: measuring how long audio takes from the loudspeaker through the phone
mic, the OpenAY wire path, and out of the `openay_mic` PipeWire source — the
"glass-to-glass" numbers the Phase 6 latency targets are built on.

```text
speaker ──acoustic──▶ phone mic ──(Oboe capture + codec)──▶ network ──▶
openay-server ──(jitter buffer)──▶ PipeWire "openay_mic" ──▶ pw-cat recording
```

Two toolkits cover the audit:

- `scripts/gen_click_track.py` — synthesizes the acoustic click track (48 kHz
  mono 16-bit WAV) plus a `<out>.clicks.tsv` ground-truth file with every
  click's exact onset sample and time.
- `scripts/analyze_clicks.py` — scans the recorded WAV for click onsets and
  prints onset times, inter-onset statistics, and the count of missing clicks.
- `scripts/cpu_profile.sh` — measures the desktop CPU/RSS budget separately
  (see `docs/latency-report.md`).
- `scripts/latency_probe.sh` (built separately) — the *software* probe: it
  measures the automated ingest→present path (network + jitter buffer)
  **without hardware**. Use it first, as the fast feedback loop; the acoustic
  audit below validates the real glass-to-glass path.

---

## 1. Prerequisites

| Item | Check |
|---|---|
| Desktop server built | `desktop/target/release/openay-server` exists (Phase 6 check), or `cargo build --release -p openay-server` |
| `openay_mic` present in PipeWire | `pw-cli ls Node | grep -B1 -A2 openay_mic` shows the `Audio/Source` node (server must have been started once) |
| Click/analyze tools | `python3 scripts/gen_click_track.py --self-test && python3 scripts/analyze_clicks.py --self-test` both pass |
| Playback + recording tools | `pw-play` / `pw-cat` (PipeWire), or `ffmpeg` with libpipewire/pulse support |
| Android phone | OpenAY app installed, Phase 3-5 build, mic permission granted, `adb devices` shows it (USB path) or it is reachable on the LAN (Wi-Fi path) |
| Phone + desktop clock note | Onset analysis is entirely relative (same recording stream), so no cross-clock sync is needed |
| Loudspeaker | Wired to the desktop; volume high enough for a clean mic capture but not distorting the speaker amp |

Physical setup: put the phone ~0.3–1 m from the speaker, mic grill facing the
speaker, no hands/objects in the path. **Measure and record `D` = speaker-to-phone
distance in meters** — it is needed for the acoustic correction (section 4).

---

## 2. Generate and play the click track

Generate a 30 s track (defaults: 1 kHz clicks, 5 ms long, every 2.0 s, first
click at t = 1.0 s, level 0.8 full scale):

```bash
python3 scripts/gen_click_track.py --out /tmp/openay_click_track.wav --duration 30
#→ CLICKTRACK out=/tmp/openay_click_track.wav duration_s=30.000 clicks=15 spacing_s=2.0 first_onset_s=1.000
```

Ground truth (exact onset sample index + time per click):

```bash
cat /tmp/openay_click_track.wav.clicks.tsv
```

The `.clicks.tsv` exists so the analyzer can be told the *expected* first
onset (`--expect-first-s`) instead of inferring it from the detected one.

Play it into the room during each measurement (playback may run in parallel
with the recorder; see the sequence in section 3):

```bash
pw-play /tmp/openay_click_track.wav
# ffmpeg alternative:  ffmpeg -i /tmp/openay_click_track.wav -f pulse - <null> ...
```

## 3. Record the desktop side and run the sequence

Record the `openay_mic` PipeWire source while the track plays:

```bash
# PipeWire-native (preferred)
pw-cat -r --target openay_mic --rate 48000 --channels 1 --format s16 \
    --latency 0 /tmp/openay_capture.wav

# ffmpeg alternative (needs pipewire-pulse to expose the virtual source)
ffmpeg -f pulse -i openay_mic -ac 1 -ar 48000 -c:a pcm_s16le \
    /tmp/openay_capture.wav
```

Notes:

- `--target openay_mic` links the recorder to the virtual source by name. If
  the node is not present yet, pw-cat may wait silently — start `openay-server`
  first and confirm with `pw-cli ls Node`.
- `--latency 0` requests minimal recorder buffering; larger values add a
  *constant* offset to every measurement (fine for stability, bad for the
  absolute number).
- The recorder output is 48 kHz mono 16-bit, matching the generator, so
  `analyze_clicks.py` can compare sample indices directly.

### Exact sequence per measurement run

The recording must cover the whole playback, so **start recording first, then
play, then stop** — this guarantees the first click (t = 1.0 s) is never
clipped and the tail of the track is captured.

1. Start the desktop server for the transport under test (see below) and wait
   ~1 s for it to bind.
2. Start `pw-cat -r ...` in the background (or a second terminal).
3. Wait ~0.5 s (confirm the recorder is writing frames).
4. Play the click track: `pw-play /tmp/openay_click_track.wav`.
5. Wait for playback to finish plus ~1 s of tail.
6. Stop the recorder (Ctrl-C) and the server.
7. Analyze:

```bash
python3 scripts/analyze_clicks.py --wav /tmp/openay_capture.wav \
    --spacing 2.0 --expect-first-s 1.0
#→ CLICKS n=15 expected=15 missing=0 intervals_mean_s=2.0000 ... first_onset_s=1.0103
#→   ONSET idx=0 t_s=1.0103 peak=17646
#→   ...
```

Interpretation:

- `first_onset_s - expected_first` (e.g. 1.0103 − 1.0) ≈ the **glass-to-glass
  latency** of the first click (before the acoustic correction of section 4).
- `intervals_*`: mean/min/max and `stddev_ms` of inter-onset gaps. Mean minus
  the expected spacing flags clock drift; stddev quantifies accumulated
  jitter; `min` far below spacing flags dropped/skipped audio (xruns).
- `missing` / `n < expected`: click dropouts → investigate jitter-buffer
  underruns, network loss, or phone capture stalls; cross-check
  `openay-server` stats (`lost=`/xruns) and the software probe.

### Transport paths

**USB (TCP over adb reverse)** — server and phone configuration:

```bash
adb reverse tcp:41700 tcp:41700          # device tcp:41700 → host tcp:41700
desktop/target/release/openay-server --transport tcp --port 41700
```

Phone app: transport TCP, host `127.0.0.1`, port 41700.

**Wi-Fi (UDP)** — server and phone configuration:

```bash
desktop/target/release/openay-server --transport udp --port 41700 \
    --bind 0.0.0.0
```

Phone app: transport UDP, host `<desktop LAN IP>`, port 41700. Same network /
no firewall: UI tests use `ping 10.0.2.2`-style reachability or
`adb shell ip addr` / `ipconfig` to confirm.

> Port choice: pick a port in the project's ranges and keep it out of other
> scripts' ranges (phase scripts use 416xx, `cpu_profile.sh` 419xx).

### How many runs

- **≥ 10 full runs per transport path** (USB and Wi-Fi), spread over the
  session.
- Record each run's summary line; paste into `docs/latency-report.md`.
- Report the distribution of per-run *mean* latencies as p50/p95/p99, and
  report the pooled run-to-run stddev as the stability figure.

---

## 4. Calibration & error bars

The acoustic setup adds **systematic** offsets that you cannot remove without
instrumenting the speaker:

1. **Speaker driver + amplifier group delay ≈ 1–3 ms** (depends on the active
   speaker; measure a passive speaker's ≈ 0 ms is not achievable — treat
   1–3 ms as the budget for the whole electrical/acoustic chain).
2. **Acoustic propagation**: sound travels ≈ 0.343 m per ms (343 m/s at 20 °C,
   ~331+0.6·T m/s). If the speaker is `D` meters from the phone mic:

   ```text
   acoustic_delay_ms = D / 0.343        # D in meters
   ```

   E.g. D = 0.5 m → ≈ 1.5 ms. **Subtract** this from the measured onset
   difference if you want a propagation-corrected number, or keep it in and
   state it.
3. **Phone-side audio buffering** (capture path: A/D, Oboe buffer, codec
   frame; plus the phone's playback buffer if the phone itself ever plays the
   track in a self-audit) adds an approximately constant offset per phone —
   measure it once (same phone, same app settings) and treat it as a constant.

**Consequence for how to read numbers:**

- **Absolute latency** carries the combined error bar (≈ 1–3 ms + acoustic
  distance + phone buffer). It is comparable across setups *only* when the
  setup (speaker, distance, phone) is unchanged.
- **Run-to-run stddev and inter-onset stddev are the meaningful stability
  metrics** — they are differences of numbers that all carry the same
  constant, so the constants cancel.
- When a door/wall/people change the room, re-measure the distance D and note
  it in the report.

---

## 5. Targets and where results go

| Path | Transport | Target (plan) |
|---|---|---|
| USB | TCP over `adb reverse` | **< 20 ms** glass-to-glass |
| Wi-Fi | UDP | **< 40 ms** glass-to-glass |

Measurement expectation (before acoustic correction): measured latency ≈
jitter-buffer target (5–20 ms) + transport jitter + phone pipeline + acoustic
offset (1–3 ms) + amp delay, so typical passing runs land a few ms above the
pure software-side numbers. If a path fails its target, first compare against
the software probe (`scripts/latency_probe.sh`) to separate wire/software
latency from the acoustic/hardware offset, then investigate the stage whose
delta is unexpected.

**Copy every measured number into `docs/latency-report.md`** — that file is
the fill-in template for the audit session, including the CPU budget table.
