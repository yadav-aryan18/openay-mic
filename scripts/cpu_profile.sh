#!/usr/bin/env bash
# Phase 6 CPU budget profiler: measures %CPU and peak RSS of the OpenAY
# desktop processes (openay-server, optionally openay-gui) across three
# scenarios:
#   idle           server running, no sender stream
#   pcm/opus       server + openay_loopback tone-udp sender at 440 Hz
#
# Sampling is a 200 ms loop over /proc/<pid>/stat:
#   ticks   = utime(14) + stime(15)   (field numbers in the *stripped* stat
#             line after "pid (comm) " are therefore 12 and 13 — see stat_fields)
#   %CPU    = (delta_ticks / CLK_TCK) / delta_wall * 100
#   RSS     = pages(24 -> stripped field 22) * PAGE_SIZE/1024  KiB
# RSS reported is the peak observed over the window (memory-budget oriented);
# %CPU reported is the window mean.
#
# Budget assertions (--assert): idle < 1.00 %CPU, opus < 3.00 %CPU on the
# server rows; a +0.25 tolerance band only warns, beyond it the run FAILs.
#
# Dev flag: --mock-pid <pid> profiles a live process with the identical
# sampling math — the standalone way to verify the script without the
# (possibly concurrently-edited) desktop/android builds.
#
# Logs go to /tmp/openay-cpu-*.log; ports used: 41901-41903 (private range).
set -u
# Pin the locale so awk/printf decimals stay dot-formatted regardless of
# the host's LC_NUMERIC (the CPU lines are parsed as floats downstream).
export LC_ALL=C

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
if [ ! -f "$ROOT/scripts/env.sh" ]; then
  echo "error: scripts/env.sh not found — copy scripts/env.sh.example to" >&2
  echo "       scripts/env.sh and adjust the marked values (see README)." >&2
  exit 1
fi
source "$ROOT/scripts/env.sh"

PASS=0; FAIL=0
check() { # <name> <exit_code>
  if [ "$2" -eq 0 ]; then echo "[PASS] $1"; PASS=$((PASS + 1)); else echo "[FAIL] $1"; FAIL=$((FAIL + 1)); fi
}

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
PORT_BASE=41901                # private UDP range: 41901 idle, 41902 pcm, 41903 opus
SCENARIO_SECONDS=10            # sampling/stream window per scenario
SAMPLE_INTERVAL=0.2            # s between /proc/<pid>/stat reads
CLK_TCK="$(getconf CLK_TCK 2>/dev/null || echo 100)"
PAGE_KB=$(( $(getconf PAGESIZE 2>/dev/null || echo 4096) / 1024 ))

SERVER_BIN="$ROOT/desktop/target/release/openay-server"
LOOPBACK_BIN="$ROOT/desktop/target/release/openay-loopback"
NATIVE_BIN="$ROOT/android/native/build-host/openay_loopback"
GUI_BIN="$ROOT/desktop/target/release/openay-gui"

MODE=measure          # measure | assert
WITH_GUI=0
MOCK_PID=""

# Pids started by us, killed on EXIT so no stray servers leak.
STARTED_PIDS=""

trap 'for p in $STARTED_PIDS; do kill "$p" 2>/dev/null; done' EXIT

# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
while [ $# -gt 0 ]; do
  case "$1" in
    --assert)   MODE=assert; shift ;;
    --with-gui) WITH_GUI=1; shift ;;
    --mock-pid)
      [ $# -ge 2 ] || { echo "error: --mock-pid requires a pid" >&2; exit 2; }
      MOCK_PID="$2"; shift 2 ;;
    *)
      echo "error: unknown argument '$1' (--assert | --with-gui | --mock-pid <pid>)" >&2
      exit 2 ;;
  esac
done

# ---------------------------------------------------------------------------
# Sampling primitives (shared by real and mock measurement)
# ---------------------------------------------------------------------------
stat_fields() { # <pid> -> "utime stime rss_pages"; empty + nonzero on error
  local pid="$1" stat rest
  [ -r "/proc/$pid/stat" ] || return 1
  stat="$(cat "/proc/$pid/stat" 2>/dev/null)" || return 1
  rest="${stat##*) }"   # strip "pid (comm) " — fields beyond the last ')'
  echo "$rest" | awk '{print $12, $13, $22}'   # utime, stime, rss (pages)
}

# profile_pid <pid> <duration_s> -> prints "cpu_pct rss_kib samples count"
profile_pid() {
  local pid="$1" duration="$2"
  local line utime stime pages ticks
  local prev_utime="" prev_stime="" prev_ticks="" prev_ns=""
  local cpu_sum=0.0 rss_max=0 samples=0 now
  local start; start="$(date +%s.%N)"

  while :; do
    now="$(date +%s.%N)"
    awk -v a="$now" -v b="$start" 'BEGIN{exit !((a-b) >= '$duration' + 0)}' && break
    if line="$(stat_fields "$pid")"; then
      set -- $line
      utime="$1"; stime="$2"; pages="$3"
      if [ -n "$utime" ] && [ -n "$pages" ]; then
        if [ -n "$prev_ticks" ]; then
          ticks=$(( utime - prev_utime + stime - prev_stime ))
          if [ "$ticks" -lt 0 ]; then ticks=0; fi
          wall="$(awk -v a="$now" -v b="$prev_ns" 'BEGIN{print (a-b) + 0}')"
          cpu="$(awk -v t="$ticks" -v c="$CLK_TCK" -v w="$wall" \
                  'BEGIN{ if (w <= 0) print 0; else printf "%.3f", (t/c)/w*100 }')"
          cpu_sum="$(awk -v s="$cpu_sum" -v c="$cpu" 'BEGIN{printf "%.3f", s+c}')"
          rss_kib=$(( pages * PAGE_KB ))
          [ "$rss_kib" -gt "$rss_max" ] && rss_max="$rss_kib"
          samples=$(( samples + 1 ))
        fi
        prev_utime="$utime"; prev_stime="$stime"
        prev_ticks=$(( utime + stime )); prev_ns="$now"
      fi
    fi
    sleep "$SAMPLE_INTERVAL"
  done

  if [ "$samples" -lt 1 ]; then
    echo "0.00 0 0"
    return
  fi
  cpu="$(awk -v s="$cpu_sum" -v n="$samples" 'BEGIN{printf "%.2f", s/n}')"
  echo "$cpu $rss_max $samples"
}

# ---------------------------------------------------------------------------
# Builds (fail soft: log, retry once, then WARN and fall back / report)
# ---------------------------------------------------------------------------
build_server() {
  [ -x "$SERVER_BIN" ] && return 0
  local log=/tmp/openay-cpu-server-build.log
  (cd desktop && cargo build --release -p openay-server) >"$log" 2>&1
  if [ ! -x "$SERVER_BIN" ]; then
    echo "WARN server build failed, retrying in 60 s: $(tail -1 "$log")" >&2
    sleep 60
    (cd desktop && cargo build --release -p openay-server) >>"$log" 2>&1
  fi
  [ -x "$SERVER_BIN" ]
}

build_native_sender() {
  [ -x "$NATIVE_BIN" ] && return 0
  local log=/tmp/openay-cpu-cpp-build.log
  cmake -S android/native -B android/native/build-host -DCMAKE_BUILD_TYPE=Release >"$log" 2>&1 &&
    cmake --build android/native/build-host -j"$(nproc)" >>"$log" 2>&1
  [ -x "$NATIVE_BIN" ]
}

# ---------------------------------------------------------------------------
# Scenario runner: starts server (+ sender + optional GUI), profiles, cleans up
# ---------------------------------------------------------------------------
start_server() { # <port> <label> -> echoes the server pid (or "" on failure)
  local port="$1" label="$2" i
  "$SERVER_BIN" --transport udp --port "$port" --bind 127.0.0.1 \
      >"/tmp/openay-cpu-server-$label.log" 2>&1 &
  local srv=$!
  STARTED_PIDS="$STARTED_PIDS $srv"
  # Port live within ~200 ms per the server's cold-start contract; poll up to 5 s.
  for i in $(seq 1 25); do
    kill -0 "$srv" 2>/dev/null || { echo ""; return 1; }
    if command -v ss >/dev/null 2>&1 && ss -uln 2>/dev/null | grep -q "[:.]$port "; then
      echo "$srv"; return 0
    fi
    sleep 0.2
  done
  echo "$srv"
}

run_scenario() { # <label> <port> <sender: none|pcm|opus>
  local label="$1" port="$2" sender="$3"
  local srv gui="" spid="" srv_cpu srv_rss srv_n gui_cpu gui_rss gui_n
  echo "== scenario: $label (port $port) =="

  srv="$(start_server "$port" "$label")"
  if [ -z "$srv" ] || ! kill -0 "$srv" 2>/dev/null; then
    echo "WARN server-$label exited early; see /tmp/openay-cpu-server-$label.log" >&2
    check "server up ($label)" 1
    return 1
  fi

  if [ "$sender" = none ]; then
    : # idle: no sender
  else
    if [ -x "$NATIVE_BIN" ]; then
      "$NATIVE_BIN" tone-udp 127.0.0.1 "$port" "$SCENARIO_SECONDS" 440 "$sender" \
          >"/tmp/openay-cpu-sender-$label.log" 2>&1 &
      spid=$!
      STARTED_PIDS="$STARTED_PIDS $spid"
    else
      # Degraded fallback: Rust loopback sender paced to ~100 pkt/s.
      echo "WARN native sender unavailable; using openay-loopback send-udp (degraded fidelity: 100 pkt/s, 480 B filler, not real audio capture)" >&2
      count=$(( SCENARIO_SECONDS * 100 ))
      "$LOOPBACK_BIN" send-udp 127.0.0.1 "$port" "$count" 480 10000 \
          >"/tmp/openay-cpu-sender-$label.log" 2>&1 &
      spid=$!
      STARTED_PIDS="$STARTED_PIDS $spid"
      check "sender build (native)" 1   # count the missing native binary as a fail
    fi
  fi

  if [ "$WITH_GUI" -eq 1 ]; then
    if [ -z "${DISPLAY:-}" ]; then
      echo "WARN gui skipped reason=no-display (budgets are on the server anyway)" >&2
    elif [ ! -x "$GUI_BIN" ]; then
      echo "WARN gui binary missing; skipping (build: cargo build -p openay-gui --release)" >&2
    else
      "$GUI_BIN" >"/tmp/openay-cpu-sender-gui.log" 2>&1 &
      gui=$!
      STARTED_PIDS="$STARTED_PIDS $gui"
      sleep 1.0  # let the window come up before sampling
      kill -0 "$gui" 2>/dev/null || { echo "WARN gui exited early pid=$gui" >&2; gui=""; }
    fi
  fi

  read -r srv_cpu srv_rss srv_n <<<"$(profile_pid "$srv" "$SCENARIO_SECONDS")"
  if [ -n "$spid" ]; then wait "$spid" 2>/dev/null; fi
  if [ -n "$gui" ]; then
    read -r gui_cpu gui_rss gui_n <<<"$(profile_pid "$gui" "$SCENARIO_SECONDS")"
  fi

  printf 'CPU scenario=%s proc=server cpu_pct=%s rss_kib=%s samples=%s\n' \
      "$label" "$srv_cpu" "$srv_rss" "$srv_n"
  if [ -n "$gui" ]; then
    printf 'CPU scenario=%s proc=gui cpu_pct=%s rss_kib=%s samples=%s\n' \
        "$label" "$gui_cpu" "$gui_rss" "$gui_n"
  fi

  kill "$srv" 2>/dev/null; wait "$srv" 2>/dev/null
  [ -n "$gui" ] && { kill "$gui" 2>/dev/null; wait "$gui" 2>/dev/null; }
  if [ "$MODE" = assert ]; then
    budget_check "$label" "server" "$srv_cpu"
  fi
}

budget_check() { # <scenario> <proc> <cpu_pct>
  local scenario="$1" proc="$2" cpu="$3" thr
  case "$scenario" in
    idle) thr=1.00 ;;
    opus) thr=3.00 ;;
    *) return 0 ;;
  esac
  local st
  st="$(awk -v c="$cpu" -v t="$thr" 'BEGIN{ if (c < t) print "PASS"; else if (c < t + 0.25) print "TOL"; else print "FAIL" }')"
  case "$st" in
    PASS) check "budget $scenario $proc cpu<$thr% (got ${cpu}%)" 0 ;;
    TOL)  echo "[TOL ] budget $scenario $proc cpu=${cpu}% inside the +0.25 tolerance band"; PASS=$((PASS+1)) ;;
    FAIL) echo "[FAIL] budget $scenario $proc cpu=${cpu}% >= ${thr}+0.25%"; FAIL=$((FAIL+1)) ;;
  esac
}

# ---------------------------------------------------------------------------
# Mock verification path (--mock-pid): identical sampling math, no builds
# ---------------------------------------------------------------------------
mock_measure() {
  local pid="$1"
  if ! [ -r "/proc/$pid/stat" ]; then
    echo "error: --mock-pid $pid: no such process" >&2
    return 2
  fi
  echo "== mock: profiling pid $pid for 5 s (same sampling math) =="
  local name
  name="$(awk 'NR==1 {print $2}' "/proc/$pid/stat" 2>/dev/null | tr -d '()')"
  read -r cpu rss n <<<"$(profile_pid "$pid" 5)"
  printf 'CPU scenario=mock proc=mock cpu_pct=%s rss_kib=%s samples=%s\n' "$cpu" "$rss" "$n"
  printf 'MOCK-OK comm=%s\n' "$name"
}

# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------
if [ -n "$MOCK_PID" ]; then
  mock_measure "$MOCK_PID"
  exit $?
fi

echo "== CPU profiler: CLK_TCK=$CLK_TCK PAGE_KB=${PAGE_KB}KiB window=${SCENARIO_SECONDS}s =="
check "server binary (build on demand)" "$(build_server; echo $?)"
if [ "$FAIL" -ne 0 ] && [ ! -x "$SERVER_BIN" ]; then
  echo "WARN server binary unavailable after retry; real measurement skipped" >&2
  echo "      (use ./cpu_profile.sh --mock-pid <pid> to verify the sampling logic)" >&2
  echo
  echo "=========================================="
  echo " CPU PROFILE GATE: $PASS passed, $FAIL failed"
  echo "=========================================="
  exit 1
fi

check "native sender build (optional if build-host exists)" "$(build_native_sender; echo $?)"

run_scenario idle "$PORT_BASE" none
run_scenario pcm  "$((PORT_BASE + 1))" pcm
run_scenario opus "$((PORT_BASE + 2))" opus

echo
echo "=========================================="
echo " CPU PROFILE GATE: $PASS passed, $FAIL failed"
echo "=========================================="
[ "$FAIL" -eq 0 ]
