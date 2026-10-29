#!/usr/bin/env bash
# Windows client build: cross-compiles the same openay-server binary for
# x86_64-pc-windows-msvc and stages a distributable folder:
#
#   dist/openay-windows/openay-server.exe    (when linking works)
#   dist/openay-windows/README-WINDOWS.txt   (quick start, staged always)
#
# Toolchain (all user-space, no root required):
#   * rustup target x86_64-pc-windows-msvc (installed on demand)
#   * cargo-xwin — downloads the MSVC CRT/SDK headers + import libs and
#     drives clang-cl + lld-link. Install with: cargo install cargo-xwin
#   * LLVM tools (clang-cl, lld-link, llvm-ar) on PATH — only needed when a
#     dependency contains C code (libopus via audiopus-sys); pure-Rust code
#     links with rust-lld alone. On this host they live under the ROCm/LLVM
#     install (add its bin/ to PATH before running).
#
# libopus: audiopus-sys compiles its bundled opus via CMake, and that build
# fails under clang-cl — opus skips the per-file -msse4.1 flags whenever it
# thinks it talks to MSVC (clang-cl simulates MSVC), while clang still gates
# always_inline SSE4.1 intrinsics behind target features. The plain clang
# backend compiles opus but breaks at link time (undefined _mm_* symbols).
# Rather than patching the crate, this script builds a static /MT libopus
# once (intrinsics disabled: scalar opus decode is negligible for 48 kHz
# mono) and hands it to audiopus-sys via LIBOPUS_LIB_DIR, which its build
# script honors and which skips the CMake build entirely. The result is a
# single self-contained openay-server.exe (RUSTFLAGS crt-static: no
# vcruntime140.dll / VC++ redistributable needed on the target machine).
#
# Heavy caches stay beside the gitignored cargo target dir (override with
# XWIN_CACHE_DIR / OPENAY_WIN_OPUS_PREFIX):
#   XWIN_CACHE_DIR   MSVC SDK/CRT, several GB
#   xwin-libopus-*   the prebuilt static opus (small)
#
# Degradation: if the xwin build is impossible on this host (no cargo-xwin,
# MSVC download unreachable, no LLVM, ...), the script falls back to a
# compile-only gate for the Windows target (cargo xwin check, else plain
# `cargo check --target`) and exits non-zero with the reason. A mingw-w64
# alternative exists for hosts WITH sudo: rustup target add
# x86_64-pc-windows-gnu + `apt install mingw-w64`, then
# `cargo build --target x86_64-pc-windows-gnu -p openay-server`
# (openay-server has no MSVC-only requirements) — unusable on this host
# (no passwordless sudo). End-user docs: README-WINDOWS.txt.
set -u
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

TARGET=x86_64-pc-windows-msvc
DIST="$ROOT/dist/openay-windows"
SERVER_TOML="$ROOT/desktop/crates/openay-server/Cargo.toml"
README_WIN="$ROOT/README-WINDOWS.txt"
LOG=/tmp/openay-windows-build.log

# Heavy caches: default beside the (gitignored) cargo target dir so they do
# not fill ~/.cache on a small root disk. Honor pre-set values.
export XWIN_CACHE_DIR="${XWIN_CACHE_DIR:-$ROOT/desktop/target/xwin-cache}"
export XWIN_CROSS_COMPILER="${XWIN_CROSS_COMPILER:-clang-cl}"

echo "== Toolchain =="
if ! command -v cargo-xwin >/dev/null 2>&1; then
  echo "cargo-xwin not found — trying: cargo install cargo-xwin"
  cargo install cargo-xwin >>"$LOG" 2>&1 || true
fi
if command -v cargo-xwin >/dev/null 2>&1; then
  check "cargo-xwin available" 0
else
  check "cargo-xwin available (install with: cargo install cargo-xwin)" 1
fi

if rustup target list --installed 2>/dev/null | grep -q "^$TARGET$"; then
  check "rust target $TARGET" 0
else
  rustup target add "$TARGET" >>"$LOG" 2>&1
  check "rust target $TARGET (installed)" $?
fi

# LLVM tools are only REQUIRED when libopus has to be compiled from source.
# When a prebuilt static opus prefix exists (the normal, cached path) the
# audiopus_sys build script skips C compilation entirely and the pure-Rust
# link needs no clang-cl/lld-link on PATH at all. This host ships a
# user-space LLVM under /opt/rocm/llvm — add it opportunistically.
if [ -d /opt/rocm/llvm/bin ]; then
  export PATH="/opt/rocm/llvm/bin:$PATH"
fi

# --- Prebuilt static opus for the MSVC target (see header) ------------------
CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
# Pin the opus source to the Cargo.lock version — with two fetched
# audiopus_sys versions the wrong (untested) opus.lib could otherwise be
# linked into the prefix cache.
OPUS_LOCK_VER="$(awk '/^name = "audiopus_sys"$/{getline; sub(/.*version = "/,""); sub(/".*/,""); print; exit}' "$ROOT/desktop/Cargo.lock" 2>/dev/null)"
OPUS_SRC=""
if [ -n "$OPUS_LOCK_VER" ]; then
  OPUS_SRC="$(ls -d "$CARGO_HOME"/registry/src/*/audiopus_sys-"$OPUS_LOCK_VER"/opus 2>/dev/null)"
fi
if [ -z "$OPUS_SRC" ]; then
  OPUS_SRC="$(ls -d "$CARGO_HOME"/registry/src/*/audiopus_sys-*/opus 2>/dev/null | sort -V | tail -1)"
fi
OPUS_BASE="${OPENAY_WIN_OPUS_PREFIX:-$ROOT/desktop/target}"
OPUS_PREFIX=""
if [ -n "$OPUS_SRC" ]; then
  OPUS_VER="$(basename "$(dirname "$OPUS_SRC")")"
  OPUS_PREFIX="$OPUS_BASE/xwin-libopus-$OPUS_VER"
fi

# CMake toolchain for the opus build: cargo-xwin's clang-cl toolchain with a
# static CRT (CMP0091 NEW + MultiThreaded) so opus.lib matches the
# crt-static Rust exe. @XWIN_CACHE_DIR@ is substituted below.
write_opus_toolchain() { # <dir> <llvm-ar-path>
  mkdir -p "$1" || return 1
  cat > "$1/x86_64-pc-windows-msvc-toolchain.cmake" <<'EOF'
set(CMAKE_SYSTEM_NAME Windows)
set(CMAKE_SYSTEM_PROCESSOR AMD64)

set(CMAKE_C_COMPILER clang-cl CACHE FILEPATH "")
set(CMAKE_CXX_COMPILER clang-cl CACHE FILEPATH "")
set(CMAKE_AR "@LLVM_AR@")
set(CMAKE_LINKER lld-link CACHE FILEPATH "")
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded" CACHE STRING "" FORCE)

set(COMPILE_FLAGS
    --target=x86_64-pc-windows-msvc
    -Wno-unused-command-line-argument
    -fuse-ld=lld-link

    /imsvc @XWIN_CACHE_DIR@/xwin/crt/include
    /imsvc @XWIN_CACHE_DIR@/xwin/sdk/include/ucrt
    /imsvc @XWIN_CACHE_DIR@/xwin/sdk/include/um
    /imsvc @XWIN_CACHE_DIR@/xwin/sdk/include/shared
    /imsvc @XWIN_CACHE_DIR@/xwin/sdk/include/winrt)

set(LINK_FLAGS
    /manifest:no

    -libpath:"@XWIN_CACHE_DIR@/xwin/crt/lib/x86_64"
    -libpath:"@XWIN_CACHE_DIR@/xwin/sdk/lib/um/x86_64"
    -libpath:"@XWIN_CACHE_DIR@/xwin/sdk/lib/ucrt/x86_64")

string(REPLACE ";" " " COMPILE_FLAGS "${COMPILE_FLAGS}")

set(_CMAKE_C_FLAGS_INITIAL "${CMAKE_C_FLAGS}" CACHE STRING "")
set(CMAKE_C_FLAGS "${_CMAKE_C_FLAGS_INITIAL} ${COMPILE_FLAGS}" CACHE STRING "" FORCE)

set(_CMAKE_CXX_FLAGS_INITIAL "${CMAKE_CXX_FLAGS}" CACHE STRING "")
set(CMAKE_CXX_FLAGS "${_CMAKE_CXX_FLAGS_INITIAL} ${COMPILE_FLAGS} /EHsc" CACHE STRING "" FORCE)

string(REPLACE ";" " " LINK_FLAGS "${LINK_FLAGS}")

set(_CMAKE_EXE_LINKER_FLAGS_INITIAL "${CMAKE_EXE_LINKER_FLAGS}" CACHE STRING "")
set(CMAKE_EXE_LINKER_FLAGS "${_CMAKE_EXE_LINKER_FLAGS_INITIAL} ${LINK_FLAGS}" CACHE STRING "" FORCE)

set(_CMAKE_MODULE_LINKER_FLAGS_INITIAL "${CMAKE_MODULE_LINKER_FLAGS}" CACHE STRING "")
set(CMAKE_MODULE_LINKER_FLAGS "${_CMAKE_MODULE_LINKER_FLAGS_INITIAL} ${LINK_FLAGS}" CACHE STRING "" FORCE)

set(_CMAKE_SHARED_LINKER_FLAGS_INITIAL "${CMAKE_SHARED_LINKER_FLAGS}" CACHE STRING "")
set(CMAKE_SHARED_LINKER_FLAGS "${_CMAKE_SHARED_LINKER_FLAGS_INITIAL} ${LINK_FLAGS}" CACHE STRING "" FORCE)

# CMake populates these with a bunch of unnecessary libraries, which requires
# extra case-correcting symlinks and what not. Instead, let projects explicitly
# control which libraries they require.
set(CMAKE_C_STANDARD_LIBRARIES "" CACHE STRING "" FORCE)
set(CMAKE_CXX_STANDARD_LIBRARIES "" CACHE STRING "" FORCE)

set(CMAKE_TRY_COMPILE_CONFIGURATION Release)

# Allow clang-cl to work with POSIX-style absolute paths.
set(CMAKE_USER_MAKE_RULES_OVERRIDE "${CMAKE_CURRENT_LIST_DIR}/override.cmake")
EOF
  cat > "$1/override.cmake" <<'EOF'
# clang-cl interprets paths starting with / (e.g. /home, /media) as cl-style
# options, so the input file path needs a `--` separator. Overriding the
# compilation rules is the reliable way to get that in.
string(REPLACE "-c <SOURCE>" "-c -- <SOURCE>" CMAKE_C_COMPILE_OBJECT "${CMAKE_C_COMPILE_OBJECT}")
string(REPLACE "-c <SOURCE>" "-c -- <SOURCE>" CMAKE_CXX_COMPILE_OBJECT "${CMAKE_CXX_COMPILE_OBJECT}")
string(REPLACE "/D" "-D" CMAKE_RC_FLAGS "${CMAKE_RC_FLAGS_INIT}")
string(REPLACE "/D" "-D" CMAKE_RC_FLAGS_DEBUG "${CMAKE_RC_FLAGS_DEBUG_INIT}")
if(NOT CMAKE_HOST_WIN32)
  set(CMAKE_NINJA_CMCLDEPS_RC 0) # cmcldeps is windows only
endif()
EOF
  # Substitute via temp+mv (sed -i trips over NTFS permissions on some mounts).
  sed -e "s|@XWIN_CACHE_DIR@|$XWIN_CACHE_DIR|g" -e "s|@LLVM_AR@|$2|g" \
      "$1/x86_64-pc-windows-msvc-toolchain.cmake" > "$1/toolchain.cmake.tmp" &&
    mv -f "$1/toolchain.cmake.tmp" "$1/x86_64-pc-windows-msvc-toolchain.cmake"
}

build_prebuilt_opus() { # -> 0 on success
  local tc="$OPUS_PREFIX/toolchain"
  # CMake drives the archiver with MSVC-style flags (/out:...), so it must be
  # llvm-lib (lib.exe CLI). llvm-lib is llvm-ar dispatched by argv[0]; many
  # LLVM installs lack the alias, so materialize it as a copy (NTFS-safe).
  local llvm_ar
  if command -v llvm-lib >/dev/null 2>&1; then
    llvm_ar="$(command -v llvm-lib)"
  elif command -v llvm-ar >/dev/null 2>&1; then
    mkdir -p "$tc/shims" || return 1
    cp -f "$(command -v llvm-ar)" "$tc/shims/llvm-lib" || return 1
    llvm_ar="$tc/shims/llvm-lib"
  else
    return 1
  fi
  write_opus_toolchain "$tc" "$llvm_ar" || return 1
  cmake -S "$OPUS_SRC" -B "$OPUS_PREFIX/build" -G Ninja \
      -DCMAKE_TOOLCHAIN_FILE="$tc/x86_64-pc-windows-msvc-toolchain.cmake" \
      -DCMAKE_POLICY_DEFAULT_CMP0091=NEW -DCMAKE_POLICY_VERSION_MINIMUM=3.5 \
      -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$OPUS_PREFIX" \
      -DOPUS_DISABLE_INTRINSICS=ON -DOPUS_BUILD_SHARED_LIBRARY=OFF \
      -DOPUS_BUILD_PROGRAMS=OFF -DOPUS_BUILD_TESTING=OFF >>"$LOG" 2>&1 &&
    cmake --build "$OPUS_PREFIX/build" --target install >>"$LOG" 2>&1
}

OPUS_READY=0
if [ -z "$OPUS_SRC" ]; then
  echo "note: audiopus_sys opus source not found under $CARGO_HOME —" >&2
  echo "      run 'cargo fetch' once, or the libopus step cannot run." >&2
elif [ -f "$OPUS_PREFIX/lib/opus.lib" ]; then
  OPUS_READY=1   # already built (idempotent re-run)
elif command -v cmake >/dev/null 2>&1 && command -v ninja >/dev/null 2>&1 \
  && command -v clang-cl >/dev/null 2>&1; then
  echo "== Building static libopus for $TARGET (once, cached) =="
  build_prebuilt_opus && OPUS_READY=1
fi
if [ "$OPUS_READY" -eq 1 ] && [ -n "$OPUS_PREFIX" ] && [ -f "$OPUS_PREFIX/lib/opus.lib" ]; then
  export LIBOPUS_LIB_DIR="$OPUS_PREFIX"   # audiopus-sys skips its own CMake build
  check "prebuilt static libopus ($OPUS_VER)" 0
else
  # LLVM tools only matter here: with no prebuilt libopus, the C build
  # (clang-cl + lld-link) is the only way forward. With the prefix in place
  # the whole build is pure-Rust linking and needs no LLVM on PATH.
  if command -v clang-cl >/dev/null 2>&1 && command -v lld-link >/dev/null 2>&1; then
    check "LLVM toolchain present (needed to build libopus)" 0
  else
    check "LLVM toolchain present (needed to build libopus)" 1
    echo "note: install LLVM in user space (e.g. /opt/rocm/llvm) and add bin/ to PATH," >&2
    echo "      or prebuild the static libopus prefix once on a host that has it." >&2
  fi
  check "prebuilt static libopus (see $LOG)" 1
  echo "note: without it, audiopus-sys runs its own CMake build, which is" >&2
  echo "      known to fail under clang-cl (SSE4.1 gating) — both the exe" >&2
  echo "      build and the compile-only gate below will likely fail." >&2
fi

# Self-contained exe: statically link the CRT so the target machine needs no
# VC++ redistributable. Override with OPENAY_WIN_CRT_STATIC=0 if ever needed.
if [ "${OPENAY_WIN_CRT_STATIC:-1}" = 1 ]; then
  export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-Ctarget-feature=+crt-static"
fi

# The audio-output feature (cpal playback) is the Windows client's reason
# to exist — always built.
FEATURES="--features audio-output"

echo "== Windows cross-build (cargo xwin build --release) =="
XWIN_OK=1
if command -v cargo-xwin >/dev/null 2>&1; then
  (cd desktop && cargo xwin build --release --target "$TARGET" -p openay-server -p openay-gui $FEATURES >>"$LOG" 2>&1)
  XWIN_OK=$?
fi
check "cargo xwin build openay-server + openay-gui" "$XWIN_OK"

echo "== Dist staging =="
EXE="desktop/target/$TARGET/release/openay-server.exe"
GUI_EXE="desktop/target/$TARGET/release/openay-gui.exe"
EXE_OK=1
if [ "$XWIN_OK" -eq 0 ] && [ -f "$ROOT/$EXE" ]; then
  rm -rf "$DIST"
  mkdir -p "$DIST"
  cp -f "$ROOT/$EXE" "$DIST/"
  EXE_OK=0
else
  rm -f "$DIST/openay-server.exe"   # never leave a stale exe behind a failure
  mkdir -p "$DIST"   # still stage the docs below
fi
check "copy openay-server.exe -> dist/openay-windows/" "$EXE_OK"

# The GUI console (same Iced app as Linux; WASAPI output on Windows).
GUI_OK=1
if [ "$XWIN_OK" -eq 0 ] && [ -f "$ROOT/$GUI_EXE" ]; then
  cp -f "$ROOT/$GUI_EXE" "$DIST/"
  GUI_OK=0
fi
check "copy openay-gui.exe -> dist/openay-windows/" "$GUI_OK"

README_OK=1
if [ -f "$README_WIN" ]; then
  cp -f "$README_WIN" "$DIST/"
  README_OK=0
else
  echo "error: $README_WIN missing — cannot stage quick-start docs" >&2
fi
check "copy README-WINDOWS.txt -> dist/openay-windows/" "$README_OK"

# Session logger: wraps the exe so every test run is captured to a
# timestamped log directory (see the "Collecting logs" section of
# README-WINDOWS.txt). Only staged with the exe it wraps.
if [ "$EXE_OK" -eq 0 ]; then
  cp -f "$ROOT/scripts/openay-log.ps1" "$DIST/"
  check "copy openay-log.ps1 -> dist/openay-windows/" 0
fi

# --- Bundled adb (USB transport) -------------------------------------------
# dist/openay-windows/adb/ carries adb.exe (+ its two DLLs) so USB forwarding
# works without a separate Android SDK install. Android platform-tools are
# Apache-2.0 licensed; the license text and NOTICE are staged next to the
# binaries (see adb/LICENSE-2.0.txt / adb/NOTICE.txt). Best effort: prefers
# the local SDK ($ANDROID_HOME/platform-tools) and otherwise downloads the
# official zip; a missing bundle only warns (USB users can still point at
# their own adb install).
ADB_DIR="$DIST/adb"
ADB_FILES="adb.exe AdbWinApi.dll AdbWinUsbApi.dll"
ADB_OK=1

stage_adb_from() { # <source dir>
  local src="$1" f
  [ -d "$src" ] || return 1
  for f in $ADB_FILES; do
    [ -f "$src/$f" ] || return 1
  done
  mkdir -p "$ADB_DIR"
  for f in $ADB_FILES; do
    cp -f "$src/$f" "$ADB_DIR/$f"
  done
  return 0
}

if [ "$EXE_OK" -ne 0 ]; then
  echo "skip: adb bundle (no openay-server.exe staged)"
elif stage_adb_from "$ANDROID_HOME/platform-tools"; then
  echo "adb: staged from the local SDK ($ANDROID_HOME/platform-tools)"
elif stage_adb_from "$HOME/Android/Sdk/platform-tools"; then
  echo "adb: staged from the local SDK (~/Android/Sdk/platform-tools)"
else
  echo "adb: not in the local SDK — downloading platform-tools (windows)..."
  ADB_ZIP="$(mktemp -d)/platform-tools.zip"
  if curl -fsSL -o "$ADB_ZIP" "https://dl.google.com/android/repository/platform-tools-latest-windows.zip" \
     && python3 - "$ADB_ZIP" "$ADB_DIR" <<'PYZIP'
import sys, zipfile, os
zip_path, out_dir = sys.argv[1], sys.argv[2]
os.makedirs(out_dir, exist_ok=True)
wanted = {"adb.exe", "AdbWinApi.dll", "AdbWinUsbApi.dll"}
with zipfile.ZipFile(zip_path) as z:
    for name in z.namelist():
        base = os.path.basename(name)
        if base in wanted and name.startswith("platform-tools/"):
            with z.open(name) as src, open(os.path.join(out_dir, base), "wb") as dst:
                dst.write(src.read())
            wanted.discard(base)
sys.exit(0 if not wanted else 1)
PYZIP
  then
    echo "adb: downloaded and staged"
  else
    ADB_OK=0
    echo "warning: could not bundle adb (offline?) — USB users need their own platform-tools" >&2
  fi
fi

if [ "$ADB_OK" -eq 0 ] && [ -f "$ADB_DIR/adb.exe" ]; then
  # Apache-2.0 attribution for the redistributed platform-tools binaries.
  ADB_LIC="$ADB_DIR/LICENSE-2.0.txt"
  if [ ! -f "$ADB_LIC" ]; then
    curl -fsSL -o "$ADB_LIC" "https://www.apache.org/licenses/LICENSE-2.0.txt" \
      || echo "warning: could not download the Apache-2.0 text for the adb bundle" >&2
  fi
  cat > "$ADB_DIR/NOTICE.txt" <<'NOTICE_EOF'
This distribution bundles "platform-tools" (adb.exe, AdbWinApi.dll,
AdbWinUsbApi.dll) from the Android SDK, Copyright (C) The Android Open
Source Project.

Licensed under the Apache License, Version 2.0 (see LICENSE-2.0.txt):
  http://www.apache.org/licenses/LICENSE-2.0

Source code: https://android.googlesource.com/platform/packages/modules/adb
Unless required by applicable law or agreed to in writing, the software is
distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
KIND.
NOTICE_EOF
  check "bundle adb + attribution -> dist/openay-windows/adb/" 0
elif [ "$EXE_OK" -eq 0 ]; then
  check "bundle adb + attribution -> dist/openay-windows/adb/" "$ADB_OK"
fi

if [ "$XWIN_OK" -ne 0 ]; then
  echo "== Fallback: compile-only gate for $TARGET =="
  GATE_OK=1
  if command -v cargo-xwin >/dev/null 2>&1; then
    (cd desktop && cargo xwin check --target "$TARGET" -p openay-server $FEATURES >>"$LOG" 2>&1)
    GATE_OK=$?
  fi
  if [ "$GATE_OK" -ne 0 ]; then
    (cd desktop && cargo check --target "$TARGET" -p openay-server $FEATURES >>"$LOG" 2>&1)
    GATE_OK=$?
  fi
  check "windows-target check gate (no exe)" "$GATE_OK"
  echo "error: openay-server.exe could NOT be linked on this host" >&2
  echo "       (see $LOG; last lines below). The check gate result above" >&2
  echo "       shows whether the code compiles for the Windows target." >&2
  tail -15 "$LOG" >&2
fi

echo
echo "=========================================="
echo " WINDOWS BUILD: $PASS passed, $FAIL failed"
echo "=========================================="
if [ "$EXE_OK" -eq 0 ] && [ "$README_OK" -eq 0 ]; then
  echo " dist: $DIST"
fi
[ "$FAIL" -eq 0 ]
