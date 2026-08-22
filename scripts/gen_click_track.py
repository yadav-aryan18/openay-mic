#!/usr/bin/env python3
"""OpenAY Mic acoustic click-track generator.

Renders a 48 kHz mono 16-bit PCM WAV of short sine-burst clicks separated by
exact sample-aligned digital silence. Used by the hardware latency audit
(docs/latency-audit.md): the track is played over a speaker next to the phone;
the phone's mic picks the clicks up and streams them to the desktop, where the
recording of the `openay_mic` PipeWire source is scanned by
scripts/analyze_clicks.py. The `<out>.clicks.tsv` sidecar lists the exact
generator onset sample index and time of every click (ground truth).

Click shape: a `--freq` sine burst of `--click-ms` milliseconds with
raised-cosine (Hann) edge ramps so the spectrum stays compact; the first click
starts at t = 1.0 s so the recording start never clips an attack. All clicks
begin on integer sample indices: onset_k = first_onset + k * round(spacing *
48000), where first_onset = round(1.0 * 48000) = 48000.

Exit code 0 on success; nonzero on argument errors or a failed --self-test.
"""

from __future__ import annotations

import argparse
import array
import math
import sys
import tempfile
from pathlib import Path

PCM_RATE: int = 48_000
NCHANNELS: int = 1
SAMPLE_WIDTH: int = 2
FULL_SCALE: int = 32767
PI: float = math.pi

# Defaults documented in --help.
DEFAULT_FIRST_ONSET_S: float = 1.0
DEFAULT_EDGE_MS: float = 1.0


def _positive_float(name: str) -> float:
    def parse(value: str) -> float:
        try:
            v = float(value)
        except ValueError:
            raise argparse.ArgumentTypeError(f"{name} must be a number, got {value!r}")
        if not math.isfinite(v) or v <= 0.0:
            raise argparse.ArgumentTypeError(f"{name} must be > 0, got {v}")
        return v

    return parse


def _level_float(value: str) -> float:
    v = float(value)
    if not math.isfinite(v) or not 0.0 < v <= 1.0:
        raise argparse.ArgumentTypeError(f"--level must be in (0, 1], got {v}")
    return v


def click_window(click_samples: int, edge_samples: int) -> list[float]:
    """Raised-cosine (Hann) ramp of length `click_samples` samples.

    Attack ramps 0 -> 1 over `edge_samples` samples, release ramps back 1 -> 0
    over the same length; the middle stays at 1.0. The first and last samples
    are exactly 0.0 (Hann endpoints), so clicks cannot look like a step.
    """
    edge = max(1, min(edge_samples, (click_samples - 1) // 2))
    if edge == 0:
        edge = 1
    w = [1.0] * click_samples
    for i in range(edge):
        s = 0.5 - 0.5 * math.cos(PI * i / edge)  # sin^2 ramp: 0 -> ~1
        w[i] = s
        w[click_samples - 1 - i] = s
    return w


def click_onsets(
    total_samples: int,
    first_onset: int,
    spacing_samples: int,
    click_samples: int,
) -> list[int]:
    """Exact onset sample indices: click k starts at first_onset + k*spacing.

    A click counts only if it fits entirely inside the track; partial trailing
    clicks (which would be clipped by the recorder) are dropped. Values are
    exact integers by construction (no drifting float accumulation).
    """
    onsets: list[int] = []
    k = 0
    while True:
        start = first_onset + k * spacing_samples
        if start + click_samples > total_samples:
            break
        onsets.append(start)
        k += 1
    return onsets


def generate_samples(
    total_samples: int,
    freq: float,
    first_onset: int,
    spacing_samples: int,
    click_samples: int,
    level: float,
) -> tuple[array.array, list[int]]:
    """Render the whole track. Returns (int16 mono samples, onset indices)."""
    onsets = click_onsets(total_samples, first_onset, spacing_samples, click_samples)
    if not onsets:
        raise ValueError("no click fits inside the requested duration")
    window = click_window(click_samples, max(1, int(round(DEFAULT_EDGE_MS * PCM_RATE / 1000.0))))
    samples = array.array("h", bytes(2 * total_samples))  # digital silence
    wavetwo = 2.0 * PI
    amp = level * FULL_SCALE
    for start in onsets:
        for i in range(click_samples):
            v = amp * window[i] * math.sin(wavetwo * freq * i / PCM_RATE)
            samples[start + i] = int(round(v))
    return samples, onsets


def write_wav(path: Path, samples: array.array) -> None:
    import wave

    with wave.open(str(path), "wb") as wav:
        wav.setnchannels(NCHANNELS)
        wav.setsampwidth(SAMPLE_WIDTH)
        wav.setframerate(PCM_RATE)
        wav.writeframes(samples.tobytes())


def write_tsv(path: Path, onsets: list[int]) -> None:
    with open(path, "w", encoding="utf-8") as f:
        f.write("# OpenAY click-track ground truth\n")
        f.write("# cols: onset_sample<TAB>onset_s\n")
        for start in onsets:
            f.write(f"{start}\t{start / PCM_RATE:.6f}\n")


def run_self_test() -> int:
    """Pure-function assertions; returns process exit code (0 = pass)."""
    checks: list[tuple[str, bool]] = []

    # 1) Onset positions are exact integers of the spec.
    first = int(round(DEFAULT_FIRST_ONSET_S * PCM_RATE))
    spacing = int(round(2.0 * PCM_RATE))
    click = int(round(5.0 * PCM_RATE / 1000.0))
    total = int(round(6.0 * PCM_RATE))
    onsets = click_onsets(total, first, spacing, click)
    checks.append(("onsets exact (1.0/3.0/5.0 s)", onsets == [48_000, 144_000, 240_000]))
    checks.append(("onset spacing exact", all(b - a == spacing for a, b in zip(onsets, onsets[1:]))))

    # 2) Sample-exactness holds for non-integer spacing (rounding is exact).
    sp2 = int(round(0.5 * PCM_RATE))  # 24000 ms spacing
    onsets2 = click_onsets(total, first, sp2, click)
    checks.append(("non-integer spacing exact", all(b - a == sp2 for a, b in zip(onsets2, onsets2[1:]))))

    # 3) Render and validate content.
    samples, rendered_onsets = generate_samples(
        total, freq=1000.0, first_onset=first,
        spacing_samples=spacing, click_samples=click, level=0.8,
    )
    n = len(samples)
    checks.append(("sample count exact", n == total))
    checks.append(("onsets match render", rendered_onsets == onsets))
    checks.append(("silence before first click", all(v == 0 for v in samples[: 48_000 - 1])))
    checks.append(("silence between clicks", all(v == 0 for v in samples[144_000 + click : 240_000])))

    amp_bounds = all(-FULL_SCALE <= v <= FULL_SCALE for v in samples)
    checks.append(("amplitude within 16-bit range", amp_bounds))

    peak = max(abs(v) for v in samples)
    # 0.8*32767 = 26213.6 rounds to 26214 at the sine peak; the guarantee is
    # that the level applies (never reaches full scale), not an exact bound.
    checks.append(
        ("no clipping at --level 0.8 (peak <= 0.8*32767 + 1)", peak <= int(0.8 * FULL_SCALE) + 1)
    )
    checks.append(("click energy present at onset", any(samples[48_000 + i] != 0 for i in range(click))))

    # 4) WAV written by the same code path round-trips exactly.
    with tempfile.TemporaryDirectory() as td:
        wav_path = Path(td) / "selftest.wav"
        tsv_path = Path(td) / "selftest.wav.clicks.tsv"
        write_wav(wav_path, samples)
        write_tsv(tsv_path, onsets)
        import wave

        with wave.open(str(wav_path), "rb") as wav:
            checks.append(
                ("wav header 48k/1ch/16-bit",
                 wav.getframerate() == PCM_RATE and wav.getnchannels() == 1
                 and wav.getsampwidth() == 2 and wav.getnframes() == n)
            )
            back = array.array("h", wav.readframes(n))
        checks.append(("wav samples round-trip", back == samples))
        with open(tsv_path, encoding="utf-8") as f:
            lines = f.read().splitlines()
        checks.append(("tsv has # header + N rows", len(lines) == 2 + len(onsets)))

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
    parser.add_argument("--out", type=Path, default=Path("click_track.wav"))
    parser.add_argument("--duration", type=_positive_float("--duration"), default=30.0)
    parser.add_argument("--freq", type=_positive_float("--freq"), default=1000.0)
    parser.add_argument("--spacing", type=_positive_float("--spacing"), default=2.0)
    parser.add_argument("--click-ms", type=_positive_float("--click-ms"), default=5.0)
    parser.add_argument("--level", type=_level_float, default=0.8)
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="run internal assertions (onsets exact, amplitude bounds, round-trip) and exit",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.self_test:
        return run_self_test()

    total_samples = int(round(args.duration * PCM_RATE))
    first_onset = int(round(DEFAULT_FIRST_ONSET_S * PCM_RATE))
    spacing_samples = int(round(args.spacing * PCM_RATE))
    click_samples = int(round(args.click_ms * PCM_RATE / 1000.0))
    if click_samples < 2:
        print(f"CLICKTRACK error: --click-ms {args.click_ms} too short (< 2 samples)", file=sys.stderr)
        return 1
    if spacing_samples <= click_samples:
        print(
            f"CLICKTRACK warning: --spacing {args.spacing} s <= click length "
            f"{click_samples} samples; clicks will overlap",
            file=sys.stderr,
        )

    samples, onsets = generate_samples(
        total_samples, args.freq, first_onset, spacing_samples, click_samples, args.level
    )
    write_wav(args.out, samples)
    write_tsv(Path(str(args.out) + ".clicks.tsv"), onsets)

    duration_s = len(samples) / PCM_RATE
    print(
        f"CLICKTRACK out={args.out} duration_s={duration_s:.3f} clicks={len(onsets)} "
        f"spacing_s={args.spacing} first_onset_s={onsets[0] / PCM_RATE:.3f}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
