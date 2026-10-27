#!/usr/bin/env bash
# OpenAY Mic desktop test-session logger (Ubuntu).
#
# Wraps the desktop app so every run is captured to a timestamped log
# directory while output still streams to the console:
#
#   scripts/devlog.sh                      # headless server, UDP 41700, PipeWire mic
#   scripts/devlog.sh --gui                # the Iced console instead
#   scripts/devlog.sh --transport bt       # Bluetooth SPP mode (feature/bluetooth build)
#   scripts/devlog.sh --adb                # ALSO capture the phone's logcat (USB debugging)
#
# Logs land in ~/OpenAY-logs/<yyyyMMdd-HHmmss>/ :
#   app.log        everything the app printed (SRV stats, UNDERRUN lines, errors)
#   env.txt        OS/toolchain/PipeWire snapshot for reproducing
#   phone.logcat   the phone's capture-engine logs (with --adb and a device)
# Ctrl-C stops the session; the directory is kept. Zip it and attach it to a
# bug report.

set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

MODE=server
WITH_ADB=0
APP_ARGS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --gui) MODE=gui; shift ;;
    --adb) WITH_ADB=1; shift ;;
    *) APP_ARGS+=("$1"); shift ;;
  esac
done

if [ ! -f "$ROOT/scripts/env.sh" ]; then
  echo "error: $ROOT/scripts/env.sh missing (copy scripts/env.sh.example)" >&2
  exit 1
fi
# shellcheck disable=SC1091
source "$ROOT/scripts/env.sh"

SERVER_BIN="$ROOT/desktop/target/release/openay-server"
GUI_BIN="$ROOT/desktop/target/release/openay-gui"
if [ "$MODE" = gui ]; then
  APP_BIN="$GUI_BIN"
else
  APP_BIN="$SERVER_BIN"
fi
if [ ! -x "$APP_BIN" ]; then
  echo "error: $APP_BIN missing — build it first:" >&2
  echo "  (cd desktop && cargo build --release -p openay-server --features pipewire)" >&2
  echo "  (cd desktop && cargo build --release -p openay-gui)" >&2
  exit 1
fi

STAMP="$(date +%Y%m%d-%H%M%S)"
LOG_DIR="${OPENAY_LOG_DIR:-$HOME/OpenAY-logs/$STAMP}"
mkdir -p "$LOG_DIR"

# Environment snapshot: reproduce a bug report against the exact versions.
{
  date
  uname -a
  echo "--- binaries"
  "$SERVER_BIN" --version 2>/dev/null || echo "server: n/a"
  echo "--- pipewire"
  pw-cli --version 2>/dev/null | head -1
  pw-cli info 0 >/dev/null 2>&1 && echo "daemon: reachable" || echo "daemon: NOT reachable"
  echo "--- relevant env"
  env | grep -E "OPENAY|PIPEWIRE|LD_LIBRARY_PATH" | sort
} >"$LOG_DIR/env.txt" 2>&1

# Virtual source presence, checked again live while the session runs.
pw-cli ls Node 2>/dev/null | grep -n "openay_mic" >"$LOG_DIR/openay_mic.node" 2>&1 || true

ADB_PID=""
if [ "$WITH_ADB" -eq 1 ]; then
  ADB="$ANDROID_HOME/platform-tools/adb"
  if [ -x "$ADB" ] && "$ADB" get-state >/dev/null 2>&1; then
    # The two tags that matter: the Kotlin service (stats/xrun warnings) and
    # the native engine (capture/pipeline errors).
    "$ADB" logcat -s MicCaptureService:* openaymic:* >"$LOG_DIR/phone.logcat" 2>&1 &
    ADB_PID=$!
    echo "phone logcat -> $LOG_DIR/phone.logcat"
  else
    echo "note: --adb requested but no device attached — skipping logcat" >&2
  fi
fi

cleanup() {
  [ -n "$ADB_PID" ] && kill "$ADB_PID" 2>/dev/null
}
trap cleanup EXIT

echo "Logging to $LOG_DIR/app.log  (Ctrl-C ends the session, logs are kept)"
if [ "$MODE" = gui ]; then
  # The GUI is a windowed app: capture stdout/stderr only.
  stdbuf -oL -eL "$APP_BIN" "${APP_ARGS[@]}" 2>&1 | tee "$LOG_DIR/app.log"
  GUI_RC=${PIPESTATUS[0]}
  echo "gui exited rc=$GUI_RC" | tee -a "$LOG_DIR/app.log"
else
  stdbuf -oL -eL "$APP_BIN" "${APP_ARGS[@]}" 2>&1 | tee "$LOG_DIR/app.log"
fi
