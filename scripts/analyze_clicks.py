#!/usr/bin/env python3
"""OpenAY Mic click-onset analyzer (hardware latency audit).

Scans a WAV recording of the `openay_mic` PipeWire source (taken while the
click track from scripts/gen_click_track.py played over speakers next to the
phone) and detects acoustic click onsets so the glass-to-glass latency and
dropout behavior of the phone->desktop path can be quantified.

Detection: a trailing-window RMS envelope (default 5 ms window, 2 ms hop) is
computed with an exact running-sum; envelope bins above an adaptive threshold
(max(median * factor, peak * floor_rel, 1.0)) are grouped into click regions;
peaks closer than half the expected click spacing are merged (stronger wins);
each onset is refined to the rising-edge crossing of `refine_frac` of the
region peak with linear interpolation between envelope bins. The trailing
window means a measured onset is biased ~1-2 ms late relative to the true
arrival sample; the bias is constant, so run-to-run timing *differences* and
the standard deviation of the inter-onset intervals are the meaningful
stability metrics (see docs/latency-audit.md, "Calibration & error bars").

Missing clicks (dropouts) are tolerated: the summary prints expected vs
detected counts and the CLI exits 1 only when NOTHING was detected.

Exit code: 0 = onsets detected (even if some are missing); 1 = no onsets,
bad WAV, or a failed --self-test.
"""

from __future__ import annotations

import argparse
import array
import math
import statistics
import sys
import wave
from pathlib import Path

# Defaults — documented in --help.
DEFAULT_SPACING_S: float = 2.0
DEFAULT_WINDOW_MS: float = 5.0
DEFAULT_HOP_MS: float = 2.0
DEFAULT_THRESHOLD: float = 4.0  # relative to the median envelope
DEFAULT_FLOOR_REL: float = 0.001  # absolute floor relative to the envelope peak
DEFAULT_MIN_SEP_FRAC: float = 0.5  # x expected spacing
DEFAULT_REFINE_FRAC: float = 0.30  # of the local envelope peak
MERGE_GAP_BINS: int = 2  # bins of near-silence allowed inside one click region


class Onset:
    """One detected click onset."""

    __slots__ = ("time_s", "bin", "peak")

    def __init__(self, time_s: float, bin_idx: int, peak: float) -> None:
        self.time_s = time_s
        self.bin = bin_idx
        self.peak = peak


def rms_envelope(
    samples: array.array, rate: int, window_ms: float, hop_ms: float
) -> list[float]:
    """Trailing-window RMS envelope.

    Envelope bin i is the RMS of the samples ending at sample index i*hop
    (window length `window_ms`); the window never looks ahead, so a rising
    edge location is not shifted earlier than the audio itself.
    """
    win = max(1, int(round(window_ms * rate / 1000.0)))
    hop = max(1, int(round(hop_ms * rate / 1000.0)))
    n = len(samples)
    # Exact running sum of squares (Python int: no float rounding drift).
    csum = [0] * (n + 1)
    total = 0
    for i, v in enumerate(samples):
        total += v * v
        csum[i + 1] = total
    env: list[float] = []
    i = hop
    while i <= n:
        a = i - win
        if a < 0:
            a = 0
        seg = csum[i] - csum[a]
        env.append(math.sqrt(seg / (i - a)))
        i += hop
    return env


def detect_onsets(
    samples: array.array,
    rate: int,
    spacing_s: float,
    threshold: float = DEFAULT_THRESHOLD,
    window_ms: float = DEFAULT_WINDOW_MS,
    hop_ms: float = DEFAULT_HOP_MS,
    floor_rel: float = DEFAULT_FLOOR_REL,
    min_sep_frac: float = DEFAULT_MIN_SEP_FRAC,
    refine_frac: float = DEFAULT_REFINE_FRAC,
) -> list[Onset]:
    """Detect click onsets in raw samples; returns onsets sorted by time."""
    hop = max(1, int(round(hop_ms * rate / 1000.0)))
    env = rms_envelope(samples, rate, window_ms, hop_ms)
    if not env:
        return []

    med = statistics.median(env)
    peak_env = max(env)
    level = max(med * threshold, peak_env * floor_rel, 1.0)

    # 1) Runs of bins above the threshold = click regions.
    regions: list[list[int]] = []
    run: list[int] = []
    gap = 0
    for i, e in enumerate(env):
        if e > level:
            run.append(i)
            gap = 0
        else:
            gap += 1
            if run and gap > MERGE_GAP_BINS:
                regions.append(list(run))
                run = []
    if run:
        regions.append(list(run))

    # 2) Enforce a minimum separation (≈ half the expected spacing);
    #    when two candidates collide keep the stronger peak.
    min_sep_bins = int(min_sep_frac * spacing_s * rate / hop)
    candidates: list[Onset] = []
    for region in regions:
        peak_bin = max(region, key=lambda b: env[b])
        peak = env[peak_bin]
        level_hi = peak * refine_frac
        b = peak_bin
        while b - 1 >= 0 and env[b - 1] >= level_hi:
            b -= 1
        # Rising-edge crossing between bins b-1 and b, linearly interpolated.
        if b == 0:
            frac = 0.0
        else:
            lo, hi = env[b - 1], env[b]
            frac = 0.5 if hi == lo else (level_hi - lo) / (hi - lo)
            frac = min(1.0, max(0.0, frac))
        t = (max(0, b - 1) + frac) * hop / rate
        candidates.append(Onset(t, b, peak))

    candidates.sort(key=lambda o: o.time_s)
    accepted: list[Onset] = []
    for cand in candidates:
        merged = False
        for i, acc in enumerate(accepted):
            if abs(cand.time_s - acc.time_s) * rate / hop < min_sep_bins:
                if cand.peak > acc.peak:
                    accepted[i] = cand
                merged = True
                break
        if not merged:
            accepted.append(cand)
    accepted.sort(key=lambda o: o.time_s)
    return accepted


def expected_clicks(
    duration_s: float, spacing_s: float, first_onset_s: float | None
) -> int:
    """How many clicks the recording *should* contain.

    `first_onset_s` is the ground-truth first click time (the generator's TSV
    or the measured first onset); clicks fully inside the recording count.
    """
    if first_onset_s is None:
        return 0
    n = int((duration_s - first_onset_s - 0.01) // spacing_s) + 1
    return max(0, n)


def summarize(
    onsets: list[Onset], duration_s: float, spacing_s: float, expect_first_s: float | None
) -> dict[str, float | int]:
    """Compute the parseable summary metrics over the detected onsets."""
    times = [o.time_s for o in onsets]
    intervals = [b - a for a, b in zip(times, times[1:])]
    expected_n = expected_clicks(
        duration_s, spacing_s, expect_first_s if expect_first_s is not None else times[0]
    ) if times else expected_clicks(duration_s, spacing_s, expect_first_s)
    return {
        "n": len(onsets),
        "expected": expected_n,
        "missing": max(0, expected_n - len(onsets)),
        "mean_s": statistics.fmean(intervals) if intervals else 0.0,
        "min_s": min(intervals) if intervals else 0.0,
        "max_s": max(intervals) if intervals else 0.0,
        "stddev_ms": statistics.pstdev(intervals) * 1000.0 if len(intervals) > 1 else 0.0,
        "first_s": times[0] if times else 0.0,
    }


def read_wav(path: Path) -> tuple[array.array, int, float]:
    """Read a mono 16-bit WAV; returns (samples, rate, duration_s)."""
    with wave.open(str(path), "rb") as wav:
        if wav.getsampwidth() != 2:
            raise ValueError(f"{path}: expected 16-bit samples, got {wav.getsampwidth() * 8}-bit")
        rate = wav.getframerate()
        if rate <= 0:
            raise ValueError(f"{path}: invalid sample rate {rate}")
        channels = wav.getnchannels()
        frames = wav.readframes(wav.getnframes())
    raw = array.array("h")
    raw.frombytes(frames)
    if channels > 1:
        # Take channel 0 (the recorder writes mono; tolerate stereo captures).
        raw = raw[::channels]
    if len(raw) == 0:
        raise ValueError(f"{path}: no audio frames")
    return raw, rate, len(raw) / rate


def run_self_test() -> int:
    """Synthesize a track with known onsets (one click dropped) and verify."""
    rate = PCM_RATE_SELFTEST = 48_000
    spacing_s = 2.0
    click_samples = 240  # 5 ms
    freq = 1000.0
    truth_s = [0.2, 4.2]  # the click at 2.2 s is MISSING (dropout)
    total = int(rate * 6.0)

    samples = array.array("h", bytes(2 * total))
    import random

    rng = random.Random(1234)
    for i in range(total):
        samples[i] = rng.randrange(-40, 41)  # quiet noise floor
    for t in truth_s:
        start = int(round(t * rate))
        for i in range(click_samples):
            w = 0.5 - 0.5 * math.cos(math.pi * min(i, click_samples - 1 - i, 119) / 119) \
                if min(i, click_samples - 1 - i) < 119 else 1.0
            samples[start + i] += int(round(0.8 * 32767 * w * math.sin(2 * math.pi * freq * i / rate)))

    onsets = detect_onsets(samples, rate, spacing_s)
    got = [o.time_s for o in onsets]
    checks: list[tuple[str, bool]] = []
    checks.append(("detected 2 of 3 clicks (1 dropped)", len(got) == 2))
    got_delta = abs(got[0] - truth_s[0]) if len(got) >= 1 else 999
    checks.append(
        ("first onset within 4 ms of truth", len(got) >= 1 and got_delta <= 0.004)
    )
    checks.append(
        ("second onset within 4 ms of truth", len(got) >= 2 and abs(got[1] - truth_s[1]) <= 0.004)
    )
    checks.append(
        ("interval ~ 4.0 s", len(got) >= 2 and abs((got[1] - got[0]) - 4.0) <= 0.008)
    )
    summary = summarize(onsets, total / rate, spacing_s, expect_first_s=0.2)
    checks.append(("expected=3 missing=1", summary["expected"] == 3 and summary["missing"] == 1))
    checks.append(("intervals stddev == 0 (deterministic)", summary["stddev_ms"] == 0.0))

    failed = [name for name, ok in checks if not ok]
    for name, ok in checks:
        print(f"SELFTEST {'ok ' if ok else 'FAIL '} {name}")
    if failed:
        print(f"SELFTEST FAILED: {len(failed)}/{len(checks)} checks failed")
        return 1
    print(f"SELFTEST PASSED: {len(checks)}/{len(checks)} checks")
    return 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--wav", type=Path,
        required="--self-test" not in sys.argv[1:],
        help="recorded WAV to analyze (not needed with --self-test)",
    )
    parser.add_argument("--spacing", type=float, default=DEFAULT_SPACING_S,
                        help=f"expected click spacing in seconds (default {DEFAULT_SPACING_S})")
    parser.add_argument("--threshold", type=float, default=DEFAULT_THRESHOLD,
                        help=f"onset level = threshold x median envelope (default {DEFAULT_THRESHOLD}; "
                             "raise it if noise produces false clicks)")
    parser.add_argument("--window-ms", type=float, default=DEFAULT_WINDOW_MS,
                        help=f"RMS window in ms (default {DEFAULT_WINDOW_MS})")
    parser.add_argument("--hop-ms", type=float, default=DEFAULT_HOP_MS,
                        help=f"envelope hop in ms (default {DEFAULT_HOP_MS})")
    parser.add_argument("--expect-first-s", type=float, default=None,
                        help="ground-truth first click time (generator TSV); "
                             "used to compute the expected click count")
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="synthesize an in-memory click track (one dropped click) and run detection",
    )
    return parser.parse_args()


def format_summary(summary: dict[str, float | int]) -> str:
    return (
        f"CLICKS n={summary['n']} expected={summary['expected']} missing={summary['missing']} "
        f"intervals_mean_s={summary['mean_s']:.4f} intervals_min_s={summary['min_s']:.4f} "
        f"intervals_max_s={summary['max_s']:.4f} intervals_stddev_ms={summary['stddev_ms']:.2f} "
        f"first_onset_s={summary['first_s']:.4f}"
    )


def main() -> int:
    args = parse_args()
    if args.self_test:
        return run_self_test()
    if args.spacing <= 0 or args.threshold <= 0:
        print("CLICKS error: --spacing and --threshold must be > 0", file=sys.stderr)
        return 1

    try:
        samples, rate, duration_s = read_wav(args.wav)
    except (ValueError, wave.Error) as exc:
        print(f"CLICKS error: {exc}", file=sys.stderr)
        return 1

    onsets = detect_onsets(
        samples, rate, args.spacing,
        threshold=args.threshold, window_ms=args.window_ms, hop_ms=args.hop_ms,
    )
    summary = summarize(onsets, duration_s, args.spacing, args.expect_first_s)
    print(format_summary(summary))
    for i, o in enumerate(onsets):
        print(f"  ONSET idx={i} t_s={o.time_s:.6f} peak={o.peak:.0f}")
    if not onsets:
        print("CLICKS warning: no onsets detected (recording silent or unlinked?)", file=sys.stderr)
        return 1
    if summary["missing"]:
        print(
            f"CLICKS warning: {summary['missing']} of {summary['expected']} clicks missing "
            f"(possible dropout)",
            file=sys.stderr,
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
