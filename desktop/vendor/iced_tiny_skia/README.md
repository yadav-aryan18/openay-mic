# Vendored `iced_tiny_skia` 0.13.0 (patched)

Upstream: https://crates.io/crates/iced_tiny_skia (version 0.13.0, MIT).

## Why this fork exists

iced 0.13.1 with the tiny-skia renderer drops canvas geometry whenever a
fill or stroke's translated bounds fail an **exact** `f32` equality check
against the clip intersection:

```rust
let clip_bounds = layer_bounds.intersection(&physical_bounds) ...;
let clip_mask = (physical_bounds != clip_bounds).then_some(clip_mask as &_);
```

The intersection recomputes `width = (x + w) - x`, which is not always
bit-equal to the original `w` for fractional coordinates (e.g. a circle's
bounds `56.000004` vs `56.0`, or `13.958332` vs `13.958333`). When the
comparison fails, the primitive is drawn **with the group's clip mask —
which is still in canvas-local coordinates** (it was never multiplied by
the group transformation). At non-zero window positions the translated
content falls outside that misplaced mask and is clipped to nothing.

Observed symptoms in OpenAY Mic before the fix:

- the MIC ring track (stroked circle) rendered nothing;
- 7 of the 24 VU ladder segments vanished (deterministic positions);
- the ON AIR dot group was skipped entirely, because its local clip
  bounds never intersected the button's window-space clip layer.

## The fix

In `src/lib.rs`, `Renderer::draw` — map the group's clip bounds through
the group transformation before intersecting with the window-space clip:

```rust
let Some(new_clip_bounds) = (group.clip_bounds()
    * group.transformation()
    * scale_factor)
    .intersection(&clip_bounds)
else { ... };
```

This is the same contract the wgpu backend uses (clip bounds live in the
same space as the transformed geometry), and it makes the clip mask land
exactly on the translated content.

## Second fix: damage tracking dropped every canvas-only change

`src/layer.rs`, `Layer::damage` computed the damaged region of a canvas
primitive group by intersecting each primitive's **screen-space** bounds
(already multiplied by the group transformation) with `group_bounds`, which
are recorded in **canvas-local** space. At any non-zero window position
that intersection is empty, so:

- a frame whose only changes were canvas geometry (VU ladder segments,
  MIC ring arc, cable pulse, buffer fill bar) diffed as **zero damage**
  and `present` returned early — the window froze on its first frame;
- text changes still produced damage (their bounds are not intersected
  with a clip rect), which is why readouts occasionally updated while the
  meters beside them stayed frozen.

The fix damages the whole transformed group (`group_bounds *
transformation`), matching how `Item::Cached` groups are already treated.
Slightly over-damages a changed canvas region; correctness over
micro-optimality for a software renderer.

Observed symptoms before this second fix: meters painted correctly on the
first frame after launch and never repainted afterwards; the LINK packets/s
readout updated while the VU ladder next to it stayed dark during an active
stream.

## Third fix: HiDPI / scale-factor matrix multiplication order

In `src/lib.rs`, `Renderer::draw` combined the group transformation and the
viewport scale factor as `group.transformation() * Transformation::scale(scale_factor)`.
In affine transformation matrices, translation followed by scaling leaves the
translation vector unscaled in logical space ($T \cdot S \cdot v = s \cdot v + [x, y]^T$).
On HiDPI/fractionally scaled screens (e.g. 1.25x, 1.5x, 2.0x), quads and window
boxes scaled to physical coordinates while canvas primitive groups (MIC level
ring, cables, VU ladder segments, ON AIR toggle) rendered at unscaled logical
positions, causing misaligned and displaced graphics across cards.

The fix combines the transformations in scale-then-translate order:
`Transformation::scale(scale_factor) * group.transformation()`, ensuring
logical coordinates scale accurately across all DPI factors.

## Diffs against the published crate

1. `src/lib.rs`, `Renderer::draw` — map group clip bounds and primitive/text transforms through `Transformation::scale(scale_factor) * group.transformation()`.
2. `src/layer.rs`, `Layer::damage` — transform group bounds before using them as damage regions (above).

The `[patch.crates-io]` entry lives in the workspace `Cargo.toml`.
