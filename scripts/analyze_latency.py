#!/usr/bin/env python3
"""Software latency analyzer: ingest->present latency from an onset-marked
recording of the `openay_mic` virtual source.

Pairs with scripts/latency_probe.sh, which records the virtual source with
pw-cat while `openay_loopback tone-udp ... --onset-after N` streams N silent
10 ms frames followed by a sine. The sender knows the onset's audio-time
exactly (N * frame_ms after its stream start); the analyzer locates the
onset's position in the recording and converts the difference into latency:

    latency_ms = onset_sample / sr * 1000 - anchor_ms - N * frame_ms

`anchor_ms` is the wall-clock offset between the recording's sample 0 and the
sender's stream start, supplied by the probe script (it can only estimate
that instant from process launch timing — the estimate's error is part of the
probe's documented error bar; see latency_probe.sh). This tool is the pure,
exactly-testable part: detection plus math, verified against synthetic WAVs
with known ground truth via --self-test.

A span cross-check validates each run: the audible tone in the recording must
last (packets - onset_frame) * frame_ms within --span-tolerance-ms. A large
negative delta means packet loss or a recorder stall; such runs are flagged
(ok=no, exit 1) so the probe can exclude them.

Output (parseable, always one LATENCY line):
    LATENCY latency_ms=<F.2> onset_sample=<S> anchor_ms=<A.F1> \
span_expected_ms=<E.F1> span_measured_ms=<M.F1> span_delta_ms=<D.F1> ok=yes|no

Exit codes: 0 = detected and span ok; 1 = detected but span mismatch;
2 = no onset found (or unreadable WAV).
"""

from __future__ import annotations

import argparse
import array
import math
import sys
import wave

DEFAULT_SAMPLE_RATE = 48_000
DEFAULT_FRAME_MS = 10.0


def read_wav_mono(path: str) -> tuple[int, list[float]]:
    """Read a WAV file as mono float samples in [-1, 1). Returns (sr, samples).

    Multi-channel files are averaged; only 16-bit PCM is supported (what
    pw-cat writes with --format s16).
    """
    with wave.open(path, "rb") as wav:
        sr = wav.getframerate()
        channels = wav.getnchannels()
        if wav.getsampwidth() != 2:
            raise ValueError(f"expected 16-bit PCM, got sampwidth={wav.getsampwidth()}")
        frames = wav.readframes(wav.getnframes())
    raw = array.array("h")
    raw.frombytes(frames)
    if channels > 1:
        mono = [
            sum(raw[i : i + channels]) / (channels * 32_768.0)
            for i in range(0, len(raw), channels)
        ]
    else:
        mono = [s / 32_768.0 for s in raw]
    return sr, mono


def _rms_envelope(samples: list[float], sr: int) -> tuple[list[float], int]:
    """RMS envelope: 2 ms windows, 1 ms hop. Returns (rms, hop)."""
    hop = max(1, sr // 1000)
    win = 2 * hop
    rms: list[float] = []
    for start in range(0, len(samples) - win, hop):
        seg = samples[start : start + win]
        rms.append(math.sqrt(sum(x * x for x in seg) / win))
    return rms, hop


def _threshold(rms: list[float], factor: float, hard_cap: float) -> float:
    """Adaptive threshold: quiet-level percentile x factor, absolutely floored
    and capped so files with little/no silence still get a sane value."""
    ordered = sorted(rms)
    quiet = ordered[max(0, int(0.1 * len(ordered)) - 1)] if ordered else 0.0
    return min(max(quiet * factor, 1e-4), hard_cap)


def detect_onset(
    samples: list[float], sr: int, threshold_factor: float = 6.0
) -> int | None:
    """Return the sample index of the first audio onset, or None.

    Windowed-RMS detection: the first 1 ms hop whose 2 ms window leaves the
    adaptive noise threshold AND whose following 20 ms stay above it (rejects
    single glitches), refined down to the first sample that clearly exceeds
    the noise (sub-ms precision against the exact digital silence the
    software path produces before an onset).
    """
    if len(samples) < sr // 10:
        return None
    rms, hop = _rms_envelope(samples, sr)
    threshold = _threshold(rms, threshold_factor, hard_cap=0.25)
    sustain = 20  # hops == 20 ms

    coarse: int | None = None
    for g in range(len(rms) - sustain):
        if rms[g] > threshold and all(v > threshold for v in rms[g : g + sustain]):
            coarse = g
            break
    if coarse is None:
        return None

    # Refine to the first strong sample around the coarse window start.
    refine_threshold = _threshold(rms, threshold_factor * 0.66, hard_cap=0.1)
    start = max(0, coarse * hop - hop)
    for i in range(start, len(samples)):
        if abs(samples[i]) > refine_threshold:
            return i
    return coarse * hop


def detect_tone_end(
    samples: list[float], sr: int, onset: int, threshold_factor: float = 6.0
) -> int:
    """Return the last sample index of the sustained tone after `onset`.

    First run of >= 20 ms of window-RMS below the adaptive threshold ends the
    tone; the boundary is the start of that quiet run.
    """
    rms, hop = _rms_envelope(samples, sr)
    threshold = _threshold(rms, threshold_factor, hard_cap=0.25)
    sustain = 20  # hops == 20 ms

    g_onset = onset // hop
    for g in range(g_onset, len(rms) - sustain):
        if all(v <= threshold for v in rms[g : g + sustain]):
            return g * hop
    return len(samples) - 1


def compute_latency(
    onset_sample: int,
    anchor_ms: float,
    onset_frame: int,
    frame_ms: float,
    sr: int,
) -> float:
    """Ingest->present latency in ms (see module docstring)."""
    return onset_sample / sr * 1000.0 - anchor_ms - onset_frame * frame_ms


def analyze(
    wav: str,
    onset_frame: int,
    packets: int,
    anchor_ms: float,
    frame_ms: float = DEFAULT_FRAME_MS,
    span_tolerance_ms: float = 25.0,
) -> tuple[int, str]:
    """Analyze one recording. Returns (exit_code, LATENCY line)."""
    sr, samples = read_wav_mono(wav)
    onset = detect_onset(samples, sr)
    if onset is None:
        return 2, (
            f"LATENCY latency_ms=nan onset_sample=-1 anchor_ms={anchor_ms:.1f} "
            f"span_expected_ms=0.0 span_measured_ms=0.0 span_delta_ms=0.0 ok=no"
        )
    end = detect_tone_end(samples, sr, onset)
    latency = compute_latency(onset, anchor_ms, onset_frame, frame_ms, sr)
    span_expected = (packets - onset_frame) * frame_ms
    span_measured = (end - onset) / sr * 1000.0
    span_delta = span_measured - span_expected
    ok = abs(span_delta) <= span_tolerance_ms
    code = 0 if ok else 1
    line = (
        f"LATENCY latency_ms={latency:.2f} onset_sample={onset} "
        f"anchor_ms={anchor_ms:.1f} span_expected_ms={span_expected:.1f} "
        f"span_measured_ms={span_measured:.1f} span_delta_ms={span_delta:.1f} "
        f"ok={'yes' if ok else 'no'}"
    )
    return code, line


# ---------------------------------------------------------------------------
# Self-test: synthetic WAVs with exact ground truth.
# ---------------------------------------------------------------------------

def _synth(
    sr: int,
    anchor_samples: int,
    onset_frame: int,
    frame_ms: float,
    packets: int,
    latency_ms: float,
    freq: float = 440.0,
    level: float = 0.8,
    noise: float = 0.0,
    holes: list[tuple[int, int]] | None = None,
    tail_ms: float = 300.0,
) -> list[float]:
    """Build a synthetic recording: anchor silence, N silent frames, latency
    gap, then the tone ((P-N) frames), optional zeroed holes (sample ranges),
    and a silent tail. Returns float samples."""
    frame_samples = int(sr * frame_ms / 1000.0)
    lead = anchor_samples + onset_frame * frame_samples + int(sr * latency_ms / 1000.0)
    tone = (packets - onset_frame) * frame_samples
    total = lead + tone + int(sr * tail_ms / 1000.0)
    out = [0.0] * total
    for i in range(lead, lead + tone):
        t = (i - lead) / sr
        out[i] = level * math.sin(2.0 * math.pi * freq * t)
    if noise > 0.0:
        # Deterministic tiny dither in the leading silence.
        for i in range(lead):
            out[i] = noise * math.sin(2.0 * math.pi * 9_000.0 * i / sr)
    for start, length in holes or []:
        for i in range(start, min(start + length, total)):
            out[i] = 0.0
    return out


def _write_wav(path: str, samples: list[float], sr: int) -> None:
    pcm = array.array("h", (max(-32768, min(32767, int(s * 32767))) for s in samples))
    with wave.open(path, "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(sr)
        wav.writeframes(pcm.tobytes())


def self_test() -> int:
    import tempfile
    from pathlib import Path

    sr = DEFAULT_SAMPLE_RATE
    checks = 0
    failed = 0

    def check(name: str, cond: bool, detail: str = "") -> None:
        nonlocal checks, failed
        checks += 1
        if cond:
            print(f"SELFTEST ok {name}")
        else:
            failed += 1
            print(f"SELFTEST FAIL {name} {detail}")

    with tempfile.TemporaryDirectory() as tmp:
        # 1. Latency recovery across a grid of anchors / onsets / lengths.
        for latency_ms in (5.0, 20.0, 35.0, 60.0):
            for anchor_ms in (0.0, 700.0):
                for n, p in ((0, 300), (100, 450)):
                    samples = _synth(
                        sr,
                        anchor_samples=int(sr * anchor_ms / 1000.0),
                        onset_frame=n,
                        frame_ms=10.0,
                        packets=p,
                        latency_ms=latency_ms,
                    )
                    path = str(Path(tmp) / "grid.wav")
                    _write_wav(path, samples, sr)
                    code, line = analyze(
                        path, onset_frame=n, packets=p, anchor_ms=anchor_ms
                    )
                    got = float(line.split("latency_ms=")[1].split()[0])
                    check(
                        f"latency L={latency_ms} anchor={anchor_ms} N={n} P={p}",
                        code == 0 and abs(got - latency_ms) <= 0.1,
                        f"got {got:.2f} [{line}]",
                    )

        # 2. Small noise floor does not shift detection by more than 1 ms.
        samples = _synth(
            sr,
            anchor_samples=int(0.5 * sr),
            onset_frame=100,
            frame_ms=10.0,
            packets=400,
            latency_ms=30.0,
            noise=3e-4,
        )
        path = str(Path(tmp) / "noise.wav")
        _write_wav(path, samples, sr)
        code, line = analyze(path, onset_frame=100, packets=400, anchor_ms=500.0)
        got = float(line.split("latency_ms=")[1].split()[0])
        check("noise floor robustness", code == 0 and abs(got - 30.0) <= 1.0, line)

        # 3. A 40 ms hole mid-tone trips the span check (exit 1).
        lead = int(0.5 * sr) + 100 * 480 + int(sr * 0.030)
        hole_start = lead + int(2.0 * sr)
        samples = _synth(
            sr,
            anchor_samples=int(0.5 * sr),
            onset_frame=100,
            frame_ms=10.0,
            packets=400,
            latency_ms=30.0,
            holes=[(hole_start, int(0.040 * sr))],
        )
        path = str(Path(tmp) / "hole.wav")
        _write_wav(path, samples, sr)
        code, line = analyze(path, onset_frame=100, packets=400, anchor_ms=500.0)
        delta = float(line.split("span_delta_ms=")[1].split()[0])
        check("40 ms hole flags span", code == 1 and delta <= -35.0, line)

        # 4. A 10 ms hole stays inside tolerance (exit 0).
        hole_start = lead + int(1.0 * sr)
        samples = _synth(
            sr,
            anchor_samples=int(0.5 * sr),
            onset_frame=100,
            frame_ms=10.0,
            packets=400,
            latency_ms=30.0,
            holes=[(hole_start, int(0.010 * sr))],
        )
        path = str(Path(tmp) / "small_hole.wav")
        _write_wav(path, samples, sr)
        code, line = analyze(path, onset_frame=100, packets=400, anchor_ms=500.0)
        check("10 ms hole within tolerance", code == 0, line)

        # 5. No onset at all -> exit 2.
        samples = [0.0] * sr
        path = str(Path(tmp) / "silent.wav")
        _write_wav(path, samples, sr)
        code, line = analyze(path, onset_frame=0, packets=10, anchor_ms=0.0)
        check("silence exits 2", code == 2, line)

    print(f"SELFTEST {'PASSED' if failed == 0 else 'FAILED'}: "
          f"{checks - failed}/{checks} checks")
    return 0 if failed == 0 else 1


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wav", help="recording to analyze (16-bit PCM WAV)")
    parser.add_argument("--onset-frame", type=int, default=0,
                        help="N: silent 10 ms frames streamed before the tone")
    parser.add_argument("--packets", type=int, default=0,
                        help="P: total packets the sender streamed")
    parser.add_argument("--anchor-ms", type=float, default=0.0,
                        help="wall-clock offset recording-sample-0 -> sender "
                             "stream start (from latency_probe.sh)")
    parser.add_argument("--frame-ms", type=float, default=DEFAULT_FRAME_MS)
    parser.add_argument("--span-tolerance-ms", type=float, default=25.0)
    parser.add_argument("--self-test", action="store_true",
                        help="run the synthetic ground-truth suite")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.self_test:
        return self_test()
    if not args.wav:
        print("error: --wav is required (or use --self-test)", file=sys.stderr)
        return 2
    try:
        code, line = analyze(
            args.wav,
            onset_frame=args.onset_frame,
            packets=args.packets,
            anchor_ms=args.anchor_ms,
            frame_ms=args.frame_ms,
            span_tolerance_ms=args.span_tolerance_ms,
        )
    except (OSError, ValueError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    print(line)
    return code


if __name__ == "__main__":
    sys.exit(main())
