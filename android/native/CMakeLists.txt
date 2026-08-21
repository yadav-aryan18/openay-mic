# OpenAY Mic — native core (C++17, POSIX sockets; Linux host + Android NDK).
cmake_minimum_required(VERSION 3.16)
project(openay_native CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

find_package(Threads REQUIRED)

# Warning-clean is a hard requirement.
add_compile_options(-Wall -Wextra)
# Strict -std=c++17 hides POSIX declarations on glibc/bionic; re-enable the
# POSIX.1-2008 + XSI surface used by the transports.
add_compile_definitions(_DEFAULT_SOURCE)

set(OPENAY_ANDROID OFF)
if(CMAKE_SYSTEM_NAME STREQUAL "Android")
  set(OPENAY_ANDROID ON)
endif()

# --- Opus resolution (must precede target definitions) ----------------------
# Host: located via pkg-config (conda env under scripts/env.sh). When absent
# the core still builds; OPENAY_HAVE_OPUS controls what gets compiled.
# Android: pkg-config must NEVER run — scripts/env.sh exports PKG_CONFIG_PATH
# to the host conda env, which would leak an x86_64 libopus into the NDK
# build. There the codec comes from a vendored static opus built from source,
# feeding openay_core directly so capture_pipeline's Opus path stays live.
if(OPENAY_ANDROID)
  # Prefab provides the oboe package to Gradle externalNativeBuild.
  find_package(oboe REQUIRED CONFIG)

  include(FetchContent)
  set(OPUS_BUILD_SHARED_LIBRARY OFF CACHE BOOL "" FORCE)
  set(OPUS_BUILD_PROGRAMS OFF CACHE BOOL "" FORCE)
  set(OPUS_BUILD_TESTING OFF CACHE BOOL "" FORCE)
  set(OPUS_INSTALL_PKG_CONFIG_MODULE OFF CACHE BOOL "" FORCE)
  set(OPUS_INSTALL_CMAKE_PACKAGE OFF CACHE BOOL "" FORCE)
  set(OPUS_DISABLE_DOCS ON CACHE BOOL "" FORCE)
  # NOTE: no DOWNLOAD_EXTRACT_TIMESTAMP here — AGP pins CMake 3.22, where the
  # option does not exist and FetchContent forwards it into ExternalProject's
  # URL list, breaking the download.
  # TODO: pin URL_HASH <sha256> for supply-chain hygiene once a version is
  # frozen (compute from the tarball at release time).
  # 2026-08-21: downloads.opuscodec.org no longer resolves (curl: Could not
  # resolve host); the canonical xiph mirror on GitHub is used instead.
  FetchContent_Declare(opus
    URL https://github.com/xiph/opus/archive/refs/tags/v1.5.2.tar.gz
  )
  FetchContent_MakeAvailable(opus)

  set(OPENAY_HAVE_OPUS ON)
  set(OPENAY_OPUS_TARGET opus)
else()
  find_package(PkgConfig QUIET)
  set(OPENAY_HAVE_OPUS OFF)
  if(PkgConfig_FOUND)
    pkg_check_modules(OPUS QUIET IMPORTED_TARGET opus)
    if(OPUS_FOUND)
      set(OPENAY_HAVE_OPUS ON)
      set(OPENAY_OPUS_TARGET PkgConfig::OPUS)
    endif()
  endif()
endif()
message(STATUS "openay_native: OPENAY_HAVE_OPUS=${OPENAY_HAVE_OPUS}")

# --- Core library ----------------------------------------------------------
# Phase 3 additions: ring_buffer (lock-free SPSC), capture_pipeline (RT
# callback -> ring -> network thread), test_source (host sine simulator),
# capture_engine (portable facade; TestSource on host, OboeSource on Android).
set(OPENAY_CORE_SOURCES
  src/protocol.cpp
  src/udp_transport.cpp
  src/tcp_transport.cpp
  src/ring_buffer.cpp
  src/capture_pipeline.cpp
  src/test_source.cpp
  src/capture_engine.cpp
)
if(OPENAY_HAVE_OPUS)
  list(APPEND OPENAY_CORE_SOURCES src/opus_codec.cpp)
endif()

add_library(openay_core STATIC ${OPENAY_CORE_SOURCES})
target_include_directories(openay_core PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/include)
target_link_libraries(openay_core PUBLIC Threads::Threads)
if(OPENAY_HAVE_OPUS)
  target_link_libraries(openay_core PUBLIC ${OPENAY_OPUS_TARGET})
  # capture_pipeline.cpp and the capture test compile the Opus code path only
  # when the codec is actually built in.
  target_compile_definitions(openay_core PUBLIC OPENAY_HAVE_OPUS=1)
  if(OPENAY_ANDROID)
    target_include_directories(openay_core PUBLIC ${opus_SOURCE_DIR}/include)
  else()
    # Some packaging layouts (e.g. conda-forge opus 1.6.1) install the headers
    # into <includedir>/opus/ while pkg-config only reports -I<includedir>;
    # add the subdir so `#include <opus.h>` resolves in both layouts.
    if(OPUS_INCLUDEDIR)
      target_include_directories(openay_core PUBLIC ${OPUS_INCLUDEDIR}/opus)
    endif()
  endif()
endif()

# --- Tool ------------------------------------------------------------------
add_executable(openay_loopback tools/openay_loopback.cpp)
target_link_libraries(openay_loopback PRIVATE openay_core)

# --- Tests (ctest) ---------------------------------------------------------
option(OPENAY_BUILD_TESTS "Build the OpenAY native test suite" ON)
if(OPENAY_ANDROID)
  # The ctest suite is a host facility; under the NDK build its executables
  # would need to link the app-layer OboeSource (capture_engine.cpp calls
  # MakeSource() -> OboeSource, defined in the openaymic SHARED target).
  set(OPENAY_BUILD_TESTS OFF)
endif()
if(OPENAY_BUILD_TESTS)
  enable_testing()

  add_executable(test_protocol tests/test_protocol.cpp)
  target_link_libraries(test_protocol PRIVATE openay_core)
  add_test(NAME protocol COMMAND test_protocol)

  add_executable(test_transport tests/test_transport.cpp)
  target_link_libraries(test_transport PRIVATE openay_core)
  add_test(NAME transport COMMAND test_transport)

  if(OPENAY_HAVE_OPUS)
    add_executable(test_opus tests/test_opus.cpp)
    target_link_libraries(test_opus PRIVATE openay_core)
    add_test(NAME opus COMMAND test_opus)
  endif()

  # Phase 3 tests.
  add_executable(test_ring tests/test_ring.cpp)
  target_link_libraries(test_ring PRIVATE openay_core)
  add_test(NAME ring COMMAND test_ring)

  add_executable(test_capture tests/test_capture.cpp)
  target_link_libraries(test_capture PRIVATE openay_core)
  if(OPENAY_HAVE_OPUS)
    target_compile_definitions(test_capture PRIVATE OPENAY_HAVE_OPUS=1)
  endif()
  add_test(NAME capture COMMAND test_capture)
endif()

# --- Android (NDK) library: JNI bridge + Oboe source -----------------------
# Only evaluated when Gradle cross-compiles via the NDK toolchain. The host
# build never enters this branch. openaymic is the SHARED library the app
# loads (System.loadLibrary("openaymic")); the JNI entry points, Oboe input
# source, codec, transports and pipeline all flow through openay_core (which
# carries the vendored opus PUBLIC above).
if(OPENAY_ANDROID)
  # openay_core's capture_engine.cpp includes openay/oboe_source.h, which
  # includes <oboe/Oboe.h>: the core itself must see the oboe prefab package
  # (not just openaymic). PUBLIC so every TU that pulls in oboe_source.h — the
  # core, openaymic, and the NDK-side test executables — inherits the include
  # dir and links oboe at the final link.
  target_link_libraries(openay_core PUBLIC oboe::oboe)
  add_library(openaymic SHARED
    jni/capture_jni.cpp
    src/android/oboe_source.cpp
  )
  target_link_libraries(openaymic PRIVATE openay_core oboe::oboe android log)
endif()
