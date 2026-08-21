#!/usr/bin/env python3
"""OpenAY Mic tray icon generator (committed, one-time use).

Writes the three 24x24 RGBA tray icons used by the openay-gui tray
(ksni StatusNotifierItem) into `src/icons.rs` as raw RGBA byte arrays:

- idle  : gray ring  (#8D8477), transparent center  -- engine stopped
- armed : amber ring (#FFB454) with amber center dot -- engine running
- live  : tally red ring (#E5484D) with red center dot -- engine running
          (reserved for the live state; visually distinct from armed)

Run from the crate directory:
    python3 tools/gen_icons.py
"""

import math
import os

SIZE = 24
CX = CY = (SIZE - 1) / 2.0

INK = (20, 18, 15, 255)  # icon backdrop: palette ink
GRAY = (141, 132, 119, 255)  # dim
AMBER = (255, 180, 84, 255)  # amber
TALLY = (229, 72, 77, 255)  # tally red

RING_R = 9.0  # outer radius of the ring
RING_W = 2.5  # ring stroke width
DOT_R = 4.0  # center dot radius


def make_icon(fg, ring, dot):
    """`ring`/`dot`: whether to draw the ring and the center dot."""
    px = [[(0, 0, 0, 0) for _ in range(SIZE)] for _ in range(SIZE)]
    for y in range(SIZE):
        for x in range(SIZE):
            d = math.hypot(x - CX, y - CY)
            if ring and abs(d - RING_R) <= RING_W / 2:
                px[y][x] = fg
            elif dot and d <= DOT_R:
                px[y][x] = fg
    # Soft 1px edge: blend the ring's outer edge into the backdrop so the
    # icon reads well on light and dark trays. Inner edge stays crisp.
    for y in range(SIZE):
        for x in range(SIZE):
            d = math.hypot(x - CX, y - CY)
            if ring and RING_W / 2 < abs(d - RING_R) <= RING_W / 2 + 1.0:
                a = 1.0 - (abs(d - RING_R) - RING_W / 2)
                r, g, b, aa = px[y][x]
                px[y][x] = (r, g, b, int(aa * a))
    flat = [v for row in px for v in row]
    out = bytearray()
    for r, g, b, a in flat:
        out += bytes((r, g, b, a))
    return bytes(out)


def rust_array(name, data):
    lines = []
    lines.append(f"    /// {SIZE}x{SIZE} RGBA tray icon (`{name}`).")
    lines.append(f"    pub const {name.upper()}: [u8; {len(data)}] = [")
    for i in range(0, len(data), 16):
        chunk = ", ".join(f"0x{b:02x}" for b in data[i : i + 16])
        lines.append(f"        {chunk},")
    lines.append("    ];")
    lines.append("")
    return "\n".join(lines)


def main():
    out_path = os.path.join(os.path.dirname(__file__), "..", "src", "icons.rs")
    idle = make_icon(GRAY, ring=True, dot=False)
    armed = make_icon(AMBER, ring=True, dot=True)
    live = make_icon(TALLY, ring=True, dot=True)

    header = """//! Tray icon pixel data, generated ONCE by `tools/gen_icons.py`
//! (committed generator; rerun it to regenerate). Each icon is a 24x24 RGBA
//! bitmap consumed by the ksni StatusNotifierItem (raw pixmap, no PNG
//! decoding needed).

/// Icon states, in the same order as the tray states.
pub const ICONS: [&[u8]; 3] = [&IDLE, &ARMED, &LIVE];

"""
    body = rust_array("idle", idle) + rust_array("armed", armed) + rust_array("live", live)
    with open(out_path, "w") as f:
        f.write(header + body)
    print(f"wrote {out_path} ({len(idle) + len(armed) + len(live)} bytes)")


if __name__ == "__main__":
    main()
