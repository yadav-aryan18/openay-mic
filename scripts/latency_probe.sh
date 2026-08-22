#!/usr/bin/env bash
# Software-path latency probe (Phase 6.4): ingest->present latency of the
# full desktop receive chain (UDP -> decode -> jitter buffer -> PipeWire
# virtual source) measured externally, no phone needed.
#
# Per measurement run:
#   1. start a pw-cat recorder on the `openay_mic` source,
#   2. note the wall clock as the recording anchor,
#   3. stream an onset-marked sine with the native sender
#      (`tone-udp <host> <port> <secs> 440 pcm --onset-after N`: N silent
#      10 ms frames, then the tone; the TONE line reports packet totals),
#   4. stop the recorder and let scripts/analyze_latency.py locate the
#      onset and compute
#          latency_ms = onset_sample/sr*1000 - anchor_ms - N*frame_ms.
#
# ANCHOR ERROR BARS (read before quoting numbers): the analyzer's anchor_ms
# is the wall-clock offset between recording sample 0 and the sender's
# process launch — the true *stream* start happens one pipeline-init later
# (fork/exec/dlopen/thread spawn, typically ~5-20 ms on an idle Linux box,
# variable per run). The probe therefore reports min/p50/p95/max over many
# runs rather than single-shot values, keeps a discarded warmup run, and
# treats the hardware glass-to-glass audit (docs/latency-audit.md) as ground
# truth. Expectation per plan: ~prebuffer + one quantum + overhead
# (~30-40 ms); findings feed the node.latency evaluation.
#
# Each run is span-checked by the analyzer (audible tone length vs
# (P-N)*frame_ms); mismatching runs (packet loss, recorder stall) are
# excluded from the statistics and counted.
#
# Usage: scripts/latency_probe.sh [--runs N] [--warmup N] [--port P]
#        [--onset-frames N] [--duration S] [--target-ms MS] [--min-ok N]
#        [--keep] [--skip-build]
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
source "$ROOT/scripts/env.sh"

RUNS=12
WARMUP=1
PORT=41890
ONSET_FRAMES=100     # 1 s of silence before the tone
DURATION=3.0         # sender stream length, seconds
TARGET_MS=10
MIN_OK=""            # default: >=60% of RUNS, at least 3
KEEP=0
SKIP_BUILD=0

while [ $# -gt 0 ]; do
  case "$1" in
    --runs) RUNS="$2"; shift 2 ;;
    --warmup) WARMUP="$2"; shift 2 ;;
    --port) PORT="$2"; shift 2 ;;
    --onset-frames) ONSET_FRAMES="$2"; shift 2 ;;
    --duration) DURATION="$2"; shift 2 ;;
    --target-ms) TARGET_MS="$2"; shift 2 ;;
    --min-ok) MIN_OK="$2"; shift 2 ;;
    --keep) KEEP=1; shift ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[ -z "$MIN_OK" ] && MIN_OK=$(( RUNS * 6 / 10 > 3 ? RUNS * 6 / 10 : 3 ))

CPP_BIN="$ROOT/android/native/build-host/openay_loopback"
SERVER_BIN="$ROOT/desktop/target/release/openay-server"
ANALYZE="$ROOT/scripts/analyze_latency.py"
LOG_DIR="${TMPDIR:-/tmp}/openay-latency-probe"
mkdir -p "$LOG_DIR"

if [ "$SKIP_BUILD" -eq 0 ]; then
  if [ ! -x "$CPP_BIN" ]; then
    echo "== Building native host tools =="
    cmake -S android/native -B android/native/build-host -DCMAKE_BUILD_TYPE=Release >/dev/null &&
      cmake --build android/native/build-host -j"$(nproc)" >/dev/null || {
        echo "FATAL: native sender build failed"; exit 1; }
  fi
  if [ ! -x "$SERVER_BIN" ]; then
    echo "== Building openay-server (release, pipewire) =="
    (cd desktop && cargo build --release -p openay-server --features pipewire >/dev/null) || {
      echo "FATAL: openay-server build failed"; exit 1; }
  fi
fi
[ -x "$CPP_BIN" ] || { echo "FATAL: sender missing: $CPP_BIN (drop --skip-build)"; exit 1; }
[ -x "$SERVER_BIN" ] || { echo "FATAL: server missing: $SERVER_BIN (drop --skip-build)"; exit 1; }
command -v pw-cat >/dev/null || { echo "FATAL: pw-cat not found (PipeWire user-space tools)"; exit 1; }
pw-cli info 0 >/dev/null 2>&1 || { echo "FATAL: no PipeWire daemon reachable (pw-cli info 0 failed)"; exit 1; }

SRV_LOG="$LOG_DIR/server.log"
: > "$SRV_LOG"
"$SERVER_BIN" --transport udp --port "$PORT" --bind 127.0.0.1 \
  --codec pcm --target-ms "$TARGET_MS" >"$SRV_LOG" 2>&1 &
SERVER_PID=$!
cleanup() { kill "$SERVER_PID" 2>/dev/null; wait "$SERVER_PID" 2>/dev/null; }
trap cleanup EXIT

# Wait for the UDP socket, then for the virtual source node to appear.
sleep 1
if ! kill -0 "$SERVER_PID" 2>/dev/null; then
  echo "FATAL: server exited early:"; tail -5 "$SRV_LOG"; exit 1
fi
for _ in $(seq 1 50); do
  pw-cli ls Node 2>/dev/null | grep -q '"openay_mic"' && break
  sleep 0.2
done
pw-cli ls Node 2>/dev/null | grep -q '"openay_mic"' || {
  echo "FATAL: openay_mic node did not appear"; tail -5 "$SRV_LOG"; exit 1; }
echo "server up on 127.0.0.1:$PORT, openay_mic present"

OK_COUNT=0; EXCLUDED=0; FAILED=0
LATENCIES=""
TOTAL=$(( WARMUP + RUNS ))

for i in $(seq 1 "$TOTAL"); do
  WAV="$LOG_DIR/run_$i.wav"
  rm -f "$WAV"

  pw-cat --record --target openay_mic --rate 48000 --channels 1 \
    --format s16 --latency 0 "$WAV" >/dev/null 2>&1 &
  REC_PID=$!
  # Anchor t0: the moment the WAV appears == recording sample 0 (best
  # effort; residual error = poll granularity + pw-cat open->capture gap,
  # part of the documented error bar). Only a tiny settle follows, so the
  # capture is live before the sender fires — it must NOT be included in
  # the anchor.
  for _ in $(seq 1 100); do [ -s "$WAV" ] && break; sleep 0.02; done
  T_REC=$(date +%s.%N)
  sleep 0.1   # capture settle (NOT part of the anchor)

  T_SEND=$(date +%s.%N)
  "$CPP_BIN" tone-udp 127.0.0.1 "$PORT" "$DURATION" 440 pcm \
    --onset-after "$ONSET_FRAMES" >"$LOG_DIR/send_$i.log" 2>&1
  SEND_RC=$?
  sleep 0.6   # drain margin: jitter buffer empties into the recorder
  kill -INT "$REC_PID" 2>/dev/null
  wait "$REC_PID" 2>/dev/null

  if [ "$SEND_RC" -ne 0 ]; then
    echo "run $i: sender failed (rc=$SEND_RC)"
    FAILED=$((FAILED + 1)); continue
  fi
  PACKETS=$(sed -n 's/.*packets=\([0-9]*\).*/\1/p' "$LOG_DIR/send_$i.log")
  ANCHOR_MS=$(awk -v r="$T_REC" -v s="$T_SEND" 'BEGIN { printf "%.1f", (s - r) * 1000 }')

  LINE=$(python3 "$ANALYZE" --wav "$WAV" --onset-frame "$ONSET_FRAMES" \
    --packets "$PACKETS" --anchor-ms "$ANCHOR_MS")
  CODE=$?
  if [ $i -le "$WARMUP" ]; then
    echo "cal $i: $LINE"
  else
    case "$CODE" in
      0)
        OK_COUNT=$((OK_COUNT + 1))
        L=$(printf '%s' "$LINE" | sed -n 's/.*latency_ms=\([0-9.naN-]*\).*/\1/p')
        LATENCIES="$LATENCIES $L"
        echo "run $i: $LINE"
        ;;
      1)
        EXCLUDED=$((EXCLUDED + 1))
        echo "run $i (span-excluded): $LINE"
        ;;
      *)
        FAILED=$((FAILED + 1))
        echo "run $i (no onset): $LINE"
        ;;
    esac
  fi
  [ "$KEEP" -eq 0 ] && rm -f "$WAV"
done

STATS=$(python3 - "$LATENCIES" "$MIN_OK" "$RUNS" "$EXCLUDED" "$FAILED" <<'PY'
import sys
lat_arg, min_ok, runs, excluded, failed = (
    sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5])
)
vals = sorted(float(v) for v in lat_arg.split())
if not vals:
    print(f"LATPROBE runs={runs} ok=0 excluded={excluded} failed={failed} "
          f"p50_ms=nan p95_ms=nan min_ms=nan max_ms=nan verdict=FAIL")
    raise SystemExit(1)
def pct(p):
    i = max(0, min(len(vals) - 1, round(p / 100 * len(vals)) - 1))
    return vals[i]
verdict = "PASS" if len(vals) >= min_ok else "FAIL"
print(
    f"LATPROBE runs={runs} ok={len(vals)} excluded={excluded} failed={failed} "
    f"p50_ms={pct(50):.1f} p95_ms={pct(95):.1f} "
    f"min_ms={vals[0]:.1f} max_ms={vals[-1]:.1f} verdict={verdict}"
)
raise SystemExit(0 if len(vals) >= min_ok else 1)
PY
)
STATS_RC=$?

LAST_SRV=$(grep '^SRV ' "$SRV_LOG" | tail -1)
echo "engine last stats: ${LAST_SRV:-<none>}"
echo "$STATS"
[ "$FAILED" -gt 0 ] && echo "note: $FAILED run(s) failed outright (see $LOG_DIR)"
exit $STATS_RC
