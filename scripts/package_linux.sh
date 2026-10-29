#!/usr/bin/env bash
# Package the Linux release artifacts: dist/openay-linux/ (GUI + headless
# server + platform README) and a .tar.gz next to it. Version comes from
# the workspace manifest.

set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
if [ ! -f "$ROOT/scripts/env.sh" ]; then
  echo "error: $ROOT/scripts/env.sh missing (copy scripts/env.sh.example)" >&2
  exit 1
fi
# shellcheck disable=SC1091
source "$ROOT/scripts/env.sh"

VERSION="$(awk '/^version = /{gsub(/"/,""); sub(/version = /,""); print; exit}' "$ROOT/desktop/Cargo.toml")"
DIST="$ROOT/dist/openay-linux"
GUI="$ROOT/desktop/target/release/openay-gui"
SERVER="$ROOT/desktop/target/release/openay-server"

echo "== Building Linux release binaries (PipeWire output) =="
(cd desktop && cargo build --release -p openay-gui -p openay-server \
  --features openay-server/pipewire >>/tmp/openay-linux-build.log 2>&1) || {
  echo "error: release build failed (see /tmp/openay-linux-build.log)" >&2; exit 1; }

echo "== Staging dist/openay-linux =="
rm -rf "$DIST"
mkdir -p "$DIST"
cp -f "$GUI" "$DIST/"
cp -f "$SERVER" "$DIST/"

cat > "$DIST/README-LINUX.txt" <<EOF
OpenAY Mic $VERSION — Linux receiver (x86_64)

Contents
--------
  openay-gui     desktop console (recommended): The Chain, VU meter,
                 ON AIR toggle, settings slide-over
  openay-server  headless receiver (same engine, no window)

Requirements
------------
  * Linux x86_64 with PipeWire 1.0+ running (the built-in audio server on
    Ubuntu 22.04+, Fedora 36+; install libpipewire-0.3 if absent)
  * libopus (libopus0) — present on the same distributions
  * Built on Ubuntu 24.04 (glibc 2.39); older distributions may need to
    build from source instead (see the repository README).

Running (Wi-Fi)
---------------
  1. Allow inbound UDP 41700 through your firewall:
         sudo ufw allow 41700/udp
  2. ./openay-gui          (console)   or   ./openay-server
  3. On the phone: transport UDP, host = this PC's IP (hostname -I),
     port 41700, then ON AIR.
  4. "OpenAY Mic" appears as a microphone; select it in any application,
     or record a test clip:
         pw-cat --record --target openay_mic --rate 48000 --channels 1 \
           --format s16 /tmp/test.wav

Running (USB)
-------------
  adb forward tcp:41700 tcp:41700
  On the phone: transport TCP, host 127.0.0.1, port 41700.

Tuning
------
  OPENAY_NODE_LATENCY=480/48000 ./openay-server ...
  requests a smaller PipeWire quantum: measured ~10 ms lower latency for
  a few tenths of a percent CPU.
EOF

echo "== Packing tarball =="
tar -czf "$ROOT/dist/openay-mic-$VERSION-linux-x86_64.tar.gz" -C "$DIST" .
echo "dist/openay-mic-$VERSION-linux-x86_64.tar.gz"
