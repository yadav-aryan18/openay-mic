#!/usr/bin/env bash
# Phase 1+2 validation gate: builds both implementations from scratch and runs
#   1. native unit/integration tests on each side (ctest / cargo test)
#   2. cross-language wire-format interop over UDP and TCP (both directions)
#   3. loopback latency benches (plan target: <5 ms network overhead)
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
source "$ROOT/scripts/env.sh"

PASS=0; FAIL=0
check() { # <name> <exit_code>
  if [ "$2" -eq 0 ]; then echo "[PASS] $1"; PASS=$((PASS + 1)); else echo "[FAIL] $1"; FAIL=$((FAIL + 1)); fi
}

CPP_BIN="$ROOT/android/native/build-host/openay_loopback"
RUST_BIN="$ROOT/desktop/target/release/openay-loopback"
P_BASE=41600   # interop port range owned by this script

echo "== Building C++ native core =="
cmake -S android/native -B android/native/build-host -DCMAKE_BUILD_TYPE=Release >/tmp/openay-cpp-build.log 2>&1 &&
  cmake --build android/native/build-host -j"$(nproc)" >>/tmp/openay-cpp-build.log 2>&1
check "cpp build" $?

echo "== Building Rust workspace =="
(cd desktop && cargo build --workspace --release >/tmp/openay-rust-build.log 2>&1)
check "rust build" $?

echo "== C++ tests (ctest) =="
ctest --test-dir android/native/build-host >/tmp/openay-cpp-test.log 2>&1
check "cpp ctest" $?

echo "== Rust tests (cargo) =="
(cd desktop && cargo test --workspace >/tmp/openay-rust-test.log 2>&1)
check "cargo test" $?

run_interop() { # <name> <receiver-bin> <recv-cmd...> ; <sender-bin> <send-cmd...> passed via extra args
  local name="$1" rbin="$2" sbin="$3"; shift 3
  local recv_kind="$1" port="$2" count="$3" size="$4"
  "$rbin" "recv-$recv_kind" "$port" "$count" "$size" >/tmp/openay-recv.log 2>&1 &
  local rp=$!
  sleep 0.4
  "$sbin" "send-$recv_kind" 127.0.0.1 "$port" "$count" "$size" >/tmp/openay-send.log 2>&1
  local sc=$?
  wait $rp
  local rc=$?
  check "$name" $(( sc == 0 && rc == 0 ? 0 : 1 ))
  echo "    $(cat /tmp/openay-send.log | tail -1)"
  echo "    $(cat /tmp/openay-recv.log | tail -1)"
}

echo "== Cross-language interop (5000 pkts @480B flat-out) =="
run_interop "udp cpp->rust" "$RUST_BIN" "$CPP_BIN" udp $((P_BASE + 1)) 5000 480
run_interop "udp rust->cpp" "$CPP_BIN" "$RUST_BIN" udp $((P_BASE + 2)) 5000 480
run_interop "tcp cpp->rust" "$RUST_BIN" "$CPP_BIN" tcp $((P_BASE + 3)) 5000 480
run_interop "tcp rust->cpp" "$CPP_BIN" "$RUST_BIN" tcp $((P_BASE + 4)) 5000 480

echo "== Interop at MTU-safe payload boundary (1400 B) =="
run_interop "udp cpp->rust 1400B" "$RUST_BIN" "$CPP_BIN" udp $((P_BASE + 5)) 3000 1400
run_interop "udp rust->cpp 1400B" "$CPP_BIN" "$RUST_BIN" udp $((P_BASE + 6)) 3000 1400

echo "== Loopback latency benches (target p99 < 5 ms) =="
"$CPP_BIN" bench udp $((P_BASE + 11)) 20000 >/tmp/openay-bench-cpp-udp.log 2>&1
check "bench cpp udp (p99<5ms)" $?
tail -1 /tmp/openay-bench-cpp-udp.log
"$CPP_BIN" bench tcp $((P_BASE + 12)) 20000 >/tmp/openay-bench-cpp-tcp.log 2>&1
check "bench cpp tcp (p99<5ms)" $?
tail -1 /tmp/openay-bench-cpp-tcp.log
"$RUST_BIN" bench udp $((P_BASE + 13)) 20000 >/tmp/openay-bench-rust-udp.log 2>&1
check "bench rust udp (p99<5ms)" $?
tail -1 /tmp/openay-bench-rust-udp.log
"$RUST_BIN" bench tcp $((P_BASE + 14)) 20000 >/tmp/openay-bench-rust-tcp.log 2>&1
check "bench rust tcp (p99<5ms)" $?
tail -1 /tmp/openay-bench-rust-tcp.log

echo
echo "=========================================="
echo " PHASE 1+2 GATE: $PASS passed, $FAIL failed"
echo "=========================================="
[ "$FAIL" -eq 0 ]
