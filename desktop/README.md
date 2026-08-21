# OpenAY Mic — Desktop (Rust)

The desktop half of the OpenAY Mic virtual-microphone system. An Android
phone captures the microphone and streams audio to this Linux desktop over
UDP (Wi-Fi), TCP (USB via `adb forward`), or Bluetooth RFCOMM. This workspace
implements the wire protocol, transports, Opus codec, the loopback
send/receive/bench tooling that the interop tests are built on, and the
`openay-server` desktop receiver that exposes a PipeWire virtual microphone.

Wire format, sequencing rules, and test filler are defined in
`shared/protocol.md` (canonical); golden vectors live in
`shared/test-vectors.json` and are exercised by `openay-protocol`'s tests.

## Prerequisites

Source the project's environment script before any cargo command:

```bash
source scripts/env.sh
```

This puts Rust (`~/.cargo/bin`) and the conda env (which provides
`pkg-config`, `cmake`, and **libopus 1.6.1**) on `PATH`/`PKG_CONFIG_PATH`.

> **libopus runtime linkage:** `.cargo/config.toml` in this directory embeds
> a RUNPATH to the conda env's `lib/` so every binary/test finds the libopus
> it was built against. Without it, the loader falls back to the system
> libopus (1.4 on this machine), which fails the codec quality test
> measurably (RMS error ~4x higher). `--enable-new-dtags` keeps
> system-first resolution for every other library; `LD_LIBRARY_PATH` still
> wins if set.

## Workspace layout

| Crate                     | Purpose                                                                        |
|---------------------------|--------------------------------------------------------------------------------|
| `crates/openay-protocol`  | Packet encode/decode, 6-byte big-endian header, `DecodeError` taxonomy        |
| `crates/openay-transport` | UDP one-datagram-per-packet, TCP byte-stream framing with 64 KiB bad-magic resync, optional RFCOMM server (`bluetooth` feature), `SeqTracker`, `PacketStats`, `fill_xorshift` |
| `crates/openay-codec`     | libopus wrapper: 48 kHz mono, 10 ms frames, restricted lowdelay, 32 kbps default |
| `crates/openay-jitter`    | Lock-free SPSC `f32` jitter buffer (pure std, zero dependencies), prebuffer/overrun/underrun accounting |
| `crates/openay-loopback`  | `openay-loopback` CLI: send/recv over UDP+TCP with full verification, one-way latency bench |
| `crates/openay-server`    | Engine library + `openay-server` CLI: desktop receiver (UDP/TCP -> jitter buffer -> PipeWire virtual mic, `pipewire` feature). The engine API (`spawn_engine`/`EngineHandle`) is shared with the GUI |
| `crates/openay-gui`       | `openay-gui` console: tray-resident Iced GUI (design.md "Studio rack at night") over the engine — The Chain hero card, VU ladder, ON AIR toggle, settings slide-over |

## Build & test

```bash
cd desktop
source ../scripts/env.sh

cargo build --workspace --release      # zero warnings/errors
cargo test --workspace                 # all tests pass

# Bluetooth RFCOMM server (compile-only without BT hardware):
cargo build --features openay-transport/bluetooth --workspace
# The hardware test is #[ignore]d; run it explicitly to probe the adapter:
cargo test -p openay-transport --features bluetooth --test transports -- --ignored
```

## openay-server CLI

The desktop receiver (Phase 4) is now a library with a thin CLI wrapper.
The engine API (`spawn_engine`, `EngineHandle`, `EngineConfig`) is shared
with the GUI so both can drive the same receive pipeline.

The CLI receives OpenAY audio packets, decodes them into `f32`, feeds a
lock-free jitter buffer, and — with the `pipewire` feature — exposes the
audio as a PipeWire virtual microphone source node named `openay_mic`
(`media.class = Audio/Source/Virtual`, F32LE mono 48 kHz).

```
openay-server [--transport udp|tcp] [--port N] [--bind ADDR]
              [--codec pcm|opus|auto] [--target-ms F] [--capacity-ms F]
```

- `--transport` default `udp`, `--port` default `41700`, `--bind` default
  `0.0.0.0`.
- `--codec` default `auto` (accept either PCM or Opus per packet); `pcm` /
  `opus` reject the other type as malformed.
- `--target-ms` default `10.0`, clamped to `[MIN_PREBUFFER_MS, MAX_PREBUFFER_MS]`
  (5–20 ms); the virtual mic starts emitting real samples once the jitter
  buffer holds `ceil(target-ms * 48)` samples, and re-prebuffers after every
  underrun.
- `--capacity-ms` default `100.0` — jitter buffer capacity in ms of audio.

The receive loop runs standalone; without the `pipewire` feature it prints
`built without PipeWire support — network+jitter only` and still receives,
decodes, and buffers (useful for testing the pipeline headlessly). Stats are
printed to stdout every 5 s and once at shutdown:

```
SRV transport=udp received=<n> lost=<n> dup=<d> ooo=<o> malformed=<m> overruns=<r> underruns=<u> fill_ms=<F.1>
```

### PipeWire virtual microphone

```bash
# Requires libpipewire 0.3 + libspa 0.2 (pkg-config; see scripts/env.sh):
cargo build --release -p openay-server --features pipewire
target/release/openay-server --port 43210          # SIGINT to stop
wpctl status | grep -i openay                      # should show the node
```

The process callback runs on PipeWire's RT data thread and only touches the
lock-free jitter buffer and atomics (no locks, allocation, or logging).

### Engine API and level metering

`openay_server` exposes the pipeline as a start/stop-able engine:

```rust
let handle = openay_server::spawn_engine(None);          // dedicated thread + runtime
handle.cmd().send(EngineCommand::Start(config)).await?;  // or pass Some(config) to spawn
let status: EngineStatus = handle.status();              // snapshot (see below)
handle.cmd().send(EngineCommand::Stop).await?;
```

`EngineStatus::level_peak` is the peak capture level (`0.0..=1.0`) over the
interval since the previous snapshot — each `status()` call consumes the
interval (same semantics as the Android side). The RT process callback in
`pw.rs` folds `max |sample|` into an `AtomicU32` scaled to 0..=65535 with a
strict-max CAS loop; no locks/alloc/log in the callback. **Without the
`pipewire` feature nothing consumes the jitter buffer, so `level_peak`
stays `0.0`** — asserted by the headless smoke test
(`crates/openay-server/tests/engine_smoke.rs`), which drives the engine over
UDP with `UdpSender` and checks counts/loss/fill, then verifies the engine
survives a Stop -> Start cycle on the same handle.

### Headless smoke test (non-GUI logic)

```bash
cargo test -p openay-server --test engine_smoke
```

Constructs an `EngineHandle`, starts the UDP engine on a free port, feeds
100 packets via `openay_transport::UdpSender`, and asserts the status
counters (`received == 100`, `lost == 0`, overruns counted, buffer full)
plus `level_peak == 0.0` in this network-only build.

## openay-gui console (Phase 5)

The desktop console: a tray-resident Iced window implementing the
`shared/design.md` "Studio rack at night" contract faithfully — ink/panel/
cream/amber/tally palette, Chakra Petch + IBM Plex Mono (embedded from
`shared/fonts/`), The Chain hero card (MIC level ring / LINK pps+loss /
CONSOLE jitter target), a 24-segment VU ladder (18 cream / 3 amber / 3 red,
~12 dB/s decay ballistics), the circular ON AIR/STANDBY toggle with a
~400 ms power-on stagger, and a settings slide-over (port, bind-address
dropdown from local interfaces, codec chips, 5–20 ms jitter slider,
autostart / start-minimized / reduced-motion switches).

```bash
cargo build --release -p openay-gui --features openay-server/pipewire
target/release/openay-gui                 # window; close-requested hides to tray
target/release/openay-gui --minimized     # tray-only until Show Console
```

- **Config**: `~/.config/openay-mic/config.toml` (serde/toml; missing or
  partial files load defaults). XDG autostart entry
  `~/.config/autostart/openay-mic.desktop` is written/removed by the
  AUTOSTART switch (Exec points at the running binary with `--minimized`).
- **Tray** (ksni StatusNotifierItem): Show Console / Start / Stop (checkmark
  state) / Quit; the 24x24 pixmap reflects the state (gray idle, amber
  armed, red live) — generated once by `crates/openay-gui/tools/gen_icons.py`
  into `src/icons.rs` (ARGB32 converted at runtime for the freedesktop spec).
- **Robustness**: the engine handle is created once; settings changes stop,
  mutate, and restart the pipeline on the same handle. Quitting the window
  (close button) hides to the tray; real exit is via tray Quit. Second
  launch behavior is intentionally undefined.
- **Reduced motion**: a config flag drops the cable pulse and the power-on
  stagger (iced has no built-in API for it in 0.13).

## openay-loopback CLI

```
openay-loopback send-udp <host> <port> <count> [payload_size=480] [interval_us=0]
openay-loopback recv-udp <port> <count> [payload_size=480]
openay-loopback send-tcp <host> <port> <count> [payload_size=480]
openay-loopback recv-tcp <port> <count> [payload_size=480]
openay-loopback bench <udp|tcp> <port> <count> [payload_size=480]
```

Senders alternate Pcm/Opus payloads starting with Pcm; sequence numbers run
from 0 mod 2^16; payloads are `fill_xorshift(seed=seq)`. Receivers verify
type alternation, sequence contiguity from 0, payload length, and filler
content, then print the canonical stats line:

```
RECV ok=<received> lost=<lost> dup=<duplicate> ooo=<out_of_order> malformed=<malformed> content_errors=<content_errors>
```

Senders print `SENT count=<n> bytes=<total_wire_bytes>`. `bench` embeds an
8-byte little-endian monotonic-ns timestamp at the start of each payload,
measures one-way loopback latency, and prints:

```
BENCH transport=<udp|tcp> count=<n> p50_us=<..> p95_us=<..> p99_us=<..> max_us=<..>
```

### Example: cross-process UDP round trip

```bash
target/release/openay-loopback recv-udp 42001 5000 &   # terminal 1
target/release/openay-loopback send-udp 127.0.0.1 42001 5000 480 1000   # terminal 2
```

### Bench results (this machine, release build)

```
BENCH transport=udp count=20000 p50_us=6 p95_us=7 p99_us=12 max_us=60
BENCH transport=tcp count=20000 p50_us=12 p95_us=21 p99_us=35 max_us=215
```

Both exit 0 (gate: `p99_us < 5000`). TCP paths set `TCP_NODELAY` — without
it, Nagle/delayed-ACK batching raises TCP p99 to ~400 us.

## Bluetooth

`openay-transport`'s non-default `bluetooth` feature registers an SPP-style
profile via `bluer` (needs a BlueZ D-Bus daemon at runtime). Accepted
connections are plain byte streams (`AsyncRead + AsyncWrite`) and are framed
exactly like TCP via `TcpPacketStream`. Hardware testing requires a peer
device (e.g. the Android app paired over BT); the `rfcomm_adapter_presence`
test is `#[ignore]`d and only probes for an adapter, printing `SKIP` when
none is available.

## Deviations from the original task spec

- **openay-server UDP path**: the transport crate's `UdpReceiver` hardcodes
  binding to `127.0.0.1`, but the server CLI must honor `--bind` (default
  `0.0.0.0`). The server therefore binds a `tokio::net::UdpSocket` to the
  configured address itself and reuses the shared building blocks —
  `openay_protocol::decode` plus `openay-transport`'s `SeqTracker` (via the
  `Ingest` pipeline) — with malformed datagrams counted the same way.
- **`Ingest` decoder field**: the spec sketch names it `Option<OpusDecoder>`;
  the server uses `openay-codec`'s `OpusCodec` wrapper (encoder half unused)
  so the decode path is the exact codec crate the workspace tests validate.
- **PipeWire shutdown**: pipewire-rs 0.8's `MainLoop` is `!Send`, so the
  PipeWire thread drives the loop manually (`loop_.iterate()` with a 50 ms
  poll timeout) and checks a shared `quit` atomic instead of calling
  `main_loop.quit()` cross-thread.
- **Test-vector path**: the spec's `../../shared/test-vectors.json` resolves
  to `desktop/shared` from the crate dir; the vectors actually live at the
  repo root, so `openay-protocol` uses `../../../shared/test-vectors.json`
  (the spec's own "load ../../shared" intent is preserved — it loads the
  canonical vectors).
- **tokio features**: `io-util` was added to the transport/loopback tokio
  features — `AsyncReadExt`/`AsyncWriteExt` (`read_exact`, `write_all`) are
  required by the TCP framing API and live behind it.
- **audiopus version**: there is no stable `audiopus 0.3`; the 0.3 line is
  only published as `0.3.0-rc.0`, so the codec pins it explicitly.
- **Codec test window**: the codec's 2.5 ms lookahead (120 samples @48 kHz)
  plus a decoder startup transient makes a naive per-sample comparison
  impossible (the first output samples have no corresponding input). The
  test measures the physical delay on the steady-state region (excluding the
  first decoded frame) and asserts max abs error < 1500 and RMS < 2% FS on
  the aligned steady-state samples. Measured: max 1436, RMS 255 (0.78% FS).
- **UDP receive buffer**: `UdpReceiver` additionally exposes
  `set_recv_buffer_size` (via `socket2`; tokio's `UdpSocket` lacks it), and
  tracks sequencing stats (lost/dup/ooo) alongside malformed counts. The
  recv-* commands and benches request a 4 MB SO_RCVBUF (kernel-capped) so
  flat-out senders self-pace instead of overflowing the queue.
- **Bench clock**: `std::time::Instant` (Linux `CLOCK_MONOTONIC`) is sampled
  once per process as a shared reference; the sender embeds
  `t0.elapsed().as_nanos()`, the receiver computes the delta against its own
  `t0.elapsed()`. Valid in-process, and cross-process on Linux since both
  processes share the same monotonic clock.
