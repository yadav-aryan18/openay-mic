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

The only diff against the published crate is that one expression; the
`[patch.crates-io]` entry lives in the workspace `Cargo.toml`.
