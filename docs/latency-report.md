# OpenAY Mic — Latency audit report

Fill-in template. Every value here is *measured during an audit session* using
the tooling cross-referenced below; nothing in this document should be
invented. Run the verification of the tools first:
`scripts/gen_click_track.py --self-test`,
`scripts/analyze_clicks.py --self-test`, and
`bash -n scripts/cpu_profile.sh`.

## 1. Environment

| Field | Value |
|---|---|
| Date / time of session | `__________` |
| Operator | `__________` |
| Desktop machine | `__________` (CPU, RAM, OS, kernel) |
| PipeWire version | `__________` (`pw-cli info 0`) |
| Desktop commit | `__________` (`git rev-parse HEAD` in the repo) |
| Phone model | `__________` |
| Phone OS / build | `__________` |
| Phone app commit | `__________` |
| App settings (codec, frame ms, jitter target/capacity ms) | `__________` |
| Speaker / amplifier | `__________` |
| Speaker-to-phone distance D (m) | `__________` (acoustic correction `D / 0.343` ms) |
| Network (Wi-Fi AP / USB cable) | `__________` |
| Phantom? No | — |

## 2. Software probe (scripts/latency_probe.sh)

Automated ingest→present numbers without hardware (network + jitter buffer
only). Target: p99 < 5 ms.

| Transport | Codec | p50 (ms) | p95 (ms) | p99 (ms) | max (ms) | lost | dup | ooo | Verdict |
|---|---|---|---|---|---|---|---|---|---|
| udp | pcm | `__` | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| udp | opus | `__` | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| tcp | pcm | `__` | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| tcp | opus | `__` | `__` | `__` | `__` | `__` | `__` | `__` | `__` |

Command used: `__________`

## 3. Hardware audit — USB (TCP over adb reverse, target < 20 ms)

Per-run on a 30 s / 15-click track (`gen_click_track.py` defaults; first click
t = 1.0 s, spacing 2.0 s). Acoustic correction, if applied:
`________ ms` (`D = ____ m`).

| Run | Analyzer summary line (CLICKS ...) | Mean latency ms | Min ms | Max ms | Stddev ms | Missing clicks | xruns/interruptions |
|---|---|---|---|---|---|---|---|
| 1 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| 2 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| 3 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| 4 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| 5 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| 6 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| 7 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| 8 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| 9 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| 10 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| (extra runs if taken) | `__` | `__` | `__` | `__` | `__` | `__` | `__` |

Summary (p50/p95 over the per-run mean latencies): p50 = `__` ms,
p95 = `__` ms, worst = `__` ms. **Target < 20 ms: PASS / FAIL** (`__`).

xrun source side (server stats line / phone stats / jitter buffer): `______`

## 4. Hardware audit — Wi-Fi (UDP, target < 40 ms)

| Run | Analyzer summary line (CLICKS ...) | Mean latency ms | Min ms | Max ms | Stddev ms | Missing clicks | xruns/interruptions |
|---|---|---|---|---|---|---|---|
| 1 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| 2 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| 3 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| 4 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| 5 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| 6 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| 7 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| 8 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| 9 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| 10 | `__` | `__` | `__` | `__` | `__` | `__` | `__` |
| (extra runs if taken) | `__` | `__` | `__` | `__` | `__` | `__` | `__` |

Summary (p50/p95 over the per-run mean latencies): p50 = `__` ms,
p95 = `__` ms, worst = `__` ms. **Target < 40 ms: PASS / FAIL** (`__`).

xrun source side: `______`

## 5. CPU budget (scripts/cpu_profile.sh)

Budgets from the Phase 6 plan on the **server** rows: idle < 1.00 %CPU,
Opus < 3.00 %CPU (+0.25 tolerance band). GUI rows are informative only.

| Scenario | Process | cpu_pct | rss_kib | samples | Budget | Verdict |
|---|---|---|---|---|---|---|
| idle | server | `__` | `__` | `__` | < 1.00 % | `__` |
| pcm | server | `__` | `__` | `__` | observe | `__` |
| opus | server | `__` | `__` | `__` | < 3.00 % | `__` |
| idle | gui | `__` | `__` | `__` | informative | `__` |
| pcm | gui | `__` | `__` | `__` | informative | `__` |
| opus | gui | `__` | `__` | `__` | informative | `__` |

Command used: `scripts/cpu_profile.sh --assert [--with-gui]`
Sender used (native `openay_loopback tone-udp` or degraded
`openay-loopback send-udp` fallback): `______`

## 6. Findings & conclusions

- **Targets:** USB `____` (PASS/FAIL), Wi-Fi `____` (PASS/FAIL),
  CPU idle/opus `____` (PASS/FAIL).
- **Stability:** worst run-to-run stddev = `__ ms` (per path). Onset interval
  stddev = `__ ms`. Drift (interval mean − 2.0 s fixed spacing) = `__ ms/s`.
- **Dropouts:** total missing clicks = `__` (USB) / `__` (Wi-Fi), correlated
  with xruns at `______`.
- **Compare paths:** USB vs Wi-Fi delta = `__ ms`; software probe vs hardware
  delta = `__ ms` (acoustic + phone chain), consistent with the 1–3 ms +
  `D/0.343` ms calibration expectation (Y/N `__`).
- **Known error bars on absolute numbers:** amp + acoustic + phone buffer;
  run-to-run stddev is the trustworthy figure.
- **Issues found and fixes:** `______`
- **Next actions:** `______`

---

*Generated by the Phase 6 latency-audit kit (scripts/gen_click_track.py,
scripts/analyze_clicks.py, scripts/cpu_profile.sh, docs/latency-audit.md).*
