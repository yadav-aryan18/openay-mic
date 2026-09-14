#!/usr/bin/env bash
# smoke_audio_output.sh — end-to-end validation of the `audio-output` engine
# path (cpal: ALSA on this Linux host, WASAPI on Windows):
#
#   1. builds the receiver with `--features audio-output` and the native
#      tone-udp sender (openay_loopback);
#   2. starts `openay-server --transport udp --port 41895 --output`;
#   3. starts `pw-record` on the DEFAULT SINK MONITOR (the "real proof" arm:
#      audio must actually reach the audio graph);
#   4. streams 4 s of 440 Hz PCM from `openay_loopback tone-udp`;
#   5. asserts the SRV stats line shows received≈400 and underruns=0, and —
#      when monitor capture worked — asserts the recorded WAV has a clearly
#      non-silent RMS. If monitor capture is impossible on this host, the
#      reason is printed verbatim and the gate degrades to stats-only.
#
# Env: sources scripts/env.sh (worktree-local, else the main checkout's).
# Note: the conda libasound looks for host plugins inside its own prefix;
# where the PipeWire ALSA glue plugin exists system-wide we point
# ALSA_PLUGIN_DIR at that dir so the ALSA "default" PCM (PipeWire) opens.
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ENV_SH=""
for candidate in "$ROOT/scripts/env.sh" "${OPENAY_ENV_SH:-}"; do
  if [ -n "$candidate" ] && [ -f "$candidate" ]; then ENV_SH="$candidate"; break; fi
done
if [ -z "$ENV_SH" ]; then
  echo "error: no scripts/env.sh found (copy scripts/env.sh.example)" >&2
  exit 1
fi
# shellcheck source=/dev/null
source "$ENV_SH"

PW_PLUGIN="$(ls /usr/lib/*/alsa-lib/libasound_module_pcm_pipewire.so 2>/dev/null | head -1 || true)"
if [ -n "${PW_PLUGIN:-}" ]; then
  export ALSA_PLUGIN_DIR="$(dirname "$PW_PLUGIN")"
  echo "env: ALSA_PLUGIN_DIR=$ALSA_PLUGIN_DIR (PipeWire ALSA glue plugin)"
else
  echo "env: no system PipeWire ALSA plugin found; using ambient ALSA config"
fi

PASS=0; FAIL=0
check() { # <name> <exit_code>
  if [ "$2" -eq 0 ]; then echo "[PASS] $1"; PASS=$((PASS + 1)); else echo "[FAIL] $1"; FAIL=$((FAIL + 1)); fi
}

PORT=41895
SRV_BIN="$ROOT/desktop/target/release/openay-server"
LOOP_BIN="$ROOT/android/native/build-host/openay_loopback"
SRV_LOG=/tmp/openay-smoke-server.log
TONE_LOG=/tmp/openay-smoke-tone.log
WAV=/tmp/openay-smoke-monitor.wav
PWREC_LOG=/tmp/openay-smoke-pwrecord.log

SRV_PID=""
REC_PID=""
TONE_PID=""

cleanup() {
  [ -n "$REC_PID" ] && kill -INT "$REC_PID" 2>/dev/null; sleep 0.2
  [ -n "$TONE_PID" ] && kill "$TONE_PID" 2>/dev/null
  [ -n "$SRV_PID" ] && kill -INT "$SRV_PID" 2>/dev/null
  wait 2>/dev/null
  return 0
}
trap cleanup EXIT

echo "== Build: openay-server --features audio-output (release) =="
(cd desktop && cargo build --release -p openay-server --features audio-output >/tmp/openay-smoke-rust-build.log 2>&1)
check "rust build (release, audio-output)" $?

echo "== Build: native openay_loopback (tone-udp) =="
if [ ! -x "$LOOP_BIN" ]; then
  cmake -S android/native -B android/native/build-host -DCMAKE_BUILD_TYPE=Release >/tmp/openay-smoke-cpp-build.log 2>&1 &&
    cmake --build android/native/build-host -j"$(nproc)" >>/tmp/openay-smoke-cpp-build.log 2>&1
fi
check "cpp build (openay_loopback)" $?
[ -x "$LOOP_BIN" ] || { echo "error: tone-udp sender missing; aborting" >&2; exit 1; }

echo "== Device inventory: openay-server --list-devices =="
"$SRV_BIN" --list-devices 2>/dev/null
check "openay-server --list-devices prints at least one device" "$?"

echo "== Start receiver: openay-server --transport udp --port $PORT --output =="
rm -f "$SRV_LOG" "$WAV" "$PWREC_LOG"
# --target-ms 20 (the clamped maximum): with the ALSA host the first
# post-latch callback can request a full device period (~25 ms); the default
# 10 ms latch margin is too thin for a guaranteed underrun-free cold start
# (the adaptive depth controller then settles it at runtime).
"$SRV_BIN" --transport udp --port "$PORT" --target-ms 20 --output >"$SRV_LOG" 2>&1 &
SRV_PID=$!
srv_up=1
for _ in $(seq 1 40); do
  kill -0 "$SRV_PID" 2>/dev/null || { srv_up=0; break; }
  grep -q "listening" "$SRV_LOG" 2>/dev/null && break
  sleep 0.1
done
if [ "$srv_up" -eq 1 ] && grep -q "listening" "$SRV_LOG" 2>/dev/null; then
  echo "    $(head -3 "$SRV_LOG" | tr '\n' '|')"
  check "receiver is up and listening" 0
else
  echo "    server log:"; sed 's/^/    /' "$SRV_LOG"
  check "receiver is up and listening" 1
fi

# Resolve the default sink for pw-record --target: pactl (PulseAudio
# compat) first; else wpctl's starred sink (WirePlumber marks the default
# with '*'); the value may be a name or a node id — both are valid targets.
resolve_default_sink() {
  if command -v pactl >/dev/null 2>&1; then
    pactl get-default-sink 2>/dev/null && return 0
  fi
  if command -v wpctl >/dev/null 2>&1; then
    local id
    id="$(wpctl status 2>/dev/null | sed -n '/Sinks:/,/Sources:/p' |
      grep -E '\*[[:space:]]*[0-9]+\.' | head -1 | grep -oE '[0-9]+' | head -1)"
    if [ -n "$id" ]; then echo "$id"; return 0; fi
  fi
  return 1
}

echo "== Start monitor capture: pw-record on the default sink monitor =="
REC_OK=1; REC_REASON=""
if ! command -v pw-record >/dev/null 2>&1; then
  REC_OK=0; REC_REASON="pw-record not found on PATH (pipewire-tools not installed)"
elif ! SINK="$(resolve_default_sink)"; then
  REC_OK=0; REC_REASON="could not resolve the default sink (pactl missing/failed and no starred sink in wpctl status) — pw-record --target has nothing to point at"
elif [ -z "$SINK" ]; then
  REC_OK=0; REC_REASON="default sink resolver returned an empty name"
else
  echo "    default sink: $SINK"
  pw-record --target "$SINK" --format s16 --rate 48000 --channels 2 "$WAV" 2>"$PWREC_LOG" &
  REC_PID=$!
  sleep 0.5
  if ! kill -0 "$REC_PID" 2>/dev/null; then
    REC_OK=0; REC_REASON="pw-record exited immediately:"" $(tail -1 "$PWREC_LOG")"
    REC_PID=""
  fi
fi
if [ "$REC_OK" -eq 1 ]; then
  check "monitor capture running (pw-record)" 0
else
  echo "    monitor capture unavailable: $REC_REASON"
  echo "    -> falling back to stats-only assertion (documented per spec)"
  check "monitor capture running (pw-record)" 1
fi

# The stream runs 6 s so the engine's 5 s stats line is a MID-STREAM line:
# underruns=0 is only meaningful while audio still flows. A stream that ends
# while the RT callback keeps pulling always produces exactly one dry pop
# ~20 ms after the last frame (the latch drains to true silence and resets —
# the pw.rs RT contract, mirrored in audio.rs), so the FINAL line is
# asserted for delivered frames only.
echo "== Stream 6 s of 440 Hz PCM: openay_loopback tone-udp =="
"$LOOP_BIN" tone-udp 127.0.0.1 "$PORT" 6 440 pcm >"$TONE_LOG" 2>&1 &
TONE_PID=$!

# Mid-stream: the 5 s stats line (audio still flowing). The stats tick is
# measured from pipeline start while the sender begins after the capture
# setup, so received at the tick reads ~4 s of frames (380..500), not a
# full 5 s. underruns=0 while audio flows is the assertion that matters.
MID_LINE=""
for _ in $(seq 1 60); do
  MID_LINE="$(grep -E '^SRV ' "$SRV_LOG" | head -1)"
  [ -n "$MID_LINE" ] && break
  sleep 0.25
done
echo "== Mid-stream stats (5 s SRV line) =="
echo "    ${MID_LINE:-<no SRV line yet>}"
check "mid-stream SRV stats line present" "$([ -n "$MID_LINE" ] && echo 0 || echo 1)"
MID_RECEIVED="$(sed -n 's/.*received=\([0-9]*\).*/\1/p' <<<"$MID_LINE")"
MID_UNDERRUNS="$(sed -n 's/.*underruns=\([0-9]*\).*/\1/p' <<<"$MID_LINE")"
if [ -n "${MID_RECEIVED:-}" ] && [ "$MID_RECEIVED" -ge 380 ] && [ "$MID_RECEIVED" -le 500 ]; then
  check "mid-stream received ~440 (380..500, got $MID_RECEIVED)" 0
else
  check "mid-stream received ~440 (380..500, got ${MID_RECEIVED:-none})" 1
fi
if [ "${MID_UNDERRUNS:-1}" -eq 0 ]; then
  check "mid-stream underruns = 0" 0
else
  check "mid-stream underruns = 0 (got ${MID_UNDERRUNS:-none})" 1
fi

# Let the sender finish (6 s total), then drain and stop cleanly.
wait "$TONE_PID"
check "tone-udp sender healthy (TONE stats, exit 0)" $?
sleep 0.8   # let the playback pipeline drain the tail

if [ -n "$REC_PID" ]; then
  kill -INT "$REC_PID" 2>/dev/null
  wait "$REC_PID" 2>/dev/null
  REC_PID=""
fi
sleep 0.3
kill -INT "$SRV_PID" 2>/dev/null
wait "$SRV_PID" 2>/dev/null
SRV_PID=""

echo "== Final stats (shutdown SRV line) =="
FINAL_LINE="$(grep -E '^SRV ' "$SRV_LOG" | tail -1)"
echo "    ${FINAL_LINE:-<no SRV line>}"
check "final SRV stats line present" "$([ -n "$FINAL_LINE" ] && echo 0 || echo 1)"
FINAL_RECEIVED="$(sed -n 's/.*received=\([0-9]*\).*/\1/p' <<<"$FINAL_LINE")"
if [ -n "${FINAL_RECEIVED:-}" ] && [ "$FINAL_RECEIVED" -ge 580 ] && [ "$FINAL_RECEIVED" -le 620 ]; then
  check "final received ~600 (600 frames sent, got $FINAL_RECEIVED)" 0
else
  check "final received ~600 (600 frames sent, got ${FINAL_RECEIVED:-none})" 1
fi

echo "== WAV RMS of the captured sink monitor =="
if [ "$REC_OK" -ne 1 ]; then
  echo "    skipped: monitor capture was unavailable ($REC_REASON)"
  check "WAV RMS clearly non-silent (stats-only fallback: monitor capture unavailable)" 0
elif [ ! -s "$WAV" ]; then
  echo "    pw-record produced no/empty WAV:"
  sed 's/^/    /' "$PWREC_LOG" 2>/dev/null
  check "WAV RMS clearly non-silent" 1
else
  python3 - "$WAV" <<'PY'
import struct, sys, wave

path = sys.argv[1]
w = wave.open(path, "rb")
nch, width, rate, nframes = (
    w.getnchannels(), w.getsampwidth(), w.getframerate(), w.getnframes(),
)
raw = w.readframes(nframes)
w.close()
assert width == 2, f"expected s16 samples, got sampwidth={width}"
n = len(raw) // 2
samples = struct.unpack(f"<{n}h", raw)
acc = sum(s * s for s in samples)
rms = (acc / n) ** 0.5 if n else 0.0
peak = max(abs(s) for s in samples) if n else 0
dur = nframes / rate if rate else 0.0
print(f"    capture: {nch}ch {rate}Hz {dur:.2f}s rms={rms:.0f} peak={peak} (s16)")
THRESHOLD = 1000  # sine at 0.4 FS would be ~9200; 1000 ≈ 3% FS is clearly live
sys.exit(0 if rms > THRESHOLD else 1)
PY
  rms_rc=$?
  if [ "$rms_rc" -eq 0 ]; then
    echo "    audio is clearly audible on the default sink monitor"
  else
    echo "    RMS at or below the silence threshold (or malformed capture)"
  fi
  check "WAV RMS clearly non-silent" $rms_rc
fi

echo
echo "=========================================="
echo " AUDIO OUTPUT SMOKE: $PASS passed, $FAIL failed"
echo "=========================================="
[ "$FAIL" -eq 0 ]
