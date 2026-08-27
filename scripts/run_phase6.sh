#!/usr/bin/env bash
# Phase 6 validation gate: builds everything, then runs
#   1. native unit/integration tests (ctest)
#   2. Rust workspace tests (cargo test --workspace) PLUS a standalone
#      `cargo test -p openay-server` run — the adaptive-jitter scenarios
#      (tests/adaptive_depth.rs) are gated on not(feature = "pipewire")
#      and compile out of the workspace run via feature unification
#      (openay-gui enables openay-server/pipewire)
#   3. QA-kit python self-tests (click track / click analysis / latency math)
#   4. proxy CLI smoke: real openay-proxy binary under uniform loss,
#      delivery ratio must match the profile
#   5. software latency probe (needs a reachable PipeWire daemon; SKIP if
#      none) — verdict from scripts/latency_probe.sh
#   6. CPU budget assertions: idle < 1 %CPU, Opus-active < 3 %CPU
#
# Port ranges owned by this script: none directly (children own their own):
#   phase2 416xx | unit tests 417xx | this gate's proxy smoke 41860-41869 |
#   latency probe 41890 | cpu_profile 41901-41903
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
if [ ! -f "$ROOT/scripts/env.sh" ]; then
  echo "error: scripts/env.sh not found — copy scripts/env.sh.example to" >&2
  echo "       scripts/env.sh and adjust the marked values (see README)." >&2
  exit 1
fi
source "$ROOT/scripts/env.sh"

PASS=0; FAIL=0; SKIP=0
check() { # <name> <exit_code>
  if [ "$2" -eq 0 ]; then echo "[PASS] $1"; PASS=$((PASS + 1));
  else echo "[FAIL] $1"; FAIL=$((FAIL + 1)); fi
}
skip() { # <name>
  echo "[SKIP] $1"; SKIP=$((SKIP + 1));
}

echo "== Building C++ native core =="
cmake -S android/native -B android/native/build-host -DCMAKE_BUILD_TYPE=Release >/tmp/openay-cpp-build.log 2>&1 &&
  cmake --build android/native/build-host -j"$(nproc)" >>/tmp/openay-cpp-build.log 2>&1
check "cpp build" $?

echo "== Building Rust workspace (release) =="
(cd desktop && cargo build --workspace --release >/tmp/openay-rust-build.log 2>&1)
check "rust build" $?

echo "== C++ tests (ctest) =="
ctest --test-dir android/native/build-host >/tmp/openay-cpp-test.log 2>&1
check "cpp ctest" $?

echo "== Rust tests (cargo test --workspace) =="
(cd desktop && cargo test --workspace >/tmp/openay-rust-test.log 2>&1)
check "rust workspace tests" $?

echo "== Adaptive depth scenarios (headless: cargo test -p openay-server) =="
(cd desktop && cargo test -p openay-server >/tmp/openay-adaptive.log 2>&1)
check "adaptive depth scenarios" $?
grep -h "^test result" /tmp/openay-adaptive.log | tail -2 | sed 's/^/    /'

echo "== Python tool self-tests =="
python3 scripts/gen_click_track.py --self-test >/tmp/openay-clicktrack-selftest.log 2>&1
check "gen_click_track self-test" $?
python3 scripts/analyze_clicks.py --self-test >/tmp/openay-clicks-selftest.log 2>&1
check "analyze_clicks self-test" $?
python3 scripts/analyze_latency.py --self-test >/tmp/openay-latency-selftest.log 2>&1
check "analyze_latency self-test" $?

echo "== Proxy CLI smoke (loss2 over real sockets) =="
P_PROXY=41860; P_SINK=41861
python3 - "$P_PROXY" "$P_SINK" <<'PY' >/tmp/openay-proxy-smoke.log 2>&1
import socket, subprocess, sys, threading, time
proxy_port, sink_port = int(sys.argv[1]), int(sys.argv[2])
sink = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sink.bind(("127.0.0.1", sink_port))
sink.settimeout(0.2)
# Drain concurrently with the sender: a socket that is not read while the
# sender paces its stream overflows its kernel rcvbuf (~276 tiny datagrams
# by default) and the loss profile gets blamed for OS-side drops.
seen = 0
stop = threading.Event()
def drain():
    global seen
    while not stop.is_set():
        try:
            sink.recvfrom(2048); seen += 1
        except socket.timeout:
            continue
reader = threading.Thread(target=drain)
reader.start()
proxy = subprocess.Popen(
    ["desktop/target/release/openay-proxy",
     "--listen", f"127.0.0.1:{proxy_port}",
     "--forward", f"127.0.0.1:{sink_port}",
     "--profile", "loss2", "--seed", "7"])
time.sleep(0.5)
sender = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sent = 400
for i in range(sent):
    sender.sendto(b"x" * 480, ("127.0.0.1", proxy_port))
    time.sleep(0.0025)   # 400 pkt/s, well below the reader task's ceiling
time.sleep(0.5)          # let the pipe empty before closing
stop.set(); reader.join(timeout=2)
proxy.terminate(); proxy.wait(timeout=5)
# loss2 drops ~2%: accept [95%, 99.9%] (never 100%: the seeded sequence
# contains drops; never catastrophic loss).
ok = 0.95 * sent <= seen < sent * 0.999
print(f"sent={sent} seen={seen}")
sys.exit(0 if ok else 1)
PY
check "proxy loss2 delivery ratio" $?
tail -1 /tmp/openay-proxy-smoke.log | sed 's/^/    /'

echo "== Software latency probe (PipeWire-dependent) =="
if pw-cli info 0 >/dev/null 2>&1; then
  ./scripts/latency_probe.sh --runs 4 --warmup 1 --duration 2.0 \
    >/tmp/openay-latency-probe.log 2>&1
  check "latency probe verdict" $?
  grep -E "^LATPROBE" /tmp/openay-latency-probe.log | sed 's/^/    /'
else
  skip "latency probe (no PipeWire daemon)"
fi

echo "== CPU budgets (idle <1%, opus <3%) =="
./scripts/cpu_profile.sh --assert >/tmp/openay-cpu-profile.log 2>&1
check "cpu budget assertions" $?
grep -E "^CPU scenario" /tmp/openay-cpu-profile.log | sed 's/^/    /'

echo
echo "=========================================="
echo " PHASE 6 GATE: $PASS passed, $FAIL failed, $SKIP skipped"
echo "=========================================="
[ "$FAIL" -eq 0 ]
